//! The one self-contained page `acos view` serves: inline CSS and JS, no CDN,
//! no external request. The token travels only in `location.search` (the URL the
//! agent opened), so every API call appends it to its own query string.

/// The page (inline CSS/JS; no network beyond this origin).
pub const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>ACOS live view</title>
<link rel="icon" href="data:,">
<style>
  :root {
    color-scheme: dark;
    --bg: #0d1117; --panel: #161b22; --line: #30363d; --text: #e6edf3;
    --muted: #8b949e; --accent: #4f9cf9; --ok: #3fb950; --bad: #f85149;
    --warn: #d29922; --mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--text);
    font: 14px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; }
  header { display: flex; align-items: baseline; gap: 16px; flex-wrap: wrap;
    padding: 14px 18px; border-bottom: 1px solid var(--line); }
  header h1 { font-size: 16px; margin: 0; letter-spacing: .02em; }
  header .spacer { flex: 1; }
  .pill { border: 1px solid var(--line); border-radius: 999px; padding: 2px 10px;
    color: var(--muted); font-size: 12px; white-space: nowrap; }
  .pill.ok { color: var(--ok); border-color: #1f6f36; }
  .pill.bad { color: var(--bad); border-color: #6e2a26; }
  .pill.warn { color: var(--warn); border-color: #7d5913; }
  .banner { border: 1px solid #7d5913; background: rgba(210, 153, 34, .14);
    color: var(--warn); border-radius: 8px; padding: 3px 10px; font-size: 12px;
    font-weight: 600; white-space: nowrap; }
  main { padding: 18px; }
  #grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 12px; }
  .card { background: var(--panel); border: 1px solid var(--line); border-radius: 10px;
    padding: 12px 14px; cursor: pointer; transition: border-color .12s, transform .12s; }
  .card:hover { border-color: var(--accent); transform: translateY(-1px); }
  .card.off { cursor: default; opacity: .6; }
  .card.off:hover { border-color: var(--line); transform: none; }
  .card .top { display: flex; gap: 8px; align-items: baseline; }
  .card .id { font: 12px/1.4 var(--mono); color: var(--muted); overflow: hidden;
    text-overflow: ellipsis; white-space: nowrap; }
  .card .pkg { font-weight: 600; margin: 4px 0 2px; overflow: hidden;
    text-overflow: ellipsis; white-space: nowrap; }
  .card .line { color: var(--muted); font-size: 12px; overflow: hidden;
    text-overflow: ellipsis; white-space: nowrap; }
  .card .mono { font-family: var(--mono); }
  .card .foot { display: flex; gap: 10px; margin-top: 8px; font-size: 11px; color: var(--muted); }
  .empty { color: var(--muted); padding: 32px 4px; }
  #detail { display: flex; flex-direction: column; height: 100vh; padding: 10px 14px; gap: 10px; overflow: hidden; }
  .bar { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; flex-shrink: 0; }
  .bar button { background: var(--panel); color: var(--text); border: 1px solid var(--line);
    border-radius: 8px; padding: 5px 11px; cursor: pointer; font-size: 12px; }
  .bar button:hover:not(:disabled) { border-color: var(--accent); }
  .bar button.active { border-color: var(--accent); background: rgba(79, 156, 249, .18); color: var(--accent); }
  .bar button:disabled { opacity: .45; cursor: not-allowed; }
  .bar .actions { display: flex; gap: 5px; align-items: center; flex-wrap: wrap; }
  .bar .sep { width: 1px; height: 18px; background: var(--line); margin: 0 2px; }
  .bar .spacer { flex: 1; }
  .bar .titles { min-width: 0; max-width: 320px; }
  .bar .titles .big { font-weight: 600; font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .bar .titles .small { color: var(--muted); font-size: 11px; font-family: var(--mono);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .stage { flex: 1; min-height: 0; display: grid; grid-template-columns: minmax(0, 1fr) 310px;
    gap: 14px; align-items: stretch; }
  .screenwrap { position: relative; background: #000; border: 1px solid var(--line);
    border-radius: 12px; overflow: hidden; width: 100%; height: 100%; min-width: 0; min-height: 0;
    display: flex; align-items: center; justify-content: center;
    touch-action: none; user-select: none; outline: none; cursor: crosshair; }
  .screenwrap:focus-visible { border-color: var(--accent); }
  .screenwrap.watchonly { cursor: default; }
  #frame { display: block; width: 100%; height: 100%; object-fit: contain;
    pointer-events: none; -webkit-user-drag: none; user-select: none; }
  #gesture-svg { position: absolute; inset: 0; width: 100%; height: 100%;
    pointer-events: none; z-index: 2; overflow: hidden; }
  #offline { position: absolute; inset: auto 0 0 0; background: rgba(13,17,23,.82);
    color: var(--warn); font-size: 12px; padding: 6px 10px; z-index: 4; }
  #overlay { position: absolute; inset: 0; pointer-events: none; z-index: 3; overflow: hidden; }
  .touch-dot { position: absolute; width: 26px; height: 26px; margin: -13px 0 0 -13px;
    border: 2px solid var(--ok); background: rgba(63, 185, 80, .18); border-radius: 50%;
    pointer-events: none; }
  .touch-dot.mirror { border-color: var(--accent); background: rgba(79, 156, 249, .18); }
  .pinch-pivot { position: absolute; width: 8px; height: 8px; margin: -4px 0 0 -4px;
    border-radius: 50%; background: var(--accent); opacity: .8; pointer-events: none; }
  .trail-fade { transition: opacity .45s ease-out; opacity: 0; }
  .ripple { position: absolute; width: 44px; height: 44px; margin: -22px 0 0 -22px;
    border: 2px solid var(--accent); border-radius: 50%; opacity: .95;
    animation: ripple .9s ease-out forwards; }
  .ripple.panel { border-color: var(--ok); }
  .ripple.bad { border-color: var(--bad); }
  @keyframes ripple {
    from { transform: scale(.25); opacity: .95; }
    to { transform: scale(1.6); opacity: 0; }
  }
  .rip-tag { position: absolute; transform: translate(-50%, 26px); font: 11px/1 var(--mono);
    background: rgba(13,17,23,.85); border: 1px solid var(--line); border-radius: 6px;
    padding: 3px 6px; color: var(--text); animation: fade 1.4s ease-out forwards; white-space: nowrap; }
  @keyframes fade { from { opacity: 1; } to { opacity: 0; } }
  aside { background: var(--panel); border: 1px solid var(--line); border-radius: 12px;
    padding: 12px 14px; min-width: 0; min-height: 0; display: flex; flex-direction: column; overflow: hidden; }
  aside h2 { font-size: 11px; text-transform: uppercase; letter-spacing: .08em;
    color: var(--muted); margin: 0 0 6px; }
  #events { list-style: none; margin: 0; padding: 0; flex: 1; min-height: 0; overflow-y: auto; }
  #events li { border-top: 1px solid var(--line); padding: 6px 0; font-size: 12px;
    display: grid; grid-template-columns: 54px minmax(0, 1fr); gap: 8px; }
  #events li:first-child { border-top: 0; }
  #events .t { color: var(--muted); font-family: var(--mono); font-size: 11px; }
  #events .what { min-width: 0; overflow-wrap: anywhere; }
  #events .src { display: inline-block; font: 10px/1.3 var(--mono); border-radius: 4px;
    padding: 1px 5px; margin-right: 5px; border: 1px solid var(--line); color: var(--muted); }
  #events .src.panel { color: var(--accent); border-color: #264f78; background: rgba(79,156,249,.12); }
  #events .src.agent { color: var(--ok); border-color: #1f6f36; background: rgba(63,185,80,.10); }
  #events .cmd { font-weight: 600; }
  #events .st { font-family: var(--mono); font-size: 11px; }
  #events .st.ok { color: var(--ok); }
  #events .st.bad { color: var(--bad); }
  #events .txt { color: var(--muted); font-family: var(--mono); }
  .note { color: var(--muted); font-size: 11px; margin-top: 8px; flex-shrink: 0; }
  #cap-tray { position: fixed; left: 22px; bottom: 22px; z-index: 30;
    display: flex; flex-direction: column; gap: 10px; max-width: min(420px, calc(100vw - 44px));
    pointer-events: none; }
  .cap-card { pointer-events: auto; background: rgba(22, 27, 34, .97);
    border: 1px solid #7d5913; border-left: 3px solid var(--warn); border-radius: 10px;
    padding: 12px 14px; box-shadow: 0 12px 32px rgba(0, 0, 0, .55);
    backdrop-filter: blur(8px); animation: cap-in .18s ease-out; }
  .cap-card.info { border-color: #264f78; border-left-color: var(--accent); }
  @keyframes cap-in { from { opacity: 0; transform: translateY(8px); } to { opacity: 1; transform: none; } }
  .cap-head { display: flex; align-items: flex-start; gap: 8px; }
  .cap-badge { font: 10px/1.3 var(--mono); text-transform: uppercase; letter-spacing: .04em;
    border-radius: 4px; padding: 1px 6px; color: var(--warn); border: 1px solid #7d5913;
    background: rgba(210, 153, 34, .12); white-space: nowrap; flex-shrink: 0; }
  .cap-card.info .cap-badge { color: var(--accent); border-color: #264f78; background: rgba(79, 156, 249, .12); }
  .cap-title { font-weight: 600; font-size: 13px; flex: 1; min-width: 0; }
  .cap-x { background: transparent; border: 0; color: var(--muted); cursor: pointer;
    font-size: 16px; line-height: 1; padding: 0 2px; }
  .cap-x:hover { color: var(--text); }
  .cap-msg { color: var(--text); font-size: 12px; margin: 6px 0 6px; line-height: 1.45; }
  .cap-req { font: 11px/1.35 var(--mono); color: var(--muted); background: rgba(13, 17, 23, .7);
    border: 1px solid var(--line); border-radius: 6px; padding: 4px 7px;
    overflow-wrap: anywhere; margin-bottom: 10px; }
  .cap-actions { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; }
  .cap-btn { background: var(--bg); color: var(--text); border: 1px solid var(--line);
    border-radius: 7px; padding: 5px 10px; cursor: pointer; font-size: 12px; font-weight: 500; }
  .cap-btn:hover:not(:disabled) { border-color: var(--accent); }
  .cap-btn.primary { background: rgba(79, 156, 249, .18); border-color: var(--accent); color: #cde3ff; }
  .cap-btn.primary:hover:not(:disabled) { background: rgba(79, 156, 249, .28); }
  .cap-btn:disabled { opacity: .5; cursor: not-allowed; }
  .cap-status { font: 11px/1.3 var(--mono); margin-top: 7px; color: var(--muted); }
  .cap-status.ok { color: var(--ok); }
  .cap-status.bad { color: var(--bad); }
  body.watching { height: 100vh; overflow: hidden; }
  body.watching header { display: none; }
  @media (max-width: 820px) {
    .stage { grid-template-columns: minmax(0, 1fr); grid-template-rows: minmax(0, 1fr) 150px; }
    .bar .titles { max-width: 180px; }
    aside .note { display: none; }
  }
  [hidden] { display: none !important; }
</style>
</head>
<body>
<header>
  <h1>ACOS live view</h1>
  <span class="pill" id="status">connecting…</span>
  <span class="spacer"></span>
  <span class="pill">127.0.0.1 · token-gated · verify stays watch-only</span>
</header>
<main id="grid"></main>
<section id="detail" hidden>
  <div class="bar">
    <button id="back" type="button">← sessions</button>
    <div class="titles">
      <div class="big" id="dtitle">session</div>
      <div class="small" id="dsub"></div>
    </div>
    <span class="banner" id="verify-banner" hidden>watch-only: verify run</span>
    <div class="actions">
      <button id="btn-back" type="button" title="Back (Escape)">Back</button>
      <button id="btn-home" type="button" title="Home">Home</button>
      <button id="btn-recents" type="button" title="Recents">Recents</button>
      <button id="btn-rotate" type="button" title="Rotate 90°">Rotate</button>
      <span class="sep" aria-hidden="true"></span>
      <button id="btn-vol-down" type="button" title="Volume down">Vol−</button>
      <button id="btn-vol-up" type="button" title="Volume up">Vol+</button>
      <button id="btn-power" type="button" title="Send KEYCODE_POWER key event">Power key</button>
      <span class="sep" aria-hidden="true"></span>
      <button id="btn-pinch" type="button" aria-pressed="false" title="Two-finger pinch mode (or Alt + drag, or Ctrl + wheel)">Pinch</button>
      <button id="btn-shot" type="button" title="Save PNG screenshot" disabled>Shot</button>
    </div>
    <span class="spacer"></span>
    <span class="pill" id="dinput">idle</span>
    <span class="pill" id="dstate">stream</span>
  </div>
  <div class="stage">
    <div class="screenwrap" id="screenwrap" tabindex="0">
      <canvas id="frame" width="0" height="0" aria-label="live screen"></canvas>
      <svg id="gesture-svg" aria-hidden="true"></svg>
      <div id="overlay"></div>
      <div id="offline" hidden>stream offline · reconnecting…</div>
    </div>
    <aside>
      <h2>activity</h2>
      <div class="mono" id="dactivity" style="font-size:12px;color:var(--muted);margin-bottom:10px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">-</div>
      <h2>agent &amp; human activity</h2>
      <ol id="events" reversed></ol>
      <div class="note">Click: tap · Drag: swipe/fling · Hold: long-press · Wheel: scroll · Alt+drag / Ctrl+wheel: pinch · Type / Ctrl+V: text. Verify sessions stay read-only.</div>
    </aside>
  </div>
  <div id="cap-tray" aria-live="polite"></div>
</section>
<script>
"use strict";
const token = new URLSearchParams(location.search).get("t") || "";
const auth = (path) => path + (path.includes("?") ? "&" : "?") + "t=" + encodeURIComponent(token);
const $ = (id) => document.getElementById(id);
const grid = $("grid"), detail = $("detail"), statusPill = $("status");
const frame = $("frame"), frameCtx = frame.getContext("2d"), screenWrap = $("screenwrap"), gestureSvg = $("gesture-svg"), overlay = $("overlay"), eventsList = $("events"), capTray = $("cap-tray");
const btnBack = $("btn-back"), btnHome = $("btn-home"), btnRecents = $("btn-recents"), btnRotate = $("btn-rotate");
const btnVolDown = $("btn-vol-down"), btnVolUp = $("btn-vol-up"), btnPower = $("btn-power"), btnPinch = $("btn-pinch"), btnShot = $("btn-shot");
const verifyBanner = $("verify-banner"), dinput = $("dinput");
const SVG_NS = "http://www.w3.org/2000/svg";
const capabilityCards = new Map();

let sessions = [];
let current = null;
let currentGuestGen = null;
let source = null;       // EventSource
let display = { w: 0, h: 0 };
let frameSize = { w: 0, h: 0 };
let lastFrameSeq = 0;
let pendingRotate = null;
let currentRotation = 0;
let pollTimer = null;
let streamAbort = null;
let frameGeneration = 0;
let sessionEnded = false;
let sessionEverConnected = false;
let disconnectedPolls = 0;
let hasPolledSessions = false;

function afterLoad(fn) {
  if (document.readyState === "complete") fn();
  else window.addEventListener("load", fn, { once: true });
}

let ws = null;
let wsQueue = [];
const activePointers = new Map();
const heldGuestKeys = new Set();
let pinchMode = false;
let panelTouchState = { active: 0, maxPointers: 0, startX: 0, startY: 0, startTs: 0, gesture: null };

const KEY_MAP = {
  Backspace: "KEYCODE_DEL",
  Delete: "KEYCODE_FORWARD_DEL",
  Enter: "KEYCODE_ENTER",
  Escape: "KEYCODE_BACK",
  ArrowUp: "KEYCODE_DPAD_UP",
  ArrowDown: "KEYCODE_DPAD_DOWN",
  ArrowLeft: "KEYCODE_DPAD_LEFT",
  ArrowRight: "KEYCODE_DPAD_RIGHT",
  Home: "KEYCODE_MOVE_HOME",
  End: "KEYCODE_MOVE_END",
  PageUp: "KEYCODE_PAGE_UP",
  PageDown: "KEYCODE_PAGE_DOWN",
};

const KEY_NAMES = {
  3: "Home",
  4: "Back",
  19: "Up",
  20: "Down",
  21: "Left",
  22: "Right",
  24: "Vol+",
  25: "Vol−",
  26: "Power",
  29: "A",
  66: "Enter",
  67: "Del",
  112: "FwdDel",
  187: "Recents",
};

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function sessionTitle(s) {
  if (s.device_name) return s.device_name;
  const label = s.app_label || s.package || "session";
  return label + " · " + s.session_id;
}

function age(ms) {
  if (!(ms > 0)) return "-";
  const s = Math.floor(ms / 1000);
  if (s < 60) return s + "s";
  const m = Math.floor(s / 60);
  if (m < 60) return m + "m" + String(s % 60).padStart(2, "0") + "s";
  return Math.floor(m / 60) + "h" + String(m % 60).padStart(2, "0") + "m";
}

function clientLabel(s) {
  const c = s.client || {};
  const name = c.name || "-";
  return c.version ? name + " " + c.version : name;
}

function projectName(path) {
  if (!path) return "-";
  const parts = String(path).split("/").filter(Boolean);
  return parts.length ? parts[parts.length - 1] : path;
}

function applyToolbarUi() {
  const isWatchOnly = !!(current && current.watch_only);
  const noLauncher = !!(current && current.has_home === false);
  const inputBlocked = isWatchOnly || sessionEnded;
  verifyBanner.hidden = !isWatchOnly;
  screenWrap.classList.toggle("watchonly", isWatchOnly);

  btnBack.disabled = inputBlocked;
  btnBack.title = isWatchOnly ? "watch-only: verify run" : "Back (Escape)";

  btnHome.disabled = inputBlocked || noLauncher;
  btnHome.title = isWatchOnly
    ? "watch-only: verify run"
    : (noLauncher ? "no launcher in this session" : "Home");

  btnRecents.disabled = inputBlocked;
  btnRecents.title = isWatchOnly ? "watch-only: verify run" : "Recents";

  const rotatePending = isRotationPending();
  btnRotate.disabled = inputBlocked || rotatePending;
  btnRotate.title = isWatchOnly
    ? "watch-only: verify run"
    : (rotatePending ? "Waiting for rotated frame…" : "Rotate 90°");

  btnVolDown.disabled = inputBlocked;
  btnVolDown.title = isWatchOnly ? "watch-only: verify run" : "Volume down";

  btnVolUp.disabled = inputBlocked;
  btnVolUp.title = isWatchOnly ? "watch-only: verify run" : "Volume up";

  btnPower.disabled = inputBlocked;
  btnPower.title = isWatchOnly ? "watch-only: verify run" : "Send KEYCODE_POWER key event";

  if (inputBlocked && pinchMode) {
    pinchMode = false;
  }
  btnPinch.disabled = inputBlocked;
  btnPinch.classList.toggle("active", pinchMode);
  btnPinch.setAttribute("aria-pressed", String(pinchMode));
  btnPinch.title = isWatchOnly
    ? "watch-only: verify run"
    : "Two-finger pinch mode (or Alt + drag, or Ctrl + wheel)";

  btnShot.disabled = !hasDecodedFrame();

  if (isWatchOnly) {
    dinput.textContent = "watch-only";
    dinput.className = "pill warn";
  } else if (!ws) {
    dinput.textContent = "idle";
    dinput.className = "pill";
  }
}

function setAspect(w, h) {
  if (w > 0 && h > 0) {
    screenWrap.style.setProperty("--ar", w + " / " + h);
  }
}

let lastGridSig = "";

function gridSignature(list) {
  if (!list.length) return "empty";
  return JSON.stringify(list.map((s) => [
    s.session_id, s.stream, s.connected, s.watchers > 0, s.watch_only,
    s.has_home, s.device_name, s.app_label, s.package, s.activity,
    s.display ? s.display.w : 0, s.display ? s.display.h : 0,
    s.project_dir, clientLabel(s),
  ]));
}

function renderGrid() {
  const sig = gridSignature(sessions);
  if (sig === lastGridSig && grid.children.length === (sessions.length || 1)) {
    for (let i = 0; i < sessions.length; i++) {
      const card = grid.children[i];
      if (!card) continue;
      card._session = sessions[i];
      const ageEl = card.querySelector(".age");
      if (ageEl) ageEl.textContent = age(sessions[i].age_ms) + " old";
    }
    return;
  }
  lastGridSig = sig;
  grid.textContent = "";
  if (!sessions.length) {
    const empty = el("div", "empty", "No live sessions. Start one with acos_launch — this page refreshes by itself.");
    grid.appendChild(empty);
    return;
  }
  for (const s of sessions) {
    const card = el("div", "card" + (s.stream ? "" : " off"));
    card._session = s;
    const top = el("div", "top");
    top.appendChild(el("div", "id", s.session_id));
    const spacer = el("span"); spacer.style.flex = "1"; top.appendChild(spacer);
    if (s.watch_only) {
      top.appendChild(el("span", "pill warn", "watch-only"));
    }
    if (s.stream) {
      top.appendChild(el("span", "pill" + (s.connected ? " ok" : ""), s.watchers > 0 ? "watching" : "live"));
    } else {
      top.appendChild(el("span", "pill", "no stream"));
    }
    card.appendChild(top);
    card.appendChild(el("div", "pkg", sessionTitle(s)));
    const pkgLine = (s.package ? s.package + " · " : "") + (s.activity || "activity unknown");
    card.appendChild(el("div", "line mono", pkgLine));
    const proj = el("div", "line", "project: " + projectName(s.project_dir));
    proj.title = s.project_dir || "";
    card.appendChild(proj);
    const foot = el("div", "foot");
    foot.appendChild(el("span", null, clientLabel(s)));
    foot.appendChild(el("span", "age", age(s.age_ms) + " old"));
    foot.appendChild(el("span", null, (s.display ? s.display.w + "×" + s.display.h : "-")));
    card.appendChild(foot);
    if (s.stream) {
      card.addEventListener("click", () => openSession(card._session));
    }
    grid.appendChild(card);
  }
}

async function refresh() {
  try {
    const response = await fetch(auth("/api/sessions"), { cache: "no-store" });
    if (!response.ok) throw new Error("HTTP " + response.status);
    const data = await response.json();
    hasPolledSessions = true;
    sessions = data.sessions || [];
    statusPill.textContent = sessions.length + (sessions.length === 1 ? " live session" : " live sessions");
    statusPill.className = "pill ok";
    if (!current) renderGrid();
    if (current) {
      const updated = sessions.find((s) => s.session_id === current.session_id);
      if (!updated) {
        markSessionEnded(current.session_id);
      } else if (
        currentGuestGen !== null &&
        typeof updated.guest_gen === "number" &&
        updated.guest_gen !== currentGuestGen
      ) {
        markSessionEnded(current.session_id);
      } else {
        if (currentGuestGen === null && typeof updated.guest_gen === "number" && updated.guest_gen > 0) {
          currentGuestGen = updated.guest_gen;
        }
        if (updated.connected === true) {
          sessionEverConnected = true;
          disconnectedPolls = 0;
        } else if (updated.connected === false) {
          disconnectedPolls += 1;
          if (sessionEverConnected && !sessionEnded) {
            $("dstate").textContent = "reconnecting";
            $("dstate").className = "pill bad";
          }
        }
        if (!sessionEnded) {
          const prevHasHome = current.has_home;
          current = updated;
          if (typeof current.has_home !== "boolean" && typeof prevHasHome === "boolean") {
            current.has_home = prevHasHome;
          }
          $("dtitle").textContent = sessionTitle(updated);
          if (
            !pendingRotate &&
            updated.display && updated.display.w && updated.display.h &&
            (!frameSize.w || !frameSize.h ||
              ((updated.display.w < updated.display.h) === (frameSize.w < frameSize.h)))
          ) {
            display = { w: updated.display.w, h: updated.display.h };
            setAspect(display.w, display.h);
          }
          if (updated.activity) $("dactivity").textContent = updated.activity;
          applyToolbarUi();
        }
      }
    }
  } catch (error) {
    statusPill.textContent = "hub unreachable · " + error.message;
    statusPill.className = "pill bad";
  }
  clearTimeout(pollTimer);
  pollTimer = setTimeout(refresh, 2000);
}

function resetPanelTouchState() {
  panelTouchState = { active: 0, maxPointers: 0, startX: 0, startY: 0, startTs: 0, gesture: null };
}

function clearPointerVisuals(entry, fadeTrail) {
  if (!entry) return;
  if (entry.dotEl) { entry.dotEl.remove(); entry.dotEl = null; }
  if (entry.mirrorDotEl) { entry.mirrorDotEl.remove(); entry.mirrorDotEl = null; }
  if (entry.pivotEl) { entry.pivotEl.remove(); entry.pivotEl = null; }
  if (entry.pinchLineEl) { entry.pinchLineEl.remove(); entry.pinchLineEl = null; }
  if (entry.trailEl) {
    const trail = entry.trailEl;
    entry.trailEl = null;
    if (fadeTrail) {
      requestAnimationFrame(() => trail.classList.add("trail-fade"));
      setTimeout(() => trail.remove(), 480);
    } else {
      trail.remove();
    }
  }
}

function clearAllPointerVisuals() {
  for (const entry of activePointers.values()) {
    clearPointerVisuals(entry, false);
  }
  gestureSvg.textContent = "";
}

function closeWs() {
  clearAllPointerVisuals();
  activePointers.clear();
  heldGuestKeys.clear();
  pendingRotate = null;
  wsQueue = [];
  if (ws) {
    try { ws.close(); } catch (_) {}
    ws = null;
  }
}

function isTerminalInput(msg) {
  if (!msg) return false;
  if (msg.t === "touch") return msg.phase === "up" || msg.phase === "cancel";
  if (msg.t === "key") return msg.phase === "up";
  return false;
}

function hasDecodedFrame() {
  return frameSize.w > 0 && frameSize.h > 0 && frame.width > 0 && frame.height > 0;
}

function isRotationPending() {
  if (pendingRotate !== null) return true;
  if (display.w && display.h && frameSize.w && frameSize.h) {
    return (display.w < display.h) !== (frameSize.w < frameSize.h);
  }
  return false;
}

function cancelActivePointers() {
  if (activePointers.size === 0) return;
  const entries = Array.from(activePointers.entries());
  for (const [pid, entry] of entries) {
    clearPointerVisuals(entry, false);
    try {
      if (screenWrap.hasPointerCapture && screenWrap.hasPointerCapture(pid)) {
        screenWrap.releasePointerCapture(pid);
      }
    } catch (_) {}
  }
  activePointers.clear();
  const [firstId, firstEntry] = entries[0];
  if (firstEntry) {
    sendInput({ t: "touch", phase: "cancel", id: firstId, x: firstEntry.x, y: firstEntry.y });
  }
}

function releaseHeldGuestKeys() {
  if (heldGuestKeys.size === 0) return;
  const keys = Array.from(heldGuestKeys);
  heldGuestKeys.clear();
  for (const kc of keys) {
    sendInput({ t: "key", code: kc, phase: "up" });
  }
}

function ensureWs() {
  if (!current || current.watch_only || sessionEnded) return null;
  if (!(typeof currentGuestGen === "number" && currentGuestGen > 0)) return null;
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
    return ws;
  }
  const proto = location.protocol === "https:" ? "wss://" : "ws://";
  const path = "/api/input/" + encodeURIComponent(current.session_id) + "?g=" + encodeURIComponent(currentGuestGen);
  const url = proto + location.host + auth(path);
  const sock = new WebSocket(url);
  ws = sock;
  dinput.textContent = "connecting…";
  dinput.className = "pill";
  sock.onopen = () => {
    if (ws !== sock) return;
    dinput.textContent = "interactive";
    dinput.className = "pill ok";
    while (wsQueue.length > 0 && sock.readyState === WebSocket.OPEN) {
      sock.send(wsQueue.shift().raw);
    }
  };
  sock.onmessage = (ev) => {
    if (ws !== sock) return;
    let msg = null;
    try { msg = JSON.parse(ev.data); } catch (_) { return; }
    if (!msg) return;
    if (msg.ok === false && msg.code === "SESSION_REPLACED") {
      if (current) markSessionEnded(current.session_id);
      return;
    }
    if (msg.ok === false && msg.code === "QUEUE_FULL" && msg.disconnected) {
      closeWs();
      return;
    }
    if (msg.ok === false && msg.code === "VERIFY_WATCH_ONLY") {
      if (current) current.watch_only = true;
      applyToolbarUi();
      closeWs();
      return;
    }
    if (msg.ok === false && msg.code === "HOME_UNAVAILABLE") {
      if (current) current.has_home = false;
      applyToolbarUi();
      return;
    }
    if (msg.ok === false && msg.t === "rotate") {
      pendingRotate = null;
      applyToolbarUi();
      return;
    }
    if (msg.t === "capability") {
      handleCapabilityReply(msg);
      return;
    }
    if (msg.ok && msg.t === "rotate") {
      cancelActivePointers();
      if (msg.width > 0 && msg.height > 0) {
        display = { w: msg.width, h: msg.height };
        setAspect(display.w, display.h);
      }
      if (typeof msg.rotation === "number") currentRotation = msg.rotation;
      const targetSeq = Math.max(
        pendingRotate ? pendingRotate.minSeq : lastFrameSeq + 1,
        typeof msg.frame_seq === "number" ? msg.frame_seq : 0
      );
      const orientOk = !(msg.width > 0 && msg.height > 0) || (
        frameSize.w > 0 && frameSize.h > 0 &&
        ((msg.width < msg.height) === (frameSize.w < frameSize.h))
      );
      if (lastFrameSeq >= targetSeq && orientOk) {
        pendingRotate = null;
      } else {
        pendingRotate = {
          minSeq: targetSeq,
          expectedW: msg.width || 0,
          expectedH: msg.height || 0,
          awaitingReply: false,
        };
      }
      applyToolbarUi();
    }
  };
  sock.onclose = () => {
    if (ws !== sock) return;
    ws = null;
    wsQueue = [];
    pendingRotate = null;
    clearAllPointerVisuals();
    activePointers.clear();
    heldGuestKeys.clear();
    if (current) applyToolbarUi();
    if (current && !current.watch_only) {
      dinput.textContent = "idle";
      dinput.className = "pill";
    }
  };
  sock.onerror = () => {
    if (ws === sock && current && !current.watch_only) {
      dinput.textContent = "input error";
      dinput.className = "pill bad";
    }
  };
  return sock;
}

function sendInput(msg) {
  if (!current) return false;
  if (current.watch_only) {
    applyToolbarUi();
    return false;
  }
  const sock = ensureWs();
  if (!sock) return false;
  const raw = JSON.stringify(msg);
  if (sock.readyState === WebSocket.OPEN) {
    sock.send(raw);
  } else if (sock.readyState === WebSocket.CONNECTING) {
    // Coalesce only when the queue tail itself is a touch move for the same
    // pointer; never scan past intervening commands such as rotate.
    if (msg.t === "touch" && msg.phase === "move" && wsQueue.length > 0) {
      const tail = wsQueue[wsQueue.length - 1].msg;
      if (tail && tail.t === "touch" && tail.phase === "move" && tail.id === msg.id) {
        wsQueue[wsQueue.length - 1] = { msg, raw };
        return true;
      }
    }
    if (wsQueue.length >= 128) {
      if (isTerminalInput(msg)) {
        // Only evict a queued touch move; never evict a queued down/press.
        const evictIdx = wsQueue.findIndex(
          (item) => item.msg && item.msg.t === "touch" && item.msg.phase === "move"
        );
        if (evictIdx >= 0) {
          wsQueue.splice(evictIdx, 1);
        } else {
          closeWs();
          return false;
        }
      } else {
        return false;
      }
    }
    wsQueue.push({ msg, raw });
  }
  return true;
}

function renderedImageRect() {
  const rect = frame.getBoundingClientRect();
  const nw = frameSize.w || frame.width || display.w || 1080;
  const nh = frameSize.h || frame.height || display.h || 2400;
  if (!rect.width || !rect.height || !nw || !nh) return rect;
  const imgRatio = nw / nh;
  const boxRatio = rect.width / rect.height;
  let w = rect.width, h = rect.height, left = rect.left, top = rect.top;
  if (boxRatio > imgRatio) {
    w = rect.height * imgRatio;
    left = rect.left + (rect.width - w) / 2;
  } else {
    h = rect.width / imgRatio;
    top = rect.top + (rect.height - h) / 2;
  }
  return { left, top, width: w, height: h, right: left + w, bottom: top + h };
}

function clientToDisplay(clientX, clientY, clampOutside) {
  if (!hasDecodedFrame() || isRotationPending()) return null;
  const r = renderedImageRect();
  const dw = display.w || frameSize.w || frame.width || 1080;
  const dh = display.h || frameSize.h || frame.height || 2400;
  if (!r.width || !r.height) return null;
  let nx = (clientX - r.left) / r.width;
  let ny = (clientY - r.top) / r.height;
  if (!clampOutside && (nx < 0 || nx > 1 || ny < 0 || ny > 1)) return null;
  nx = Math.max(0, Math.min(1, nx));
  ny = Math.max(0, Math.min(1, ny));
  return {
    x: Math.max(0, Math.min(dw - 1, Math.round(nx * (dw - 1)))),
    y: Math.max(0, Math.min(dh - 1, Math.round(ny * (dh - 1)))),
  };
}

const CRLF2 = new Uint8Array([13, 10, 13, 10]);

function indexOfBytes(hay, needle, from) {
  const start = from || 0;
  const limit = hay.length - needle.length;
  outer: for (let i = start; i <= limit; i++) {
    for (let j = 0; j < needle.length; j++) {
      if (hay[i + j] !== needle[j]) continue outer;
    }
    return i;
  }
  return -1;
}

function concatBytes(a, b) {
  if (!a.length) return b;
  if (!b.length) return a;
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

function stopFrameStream() {
  frameGeneration += 1;
  if (streamAbort) {
    try { streamAbort.abort(); } catch (_) {}
    streamAbort = null;
  }
}

function clearFrameCanvas() {
  frameSize = { w: 0, h: 0 };
  lastFrameSeq = 0;
  if (frameCtx && frame.width && frame.height) {
    frameCtx.clearRect(0, 0, frame.width, frame.height);
  }
  frame.width = 0;
  frame.height = 0;
  btnShot.disabled = true;
}

async function drawDecodedFrame(part, sid, gen, ctrl) {
  const blob = new Blob([part.payload], { type: part.contentType || "image/png" });
  const bmp = await createImageBitmap(blob);
  if (
    ctrl.signal.aborted ||
    streamAbort !== ctrl ||
    frameGeneration !== gen ||
    sessionEnded ||
    !current ||
    current.session_id !== sid ||
    (currentGuestGen !== null && part.guestGen && part.guestGen !== currentGuestGen)
  ) {
    bmp.close();
    return;
  }
  const prevOrientationFlipped =
    frameSize.w > 0 && frameSize.h > 0 &&
    ((frameSize.w < frameSize.h) !== (bmp.width < bmp.height));
  if (prevOrientationFlipped) {
    cancelActivePointers();
  }
  if (frame.width !== bmp.width || frame.height !== bmp.height) {
    frame.width = bmp.width;
    frame.height = bmp.height;
  }
  frameSize = { w: bmp.width, h: bmp.height };
  if (typeof part.frameSeq === "number" && part.frameSeq > lastFrameSeq) {
    lastFrameSeq = part.frameSeq;
  }
  if (frameCtx) {
    frameCtx.drawImage(bmp, 0, 0);
  }
  bmp.close();
  btnShot.disabled = !hasDecodedFrame();
  markLive();
  setAspect(frameSize.w, frameSize.h);
  const wasRotatePending = isRotationPending();
  if (pendingRotate && !pendingRotate.awaitingReply) {
    const orientOk =
      !(pendingRotate.expectedW > 0 && pendingRotate.expectedH > 0) ||
      ((pendingRotate.expectedW < pendingRotate.expectedH) === (frameSize.w < frameSize.h));
    if (lastFrameSeq >= pendingRotate.minSeq && orientOk) {
      pendingRotate = null;
    }
  }
  if (!pendingRotate && display.w && display.h && ((display.w < display.h) !== (frameSize.w < frameSize.h))) {
    display = { w: display.h, h: display.w };
  }
  if (wasRotatePending !== isRotationPending()) {
    applyToolbarUi();
  }
}

async function startFrameStream(sid) {
  stopFrameStream();
  const gen = ++frameGeneration;
  const ctrl = new AbortController();
  streamAbort = ctrl;
  const decoder = new TextDecoder();
  let retryDelay = 400;

  while (!ctrl.signal.aborted && streamAbort === ctrl && current && current.session_id === sid && !sessionEnded) {
    let reader = null;
    try {
      const expectedGen = (typeof currentGuestGen === "number" && currentGuestGen > 0)
        ? currentGuestGen
        : null;
      const framePath = "/api/frames/" + encodeURIComponent(sid)
        + (expectedGen !== null ? "?g=" + encodeURIComponent(expectedGen) : "");
      const response = await fetch(auth(framePath), {
        cache: "no-store",
        signal: ctrl.signal,
      });
      if (response.status === 404 || response.status === 409) {
        markSessionEnded(sid);
        return;
      }
      if (!response.ok || !response.body) {
        throw new Error("HTTP " + response.status);
      }
      const headerGen = Number.parseInt(response.headers.get("X-Guest-Gen") || "", 10) || 0;
      if (headerGen > 0) {
        if (currentGuestGen !== null && headerGen !== currentGuestGen) {
          markSessionEnded(sid);
          return;
        }
        currentGuestGen = headerGen;
      }
      const streamGuestGen = headerGen || expectedGen || 0;
      reader = response.body.getReader();
      let buf = new Uint8Array(0);
      while (!ctrl.signal.aborted && streamAbort === ctrl && !sessionEnded) {
        const { value, done } = await reader.read();
        if (done) break;
        if (!value || !value.length) continue;
        buf = concatBytes(buf, value);
        let latest = null;
        while (true) {
          const hdrEnd = indexOfBytes(buf, CRLF2, 0);
          if (hdrEnd < 0) break;
          const hdrText = decoder.decode(buf.subarray(0, hdrEnd));
          let contentType = "image/png";
          let contentLength = -1;
          let frameSeq = 0;
          for (const line of hdrText.split(/\r?\n/)) {
            const colon = line.indexOf(":");
            if (colon <= 0) continue;
            const k = line.slice(0, colon).trim().toLowerCase();
            const v = line.slice(colon + 1).trim();
            if (k === "content-type") contentType = v;
            else if (k === "content-length") contentLength = Number.parseInt(v, 10);
            else if (k === "x-frame-seq") frameSeq = Number.parseInt(v, 10) || 0;
          }
          const bodyStart = hdrEnd + 4;
          if (!(contentLength > 0)) {
            buf = buf.slice(bodyStart);
            continue;
          }
          if (buf.length < bodyStart + contentLength) break;
          const payload = buf.slice(bodyStart, bodyStart + contentLength);
          let nextStart = bodyStart + contentLength;
          if (nextStart + 2 <= buf.length && buf[nextStart] === 13 && buf[nextStart + 1] === 10) {
            nextStart += 2;
          }
          buf = buf.slice(nextStart);
          latest = { contentType, payload, frameSeq, guestGen: streamGuestGen };
        }
        if (latest) {
          retryDelay = 400;
          await drawDecodedFrame(latest, sid, gen, ctrl);
        }
      }
    } catch (error) {
      if (ctrl.signal.aborted || streamAbort !== ctrl || sessionEnded) return;
    } finally {
      if (reader) {
        try { await reader.cancel(); } catch (_) {}
      }
    }
    if (ctrl.signal.aborted || streamAbort !== ctrl || !current || current.session_id !== sid || sessionEnded) {
      return;
    }
    const liveEntry = sessions.find((s) => s.session_id === sid);
    if (
      hasPolledSessions &&
      (!liveEntry ||
        (currentGuestGen !== null &&
          typeof liveEntry.guest_gen === "number" &&
          liveEntry.guest_gen !== currentGuestGen))
    ) {
      markSessionEnded(sid);
      return;
    }
    $("offline").textContent = "stream offline · reconnecting…";
    $("offline").hidden = false;
    $("dstate").textContent = "reconnecting";
    $("dstate").className = "pill bad";
    const delay = retryDelay;
    retryDelay = Math.min(retryDelay * 2, 5000);
    await new Promise((resolve) => {
      const t = setTimeout(resolve, delay);
      ctrl.signal.addEventListener("abort", () => {
        clearTimeout(t);
        resolve();
      }, { once: true });
    });
  }
}

function openSession(s) {
  closeWs();
  stopFrameStream();
  if (source) { source.close(); source = null; }
  clearFrameCanvas();
  resetPanelTouchState();
  pinchMode = false;
  overlay.textContent = "";
  gestureSvg.textContent = "";
  clearCapabilityPrompts();
  sessionEnded = false;
  sessionEverConnected = false;
  disconnectedPolls = 0;
  current = s;
  currentGuestGen = (typeof s.guest_gen === "number" && s.guest_gen > 0) ? s.guest_gen : null;
  pendingRotate = null;
  display = { w: (s.display && s.display.w) || 0, h: (s.display && s.display.h) || 0 };
  currentRotation = (display.w > display.h) ? 1 : 0;
  grid.hidden = true;
  detail.hidden = false;
  document.body.classList.add("watching");
  $("dtitle").textContent = sessionTitle(s);
  $("dsub").textContent = s.session_id + " · " + clientLabel(s) + " · " + projectName(s.project_dir);
  $("dactivity").textContent = s.activity || "-";
  eventsList.textContent = "";
  $("dstate").textContent = s.stream ? "connecting" : "no stream";
  $("dstate").className = "pill";
  $("offline").textContent = "stream offline · reconnecting…";
  $("offline").hidden = true;
  applyToolbarUi();
  if (s.stream) {
    setAspect(display.w, display.h);
    connectEvents(s);
    const sid = s.session_id;
    afterLoad(() => {
      if (current && current.session_id === sid && !sessionEnded) {
        startFrameStream(sid);
      }
    });
  }
  location.hash = "s=" + encodeURIComponent(s.session_id);
  history.replaceState(null, "", location.pathname + location.search + location.hash);
  screenWrap.focus({ preventScroll: true });
}

function closeSession() {
  sessionEnded = false;
  sessionEverConnected = false;
  disconnectedPolls = 0;
  pinchMode = false;
  resetPanelTouchState();
  closeWs();
  stopFrameStream();
  if (source) { source.close(); source = null; }
  clearFrameCanvas();
  overlay.textContent = "";
  gestureSvg.textContent = "";
  clearCapabilityPrompts();
  current = null;
  currentGuestGen = null;
  pendingRotate = null;
  detail.hidden = true;
  grid.hidden = false;
  document.body.classList.remove("watching");
  $("offline").textContent = "stream offline · reconnecting…";
  $("offline").hidden = true;
  location.hash = "";
  history.replaceState(null, "", location.pathname + location.search);
  refresh();
}

function markLive() {
  if (sessionEnded) return;
  sessionEverConnected = true;
  disconnectedPolls = 0;
  $("offline").textContent = "stream offline · reconnecting…";
  $("offline").hidden = true;
  $("dstate").textContent = "live";
  $("dstate").className = "pill ok";
}

function markSessionEnded(sid) {
  if (!current || (sid && current.session_id !== sid)) return;
  sessionEnded = true;
  pinchMode = false;
  resetPanelTouchState();
  closeWs();
  stopFrameStream();
  if (source) { source.close(); source = null; }
  applyToolbarUi();
  $("offline").textContent = "session ended";
  $("offline").hidden = false;
  $("dstate").textContent = "session ended";
  $("dstate").className = "pill bad";
}

function connectEvents(s) {
  source = new EventSource(auth("/api/events/" + encodeURIComponent(s.session_id)));
  source.addEventListener("replaced", () => {
    if (current) markSessionEnded(current.session_id);
  });
  source.addEventListener("state", (message) => {
    if (sessionEnded) return;
    let data = {};
    try { data = JSON.parse(message.data); } catch (error) { return; }
    if (typeof data.guest_gen === "number" && data.guest_gen > 0) {
      if (currentGuestGen !== null && data.guest_gen !== currentGuestGen) {
        if (current) markSessionEnded(current.session_id);
        return;
      }
      currentGuestGen = data.guest_gen;
    }
    const entry = data.entry || {};
    const hello = data.hello || {};
    if (
      !pendingRotate &&
      entry.display && entry.display.w && entry.display.h &&
      (!frameSize.w || !frameSize.h ||
        ((entry.display.w < entry.display.h) === (frameSize.w < frameSize.h)))
    ) {
      display = { w: entry.display.w, h: entry.display.h };
      setAspect(display.w, display.h);
    }
    if (current) {
      if (typeof entry.watch_only === "boolean") {
        current.watch_only = entry.watch_only;
      }
      if (typeof entry.has_home === "boolean") {
        current.has_home = entry.has_home;
      } else if (typeof hello.has_home === "boolean") {
        current.has_home = hello.has_home;
      }
      applyToolbarUi();
    }
    if (entry.activity) $("dactivity").textContent = entry.activity;
    if (data.connected === true) {
      sessionEverConnected = true;
      disconnectedPolls = 0;
    }
    $("offline").textContent = "stream offline · reconnecting…";
    $("offline").hidden = !!data.connected;
    $("dstate").textContent = data.connected ? "live" : "reconnecting";
    $("dstate").className = "pill" + (data.connected ? " ok" : " bad");
  });
  source.addEventListener("ev", (message) => {
    if (sessionEnded) return;
    let data = {};
    try { data = JSON.parse(message.data); } catch (error) { return; }
    markLive();
    onEvent(data.event || {}, data.kind || "event");
  });
  source.addEventListener("lag", (message) => {
    if (sessionEnded) return;
    resetPanelTouchState();
    let data = {};
    try { data = JSON.parse(message.data); } catch (error) { return; }
    if (data.missed) addEvent({ cmd: "…", status: "", text: data.missed + " earlier events dropped" }, "lag");
  });
  source.onopen = () => {
    if (sessionEnded) return;
    $("offline").textContent = "stream offline · reconnecting…";
    $("offline").hidden = true;
  };
  source.onerror = () => {
    if (sessionEnded) return;
    $("offline").textContent = "stream offline · reconnecting…";
    $("offline").hidden = false;
    $("dstate").textContent = "reconnecting";
    $("dstate").className = "pill bad";
  };
}

function displayToOverlayPx(pt) {
  const dw = display.w || frameSize.w || frame.width || 1080;
  const dh = display.h || frameSize.h || frame.height || 2400;
  const imgRect = renderedImageRect();
  const ovRect = overlay.getBoundingClientRect();
  if (!imgRect.width || !imgRect.height || !ovRect.width || !ovRect.height) return null;
  return {
    x: (imgRect.left - ovRect.left) + (pt.x / dw) * imgRect.width,
    y: (imgRect.top - ovRect.top) + (pt.y / dh) * imgRect.height,
  };
}

function mirrorDisplayPoint(pt) {
  const dw = display.w || frameSize.w || frame.width || 1080;
  const dh = display.h || frameSize.h || frame.height || 2400;
  const cx = (dw - 1) / 2;
  const cy = (dh - 1) / 2;
  return {
    x: Math.max(0, Math.min(dw - 1, Math.round(2 * cx - pt.x))),
    y: Math.max(0, Math.min(dh - 1, Math.round(2 * cy - pt.y))),
  };
}

function syntheticMirrorId(pid) {
  return 1000000 + (Math.abs(Number(pid) || 0) % 100000);
}

function svgEl(tag, attrs) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) {
    node.setAttribute(k, String(v));
  }
  return node;
}

function updatePointerVisuals(entry) {
  const p = displayToOverlayPx(entry);
  if (!p) return;
  if (entry.dotEl) {
    entry.dotEl.style.left = p.x + "px";
    entry.dotEl.style.top = p.y + "px";
  }
  if (entry.trailEl) {
    entry.overlayPts.push(p.x.toFixed(1) + "," + p.y.toFixed(1));
    if (entry.overlayPts.length > 256) entry.overlayPts.shift();
    entry.trailEl.setAttribute("points", entry.overlayPts.join(" "));
  }
  if (entry.mirrorPt) {
    const mp = displayToOverlayPx(entry.mirrorPt);
    if (mp) {
      if (entry.mirrorDotEl) {
        entry.mirrorDotEl.style.left = mp.x + "px";
        entry.mirrorDotEl.style.top = mp.y + "px";
      }
      if (entry.pinchLineEl) {
        entry.pinchLineEl.setAttribute("x1", p.x.toFixed(1));
        entry.pinchLineEl.setAttribute("y1", p.y.toFixed(1));
        entry.pinchLineEl.setAttribute("x2", mp.x.toFixed(1));
        entry.pinchLineEl.setAttribute("y2", mp.y.toFixed(1));
      }
      if (entry.pivotEl) {
        entry.pivotEl.style.left = ((p.x + mp.x) / 2) + "px";
        entry.pivotEl.style.top = ((p.y + mp.y) / 2) + "px";
      }
    }
  }
}

function clearCapabilityPrompts() {
  capabilityCards.clear();
  if (capTray) capTray.textContent = "";
}

function copyTextToClipboard(text, button, fallbackLabel) {
  const done = () => {
    const prev = button.textContent;
    button.textContent = "Copied";
    setTimeout(() => { button.textContent = fallbackLabel || prev; }, 1400);
  };
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(text).then(done).catch(() => {
      try {
        const ta = document.createElement("textarea");
        ta.value = text;
        ta.style.position = "fixed";
        ta.style.opacity = "0";
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        ta.remove();
        done();
      } catch (_) {}
    });
  } else {
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
      done();
    } catch (_) {}
  }
}

function dismissCapabilityPrompt(seq) {
  const rec = capabilityCards.get(seq);
  if (!rec) return;
  capabilityCards.delete(seq);
  rec.card.remove();
}

function showCapabilityPrompt(prompt) {
  if (!capTray || !prompt || typeof prompt.seq !== "number") return;
  if (capabilityCards.has(prompt.seq)) return;
  while (capabilityCards.size >= 3) {
    const oldest = capabilityCards.keys().next().value;
    if (oldest === undefined) break;
    dismissCapabilityPrompt(oldest);
  }
  const isInfo = prompt.trigger === "intent_captured";
  const card = el("div", "cap-card" + (isInfo ? " info" : ""));
  const head = el("div", "cap-head");
  const badgeLabel = isInfo ? "external intent" : "capability off";
  head.appendChild(el("span", "cap-badge", badgeLabel));
  head.appendChild(el("div", "cap-title", prompt.title || "Capability needed"));
  const closeBtn = el("button", "cap-x", "×");
  closeBtn.type = "button";
  closeBtn.title = "Dismiss";
  closeBtn.addEventListener("click", () => dismissCapabilityPrompt(prompt.seq));
  head.appendChild(closeBtn);
  card.appendChild(head);

  if (prompt.message) {
    card.appendChild(el("div", "cap-msg", prompt.message));
  }
  if (prompt.request) {
    card.appendChild(el("div", "cap-req", prompt.request));
  }

  const actions = el("div", "cap-actions");
  const statusEl = el("div", "cap-status");
  statusEl.hidden = true;
  const buttons = [];
  const opts = Array.isArray(prompt.options) ? prompt.options : [];
  let primaryUsed = false;

  for (const opt of opts) {
    if (!opt || typeof opt !== "object") continue;
    if (opt.kind === "hot_attach") {
      const btn = el("button", "cap-btn" + (!primaryUsed ? " primary" : ""), opt.label || "Enable");
      primaryUsed = true;
      btn.type = "button";
      if (current && current.watch_only) {
        btn.disabled = true;
        btn.title = "watch-only: verify run";
      }
      btn.addEventListener("click", () => {
        if (current && current.watch_only) return;
        for (const b of buttons) b.disabled = true;
        statusEl.hidden = false;
        statusEl.className = "cap-status";
        statusEl.textContent = "Enabling…";
        const ok = sendInput({
          t: "capability",
          prompt_seq: prompt.seq,
          option_id: opt.id || null,
          modules: Array.isArray(opt.modules) ? opt.modules : [],
          control: opt.control || null,
          replay_intent: typeof opt.replay_intent === "number" ? opt.replay_intent : null,
        });
        if (!ok) {
          for (const b of buttons) b.disabled = !!(current && current.watch_only);
          statusEl.className = "cap-status bad";
          statusEl.textContent = "Could not send request (session not interactive).";
        }
      });
      buttons.push(btn);
      actions.appendChild(btn);
    } else if (opt.kind === "relaunch_required") {
      const specs = Array.isArray(opt.modules)
        ? opt.modules.map((m) => (m.id || "") + ":" + (m.profile || "real")).filter(Boolean).join(",")
        : "";
      const flag = specs ? "--services " + specs : "";
      const btn = el("button", "cap-btn", opt.label || "Copy launch flag");
      btn.type = "button";
      btn.title = flag || "Requires relaunching with this module enabled";
      btn.addEventListener("click", () => {
        if (flag) copyTextToClipboard(flag, btn, opt.label || "Copy launch flag");
      });
      actions.appendChild(btn);
    } else if (opt.kind === "info_only" && opt.url) {
      const btn = el("button", "cap-btn primary", opt.label || "Copy URL");
      btn.type = "button";
      btn.title = opt.url;
      btn.addEventListener("click", () => {
        copyTextToClipboard(opt.url, btn, opt.label || "Copy URL");
      });
      actions.appendChild(btn);
    }
  }

  const dismissBtn = el("button", "cap-btn", "Dismiss");
  dismissBtn.type = "button";
  dismissBtn.addEventListener("click", () => dismissCapabilityPrompt(prompt.seq));
  actions.appendChild(dismissBtn);

  card.appendChild(actions);
  card.appendChild(statusEl);
  capTray.appendChild(card);
  capabilityCards.set(prompt.seq, { card, buttons, statusEl, prompt });
}

function handleCapabilityReply(msg) {
  const seq = typeof msg.prompt_seq === "number" ? msg.prompt_seq : null;
  const rec = seq !== null ? capabilityCards.get(seq) : null;
  if (msg.ok) {
    addEvent({
      source: "panel",
      cmd: "capability",
      status: "ok",
      text: rec && rec.prompt ? rec.prompt.title : (msg.option_id || "enabled"),
      ts_ms: Date.now(),
    }, "capability");
    if (rec) {
      rec.statusEl.hidden = false;
      rec.statusEl.className = "cap-status ok";
      rec.statusEl.textContent = "Enabled.";
      setTimeout(() => dismissCapabilityPrompt(seq), 750);
    }
  } else {
    addEvent({
      source: "panel",
      cmd: "capability",
      status: msg.code || "error",
      note: msg.error || "capability action failed",
      ts_ms: Date.now(),
    }, "capability");
    if (rec) {
      for (const b of rec.buttons) b.disabled = !!(current && current.watch_only);
      rec.statusEl.hidden = false;
      rec.statusEl.className = "cap-status bad";
      rec.statusEl.textContent = msg.error || msg.code || "Failed to enable capability.";
    }
  }
}

function onEvent(event, kind) {
  if (event.kind === "capability_prompt" && event.prompt) {
    showCapabilityPrompt(event.prompt);
    addEvent({
      source: "acos",
      cmd: "capability",
      status: event.prompt.trigger || "prompt",
      text: event.prompt.title || "",
      note: event.prompt.request || "",
      ts_ms: event.prompt.utc_ms || Date.now(),
    }, "capability_prompt");
    return;
  }
  if (kind === "activity") {
    $("dactivity").textContent = event.activity || "-";
    addEvent(event, kind);
    return;
  }
  if (event.kind === "activity" && event.activity) {
    $("dactivity").textContent = event.activity;
    addEvent(event, "activity");
    return;
  }
  if (event.kind === "action" && event.cmd === "rotate" && event.status === "ok") {
    cancelActivePointers();
    if (
      typeof event.frame_seq === "number" &&
      event.frame_seq > lastFrameSeq &&
      (!pendingRotate || (!pendingRotate.awaitingReply && event.frame_seq > pendingRotate.minSeq))
    ) {
      pendingRotate = {
        minSeq: event.frame_seq,
        expectedW: 0,
        expectedH: 0,
        awaitingReply: false,
      };
      applyToolbarUi();
    }
  }
  if (event.kind === "action" && event.source === "panel" && event.cmd === "touch") {
    const ok = !event.status || event.status === "ok";
    if (!ok) {
      if (typeof event.x === "number" && typeof event.y === "number") ripple(event);
      addEvent(event, kind);
      if (event.phase === "up" || event.phase === "cancel") resetPanelTouchState();
      return;
    }
    if (event.phase === "down") {
      const evTs = event.ts_ms || Date.now();
      if (panelTouchState.active > 0 && panelTouchState.startTs > 0 && (evTs - panelTouchState.startTs) > 30000) {
        panelTouchState.active = 0;
        panelTouchState.maxPointers = 0;
        panelTouchState.startTs = 0;
        panelTouchState.gesture = null;
      }
      panelTouchState.active += 1;
      if (panelTouchState.active === 1) {
        panelTouchState.maxPointers = 1;
        panelTouchState.startX = typeof event.x === "number" ? event.x : 0;
        panelTouchState.startY = typeof event.y === "number" ? event.y : 0;
        panelTouchState.startTs = evTs;
        panelTouchState.gesture = typeof event.gesture === "string" ? event.gesture : null;
      } else {
        panelTouchState.maxPointers = Math.max(panelTouchState.maxPointers, panelTouchState.active);
        if (typeof event.gesture === "string" && !panelTouchState.gesture) {
          panelTouchState.gesture = event.gesture;
        }
      }
      return;
    }
    if (event.phase === "up") {
      panelTouchState.active = Math.max(0, panelTouchState.active - 1);
      if (panelTouchState.active > 0) return;
      const ex = typeof event.x === "number" ? event.x : panelTouchState.startX;
      const ey = typeof event.y === "number" ? event.y : panelTouchState.startY;
      const hasStart = panelTouchState.startTs > 0;
      const sx = hasStart ? panelTouchState.startX : ex;
      const sy = hasStart ? panelTouchState.startY : ey;
      const dt = hasStart ? Math.max(0, (event.ts_ms || Date.now()) - panelTouchState.startTs) : 0;
      const dist = Math.hypot(ex - sx, ey - sy);
      const producerGesture = (typeof event.gesture === "string" ? event.gesture : null)
        || panelTouchState.gesture;
      let gestureCmd = "tap";
      if (panelTouchState.maxPointers >= 2 || producerGesture === "pinch") {
        gestureCmd = "pinch";
      } else if (producerGesture === "scroll") {
        gestureCmd = "scroll";
      } else if (dist >= 24) {
        gestureCmd = "drag";
      } else if (dt >= 450) {
        gestureCmd = "long-press";
      }
      panelTouchState.startTs = 0;
      panelTouchState.maxPointers = 0;
      panelTouchState.gesture = null;
      const summary = Object.assign({}, event, { cmd: gestureCmd });
      if (gestureCmd === "drag" || gestureCmd === "scroll" || gestureCmd === "pinch") {
        summary.x = sx;
        summary.y = sy;
        summary.x2 = ex;
        summary.y2 = ey;
      } else {
        summary.x = ex;
        summary.y = ey;
      }
      if (typeof summary.x === "number" && typeof summary.y === "number") {
        ripple(summary);
      }
      addEvent(summary, kind);
      return;
    }
    if (event.phase === "cancel") {
      resetPanelTouchState();
      addEvent(Object.assign({}, event, { cmd: "touch cancel" }), kind);
      return;
    }
  }
  if (event.kind === "action" && typeof event.x === "number" && typeof event.y === "number") {
    ripple(event);
  }
  addEvent(event, kind);
}

function ripple(event) {
  const p1 = displayToOverlayPx({ x: event.x, y: event.y });
  if (!p1) return;
  const ok = !event.status || event.status === "ok";
  const srcCls = event.source === "panel" ? " panel" : "";
  const hasEnd = typeof event.x2 === "number" && typeof event.y2 === "number";
  const p2 = hasEnd ? displayToOverlayPx({ x: event.x2, y: event.y2 }) : null;
  const isPath = p2 && Math.hypot(p2.x - p1.x, p2.y - p1.y) >= 12;

  if (isPath) {
    const color = !ok ? "#f85149" : (event.source === "panel" ? "#3fb950" : "#4f9cf9");
    const line = svgEl("line", {
      x1: p1.x.toFixed(1),
      y1: p1.y.toFixed(1),
      x2: p2.x.toFixed(1),
      y2: p2.y.toFixed(1),
      stroke: color,
      "stroke-width": "3",
      "stroke-linecap": "round",
      "stroke-opacity": "0.85",
    });
    gestureSvg.appendChild(line);
    requestAnimationFrame(() => line.classList.add("trail-fade"));
    setTimeout(() => line.remove(), 900);
  }

  const endPx = isPath ? p2 : p1;
  const dot = el("div", "ripple" + srcCls + (ok ? "" : " bad"));
  dot.style.left = endPx.x + "px";
  dot.style.top = endPx.y + "px";
  overlay.appendChild(dot);
  const srcPrefix = event.source ? event.source + " " : "";
  const coordText = isPath
    ? (event.x + "," + event.y + " → " + event.x2 + "," + event.y2)
    : (event.x + "," + event.y);
  const tag = el("div", "rip-tag", srcPrefix + (event.cmd || "tap") + " " + coordText
    + (ok ? "" : " · " + event.status));
  tag.style.left = endPx.x + "px";
  tag.style.top = endPx.y + "px";
  overlay.appendChild(tag);
  setTimeout(() => { dot.remove(); tag.remove(); }, 1200);
}

function addEvent(event, kind) {
  const item = el("li");
  const stamp = new Date(event.ts_ms || Date.now());
  item.appendChild(el("span", "t", stamp.toLocaleTimeString([], { hour12: false })));
  const what = el("div", "what");
  if (event.source) {
    const srcCls = event.source === "panel" ? "src panel" : (event.source === "agent" ? "src agent" : "src");
    what.appendChild(el("span", srcCls, event.source));
  }
  const label = event.cmd || event.kind || kind;
  what.appendChild(el("span", "cmd", label));
  if (event.status) {
    const status = el("span", "st " + (event.status === "ok" ? "ok" : "bad"), " " + event.status);
    what.appendChild(status);
  }
  if (typeof event.x === "number" && typeof event.y === "number") {
    what.appendChild(el("span", "txt", " @ " + event.x + "," + event.y
      + (typeof event.x2 === "number" ? " → " + event.x2 + "," + event.y2 : "")));
  }
  if (event.key !== undefined && event.key !== null) {
    const kname = KEY_NAMES[event.key];
    what.appendChild(el("span", "txt", " key " + (kname ? kname + " (" + event.key + ")" : event.key)));
  }
  if (event.text) what.appendChild(el("span", "txt", " “" + event.text + "”"));
  if (event.note) what.appendChild(el("span", "txt", " · " + event.note));
  if (event.activity && event.kind === "activity") what.appendChild(el("span", "txt", " " + event.activity));
  item.appendChild(what);
  eventsList.prepend(item);
  while (eventsList.children.length > 40) eventsList.lastChild.remove();
}

screenWrap.addEventListener("pointerdown", (e) => {
  if (!current || sessionEnded) return;
  if (e.pointerType === "mouse" && e.button !== 0) return;
  const pt = clientToDisplay(e.clientX, e.clientY, false);
  if (!pt) return;
  e.preventDefault();
  screenWrap.focus({ preventScroll: true });
  if (current.watch_only) {
    applyToolbarUi();
    return;
  }
  if (!sendInput({ t: "touch", phase: "down", id: e.pointerId, x: pt.x, y: pt.y })) {
    return;
  }
  try { screenWrap.setPointerCapture(e.pointerId); } catch (_) {}

  const usePinch = (e.altKey || pinchMode) && activePointers.size === 0 && e.pointerType !== "touch";
  let mirrorId = null;
  let mirrorPt = null;
  if (usePinch) {
    const candId = syntheticMirrorId(e.pointerId);
    const candPt = mirrorDisplayPoint(pt);
    if (sendInput({ t: "touch", phase: "down", id: candId, x: candPt.x, y: candPt.y })) {
      mirrorId = candId;
      mirrorPt = candPt;
    }
  }

  const dotEl = el("div", "touch-dot");
  overlay.appendChild(dotEl);
  let trailEl = null;
  let mirrorDotEl = null;
  let pinchLineEl = null;
  let pivotEl = null;
  if (mirrorId !== null) {
    mirrorDotEl = el("div", "touch-dot mirror");
    overlay.appendChild(mirrorDotEl);
    pivotEl = el("div", "pinch-pivot");
    overlay.appendChild(pivotEl);
    pinchLineEl = svgEl("line", {
      stroke: "#4f9cf9",
      "stroke-width": "2",
      "stroke-dasharray": "5 4",
      "stroke-opacity": "0.85",
    });
    gestureSvg.appendChild(pinchLineEl);
  } else {
    trailEl = svgEl("polyline", {
      fill: "none",
      stroke: "#3fb950",
      "stroke-width": "3",
      "stroke-linecap": "round",
      "stroke-linejoin": "round",
      "stroke-opacity": "0.8",
    });
    gestureSvg.appendChild(trailEl);
  }

  const entry = {
    x: pt.x,
    y: pt.y,
    startX: pt.x,
    startY: pt.y,
    mirrorId,
    mirrorPt,
    overlayPts: [],
    dotEl,
    mirrorDotEl,
    pivotEl,
    pinchLineEl,
    trailEl,
  };
  activePointers.set(e.pointerId, entry);
  updatePointerVisuals(entry);
});

screenWrap.addEventListener("pointermove", (e) => {
  const entry = activePointers.get(e.pointerId);
  if (!entry) return;
  const pt = clientToDisplay(e.clientX, e.clientY, true);
  if (!pt) return;
  if (entry.x === pt.x && entry.y === pt.y) return;
  entry.x = pt.x;
  entry.y = pt.y;
  sendInput({ t: "touch", phase: "move", id: e.pointerId, x: pt.x, y: pt.y });
  if (entry.mirrorId !== null) {
    const mpt = mirrorDisplayPoint(pt);
    if (!entry.mirrorPt || entry.mirrorPt.x !== mpt.x || entry.mirrorPt.y !== mpt.y) {
      entry.mirrorPt = mpt;
      sendInput({ t: "touch", phase: "move", id: entry.mirrorId, x: mpt.x, y: mpt.y });
    }
  }
  updatePointerVisuals(entry);
});

function finishPointer(e, phase) {
  const entry = activePointers.get(e.pointerId);
  if (!entry) return;
  const pt = clientToDisplay(e.clientX, e.clientY, true) || { x: entry.x, y: entry.y };
  if (phase === "cancel") {
    for (const [pid, item] of activePointers.entries()) {
      clearPointerVisuals(item, false);
      try {
        if (screenWrap.hasPointerCapture && screenWrap.hasPointerCapture(pid)) {
          screenWrap.releasePointerCapture(pid);
        }
      } catch (_) {}
    }
    activePointers.clear();
    sendInput({ t: "touch", phase: "cancel", id: e.pointerId, x: pt.x, y: pt.y });
    return;
  }
  const dragged = Math.hypot(pt.x - entry.startX, pt.y - entry.startY) >= 24;
  activePointers.delete(e.pointerId);
  try {
    if (screenWrap.hasPointerCapture && screenWrap.hasPointerCapture(e.pointerId)) {
      screenWrap.releasePointerCapture(e.pointerId);
    }
  } catch (_) {}
  clearPointerVisuals(entry, dragged);
  if (entry.mirrorId !== null) {
    const mpt = mirrorDisplayPoint(pt);
    sendInput({ t: "touch", phase: "up", id: entry.mirrorId, x: mpt.x, y: mpt.y });
  }
  sendInput({ t: "touch", phase: "up", id: e.pointerId, x: pt.x, y: pt.y });
}

screenWrap.addEventListener("pointerup", (e) => finishPointer(e, "up"));
screenWrap.addEventListener("pointercancel", (e) => finishPointer(e, "cancel"));

screenWrap.addEventListener("wheel", (e) => {
  if (!current || sessionEnded) return;
  if (!hasDecodedFrame() || isRotationPending() || activePointers.size > 0) return;
  const pt = clientToDisplay(e.clientX, e.clientY, false);
  if (!pt) return;
  e.preventDefault();
  if (current.watch_only) {
    applyToolbarUi();
    return;
  }
  const scale = e.deltaMode === 1 ? 40 : (e.deltaMode === 2 ? 400 : 1);
  if (e.ctrlKey) {
    const delta = Math.round(e.deltaY * scale);
    if (!delta) return;
    sendInput({ t: "pinch", x: pt.x, y: pt.y, delta });
    return;
  }
  sendInput({
    t: "scroll",
    x: pt.x,
    y: pt.y,
    dx: Math.round(e.deltaX * scale),
    dy: Math.round(e.deltaY * scale),
  });
}, { passive: false });

document.addEventListener("keydown", (e) => {
  if (!current || detail.hidden || current.watch_only || sessionEnded) return;
  if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey) {
    if (e.key && e.key.toLowerCase() === "a" && document.activeElement === screenWrap) {
      e.preventDefault();
      // Android META_CTRL_ON (0x1000) | META_CTRL_LEFT_ON (0x2000)
      sendInput({ t: "key", code: "KEYCODE_A", phase: "press", meta: 0x3000 });
    }
    return;
  }
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  if (document.activeElement && document.activeElement.tagName === "BUTTON" && (e.key === "Enter" || e.key === " ")) {
    return;
  }
  const kc = KEY_MAP[e.key];
  if (kc) {
    e.preventDefault();
    if (!e.repeat) {
      if (sendInput({ t: "key", code: kc, phase: "down" })) {
        heldGuestKeys.add(kc);
      }
    }
    return;
  }
  if (e.key && e.key.length === 1) {
    e.preventDefault();
    sendInput({ t: "text", s: e.key });
  }
});

document.addEventListener("keyup", (e) => {
  if (!current || detail.hidden) return;
  const kc = KEY_MAP[e.key];
  if (kc && heldGuestKeys.delete(kc)) {
    e.preventDefault();
    sendInput({ t: "key", code: kc, phase: "up" });
  }
});

document.addEventListener("paste", (e) => {
  if (!current || detail.hidden || current.watch_only || sessionEnded) return;
  const raw = (e.clipboardData && e.clipboardData.getData("text/plain")) || "";
  if (!raw) return;
  e.preventDefault();
  const normalized = raw.replace(/\r\n?/g, "\n");
  const asciiOnly = normalized.replace(/[^\x20-\x7e\n\t]/g, "");
  const dropped = normalized.length - asciiOnly.length;
  const MAX_PASTE_CHARS = 2048;
  const PASTE_CHUNK = 64;
  const truncated = asciiOnly.length > MAX_PASTE_CHARS;
  const toSend = asciiOnly.slice(0, MAX_PASTE_CHARS);
  if (!toSend) {
    addEvent({ source: "panel", cmd: "paste", status: "bad", note: "ASCII text only" }, "action");
    return;
  }
  for (let i = 0; i < toSend.length; i += PASTE_CHUNK) {
    sendInput({ t: "text", s: toSend.slice(i, i + PASTE_CHUNK) });
  }
  if (dropped > 0 || truncated) {
    const notes = [];
    if (dropped > 0) notes.push(dropped + " non-ASCII skipped");
    if (truncated) notes.push("capped at " + MAX_PASTE_CHARS + " chars");
    addEvent({ source: "panel", cmd: "paste", status: "ok", note: notes.join(", ") }, "action");
  }
});

function sendKeyPress(code) {
  sendInput({ t: "key", code, phase: "down" });
  sendInput({ t: "key", code, phase: "up" });
  screenWrap.focus({ preventScroll: true });
}

btnBack.addEventListener("click", () => sendKeyPress("KEYCODE_BACK"));
btnHome.addEventListener("click", () => {
  if (current && current.has_home === false) return;
  sendKeyPress("KEYCODE_HOME");
});
btnRecents.addEventListener("click", () => sendKeyPress("KEYCODE_APP_SWITCH"));
btnVolDown.addEventListener("click", () => sendKeyPress("KEYCODE_VOLUME_DOWN"));
btnVolUp.addEventListener("click", () => sendKeyPress("KEYCODE_VOLUME_UP"));
btnPower.addEventListener("click", () => sendKeyPress("KEYCODE_POWER"));
btnPinch.addEventListener("click", () => {
  if (!current || current.watch_only || sessionEnded) return;
  pinchMode = !pinchMode;
  applyToolbarUi();
  screenWrap.focus({ preventScroll: true });
});
btnShot.addEventListener("click", () => {
  if (!hasDecodedFrame()) return;
  const sid = current ? String(current.session_id).replace(/[^a-zA-Z0-9._-]+/g, "_") : "screen";
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const filename = "acos-" + sid + "-" + stamp + ".png";
  frame.toBlob((blob) => {
    if (!blob) return;
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }, "image/png");
  screenWrap.focus({ preventScroll: true });
});
btnRotate.addEventListener("click", () => {
  if (!current || current.watch_only || sessionEnded || isRotationPending()) return;
  cancelActivePointers();
  const next = (currentRotation + 1) % 4;
  const preSeq = lastFrameSeq;
  if (sendInput({ t: "rotate", rotation: next })) {
    pendingRotate = {
      minSeq: preSeq + 1,
      expectedW: 0,
      expectedH: 0,
      awaitingReply: true,
    };
    applyToolbarUi();
  }
  screenWrap.focus({ preventScroll: true });
});

$("back").addEventListener("click", closeSession);
window.addEventListener("blur", () => {
  releaseHeldGuestKeys();
  cancelActivePointers();
});
document.addEventListener("visibilitychange", () => {
  if (document.hidden) {
    releaseHeldGuestKeys();
    cancelActivePointers();
  }
});
window.addEventListener("pagehide", () => {
  closeWs();
  stopFrameStream();
  if (source) { source.close(); source = null; }
});
window.addEventListener("pageshow", (e) => {
  if (e.persisted && current && !sessionEnded && current.stream) {
    connectEvents(current);
    startFrameStream(current.session_id);
  }
});
refresh();
if (location.hash.startsWith("#s=")) {
  const wanted = decodeURIComponent(location.hash.slice(3));
  const timer = setInterval(() => {
    const found = sessions.find((s) => s.session_id === wanted);
    if (found) { clearInterval(timer); openSession(found); }
  }, 250);
  setTimeout(() => clearInterval(timer), 5000);
}
</script>
</body>
</html>
"##;
