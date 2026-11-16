# VIEW1a handoff — live view, producer side (protocol v1)

**Worktree:** `/home/Qui/Projects/acos/.cache/wt-view1`
**Branch:** `live-view-1`, from `rel3-int` = `19db6b8` (v0.2.0 candidate). Not pushed, not merged.
**Track:** VIEW1a (live view phase 1, producer). Owner decision: ADR-0027.
**Consumer track (VIEW1b, later):** `acos view`, built against `docs/LIVE_VIEW.md`.

---

## 1. What exists now

A live ART session streams its screen and an action/event feed to local viewers,
at ~zero cost when nobody watches.

* The launcher (`acos-art`, which owns the C1 compositor state) binds a Unix
  stream socket in the session's existing short runtime dir
  (`/run/user/<uid>/acos-art-<pid>-<seq>/view.sock`, mode 0600) and reports it as
  a new ready-line field **`stream_socket`** (string, or `null` when it could not
  be bound — a session without a stream is a normal session).
* Protocol v1 (exact wire format, consumer rules, error behaviour):
  **`docs/LIVE_VIEW.md`**. `H` hello, `F` frame (JSON header line + image bytes),
  `E` event (`action`/`activity`/`lifecycle`/`settle`), 4-byte big-endian length
  + type byte framing, one subscribe line in.
* `acos serve` publishes `$XDG_RUNTIME_DIR/acos/live/<session_id>-<daemon_pid>.json`
  atomically (dir 0700, file 0600) when a session reaches ready, updates
  `activity`, and removes it on destroy/close/daemon exit. `client`/`project_dir`
  come from `ACOS_VIEW_CLIENT` / `ACOS_VIEW_PROJECT`, which the MCP server fills
  from the `initialize` `clientInfo` and its own cwd. A CLI-started daemon
  publishes `acos-cli` and its cwd. Stale = dead `daemon_pid`
  (`live_registry::is_stale` / `sweep`).
* Reference consumer: `tools/view/stream_probe.py` (stdlib only).

### Files

| file | change |
|---|---|
| `crates/acos-art/src/live_view.rs` | **new** (~1100 lines with tests): protocol framing, subscriber hub, tap/encoder/writer threads, box downscale, jpeg/png encode, event mapping (masking, truncation, activity), stats |
| `crates/acos-art/src/launch.rs` | bind the socket + `stream_socket` in the ready line; the frame source (`capture_latest` only when the C1 store counter advanced); `protocol_write` → event feed hook; request hook (coordinates/key/text); `ProbeTapWake` |
| `crates/acos-art/Cargo.toml`, `src/lib.rs` | `image` (jpeg+png only, offline), `pub mod live_view` |
| `crates/acos-cli/src/live_registry.rs` | **new**: entry document, atomic write, activity update, remove/sweep, stale detection, client parsing (3 unit tests) |
| `crates/acos-cli/src/engine/serve.rs` | publish on ready, refresh activity per call, remove on destroy, `remove_all` on shutdown |
| `crates/acos-cli/src/engine/art.rs` | `stream_socket`, `launcher_pid`, `display_size` accessors |
| `crates/acos-mcp/src/{server,private_daemon}.rs` | `clientInfo` → `ACOS_VIEW_CLIENT`, cwd → `ACOS_VIEW_PROJECT` on the private daemon |
| `docs/LIVE_VIEW.md` | **new**: the protocol contract (this is what VIEW1b codes against) |
| `tools/view/stream_probe.py` | **new**: reference consumer / probe |

---

## 2. Acceptance

Every ART run and every `acos-cli`/`acos-art`/`acos-mcp` test went through
`/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task VIEW1a --
tools/guarded.sh --mem … --time … --`. Builds through `tools/guarded.sh --mem 14G
--vmem 40000000 --time 1500 --`. `acos-bench.sh` was never wrapped in
`run-exclusive.sh` (it takes the lock itself).

### A. Unit/integration tests — **PASS**

`cargo test --release --offline -p acos-art -p acos-cli -p acos-mcp -p acos-wm`
→ **527 passed, 0 failed, 5 ignored** (52 suites, `--test-threads=1`).

`crates/acos-art/src/live_view.rs` (8 tests, real Unix sockets):

| test | asserts |
|---|---|
| `framing_round_trips_a_hello_a_frame_and_an_event` | 4-byte BE length + type + payload round-trips for `H`/`F`/`E`; a truncated message and a zero length are errors |
| `no_subscriber_means_no_source_call_and_no_encode` | **the cost seam**: 10 ticks with no subscriber → `source_calls == 0`, `encodes == 0`, `frames_captured == 0` (the source closure is never called) |
| `a_subscriber_receives_hello_frames_and_events` | hello (`v`, `session`, `display`, `format`, `scale`), ≥2 `F` messages with a JSON header (`w`/`h` = 540×1200 at scale 0.5, non-empty image), one `E` action event with x/y/status, 0 subscribers dropped |
| `a_slow_subscriber_is_dropped_without_blocking_the_encoder` | a viewer that never reads is disconnected (`subscribers_dropped ≥ 1`, `has_subscribers() == false`) and the tap never blocks |
| `event_queue_drops_the_oldest_when_full` | no subscriber → events discarded before the queue (zero cost); queue bounded at 64, oldest dropped, `events_dropped` counted |
| `event_text_is_truncated_and_password_text_masked` | text truncated to 64 chars + `…`; password text → `[masked]` and the secret never appears in the event; `key`, failed-tap status |
| `focused_password_nodes_are_detected_in_the_probe_tree` | only a **focused** `password: true` node masks |
| `downscale_box_averages_2x2_blocks` | box filter is an area average and honours a padded stride |

`crates/acos-cli/src/live_registry.rs` (3 tests): `write_update_remove_and_stale_detection`
(0600/0700 modes, every protocol field, activity rewrite only on change, a dead
`daemon_pid` reads stale and `sweep` removes it, `remove_all` only touches this
daemon's entries), `remove_is_idempotent_and_only_touches_its_own_session`,
`client_identity_parses_json_and_plain_shapes`.

### B. Live — **PASS** (counter and Gaten)

Driven through a real `acos-mcp` (its own private `acos serve --engine art`
daemon, REL3a) with `clientInfo {"name":"view1a-proof","version":"1.0"}`; the
harness read the registry, subscribed with `tools/view/stream_probe.py`, tapped,
and compared the decoded frames with PIL. Evidence: `.cache/view1-proof/*/report.json`
and the saved frames (`probe-*/frame-*.png|jpg`).

**counter.apk** (default format png, `.cache/view1-proof/counter-v3/report.json`) — 14/14 checks:

* registry entry: `v 1`, `client {view1a-proof, 1.0}`, `project_dir` = the
  worktree, `apk`, `package dev.acos.fixtures.counter`,
  `activity …MainActivity`, `display 1080×2400`,
  `stream_socket /run/user/1000/acos-art-<pid>-0/view.sock`, `launcher_pid`,
  `daemon_pid`; socket exists; **removed after `acos_destroy`**.
* probe: hello `{v 1, display 1080×2400, format png, scale 0.5}`; **27 frames, 3
  distinct**, and `frames_differ_decoded` = 3 distinct **decoded** images (the
  counter goes 0 → 1 → 2).
* **2 `E` tap events** with `x 157, y 431`, `status "ok"`, `frame_seq 2` and `3`.
* producer stats: 27 encodes, 0 dropped-latest, 0 subscribers dropped,
  1.16 ms / 20.0 KB per frame, 49 taps skipped by the capture rate gate.

**counter.apk, jpeg** (`.cache/view1-proof/counter-jpeg-final/report.json`) — 14/14:
25 frames, 3 distinct decoded, 2 tap events, 8.98 ms / 22.7 KB per frame.

**Gaten APK** (`/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`,
default png, `.cache/view1-proof/gaten-final/report.json`) — 15/15 during launch
+ one tap:

* registry entry with `package`, `activity`, `display`, `stream_socket`; removed
  after destroy.
* **44 frames, 44 distinct** (the RN screen animates through the launch),
  1.31 ms / 55.0 KB per frame.
* one `E` tap event at the tapped tab's bounds `x 810, y 2189`, `status "ok"`,
  plus the `tree` action event.

### C. Metrics — **PASS** (viewer-off at parity) + viewer-on cost

Interleaved A/B, **3 reps per arm alternating**, `tools/bench/acos-bench.sh --reps
1 --groups "a:latency:8G:300"` per invocation (one lock acquisition each), viewer
off in both arms.

* **base arm** = rel3-int, built from a pristine `git archive 19db6b8` export
  (`.cache/view1-base`, not a git worktree): `acos` sha `0fddb136…`, `acos-art`
  sha `f1b70d8f…` (no live-view code: `grep -c live_view|stream_socket
  crates/acos-art/src/launch.rs` = 0).
* **view arm** = this branch: `acos` sha `406762e2…`, `acos-art` sha `aaa786af…`.

The earlier attempt to use `~/.paseo/worktrees/3oxa6xao/h4`'s binaries was not
possible: its target dir is gone, and the orchestrator's `acos-bench` had already
finished (`pgrep -af tools/bench/acos-bench.sh` empty). The export build is the
same commit, so it is the same main arm.

p50 medians of 3 reps (`.cache/view1-ab/ab-final2.txt`):

| metric | base p50 | view p50 | delta | delta % | gate |
|---|---|---|---|---|---|
| `cpu_active_tap_strict_pct` | 47.25 | 46.49 | −0.76 | −1.61 % | ok |
| M4 warm launch (fast) | 164.279 | 163.473 | −0.805 | **−0.49 %** | ok |
| M4 warm launch (strict) | 306.353 | 306.293 | −0.060 | **−0.02 %** | ok |
| M5 screenshot | 24.259 | 24.200 | −0.059 | **−0.25 %** | ok |
| M6 reset (fast) | 197.982 | 198.232 | +0.250 | **+0.13 %** | ok |
| M6 reset (strict) | 344.579 | 339.548 | −5.030 | **−1.46 %** | ok |
| M7 tree (fast) | 0.512 | 0.586 | +0.074 | +14.34 % | ok (absolute +0.07 ms; both arms' rep spread 23 %/12 %) |
| M7 tree (strict) | 41.370 | 41.328 | −0.041 | **−0.10 %** | ok |
| M8 tap counter (fast) | 48.676 | 48.090 | −0.586 | **−1.20 %** | ok |
| M8 tap counter (strict) | 960.735 | 954.969 | −5.766 | **−0.60 %** | ok |
| M8 tap form (fast) | 44.433 | 44.200 | −0.233 | **−0.52 %** | ok |
| M8 tap form (strict) | 948.113 | 948.088 | −0.025 | **−0.00 %** | ok |
| tap ack | 5.000 | 5.000 | 0 | 0 % | ok |
| teardown / teardown wall | 13.0 / 18.893 | 13.0 / 18.824 | 0 / −0.069 | 0 % / −0.37 % | ok |

**No metric is over its own gate** (`max(category floor, 2 × worst rep
deviation)` per metric). Every M4–M8 delta is inside ±1.5 % except the
sub-millisecond M7-fast (+0.07 ms) which is inside the 2 ms absolute floor.

**Viewer-ON** (same arm/binary, 3 reps each; a `stream_probe` subscribed to every
session the run published — 13-18 sessions, 301-313 frames received per rep;
`.cache/view1-ab/viewer-on-final.txt`):

| metric | viewer off | viewer on | delta |
|---|---|---|---|
| `cpu_active_tap_strict_pct` | 46.49 | 58.14 | **+11.65 pp (+25 %)** |
| M4 warm launch (fast / strict) | 163.5 / 306.3 ms | 169.8 / 306.2 ms | +3.9 % / −0.0 % |
| M5 screenshot | 24.20 ms | 24.20 ms | +0.0 % |
| M6 reset (fast / strict) | 198.2 / 339.5 ms | 199.1 / 347.3 ms | +0.4 % / +2.3 % |
| M7 tree (fast / strict) | 0.586 / 41.33 ms | 0.528 / 41.25 ms | −10 % (−0.06 ms) / −0.2 % |
| M8 tap counter (fast / strict) | 48.1 / 955.0 ms | 47.6 / 954.3 ms | −1.0 % / −0.1 % |
| M8 tap form (fast / strict) | 44.2 / 948.1 ms | 44.6 / 948.6 ms | +0.8 % / +0.1 % |

**Encoder cost per frame at scale 0.5 (540×1200), producer's own clock**
(`ACOS_VIEW_STATS_FILE`, mean over a real streamed session):

| screen | format | mean bytes/frame | mean encode ms | frames |
|---|---|---|---|---|
| counter | **png (default)** | 20,041 | **1.16** | 27 |
| counter | jpeg q75 | 22,660 | 8.98 | 25 |
| Gaten (RN) | **png (default)** | 54,995 | **1.31** | 44 |
| Gaten (RN) | jpeg q75 | 38,337 | 9.44 | 46 |

Per frame with a viewer: capture (one composite) ≈ 4-6 ms + box downscale ≈ 2 ms
+ encode 1.2 ms ≈ **7-9 ms of a separate thread at ≤ 15 fps** — the +12 pp of one
core the bench sees during a tap cohort. Text at scale 0.5 is readable in both
formats (checked by eye on the saved frames: "Counter", "1", "Increment",
"Reset").

### D. Gates — **PASS** (final binary)

`acos verify --engine art` on the release build (`.cache/view1-gates/*.json`):

| scenario | result | steps |
|---|---|---|
| `v2-art-counter` | pass, art-reference, engine art | 11 |
| `w22-counter-reset` | pass, art-reference, engine art | 9 |
| `form-validation` | pass, art-reference, engine art | 28 |
| `w54-dialog-pixels` | pass, art-reference, engine art | 14 |
| `gaten-home` | pass, art-reference, engine art | 14 |

---

## 3. Decisions

1. **One encoder per session.** The first subscriber fixes `format`, `scale` and
   `max_fps`; later subscribers are told the active values in the hello and only
   choose whether they want frames/events. One encode per frame no matter how
   many viewers watch.
2. **Default format = PNG**, against ADR-0027's parenthetical "(JPEG/WebP)": it
   is 8-9× cheaper in CPU (the scarce resource — the encoder shares the host with
   the real guest) and lossless, so half-resolution text is readable by
   construction; it is also smaller than JPEG on a flat UI screen. JPEG stays
   selectable per subscriber / `ACOS_VIEW_FORMAT` and wins on bytes for
   image-heavy screens (Gaten: 38 KB vs 54 KB). Both measured, per the dispatch's
   "cheaper per frame that keeps readable text".
3. **The tap thread exists only while a viewer watches.** A permanent tap thread
   waiting on the guest's probe condvar added a second waiter that woke on every
   probe sample — measurable in M8 fast tap (the first A/B showed +8.1 % there).
   `LiveView` now starts the tap with the first subscriber and stops it with the
   last, and the writer blocks on a condvar until someone attaches. That made the
   viewer-off A/B clean (all metrics inside ±1.5 %).
4. **The capture is rate-limited to `max_fps`.** A capture is a full 1080×2400
   composite; capturing faster than the stream can send only produced frames the
   encoder dropped ("latest wins"). This cut viewer-on cost from +103 % to +25 %
   of one core with no loss of frames (0 dropped-latest in every run).
5. **Events carry the request's `x/y/key/text`, not the reply's.** A reply does
   not echo a selector tap's coordinates; `protocol_write`'s hook alone lost them.
   The launcher records the current request and the event merges it.
6. **Password text is masked from the launcher's own probe tree** (`focused` and
   `password` on a node), not from a caller flag — the guest already refuses to
   leak a password field's characters, so the tree is the authoritative signal.
7. **Registry entries are per session, named `<session_id>-<daemon_pid>.json`,**
   so two daemons can never collide and one daemon's `remove_all` cannot touch
   another's files. Staleness is the daemon pid; the hub should also connect to
   `stream_socket` (documented, since pids are reused).
8. **A bind failure never fails a session** (`stream_socket: null`), and a
   registry write failure is a log line: a viewer is optional infrastructure.

## 4. Remains / next actions

* **VIEW1b** (`acos view`): the hub reads the registry, sweeps stale entries,
  connects to `stream_socket`, renders `F` payloads and overlays `E` events.
  Everything it needs is in `docs/LIVE_VIEW.md`; nothing in this track has to
  change for it (a late subscriber sees only events after it connects, and the
  registry entry carries the current `activity`).
* Optional producer optimisations (not needed for phase 1): capture only the
  newest layer instead of compositing the display when a session has one visible
  layer (would cut the per-frame cost from ~7-9 ms to ~3 ms), and/or downscale
  during the composite.
* `m7_tree_fast_ms` is a sub-millisecond metric with 12-23 % rep-to-rep spread in
  both arms; it is not a useful regression signal at n=3 (absolute floor covers
  it).
* The `ACOS_VIEW_*` knobs (`FORMAT`, `SCALE`, `MAX_FPS`, `JPEG_QUALITY`,
  `REGISTRY_DIR`, `STATS_FILE`) are dev seams and are documented only in
  `docs/LIVE_VIEW.md` §1/§5 — `STATS_FILE` is measurement-only and must not be
  used by the hub.

## 5. Resources

* Disk: `/` was at 87 % when the track started (the operating directive says to
  prune at 85 %); nothing outside this worktree was deleted — the main checkout's
  `out/` is 4 KB and the other worktrees belong to other tracks. This track added
  ~14 GB (`.cache/view1-target` 6.7 GB + `.cache/view1-base*` ~7 GB) and `/` now
  sits at **84 % (146 GB free)**.
* Host RAM: 60 GiB; `MemAvailable` never sampled below **29 GiB**; the guards
  (`--mem 10-14G`) never tripped, no OOM, no swap.
* Left running: nothing (`pgrep -af 'acos serve|acos-art launch|dalvikvm64'`
  filtered to this worktree: none). Scratch kept for evidence:
  `.cache/view1-proof/` (frames, reports, logs), `.cache/view1-ab/` (bench runs,
  probes, tables), `.cache/view1-gates/` (gate JSON).
