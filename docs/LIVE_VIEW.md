# Live view — protocol v1 (producer side)

Status: **implemented** (producer: track VIEW1a; browser hub: track VIEW1b; interactive input: track HUMANVIEW1; owner decisions ADR-0027 + ADR-0033). §1–§7 are the producer contract; §8 is the hub,
`acos view`. Nothing here changes an agent-facing reply, the settle rules or a
tracked benchmark metric.

A live ART session can stream its screen and an action/event feed to local
viewers. The stream exists only while a viewer is connected: with no subscriber
the session's frame path does **one atomic load** and nothing else.

* Producer: `acos-art` (the launcher owns the C1 compositor state).
* Socket: one Unix `SOCK_STREAM` per session, bound in its **own host-only
  directory** (`/run/user/<uid>/acos-view-<pid>-<seq>/view.sock`, `sun_path`
  ≤ 107 bytes, directory 0700 and owned by this uid, socket 0600). **It is not in
  the session's socket directory** (`acos-art-<pid>-<seq>`, which is bind-mounted
  into the guest as `/dev/socket`): the app under test and a shared zygote cannot
  see, connect to or replace it (VIEW1c M1). The producer refuses a directory it
  does not own or cannot make 0700, never `chmod`s by path (the mode is set with
  `fchmodat(AT_SYMLINK_NOFOLLOW)` on the directory fd), and serves a subscribe
  line only after `SO_PEERCRED` shows the peer is the same uid.
* Path published in the launcher's ready line as **`stream_socket`** (a string,
  or `null` when the socket could not be bound — a session without a stream is
  still a normal session).
* Registry: `acos serve` publishes one small JSON file per ready session, see
  §5. A CLI-started session (no `acos serve`) has a stream socket but no
  registry entry; the ready line and the `acos serve` session list still name it.

## 1. Client → server

One JSON line, sent immediately after connecting, then nothing else on
`view.sock`. The frame/event socket itself stays unidirectional (`H`/`F`/`E`
out); interactive human input from `acos view` is routed by the hub through the
session's `serve.sock` `InputDispatch` path (`panel_*`, ADR-0033), never injected
over `view.sock`.

```json
{"v":1,"subscribe":{"frames":true,"events":true,"max_fps":15,"scale":0.5,"format":"png"}}
```

| field | meaning |
|---|---|
| `v` | protocol version, must be `1` |
| `subscribe.frames` | deliver `F` messages (default `true`) |
| `subscribe.events` | deliver `E` messages (default `true`) |
| `subscribe.max_fps` | 1..30 (default 15) |
| `subscribe.scale` | 0.05..1.0 (default 0.5 → 540×1200 for a 1080×2400 session) |
| `subscribe.format` | `"png"` (default, measured cheapest) or `"jpeg"` |

**One encoder per session.** The *first* subscriber fixes `format`, `scale` and
`max_fps`; the hello says which values are active, and a later subscriber's
request for different ones is ignored (it still gets `frames`/`events` as it
asked). This keeps one encode per frame no matter how many viewers watch. The
environment variables `ACOS_VIEW_FORMAT`, `ACOS_VIEW_SCALE`, and
`ACOS_VIEW_MAX_FPS` set the session's default parameters when a subscriber omits
those fields, while explicit fields in the first subscriber's `subscribe`
request override them (`ACOS_VIEW_JPEG_QUALITY` configures JPEG quality when
JPEG encoding is active).

An unknown `v` is answered with one `E` message carrying `{"v":1,"error":…}` and
the connection is closed.

## 2. Server → client framing

Every message is:

```
+----------------+--------+---------------------------+
| u32 BE length  | u8 type| payload (length-1 bytes)  |
+----------------+--------+---------------------------+
```

`length` counts the type byte **plus** the payload (so it is always ≥ 1).
A consumer must refuse `length == 0` and should refuse `length > 64 MiB`.

### Type `H` — hello (once, first message)

JSON object:

```json
{"v":1,"session":12345,"display":{"w":1080,"h":2400},"format":"png","scale":0.5,"has_home":true}
```

| field | meaning |
|---|---|
| `v` | protocol version (1) |
| `session` | the launcher (`acos-art`) pid |
| `display.w`,`display.h` | the **session display** size in pixels (frames are `scale` times this) |
| `format` | the active image format |
| `scale` | the active downscale factor |
| `has_home` | optional boolean: `true` when a `CATEGORY_HOME` activity is installed, `false` when none is installed, omitted when unknown |

### Type `F` — frame

Payload = a JSON header **line terminated by `\n`**, immediately followed by the
encoded image bytes (no length prefix of their own: the rest of the payload).

```json
{"seq":42,"generation":41,"w":540,"h":1200,"format":"png","ts_ms":1763158000123,"secure":false}
```

| field | meaning |
|---|---|
| `seq` | the C1 store counter (monotonic; advances for **every** buffer the guest posts) |
| `generation` | the C1 layer generation of the captured buffer |
| `w`,`h` | the encoded image size (`display * scale`, rounded) |
| `format` | `"jpeg"` or `"png"` |
| `ts_ms` | host wall clock at capture (ms since the Unix epoch) |
| `secure` | `true` when a visible window carries `FLAG_SECURE`: the image is then an **all-black placeholder** (1/8 of the display), never the app's pixels. The producer blanks it even if the source hands over content |

**`FLAG_SECURE`** is the window manager's flag (`LayoutParams.flags`); the host
`WmsState` publishes it to the compositor on every `relayout`, so the stream
knows it. While any non-hidden window has it the stream shows the placeholder,
like Android's own screenshot/screen-record blanking. A window the guest
positions itself (no WMS placement) can neither set nor hide it.

**Agent screenshots ignore `FLAG_SECURE`.** `observe` screenshots (`capture_c1`,
the agent's own pixel capture) do **not** honour `FLAG_SECURE`: an agent that asks
for a screenshot gets the real pixels of a secure window. Only the live stream
blanks secure windows. The stream is the surface that leaves the owner's
control, which is why the blanking lives there.

Frames are **not** acknowledged, ordered relative to events beyond `seq`/`ts_ms`,
or re-sent. Intermediate frames are dropped: the producer always encodes the
newest available frame, at most `max_fps` times per second. A viewer that cannot
keep up is disconnected (see §4).

### Type `E` — event

JSON object, one per line of the feed:

```json
{"ts_ms":1763158000123,"kind":"action","cmd":"tap","x":540,"y":1200,"status":"ok","frame_seq":7}
```

| field | meaning |
|---|---|
| `ts_ms` | host wall clock (ms since the Unix epoch) |
| `kind` | `"action"` \| `"activity"` \| `"lifecycle"` \| `"settle"` |
| `source` | `"agent"` (agent tool / CLI / MCP) or `"panel"` (human input via `acos view` `/api/input`), when emitted by an input or action event |
| `cmd` | for `action`/`lifecycle`: `tap` \| `touch` \| `scroll` \| `key` \| `text` \| `swipe` \| `rotate` \| `tree` \| `screenshot` \| `reset` \| `close` \| `launch` \| `session` |
| `x`,`y`,`x2`,`y2` | input coordinates (display space) when the command had them |
| `key` | key code for `key`: **masked** (`"[masked]"`) exactly when `text` is (a key code is a keystroke, and `KEYCODE_A`…`KEYCODE_Z` spell a secret) |
| `text` | typed text: **masked** (`"[masked]"`) by default; shown (truncated to 64 characters + `…`) only when the reply of that very command carries a fresh tree that proves a **focused, non-password** field. A tree with no focused node, a password field, no tree at all, or a tree from an earlier command all mask |
| `status` | `"ok"` or the launcher's structured code (`ELEMENT_OFFSCREEN`, `NO_FRESH_FRAME`, `VERIFY_WATCH_ONLY`, …) |
| `activity` | focused activity (`<package>/<Class>`) on `activity`/`lifecycle` events |
| `frame_seq` | the launcher's logical frame id of the reply this event describes, when it has one |
| `settle` | present only when the reply's observation was **not settled** (a timeout or an animating screen): the launcher's settle object, verbatim |

Event kinds:

* `action` — every command the launcher executed (`tap`, `touch`, `scroll`,
  `key`, `text`, `tree`, `screenshot`, `swipe`, `rotate`), with its `source`
  (`"agent"` or `"panel"`) and result `status`. Continuous `touch` `move`
  events are intentionally omitted from the feed (only `down`/`up`/`cancel`
  edges are recorded). Emitted for failures (except `panel_touch` error replies
  that omit an `action` field, such as `INPUT_UNAVAILABLE`) and for valid
  verify-run `VERIFY_WATCH_ONLY` refusals that reach the watch-only check on
  `touch down`, `scroll`, `key down`/`press`, `text`, or `rotate` (not
  `touch move`/`up`, `key up`, malformed JSON, or queue-admission rejections).
* `activity` — the focused activity changed (once per distinct value).
* `lifecycle` — `launch` (the ready line, `status: "ok"` or the failure code),
  `reset`, `close`, `session` (the session ended by itself: guest exit, timeout,
  host memory limit).
* `settle` — the reply carried an unsettled observation. Attached to the
  `action`/`lifecycle` event it belongs to (not a separate message), so a viewer
  can grey out a frame it must not treat as settled.

Events go through a bounded queue (64); when a viewer is slower than the feed,
the **oldest** events are dropped and the count is reported in the producer's
stats, never a block.

## 3. Producer cost model (hard rules)

1. **No subscriber** → the tap thread does not exist. `LiveView` starts it with
   the first subscriber and stops it with the last, so an unviewed session runs
   exactly the threads it ran before the viewer existed (no extra condvar waiter,
   no timer, no atomic load on the command path). Events are discarded before the
   queue, and the writer thread blocks on a condvar until someone attaches.
   The one cheap check that remains is inside the tap step itself: one atomic
   load of `subscriber_count` and a return (test seam: `source_calls == 0`,
   `encodes == 0`).
2. **With a subscriber** → on a frame whose `seq` advanced, the launcher
   composites once (`capture_composite`, the same pixels a screenshot would use),
   box-filters it to `scale` (area average; 2×2 per output pixel at 0.5) and
   stores it in a single "latest" slot under a short lock. The compositor is
   never asked to `present`/`present_forced`; `frame_seq`, `generation`, damage
   and the settle state are untouched.
   The capture is itself rate-limited to `max_fps`: a frame captured more often
   than the stream can send it would only be dropped by the encoder, and a
   capture is the expensive step (a full 1080×2400 composite).
3. One encoder thread per session: latest wins, `max_fps` rate limit, one encode
   shared by every subscriber, non-blocking sends. A subscriber whose socket
   would block (or that closed) is dropped; the encoder never waits for it.
4. At most 4 subscribers; a fifth connection drops the oldest, so a viewer that
   reconnects is never locked out.
5. Text and key codes in the feed are masked unless a fresh tree proves a
   non-password focus, and truncated; watch-only subscribers default to `0.5`
   scale while interactive `acos view` sessions upgrade to `1.0` scale only
   while `/api/input` is open.
6. At most `MAX_SUBSCRIBE_THREADS` (8) connections are in the subscribe phase at
   once (a connection beyond the cap is closed at accept, with no thread), and
   the whole subscribe line must arrive within one 5 s deadline: a peer that
   trickles bytes cannot hold a slot.
7. A message larger than the socket buffer (a large frame) is queued per
   subscriber with a write offset and delivered across several non-blocking
   writes; a viewer is dropped only on a write error or when more than 8 MiB is
   queued for it, so a slow-but-alive reader is not disconnected by a big frame.

**Password echo caveat.** Masking covers the *event feed* and the *frames of a
`FLAG_SECURE` window*. A password field that does not set `FLAG_SECURE` is drawn
by the framework as bullets (and, like Android, the *last typed character* may
be echoed in clear for about a second while typing). That glyph is in the
pixels, and the stream is those pixels; only `FLAG_SECURE` removes it. Apps that
handle credentials should set `FLAG_SECURE` on those screens (it also hides them
from the stream), and `acos view` is for the owner's own sessions on a loopback
port with a token, not for sessions that carry real credentials.

## 4. Consumer rules

* Connect, send the subscribe line, then read messages in a loop. A closed
  socket, a short read or a message that breaks framing means **reconnect** (the
  session may still be live; the hub re-reads the registry).
* Treat the stream as a lossy preview: it is not evidence, it is not a capture,
  and `frame_seq` here is the C1 store counter, not the agent-facing logical
  `frame_seq` of `acos_observe`.
* Decode `F` payloads with the image decoder matching `format`; `w`/`h` are the
  encoded size.
* Do not send anything after the subscribe line.

## 5. Registry (`acos serve` side)

The daemon writes one file per ready session:

```
$XDG_RUNTIME_DIR/acos/live/<session_id>-<daemon_pid>.json      (0700 dir, 0600 file)
```

written atomically (tmp file + `rename`), updated when the focused activity
changes, and removed on destroy/close/daemon exit. Document:

```json
{
  "v": 1,
  "session_id": "mcp-1234",
  "daemon_pid": 5678,
  "launcher_pid": 5701,
  "started_ms": 1763158000123,
  "client": {"name": "t3-code", "version": "1.2.3"},
  "project_dir": "/home/Qui/Projects/gaten",
  "apk": "/home/Qui/Projects/gaten/android/app/build/outputs/apk/debug/app-debug.apk",
  "package": "com.gaten",
  "app_label": "Gaten",
  "device_name": "Gaten Checkout",
  "watch_only": false,
  "serve_socket": "/run/user/1000/acos/mcp-5678/session.sock",
  "activity": "com.gaten/.MainActivity",
  "display": {"w": 1080, "h": 2400},
  "stream_socket": "/run/user/1000/acos-view-5701-0/view.sock",
  "has_home": true
}
```

* `client`/`project_dir` come from the environment the MCP server passes to its
  private daemon: `ACOS_VIEW_CLIENT` (the MCP `initialize` `clientInfo`, as
  `{"name":…,"version":…}`) and `ACOS_VIEW_PROJECT` (the MCP server's working
  directory). A CLI-started daemon publishes `"client": {"name": "acos-cli",
  "version": ""}` and its own working directory.
* `device_name` comes from `acos_launch {device_name}` or
  `acos launch --device-name`, sanitized (control chars/newlines/`=` stripped,
  max 64 chars). When absent, the page falls back to `<app_label or package> · <session_id>`.
* `watch_only` is `true` for `acos verify` runs (`serve_socket: null`); normal
  sessions publish `watch_only: false` and `serve_socket` so the hub can route
  panel input.
* `ACOS_VIEW_REGISTRY_DIR` overrides the directory (tests and tools only).
* **Stale entries**: a reader must treat an entry whose `daemon_pid` is not
  alive (`kill(pid, 0)` → `ESRCH`) as stale and delete it — a SIGKILLed daemon
  cannot remove its own files. Pid reuse is possible, so the real liveness test
  is connecting to `stream_socket`; a hub should do both. `acos-cli` exposes the
  helper (`live_registry::is_stale` / `sweep`) and the hub sweeps on each poll.
* `stream_socket` is `null` when the launcher could not bind one; such an entry
  is metadata only.
* `has_home` is an optional boolean (`true`, `false`, or `null` when unknown)
  reporting whether the session has an installed `CATEGORY_HOME` activity.
  `acos view` propagates it into `/api/sessions` and SSE `state` (`entry.has_home`
  alongside `hello.has_home`) so the page disables the `Home` toolbar button
  when `has_home === false`.

## 6. Reference consumer

`tools/view/stream_probe.py` (stdlib only) subscribes, saves frames and events to
a directory and prints a JSON summary:

```bash
tools/view/stream_probe.py --socket "$SOCK" --out .cache/view1-probe/run1 \
    --frames 4 --seconds 20 --save-frames
```

`--idle-exit S` makes it end by itself S seconds after the last message, which is
how a probe watches a live session without a kill.

## 7. Measurements and the default format

Measured 2026-11-14 on this host (60 GiB, release build) from the producer's own
encoder clock during a real streamed session at scale 0.5 (1080×2400 → 540×1200,
`ACOS_VIEW_STATS_FILE`, one encode per frame, JPEG quality 75):

| screen | format | mean bytes/frame | mean encode ms | frames | note |
|---|---|---|---|---|---|
| `counter.apk` (flat UI + text) | **png** | 20,042 | **1.23** | 29 | lossless, text exactly the framework's pixels |
| `counter.apk` | jpeg q75 | 22,657 | 11.57 | 26 | ringing around text, still readable |
| Gaten (RN, images/gradients) | **png** | 53,727 | **1.12** | 52 | 15 KB/frame larger than jpeg |
| Gaten | jpeg q75 | 38,337 | 9.44 | 46 | smaller on bytes, ~8× the CPU |

**Default: PNG.** It is 8-9× cheaper in CPU, which is the scarce resource — the
encoder thread shares the host with the real ART guest, so its cost must stay
away from the tracked M4–M8 metrics — and it is lossless, so half-resolution UI
text is readable by construction. JPEG stays selectable (per subscriber, or
`ACOS_VIEW_FORMAT=jpeg`) and wins on bytes for image-heavy screens. Both are far
below the 15 fps budget (66 ms): a frame costs ≤ 12 ms of a separate thread.
At full `1.0` scale (1080×2400) for interactive `acos view` (HUMANVIEW1), fast
adaptive PNG stays ~10× faster to encode than scalar JPEG (**2.08 ms** mean
encode, ~56 KB/frame on `counter.apk`) and delivers **p50 = 66.70 ms, p95 = 68.90 ms**
input-to-changed-frame latency at 30 fps.

Readability at 0.5 is checked by eye on the saved frames (`counter`: "Counter",
"1", "Increment", "Reset" all legible in both formats).

Viewer-on cost, measured the same way (3 interleaved bench reps each, a
`stream_probe` subscribed to every session the run published — 13-18 sessions,
301-313 frames received per rep):

| metric | viewer off | viewer on | delta |
|---|---|---|---|
| `cpu_active_tap_strict_pct` | 46.5 % | 58.1 % | **+11.7 pp of one core** |
| M4 warm launch (fast) | 163.5 ms | 169.8 ms | +3.9 % |
| M4 warm launch (strict) | 306.3 ms | 306.2 ms | −0.0 % |
| M5 screenshot | 24.20 ms | 24.20 ms | +0.0 % |
| M6 reset (fast / strict) | 198.2 / 339.5 ms | 199.1 / 347.3 ms | +0.4 % / +2.3 % |
| M7 tree (fast / strict) | 0.59 / 41.33 ms | 0.53 / 41.25 ms | −10 % (−0.06 ms) / −0.2 % |
| M8 tap counter (fast / strict) | 48.1 / 955.0 ms | 47.6 / 954.3 ms | −1.0 % / −0.1 % |
| M8 tap form (fast / strict) | 44.2 / 948.1 ms | 44.6 / 948.6 ms | +0.8 % / +0.1 % |

So a watching viewer costs ~12 % of one core on this host (capture ≈ 4-6 ms +
downscale ≈ 2 ms + encode ≈ 1.2 ms per frame at up to 15 fps) and leaves every
tracked latency metric inside noise. **Viewer-off is at parity** (see
`results/handoff/VIEW1a.md` for the interleaved A/B table).

A subscriber only receives events that happen **while it is connected**; the
current activity is in the registry entry and the `H` hello names the display
and the active format/scale. See `results/handoff/VIEW1a.md` for the viewer-on
benchmark table and the viewer-off A/B.

## 8. The browser hub (`acos view`, VIEW1b + HUMANVIEW1)

`acos view` is the consumer the owner uses: one process that serves a single
self-contained page, proxies the sessions' streams to it, and routes interactive
human input back to non-verify sessions (ADR-0033 amending ADR-0027 §5). It is
the **only** stream subscriber a session has while someone watches, and it holds
no session open on its own.

```bash
acos view                 # prints one line, then serves until it is stopped
acos view --json          # same, as one JSON record {url, bind, port, token}
acos view --once --json   # list the live sessions and exit (tests, agents)
```

```
acos view: http://127.0.0.1:54321/?t=<token>
```

* **127.0.0.1 only.** `--bind` accepts exactly `127.0.0.1`; any other address is
  refused before a socket exists (ADR-0027 §1). `--port 0` (the default) picks a
  free port and prints it.
* **Host check, connection cap, head deadline (VIEW1c).** A request whose
  `Host` header is not exactly `127.0.0.1:<port>` (this hub's own port) gets
  `403` before any route, which stops DNS-rebinding pages. At most 64
  connections are served at once (a connection beyond that is closed at accept)
  and the request head must arrive within one 5 s deadline. The hub verifies
  the upstream socket's server uid with `SO_PEERCRED` before it reads a frame
  from it, restarts the upstream if a browser attaches while the last one is
  detaching, and only reads a registry directory that this uid owns and that is
  not group/world-writable; the registry directory is created 0700 and an
  existing one that is another uid's, not a directory or a symlink is refused.
* **Token on every request.** A fresh 32-byte token (256 bits, hex) is read from
  `/dev/urandom` at startup and compared in constant time; `/?t=` missing or
  wrong is `HTTP 403` on the page, the JSON API, the MJPEG/SSE streams, and the
  `/api/input` WebSocket. Only `GET` is answered (`405` otherwise). The token is
  printed once and never logged again.
* **Browsing.** The page polls `/api/sessions` every 2 s: a grid of live
  sessions from the registry (title from `device_name` or
  `<app_label or package> · <session_id>`, agent/client, project dir,
  APK/package, current activity, age, display, watchers, `watch_only`, `has_home`) with
  stale entries swept and hidden. Clicking one opens it.
* **Watching one session.** The page opens two streams for the clicked session:
  `/api/frames/<session_id>` (one `multipart/x-mixed-replace` MJPEG response of
  the producer's PNG/JPEG frames, with `X-Frame-Seq`/`X-Frame-Size` and only the
  newest frame, so a slow browser skips frames) and `/api/events/<session_id>`
  (SSE: one `state` event — registry entry + `hello` (both carrying optional `has_home`) + `connected` — then one `ev` per `E`
  message, a `lag` event when the bounded ring dropped history, and a
  keep-alive). Overlays are drawn client-side from those events: a tap ripple at
  the event's `x/y` (display space, scaled to the frame), the current activity,
  and the recent actions (`agent` / `panel` source pill, tool, masked text,
  status). The page is ~40 KB of self-contained inline HTML/CSS/JS: no CDN, no
  external request, nothing served from disk, and no path parameter is used as a
  filesystem path (a session id is only matched against the registry).
* **Interactive input (`/api/input/<session_id>?t=<token>&g=<guest_gen>`, ADR-0033).**
  - Opened **lazily** on the first human pointer, wheel, keyboard, or toolbar
    (`Back`, `Home`, `Recents`, `Rotate`) interaction over an RFC 6455
    (`Sec-WebSocket-Version: 13`) WebSocket with the same `Host`, constant-time
    token, guest-generation (`g`, rejecting stale generations with `409`), and
    64-connection cap checks.
  - Messages are JSON in display coordinates:
    - `{"t":"touch","phase":"down|move|up|cancel","id":0,"x":540,"y":1200}`
    - `{"t":"scroll","x":540,"y":1200,"dx":0,"dy":450}`
    - `{"t":"key","code":"KEYCODE_BACK","phase":"down|up|press"}`
    - `{"t":"text","s":"hello"}`
    - `{"t":"rotate","rotation":0|1|2|3}`
  - **Ordering & safety.** One ordered worker per session routes each message
    over the session's `serve_socket` via the existing `InputDispatch` path
    (`panel_touch`, `panel_key`, `panel_text`, `panel_rotate`), tagging every
    emitted event `source: "panel"` in `/api/events`. On a mid-gesture
    disconnect, the worker attempts cleanup for the departing client's active
    pointers—sending indexed `ACTION_POINTER_UP` frames when other clients
    still hold active pointers rather than canceling the whole gesture, or
    `ACTION_CANCEL` when no other client has active pointers. Ambiguous
    touch-down failures (including post-write I/O errors and `TIMEOUT`
    replies) trigger a compensating cancel without replaying the down; failed
    initial or blocked `up`/`cancel`/disconnect releases are retained and only
    removed once confirmed (or once the guest is replaced/exits), and any
    pending recovery or release is retried with bounded backoff both while
    browsers remain connected and after the last client disconnects (stopping
    on confirmation, guest replacement/exit, or retry exhaustion, leaving
    recovery pending for the next gesture if the daemon stayed unreachable—so
    a disconnect does not by itself prove the guest released the pointer).
    `ArtSession` stamps monotonic `req_id`s echoed by `acos-art` so late
    replies from timed-out commands are discarded rather than mistaken for
    cancel confirmations, rejects out-of-order `panel_touch` frames whose
    `event_time` precedes an already-processed `panel_touch`, and `acos-art`
    preserves the pinned target window across transient `touch_frame` errors
    so compensating releases still hit the original window.
  - **Dynamic stream upgrade.** Watch-only stays `0.5` PNG @ `15` fps. While an
    input WebSocket is open, the hub upgrades the upstream subscription to `1.0`
    PNG @ `30` fps and reverts back to `0.5` PNG @ `15` fps when the last input
    socket closes.
* **Verify runs stay watch-only.** For sessions owned by `acos verify`
  (`watch_only: true`), the page shows a `watch-only: verify run` banner with
  disabled toolbar buttons, and `/api/input` refuses every valid message that
  reaches the watch-only check with `{"ok":false,"code":"VERIFY_WATCH_ONLY"}`
  (malformed JSON and messages rejected earlier at queue admission return their
  own error codes and emit no watch-only marker). Refusals for `touch down`,
  `scroll`, `key down`/`press`, `text`, and `rotate` (not `touch move`/`up` or
  `key up`) are recorded in the event feed; the verify verdict and settle rules
  are unaffected.
* **One upstream per session, only while watched.** The first browser
  connection to a session makes the hub connect to its `stream_socket` and send
  the one subscribe line; later connections join the same subscription (one
  encode per frame, per §3.3). When the **last** connection for that session
  ends the hub closes the socket and joins the thread before returning, so the
  producer's subscriber count goes back to zero and an unviewed session costs
  what it cost before. A browser that stalls a write for more than 2 s is
  dropped; it never blocks the producer or another browser. A closed tab is
  noticed by an EOF poll on the idle stream, not by waiting for the next frame.
* **Registry only.** `--registry-dir` overrides the directory
  (`$XDG_RUNTIME_DIR/acos/live` by default); `ACOS_VIEW_REGISTRY_DIR` has the
  same effect. The hub reads exactly that directory, sweeps entries whose
  `daemon_pid` is dead, lists a `stream_socket: null` entry as metadata only
  (not clickable), and reconnects by itself when a producer restarts.
* **T3 Code.** Paste the printed URL into T3 Code's preview pane. No patch and
  no plugin (ADR-0027 §3).
* **Cost.** `acos view` is a separate process that only runs when it is started;
  it changes nothing on the producer's command path, so the tracked M4–M8
  metrics, memory, and idle CPU match `v0.4.1` within noise both with `acos view`
  closed and open-but-idle (see `results/handoff/HUMANVIEW1/REPORT.md`).

The dev-only `ACOS_VIEW_STATS_FILE` seam (§3, measurement only) now also writes
once when the last subscriber leaves, so a consumer test can show the producer's
subscriber count back at zero.
