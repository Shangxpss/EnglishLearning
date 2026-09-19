import { useEffect, useRef, useState } from 'react';

/**
 * Dubbing Studio page.
 *
 * Lets the user point the backend at a folder or video file (by server-side
 * path) and dub every video inside it using the sync dubbing pipeline
 * (edge-tts + respeed synthesis). Progress is polled from
 * ``GET /dubbing/progress/{job_id}`` and rendered as a numeric progress bar.
 *
 * Output layout (handled by the backend):
 *   • Folder input → ``<folder>_dubbed/`` sibling directory.
 *   • File input   → ``<name>_dubbed.mp4`` next to the original.
 */

interface VideoResult {
  name: string;
  status: 'ok' | 'failed';
  out_video?: string;
  out_srt?: string;
  duration: number;
  processing_time: number;
  error?: string;
}

interface JobProgress {
  job_id: string;
  status: 'pending' | 'running' | 'completed' | 'failed';
  input_path: string;
  input_type: 'file' | 'folder';
  output_path: string;
  voice: string;
  total: number;
  completed: number;
  failed: number;
  current_video: string | null;
  current_stage: string | null;
  stage_progress: number;
  stage_total: number;
  started_at: number;
  finished_at: number | null;
  results: VideoResult[];
  error: string | null;
  elapsed: number;
  progress_pct: number;
}

interface VoiceOption {
  label: string;
  value: string;
}

const VOICE_OPTIONS: VoiceOption[] = [
  { label: 'Maisie (en-GB, Female)', value: 'en-GB-MaisieNeural' },
  { label: 'Sonia (en-GB, Female)', value: 'en-GB-SoniaNeural' },
  { label: 'Libby (en-GB, Female)', value: 'en-GB-LibbyNeural' },
  { label: 'Ryan (en-GB, Male)', value: 'en-GB-RyanNeural' },
  { label: 'Aria (en-US, Female)', value: 'en-US-AriaNeural' },
  { label: 'Guy (en-US, Male)', value: 'en-US-GuyNeural' },
];

const STAGE_LABELS: Record<string, string> = {
  '0_watermark': 'Removing watermark',
  '1_duration': 'Probing duration',
  '2_alignment': 'Aligning words (Whisper)',
  '3_cues': 'Building cues',
  '4_subtitles': 'Writing subtitles',
  '5_synthesis': 'Synthesizing TTS',
  '6_stitching': 'Stitching audio',
  '7_muxing': 'Muxing video',
  starting: 'Starting',
  'skipped (already dubbed)': 'Skipped (already dubbed — resuming)',
};

function formatTime(s: number): string {
  if (s < 60) return `${s.toFixed(1)}s`;
  const m = Math.floor(s / 60);
  const sec = (s - m * 60).toFixed(0);
  return `${m}m ${sec}s`;
}

export function DubbingStudioPage() {
  const [path, setPath] = useState('');
  const [voice, setVoice] = useState('en-GB-MaisieNeural');
  const [maxWordsPerSegment, setMaxWordsPerSegment] = useState(10);
  const [roomTone, setRoomTone] = useState(true);
  const [removeWatermark, setRemoveWatermark] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [job, setJob] = useState<JobProgress | null>(null);
  const pollRef = useRef<number | null>(null);

  // Stop polling on unmount.
  useEffect(() => {
    return () => {
      if (pollRef.current) window.clearInterval(pollRef.current);
    };
  }, []);

  const startPolling = (jobId: string) => {
    if (pollRef.current) window.clearInterval(pollRef.current);
    pollRef.current = window.setInterval(async () => {
      try {
        const resp = await fetch(`/api/dubbing/progress/${jobId}`);
        if (!resp.ok) {
          window.clearInterval(pollRef.current!);
          pollRef.current = null;
          setError(`Progress poll failed: HTTP ${resp.status}`);
          setLoading(false);
          return;
        }
        const data: JobProgress = await resp.json();
        setJob(data);
        if (data.status === 'completed' || data.status === 'failed') {
          if (pollRef.current) {
            window.clearInterval(pollRef.current);
            pollRef.current = null;
          }
          setLoading(false);
        }
      } catch (e) {
        // Network blip — keep polling, don't kill the job view.
        console.error('poll error', e);
      }
    }, 1500);
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!path.trim()) {
      setError('Please enter a file or folder path.');
      return;
    }
    setLoading(true);
    setError(null);
    setJob(null);
    try {
      const resp = await fetch('/api/dubbing/start', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          path: path.trim(),
          voice,
          max_words_per_segment: maxWordsPerSegment,
          room_tone: roomTone,
          remove_watermark: removeWatermark,
        }),
      });
      const data = await resp.json();
      if (!resp.ok) {
        throw new Error(data.detail || `HTTP ${resp.status}`);
      }
      startPolling(data.job_id);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to start dubbing.');
      setLoading(false);
    }
  };

  const isRunning = loading && job?.status === 'running';
  const isDone = job && (job.status === 'completed' || job.status === 'failed');
  const pct = job?.progress_pct ?? 0;

  return (
    <div className="max-w-3xl space-y-6">
      <div>
        <h1 className="text-2xl font-bold">Dubbing Studio</h1>
        <p className="text-sm text-muted-foreground mt-1">
          Provide a folder or video file path (on the server). The pipeline will
          dub every <code className="text-xs">.mp4</code> with the selected
          edge-tts voice. Folder input creates a{' '}
          <code className="text-xs">_dubbed</code> sibling directory; file input
          writes <code className="text-xs">_dubbed.mp4</code> alongside the
          original.
        </p>
      </div>

      {/* ── Form ── */}
      <form onSubmit={handleSubmit} className="space-y-4">
        <div>
          <label className="label">
            <span className="label-text">Path (file or folder)</span>
          </label>
          <input
            type="text"
            className="input input-bordered w-full"
            placeholder="/path/to/video.mp4  or  /path/to/folder"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            disabled={loading}
          />
        </div>

        <div>
          <label className="label">
            <span className="label-text">Voice</span>
          </label>
          <select
            className="select select-bordered w-full"
            value={voice}
            onChange={(e) => setVoice(e.target.value)}
            disabled={loading}
          >
            {VOICE_OPTIONS.map((v) => (
              <option key={v.value} value={v.value}>
                {v.label}
              </option>
            ))}
          </select>
        </div>

        <div>
          <label className="label">
            <span className="label-text">
              Max words per subtitle segment:{' '}
              <span className="font-mono font-bold">
                {maxWordsPerSegment === 0 ? 'off' : maxWordsPerSegment}
              </span>
            </span>
          </label>
          <input
            type="range"
            className="range range-primary"
            min={0}
            max={25}
            step={1}
            value={maxWordsPerSegment}
            onChange={(e) => setMaxWordsPerSegment(Number(e.target.value))}
            disabled={loading}
          />
          <p className="text-xs text-muted-foreground mt-1">
            {maxWordsPerSegment === 0
              ? 'No splitting — each subtitle is one full sentence (may be long).'
              : `Long sentences are split into segments of ≤ ${maxWordsPerSegment} words to prevent subtitles from covering too much of the video.`}
          </p>
        </div>

        <div className="flex items-center gap-3">
          <input
            type="checkbox"
            className="checkbox checkbox-primary"
            checked={roomTone}
            onChange={(e) => setRoomTone(e.target.checked)}
            disabled={loading}
          />
          <div>
            <span className="text-sm font-medium">Room tone fill</span>
            <p className="text-xs text-muted-foreground">
              Fills silence between sentences with low-level ambient noise
              extracted from the original video, so the dub doesn't sound
              choppy. Recommended.
            </p>
          </div>
        </div>

        <div className="flex items-center gap-3">
          <input
            type="checkbox"
            className="checkbox checkbox-primary"
            checked={removeWatermark}
            onChange={(e) => setRemoveWatermark(e.target.checked)}
            disabled={loading}
          />
          <div>
            <span className="text-sm font-medium">Remove watermark</span>
            <p className="text-xs text-muted-foreground">
              Auto-detects and removes static watermarks (corner text/logos)
              from the video before dubbing. Adds processing time proportional
              to video length.
            </p>
          </div>
        </div>

        <button
          type="submit"
          className={`btn btn-primary ${loading ? 'loading' : ''}`}
          disabled={loading}
        >
          {loading ? 'Dubbing…' : 'Start Dubbing'}
        </button>
      </form>

      {/* ── Error ── */}
      {error && (
        <div className="alert alert-error">
          <span>{error}</span>
        </div>
      )}

      {/* ── Progress ── */}
      {job && (
        <div className="card bg-base-100 shadow">
          <div className="card-body space-y-4">
            {/* Status header */}
            <div className="flex items-center justify-between">
              <h2 className="card-title text-lg">
                {isRunning && (
                  <span className="badge badge-primary badge-sm">Running</span>
                )}
                {job.status === 'completed' && (
                  <span className="badge badge-success badge-sm">Completed</span>
                )}
                {job.status === 'failed' && (
                  <span className="badge badge-error badge-sm">Failed</span>
                )}
              </h2>
              <span className="text-sm text-muted-foreground">
                Elapsed: {formatTime(job.elapsed)}
              </span>
            </div>

            {/* Progress bar */}
            <div>
              <div className="flex justify-between text-sm mb-1">
                <span>
                  {job.completed} / {job.total} videos
                  {job.failed > 0 && (
                    <span className="text-error ml-2">
                      ({job.failed} failed)
                    </span>
                  )}
                </span>
                <span className="font-mono">{pct.toFixed(1)}%</span>
              </div>
              <progress
                className="progress progress-primary w-full"
                value={pct}
                max="100"
              />
            </div>

            {/* Current activity */}
            {isRunning && job.current_video && (
              <div className="bg-base-200 rounded-lg p-3 text-sm">
                <div className="flex items-center gap-2">
                  <span className="loading loading-spinner loading-sm" />
                  <span className="font-medium truncate">
                    {job.current_video}
                  </span>
                </div>
                {job.current_stage && (
                  <p className="text-muted-foreground mt-1 ml-6">
                    {STAGE_LABELS[job.current_stage] || job.current_stage}…
                  </p>
                )}
                {isRunning && job.current_stage && job.stage_total > 0 && (
                  <div className="ml-6 mt-2">
                    <div className="flex justify-between text-xs text-muted-foreground mb-1">
                      <span>Stage progress</span>
                      <span className="font-mono">
                        {job.stage_progress} / {job.stage_total}
                        {' '}
                        ({((job.stage_progress / job.stage_total) * 100).toFixed(0)}%)
                      </span>
                    </div>
                    <progress
                      className="progress progress-secondary w-full h-2"
                      value={(job.stage_progress / job.stage_total) * 100}
                      max="100"
                    />
                  </div>
                )}
              </div>
            )}

            {/* Job meta */}
            <div className="grid grid-cols-2 gap-2 text-xs text-muted-foreground">
              <div>
                <span className="font-medium">Input:</span>{' '}
                <span className="font-mono">{job.input_type}</span>
              </div>
              <div>
                <span className="font-medium">Voice:</span>{' '}
                <span className="font-mono">{job.voice}</span>
              </div>
              <div className="col-span-2">
                <span className="font-medium">Output:</span>{' '}
                <span className="font-mono break-all">{job.output_path}</span>
              </div>
            </div>

            {job.error && (
              <div className="alert alert-error text-sm">
                <span>{job.error}</span>
              </div>
            )}

            {/* Results list */}
            {job.results.length > 0 && (
              <div className="overflow-x-auto">
                <h3 className="text-sm font-semibold mb-2">
                  Processed Videos
                </h3>
                <table className="table table-xs">
                  <thead>
                    <tr>
                      <th>Video</th>
                      <th>Status</th>
                      <th>Duration</th>
                      <th>Time</th>
                    </tr>
                  </thead>
                  <tbody>
                    {job.results.map((r, i) => {
                      const isSkipped =
                        r.processing_time === 0 && r.status === 'ok';
                      return (
                        <tr key={i}>
                          <td className="max-w-xs truncate" title={r.name}>
                            {r.name}
                          </td>
                          <td>
                            {isSkipped ? (
                              <span
                                className="badge badge-ghost badge-xs"
                                title="Already dubbed — skipped"
                              >
                                SKIP
                              </span>
                            ) : r.status === 'ok' ? (
                              <span className="badge badge-success badge-xs">
                                OK
                              </span>
                              ) : (
                                <span
                                  className="badge badge-error badge-xs"
                                  title={r.error}
                                >
                                  FAIL
                                </span>
                              )}
                          </td>
                          <td>{r.duration ? formatTime(r.duration) : '—'}</td>
                          <td>
                            {r.processing_time
                              ? formatTime(r.processing_time)
                              : '—'}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}

            {isDone && (
              <div className="text-sm text-muted-foreground">
                {job.status === 'completed'
                  ? `Done. ${job.completed - job.failed}/${job.total} videos dubbed successfully.`
                  : 'Dubbing finished with errors.'}{' '}
                Output is at{' '}
                <code className="text-xs">{job.output_path}</code>.
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
