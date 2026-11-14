# Live view — protocol v1 (producer side)

Status: **implemented** (track VIEW1a; owner decision ADR-0027). The consumer
(`acos view`, the local browser hub) is track VIEW1b and is built against exactly
this document. Nothing here changes an agent-facing reply, the settle rules or a
tracked benchmark metric.

A live ART session can stream its screen and an action/event feed to local
viewers. The stream exists only while a viewer is connected: with no subscriber
the session's frame path does **one atomic load** and nothing else.

* Producer: `acos-art` (the launcher owns the C1 compositor state).
* Socket: one Unix `SOCK_STREAM` per session, bound in the session's short
  runtime directory (`/run/user/<uid>/acos-art-<pid>-<seq>/view.sock`, the same
  directory as the session's peer socket, `sun_path` ≤ 107 bytes, mode 0600).
* Path published in the launcher's ready line as **`stream_socket`** (a string,
  or `null` when the socket could not be bound — a session without a stream is
  still a normal session).
* Registry: `acos serve` publishes one small JSON file per ready session, see
  §5. A CLI-started session (no `acos serve`) has a stream socket but no
  registry entry; the ready line and the `acos serve` session list still name it.

## 1. Client → server

One JSON line, sent immediately after connecting, then nothing else. The stream
is **view-only**: the producer ignores any later input (ADR-0027 §5).

```json
{"v":1,"subscribe":{"frames":true,"events":true,"max_fps":15,"scale":0.5,"format":"jpeg"}}
```

| field | meaning |
|---|---|
| `v` | protocol version, must be `1` |
| `subscribe.frames` | deliver `F` messages (default `true`) |
| `subscribe.events` | deliver `E` messages (default `true`) |
| `subscribe.max_fps` | 1..30 (default 15) |
| `subscribe.scale` | 0.05..1.0 (default 0.5 → 540×1200 for a 1080×2400 session) |
| `subscribe.format` | `"jpeg"` or `"png"` (default `"jpeg"`) |

**One encoder per session.** The *first* subscriber fixes `format`, `scale` and
`max_fps`; the hello says which values are active, and a later subscriber's
request for different ones is ignored (it still gets `frames`/`events` as it
asked). This keeps one encode per frame no matter how many viewers watch. A
session can also pin these from the environment (`ACOS_VIEW_FORMAT`,
`ACOS_VIEW_SCALE`, `ACOS_VIEW_MAX_FPS`, `ACOS_VIEW_JPEG_QUALITY`), in which case
the first subscriber's request is only honoured where the environment is silent.

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
{"v":1,"session":12345,"display":{"w":1080,"h":2400},"format":"jpeg","scale":0.5}
```

| field | meaning |
|---|---|
| `v` | protocol version (1) |
| `session` | the launcher (`acos-art`) pid |
| `display.w`,`display.h` | the **session display** size in pixels (frames are `scale` times this) |
| `format` | the active image format |
| `scale` | the active downscale factor |

### Type `F` — frame

Payload = a JSON header **line terminated by `\n`**, immediately followed by the
encoded image bytes (no length prefix of their own: the rest of the payload).

```json
{"seq":42,"generation":41,"w":540,"h":1200,"format":"jpeg","ts_ms":1763158000123}
```

| field | meaning |
|---|---|
| `seq` | the C1 store counter (monotonic; advances for **every** buffer the guest posts) |
| `generation` | the C1 layer generation of the captured buffer |
| `w`,`h` | the encoded image size (`display * scale`, rounded) |
| `format` | `"jpeg"` or `"png"` |
| `ts_ms` | host wall clock at capture (ms since the Unix epoch) |

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
| `cmd` | for `action`/`lifecycle`: `tap` \| `key` \| `text` \| `swipe` \| `tree` \| `screenshot` \| `reset` \| `close` \| `launch` \| `session` |
| `x`,`y`,`x2`,`y2` | input coordinates (display space) when the command had them |
| `key` | key code for `key` |
| `text` | typed text: **masked** (`"[masked]"`) when the focused field is a password field, otherwise always truncated to 64 characters (+ `…`) |
| `status` | `"ok"` or the launcher's structured code (`ELEMENT_OFFSCREEN`, `NO_FRESH_FRAME`, …) |
| `activity` | focused activity (`<package>/<Class>`) on `activity`/`lifecycle` events |
| `frame_seq` | the launcher's logical frame id of the reply this event describes, when it has one |
| `settle` | present only when the reply's observation was **not settled** (a timeout or an animating screen): the launcher's settle object, verbatim |

Event kinds:

* `action` — every command the launcher executed (`tap`, `key`, `text`, `tree`,
  `screenshot`, `swipe`), with its result `status`. Emitted for failures too.
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

1. **No subscriber** → the frame path is one atomic load (`subscriber_count`).
   No compositor lock, no capture, no copy, no encode, no allocation. Events are
   discarded before the queue. The tap thread blocks on the condvar the guest's
   own probe samples already signal (200 ms fallback), so an idle unviewed
   session has no extra wakeups beyond that.
2. **With a subscriber** → on a frame whose `seq` advanced, the launcher
   composites once (`capture_composite`, the same pixels a screenshot would use),
   box-filters it to `scale` (area average; 2×2 per output pixel at 0.5) and
   stores it in a single "latest" slot under a short lock. The compositor is
   never asked to `present`/`present_forced`; `frame_seq`, `generation`, damage
   and the settle state are untouched.
3. One encoder thread per session: latest wins, `max_fps` rate limit, one encode
   shared by every subscriber, non-blocking sends. A subscriber whose socket
   would block (or that closed) is dropped; the encoder never waits for it.
4. At most 4 subscribers; a fifth connection drops the oldest, so a viewer that
   reconnects is never locked out.
5. Text in the feed is masked for password fields and truncated; the producer
   never sends full-resolution pixels.

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
  "activity": "com.gaten/.MainActivity",
  "display": {"w": 1080, "h": 2400},
  "stream_socket": "/run/user/1000/acos-art-5701-0/view.sock"
}
```

* `client`/`project_dir` come from the environment the MCP server passes to its
  private daemon: `ACOS_VIEW_CLIENT` (the MCP `initialize` `clientInfo`, as
  `{"name":…,"version":…}`) and `ACOS_VIEW_PROJECT` (the MCP server's working
  directory). A CLI-started daemon publishes `"client": {"name": "acos-cli",
  "version": ""}` and its own working directory.
* `ACOS_VIEW_REGISTRY_DIR` overrides the directory (tests and tools only).
* **Stale entries**: a reader must treat an entry whose `daemon_pid` is not
  alive (`kill(pid, 0)` → `ESRCH`) as stale and delete it — a SIGKILLed daemon
  cannot remove its own files. Pid reuse is possible, so the real liveness test
  is connecting to `stream_socket`; a hub should do both. `acos-cli` exposes the
  helper (`live_registry::is_stale` / `sweep`) and the hub sweeps on each poll.
* `stream_socket` is `null` when the launcher could not bind one; such an entry
  is metadata only.

## 6. Reference consumer

`tools/view/stream_probe.py` (stdlib only) subscribes, saves frames and events to
a directory and prints a JSON summary:

```bash
tools/view/stream_probe.py --socket "$SOCK" --out .cache/view1-probe/run1 \
    --frames 4 --seconds 20 --save-frames
```

`--idle-exit S` makes it end by itself S seconds after the last message, which is
how a probe watches a live session without a kill.

## 7. Measurements (2026-11-14, this host, scale 0.5, 1080×2400 → 540×1200)

Encoded by the producer's encoder thread, one frame per encode, JPEG quality 75.
See `results/handoff/VIEW1a.md` for the full A/B and the viewer-on bench table.
