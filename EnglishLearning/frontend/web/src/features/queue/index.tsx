import { useCallback, useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { cn, fileName } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import {
  cancelJob,
  listJobs,
  pickMediaFilesOnServer,
  submitJobs,
  type Job,
  type JobState,
} from '@/lib/server-api';

/**
 * Queue page — submit a batch of media and watch it get processed.
 *
 * Adding a video is expensive (decode, whisper, optional alignment, then the
 * browser-playable transcode), so it runs on the server's bounded worker pool
 * instead of inside the request: the POST returns a handle per file in
 * milliseconds, and a notice arrives when the work is actually done. A single
 * worker is the default, so this page is also the place the queue depth is
 * visible — five files take five times one file, and that has to look
 * deliberate rather than broken.
 *
 * The table is polled (the work lives in another process-local thread, so the
 * only source of truth is the server's job table); the *notification* is pushed
 * over SSE by providers/job-notifications.tsx, which is why closing this page
 * does not mean missing the "done".
 */

/** Poll cadence: fast enough to watch a build move, cheap enough to leave open. */
const POLL_MS = 2000;

const STATE_TONES: Record<JobState, string> = {
  queued: 'bg-muted text-muted-foreground',
  running: 'bg-primary/15 text-primary',
  done: 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400',
  failed: 'bg-destructive/15 text-destructive',
  cancelled: 'bg-amber-500/15 text-amber-700 dark:text-amber-400',
};

/** The stage a running build is in, in the words the user is watching for. */
const STAGE_LABELS: Record<string, string> = {
  starting: 'starting',
  decode: 'decoding',
  transcribe: 'transcribing',
  align: 'aligning',
  // Encoding the browser-playable H.264 copy — usually the longest phase, and
  // the one FFmpeg gives no percentage for.
  transcode: 'encoding H.264',
};

function fmtElapsed(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return `${m}m ${String(s).padStart(2, '0')}s`;
}

function JobRow({
  job,
  onCancel,
}: {
  job: Job;
  onCancel: (id: string) => void;
}) {
  const cancellable = job.state === 'queued' || job.state === 'running';
  const percent =
    job.total > 0 ? Math.min(100, Math.round((job.done / job.total) * 100)) : null;

  return (
    <tr className="border-t border-border align-top">
      <td className="px-3 py-2">
        <div className="flex flex-col gap-0.5">
          <span className="truncate font-medium" title={job.media_path}>
            {fileName(job.media_path)}
          </span>
          <span className="truncate font-mono text-[11px] text-muted-foreground" title={job.media_path}>
            {job.media_path}
          </span>
        </div>
      </td>

      <td className="px-3 py-2">
        <span
          className={cn(
            'inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-medium uppercase',
            STATE_TONES[job.state]
          )}
        >
          {job.state}
        </span>
        {/* A retry is transparent otherwise: the job goes back to `queued` and
            looks like it never ran. */}
        {job.attempts > 1 && (
          <div className="mt-1 text-[11px] text-muted-foreground">
            attempt {job.attempts}/{job.max_attempts}
          </div>
        )}
      </td>

      <td className="px-3 py-2">
        {job.state === 'running' && (
          <div className="flex flex-col gap-1">
            <span className="text-xs text-muted-foreground">
              {STAGE_LABELS[job.stage ?? ''] ?? job.stage ?? 'working'}
              {percent !== null && ` · ${job.done}/${job.total}`}
            </span>
            {percent !== null && (
              <div className="h-1 w-32 overflow-hidden rounded-full bg-muted">
                <div
                  className="h-full bg-primary transition-[width]"
                  style={{ width: `${percent}%` }}
                />
              </div>
            )}
          </div>
        )}
        {job.state === 'queued' && (
          <span className="text-xs text-muted-foreground">waiting for a worker</span>
        )}
        {job.state === 'done' && job.session_id && (
          <Link
            to={`/player?session=${encodeURIComponent(job.session_id)}`}
            className="text-xs font-medium text-primary hover:underline"
          >
            Open in player →
          </Link>
        )}
        {job.last_error && (job.state === 'failed' || job.state === 'queued') && (
          <div className="max-w-md text-xs text-destructive wrap-break-word">
            {job.last_error}
          </div>
        )}
      </td>

      <td className="whitespace-nowrap px-3 py-2 text-xs text-muted-foreground">
        {job.elapsed_secs > 0 ? fmtElapsed(job.elapsed_secs) : '—'}
      </td>

      <td className="px-3 py-2 text-right">
        {cancellable && (
          <Button size="xs" variant="destructive" onClick={() => onCancel(job.id)}>
            Cancel
          </Button>
        )}
      </td>
    </tr>
  );
}

export function QueuePage() {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);
  /** Files the server refused at submit time, kept until the next attempt. */
  const [rejected, setRejected] = useState<{ media_path: string; error: string }[]>([]);

  const refresh = useCallback(async () => {
    try {
      setJobs(await listJobs({ limit: 200 }));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const t = window.setInterval(() => void refresh(), POLL_MS);
    return () => window.clearInterval(t);
  }, [refresh]);

  // The picker blocks until the OS dialog closes and the batch takes as long as
  // the server needs to write the rows, so both are guarded against a
  // double-click.
  const pickingRef = useRef(false);

  const onAddFiles = useCallback(async () => {
    if (pickingRef.current) return;
    pickingRef.current = true;
    setPicking(true);
    try {
      const paths = await pickMediaFilesOnServer();
      // An empty list is a cancelled dialog, not an error.
      if (paths.length === 0) return;
      const result = await submitJobs(paths);
      setRejected(result.rejected);
      setError(null);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      pickingRef.current = false;
      setPicking(false);
    }
  }, [refresh]);

  const onCancel = useCallback(
    async (id: string) => {
      try {
        await cancelJob(id);
        await refresh();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [refresh]
  );

  const active = jobs.filter((j) => j.state === 'running' || j.state === 'queued').length;

  return (
    <div className="mx-auto flex max-w-6xl flex-col gap-4 p-4">
      <header className="flex flex-wrap items-center gap-2">
        <h1 className="text-lg font-semibold">Queue</h1>
        <span className="text-xs text-muted-foreground">
          {jobs.length} job{jobs.length === 1 ? '' : 's'}
          {active > 0 && ` · ${active} active`}
        </span>
        <div className="ml-auto flex items-center gap-2">
          <Button size="sm" onClick={() => void onAddFiles()} disabled={picking}>
            {picking ? 'Choosing…' : 'Add files…'}
          </Button>
          <Button size="sm" variant="outline" onClick={() => void refresh()}>
            Refresh
          </Button>
        </div>
      </header>

      <p className="text-xs text-muted-foreground">
        Pick one or more videos or audio files; each is transcribed and made
        playable in the background, and a notice appears when it is ready — you
        can leave this page.
      </p>

      {error && (
        <div className="rounded-lg border border-destructive/40 bg-destructive/10 p-3 text-sm text-destructive">
          {error}
        </div>
      )}

      {rejected.length > 0 && (
        <div className="rounded-lg border border-amber-500/40 bg-amber-500/10 p-3 text-sm">
          <div className="font-medium">
            {rejected.length} file{rejected.length === 1 ? '' : 's'} not queued
          </div>
          <ul className="mt-1 flex flex-col gap-0.5 text-xs text-muted-foreground">
            {rejected.map((r) => (
              <li key={r.media_path} title={r.media_path} className="truncate">
                {fileName(r.media_path)} — {r.error}
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="overflow-hidden rounded-lg border border-border">
        <table className="w-full border-collapse text-left text-sm">
          <thead className="bg-muted/50 text-xs uppercase tracking-wide text-muted-foreground">
            <tr>
              <th className="px-3 py-2 font-medium">File</th>
              <th className="px-3 py-2 font-medium">State</th>
              <th className="px-3 py-2 font-medium">Progress</th>
              <th className="px-3 py-2 font-medium">Elapsed</th>
              <th className="px-3 py-2" />
            </tr>
          </thead>
          <tbody>
            {jobs.map((job) => (
              <JobRow key={job.id} job={job} onCancel={onCancel} />
            ))}
            {jobs.length === 0 && (
              <tr>
                <td colSpan={5} className="px-3 py-8 text-center text-sm text-muted-foreground">
                  {error
                    ? 'Could not load the queue.'
                    : 'Nothing queued yet — press “Add files…” to pick one or more.'}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
