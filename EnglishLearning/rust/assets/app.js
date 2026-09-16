/* Sentence-Segmented Player UI (vanilla JS, served by the Rust binary). */

const $ = (id) => document.getElementById(id);
const media = $("media");
const listEl = $("sentences");
const metaEl = $("meta");
const countEl = $("count");
const nowPlaying = $("nowPlaying");
const filterEl = $("filter");
const loopBtn = $("loopBtn");
const stepBtn = $("stepBtn");
const errorEl = $("error");

let session = null;
let cues = [];
let activeIdx = -1;
let loopMode = false;
let playThrough = false;
let playThroughTimer = null;

function fmtTime(sec) {
  if (sec < 0 || !isFinite(sec)) return "–";
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${String(m).padStart(2, "0")}:${s.toFixed(3).padStart(6, "0")}`;
}

function showError(msg) {
  errorEl.textContent = msg;
  errorEl.classList.remove("hidden");
  setTimeout(() => errorEl.classList.add("hidden"), 6000);
}

function mediaSrc(sid) {
  return `/api/session/${encodeURIComponent(sid)}/media`;
}

async function init() {
  try {
    const res = await fetch("/api/sessions");
    const sessions = await res.json();
    if (Array.isArray(sessions) && sessions.length > 0) {
      await loadSession(sessions[0].session_id);
    } else {
      metaEl.textContent = "No session yet. POST /api/session or relaunch with a media file.";
    }
  } catch (e) {
    showError("Failed to load session: " + e.message);
  }
}

async function loadSession(sid) {
  const res = await fetch(`/api/session/${encodeURIComponent(sid)}`);
  if (!res.ok) throw new Error("session fetch failed");
  session = await res.json();
  cues = session.cues || [];
  media.src = mediaSrc(session.id);
  metaEl.textContent = `${basename(session.media_path)} · ${session.media_duration.toFixed(1)}s · ${cues.length} sentences`;
  renderList();
}

function basename(p) {
  const parts = String(p).split(/[\\\/]/);
  return parts[parts.length - 1] || p;
}

function esc(s) {
  return String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}

function renderList() {
  const q = (filterEl.value || "").trim().toLowerCase();
  listEl.innerHTML = "";
  let shown = 0;
  cues.forEach((cue, i) => {
    const text = cue.text || "";
    if (q && !text.toLowerCase().includes(q)) return;
    shown++;
    const li = document.createElement("li");
    li.dataset.i = i;
    li.setAttribute("role", "button");
    li.className = i === activeIdx ? "active" : "";
    li.innerHTML = `
      <div class="head">
        <span class="idx">${cue.index + 1}</span>
        <span class="time">${fmtTime(cue.start)} – ${fmtTime(cue.end)}</span>
      </div>
      <div class="txt">${esc(text)}</div>`;
    li.addEventListener("click", () => playIndex(i));
    listEl.appendChild(li);
  });
  countEl.textContent = `${shown} / ${cues.length}`;
}

function scrollIntoFull(el) {
  if (el && el.scrollIntoView) el.scrollIntoView({ block: "nearest", behavior: "smooth" });
}

function playIndex(i) {
  if (!session || i < 0 || i >= cues.length) return;
  const cue = cues[i];
  activeIdx = i;
  stopPlayThroughTimer();
  // Seek to the sentence start and stop the loop for a normal click.
  media.currentTime = cue.start;
  media.play().catch(() => {});
  if (playThrough) {
    scheduleSentenceEnd(i);
  }
  markActive(i);
}

function markActive(i) {
  activeIdx = i;
  const items = listEl.querySelectorAll("li");
  items.forEach((li) => li.classList.toggle("active", Number(li.dataset.i) === i));
  const active = Array.from(items).find((li) => Number(li.dataset.i) === i);
  scrollIntoFull(active);
  const cue = cues[i];
  if (cue) nowPlaying.textContent = `▶ ${cue.text}`;
}

function scheduleSentenceEnd(i) {
  stopPlayThroughTimer();
  const cue = cues[i];
  const guard = Math.max(0.05, cue.end - media.currentTime);
  let waited = 0;
  playThroughTimer = setInterval(() => {
    waited += 0.05;
    if (media.currentTime >= cue.end || media.currentTime < cue.start - 0.05) {
      stopPlayThroughTimer();
      if (loopMode && media.currentTime >= cue.start - 0.05) {
        media.currentTime = cue.start;
        media.play().catch(() => {});
        playThroughTimer = setInterval(() => {
          if (media.currentTime >= cue.end) {
            media.currentTime = cue.start;
            media.play().catch(() => {});
          }
        }, 100);
      } else if (i + 1 < cues.length && playThrough) {
        playIndex(i + 1);
      } else {
        media.pause();
      }
    } else if (waited > guard + 1.5) {
      // safety fallback
      stopPlayThroughTimer();
      if (i + 1 < cues.length && playThrough) playIndex(i + 1);
    }
  }, 50);
  // Also detect natural end via the media "ended"/"timeupdate".
  media.ontimeupdate = null;
  media.addEventListener("timeupdate", onTime);
}

function onTime() {
  if (!session || cues.length === 0) return;
  if (media.currentTime >= (cues[activeIdx] && cues[activeIdx].end)) {
    if (playThrough && activeIdx + 1 < cues.length) {
      playIndex(activeIdx + 1);
    } else {
      media.pause();
      stopPlayThroughTimer();
    }
  } else if (media.currentTime < (cues[activeIdx] && cues[activeIdx].start) - 0.1) {
    // user scrubbed backward
    stopPlayThroughTimer();
  }
}

function stopPlayThroughTimer() {
  if (playThroughTimer) {
    clearInterval(playThroughTimer);
    playThroughTimer = null;
  }
}

// ── controls ────────────────────────────────────────────────────────────
loopBtn.addEventListener("click", () => {
  loopMode = !loopMode;
  loopBtn.classList.toggle("active", loopMode);
  if (loopMode && activeIdx >= 0) styleLoop();
});

function styleLoop() {
  if (activeIdx < 0) return;
  if (loopMode && activeIdx >= 0) {
    if (playThroughTimer) stopPlayThroughTimer();
    if (media.currentTime >= (cues[activeIdx].end)) {
      media.currentTime = cues[activeIdx].start;
      media.play().catch(() => {});
    }
    playThroughTimer = setInterval(() => {
      if (media.currentTime >= cues[activeIdx].end) {
        media.currentTime = cues[activeIdx].start;
        media.play().catch(() => {});
      }
    }, 100);
  }
}

stepBtn.addEventListener("click", () => {
  playThrough = !playThrough;
  stepBtn.classList.toggle("active", playThrough);
  if (playThrough && activeIdx >= 0) {
    media.currentTime = cues[activeIdx].start;
    media.play().catch(() => {});
    scheduleSentenceEnd(activeIdx);
  } else {
    stopPlayThroughTimer();
    if (activeIdx >= 0) scheduleSentenceEnd(activeIdx);
  }
});

filterEl.addEventListener("input", renderList);

// ── keyboard ────────────────────────────────────────────────────────────
document.addEventListener("keydown", (e) => {
  if (["INPUT", "TEXTAREA"].includes(document.activeElement.tagName)) return;
  switch (e.key) {
    case "ArrowDown":
      e.preventDefault();
      if (activeIdx + 1 < cues.length) playIndex(activeIdx + 1);
      break;
    case "ArrowUp":
      e.preventDefault();
      if (activeIdx - 1 >= 0) playIndex(activeIdx - 1);
      break;
    case " ":
      e.preventDefault();
      if (media.paused) media.play(); else media.pause();
      break;
    case "l":
    case "L":
      loopBtn.click();
      break;
    case "Enter":
      if (activeIdx >= 0) { media.currentTime = cues[activeIdx].start; media.play().catch(() => {}); }
      break;
  }
});

// A sentence click should play, and if loop/step are on respect them.
media.addEventListener("play", () => {
  if (playThrough && activeIdx >= 0) scheduleSentenceEnd(activeIdx);
});

init().catch((e) => showError(e.message));