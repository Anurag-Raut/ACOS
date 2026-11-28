//! The one self-contained page `acos view` serves: inline CSS and JS, no CDN,
//! no external request. The token travels only in `location.search` (the URL the
//! agent opened), so every API call appends it to its own query string.

/// The page (inline CSS/JS; no network beyond this origin).
pub const PAGE: &str = r####"<!doctype html>
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
  #mirror-dom { position: absolute; z-index: 1; pointer-events: none; overflow: hidden; }
  .acos-mirror-node { position: absolute; background: transparent; border: 0; margin: 0; padding: 0;
    box-sizing: border-box; pointer-events: auto; cursor: inherit; }
  .screenwrap.annotating { cursor: crosshair; }
  .screenwrap.annotating.tool-pick { cursor: pointer; }
  #annot-layer { position: absolute; inset: 0; pointer-events: none; z-index: 5; overflow: hidden; }
  #annot-svg { position: absolute; inset: 0; width: 100%; height: 100%; pointer-events: none; overflow: hidden; }
  .annot-hover-box { position: absolute; border: 2px solid var(--accent); background: rgba(79, 156, 249, .15);
    border-radius: 3px; pointer-events: none; }
  .annot-hover-label { position: absolute; font: 11px/1.25 var(--mono); background: rgba(13, 17, 23, .94);
    color: var(--text); border: 1px solid var(--accent); border-radius: 5px; padding: 2px 6px;
    white-space: nowrap; max-width: min(360px, 90%); overflow: hidden; text-overflow: ellipsis;
    pointer-events: none; box-shadow: 0 4px 12px rgba(0, 0, 0, .5); }
  .annot-draft-box { position: absolute; border: 2px dashed #ff6b4a; background: rgba(255, 107, 74, .16);
    border-radius: 3px; pointer-events: none; }
  .annot-pin-box { position: absolute; border: 2px solid #ff6b4a; background: rgba(255, 107, 74, .10);
    border-radius: 3px; pointer-events: none; }
  .annot-pin-badge { position: absolute; min-width: 20px; height: 20px; padding: 0 5px; border-radius: 999px;
    background: #ff6b4a; color: #0d1117; font: 700 11px/20px var(--mono); text-align: center;
    box-shadow: 0 2px 8px rgba(0, 0, 0, .6); pointer-events: none; transform: translate(-35%, -35%); }
  #annot-popover { position: absolute; z-index: 6; width: min(295px, calc(100% - 16px));
    background: rgba(22, 27, 34, .98); border: 1px solid var(--accent); border-radius: 10px;
    padding: 10px 12px; box-shadow: 0 12px 32px rgba(0, 0, 0, .65); pointer-events: auto; }
  .annot-pop-target { font: 11px/1.35 var(--mono); color: var(--accent); margin-bottom: 6px;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  #annot-comment { width: 100%; min-height: 62px; resize: vertical; background: var(--bg);
    color: var(--text); border: 1px solid var(--line); border-radius: 7px; padding: 6px 8px;
    font: 12px/1.4 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; outline: none; }
  #annot-comment:focus { border-color: var(--accent); }
  .annot-pop-err { color: var(--bad); font: 11px/1.3 var(--mono); margin-top: 5px; }
  .annot-pop-actions { display: flex; gap: 6px; justify-content: flex-end; margin-top: 8px; }
  .annot-pop-actions button { background: var(--bg); color: var(--text); border: 1px solid var(--line);
    border-radius: 7px; padding: 4px 10px; cursor: pointer; font-size: 12px; }
  .annot-pop-actions button:hover:not(:disabled) { border-color: var(--accent); }
  .annot-pop-actions button.primary { background: rgba(79, 156, 249, .22); border-color: var(--accent); color: #d6e8ff; font-weight: 600; }
  #annot-tools { display: inline-flex; gap: 4px; align-items: center; flex-wrap: wrap; }
  .aside-tabs { display: flex; gap: 6px; border-bottom: 1px solid var(--line); padding-bottom: 8px; margin-bottom: 8px; flex-shrink: 0; }
  .aside-tab { flex: 1; background: transparent; color: var(--muted); border: 1px solid transparent;
    border-radius: 7px; padding: 4px 8px; cursor: pointer; font-size: 11px; font-weight: 600;
    text-transform: uppercase; letter-spacing: .04em; text-align: center; }
  .aside-tab:hover { color: var(--text); border-color: var(--line); }
  .aside-tab.active { color: var(--text); background: rgba(79, 156, 249, .14); border-color: #264f78; }
  #pane-activity { display: flex; flex-direction: column; flex: 1; min-height: 0; }
  #pane-annotations { display: flex; flex-direction: column; flex: 1; min-height: 0; gap: 8px; }
  #annot-bar { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; flex-shrink: 0; }
  #annot-bar button { background: var(--bg); color: var(--text); border: 1px solid var(--line);
    border-radius: 7px; padding: 4px 8px; cursor: pointer; font-size: 11px; }
  #annot-bar button:hover:not(:disabled) { border-color: var(--accent); }
  #annot-bar button.primary { background: rgba(79, 156, 249, .18); border-color: var(--accent); color: #cde3ff; font-weight: 600; }
  #annot-bar button:disabled { opacity: .45; cursor: not-allowed; }
  .annot-filter { margin-left: auto; display: inline-flex; align-items: center; gap: 4px; font-size: 11px; color: var(--muted); cursor: pointer; user-select: none; }
  .annot-empty { color: var(--muted); font-size: 12px; padding: 14px 2px; line-height: 1.45; }
  #annot-list { list-style: none; margin: 0; padding: 0; flex: 1; min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 8px; }
  .annot-item { background: rgba(13, 17, 23, .65); border: 1px solid var(--line); border-radius: 8px; padding: 8px 9px; font-size: 12px; }
  .annot-item.resolved { opacity: .65; }
  .annot-item-head { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; margin-bottom: 4px; }
  .annot-num { display: inline-block; min-width: 22px; padding: 0 5px; border-radius: 999px;
    background: #ff6b4a; color: #0d1117; font: 700 11px/18px var(--mono); text-align: center; }
  .annot-item.resolved .annot-num { background: var(--muted); }
  .annot-tag { font: 10px/1.3 var(--mono); border: 1px solid var(--line); border-radius: 4px; padding: 1px 5px; color: var(--muted); }
  .annot-tag.ok { color: var(--ok); border-color: #1f6f36; }
  .annot-act { font: 11px/1.3 var(--mono); color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0; }
  .annot-view { font: 11px/1.35 var(--mono); color: var(--accent); overflow-wrap: anywhere; margin-bottom: 4px; }
  .annot-comment { color: var(--text); line-height: 1.4; white-space: pre-wrap; overflow-wrap: anywhere; margin-bottom: 6px; }
  .annot-shot { font: 10px/1.3 var(--mono); color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; margin-bottom: 6px; }
  .annot-item-actions { display: flex; gap: 5px; flex-wrap: wrap; }
  .annot-item-actions button { background: var(--panel); color: var(--text); border: 1px solid var(--line);
    border-radius: 6px; padding: 2px 7px; cursor: pointer; font-size: 11px; }
  .annot-item-actions button:hover:not(:disabled) { border-color: var(--accent); }
  .annot-item-actions button.danger:hover:not(:disabled) { border-color: var(--bad); color: var(--bad); }
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
      <span class="sep" aria-hidden="true"></span>
      <button id="btn-annotate" type="button" aria-pressed="false" title="Inspect Android views and draw visual annotations">Annotate</button>
      <span id="annot-tools" hidden>
        <button id="btn-annot-pick" type="button" class="active" aria-pressed="true" title="Click an Android view to annotate">Pick</button>
        <button id="btn-annot-box" type="button" aria-pressed="false" title="Drag a bounding box to annotate">Box</button>
        <button id="btn-annot-draw" type="button" aria-pressed="false" title="Draw a freehand highlight to annotate">Draw</button>
        <button id="btn-annot-copy" type="button" title="Copy all open annotations for your coding agent">Copy for Agent</button>
      </span>
    </div>
    <span class="spacer"></span>
    <span class="pill" id="dinput">idle</span>
    <span class="pill" id="dstate">stream</span>
  </div>
  <div class="stage">
    <div class="screenwrap" id="screenwrap" tabindex="0">
      <canvas id="frame" width="0" height="0" aria-label="live screen"></canvas>
      <div id="mirror-dom" aria-label="Android UI hierarchy mirror"></div>
      <svg id="gesture-svg" aria-hidden="true"></svg>
      <div id="overlay"></div>
      <div id="annot-layer" aria-hidden="true"><svg id="annot-svg" aria-hidden="true"></svg></div>
      <div id="annot-popover" hidden>
        <div class="annot-pop-target" id="annot-pop-target"></div>
        <textarea id="annot-comment" rows="3" placeholder="Describe the issue or change… (Enter to add, Esc to cancel)"></textarea>
        <div class="annot-pop-err" id="annot-pop-err" hidden></div>
        <div class="annot-pop-actions">
          <button id="annot-cancel" type="button">Cancel</button>
          <button id="annot-submit" type="button" class="primary">Add</button>
        </div>
      </div>
      <div id="offline" hidden>stream offline · reconnecting…</div>
    </div>
    <aside>
      <h2>activity</h2>
      <div class="mono" id="dactivity" style="font-size:12px;color:var(--muted);margin-bottom:8px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">-</div>
      <div class="aside-tabs" role="tablist">
        <button id="tab-activity" class="aside-tab active" type="button" role="tab" aria-selected="true">Activity</button>
        <button id="tab-annotations" class="aside-tab" type="button" role="tab" aria-selected="false">Annotations (0)</button>
      </div>
      <div id="pane-activity">
        <h2>agent &amp; human activity</h2>
        <ol id="events" reversed></ol>
        <div class="note">Click: tap · Drag: swipe/fling · Hold: long-press · Wheel: scroll · Alt+drag / Ctrl+wheel: pinch · Type / Ctrl+V: text. Verify sessions stay read-only.</div>
      </div>
      <div id="pane-annotations" hidden>
        <div id="annot-bar">
          <button id="btn-annot-copy-all" type="button" class="primary">Copy All for Agent</button>
          <label class="annot-filter"><input id="annot-show-resolved" type="checkbox"> Resolved</label>
          <button id="btn-annot-clear" type="button">Clear</button>
        </div>
        <div id="annot-empty" class="annot-empty">No open annotations yet. Click Annotate to pick a view, draw a box, or sketch on the screen.</div>
        <ol id="annot-list"></ol>
      </div>
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
const frame = $("frame"), frameCtx = frame.getContext("2d"), screenWrap = $("screenwrap"), mirrorDom = $("mirror-dom"), gestureSvg = $("gesture-svg"), overlay = $("overlay"), annotLayer = $("annot-layer"), annotSvg = $("annot-svg"), annotPopover = $("annot-popover"), annotPopTarget = $("annot-pop-target"), annotComment = $("annot-comment"), annotPopErr = $("annot-pop-err"), annotSubmit = $("annot-submit"), annotCancel = $("annot-cancel"), eventsList = $("events"), capTray = $("cap-tray");
const btnBack = $("btn-back"), btnHome = $("btn-home"), btnRecents = $("btn-recents"), btnRotate = $("btn-rotate");
const btnVolDown = $("btn-vol-down"), btnVolUp = $("btn-vol-up"), btnPower = $("btn-power"), btnPinch = $("btn-pinch"), btnShot = $("btn-shot");
const btnAnnotate = $("btn-annotate"), annotTools = $("annot-tools"), btnAnnotPick = $("btn-annot-pick"), btnAnnotBox = $("btn-annot-box"), btnAnnotDraw = $("btn-annot-draw"), btnAnnotCopy = $("btn-annot-copy");
const tabActivity = $("tab-activity"), tabAnnotations = $("tab-annotations"), paneActivity = $("pane-activity"), paneAnnotations = $("pane-annotations");
const btnAnnotCopyAll = $("btn-annot-copy-all"), btnAnnotClear = $("btn-annot-clear"), annotShowResolved = $("annot-show-resolved"), annotEmpty = $("annot-empty"), annotList = $("annot-list");
const verifyBanner = $("verify-banner"), dinput = $("dinput");
const SVG_NS = "http://www.w3.org/2000/svg";
const capabilityCards = new Map();
// prompt_seq of capability actions sent (or queued) but not yet answered.
const pendingCapabilities = new Set();

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

let mirrorNodes = [];
let mirrorMeta = { activity: "", package: "", display: { w: 0, h: 0 } };
let mirrorFetchTimer = null;
let mirrorFetchInFlight = false;
let mirrorFetchPending = false;
let mirrorGeneration = 0;
let lastTreeFrameSeq = -1;

let annotMode = false;
let annotTool = "pick"; // "pick" | "box" | "draw"
let annotItems = [];
let annotOpenCount = 0;
let annotShowResolvedFlag = false;
let asideTab = "activity"; // "activity" | "annotations"
let hoveredMirrorNode = null;
let annotDraft = null;
let annotPending = null;
let annotSubmitting = false;
let annotGeneration = 0;

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

  if (sessionEnded && annotMode) {
    annotMode = false;
  }
  const annotDisabled = !current || sessionEnded;
  btnAnnotate.disabled = annotDisabled;
  btnAnnotate.classList.toggle("active", annotMode);
  btnAnnotate.setAttribute("aria-pressed", String(annotMode));
  btnAnnotate.textContent = annotOpenCount > 0 ? "Annotate (" + annotOpenCount + ")" : "Annotate";
  annotTools.hidden = !annotMode;
  btnAnnotPick.classList.toggle("active", annotTool === "pick");
  btnAnnotPick.setAttribute("aria-pressed", String(annotTool === "pick"));
  btnAnnotBox.classList.toggle("active", annotTool === "box");
  btnAnnotBox.setAttribute("aria-pressed", String(annotTool === "box"));
  btnAnnotDraw.classList.toggle("active", annotTool === "draw");
  btnAnnotDraw.setAttribute("aria-pressed", String(annotTool === "draw"));
  btnAnnotCopy.disabled = annotOpenCount === 0;
  screenWrap.classList.toggle("annotating", annotMode);
  screenWrap.classList.toggle("tool-pick", annotMode && annotTool === "pick");

  if (annotMode) {
    dinput.textContent = "annotating";
    dinput.className = "pill warn";
  } else if (isWatchOnly) {
    dinput.textContent = "watch-only";
    dinput.className = "pill warn";
  } else if (!ws) {
    dinput.textContent = "idle";
    dinput.className = "pill";
  } else if (ws.readyState === WebSocket.OPEN) {
    dinput.textContent = "interactive";
    dinput.className = "pill ok";
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
          const prevAct = current.activity || "";
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
            positionMirrorDom();
          }
          if (updated.activity) {
            $("dactivity").textContent = updated.activity;
            if (updated.activity !== prevAct) {
              scheduleTreeRefresh(300);
              renderAnnotOverlay();
            }
          }
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
  failPendingCapabilities();
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
    failPendingCapabilities();
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
  positionMirrorDom();
  if (prevOrientationFlipped || lastFrameSeq !== lastTreeFrameSeq) {
    scheduleTreeRefresh(prevOrientationFlipped ? 150 : 300);
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
  resetMirrorDom();
  resetAnnotationsState();
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
  setAsideTab("activity");
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
  scheduleTreeRefresh(150);
  fetchAnnotations();
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
  resetMirrorDom();
  resetAnnotationsState();
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
  annotMode = false;
  annotDraft = null;
  hoveredMirrorNode = null;
  closeAnnotPopover();
  cancelMirrorFetch();
  resetPanelTouchState();
  closeWs();
  stopFrameStream();
  if (source) { source.close(); source = null; }
  applyToolbarUi();
  renderAnnotOverlay();
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
      positionMirrorDom();
    }
    const prevAct = current ? (current.activity || "") : "";
    if (current) {
      if (typeof entry.watch_only === "boolean") {
        current.watch_only = entry.watch_only;
      }
      if (typeof entry.has_home === "boolean") {
        current.has_home = entry.has_home;
      } else if (typeof hello.has_home === "boolean") {
        current.has_home = hello.has_home;
      }
      if (entry.activity) {
        current.activity = entry.activity;
      }
      applyToolbarUi();
    }
    if (entry.activity) {
      $("dactivity").textContent = entry.activity;
      if (entry.activity !== prevAct) {
        scheduleTreeRefresh(300);
        renderAnnotOverlay();
      }
    }
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
  pendingCapabilities.clear();
  if (capTray) capTray.textContent = "";
}

function copyTextToClipboard(text, button, fallbackLabel, copiedLabel) {
  const done = () => {
    if (!button) return;
    const prev = button.textContent;
    button.textContent = copiedLabel || "Copied";
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
        pendingCapabilities.add(prompt.seq);
        const ok = sendInput({
          t: "capability",
          prompt_seq: prompt.seq,
          option_id: opt.id || null,
          modules: Array.isArray(opt.modules) ? opt.modules : [],
          control: opt.control || null,
          replay_intent: typeof opt.replay_intent === "number" ? opt.replay_intent : null,
        });
        if (!ok) {
          pendingCapabilities.delete(prompt.seq);
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

// Socket closed with capability actions unanswered: the action may or may not
// have been dispatched. Re-enable the card for an explicit retry; never replay.
function failPendingCapabilities() {
  for (const seq of pendingCapabilities) {
    const rec = capabilityCards.get(seq);
    if (!rec) continue;
    for (const b of rec.buttons) b.disabled = !!(current && current.watch_only);
    rec.statusEl.hidden = false;
    rec.statusEl.className = "cap-status bad";
    rec.statusEl.textContent =
      "Connection lost: the result is uncertain (the action may have been applied). Check the session, then retry if needed.";
  }
  pendingCapabilities.clear();
}

function handleCapabilityReply(msg) {
  const seq = typeof msg.prompt_seq === "number" ? msg.prompt_seq : null;
  if (seq !== null) pendingCapabilities.delete(seq);
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
      setTimeout(() => {
        if (capabilityCards.get(seq) === rec) dismissCapabilityPrompt(seq);
      }, 750);
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
    if (current && event.activity) current.activity = event.activity;
    addEvent(event, kind);
    scheduleTreeRefresh(300);
    renderAnnotOverlay();
    return;
  }
  if (event.kind === "activity" && event.activity) {
    $("dactivity").textContent = event.activity;
    if (current) current.activity = event.activity;
    addEvent(event, "activity");
    scheduleTreeRefresh(300);
    renderAnnotOverlay();
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
    scheduleTreeRefresh(300);
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
      scheduleTreeRefresh(300);
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
  if (event.kind === "action") {
    scheduleTreeRefresh(300);
  }
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

// ---- Mirror DOM (native T3 Code & Paseo support) + Built-in Annotate UI ----

function isInsideElement(node, ancestor) {
  let cur = node;
  while (cur) {
    if (cur === ancestor) return true;
    cur = cur.parentNode;
  }
  return false;
}

function isEditableElement(node) {
  if (!node) return false;
  const tag = node.tagName;
  return tag === "TEXTAREA" || tag === "INPUT" || tag === "SELECT" || !!node.isContentEditable;
}

function parseBounds(raw) {
  if (!raw) return null;
  let l, t, r, b;
  if (Array.isArray(raw) && raw.length === 4) {
    [l, t, r, b] = raw.map(Number);
  } else if (typeof raw === "string") {
    const m = raw.match(/^\[\s*(-?\d+)\s*,\s*(-?\d+)\s*\]\[\s*(-?\d+)\s*,\s*(-?\d+)\s*\]$/)
      || raw.match(/^\[\s*(-?\d+)\s*,\s*(-?\d+)\s*,\s*(-?\d+)\s*,\s*(-?\d+)\s*\]$/);
    if (!m) return null;
    l = Number(m[1]);
    t = Number(m[2]);
    r = Number(m[3]);
    b = Number(m[4]);
  } else if (typeof raw === "object" && typeof raw.left === "number") {
    l = raw.left; t = raw.top; r = raw.right; b = raw.bottom;
  } else {
    return null;
  }
  if (![l, t, r, b].every(Number.isFinite) || r <= l || b <= t) return null;
  return [Math.round(l), Math.round(t), Math.round(r), Math.round(b)];
}

function shortClassName(cls) {
  if (!cls) return "View";
  const str = String(cls);
  const dot = str.lastIndexOf(".");
  return dot >= 0 ? str.slice(dot + 1) : str;
}

function shortResourceId(rid) {
  if (!rid) return "";
  const str = String(rid);
  const slash = str.indexOf(":id/");
  return slash >= 0 ? str.slice(slash + 4) : str;
}

function trimSnippet(s, maxLen) {
  if (!s) return "";
  const clean = String(s).replace(/\s+/g, " ").trim();
  if (!clean) return "";
  return clean.length > maxLen ? clean.slice(0, maxLen - 1) + "…" : clean;
}

function formatMirrorAriaLabel(n) {
  const parts = [shortClassName(n.class)];
  const labelText = trimSnippet(n.text, 60) || trimSnippet(n.content_desc, 60);
  if (labelText) parts.push('"' + labelText + '"');
  if (n.resource_id) parts.push("(id: " + shortResourceId(n.resource_id) + ")");
  if (n.bounds) parts.push("[" + n.bounds.join(",") + "]");
  return parts.join(" ");
}

function formatNodeSummary(n) {
  if (!n) return "";
  const parts = [shortClassName(n.class)];
  if (n.resource_id) parts.push("#" + shortResourceId(n.resource_id));
  const txt = trimSnippet(n.text, 44) || trimSnippet(n.content_desc, 44);
  if (txt) parts.push('"' + txt + '"');
  const b = parseBounds(n.bounds);
  if (b) parts.push("[" + b.join(",") + "]");
  return parts.join(" · ");
}

function snapshotNode(n) {
  const out = {
    class: n.class || "",
    bounds: n.bounds,
    clickable: !!n.clickable,
  };
  if (n.resource_id) out.resource_id = n.resource_id;
  if (n.text) out.text = trimSnippet(n.text, 200);
  if (n.content_desc) out.content_desc = trimSnippet(n.content_desc, 200);
  if (n.window) out.window = n.window;
  return out;
}

function currentDisplaySize() {
  const dw = mirrorMeta.display.w || display.w || frameSize.w || frame.width || 1080;
  const dh = mirrorMeta.display.h || display.h || frameSize.h || frame.height || 2400;
  return { w: dw, h: dh };
}

function currentActivityName() {
  const act = mirrorMeta.activity || (current && current.activity) || "";
  if (act) return act;
  const shown = $("dactivity").textContent;
  return shown && shown !== "-" ? shown : "";
}

function flattenTreeNodes(tree, activity, pkg, disp) {
  const out = [];
  if (!tree) return out;
  const roots = Array.isArray(tree.nodes)
    ? tree.nodes
    : (Array.isArray(tree) ? tree : (typeof tree === "object" ? [tree] : []));
  const dw = (disp && disp.w) || currentDisplaySize().w;
  const dh = (disp && disp.h) || currentDisplaySize().h;
  const MAX_MIRROR_NODES = 1500;

  function walk(node, depth) {
    if (!node || typeof node !== "object" || out.length >= MAX_MIRROR_NODES) return false;
    if (node.visible === false) return false;
    const b = parseBounds(node.visible_bounds) || parseBounds(node.bounds);
    const onScreen = b && b[2] > 0 && b[3] > 0 && b[0] < dw && b[1] < dh;
    const clamped = onScreen
      ? [
          Math.max(0, Math.min(dw, b[0])),
          Math.max(0, Math.min(dh, b[1])),
          Math.max(0, Math.min(dw, b[2])),
          Math.max(0, Math.min(dh, b[3])),
        ]
      : null;
    const valid = clamped && clamped[2] > clamped[0] && clamped[3] > clamped[1];
    let rec = null;
    if (valid) {
      rec = {
        class: typeof node.class === "string" ? node.class : "",
        resource_id: typeof node.resource_id === "string" ? node.resource_id : "",
        text: typeof node.text === "string" ? node.text.trim() : "",
        content_desc: typeof node.content_desc === "string" ? node.content_desc.trim() : "",
        bounds: clamped,
        clickable: !!node.clickable,
        scrollable: !!node.scrollable,
        window: typeof node.window === "string" ? node.window : "",
        activity: activity || "",
        package: (typeof node.package === "string" && node.package) ? node.package : (pkg || ""),
        depth,
        order: out.length,
        isLeaf: true,
      };
      out.push(rec);
    }
    let anyVisibleChild = false;
    if (Array.isArray(node.children)) {
      for (const child of node.children) {
        if (walk(child, depth + 1)) anyVisibleChild = true;
      }
    }
    if (rec && anyVisibleChild) rec.isLeaf = false;
    return valid || anyVisibleChild;
  }

  for (const root of roots) {
    walk(root, 0);
  }
  return out;
}

function positionMirrorDom() {
  if (!mirrorDom || !screenWrap) return;
  const imgRect = renderedImageRect();
  const wrapRect = screenWrap.getBoundingClientRect();
  if (imgRect.width > 0 && imgRect.height > 0 && wrapRect.width > 0 && wrapRect.height > 0) {
    mirrorDom.style.left = (imgRect.left - wrapRect.left).toFixed(2) + "px";
    mirrorDom.style.top = (imgRect.top - wrapRect.top).toFixed(2) + "px";
    mirrorDom.style.width = imgRect.width.toFixed(2) + "px";
    mirrorDom.style.height = imgRect.height.toFixed(2) + "px";
  }
  renderAnnotOverlay();
}

function renderMirrorDom() {
  if (!mirrorDom) return;
  positionMirrorDom();
  mirrorDom.textContent = "";
  if (!mirrorNodes.length) return;
  const { w: dw, h: dh } = currentDisplaySize();
  if (!(dw > 0 && dh > 0)) return;
  for (const n of mirrorNodes) {
    const [l, t, r, b] = n.bounds;
    const div = el("div", "acos-mirror-node");
    div.style.left = ((l / dw) * 100).toFixed(4) + "%";
    div.style.top = ((t / dh) * 100).toFixed(4) + "%";
    div.style.width = (((r - l) / dw) * 100).toFixed(4) + "%";
    div.style.height = (((b - t) / dh) * 100).toFixed(4) + "%";
    div.setAttribute("data-acos-node", "1");
    if (n.class) div.setAttribute("data-android-class", n.class);
    if (n.resource_id) div.setAttribute("data-resource-id", n.resource_id);
    if (n.text) div.setAttribute("data-text", trimSnippet(n.text, 120));
    if (n.content_desc) div.setAttribute("data-content-desc", trimSnippet(n.content_desc, 120));
    div.setAttribute("data-bounds", "[" + n.bounds.join(",") + "]");
    div.setAttribute("data-clickable", String(!!n.clickable));
    if (n.activity) div.setAttribute("data-activity", n.activity);
    if (n.package) div.setAttribute("data-package", n.package);
    div.setAttribute("aria-label", formatMirrorAriaLabel(n));
    div._acosNode = n;
    mirrorDom.appendChild(div);
  }
}

function cancelMirrorFetch() {
  mirrorGeneration += 1;
  if (mirrorFetchTimer) {
    clearTimeout(mirrorFetchTimer);
    mirrorFetchTimer = null;
  }
  mirrorFetchPending = false;
}

function resetMirrorDom() {
  cancelMirrorFetch();
  mirrorNodes = [];
  mirrorMeta = { activity: "", package: "", display: { w: 0, h: 0 } };
  lastTreeFrameSeq = -1;
  hoveredMirrorNode = null;
  if (mirrorDom) mirrorDom.textContent = "";
}

function scheduleTreeRefresh(delayMs) {
  if (!current || sessionEnded) return;
  const wait = typeof delayMs === "number" ? delayMs : 300;
  if (mirrorFetchTimer) clearTimeout(mirrorFetchTimer);
  mirrorFetchTimer = setTimeout(() => {
    mirrorFetchTimer = null;
    fetchTreeNow();
  }, wait);
}

async function fetchTreeNow() {
  if (!current || sessionEnded) return;
  if (mirrorFetchInFlight) {
    mirrorFetchPending = true;
    return;
  }
  const sid = current.session_id;
  const gen = mirrorGeneration;
  const g = (typeof currentGuestGen === "number" && currentGuestGen > 0) ? currentGuestGen : null;
  const seqAtStart = lastFrameSeq;
  mirrorFetchInFlight = true;
  try {
    const path = "/api/tree/" + encodeURIComponent(sid)
      + (g !== null ? "?g=" + encodeURIComponent(g) : "");
    const resp = await fetch(auth(path), { cache: "no-store" });
    if (mirrorGeneration !== gen || !current || current.session_id !== sid || sessionEnded) return;
    if (resp.status === 409) {
      markSessionEnded(sid);
      return;
    }
    if (!resp.ok) return;
    const data = await resp.json();
    if (mirrorGeneration !== gen || !current || current.session_id !== sid || sessionEnded) return;
    if (!data || !data.ok) return;
    lastTreeFrameSeq = seqAtStart;
    const act = (typeof data.activity === "string" && data.activity) ? data.activity : currentActivityName();
    const pkg = (typeof data.package === "string" && data.package) ? data.package : ((current && current.package) || "");
    const disp = (data.display && data.display.w > 0 && data.display.h > 0)
      ? { w: data.display.w, h: data.display.h }
      : { w: display.w || frameSize.w || 0, h: display.h || frameSize.h || 0 };
    mirrorMeta = { activity: act, package: pkg, display: disp };
    if (act) {
      $("dactivity").textContent = act;
      if (current) current.activity = act;
    }
    mirrorNodes = flattenTreeNodes(data.tree, act, pkg, disp);
    renderMirrorDom();
  } catch (_) {
    // Tree fetch is best-effort; ignore transient disconnects while frame/SSE handle health.
  } finally {
    mirrorFetchInFlight = false;
    if (mirrorFetchPending && current && !sessionEnded) {
      mirrorFetchPending = false;
      scheduleTreeRefresh(300);
    }
  }
}

function findDeepestNodeAt(pt) {
  if (!pt || !mirrorNodes.length) return null;
  const hits = [];
  for (let i = mirrorNodes.length - 1; i >= 0; i--) {
    const n = mirrorNodes[i];
    const [l, t, r, b] = n.bounds;
    if (pt.x >= l && pt.x <= r && pt.y >= t && pt.y <= b) {
      hits.push(n);
    }
  }
  if (!hits.length) return null;
  hits.sort((a, b) => {
    const areaA = (a.bounds[2] - a.bounds[0]) * (a.bounds[3] - a.bounds[1]);
    const areaB = (b.bounds[2] - b.bounds[0]) * (b.bounds[3] - b.bounds[1]);
    if (areaA !== areaB) return areaA - areaB;
    if (a.depth !== b.depth) return b.depth - a.depth;
    return b.order - a.order;
  });
  return hits[0];
}

function findNodesIntersecting(rect) {
  if (!rect || !mirrorNodes.length) return [];
  const [rl, rt, rr, rb] = rect;
  const { w: dw, h: dh } = currentDisplaySize();
  const screenArea = Math.max(1, dw * dh);
  const matches = [];
  for (const n of mirrorNodes) {
    const [l, t, r, b] = n.bounds;
    if (r <= rl || l >= rr || b <= rt || t >= rb) continue;
    const ix = Math.max(0, Math.min(r, rr) - Math.max(l, rl));
    const iy = Math.max(0, Math.min(b, rb) - Math.max(t, rt));
    const interArea = ix * iy;
    if (interArea <= 0) continue;
    const nodeArea = Math.max(1, (r - l) * (b - t));
    const meaningful = !!(n.clickable || n.resource_id || n.text || n.content_desc || n.isLeaf);
    matches.push({
      node: n,
      nodeArea,
      interRatio: interArea / nodeArea,
      meaningful,
      isFullScreenContainer: !meaningful && (nodeArea / screenArea) > 0.85,
    });
  }
  if (!matches.length) return [];
  const filtered = matches.some((m) => !m.isFullScreenContainer)
    ? matches.filter((m) => !m.isFullScreenContainer)
    : matches;
  filtered.sort((a, b) => {
    if (a.meaningful !== b.meaningful) return a.meaningful ? -1 : 1;
    if (a.node.clickable !== b.node.clickable) return a.node.clickable ? -1 : 1;
    if (a.node.isLeaf !== b.node.isLeaf) return a.node.isLeaf ? -1 : 1;
    if (a.nodeArea !== b.nodeArea) return a.nodeArea - b.nodeArea;
    return b.node.depth - a.node.depth;
  });
  return filtered.slice(0, 16).map((m) => snapshotNode(m.node));
}

function displayRectToOverlay(bounds) {
  const b = parseBounds(bounds);
  if (!b) return null;
  const p1 = displayToOverlayPx({ x: b[0], y: b[1] });
  const p2 = displayToOverlayPx({ x: b[2], y: b[3] });
  if (!p1 || !p2) return null;
  return {
    left: Math.min(p1.x, p2.x),
    top: Math.min(p1.y, p2.y),
    width: Math.max(2, Math.abs(p2.x - p1.x)),
    height: Math.max(2, Math.abs(p2.y - p1.y)),
  };
}

function clearAnnotLayer() {
  if (!annotLayer) return;
  const children = Array.from(annotLayer.children);
  for (const ch of children) {
    if (ch !== annotSvg) ch.remove();
  }
  if (annotSvg) annotSvg.textContent = "";
}

function renderAnnotOverlay() {
  if (!annotLayer) return;
  clearAnnotLayer();
  if (!current || sessionEnded) return;

  const curAct = currentActivityName();
  // Draw persistent numbered pins for open annotations belonging to the current activity while annotating.
  if (annotMode) {
    for (const item of annotItems) {
      if (item.status !== "open") continue;
      if (item.activity && curAct && item.activity !== curAct) continue;
      const box = displayRectToOverlay(item.bounds);
      if (!box) continue;
      const pinBox = el("div", "annot-pin-box");
      pinBox.style.left = box.left.toFixed(1) + "px";
      pinBox.style.top = box.top.toFixed(1) + "px";
      pinBox.style.width = box.width.toFixed(1) + "px";
      pinBox.style.height = box.height.toFixed(1) + "px";
      annotLayer.appendChild(pinBox);

      const badge = el("div", "annot-pin-badge", "#" + item.id);
      badge.style.left = Math.max(10, box.left).toFixed(1) + "px";
      badge.style.top = Math.max(10, box.top).toFixed(1) + "px";
      annotLayer.appendChild(badge);
    }
  }

  // Hover inspector in Pick mode
  if (annotMode && annotTool === "pick" && !annotDraft && !annotPending && hoveredMirrorNode) {
    const box = displayRectToOverlay(hoveredMirrorNode.bounds);
    if (box) {
      const hbox = el("div", "annot-hover-box");
      hbox.style.left = box.left.toFixed(1) + "px";
      hbox.style.top = box.top.toFixed(1) + "px";
      hbox.style.width = box.width.toFixed(1) + "px";
      hbox.style.height = box.height.toFixed(1) + "px";
      annotLayer.appendChild(hbox);

      const lbl = el("div", "annot-hover-label", formatNodeSummary(hoveredMirrorNode));
      const lblTop = box.top >= 26 ? (box.top - 24) : (box.top + box.height + 4);
      lbl.style.left = Math.max(4, box.left).toFixed(1) + "px";
      lbl.style.top = Math.max(4, lblTop).toFixed(1) + "px";
      annotLayer.appendChild(lbl);
    }
  }

  // Live drag / draw draft
  if (annotDraft) {
    if (annotDraft.tool === "box" && annotDraft.bounds) {
      const box = displayRectToOverlay(annotDraft.bounds);
      if (box) {
        const dbox = el("div", "annot-draft-box");
        dbox.style.left = box.left.toFixed(1) + "px";
        dbox.style.top = box.top.toFixed(1) + "px";
        dbox.style.width = box.width.toFixed(1) + "px";
        dbox.style.height = box.height.toFixed(1) + "px";
        annotLayer.appendChild(dbox);
      }
    } else if (annotDraft.tool === "draw" && annotDraft.points.length > 1 && annotSvg) {
      const pts = [];
      for (const p of annotDraft.points) {
        const px = displayToOverlayPx(p);
        if (px) pts.push(px.x.toFixed(1) + "," + px.y.toFixed(1));
      }
      if (pts.length > 1) {
        annotSvg.appendChild(svgEl("polyline", {
          points: pts.join(" "),
          fill: "none",
          stroke: "#ff6b4a",
          "stroke-width": "3.5",
          "stroke-linecap": "round",
          "stroke-linejoin": "round",
        }));
      }
    }
  }

  // Pending selection awaiting popover comment
  if (annotPending) {
    const box = displayRectToOverlay(annotPending.bounds);
    if (box) {
      const dbox = el("div", "annot-draft-box");
      dbox.style.left = box.left.toFixed(1) + "px";
      dbox.style.top = box.top.toFixed(1) + "px";
      dbox.style.width = box.width.toFixed(1) + "px";
      dbox.style.height = box.height.toFixed(1) + "px";
      annotLayer.appendChild(dbox);
    }
    if (annotPending.strokePts && annotPending.strokePts.length > 1 && annotSvg) {
      const pts = [];
      for (const p of annotPending.strokePts) {
        const px = displayToOverlayPx(p);
        if (px) pts.push(px.x.toFixed(1) + "," + px.y.toFixed(1));
      }
      if (pts.length > 1) {
        annotSvg.appendChild(svgEl("polyline", {
          points: pts.join(" "),
          fill: "none",
          stroke: "#ff6b4a",
          "stroke-width": "3.5",
          "stroke-linecap": "round",
          "stroke-linejoin": "round",
        }));
      }
    }
  }
}

function nextAnnotationNumber() {
  let maxId = 0;
  for (const item of annotItems) {
    if (typeof item.id === "number" && item.id > maxId) maxId = item.id;
  }
  return maxId + 1;
}

function compositeAnnotationPng(selection, badgeNum) {
  if (!hasDecodedFrame() || !selection) return null;
  try {
    const off = document.createElement("canvas");
    off.width = frame.width;
    off.height = frame.height;
    const ctx = off.getContext && off.getContext("2d");
    if (!ctx || typeof off.toDataURL !== "function") return null;
    ctx.drawImage(frame, 0, 0);

    const { w: dw, h: dh } = currentDisplaySize();
    const sx = dw > 0 ? (off.width / dw) : 1;
    const sy = dh > 0 ? (off.height / dh) : 1;
    const lineW = Math.max(3, Math.round(off.width / 220));
    const b = parseBounds(selection.bounds);

    if (selection.kind === "freehand" && Array.isArray(selection.strokePts) && selection.strokePts.length > 1) {
      if (b) {
        ctx.save();
        ctx.strokeStyle = "rgba(255, 107, 74, 0.55)";
        ctx.lineWidth = Math.max(2, Math.round(lineW * 0.6));
        if (typeof ctx.setLineDash === "function") ctx.setLineDash([8, 6]);
        ctx.strokeRect(b[0] * sx, b[1] * sy, (b[2] - b[0]) * sx, (b[3] - b[1]) * sy);
        ctx.restore();
      }
      ctx.save();
      ctx.strokeStyle = "#ff6b4a";
      ctx.lineWidth = Math.max(4, Math.round(lineW * 1.4));
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      ctx.beginPath();
      for (let i = 0; i < selection.strokePts.length; i++) {
        const px = selection.strokePts[i].x * sx;
        const py = selection.strokePts[i].y * sy;
        if (i === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      }
      ctx.stroke();
      ctx.restore();
    } else if (b) {
      const rx = b[0] * sx;
      const ry = b[1] * sy;
      const rw = Math.max(4, (b[2] - b[0]) * sx);
      const rh = Math.max(4, (b[3] - b[1]) * sy);
      ctx.save();
      ctx.fillStyle = "rgba(255, 107, 74, 0.16)";
      ctx.fillRect(rx, ry, rw, rh);
      ctx.strokeStyle = "#ff6b4a";
      ctx.lineWidth = lineW;
      ctx.strokeRect(rx, ry, rw, rh);
      ctx.restore();
    }

    if (b) {
      const radius = Math.max(14, Math.round(off.width / 54));
      const bx = Math.max(radius + 4, Math.min(off.width - radius - 4, b[0] * sx));
      const by = Math.max(radius + 4, Math.min(off.height - radius - 4, b[1] * sy));
      ctx.save();
      ctx.fillStyle = "#ff6b4a";
      ctx.beginPath();
      ctx.arc(bx, by, radius, 0, Math.PI * 2);
      ctx.fill();
      ctx.fillStyle = "#0d1117";
      ctx.font = "bold " + Math.max(12, Math.round(radius * 0.95)) + "px monospace";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText("#" + badgeNum, bx, by + 1);
      ctx.restore();
    }

    const dataUrl = off.toDataURL("image/png");
    const prefix = "data:image/png;base64,";
    if (typeof dataUrl === "string" && dataUrl.startsWith(prefix)) {
      return dataUrl.slice(prefix.length);
    }
    return null;
  } catch (_) {
    return null;
  }
}

function openAnnotPopover(selection) {
  annotPending = selection;
  annotSubmitting = false;
  hoveredMirrorNode = null;
  const primary = selection.nodes && selection.nodes.length ? selection.nodes[0] : null;
  if (primary) {
    const extra = selection.nodes.length > 1 ? " (+" + (selection.nodes.length - 1) + " more)" : "";
    annotPopTarget.textContent = formatNodeSummary(primary) + extra;
  } else {
    annotPopTarget.textContent = selection.kind + " [" + selection.bounds.join(",") + "]";
  }
  annotComment.value = "";
  annotPopErr.textContent = "";
  annotPopErr.hidden = true;
  annotSubmit.disabled = false;
  annotPopover.hidden = false;

  const box = displayRectToOverlay(selection.bounds);
  const wrapRect = screenWrap.getBoundingClientRect();
  const popW = Math.min(295, Math.max(200, (wrapRect.width || 320) - 16));
  const popH = 155;
  let left = 8;
  let top = 8;
  if (box) {
    left = Math.max(8, Math.min((wrapRect.width || popW + 16) - popW - 8, box.left));
    if (box.top + box.height + popH + 12 <= (wrapRect.height || 400)) {
      top = box.top + box.height + 8;
    } else if (box.top - popH - 8 >= 8) {
      top = box.top - popH - 8;
    } else {
      top = Math.max(8, Math.min((wrapRect.height || popH + 16) - popH - 8, box.top + 8));
    }
  }
  annotPopover.style.left = left.toFixed(0) + "px";
  annotPopover.style.top = top.toFixed(0) + "px";
  renderAnnotOverlay();
  annotComment.focus();
}

function closeAnnotPopover() {
  annotPending = null;
  annotSubmitting = false;
  if (annotPopover) annotPopover.hidden = true;
  if (annotPopErr) { annotPopErr.textContent = ""; annotPopErr.hidden = true; }
  if (annotSubmit) annotSubmit.disabled = false;
  renderAnnotOverlay();
}

function annotationsApiPath(extra) {
  const sid = encodeURIComponent(current.session_id);
  const g = (typeof currentGuestGen === "number" && currentGuestGen > 0)
    ? ("g=" + encodeURIComponent(currentGuestGen))
    : "";
  const base = "/api/annotations/" + sid + (extra || "");
  if (!g) return auth(base);
  return auth(base + (base.includes("?") ? "&" : "?") + g);
}

function resetAnnotationsState() {
  annotGeneration += 1;
  annotMode = false;
  annotTool = "pick";
  annotItems = [];
  annotOpenCount = 0;
  annotDraft = null;
  hoveredMirrorNode = null;
  closeAnnotPopover();
  renderAnnotationsPanel();
  renderAnnotOverlay();
}

function setAnnotMode(enabled) {
  const next = !!enabled && !!current && !sessionEnded;
  if (next === annotMode) return;
  annotMode = next;
  if (annotMode) {
    cancelActivePointers();
    releaseHeldGuestKeys();
    pinchMode = false;
    scheduleTreeRefresh(50);
    fetchAnnotations();
  } else {
    annotDraft = null;
    hoveredMirrorNode = null;
    closeAnnotPopover();
  }
  applyToolbarUi();
  renderAnnotOverlay();
}

function setAnnotTool(tool) {
  if (tool !== "pick" && tool !== "box" && tool !== "draw") return;
  annotTool = tool;
  if (tool !== "pick") hoveredMirrorNode = null;
  applyToolbarUi();
  renderAnnotOverlay();
}

function setAsideTab(tab) {
  asideTab = tab === "annotations" ? "annotations" : "activity";
  const isAnnot = asideTab === "annotations";
  tabActivity.classList.toggle("active", !isAnnot);
  tabActivity.setAttribute("aria-selected", String(!isAnnot));
  tabAnnotations.classList.toggle("active", isAnnot);
  tabAnnotations.setAttribute("aria-selected", String(isAnnot));
  paneActivity.hidden = isAnnot;
  paneAnnotations.hidden = !isAnnot;
  if (isAnnot && current && !sessionEnded) {
    fetchAnnotations();
  }
}

async function fetchAnnotations() {
  if (!current || sessionEnded) return;
  const sid = current.session_id;
  const gen = annotGeneration;
  try {
    const resp = await fetch(annotationsApiPath("?status=all"), { cache: "no-store" });
    if (annotGeneration !== gen || !current || current.session_id !== sid || sessionEnded) return;
    if (resp.status === 409) {
      markSessionEnded(sid);
      return;
    }
    if (!resp.ok) return;
    const data = await resp.json();
    if (annotGeneration !== gen || !current || current.session_id !== sid || sessionEnded) return;
    if (!data || !data.ok) return;
    annotItems = Array.isArray(data.annotations) ? data.annotations : [];
    renderAnnotationsPanel();
    renderAnnotOverlay();
  } catch (_) {}
}

async function submitAnnotation() {
  if (!current || sessionEnded || !annotPending || annotSubmitting) return;
  const comment = String(annotComment.value || "").trim();
  if (!comment) {
    annotPopErr.textContent = "Please enter a comment.";
    annotPopErr.hidden = false;
    annotComment.focus();
    return;
  }
  annotSubmitting = true;
  annotSubmit.disabled = true;
  annotPopErr.hidden = true;
  const badgeNum = nextAnnotationNumber();
  const pngBase64 = compositeAnnotationPng(annotPending, badgeNum);
  const { w: dw, h: dh } = currentDisplaySize();
  const payload = {
    action: "add",
    comment,
    kind: annotPending.kind,
    bounds: annotPending.bounds,
    nodes: annotPending.nodes || [],
    display: annotPending.display || { w: dw, h: dh },
  };
  if (annotPending.activity) payload.activity = annotPending.activity;
  if (annotPending.package) payload.package = annotPending.package;
  if (pngBase64) payload.image_png_base64 = pngBase64;

  try {
    const resp = await fetch(annotationsApiPath(""), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    const data = await resp.json().catch(() => ({}));
    if (resp.status === 409) {
      markSessionEnded(current.session_id);
      return;
    }
    if (!resp.ok || !data.ok) {
      annotPopErr.textContent = (data && data.error) || ("Failed (HTTP " + resp.status + ")");
      annotPopErr.hidden = false;
      annotSubmitting = false;
      annotSubmit.disabled = false;
      return;
    }
    if (data.annotation) {
      annotItems.push(data.annotation);
    }
    closeAnnotPopover();
    setAsideTab("annotations");
    renderAnnotationsPanel();
    renderAnnotOverlay();
  } catch (err) {
    annotPopErr.textContent = err && err.message ? err.message : "Network error";
    annotPopErr.hidden = false;
    annotSubmitting = false;
    annotSubmit.disabled = false;
  }
}

async function resolveAnnotation(id) {
  if (!current || sessionEnded) return;
  try {
    const resp = await fetch(annotationsApiPath(""), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ action: "resolve", id }),
    });
    const data = await resp.json().catch(() => ({}));
    if (resp.ok && data.ok) {
      const idx = annotItems.findIndex((a) => a.id === id);
      if (idx >= 0 && data.annotation) annotItems[idx] = data.annotation;
      renderAnnotationsPanel();
      renderAnnotOverlay();
    }
  } catch (_) {}
}

async function deleteAnnotation(id) {
  if (!current || sessionEnded) return;
  try {
    const resp = await fetch(annotationsApiPath("/" + encodeURIComponent(id)), {
      method: "DELETE",
    });
    const data = await resp.json().catch(() => ({}));
    if (resp.ok && data.ok) {
      annotItems = annotItems.filter((a) => a.id !== id);
      renderAnnotationsPanel();
      renderAnnotOverlay();
    }
  } catch (_) {}
}

async function clearAnnotations() {
  if (!current || sessionEnded) return;
  try {
    const resp = await fetch(annotationsApiPath(""), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ action: "clear", status: "all" }),
    });
    const data = await resp.json().catch(() => ({}));
    if (resp.ok && data.ok) {
      annotItems = [];
      renderAnnotationsPanel();
      renderAnnotOverlay();
    }
  } catch (_) {}
}

function formatAnnotationBlock(item) {
  const lines = [];
  const act = item.activity || "unknown activity";
  const statusTag = item.status === "resolved" ? " (resolved)" : "";
  lines.push("### #" + item.id + " [" + (item.kind || "element") + "] — " + act + statusTag);
  const nodes = Array.isArray(item.nodes) ? item.nodes : [];
  if (nodes.length > 0) {
    lines.push("- **Element:** " + formatNodeSummary(nodes[0]));
    if (nodes.length > 1) {
      const extras = nodes.slice(1, 5).map((n) => formatNodeSummary(n)).join("; ");
      lines.push("- **Other intersecting views:** " + extras + (nodes.length > 5 ? " (+" + (nodes.length - 5) + " more)" : ""));
    }
  } else if (item.bounds) {
    lines.push("- **Bounds:** `[" + item.bounds.join(",") + "]`");
  }
  lines.push("- **Comment:** " + item.comment);
  if (item.screenshot_path) {
    lines.push("- **Screenshot:** `" + item.screenshot_path + "`");
  }
  return lines.join("\n");
}

function formatAnnotationsForAgent(items) {
  const list = Array.isArray(items) ? items : [];
  if (!list.length) return "";
  const sid = current ? current.session_id : "session";
  const pkg = (current && current.package) || mirrorMeta.package || "";
  const header = "Please address the following Android UI annotation" + (list.length === 1 ? "" : "s")
    + " from ACOS session `" + sid + "`" + (pkg ? " (" + pkg + ")" : "") + ":";
  const blocks = list.map(formatAnnotationBlock);
  const footer = "Use `acos_annotations` (`{\"action\": \"resolve\", \"id\": <id>}`) once each item is fixed.";
  return [header, "", blocks.join("\n\n"), "", footer].join("\n");
}

function renderAnnotationsPanel() {
  const openItems = annotItems.filter((a) => a.status === "open");
  annotOpenCount = openItems.length;
  if (tabAnnotations) {
    tabAnnotations.textContent = "Annotations (" + annotOpenCount + ")";
  }
  applyToolbarUi();
  if (btnAnnotCopyAll) btnAnnotCopyAll.disabled = openItems.length === 0;
  if (btnAnnotClear) btnAnnotClear.disabled = annotItems.length === 0;
  if (!annotList || !annotEmpty) return;

  const visible = annotShowResolvedFlag ? annotItems : openItems;
  annotList.textContent = "";
  annotEmpty.hidden = visible.length > 0;
  if (!visible.length) {
    annotEmpty.textContent = annotItems.length > 0
      ? "All annotations are resolved. Toggle Resolved to review them."
      : "No open annotations yet. Click Annotate to pick a view, draw a box, or sketch on the screen.";
    return;
  }

  for (const item of visible) {
    const li = el("li", "annot-item" + (item.status === "resolved" ? " resolved" : ""));
    const head = el("div", "annot-item-head");
    head.appendChild(el("span", "annot-num", "#" + item.id));
    head.appendChild(el("span", "annot-tag", item.kind || "element"));
    if (item.status === "resolved") {
      head.appendChild(el("span", "annot-tag ok", "resolved"));
    }
    const actName = item.activity ? shortClassName(item.activity) : "-";
    const actEl = el("span", "annot-act", actName);
    actEl.title = item.activity || "";
    head.appendChild(actEl);
    li.appendChild(head);

    const nodes = Array.isArray(item.nodes) ? item.nodes : [];
    const viewSummary = nodes.length > 0
      ? formatNodeSummary(nodes[0]) + (nodes.length > 1 ? " (+" + (nodes.length - 1) + " more)" : "")
      : (item.bounds ? "[" + item.bounds.join(",") + "]" : "");
    if (viewSummary) {
      li.appendChild(el("div", "annot-view", viewSummary));
    }

    li.appendChild(el("div", "annot-comment", item.comment));

    if (item.screenshot_path) {
      const shotEl = el("div", "annot-shot", "png: " + projectName(item.screenshot_path));
      shotEl.title = item.screenshot_path;
      li.appendChild(shotEl);
    }

    const actions = el("div", "annot-item-actions");
    const copyBtn = el("button", null, "Copy");
    copyBtn.type = "button";
    copyBtn.addEventListener("click", () => {
      copyTextToClipboard(formatAnnotationsForAgent([item]), copyBtn, "Copy", "Copied!");
    });
    actions.appendChild(copyBtn);

    if (item.status === "open") {
      const resBtn = el("button", null, "Resolve");
      resBtn.type = "button";
      resBtn.addEventListener("click", () => resolveAnnotation(item.id));
      actions.appendChild(resBtn);
    }

    const delBtn = el("button", "danger", "Delete");
    delBtn.type = "button";
    delBtn.addEventListener("click", () => deleteAnnotation(item.id));
    actions.appendChild(delBtn);

    li.appendChild(actions);
    annotList.appendChild(li);
  }
}

function handleAnnotPointerDown(e, pt) {
  if (annotPending) closeAnnotPopover();
  const { w: dw, h: dh } = currentDisplaySize();
  const act = currentActivityName();
  const pkg = mirrorMeta.package || (current && current.package) || "";

  if (annotTool === "pick") {
    const node = findDeepestNodeAt(pt);
    const bounds = node
      ? node.bounds
      : [
          Math.max(0, pt.x - 24),
          Math.max(0, pt.y - 24),
          Math.min(dw, pt.x + 24),
          Math.min(dh, pt.y + 24),
        ];
    openAnnotPopover({
      kind: "element",
      bounds,
      nodes: node ? [snapshotNode(node)] : [],
      activity: act,
      package: pkg,
      display: { w: dw, h: dh },
    });
    return;
  }

  try { screenWrap.setPointerCapture(e.pointerId); } catch (_) {}
  if (annotTool === "box") {
    annotDraft = {
      pointerId: e.pointerId,
      tool: "box",
      startPt: pt,
      curPt: pt,
      bounds: [pt.x, pt.y, Math.min(dw, pt.x + 1), Math.min(dh, pt.y + 1)],
    };
  } else {
    annotDraft = {
      pointerId: e.pointerId,
      tool: "draw",
      points: [pt],
      bounds: [pt.x, pt.y, Math.min(dw, pt.x + 1), Math.min(dh, pt.y + 1)],
    };
  }
  renderAnnotOverlay();
}

function handleAnnotPointerMove(e) {
  if (annotDraft && e.pointerId === annotDraft.pointerId) {
    const pt = clientToDisplay(e.clientX, e.clientY, true);
    if (!pt) return;
    if (annotDraft.tool === "box") {
      annotDraft.curPt = pt;
      annotDraft.bounds = [
        Math.min(annotDraft.startPt.x, pt.x),
        Math.min(annotDraft.startPt.y, pt.y),
        Math.max(annotDraft.startPt.x, pt.x),
        Math.max(annotDraft.startPt.y, pt.y),
      ];
    } else if (annotDraft.tool === "draw") {
      const last = annotDraft.points[annotDraft.points.length - 1];
      if (!last || Math.hypot(pt.x - last.x, pt.y - last.y) >= 3) {
        if (annotDraft.points.length < 512) annotDraft.points.push(pt);
      }
    }
    renderAnnotOverlay();
    return;
  }
  if (annotTool === "pick" && !annotPending) {
    const pt = clientToDisplay(e.clientX, e.clientY, false);
    const hit = pt ? findDeepestNodeAt(pt) : null;
    if (hit !== hoveredMirrorNode) {
      hoveredMirrorNode = hit;
      renderAnnotOverlay();
    }
  }
}

function finishAnnotPointer(e, cancelled) {
  if (!annotDraft || e.pointerId !== annotDraft.pointerId) return;
  const draft = annotDraft;
  annotDraft = null;
  try {
    if (screenWrap.hasPointerCapture && screenWrap.hasPointerCapture(e.pointerId)) {
      screenWrap.releasePointerCapture(e.pointerId);
    }
  } catch (_) {}
  if (cancelled) {
    renderAnnotOverlay();
    return;
  }
  const { w: dw, h: dh } = currentDisplaySize();
  const act = currentActivityName();
  const pkg = mirrorMeta.package || (current && current.package) || "";

  if (draft.tool === "box") {
    const endPt = clientToDisplay(e.clientX, e.clientY, true) || draft.curPt;
    const l = Math.min(draft.startPt.x, endPt.x);
    const t = Math.min(draft.startPt.y, endPt.y);
    const r = Math.max(draft.startPt.x, endPt.x);
    const b = Math.max(draft.startPt.y, endPt.y);
    if ((r - l) < 8 && (b - t) < 8) {
      // Tiny drag: fall back to picking deepest element at click point
      const node = findDeepestNodeAt(endPt);
      const bounds = node
        ? node.bounds
        : [Math.max(0, l - 20), Math.max(0, t - 20), Math.min(dw, r + 20), Math.min(dh, b + 20)];
      openAnnotPopover({
        kind: "element",
        bounds,
        nodes: node ? [snapshotNode(node)] : [],
        activity: act,
        package: pkg,
        display: { w: dw, h: dh },
      });
      return;
    }
    const bounds = [l, t, r, b];
    openAnnotPopover({
      kind: "box",
      bounds,
      nodes: findNodesIntersecting(bounds),
      activity: act,
      package: pkg,
      display: { w: dw, h: dh },
    });
  } else if (draft.tool === "draw") {
    const pts = draft.points;
    let minX = dw, minY = dh, maxX = 0, maxY = 0;
    for (const p of pts) {
      if (p.x < minX) minX = p.x;
      if (p.y < minY) minY = p.y;
      if (p.x > maxX) maxX = p.x;
      if (p.y > maxY) maxY = p.y;
    }
    if (maxX - minX < 12) { minX = Math.max(0, minX - 12); maxX = Math.min(dw, maxX + 12); }
    if (maxY - minY < 12) { minY = Math.max(0, minY - 12); maxY = Math.min(dh, maxY + 12); }
    const bounds = [Math.round(minX), Math.round(minY), Math.round(maxX), Math.round(maxY)];
    openAnnotPopover({
      kind: "freehand",
      bounds,
      nodes: findNodesIntersecting(bounds),
      strokePts: pts,
      activity: act,
      package: pkg,
      display: { w: dw, h: dh },
    });
  }
}

screenWrap.addEventListener("pointerdown", (e) => {
  if (!current || sessionEnded) return;
  if (isInsideElement(e.target, annotPopover)) return;
  if (e.pointerType === "mouse" && e.button !== 0) return;
  const pt = clientToDisplay(e.clientX, e.clientY, false);
  if (!pt) return;
  e.preventDefault();
  if (annotMode) {
    handleAnnotPointerDown(e, pt);
    return;
  }
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
  if (annotMode) {
    handleAnnotPointerMove(e);
    return;
  }
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

screenWrap.addEventListener("pointerleave", () => {
  if (hoveredMirrorNode) {
    hoveredMirrorNode = null;
    renderAnnotOverlay();
  }
});

function finishPointer(e, phase) {
  if (annotDraft && e.pointerId === annotDraft.pointerId) {
    finishAnnotPointer(e, phase === "cancel");
    return;
  }
  if (annotMode) return;
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
  if (!current || sessionEnded || annotMode) return;
  if (isInsideElement(e.target, annotPopover)) return;
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
  if (!current || detail.hidden || sessionEnded) return;
  if (!annotPopover.hidden) {
    if (e.key === "Escape") {
      e.preventDefault();
      closeAnnotPopover();
    }
    return;
  }
  if (isEditableElement(e.target) || isEditableElement(document.activeElement)) return;
  if (annotMode) {
    if (e.key === "Escape") {
      e.preventDefault();
      setAnnotMode(false);
    }
    return;
  }
  if (current.watch_only) return;
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
  if (!annotPopover.hidden || annotMode || isEditableElement(e.target) || isEditableElement(document.activeElement)) {
    return;
  }
  const kc = KEY_MAP[e.key];
  if (kc && heldGuestKeys.delete(kc)) {
    e.preventDefault();
    sendInput({ t: "key", code: kc, phase: "up" });
  }
});

document.addEventListener("paste", (e) => {
  if (!current || detail.hidden || current.watch_only || sessionEnded) return;
  if (!annotPopover.hidden || annotMode || isEditableElement(e.target) || isEditableElement(document.activeElement)) {
    return;
  }
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

btnAnnotate.addEventListener("click", () => {
  setAnnotMode(!annotMode);
});
btnAnnotPick.addEventListener("click", () => setAnnotTool("pick"));
btnAnnotBox.addEventListener("click", () => setAnnotTool("box"));
btnAnnotDraw.addEventListener("click", () => setAnnotTool("draw"));
btnAnnotCopy.addEventListener("click", () => {
  const openItems = annotItems.filter((a) => a.status === "open");
  if (!openItems.length) return;
  copyTextToClipboard(formatAnnotationsForAgent(openItems), btnAnnotCopy, "Copy for Agent", "Copied!");
});
annotSubmit.addEventListener("click", () => submitAnnotation());
annotCancel.addEventListener("click", () => closeAnnotPopover());
annotComment.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    submitAnnotation();
  } else if (e.key === "Escape") {
    e.preventDefault();
    closeAnnotPopover();
  }
});
tabActivity.addEventListener("click", () => setAsideTab("activity"));
tabAnnotations.addEventListener("click", () => setAsideTab("annotations"));
btnAnnotCopyAll.addEventListener("click", () => {
  const openItems = annotItems.filter((a) => a.status === "open");
  if (!openItems.length) return;
  copyTextToClipboard(formatAnnotationsForAgent(openItems), btnAnnotCopyAll, "Copy All for Agent", "Copied!");
});
btnAnnotClear.addEventListener("click", () => clearAnnotations());
annotShowResolved.addEventListener("change", () => {
  annotShowResolvedFlag = !!annotShowResolved.checked;
  renderAnnotationsPanel();
});

window.addEventListener("resize", () => {
  if (current && !detail.hidden) positionMirrorDom();
});
if (typeof ResizeObserver === "function" && screenWrap) {
  try {
    const ro = new ResizeObserver(() => {
      if (current && !detail.hidden) positionMirrorDom();
    });
    ro.observe(screenWrap);
  } catch (_) {}
}

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
  cancelMirrorFetch();
  closeWs();
  stopFrameStream();
  if (source) { source.close(); source = null; }
});
window.addEventListener("pageshow", (e) => {
  if (e.persisted && current && !sessionEnded && current.stream) {
    connectEvents(current);
    startFrameStream(current.session_id);
    scheduleTreeRefresh(200);
    fetchAnnotations();
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
"####;

#[cfg(test)]
mod tests {
    use super::PAGE;
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn extract_script() -> &'static str {
        let start_tag = "<script>";
        let end_tag = "</script>";
        let start = PAGE.find(start_tag).expect("PAGE should contain <script>") + start_tag.len();
        let end = PAGE[start..]
            .find(end_tag)
            .map(|i| start + i)
            .expect("PAGE should contain </script>");
        &PAGE[start..end]
    }

    fn run_page_js_scenario(test_body: &str) {
        let page_js = serde_json::to_string(extract_script()).expect("serialize page script");
        let runner = format!(
            r#"
const assert = require("node:assert/strict");
const vm = require("node:vm");

const pageJs = {page_js};

function createHarness() {{
  let nowMs = 1000000;
  let nextTimerId = 1;
  const timers = new Map();

  function fakeSetTimeout(fn, delay = 0, ...args) {{
    const id = nextTimerId++;
    timers.set(id, {{ id, fn, args, at: nowMs + Math.max(0, Number(delay) || 0), interval: null }});
    return id;
  }}
  function fakeClearTimeout(id) {{
    timers.delete(id);
  }}
  function fakeSetInterval(fn, delay = 0, ...args) {{
    const id = nextTimerId++;
    const step = Math.max(1, Number(delay) || 1);
    timers.set(id, {{ id, fn, args, at: nowMs + step, interval: step }});
    return id;
  }}
  function fakeClearInterval(id) {{
    timers.delete(id);
  }}
  function advanceTimers(ms) {{
    const target = nowMs + ms;
    while (true) {{
      let next = null;
      for (const t of timers.values()) {{
        if (t.at <= target && (!next || t.at < next.at || (t.at === next.at && t.id < next.id))) {{
          next = t;
        }}
      }}
      if (!next) break;
      nowMs = next.at;
      if (next.interval !== null) {{
        next.at = nowMs + next.interval;
      }} else {{
        timers.delete(next.id);
      }}
      if (typeof next.fn === "function") {{
        next.fn(...next.args);
      }}
    }}
    nowMs = target;
  }}

  class ClassList {{
    constructor() {{ this.set = new Set(); }}
    add(...names) {{ for (const n of names) this.set.add(n); }}
    remove(...names) {{ for (const n of names) this.set.delete(n); }}
    toggle(name, force) {{
      if (force === undefined) {{
        if (this.set.has(name)) {{ this.set.delete(name); return false; }}
        this.set.add(name); return true;
      }}
      if (force) this.set.add(name); else this.set.delete(name);
      return !!force;
    }}
    contains(name) {{ return this.set.has(name); }}
  }}

  class StubElement {{
    constructor(tagName, id = "") {{
      this.tagName = String(tagName || "div").toUpperCase();
      this.id = id;
      this.className = "";
      this.classList = new ClassList();
      this._textContent = "";
      this.hidden = false;
      this.disabled = false;
      this.title = "";
      this.type = "";
      this.value = "";
      this.width = 0;
      this.height = 0;
      this.tabIndex = 0;
      this.children = [];
      this.parentNode = null;
      this.listeners = new Map();
      this.attrs = new Map();
      this.style = {{ setProperty() {{}} }};
    }}
    get textContent() {{ return this._textContent; }}
    set textContent(v) {{
      this._textContent = String(v ?? "");
      if (this._textContent === "") {{
        for (const c of this.children) c.parentNode = null;
        this.children = [];
      }}
    }}
    get lastChild() {{
      return this.children.length ? this.children[this.children.length - 1] : null;
    }}
    appendChild(child) {{
      if (child.parentNode) child.remove();
      child.parentNode = this;
      this.children.push(child);
      return child;
    }}
    prepend(child) {{
      if (child.parentNode) child.remove();
      child.parentNode = this;
      this.children.unshift(child);
      return child;
    }}
    remove() {{
      if (!this.parentNode) return;
      const idx = this.parentNode.children.indexOf(this);
      if (idx >= 0) this.parentNode.children.splice(idx, 1);
      this.parentNode = null;
    }}
    addEventListener(type, fn) {{
      if (!this.listeners.has(type)) this.listeners.set(type, []);
      this.listeners.get(type).push(fn);
    }}
    dispatchEvent(ev) {{
      const list = this.listeners.get(ev.type) || [];
      for (const fn of list) fn(ev);
    }}
    click() {{
      this.dispatchEvent({{ type: "click", preventDefault() {{}} }});
    }}
    setAttribute(k, v) {{ this.attrs.set(k, String(v)); }}
    getContext() {{ return {{ clearRect() {{}}, drawImage() {{}} }}; }}
    focus() {{}}
    getBoundingClientRect() {{ return {{ left: 0, top: 0, width: 100, height: 200, right: 100, bottom: 200 }}; }}
    querySelector() {{ return null; }}
    hasPointerCapture() {{ return false; }}
    releasePointerCapture() {{}}
    setPointerCapture() {{}}
    toBlob(cb) {{ cb(null); }}
  }}

  const elementsById = new Map();
  const docListeners = new Map();
  const winListeners = new Map();
  const bodyEl = new StubElement("body");
  const documentStub = {{
    readyState: "complete",
    hidden: false,
    activeElement: null,
    body: bodyEl,
    getElementById(id) {{
      if (!elementsById.has(id)) elementsById.set(id, new StubElement("div", id));
      return elementsById.get(id);
    }},
    createElement(tag) {{ return new StubElement(tag); }},
    createElementNS(_ns, tag) {{ return new StubElement(tag); }},
    addEventListener(type, fn) {{
      if (!docListeners.has(type)) docListeners.set(type, []);
      docListeners.get(type).push(fn);
    }},
    execCommand() {{ return false; }},
  }};

  const createdSockets = [];
  class MockWebSocket {{
    constructor(url) {{
      this.url = url;
      this.readyState = MockWebSocket.OPEN;
      this.sent = [];
      this.closed = false;
      this.onopen = null;
      this.onmessage = null;
      this.onclose = null;
      this.onerror = null;
      createdSockets.push(this);
    }}
    send(data) {{ this.sent.push(data); }}
    close() {{
      if (this.readyState === MockWebSocket.CLOSED) return;
      this.readyState = MockWebSocket.CLOSED;
      this.closed = true;
      const sock = this;
      fakeSetTimeout(() => {{ if (sock.onclose) sock.onclose(); }}, 0);
    }}
    emitMessage(obj) {{
      if (this.onmessage) this.onmessage({{ data: JSON.stringify(obj) }});
    }}
  }}
  MockWebSocket.CONNECTING = 0;
  MockWebSocket.OPEN = 1;
  MockWebSocket.CLOSING = 2;
  MockWebSocket.CLOSED = 3;

  class MockEventSource {{
    constructor(url) {{
      this.url = url;
      this.closed = false;
    }}
    addEventListener() {{}}
    close() {{ this.closed = true; }}
  }}

  const windowStub = {{
    addEventListener(type, fn) {{
      if (!winListeners.has(type)) winListeners.set(type, []);
      winListeners.get(type).push(fn);
    }},
  }};

  const sandbox = {{
    console,
    URLSearchParams,
    AbortController,
    TextDecoder,
    Blob,
    URL,
    WebSocket: MockWebSocket,
    EventSource: MockEventSource,
    document: documentStub,
    window: windowStub,
    navigator: {{}},
    location: {{
      search: "?t=test-token",
      protocol: "http:",
      host: "127.0.0.1:8080",
      pathname: "/",
      hash: "",
    }},
    history: {{ replaceState() {{}} }},
    fetch: async () => ({{ ok: true, json: async () => ({{ sessions: [] }}) }}),
    setTimeout: fakeSetTimeout,
    clearTimeout: fakeClearTimeout,
    setInterval: fakeSetInterval,
    clearInterval: fakeClearInterval,
    requestAnimationFrame: (fn) => fakeSetTimeout(fn, 16),
  }};
  sandbox.globalThis = sandbox;

  const ctx = vm.createContext(sandbox);
  vm.runInContext(pageJs, ctx);

  function dispatchWindow(type, ev = {{}}) {{
    const list = winListeners.get(type) || [];
    for (const fn of list) fn(Object.assign({{ type }}, ev));
  }}

  return {{
    ctx,
    eval: (code) => vm.runInContext(code, ctx),
    advanceTimers,
    createdSockets,
    dispatchWindow,
    capTray: documentStub.getElementById("cap-tray"),
  }};
}}

{test_body}
"#,
            page_js = page_js,
            test_body = test_body,
        );

        let mut child = match Command::new("node")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipping page.rs JS test: `node` not found on PATH");
                return;
            }
            Err(err) => panic!("failed to spawn `node` for page.rs JS test: {err}"),
        };

        child
            .stdin
            .as_mut()
            .expect("stdin pipe")
            .write_all(runner.as_bytes())
            .expect("write runner script to node stdin");

        let output = child.wait_with_output().expect("wait for node");
        assert!(
            output.status.success(),
            "node JS test failed (status {:?})\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[test]
    fn queue_full_disconnect_fails_pending_capabilities_and_reenables_buttons() {
        run_page_js_scenario(
            r#"
const h = createHarness();
h.eval(`
  openSession({ session_id: "sess-1", guest_gen: 1, stream: false, watch_only: false });
  showCapabilityPrompt({
    seq: 7,
    title: "Location",
    options: [{ kind: "hot_attach", id: "opt-loc", label: "Enable location" }],
  });
`);
const rec = h.eval(`capabilityCards.get(7)`);
assert.equal(rec.buttons.length, 1);
rec.buttons[0].click();

assert.equal(h.eval(`pendingCapabilities.has(7)`), true);
assert.equal(rec.buttons[0].disabled, true);
assert.equal(rec.statusEl.textContent, "Enabling…");
assert.equal(h.createdSockets.length, 1);
const sock = h.createdSockets[0];
assert.equal(h.eval(`ws !== null`), true);

sock.emitMessage({ ok: false, code: "QUEUE_FULL", disconnected: true });
h.advanceTimers(10);

assert.equal(sock.closed, true);
assert.equal(h.eval(`ws`), null);
assert.equal(h.eval(`pendingCapabilities.size`), 0);
assert.equal(rec.buttons[0].disabled, false);
assert.equal(rec.statusEl.hidden, false);
assert.equal(rec.statusEl.className, "cap-status bad");
assert.match(rec.statusEl.textContent, /Connection lost: the result is uncertain/);
"#,
        );
    }

    #[test]
    fn pagehide_fails_pending_capabilities_so_bfcache_restore_does_not_stick_disabled() {
        run_page_js_scenario(
            r#"
const h = createHarness();
h.eval(`
  openSession({ session_id: "sess-2", guest_gen: 2, stream: true, watch_only: false });
  showCapabilityPrompt({
    seq: 3,
    title: "Camera",
    options: [{ kind: "hot_attach", id: "opt-cam", label: "Enable camera" }],
  });
`);
const rec = h.eval(`capabilityCards.get(3)`);
rec.buttons[0].click();

assert.equal(h.eval(`pendingCapabilities.has(3)`), true);
assert.equal(rec.buttons[0].disabled, true);
assert.equal(h.createdSockets.length, 1);
const sock = h.createdSockets[0];

h.dispatchWindow("pagehide", { persisted: true });
h.advanceTimers(10);
h.dispatchWindow("pageshow", { persisted: true });

assert.equal(sock.closed, true);
assert.equal(h.eval(`ws`), null);
assert.equal(h.eval(`pendingCapabilities.size`), 0);
assert.equal(rec.buttons[0].disabled, false);
assert.equal(rec.statusEl.hidden, false);
assert.equal(rec.statusEl.className, "cap-status bad");
assert.match(rec.statusEl.textContent, /Connection lost: the result is uncertain/);
"#,
        );
    }

    #[test]
    fn stale_success_dismiss_timer_does_not_dismiss_replacement_card_across_sessions() {
        run_page_js_scenario(
            r#"
const h = createHarness();
// Part 1: switch from session A to session B with same seq=1 before 750ms elapses.
h.eval(`
  openSession({ session_id: "sess-a", guest_gen: 1, stream: false, watch_only: false });
  showCapabilityPrompt({
    seq: 1,
    title: "Prompt A",
    options: [{ kind: "hot_attach", id: "opt-a", label: "Enable A" }],
  });
`);
const recA = h.eval(`capabilityCards.get(1)`);
recA.buttons[0].click();
h.eval(`handleCapabilityReply({ prompt_seq: 1, ok: true });`);
assert.equal(recA.statusEl.textContent, "Enabled.");

h.advanceTimers(200);
h.eval(`
  openSession({ session_id: "sess-b", guest_gen: 1, stream: false, watch_only: false });
  showCapabilityPrompt({
    seq: 1,
    title: "Prompt B",
    options: [{ kind: "hot_attach", id: "opt-b", label: "Enable B" }],
  });
`);
const recB = h.eval(`capabilityCards.get(1)`);
assert.notEqual(recB, recA);
assert.equal(h.capTray.children.includes(recB.card), true);

h.advanceTimers(600);
assert.equal(h.eval(`capabilityCards.get(1)`) === recB, true);
assert.equal(h.capTray.children.includes(recB.card), true);

// Part 2: without switching sessions, the 750ms timer still dismisses the original card.
h.eval(`
  openSession({ session_id: "sess-c", guest_gen: 1, stream: false, watch_only: false });
  showCapabilityPrompt({
    seq: 1,
    title: "Prompt C",
    options: [{ kind: "hot_attach", id: "opt-c", label: "Enable C" }],
  });
`);
const recC = h.eval(`capabilityCards.get(1)`);
h.eval(`handleCapabilityReply({ prompt_seq: 1, ok: true });`);
h.advanceTimers(749);
assert.equal(h.eval(`capabilityCards.get(1)`) === recC, true);
assert.equal(h.capTray.children.includes(recC.card), true);

h.advanceTimers(1);
assert.equal(h.eval(`capabilityCards.has(1)`), false);
assert.equal(h.capTray.children.includes(recC.card), false);
"#,
        );
    }

    #[test]
    fn mirror_dom_and_annotate_mode_js_behavior() {
        run_page_js_scenario(
            r#"
const h = createHarness();
h.eval(`
  openSession({
    session_id: "sess-annot",
    guest_gen: 1,
    stream: false,
    watch_only: false,
    package: "com.example.shop",
    activity: "com.example.shop/.LoginActivity",
    display: { w: 1000, h: 2000 },
  });
  frame.width = 1000;
  frame.height = 2000;
  frameSize = { w: 1000, h: 2000 };
  display = { w: 1000, h: 2000 };
  mirrorMeta = {
    activity: "com.example.shop/.LoginActivity",
    package: "com.example.shop",
    display: { w: 1000, h: 2000 },
  };
  mirrorNodes = flattenTreeNodes(
    {
      nodes: [
        {
          class: "android.widget.LinearLayout",
          bounds: "[0,0][1000,2000]",
          visible: true,
          clickable: false,
          children: [
            {
              class: "android.widget.Button",
              resource_id: "com.example.shop:id/btn_login",
              text: "Sign in",
              bounds: "[100,400][900,600]",
              visible: true,
              clickable: true,
              children: [],
            },
            {
              class: "android.widget.TextView",
              text: "Hidden",
              bounds: "[0,0][100,100]",
              visible: false,
              clickable: false,
              children: [],
            },
          ],
        },
      ],
    },
    "com.example.shop/.LoginActivity",
    "com.example.shop",
    { w: 1000, h: 2000 }
  );
  renderMirrorDom();
`);

// 1. Preorder Mirror DOM: LinearLayout parent before Button child, invisible node excluded.
assert.equal(h.eval(`mirrorDom.children.length`), 2);
assert.equal(h.eval(`mirrorDom.children[0].attrs.get("data-android-class")`), "android.widget.LinearLayout");
assert.equal(h.eval(`mirrorDom.children[1].attrs.get("data-acos-node")`), "1");
assert.equal(h.eval(`mirrorDom.children[1].attrs.get("data-android-class")`), "android.widget.Button");
assert.equal(h.eval(`mirrorDom.children[1].attrs.get("data-resource-id")`), "com.example.shop:id/btn_login");
assert.equal(h.eval(`mirrorDom.children[1].attrs.get("data-text")`), "Sign in");
assert.equal(h.eval(`mirrorDom.children[1].attrs.get("data-bounds")`), "[100,400,900,600]");
assert.equal(h.eval(`mirrorDom.children[1].attrs.get("data-clickable")`), "true");
assert.equal(
  h.eval(`mirrorDom.children[1].attrs.get("aria-label")`),
  'Button "Sign in" (id: btn_login) [100,400,900,600]'
);

// 2. Normal phone interaction still works when Annotate mode is OFF even if target is a mirror node.
h.eval(`
  screenWrap.dispatchEvent({
    type: "pointerdown",
    pointerType: "mouse",
    button: 0,
    pointerId: 1,
    clientX: 50,
    clientY: 50,
    target: mirrorDom.children[1],
    preventDefault() {},
  });
`);
assert.equal(h.createdSockets.length, 1);
assert.equal(h.createdSockets[0].sent.length, 1);
assert.equal(JSON.parse(h.createdSockets[0].sent[0]).t, "touch");
assert.equal(JSON.parse(h.createdSockets[0].sent[0]).phase, "down");

// 3. Turning Annotate mode ON cancels active pointers and pauses guest touch forwarding.
h.eval(`setAnnotMode(true);`);
const sentBeforeAnnot = h.createdSockets[0].sent.length;
h.eval(`
  screenWrap.dispatchEvent({
    type: "pointerdown",
    pointerType: "mouse",
    button: 0,
    pointerId: 2,
    clientX: 50,
    clientY: 50,
    target: mirrorDom.children[1],
    preventDefault() {},
  });
`);
assert.equal(h.createdSockets[0].sent.length, sentBeforeAnnot, "annotating must not send guest touches");
assert.equal(h.eval(`annotPopover.hidden`), false);
assert.equal(h.eval(`annotPending.kind`), "element");
assert.equal(h.eval(`annotPending.nodes.length`), 1);
assert.equal(h.eval(`annotPending.nodes[0].resource_id`), "com.example.shop:id/btn_login");

// 4. Copy for Agent formatting includes id, activity, element metadata, comment, and screenshot path.
const md = h.eval(`
  formatAnnotationsForAgent([
    {
      id: 1,
      kind: "element",
      status: "open",
      activity: "com.example.shop/.LoginActivity",
      bounds: [100, 400, 900, 600],
      nodes: [{ class: "android.widget.Button", resource_id: "com.example.shop:id/btn_login", text: "Sign in", bounds: [100, 400, 900, 600] }],
      comment: "Increase button height to 48dp",
      screenshot_path: "/tmp/acos/annotations/1.png",
    },
  ])
`);
assert.match(md, /### #1 \[element\] — com\.example\.shop\/\.LoginActivity/);
assert.match(md, /Button · #btn_login · "Sign in" · \[100,400,900,600\]/);
assert.match(md, /Increase button height to 48dp/);
assert.match(md, /\/tmp\/acos\/annotations\/1\.png/);
"#,
        );
    }
}
