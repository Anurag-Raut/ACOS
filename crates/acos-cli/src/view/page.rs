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
  .bar button:disabled { opacity: .45; cursor: not-allowed; }
  .bar .actions { display: flex; gap: 6px; align-items: center; }
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
  #offline { position: absolute; inset: auto 0 0 0; background: rgba(13,17,23,.82);
    color: var(--warn); font-size: 12px; padding: 6px 10px; z-index: 3; }
  #overlay { position: absolute; inset: 0; pointer-events: none; z-index: 2; overflow: hidden; }
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
    </div>
    <span class="spacer"></span>
    <span class="pill" id="dinput">idle</span>
    <span class="pill" id="dstate">stream</span>
  </div>
  <div class="stage">
    <div class="screenwrap" id="screenwrap" tabindex="0">
      <canvas id="frame" width="0" height="0" aria-label="live screen"></canvas>
      <div id="overlay"></div>
      <div id="offline" hidden>stream offline · reconnecting…</div>
    </div>
    <aside>
      <h2>activity</h2>
      <div class="mono" id="dactivity" style="font-size:12px;color:var(--muted);margin-bottom:10px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">-</div>
      <h2>agent &amp; human activity</h2>
      <ol id="events" reversed></ol>
      <div class="note">Click or type on the screen to interact (source: panel).
        Watch-only until first input; verify sessions stay read-only.</div>
    </aside>
  </div>
</section>
<script>
"use strict";
const token = new URLSearchParams(location.search).get("t") || "";
const auth = (path) => path + (path.includes("?") ? "&" : "?") + "t=" + encodeURIComponent(token);
const $ = (id) => document.getElementById(id);
const grid = $("grid"), detail = $("detail"), statusPill = $("status");
const frame = $("frame"), frameCtx = frame.getContext("2d"), screenWrap = $("screenwrap"), overlay = $("overlay"), eventsList = $("events");
const btnBack = $("btn-back"), btnHome = $("btn-home"), btnRecents = $("btn-recents"), btnRotate = $("btn-rotate");
const verifyBanner = $("verify-banner"), dinput = $("dinput");

let sessions = [];
let current = null;
let source = null;       // EventSource
let display = { w: 0, h: 0 };
let frameSize = { w: 0, h: 0 };
let currentRotation = 0;
let pollTimer = null;
let streamAbort = null;
let frameGeneration = 0;

function afterLoad(fn) {
  if (document.readyState === "complete") fn();
  else window.addEventListener("load", fn, { once: true });
}

let ws = null;
let wsQueue = [];
const activePointers = new Map();

const KEY_MAP = {
  Backspace: "KEYCODE_DEL",
  Delete: "KEYCODE_FORWARD_DEL",
  Enter: "KEYCODE_ENTER",
  Escape: "KEYCODE_BACK",
  Tab: "KEYCODE_TAB",
  ArrowUp: "KEYCODE_DPAD_UP",
  ArrowDown: "KEYCODE_DPAD_DOWN",
  ArrowLeft: "KEYCODE_DPAD_LEFT",
  ArrowRight: "KEYCODE_DPAD_RIGHT",
  Home: "KEYCODE_MOVE_HOME",
  End: "KEYCODE_MOVE_END",
  PageUp: "KEYCODE_PAGE_UP",
  PageDown: "KEYCODE_PAGE_DOWN",
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
  verifyBanner.hidden = !isWatchOnly;
  screenWrap.classList.toggle("watchonly", isWatchOnly);

  btnBack.disabled = isWatchOnly;
  btnBack.title = isWatchOnly ? "watch-only: verify run" : "Back (Escape)";

  btnHome.disabled = isWatchOnly || noLauncher;
  btnHome.title = isWatchOnly
    ? "watch-only: verify run"
    : (noLauncher ? "no launcher in this session" : "Home");

  btnRecents.disabled = isWatchOnly;
  btnRecents.title = isWatchOnly ? "watch-only: verify run" : "Recents";

  btnRotate.disabled = isWatchOnly;
  btnRotate.title = isWatchOnly ? "watch-only: verify run" : "Rotate 90°";

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
    sessions = data.sessions || [];
    statusPill.textContent = sessions.length + (sessions.length === 1 ? " live session" : " live sessions");
    statusPill.className = "pill ok";
    if (!current) renderGrid();
    if (current) {
      const updated = sessions.find((s) => s.session_id === current.session_id);
      if (!updated) {
        $("dstate").textContent = "session ended";
        $("dstate").className = "pill bad";
      } else {
        const prevHasHome = current.has_home;
        current = updated;
        if (typeof current.has_home !== "boolean" && typeof prevHasHome === "boolean") {
          current.has_home = prevHasHome;
        }
        $("dtitle").textContent = sessionTitle(updated);
        if (updated.display && updated.display.w && updated.display.h) {
          display = { w: updated.display.w, h: updated.display.h };
          setAspect(display.w, display.h);
        }
        if (updated.activity) $("dactivity").textContent = updated.activity;
        applyToolbarUi();
      }
    }
  } catch (error) {
    statusPill.textContent = "hub unreachable · " + error.message;
    statusPill.className = "pill bad";
  }
  clearTimeout(pollTimer);
  pollTimer = setTimeout(refresh, 2000);
}

function closeWs() {
  activePointers.clear();
  wsQueue = [];
  if (ws) {
    try { ws.close(); } catch (_) {}
    ws = null;
  }
}

function ensureWs() {
  if (!current || current.watch_only) return null;
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
    return ws;
  }
  const proto = location.protocol === "https:" ? "wss://" : "ws://";
  const url = proto + location.host + auth("/api/input/" + encodeURIComponent(current.session_id));
  const sock = new WebSocket(url);
  ws = sock;
  dinput.textContent = "connecting…";
  dinput.className = "pill";
  sock.onopen = () => {
    if (ws !== sock) return;
    dinput.textContent = "interactive";
    dinput.className = "pill ok";
    while (wsQueue.length > 0 && sock.readyState === WebSocket.OPEN) {
      sock.send(wsQueue.shift());
    }
  };
  sock.onmessage = (ev) => {
    if (ws !== sock) return;
    let msg = null;
    try { msg = JSON.parse(ev.data); } catch (_) { return; }
    if (!msg) return;
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
    if (msg.ok && msg.t === "rotate" && msg.width > 0 && msg.height > 0) {
      display = { w: msg.width, h: msg.height };
      if (typeof msg.rotation === "number") currentRotation = msg.rotation;
      setAspect(display.w, display.h);
    }
  };
  sock.onclose = () => {
    if (ws !== sock) return;
    ws = null;
    wsQueue = [];
    activePointers.clear();
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
    if (wsQueue.length < 128) wsQueue.push(raw);
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
  if (frameCtx && frame.width && frame.height) {
    frameCtx.clearRect(0, 0, frame.width, frame.height);
  }
  frame.width = 0;
  frame.height = 0;
}

async function drawDecodedFrame(part, sid, gen, ctrl) {
  const blob = new Blob([part.payload], { type: part.contentType || "image/png" });
  const bmp = await createImageBitmap(blob);
  if (ctrl.signal.aborted || streamAbort !== ctrl || frameGeneration !== gen || !current || current.session_id !== sid) {
    bmp.close();
    return;
  }
  if (frame.width !== bmp.width || frame.height !== bmp.height) {
    frame.width = bmp.width;
    frame.height = bmp.height;
  }
  frameSize = { w: bmp.width, h: bmp.height };
  if (frameCtx) {
    frameCtx.drawImage(bmp, 0, 0);
  }
  bmp.close();
  markLive();
  setAspect(frameSize.w, frameSize.h);
  if (display.w && display.h && ((display.w < display.h) !== (frameSize.w < frameSize.h))) {
    display = { w: display.h, h: display.w };
  }
}

async function startFrameStream(sid) {
  stopFrameStream();
  const gen = ++frameGeneration;
  const ctrl = new AbortController();
  streamAbort = ctrl;
  const decoder = new TextDecoder();

  while (!ctrl.signal.aborted && streamAbort === ctrl && current && current.session_id === sid) {
    let reader = null;
    try {
      const response = await fetch(auth("/api/frames/" + encodeURIComponent(sid)), {
        cache: "no-store",
        signal: ctrl.signal,
      });
      if (!response.ok || !response.body) {
        throw new Error("HTTP " + response.status);
      }
      reader = response.body.getReader();
      let buf = new Uint8Array(0);
      while (!ctrl.signal.aborted && streamAbort === ctrl) {
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
          for (const line of hdrText.split(/\r?\n/)) {
            const colon = line.indexOf(":");
            if (colon <= 0) continue;
            const k = line.slice(0, colon).trim().toLowerCase();
            const v = line.slice(colon + 1).trim();
            if (k === "content-type") contentType = v;
            else if (k === "content-length") contentLength = Number.parseInt(v, 10);
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
          latest = { contentType, payload };
        }
        if (latest) {
          await drawDecodedFrame(latest, sid, gen, ctrl);
        }
      }
    } catch (error) {
      if (ctrl.signal.aborted || streamAbort !== ctrl) return;
    } finally {
      if (reader) {
        try { await reader.cancel(); } catch (_) {}
      }
    }
    if (ctrl.signal.aborted || streamAbort !== ctrl || !current || current.session_id !== sid) {
      return;
    }
    $("offline").hidden = false;
    $("dstate").textContent = "reconnecting";
    $("dstate").className = "pill bad";
    await new Promise((resolve) => setTimeout(resolve, 400));
  }
}

function openSession(s) {
  closeWs();
  stopFrameStream();
  clearFrameCanvas();
  current = s;
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
  $("offline").hidden = true;
  applyToolbarUi();
  if (s.stream) {
    setAspect(display.w, display.h);
    connectEvents(s);
    const sid = s.session_id;
    afterLoad(() => {
      if (current && current.session_id === sid) {
        startFrameStream(sid);
      }
    });
  }
  location.hash = "s=" + encodeURIComponent(s.session_id);
  history.replaceState(null, "", location.pathname + location.search + location.hash);
  screenWrap.focus({ preventScroll: true });
}

function closeSession() {
  closeWs();
  stopFrameStream();
  if (source) { source.close(); source = null; }
  clearFrameCanvas();
  overlay.textContent = "";
  current = null;
  detail.hidden = true;
  grid.hidden = false;
  document.body.classList.remove("watching");
  location.hash = "";
  history.replaceState(null, "", location.pathname + location.search);
  refresh();
}

function markLive() {
  $("offline").hidden = true;
  $("dstate").textContent = "live";
  $("dstate").className = "pill ok";
}

function connectEvents(s) {
  source = new EventSource(auth("/api/events/" + encodeURIComponent(s.session_id)));
  source.addEventListener("state", (message) => {
    let data = {};
    try { data = JSON.parse(message.data); } catch (error) { return; }
    const entry = data.entry || {};
    const hello = data.hello || {};
    if (entry.display && entry.display.w) {
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
    $("offline").hidden = !!data.connected;
    $("dstate").textContent = data.connected ? "live" : "reconnecting";
    $("dstate").className = "pill" + (data.connected ? " ok" : " bad");
  });
  source.addEventListener("ev", (message) => {
    let data = {};
    try { data = JSON.parse(message.data); } catch (error) { return; }
    markLive();
    onEvent(data.event || {}, data.kind || "event");
  });
  source.addEventListener("lag", (message) => {
    let data = {};
    try { data = JSON.parse(message.data); } catch (error) { return; }
    if (data.missed) addEvent({ cmd: "…", status: "", text: data.missed + " earlier events dropped" }, "lag");
  });
  source.onopen = () => { $("offline").hidden = true; };
  source.onerror = () => {
    $("offline").hidden = false;
    $("dstate").textContent = "reconnecting";
    $("dstate").className = "pill bad";
  };
}

function onEvent(event, kind) {
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
  if (event.kind === "action" && typeof event.x === "number" && typeof event.y === "number") {
    ripple(event);
  }
  addEvent(event, kind);
}

function ripple(event) {
  const dw = display.w || frameSize.w || frame.width || 1080;
  const dh = display.h || frameSize.h || frame.height || 2400;
  const imgRect = renderedImageRect();
  const ovRect = overlay.getBoundingClientRect();
  if (!imgRect.width || !imgRect.height || !ovRect.width || !ovRect.height) return;
  const leftPx = (imgRect.left - ovRect.left) + (event.x / dw) * imgRect.width;
  const topPx = (imgRect.top - ovRect.top) + (event.y / dh) * imgRect.height;
  const ok = !event.status || event.status === "ok";
  const srcCls = event.source === "panel" ? " panel" : "";
  const dot = el("div", "ripple" + srcCls + (ok ? "" : " bad"));
  dot.style.left = leftPx + "px";
  dot.style.top = topPx + "px";
  overlay.appendChild(dot);
  const srcPrefix = event.source ? event.source + " " : "";
  const tag = el("div", "rip-tag", srcPrefix + (event.cmd || "tap") + " " + event.x + "," + event.y
    + (ok ? "" : " · " + event.status));
  tag.style.left = leftPx + "px";
  tag.style.top = topPx + "px";
  overlay.appendChild(tag);
  setTimeout(() => { dot.remove(); tag.remove(); }, 1400);
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
  if (event.key !== undefined && event.key !== null) what.appendChild(el("span", "txt", " key " + event.key));
  if (event.text) what.appendChild(el("span", "txt", " “" + event.text + "”"));
  if (event.note) what.appendChild(el("span", "txt", " · " + event.note));
  if (event.activity && event.kind === "activity") what.appendChild(el("span", "txt", " " + event.activity));
  item.appendChild(what);
  eventsList.prepend(item);
  while (eventsList.children.length > 40) eventsList.lastChild.remove();
}

screenWrap.addEventListener("pointerdown", (e) => {
  if (!current) return;
  if (e.pointerType === "mouse" && e.button !== 0) return;
  const pt = clientToDisplay(e.clientX, e.clientY, false);
  if (!pt) return;
  e.preventDefault();
  screenWrap.focus({ preventScroll: true });
  if (current.watch_only) {
    applyToolbarUi();
    return;
  }
  try { screenWrap.setPointerCapture(e.pointerId); } catch (_) {}
  activePointers.set(e.pointerId, pt);
  sendInput({ t: "touch", phase: "down", id: e.pointerId, x: pt.x, y: pt.y });
});

screenWrap.addEventListener("pointermove", (e) => {
  if (!activePointers.has(e.pointerId)) return;
  const pt = clientToDisplay(e.clientX, e.clientY, true);
  if (!pt) return;
  const prev = activePointers.get(e.pointerId);
  if (prev && prev.x === pt.x && prev.y === pt.y) return;
  activePointers.set(e.pointerId, pt);
  sendInput({ t: "touch", phase: "move", id: e.pointerId, x: pt.x, y: pt.y });
});

function finishPointer(e, phase) {
  if (!activePointers.has(e.pointerId)) return;
  const pt = clientToDisplay(e.clientX, e.clientY, true) || activePointers.get(e.pointerId);
  activePointers.delete(e.pointerId);
  try {
    if (screenWrap.hasPointerCapture && screenWrap.hasPointerCapture(e.pointerId)) {
      screenWrap.releasePointerCapture(e.pointerId);
    }
  } catch (_) {}
  sendInput({ t: "touch", phase, id: e.pointerId, x: pt.x, y: pt.y });
}

screenWrap.addEventListener("pointerup", (e) => finishPointer(e, "up"));
screenWrap.addEventListener("pointercancel", (e) => finishPointer(e, "cancel"));

screenWrap.addEventListener("wheel", (e) => {
  if (!current) return;
  const pt = clientToDisplay(e.clientX, e.clientY, false) || {
    x: Math.round((display.w || 1080) / 2),
    y: Math.round((display.h || 2400) / 2),
  };
  e.preventDefault();
  if (current.watch_only) {
    applyToolbarUi();
    return;
  }
  const scale = e.deltaMode === 1 ? 40 : (e.deltaMode === 2 ? 400 : 1);
  sendInput({
    t: "scroll",
    x: pt.x,
    y: pt.y,
    dx: Math.round(e.deltaX * scale),
    dy: Math.round(e.deltaY * scale),
  });
}, { passive: false });

document.addEventListener("keydown", (e) => {
  if (!current || detail.hidden) return;
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  if (document.activeElement && document.activeElement.tagName === "BUTTON" && (e.key === "Enter" || e.key === " ")) {
    return;
  }
  const kc = KEY_MAP[e.key];
  if (kc) {
    e.preventDefault();
    if (!e.repeat) sendInput({ t: "key", code: kc, phase: "down" });
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
  if (kc) {
    e.preventDefault();
    sendInput({ t: "key", code: kc, phase: "up" });
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
btnRotate.addEventListener("click", () => {
  const next = (currentRotation + 1) % 4;
  if (sendInput({ t: "rotate", rotation: next })) {
    currentRotation = next;
  }
  screenWrap.focus({ preventScroll: true });
});

$("back").addEventListener("click", closeSession);
window.addEventListener("pagehide", () => {
  closeWs();
  stopFrameStream();
  if (source) source.close();
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
