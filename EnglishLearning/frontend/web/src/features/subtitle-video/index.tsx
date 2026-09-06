import { useState } from 'react';

/**
 * Subtitle-to-Video generator page.
 *
 * Upload a .srt / .vtt subtitle file and the backend runs the
 * "Generate & Time-Stretch" pipeline (edge-tts -> audiostretchy -> pydub
 * -> ffmpeg) to produce a dubbed video whose audio perfectly matches the
 * subtitle timing.
 *
 * Mirrors the upload pattern of the existing SubtitlePage but targets the
 * new /api/subtitle-to-video endpoint.
 */
interface Cue {
  index: number;
  start: number;
  end: number;
  text: string;
}

interface Stats {
  cues: number;
  total_duration: number;
  voice: string;
  stretch_methods: Record<string, number>;
  min_stretch_ratio: number;
  max_stretch_ratio: number;
  work_dir?: string;
}

interface GenerateResponse {
  success: boolean;
  video_path?: string;
  audio_path?: string;
  subtitle_path?: string;
  stats?: Stats;
  cues?: Cue[];
  message?: string;
}

const VOICE_OPTIONS: { label: string; value: string }[] = [
  { label: 'Aria (en-US, Female)', value: 'en-US-AriaNeural' },
  { label: 'Guy (en-US, Male)', value: 'en-US-GuyNeural' },
  { label: 'Jenny (en-US, Female)', value: 'en-US-JennyNeural' },
  { label: 'Emma (en-GB, Female)', value: 'en-GB-EmmaNeural' },
  { label: 'Ryan (en-GB, Male)', value: 'en-GB-RyanNeural' },
  { label: 'Xiaoxiao (zh-CN, Female)', value: 'zh-CN-XiaoxiaoNeural' },
  { label: 'Yunxi (zh-CN, Male)', value: 'zh-CN-YunxiNeural' },
];

export function SubtitleVideoPage() {
  const [subtitleFile, setSubtitleFile] = useState<File | null>(null);
  const [backgroundVideo, setBackgroundVideo] = useState<File | null>(null);
  const [voice, setVoice] = useState('en-US-AriaNeural');
  const [rate, setRate] = useState('+0%');
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<GenerateResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!subtitleFile) {
      setError('Please select a subtitle file (.srt or .vtt).');
      return;
    }
    setLoading(true);
    setError(null);
    setResult(null);

    try {
      const formData = new FormData();
      formData.append('file', subtitleFile);
      formData.append('voice', voice);
      formData.append('rate', rate);
      if (backgroundVideo) {
        formData.append('background_video', backgroundVideo);
      }

      const resp = await fetch('/api/subtitle-to-video', {
        method: 'POST',
        body: formData,
      });
      const data: GenerateResponse = await resp.json();

      if (!resp.ok || !data.success) {
        throw new Error(data.message || `HTTP ${resp.status}`);
      }
      setResult(data);
    } catch (err) {
      setError(
        err instanceof Error
          ? err.message
          : 'An unknown error occurred while generating the video.'
      );
    } finally {
      setLoading(false);
    }
  };

  const downloadUrl = result?.video_path
    ? `/api/subtitle-to-video/download?path=${encodeURIComponent(result.video_path)}`
    : null;

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h1 className="text-3xl font-bold tracking-tight">🎬 Subtitle → Video</h1>
        <p className="text-muted-foreground">
          Upload an existing .srt / .vtt subtitle file. The backend will
          synthesise TTS for each cue, pitch-preserve time-stretch it to fit
          the exact subtitle slot, stitch everything with sample-accurate
          silence, and render a final video with burned-in subtitles.
        </p>
      </div>

      <form onSubmit={handleSubmit} className="space-y-4 max-w-2xl">
        <div className="space-y-2">
          <label className="text-sm font-medium">Subtitle file (.srt / .vtt)</label>
          <input
            type="file"
            accept=".srt,.vtt"
            onChange={(e) =>
              setSubtitleFile(e.target.files?.[0] ?? null)
            }
            className="file-input file-input-bordered w-full"
            required
          />
        </div>

        <div className="space-y-2">
          <label className="text-sm font-medium">Voice</label>
          <select
            value={voice}
            onChange={(e) => setVoice(e.target.value)}
            className="select select-bordered w-full"
          >
            {VOICE_OPTIONS.map((v) => (
              <option key={v.value} value={v.value}>
                {v.label}
              </option>
            ))}
          </select>
        </div>

        <div className="space-y-2">
          <label className="text-sm font-medium">
            Speech rate (e.g. +10%, -5%)
          </label>
          <input
            type="text"
            value={rate}
            onChange={(e) => setRate(e.target.value)}
            placeholder="+0%"
            className="input input-bordered w-full"
          />
        </div>

        <div className="space-y-2">
          <label className="text-sm font-medium">
            Optional background video (audio will be replaced)
          </label>
          <input
            type="file"
            accept="video/*"
            onChange={(e) =>
              setBackgroundVideo(e.target.files?.[0] ?? null)
            }
            className="file-input file-input-bordered w-full"
          />
          <p className="text-xs text-muted-foreground">
            If omitted, a solid coloured video is generated automatically.
          </p>
        </div>

        <button
          type="submit"
          disabled={loading || !subtitleFile}
          className="btn btn-primary"
        >
          {loading ? 'Generating…' : 'Generate video'}
        </button>
      </form>

      {error && (
        <div className="alert alert-error max-w-2xl">
          <span>{error}</span>
        </div>
      )}

      {result && (
        <div className="space-y-4 max-w-3xl">
          <div className="alert alert-success">
            <span>{result.message}</span>
          </div>

          {downloadUrl && (
            <div className="space-y-2">
              <video
                src={downloadUrl}
                controls
                className="w-full rounded-lg border bg-black"
              />
              <a
                href={downloadUrl}
                download
                className="btn btn-outline btn-sm"
              >
                Download MP4
              </a>
            </div>
          )}

          {result.stats && (
            <div className="card border bg-card p-4 space-y-2">
              <h2 className="text-lg font-semibold">Stats</h2>
              <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-sm">
                <dt className="text-muted-foreground">Cues</dt>
                <dd>{result.stats.cues}</dd>
                <dt className="text-muted-foreground">Total duration</dt>
                <dd>{result.stats.total_duration.toFixed(2)}s</dd>
                <dt className="text-muted-foreground">Voice</dt>
                <dd className="font-mono text-xs">{result.stats.voice}</dd>
                <dt className="text-muted-foreground">Stretch ratio range</dt>
                <dd>
                  {result.stats.min_stretch_ratio}x – {result.stats.max_stretch_ratio}x
                </dd>
                <dt className="text-muted-foreground">Stretch methods</dt>
                <dd className="font-mono text-xs">
                  {Object.entries(result.stats.stretch_methods)
                    .map(([k, v]) => `${k}: ${v}`)
                    .join(', ')}
                </dd>
              </dl>
            </div>
          )}

          {result.cues && result.cues.length > 0 && (
            <div className="card border bg-card p-4 space-y-2">
              <h2 className="text-lg font-semibold">
                Cues ({result.cues.length})
              </h2>
              <ul className="max-h-72 overflow-auto space-y-1 text-sm">
                {result.cues.map((c) => (
                  <li key={c.index} className="flex gap-3">
                    <span className="font-mono text-xs text-muted-foreground w-28">
                      {c.start.toFixed(2)} → {c.end.toFixed(2)}
                    </span>
                    <span>{c.text}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
