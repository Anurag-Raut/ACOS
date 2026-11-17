# WV5b — host wiring: lazy activation, one broker per host, session end

Branch `wv5b` (from `wv4` @ `73abec44`, merged `wvb` @ `a2c9644d`). Host side only:
nothing in `fixtures/apps/webview-provider` or any guest Java was touched (that is
WV5a's branch).

Status: **DONE** — the lazy per-session gate, the shared broker daemon, the
per-session browser context, idle stop and session-end cleanup are implemented,
and a session-socket guest gets a real PNG frame from the real engine through
them.

## 1. What was missing and what closes it

WVB shipped `Broker` + `serve(broker, ServerConfig)` (one socket, one session).
WV4 mounted a per-session directory at `/run/acos`. Nothing connected the two, and
starting Chromium in every session — including the ones that never create a
`WebView` — is ~200 MiB for nothing (ADR-0031 item 3, orchestrator decision 2).

```
guest provider (WV2, unchanged)
   │ connect /run/acos/webview.sock            (WV4 mount, ro)
   ▼
Gate  (crates/acos-webview/src/gate.rs, started by launch_session)
   │  a listener thread and a socket node — nothing else
   │  first connect:
   │    ensure the shared daemon (spawn `acos-webview --shared` if absent)
   │    attach {"attach":1,"session":"<id>"}   (gate ⇄ daemon hop only)
   │    then proxy the wire byte for byte (two io::copy threads)
   ▼
acos-webview --shared  (one per host; $XDG_RUNTIME_DIR/acos-wv/broker.sock)
   │  server::serve_shared: one connection = one session = one browser context
   │  engine lazy: Chromium starts at the first `hello`
   ▼
Broker ──CDP pipe──▶ chrome-headless-shell 157.0.8103.78
```

| file | what it is |
|---|---|
| `crates/acos-webview/src/gate.rs` | the per-session lazy listener, the attach handshake, the proxy, the daemon spawn (detached, `setsid`, reaped), the structured `ENGINE_UNAVAILABLE` when no daemon can be reached |
| `crates/acos-webview/src/main.rs` | `acos-webview` binary: `--shared --socket <endpoint>` (the daemon) and `--session <dir>` (WVB's one-session server, for hand-driving) |
| `crates/acos-webview/src/server.rs` | `serve_shared()` (many sessions, own context each, idle exit), `bind_socket()` (never steals a live endpoint; a stale node is re-bound), attach-line reading (byte-wise, no framed byte swallowed), `wait()` |
| `crates/acos-art/src/webview_socket.rs` | the socket mapping: session dir is now `$XDG_RUNTIME_DIR/acos-wv/<session-id>` (fallback `/tmp/acos-wv-<uid>/<id>`), the broker endpoint is its sibling `broker.sock`, `start_gate()` (honours `ACOS_WEBVIEW_GATE=0`) |
| `crates/acos-art/src/launch.rs` | `launch_session` starts the gate right after the socket dir is resolved and length-checked, and the launch note says which arm it is (see §5) |

Decisions honoured: **one broker per host**, started lazily on the first WebView
connect, idle-stopped; **one browser context per session**; the session pins
`ACOS_WEBVIEW_SOCKET_DIR` to a short runtime directory that the broker listens in
and WV4 mounts at `/run/acos`. The engine sandbox is unchanged (accepted shape).

Three deliberate design points:

1. **Proxy, not fd pass.** Both hops are plain `SOCK_STREAM`; two `io::copy`
   threads keep every wire byte identical and keep all framing in `server.rs`.
   The cost is one socketpair hop per message (unmeasurable against the 31–36 ms
   first frame, §3).
2. **The attach line is the only out-of-band byte**, and it is on the *gate ⇄
   daemon* hop, never on the session socket: the guest wire is untouched
   (`WIRE.md` v1 end to end). It names the session, so the broker's contexts are
   labelled with the real session id.
3. **Session end is the connection's end.** `Conn::run` already closes the
   broker session and disposes the browser context; the gate removes the socket
   and the per-session directory when its listener drops; the daemon stops the
   engine and exits when no session has been attached for its idle timeout.

## 2. Tests

| test | what it pins |
|---|---|
| `tests/gate_lazy.rs` (3, no engine) | lazy (no daemon before the first connect, checked with a spawn marker), byte-faithful proxying to a stub daemon, `ALREADY_CONNECTED` for a second concurrent connection, re-attach after the first closes, `ENGINE_UNAVAILABLE` **on the wire** when the daemon cannot be reached, socket + session dir removed on drop |
| `tests/gate_cost.rs` (1) | a session that never connects pays a socket and a thread: 5 gates, start cost, PSS delta, thread delta, zero processes |
| `tests/gate_engine.rs` (1, real engine) | two sessions through two gates into **one** daemon: `hello` → `view.create` → `view.loadData` → `render` → **PNG 49 951 B at 1080×2400** for A and a second PNG for B; `memory.contexts == 2`; the second session spawns no broker; one broker pid; session dirs removed; the daemon **idle-stops** (pid gone, endpoint file gone) |

Acceptance run (both crates, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`):
`evidence/host-tests-acceptance.txt` — **238 passed, 0 failed**, exit 0
(`acos-art --lib`: 160 passed, 0 failed, 1 ignored; `acos-webview`: 17 lib + 3
`gate_lazy` + 1 `gate_cost` + 1 `gate_engine`, and `engine_gate`/`engine_bare`
print their SKIP without `ACOS_WEBVIEW_TEST=1`).

Real-engine run: `evidence/gate-engine-2sessions.txt`
(`test result: ok. 1 passed … session A: 49951 PNG bytes, session 37–42 ms, engine
HeadlessChrome/157.0.8103.78`).

Fail-before/pass-after, per behaviour changed:

* **lazy**: `gate_is_lazy_and_reports_engine_unavailable_on_the_wire` fails
  before the gate exists (no listener at the session socket at all — WV4's arm 1
  `ENOENT`), and the spawn marker would exist before the first connect if the
  gate started a daemon at session start.
* **one broker / own context**: before `serve_shared`, a second connection to a
  broker socket was refused (`ALREADY_CONNECTED` on the second session — WIRE
  §0's rule applied host-wide). The two-session test measured exactly that
  failure while it was being written (`assert_eq!(gate_b.stats().broker_spawns,
  0)` failed with 1, `contexts` would have been 1), and `bind_socket` now refuses
  to rebind a live endpoint after that test caught a second daemon stealing it.
* **idle stop / cleanup**: the test asserts the daemon pid disappears and the
  session directories vanish; the zombie-reaping bug (a detached child nobody
  waited for) was found by that assertion and fixed with an explicit reaper
  thread.

## 3. Metrics (shared slot, not exclusive)

**Listener cost on the session process** (`evidence/gate-cost.txt`, 5 gates in
one process, PSS from `/proc/self/smaps_rollup`):

```
start per gate: 43 µs, 21 µs, 19 µs, 20 µs, 18 µs   (max 43 µs, total 0.121 ms for 5)
pss: +81 KiB for 5 gates (16.2 KiB per gate)   [a second run: 96 µs max, +149 KiB]
threads: +5 (one accept thread each)   processes: +0   broker spawns: 0
```

**Session A/B** (`evidence/session-ab.json`, `evidence/measure_session.py`;
`acos-art launch --apk fixtures/build/apps/acos-counter.apk --until
application-created`, alternating arms, 5 reps each, gate on vs
`ACOS_WEBVIEW_GATE=0`):

| arm | `wall_ms` median (min–max) | launcher `self_s` median | launcher PSS peak median (min–max) |
|---|---|---|---|
| gate on | **131** (127–137) | 0.504 s | **218 542 KiB** (202 130–220 327) |
| gate off | **131** (126–142) | 0.504 s | **220 009 KiB** (207 542–222 490) |

Both arms: `outcome: ok`, 5/5 reps, no failures. The listener adds **no measurable
start time** (`wall_ms` medians identical; the sampled PSS is inside the run-to-run
noise and the gate-on median is *lower*) and **no process** — the shared broker
and Chromium exist only after a guest connects. The wiring itself is in
`evidence/gate-wiring-notes.txt`:

```
gate on : webview socket: /run/user/1000/acos-wv/<id>/webview.sock -> guest
          /run/acos/webview.sock (lazy gate listening; first connect starts the
          shared broker at /run/user/1000/acos-wv/broker.sock)
gate off: … (gate disabled by ACOS_WEBVIEW_GATE; no listener means the guest's
          connect fails with the real ENOENT)
```

## 4. Path lengths and process ownership

* Session dir: `$XDG_RUNTIME_DIR/acos-wv/<run-id>` (`/run/user/1000/acos-wv/…`),
  fallback `/tmp/acos-wv-<uid>/<id>`; socket `<dir>/webview.sock`, broker
  `<dir>/../broker.sock`. `webview_socket::check_sun_path` still refuses a
  mapping over 107 bytes, and a new test asserts the default paths fit.
* The daemon is `setsid`-detached, closes inherited fds above stderr at start
  (it must not keep a session's pipes alive), logs to
  `<runtime>/acos-wv/broker.log`, and its parent reaps it.
* **Process ownership did not move**: the broker is its own binary, not the MCP
  daemon's child and not `acos serve`'s. The gate is a thread in the session
  process (`launch_session`), so a session's WebView listener dies with it.

## 5. Deliberately not here

* Guest-side wiring (WV5a): the provider still connects to
  `/run/acos/webview.sock`; wall 9 (`ViewDelegate.setOverScrollMode` and the rest
  of the generated delegate surface) is guest provider code.
* A failed *attach* answers the guest with a WIRE error frame whose id is `"g?"`
  (the same convention `serve()` uses for `ALREADY_CONNECTED`), because the gate
  cannot know the guest's first request id without parsing the wire. The
  connection is closed right after, so a provider sees a fatal error rather than
  a hang; if WV5a needs the error attributed to `hello`, the frame can carry
  `"g1"` (WIRE's per-connection counter starts at 1) — an orchestrator call, not
  a silent choice.
* `render` still emits PNG only, `view.history.clear` still `UNSUPPORTED` (WVB's
  list, unchanged).

## 6. Reproduce

```sh
cd <worktree>
export CARGO_TARGET_DIR=$PWD/.cache/wv5b-target CARGO_PROFILE_DEV_DEBUG=0 \
       CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 ACOS_VIEW_TEST_DIR=$PWD/.cache/lv
# fast: gate contract, gate cost, all host tests
cargo test --offline -p acos-webview
# acceptance
run-exclusive.sh --task WV5b-acc -- guarded.sh --mem 12G --time 1400 -- \
  env -u ACOS_HOME ACOS_VIEW_TEST_DIR=$PWD/.cache/lv CARGO_TARGET_DIR=$CARGO_TARGET_DIR \
  cargo test --offline -p acos-webview -p acos-art
# real engine through the gate
run-exclusive.sh --task WV5b -- guarded.sh --mem 8G --time 600 -- \
  env ACOS_WEBVIEW_TEST=1 ACOS_WEBVIEW_CACHE=/home/Qui/Projects/acos/.cache/webview \
  CARGO_TARGET_DIR=$CARGO_TARGET_DIR \
  cargo test --offline -p acos-webview --test gate_engine -- --nocapture --test-threads=1
# session A/B (needs .cache/wv5b-target/release/acos-art)
run-exclusive.sh --task WV5b -- guarded.sh --mem 10G --time 800 -- \
  python3 results/handoff/WV5b/evidence/measure_session.py 5
```

## 7. Notes for the orchestrator

1. **`ACOS_VIEW_TEST_DIR` is required for `acos-art --lib`.** 7 `live_view`
   tests fail *without* it (their own 100-byte guard on
   `<worktree>/.cache/live-view-test/<pid>-<tag>/stream.sock` = 105 bytes in a
   `t3-…` worktree; untouched by this branch:
   `live_view.rs` has no diff). With the brief's
   `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`: 160 passed, 0 failed.
2. One `acos-webview --shared` process from an intermediate, buggy build was
   left running during the two-session test's development
   (`gate-2531511-two-sessions`, the run where a second daemon had stolen the
   endpoint); it was killed. The final build's test asserts the daemon exits.
3. Two `acos-art launch` runs for `evidence/gate-wiring-notes.txt` were executed
   directly, not through `run-exclusive` (0.5 s each, after the lock was free).
   Everything else with ART or the engine went through the lock.
