import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import {
  clearLogs,
  getLogs,
  type LogLevel,
  type LogPage,
  type LogRecord,
  type LogStats,
} from '@/lib/server-api';

/**
 * Logs page — reads the server's `logs.db`.
 *
 * The Rust server records every request it answers and every background job it
 * runs (transcode, build/ASR/align, dub, mux, platform export) with a level, a
 * category and free-form detail. This page filters that stream so a problem
 * noticed after the fact — a stuck dubbing run, a mux that never produced a
 * file, a 500 on one endpoint — can be traced without an open terminal.
 *
 * It polls only while auto-refresh is on, so a live dub/mux can be watched as
 * it happens; the level chips, category select and text/session boxes narrow the
 * view server-side, so the DB does the filtering, not the browser.
 */

const LEVELS: { value: LogLevel | ''; label: string }[] = [
  { value: '', label: 'All' },
  { value: 'debug', label: 'Debug' },
  { value: 'info', label: 'Info' },
  { value: 'warn', label: 'Warn' },
  { value: 'error', label: 'Error' },
];

/** Colour for a level: the chip and the row share one meaning. */
function levelTone(level: LogLevel): string {
  switch (level) {
    case 'error':
      return 'bg-destructive/15 text-destructive';
    case 'warn':
      return 'bg-amber-500/15 text-amber-700 dark:text-amber-400';
    case 'info':
      return 'bg-primary/15 text-primary';
    case 'debug':
      return 'bg-muted text-muted-foreground';
  }
}

function fmtTime(tsMs: number): string {
  const d = new Date(tsMs);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  const ss = String(d.getSeconds()).padStart(2, '0');
  const ms = String(d.getMilliseconds()).padStart(3, '0');
  return `${hh}:${mm}:${ss}.${ms}`;
}

function fmtDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(2)}s`;
}

function basename(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

/** A small toggle button used for the level chips. */
function Chip({
  active,
  onClick,
  children,
  tone,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
  tone?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'inline-flex items-center rounded-full px-2.5 py-0.5 text-[11px] font-medium transition-colors',
        active
          ? tone ?? 'bg-primary text-primary-foreground'
          : 'bg-muted text-muted-foreground hover:bg-muted/70'
      )}
    >
      {children}
    </button>
  );
}

export function LogsPage() {
  const [page, setPage] = useState<LogPage | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [clearing, setClearing] = useState(false);

  const [level, setLevel] = useState<LogLevel | ''>('');
  const [category, setCategory] = useState('');
  const [q, setQ] = useState('');
  const [session, setSession] = useState('');
  const [auto, setAuto] = useState(true);
  const [expanded, setExpanded] = useState<number | null>(null);

  const stats: LogStats | null = page?.stats ?? null;
  const records: LogRecord[] = page?.records ?? [];

  // `refresh` reads the latest filters from refs so the polling interval does
  // not need to be torn down and rebuilt every keystroke.
  const filters = useRef({ level, category, q, session });
  filters.current = { level, category, q, session };

  const refresh = useCallback(async (showSpinner: boolean) => {
    if (showSpinner) setLoading(true);
    try {
      const f = filters.current;
      const result = await getLogs({
        level: f.level || undefined,
        category: f.category || undefined,
        q: f.q || undefined,
        session: f.session || undefined,
        limit: 500,
      });
      setPage(result);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (showSpinner) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh(true);
  }, [refresh, level, category, q, session]);

  useEffect(() => {
    if (!auto) return;
    const t = window.setInterval(() => void refresh(false), 2000);
    return () => window.clearInterval(t);
  }, [auto, refresh]);

  const categories = useMemo(
    () =>
      stats
        ? Object.entries(stats.by_category)
            .sort((a, b) => b[1] - a[1])
            .map(([name]) => name)
        : [],
    [stats]
  );

  const onClear = useCallback(async () => {
    setClearing(true);
    try {
      await clearLogs();
      setExpanded(null);
      await refresh(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setClearing(false);
    }
  }, [refresh]);

  const copyView = useCallback(() => {
    const text = records
      .map((r) => {
        const extra = [
          r.session_id ? `session=${r.session_id}` : '',
          r.media_path ? `media=${r.media_path}` : '',
          r.detail ? r.detail : '',
        ]
          .filter(Boolean)
          .join(' · ');
        return `${fmtTime(r.ts_ms)} ${r.level.toUpperCase().padEnd(5)} ${r.category} ${r.message}${
          extra ? ` — ${extra}` : ''
        }`;
      })
      .join('\n');
    void navigator.clipboard.writeText(text);
  }, [records]);

  return (
    <div className="mx-auto flex max-w-6xl flex-col gap-4 p-4">
      <header className="flex flex-wrap items-center gap-2">
        <h1 className="text-lg font-semibold">Logs</h1>
        {stats && (
          <span className="text-xs text-muted-foreground">
            {stats.total} row{stats.total === 1 ? '' : 's'}
            {stats.by_level.error ? ` · ${stats.by_level.error} error(s)` : ''}
          </span>
        )}
        <div className="ml-auto flex items-center gap-2">
          <Button
            size="sm"
            variant={auto ? 'default' : 'outline'}
            onClick={() => setAuto((v) => !v)}
          >
            {auto ? 'Auto-refresh on' : 'Auto-refresh off'}
          </Button>
          <Button size="sm" variant="outline" onClick={() => void refresh(true)}>
            Refresh
          </Button>
          <Button size="sm" variant="outline" onClick={copyView}>
            Copy
          </Button>
          <Button
            size="sm"
            variant="destructive"
            onClick={() => void onClear()}
            disabled={clearing}
          >
            {clearing ? 'Clearing…' : 'Clear'}
          </Button>
        </div>
      </header>

      {/* Filters */}
      <div className="flex flex-wrap items-center gap-3 rounded-lg border border-border p-3">
        <div className="flex items-center gap-1">
          {LEVELS.map((l) => (
            <Chip
              key={l.value || 'all'}
              active={level === l.value}
              onClick={() => setLevel(l.value)}
              tone={l.value ? levelTone(l.value) : undefined}
            >
              {l.label}
              {stats && l.value ? ` ${stats.by_level[l.value] ?? 0}` : ''}
            </Chip>
          ))}
        </div>

        <select
          value={category}
          onChange={(e) => setCategory(e.target.value)}
          className="h-8 rounded-md border border-border bg-input/30 px-2 text-sm"
        >
          <option value="">All categories</option>
          {categories.map((c) => (
            <option key={c} value={c}>
              {c} ({stats?.by_category[c] ?? 0})
            </option>
          ))}
        </select>

        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search message…"
          className="h-8 w-52 rounded-md border border-border bg-input/30 px-2 text-sm"
        />
        <input
          value={session}
          onChange={(e) => setSession(e.target.value)}
          placeholder="Session id"
          className="h-8 w-44 rounded-md border border-border bg-input/30 px-2 text-sm"
        />
      </div>

      {error && (
        <div className="rounded-lg border border-destructive/40 bg-destructive/10 p-3 text-sm text-destructive">
          {error}
        </div>
      )}

      {/* Table */}
      <div className="overflow-hidden rounded-lg border border-border">
        <table className="w-full border-collapse text-left text-sm">
          <thead className="bg-muted/50 text-xs uppercase tracking-wide text-muted-foreground">
            <tr>
              <th className="px-3 py-2 font-medium">Time</th>
              <th className="px-3 py-2 font-medium">Level</th>
              <th className="px-3 py-2 font-medium">Category</th>
              <th className="px-3 py-2 font-medium">Message</th>
              <th className="px-3 py-2 font-medium">Context</th>
            </tr>
          </thead>
          <tbody>
            {records.map((r) => {
              const open = expanded === r.id;
              const hasExtra = Boolean(r.detail || r.session_id || r.media_path);
              return (
                <tr
                  key={r.id}
                  className={cn(
                    'border-t border-border align-top',
                    hasExtra && 'cursor-pointer hover:bg-muted/30'
                  )}
                  onClick={() => hasExtra && setExpanded(open ? null : r.id)}
                >
                  <td className="whitespace-nowrap px-3 py-2 font-mono text-xs text-muted-foreground">
                    {fmtTime(r.ts_ms)}
                  </td>
                  <td className="px-3 py-2">
                    <span
                      className={cn(
                        'inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-medium uppercase',
                        levelTone(r.level)
                      )}
                    >
                      {r.level}
                    </span>
                  </td>
                  <td className="whitespace-nowrap px-3 py-2 font-mono text-xs">
                    {r.category}
                  </td>
                  <td className="px-3 py-2">
                    <div className="flex flex-col gap-0.5">
                      <span>{r.message}</span>
                      {open && r.detail && (
                        <span className="whitespace-pre-wrap wrap-break-word font-mono text-xs text-muted-foreground">
                          {r.detail}
                        </span>
                      )}
                    </div>
                  </td>
                  <td className="px-3 py-2 text-xs text-muted-foreground">
                    <div className="flex flex-col gap-0.5">
                      {r.session_id && (
                        <span title={r.session_id}>session …{r.session_id.slice(-6)}</span>
                      )}
                      {r.media_path && <span title={r.media_path}>{basename(r.media_path)}</span>}
                      {r.duration_ms !== undefined && <span>{fmtDuration(r.duration_ms)}</span>}
                    </div>
                  </td>
                </tr>
              );
            })}
            {!loading && records.length === 0 && (
              <tr>
                <td colSpan={5} className="px-3 py-8 text-center text-sm text-muted-foreground">
                  {error ? 'Could not load logs.' : 'No log records match.'}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
