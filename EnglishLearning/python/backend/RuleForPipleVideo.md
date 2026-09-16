# Rule For Pipeline Video (READ THIS FIRST)

> **Every agent session MUST read this document before starting any
> video-dubbing work.** Failure to follow this protocol has already
> caused total loss of 13 dubbed videos (hours of processing) — see
> "The Incident" below.

This document covers three things every session must do:
1. **Git setup** — so pushes actually reach the remote.
2. **CPU-only environment setup** — so the pipeline runs without a GPU.
3. **Per-video push rule** — push after EACH video, not at the end.

---

## 1. The Incident (why this doc exists)

The safe dubbing pipeline (`run_safe_pipeline.py`) processed 13 videos
successfully. Each video was committed locally (`feat: dub ...`). The
final `git push` failed because no GitHub authentication was configured
in the sandbox:

```
[git] push FAILED: fatal: could not read Username for 'https://github.com':
        terminal prompts disabled
```

All 13 commits sat locally (never pushed). When the workspace was
fresh-cloned, **all 13 dubbed videos, SRTs, and commits were wiped out**.
`git fsck --lost-found` found nothing — unrecoverable.

**Root causes:**
- Git auth was never set up, so pushes silently failed.
- The pipeline only pushed once at the very end (instead of per-video).

**This document exists so neither mistake happens again.**

---

## 2. The Workspace Is Ephemeral

This workspace can be fresh-cloned from the remote at any time
(`https://github.com/Shangxpss/FinallyMicroService`). When that happens:

| What | Survives fresh clone? |
|---|---|
| Commits pushed to GitHub | Yes |
| Local-only commits (`git commit`) | **No — lost forever** |
| `.git/config` (user, remote URL, PAT) | No |
| `~/.git-credentials`, `~/.ssh/` | No |
| Untracked files (`video/*.mp4`, `audio/*.mp4`) | No |
| The Python `.venv/` | No (must be recreated) |

**Only pushed commits survive. Everything else is destroyed.**

---

## 3. Git Setup (PAT — run this at session start)

GitHub no longer accepts account passwords for `git push` over HTTPS.
You must use a **Personal Access Token (PAT)**. In a non-interactive
sandbox (no TTY, `CI=true`), the PAT must be embedded in the remote URL
so `git push` never prompts for credentials.

### Step 0 — Create a PAT (one-time, done by the user on GitHub)

1. Go to **GitHub → Settings → Developer settings → Personal access
   tokens → Tokens (classic)**.
2. Click **Generate new token (classic)**.
3. **Scopes: check only `repo`** (Full control of private repositories).
   This single scope covers clone, push, and force-add of gitignored
   files. Leave all other scopes unchecked — they're unnecessary and
   add security risk.
4. Expiration: 90 days recommended.
5. Click **Generate token** and **copy it immediately**
   (`ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx`). GitHub won't show it
   again.

> The user does this once. The agent never creates or rotates the token.

### Step 1 — Configure git (agent runs this at session start)

**Every session, before doing any work, the agent MUST run:**

```bash
cd /workspace

# Identity (matches the GitHub account that owns the repo)
git config user.name  "Shangxpss"
git config user.email "270291223@qq.com"

# Embed PAT in remote URL so non-interactive push works.
# Replace <PAT> with the actual token the user provides.
git remote set-url origin https://Shangxpss:<PAT>@github.com/Shangxpss/FinallyMicroService
```

### Step 2 — Verify push works (agent runs immediately after Step 1)

```bash
git remote get-url origin     # token should be masked as ***
git ls-remote --heads origin  # dry-run: confirms auth works
```

If `git ls-remote` succeeds, push is ready. If it fails with `401` /
`Authentication failed`, the PAT is wrong or expired — ask the user for
a fresh one. **Do not proceed with work until push is verified.**

---

## 4. PER-VIDEO PUSH RULE (mandatory)

> **CRITICAL: After EACH video finishes dubbing, the agent MUST commit
> AND push that video to the remote BEFORE starting the next video.**
>
> **Never defer all pushes to the end of the batch.** A fresh clone
> mid-batch would destroy every locally-committed video.

The original `run_safe_pipeline.py` commits locally per-video but only
pushes once at the very end. **That is unsafe and must be changed.**

### Required pipeline behavior

After video N is dubbed, before video N+1 begins:

1. `git add` the dubbed `.mp4` + `.srt` + `.words.srt` + `.segments.srt`
2. `git commit -m "feat: dub {stem}"`
3. `git push origin HEAD:master`  ← **this step is mandatory per video**
4. Verify the commit appears on `origin/master`

### Code change for `run_safe_pipeline.py`

In `main()`, move `git_push_remote()` inside the per-video loop so it
fires right after each successful commit:

```python
if success:
    print(f"\n  [done] {elapsed:.1f}s — committing + pushing...")
    git_ok = git_commit_local(stem, AUDIO_DIR)
    if git_ok:
        git_push_remote()          # <-- push per video, not just at end
    results.append((video_name, success, elapsed, git_ok))
else:
    results.append((video_name, False, elapsed, False))
    print(f"\n  [fail] {elapsed:.1f}s — continuing to next video")
```

And remove the single end-of-batch push in `main()` (keep it only as a
final safety net, but the per-video push is the real safeguard).

### Why per-video push is mandatory

- The sandbox can be re-cloned at any moment without warning.
- A 13-video batch takes hours. Losing it mid-batch is unacceptable.
- Pushing per-video adds ~1-2 seconds (network only). Negligible vs.
  the 200-2400s each video takes to process.
- Git pushes are idempotent: re-pushing an already-pushed commit is a
  no-op. There is no harm in pushing "too often".

### Invariant

> **After video N is dubbed, commit `feat: dub {stem}` MUST exist on
> `origin/master` before video N+1 begins processing.**

---

## 5. CPU-Only Environment Setup (no GPU available)

> **TIP: CPU-only packages are completely fine. There is no GPU in this
> sandbox, and none is needed.** Do not install CUDA builds on purpose,
> and do not waste time trying to enable GPU. The CPU-only path is the
> intended and supported mode.

### Why CPU-only is fine

- `faster-whisper` with `compute_type="int8"` on CPU is fast enough
  (5-15s per minute of audio for the `base` model).
- `stable-ts` pulls in `torch` (which may bundle CUDA), but we force
  CPU at runtime via `CUDA_VISIBLE_DEVICES=""`. The CUDA bundles are
  just unused — no harm, no GPU needed.
- `edge-tts` is a network call to Microsoft's TTS service — no local
  compute at all.
- `opencv-python-headless` does watermark inpainting on CPU — fine for
  1080p video.

### One-time setup (run after every fresh clone)

```bash
cd /workspace/services/EnglishLearning/backend

# 1. Create venv. Python 3.12 required (3.13/3.14 break numba/llvmlite).
uv venv --python 3.12 .venv

# 2. Core pipeline deps (CPU). opencv-headless = no GUI libs.
uv pip install --python .venv/bin/python \
    faster-whisper \
    av \
    opencv-python-headless \
    soundfile \
    numpy \
    edge-tts \
    huggingface-hub \
    pydub \
    audiostretchy

# 3. librosa — MUST be >=0.10 (ships numba/llvmlite wheels for py3.12).
#    Older versions pull llvmlite 0.36 which only supports Python <3.10.
uv pip install --python .venv/bin/python "librosa>=0.10.0"

# 4. Word-level alignment backend. stable-ts is the default WordAligner
#    backend. It pulls in torch (CUDA-bundled is FINE — we force CPU at
#    runtime via CUDA_VISIBLE_DEVICES="", see verification below).
uv pip install --python .venv/bin/python stable-ts
```

### CPU enforcement (no GPU needed, no GPU used)

Two mechanisms force CPU even though `stable-ts` installs torch with
CUDA bundles:

1. `CUDA_VISIBLE_DEVICES=""` — hides all GPUs from torch.
2. `WordAligner(device="cpu", compute_type="int8")` — faster-whisper
   on CPU with int8 quantization.

Verify:

```bash
CUDA_VISIBLE_DEVICES= .venv/bin/python -c "import torch; print('cuda:', torch.cuda.is_available())"
# → cuda: False
```

### Running the safe pipeline (with watermark removal)

```bash
cd /workspace/services/EnglishLearning/backend

DUB_REMOVE_WATERMARK=1 \
HF_ENDPOINT=https://hf-mirror.com \
HF_HUB_DISABLE_XET=1 \
CUDA_VISIBLE_DEVICES= \
.venv/bin/python -u run_safe_pipeline.py
```

Environment variables:

| Variable | Purpose | Default |
|---|---|---|
| `DUB_REMOVE_WATERMARK` | `1` = enable watermark removal stage 0; `0` = skip | `1` |
| `HF_ENDPOINT` | HuggingFace mirror (sandbox can't reach hf.co directly) | `https://hf-mirror.com` |
| `HF_HUB_DISABLE_XET` | `1` = disable xet protocol (bypasses mirror, 401s in sandbox) | `1` |
| `CUDA_VISIBLE_DEVICES` | empty = hide GPU, force CPU | empty |

### Pipeline stages (per video)

0. **Watermark removal** — `app/services/watermark.py`. Produces
   `{stem}_clean.mp4` via OpenCV inpainting (TELEA). All downstream
   stages read from this cleaned intermediate, never from the original.
   Region: `WATERMARK_REGION = [(0, 1045, 270, 30)]` (bottom-left strip
   on 1920x1080 source).
1. Duration probe (PyAV)
2. Word alignment (faster-whisper via stable-ts, int8, CPU)
3. Cue building (pure Python)
4. Subtitles → `.srt` / `.words.srt` / `.segments.srt`
5. Respeed synthesis (edge-tts, two-pass for overflow)
6. Placement stitching (loudness norm + room tone)
7. Muxing (PyAV remux + AAC 128k) → `{stem}_dubbed_new.mp4` (temp)

On success: original `{stem}.mp4` is atomically replaced by the dubbed
version (`os.replace`), SRTs are finalized, `_clean.mp4` is deleted.
On failure: all temp files cleaned up; original preserved untouched.

### Input / output locations

- **Input videos:** `video/` (e.g. `video/34 - ....mp4`)
- **Output (dubbed):** `audio/` (created by the pipeline)
  - `audio/{stem}.mp4` — dubbed video (replaces original)
  - `audio/{stem}.srt` — sentence-level subtitles
  - `audio/{stem}.words.srt` — word-level subtitles
  - `audio/{stem}.segments.srt` — raw Whisper segments

If `run_safe_pipeline.py` points `AUDIO_DIR` at `audio/`, make sure
input videos are moved there first (or adjust the script to read from
`video/` and write to `audio/`).

### Resume / safety guarantees

- Original video is **never** deleted/overwritten during processing.
- Dubbed output is written to `{stem}_dubbed_new.mp4` (temp name).
- Only after successful muxing does `os.replace` atomically swap.
- `find_videos()` skips videos that already have a sibling `.srt` file
  (resume marker) — interrupted batches pick up where they left off.

---

## 6. Agent Checklist (run at the start of EVERY session)

```
[ ] 1. Read this document (RuleForPipleVideo.md) first.
[ ] 2. Ask the user for a PAT if one is not already in the remote URL.
[ ] 3. Run git config + remote set-url with PAT (Step 1 above).
[ ] 4. Run git ls-remote — verify auth works (Step 2). Do NOT proceed
       until this passes.
[ ] 5. Recreate the .venv if it's missing (CPU-only setup, section 5).
[ ] 6. Begin work.
[ ] 7. After EACH video finishes: commit + PUSH (not just commit).
       Verify the commit appears on origin/master before next video.
[ ] 8. At session end: final push + verify origin/master matches local.
```

---

## 7. Common Issues

**`fatal: could not read Username`** — No PAT in remote URL. Run Step 1.

**`remote: Invalid username or token` / 401** — PAT wrong/expired. User
generates a new one.

**`remote: Permission denied`** — PAT lacks `repo` scope. Regenerate
with `repo` scope checked.

**`! [rejected] master -> master (fetch first)`** — Remote has commits
local doesn't. Run `git pull --rebase origin master` then push.

**`ImportError: No aligner backend available`** — `stable-ts` not
installed. Run setup step 4.

**`llvmlite ... only versions >=3.6,<3.10 are supported`** — Old librosa
pulled llvmlite 0.36. Reinstall with `uv pip install "librosa>=0.10.0"`
which resolves to numba 0.66 + llvmlite 0.48 (py3.12 compatible).

**`CUDA out of memory` / GPU not desired** — Ensure
`CUDA_VISIBLE_DEVICES=` is set to EMPTY (not `0`). Verify with
`torch.cuda.is_available() == False`. CPU-only is the intended mode.

**HF model download 401 / timeout** — `HF_HUB_DISABLE_XET=1` is required
in sandboxes. The xet protocol bypasses `HF_ENDPOINT` and hits
`cas-server.xethub.hf.co` directly, returning 401.

**Pipeline pushed nothing / work lost after re-clone** — You didn't
follow the per-video push rule (section 4). Fix the pipeline to push
after each video, then re-run.

---

## 8. Security Notes

- The PAT is stored in plaintext in `.git/config` (remote URL). This is
  acceptable in an ephemeral sandbox that gets wiped. **Never** commit
  `.git/config` or paste the PAT into any tracked file.
- If a PAT leaks (e.g. accidentally committed), revoke it immediately on
  GitHub: **Settings → Developer settings → Personal access tokens →
  Delete**.
- The PAT in the remote URL is visible to any process that can read
  `.git/config`. In this single-user sandbox that's fine.

---

## 9. Summary (the three rules)

1. **Set up git auth (PAT) at session start.** No auth → no push → work
   lost on re-clone.
2. **CPU-only is fine.** No GPU available, none needed. Use
   `CUDA_VISIBLE_DEVICES=""` + `compute_type="int8"`.
3. **Push after EVERY video.** Not at the end. After video N is dubbed,
   its commit must be on `origin/master` before video N+1 starts.
