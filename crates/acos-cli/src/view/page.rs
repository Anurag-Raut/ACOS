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
    color: var(--muted); font-size: 12px; }
  .pill.ok { color: var(--ok); border-color: #1f6f36; }
  .pill.bad { color: var(--bad); border-color: #6e2a26; }
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
  #detail { display: grid; grid-template-columns: minmax(0, 1fr) 340px; gap: 18px; padding: 18px; }
  @media (max-width: 900px) { #detail { grid-template-columns: minmax(0, 1fr); } }
  .bar { display: flex; gap: 12px; align-items: center; grid-column: 1 / -1; }
  .bar button { background: var(--panel); color: var(--text); border: 1px solid var(--line);
    border-radius: 8px; padding: 6px 12px; cursor: pointer; font-size: 13px; }
  .bar button:hover { border-color: var(--accent); }
  .bar .titles { min-width: 0; }
  .bar .titles .big { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .bar .titles .small { color: var(--muted); font-size: 12px; font-family: var(--mono);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .screenwrap { position: relative; background: #000; border: 1px solid var(--line);
    border-radius: 12px; overflow: hidden; align-self: start; }
  #frame { display: block; width: 100%; height: auto; }
  #offline { position: absolute; inset: auto 0 0 0; background: rgba(13,17,23,.82);
    color: var(--warn); font-size: 12px; padding: 6px 10px; }
  #overlay { position: absolute; inset: 0; pointer-events: none; }
  .ripple { position: absolute; width: 44px; height: 44px; margin: -22px 0 0 -22px;
    border: 2px solid var(--accent); border-radius: 50%; opacity: .95;
    animation: ripple .9s ease-out forwards; }
  .ripple.bad { border-color: var(--bad); }
  @keyframes ripple {
    from { transform: scale(.25); opacity: .95; }
    to { transform: scale(1.6); opacity: 0; }
  }
  .rip-tag { position: absolute; transform: translate(-50%, 26px); font: 11px/1 var(--mono);
    background: rgba(13,17,23,.85); border: 1px solid var(--line); border-radius: 6px;
    padding: 3px 6px; color: var(--text); animation: fade 1.4s ease-out forwards; }
  @keyframes fade { from { opacity: 1; } to { opacity: 0; } }
  aside { background: var(--panel); border: 1px solid var(--line); border-radius: 12px;
    padding: 12px 14px; min-width: 0; align-self: start; }
  aside h2 { font-size: 12px; text-transform: uppercase; letter-spacing: .08em;
    color: var(--muted); margin: 0 0 8px; }
  #events { list-style: none; margin: 0; padding: 0; max-height: 60vh; overflow-y: auto; }
  #events li { border-top: 1px solid var(--line); padding: 6px 0; font-size: 12px;
    display: grid; grid-template-columns: 58px minmax(0, 1fr); gap: 8px; }
  #events li:first-child { border-top: 0; }
  #events .t { color: var(--muted); font-family: var(--mono); }
  #events .what { min-width: 0; overflow-wrap: anywhere; }
  #events .cmd { font-weight: 600; }
  #events .st { font-family: var(--mono); font-size: 11px; }
  #events .st.ok { color: var(--ok); }
  #events .st.bad { color: var(--bad); }
  #events .txt { color: var(--muted); font-family: var(--mono); }
  .note { color: var(--muted); font-size: 12px; margin-top: 10px; }
  /* Watching one session: only the screen, scaled to fit the pane. */
  body.watching { height: 100vh; overflow: hidden; }
  body.watching header, body.watching aside, body.watching .bar .titles,
  body.watching .bar .spacer, body.watching .bar .pill { display: none; }
  body.watching #detail { display: flex; align-items: center; justify-content: center;
    height: 100vh; padding: 8px; gap: 0; }
  body.watching .bar { position: fixed; top: 8px; left: 8px; z-index: 5; }
  body.watching .bar button { opacity: .55; }
  body.watching .bar button:hover { opacity: 1; }
  body.watching .screenwrap { align-self: center; height: calc(100vh - 16px);
    aspect-ratio: var(--ar, 9 / 20); max-width: 100%; border-radius: 10px; }
  body.watching #frame { width: 100%; height: 100%; object-fit: contain; }
  [hidden] { display: none !important; }
</style>
</head>
<body>
<header>
  <h1>ACOS live view</h1>
  <span class="pill" id="status">connecting…</span>
  <span class="spacer"></span>
  <span class="pill">view-only · no input reaches the guest</span>
</header>
<main id="grid"></main>
<section id="detail" hidden>
  <div class="bar">
    <button id="back" type="button">← sessions</button>
    <div class="titles">
      <div class="big" id="dtitle">session</div>
      <div class="small" id="dsub"></div>
    </div>
    <span class="spacer"></span>
    <span class="pill" id="dstate">stream</span>
  </div>
  <div class="screenwrap">
    <img id="frame" alt="live screen">
    <div id="overlay"></div>
    <div id="offline" hidden>stream offline · reconnecting…</div>
  </div>
  <aside>
    <h2>activity</h2>
    <div class="mono" id="dactivity" style="font-size:12px;color:var(--muted)">-</div>
    <h2 style="margin-top:14px">recent actions</h2>
    <ol id="events" reversed></ol>
    <div class="note">Frames and events appear only while this page is open;
      the session costs nothing when nobody watches.</div>
  </aside>
</section>
<script>
"use strict";
const token = new URLSearchParams(location.search).get("t") || "";
const auth = (path) => path + (path.includes("?") ? "&" : "?") + "t=" + encodeURIComponent(token);
const $ = (id) => document.getElementById(id);
const grid = $("grid"), detail = $("detail"), statusPill = $("status");
const frame = $("frame"), overlay = $("overlay"), eventsList = $("events");

let sessions = [];
let current = null;   // { session_id, display: {w,h} } while a session is open
let source = null;    // EventSource
let display = { w: 0, h: 0 };
let pollTimer = null;

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
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

function renderGrid() {
  grid.textContent = "";
  if (!sessions.length) {
    const empty = el("div", "empty", "No live sessions. Start one with acos_launch — this page refreshes by itself.");
    grid.appendChild(empty);
    return;
  }
  for (const s of sessions) {
    const card = el("div", "card" + (s.stream ? "" : " off"));
    const top = el("div", "top");
    top.appendChild(el("div", "id", s.session_id));
    const spacer = el("span"); spacer.style.flex = "1"; top.appendChild(spacer);
    if (s.stream) {
      top.appendChild(el("span", "pill" + (s.connected ? " ok" : ""), s.watchers > 0 ? "watching" : "live"));
    } else {
      top.appendChild(el("span", "pill", "no stream"));
    }
    card.appendChild(top);
    card.appendChild(el("div", "pkg", s.package || "-"));
    card.appendChild(el("div", "line mono", s.activity || "activity unknown"));
    const proj = el("div", "line", "project: " + projectName(s.project_dir));
    proj.title = s.project_dir || "";
    card.appendChild(proj);
    const foot = el("div", "foot");
    foot.appendChild(el("span", null, clientLabel(s)));
    foot.appendChild(el("span", null, age(s.age_ms) + " old"));
    foot.appendChild(el("span", null, (s.display ? s.display.w + "×" + s.display.h : "-")));
    card.appendChild(foot);
    if (s.stream) {
      card.addEventListener("click", () => openSession(s));
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
    if (current && !sessions.some((s) => s.session_id === current.session_id)) {
      // The session ended: keep the last frame, say so, keep the page usable.
      $("dstate").textContent = "session ended";
      $("dstate").className = "pill bad";
    }
  } catch (error) {
    statusPill.textContent = "hub unreachable · " + error.message;
    statusPill.className = "pill bad";
  }
  clearTimeout(pollTimer);
  pollTimer = setTimeout(refresh, 2000);
}

function openSession(s) {
  current = s;
  display = { w: (s.display && s.display.w) || 0, h: (s.display && s.display.h) || 0 };
  grid.hidden = true;
  detail.hidden = false;
  document.body.classList.add("watching");
  $("dtitle").textContent = s.package || s.session_id;
  $("dsub").textContent = s.session_id + " · " + clientLabel(s) + " · " + (s.project_dir || "-");
  $("dactivity").textContent = s.activity || "-";
  eventsList.textContent = "";
  $("dstate").textContent = s.stream ? "connecting" : "no stream";
  $("dstate").className = "pill";
  $("offline").hidden = true;
  frame.removeAttribute("src");
  if (s.stream) {
    frame.src = auth("/api/frames/" + encodeURIComponent(s.session_id));
    if (s.display && s.display.w && s.display.h) {
      frame.parentElement.style.setProperty("--ar", s.display.w + " / " + s.display.h);
    }
    connectEvents(s);
  }
  location.hash = "s=" + encodeURIComponent(s.session_id);
  history.replaceState(null, "", location.pathname + location.search + location.hash);
}

function closeSession() {
  if (source) { source.close(); source = null; }
  frame.removeAttribute("src");
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

// A decoded frame is the strongest liveness signal there is.
frame.addEventListener("load", markLive);

function connectEvents(s) {
  source = new EventSource(auth("/api/events/" + encodeURIComponent(s.session_id)));
  source.addEventListener("state", (message) => {
    let data = {};
    try { data = JSON.parse(message.data); } catch (error) { return; }
    const entry = data.entry || {};
    if (entry.display && entry.display.w) display = { w: entry.display.w, h: entry.display.h };
    if (entry.activity) $("dactivity").textContent = entry.activity;
    $("offline").hidden = !!data.connected;
    $("dstate").textContent = data.connected ? "live" : "reconnecting";
    $("dstate").className = "pill" + (data.connected ? " ok" : " bad");
  });
  source.addEventListener("ev", (message) => {
    let data = {};
    try { data = JSON.parse(message.data); } catch (error) { return; }
    // An event from the session proves the upstream is live: the one-shot
    // `state` sent at subscribe time can predate the hub's connection.
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
  const width = display.w || 1080, height = display.h || 2400;
  const ok = !event.status || event.status === "ok";
  const dot = el("div", "ripple" + (ok ? "" : " bad"));
  dot.style.left = (100 * event.x / width) + "%";
  dot.style.top = (100 * event.y / height) + "%";
  overlay.appendChild(dot);
  const tag = el("div", "rip-tag", (event.cmd || "tap") + " " + event.x + "," + event.y
    + (ok ? "" : " · " + event.status));
  tag.style.left = (100 * event.x / width) + "%";
  tag.style.top = (100 * event.y / height) + "%";
  overlay.appendChild(tag);
  setTimeout(() => { dot.remove(); tag.remove(); }, 1400);
}

function addEvent(event, kind) {
  const item = el("li");
  const stamp = new Date(event.ts_ms || Date.now());
  item.appendChild(el("span", "t", stamp.toLocaleTimeString([], { hour12: false })));
  const what = el("div", "what");
  const label = event.cmd || event.kind || kind;
  what.appendChild(el("span", "cmd", label));
  if (event.source) {
    what.appendChild(el("span", "pill", event.source));
  }
  if (event.status) {
    const status = el("span", "st " + (event.status === "ok" ? "ok" : "bad"), " " + event.status);
    what.appendChild(status);
  }
  if (typeof event.x === "number" && typeof event.y === "number") {
    what.appendChild(el("span", "txt", " @ " + event.x + "," + event.y
      + (typeof event.x2 === "number" ? " → " + event.x2 + "," + event.y2 : "")));
  }
  if (event.key) what.appendChild(el("span", "txt", " key " + event.key));
  if (event.text) what.appendChild(el("span", "txt", " “" + event.text + "”"));
  if (event.message) what.appendChild(el("span", "txt", " " + event.message));
  if (event.activity && event.kind === "activity") what.appendChild(el("span", "txt", " " + event.activity));
  item.appendChild(what);
  eventsList.prepend(item);
  while (eventsList.children.length > 40) eventsList.lastChild.remove();
}

$("back").addEventListener("click", closeSession);
window.addEventListener("pagehide", () => { if (source) source.close(); });
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
