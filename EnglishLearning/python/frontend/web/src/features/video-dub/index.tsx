import { useState } from 'react';

/**
 * Video Dubbing page.
 *
 * Two modes:
 *
 *  1. "Auto-transcribe" — upload a video whose audio you can't understand
 *     (foreign language or heavy accent). The backend extracts the audio,
 *     transcribes / translates it to English subtitles (Whisper),
 *     regenerates a clean English voiceover, and merges it back with the
 *     original video.
 *
 *  2. "Silent video + subtitle" — upload an already-silent video (or one
 *     whose audio has been stripped) plus a ready .srt / .vtt file. The
 *     backend skips transcription and goes straight to voiceover generation
 *     + mux. Use this when you already have corrected/translated subtitles.
 *
 * Both modes share the same voice / rate / burn-subtitles / keep-audio
 * options and hit sibling endpoints on the backend.
 */

interface Cue {
  index: number;
  start: number;
  end: number;
  text: string;
}

interface DubStats {
  source_language?: string;
  translated?: boolean;
  whisper_model?: string;
  video_duration?: number;
  voice?: string;
  cues?: number;
  stretch_methods?: Record<string, number>;
  min_stretch_ratio?: number;
  max_stretch_ratio?: number;
  total_audio_duration?: number;
  burn_subtitles?: boolean;
  keep_original_audio?: boolean;
  mode?: string;
}

interface DubResponse {
  success: boolean;
  video_path?: string;
  audio_path?: string;
  subtitle_path?: string;
  stats?: DubStats;
  cues?: Cue[];
  message?: string;
}

type Mode = 'auto' | 'subtitle';

const VOICE_OPTIONS: { label: string; value: string }[] = [
  { label: 'Aria (en-US, Female)', value: 'en-US-AriaNeural' },
  { label: 'Guy (en-US, Male)', value: 'en-US-GuyNeural' },
  { label: 'Jenny (en-US, Female)', value: 'en-US-JennyNeural' },
  { label: 'Emma (en-GB, Female)', value: 'en-GB-EmmaNeural' },
  { label: 'Ryan (en-GB, Male)', value: 'en-GB-RyanNeural' },
];

const LANG_OPTIONS: { label: string; value: string }[] = [
  { label: 'English (accented)', value: 'en' },
  { label: 'Japanese', value: 'ja' },
  { label: 'Chinese', value: 'zh' },
  { label: 'Korean', value: 'ko' },
  { label: 'Spanish', value: 'es' },
  { label: 'French', value: 'fr' },
  { label: 'German', value: 'de' },
  { label: 'Auto (Whisper)', value: 'auto' },
];

const MODEL_OPTIONS: { label: string; value: string }[] = [
  { label: 'tiny (fastest, lower accuracy)', value: 'tiny' },
  { label: 'base (recommended)', value: 'base' },
  { label: 'small (better accuracy)', value: 'small' },
  { label: 'medium (slow)', value: 'medium' },
];

export function VideoDubPage() {
  const [mode, setMode] = useState<Mode>('auto');

  // Shared inputs
  const [voice, setVoice] = useState('en-US-AriaNeural');
  const [rate, setRate] = useState('+0%');
  const [burnSubtitles, setBurnSubtitles] = useState(false);
  const [keepOriginalAudio, setKeepOriginalAudio] = useState(false);
  const [originalAudioVolume, setOriginalAudioVolume] = useState(20);

  // Auto-transcribe mode inputs
  const [videoFile, setVideoFile] = useState<File | null>(null);
  const [sourceLanguage, setSourceLanguage] = useState('en');
  const [translate, setTranslate] = useState(false);
  const [whisperModel, setWhisperModel] = useState('base');

  // Silent-video + subtitle mode inputs
  const [silentVideoFile, setSilentVideoFile] = useState<File | null>(null);
  const [subtitleFile, setSubtitleFile] = useState<File | null>(null);

  const [loading, setLoading] = useState(false);
  const [progress, setProgress] = useState('');
  const [result, setResult] = useState<DubResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();

    if (mode === 'auto') {
      if (!videoFile) {
        setError('Please select a video file.');
        return;
      }
    } else {
      if (!silentVideoFile || !subtitleFile) {
        setError('Please select both a video file and a subtitle file.');
        return;
      }
    }

    setLoading(true);
    setError(null);
    setResult(null);
    setProgress(
      mode === 'auto'
        ? 'Uploading & extracting audio…'
        : 'Uploading video & subtitle…'
    );

    try {
      const formData = new FormData();
      const endpoint =
        mode === 'auto' ? '/api/video/dub' : '/api/video/dub-from-subtitle';

      formData.append('file', mode === 'auto' ? videoFile! : silentVideoFile!);
      formData.append('voice', voice);
      formData.append('rate', rate);
      formData.append('burn_subtitles', String(burnSubtitles));
      formData.append('keep_original_audio', String(keepOriginalAudio));
      formData.append('original_audio_volume', String(originalAudioVolume));

      if (mode === 'auto') {
        formData.append('source_language', sourceLanguage);
        formData.append('translate', String(translate));
        formData.append('whisper_model', whisperModel);
      } else {
        formData.append('subtitle', subtitleFile!);
      }

      const resp = await fetch(endpoint, {
        method: 'POST',
        body: formData,
      });
      const data: DubResponse = await resp.json();

      if (!resp.ok || !data.success) {
        throw new Error(data.message || `HTTP ${resp.status}`);
      }
      setResult(data);
      setProgress('');
    } catch (err) {
      setProgress('');
      setError(
        err instanceof Error
          ? err.message
          : 'An unknown error occurred while dubbing the video.'
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
        <h1 className="text-3xl font-bold tracking-tight">🎬 Video Dubbing</h1>
        <p className="text-muted-foreground">
          Replace a video's audio with a clean English voiceover that matches
          the original timing, then merge it back with the video.
        </p>
      </div>

      {/* Mode tabs */}
      <div role="tablist" className="tabs tabs-boxed max-w-2xl">
        <button
          role="tab"
          className={`tab ${mode === 'auto' ? 'tab-active' : ''}`}
          onClick={() => setMode('auto')}
        >
          Auto-transcribe
        </button>
        <button
          role="tab"
          className={`tab ${mode === 'subtitle' ? 'tab-active' : ''}`}
          onClick={() => setMode('subtitle')}
        >
          Silent video + subtitle
        </button>
      </div>

      <form onSubmit={handleSubmit} className="space-y-4 max-w-2xl">
        {mode === 'auto' ? (
          <>
            <div className="space-y-2">
              <label className="text-sm font-medium">Source video</label>
              <input
                type="file"
                accept="video/*"
                onChange={(e) => setVideoFile(e.target.files?.[0] ?? null)}
                className="file-input file-input-bordered w-full"
                required
              />
              {videoFile && (
                <p className="text-xs text-muted-foreground">
                  {videoFile.name} ({(videoFile.size / 1024 / 1024).toFixed(1)} MB)
                </p>
              )}
            </div>

            <div className="grid grid-cols-2 gap-4">
              <div className="space-y-2">
                <label className="text-sm font-medium">Source language</label>
                <select
                  value={sourceLanguage}
                  onChange={(e) => setSourceLanguage(e.target.value)}
                  className="select select-bordered w-full"
                >
                  {LANG_OPTIONS.map((l) => (
                    <option key={l.value} value={l.value}>
                      {l.label}
                    </option>
                  ))}
                </select>
              </div>

              <div className="space-y-2">
                <label className="text-sm font-medium">Whisper model</label>
                <select
                  value={whisperModel}
                  onChange={(e) => setWhisperModel(e.target.value)}
                  className="select select-bordered w-full"
                >
                  {MODEL_OPTIONS.map((m) => (
                    <option key={m.value} value={m.value}>
                      {m.label}
                    </option>
                  ))}
                </select>
              </div>
            </div>

            <label className="flex items-center gap-3 cursor-pointer">
              <input
                type="checkbox"
                checked={translate}
                onChange={(e) => setTranslate(e.target.checked)}
                className="checkbox checkbox-primary"
              />
              <span className="text-sm">
                Translate to English (for non-English videos)
              </span>
            </label>
            <p className="text-xs text-muted-foreground -mt-2">
              Leave unchecked for accented English (re-speak the same words
              cleanly). Check for foreign languages.
            </p>
          </>
        ) : (
          <>
            <div className="space-y-2">
              <label className="text-sm font-medium">Silent video</label>
              <input
                type="file"
                accept="video/*"
                onChange={(e) => setSilentVideoFile(e.target.files?.[0] ?? null)}
                className="file-input file-input-bordered w-full"
                required
              />
              {silentVideoFile && (
                <p className="text-xs text-muted-foreground">
                  {silentVideoFile.name} ({(silentVideoFile.size / 1024 / 1024).toFixed(1)} MB)
                </p>
              )}
            </div>

            <div className="space-y-2">
              <label className="text-sm font-medium">Subtitle file (.srt / .vtt)</label>
              <input
                type="file"
                accept=".srt,.vtt"
                onChange={(e) => setSubtitleFile(e.target.files?.[0] ?? null)}
                className="file-input file-input-bordered w-full"
                required
              />
              {subtitleFile && (
                <p className="text-xs text-muted-foreground">
                  {subtitleFile.name}
                </p>
              )}
              <p className="text-xs text-muted-foreground">
                The voiceover will be timed to match these subtitles exactly.
              </p>
            </div>
          </>
        )}

        {/* Shared options */}
        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-2">
            <label className="text-sm font-medium">Voiceover voice</label>
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
            <label className="text-sm font-medium">Speech rate</label>
            <input
              type="text"
              value={rate}
              onChange={(e) => setRate(e.target.value)}
              placeholder="+0%"
              className="input input-bordered w-full"
            />
          </div>
        </div>

        <label className="flex items-center gap-3 cursor-pointer">
          <input
            type="checkbox"
            checked={burnSubtitles}
            onChange={(e) => setBurnSubtitles(e.target.checked)}
            className="checkbox checkbox-primary"
          />
          <span className="text-sm">Burn English subtitles into the video</span>
        </label>

        <label className="flex items-center gap-3 cursor-pointer">
          <input
            type="checkbox"
            checked={keepOriginalAudio}
            onChange={(e) => setKeepOriginalAudio(e.target.checked)}
            className="checkbox checkbox-primary"
          />
          <span className="text-sm">
            Keep original audio (mixed under voiceover — preserves music)
          </span>
        </label>

        {keepOriginalAudio && (
          <div className="space-y-2">
            <label className="text-sm font-medium">
              Original audio volume: {originalAudioVolume}%
            </label>
            <input
              type="range"
              min={0}
              max={100}
              value={originalAudioVolume}
              onChange={(e) => setOriginalAudioVolume(Number(e.target.value))}
              className="range range-primary"
            />
          </div>
        )}

        <button
          type="submit"
          disabled={loading || (mode === 'auto' ? !videoFile : !silentVideoFile || !subtitleFile)}
          className="btn btn-primary"
        >
          {loading ? 'Dubbing…' : 'Dub video'}
        </button>
        {progress && (
          <p className="text-sm text-muted-foreground">{progress}</p>
        )}
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
                Download dubbed MP4
              </a>
            </div>
          )}

          {result.stats && (
            <div className="card border bg-card p-4 space-y-2">
              <h2 className="text-lg font-semibold">Stats</h2>
              <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-sm">
                <dt className="text-muted-foreground">Mode</dt>
                <dd>
                  {result.stats.mode === 'subtitle'
                    ? 'Silent video + subtitle'
                    : 'Auto-transcribe'}
                </dd>
                {result.stats.source_language && (
                  <>
                    <dt className="text-muted-foreground">Source language</dt>
                    <dd>{result.stats.source_language}</dd>
                  </>
                )}
                {result.stats.translated !== undefined && (
                  <>
                    <dt className="text-muted-foreground">Translated</dt>
                    <dd>{result.stats.translated ? 'Yes' : 'No'}</dd>
                  </>
                )}
                {result.stats.whisper_model && (
                  <>
                    <dt className="text-muted-foreground">Whisper model</dt>
                    <dd className="font-mono text-xs">{result.stats.whisper_model}</dd>
                  </>
                )}
                <dt className="text-muted-foreground">Original duration</dt>
                <dd>{result.stats.video_duration?.toFixed(2)}s</dd>
                <dt className="text-muted-foreground">Cues</dt>
                <dd>{result.stats.cues}</dd>
                <dt className="text-muted-foreground">Voice</dt>
                <dd className="font-mono text-xs">{result.stats.voice}</dd>
                <dt className="text-muted-foreground">Stretch ratio range</dt>
                <dd>
                  {result.stats.min_stretch_ratio}x – {result.stats.max_stretch_ratio}x
                </dd>
                <dt className="text-muted-foreground">Stretch methods</dt>
                <dd className="font-mono text-xs">
                  {result.stats.stretch_methods
                    ? Object.entries(result.stats.stretch_methods)
                        .map(([k, v]) => `${k}: ${v}`)
                        .join(', ')
                    : '-'}
                </dd>
              </dl>
            </div>
          )}

          {result.cues && result.cues.length > 0 && (
            <div className="card border bg-card p-4 space-y-2">
              <h2 className="text-lg font-semibold">
                Transcript ({result.cues.length} cues)
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
