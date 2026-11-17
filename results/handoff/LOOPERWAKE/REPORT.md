# LOOPERWAKE — does a background-thread post wake the idle main looper?

**Branch** `looperwake` (from `rel-030`, HEAD at start `5a912d4b`).
**Answer for rel-030: NO DEFECT.** The guest-measured post→drain latency is
**0–1 ms** and a background `View.postInvalidate()` is painted in **16–17 ms**
with no input of any kind. The NET6/WV7 reports are *modules*-branch findings;
they are not reproducible on rel-030 and this branch cannot produce their
trigger (no network module, no WebView module, hence no guest binder callback).

A real, smaller defect **was** found in the settle path that makes such an
effect *look* late, and it is fixed with a fail-before/pass-after test
(`crates/acos-verify/src/executor.rs`).

## 1. What was run

* Fixture `fixtures/apps/looper-wake` (new, plain framework APIs):
  * **path 1** — a plain `Thread` waits 1500 ms after the first drawn frame,
    then `runOnUiThread(setText("bg-update"))`; 250 ms later the same thread
    calls `pulse.postInvalidate()`. Marks: `bg-post`, `ui-drain
    latency_ms=<guest-measured>`, `ui-setText`, `bg-invalidate`, and
    `PulseView.onDraw` → `pulse.draw ... since_invalidate_ms=<guest-measured>`.
  * **path 2** — a `HandlerThread` (a non-main thread that owns a `Looper`, the
    shape of a framework/binder callback deciding to touch the UI) posts to the
    main `Handler` on demand (`go` button).
  * **path 3** — a plain thread calls `View.postInvalidate()` on demand
    (`invalidate` button): the traversal request is made from a non-main
    thread, no text changes, so only a real frame pump can paint it.
* Scenario `scenarios/looperwake-bg-post.yaml`, `engine: art`, `animations: off`,
  1080x2400 @ 420 dpi. The decisive step is an `assert_log` **with a
  `timeout_ms`** for the guest's own latency numbers: reading the log stream is
  the only observation that touches nothing in the guest (no `PROBE` write, no
  input, no capture), so it cannot wake the looper on the app's behalf.
* Run (private runtime dir, shared ART slot):
  `tools/art-runtime/run-exclusive.sh --task LOOPERWAKE -- tools/guarded.sh
  --mem 8G --time 400 -- acos verify scenarios/looperwake-bg-post.yaml`.

## 2. Evidence (guest's own clock, `SystemClock.uptimeMillis()`)

`results/handoff/LOOPERWAKE/after/logs/logcat.txt` (release `rel-030` + the
verify fix), tag `AcosLooperWake`:

```
first-draw frame=1 t=1713002
pulse.draw n=1 since_invalidate_ms=-1 t=1713019
pulse.draw n=2 since_invalidate_ms=-1 t=1713036
bg-post t=1714502
ui-drain latency_ms=1 t=1714503        <- 1 ms after the post, no input
ui-setText t=1714503
bg-invalidate t=1714753
pulse.draw n=3 since_invalidate_ms=16  <- background postInvalidate painted in 16 ms
pulse.draw n=4 since_invalidate_ms=-1 t=1714885
go.click / bg2-post t=1714887
ui2-drain latency_ms=15 t=1714902      <- HandlerThread -> main Handler: 15 ms
ui2-setText t=1714902
pulse.draw n=6 since_invalidate_ms=-1 t=1714919
invalidate.click
bg-invalidate-on-demand t=1715353
pulse.draw n=7 since_invalidate_ms=16  <- on-demand background invalidate: 16 ms
```

The earlier pre-fix run (`results/handoff/LOOPERWAKE/before`, same scenario,
16 s earlier) measured the same thing: post→drain **0 ms**, invalidate→draw
**17 ms**, HandlerThread→main **14 ms**, on-demand invalidate→draw **17 ms**
(`before/logs/logcat.txt`). Verdict: `pass`, 11/11 steps in both runs
(`after/verdict.json`).

## 3. Why this is the right place to look (and why rel-030 has nothing to fix)

* The ART guest runs the **image's own** `libandroid_runtime.so`:
  `crates/acos-art/src/launch.rs` sets `ACOSART_LOAD=libandroid_runtime.so` and
  `crates/acos-art/shim/render_shim.c` `dlopen`s it `RTLD_GLOBAL` in the default
  namespace from `.cache/art/sysroot/system/lib64/libandroid_runtime.so`
  (2 463 208 bytes, the stock image library). So `MessageQueue.nativeWake` /
  `nativePollOnce` are AOSP's real epoll+eventfd pair — there is no ACOS looper
  shim on this path to be wrong. (The only ACOS-owned looper is the Rust VM's,
  `crates/acos-vm/src/natives/looper.rs`, which is also a real
  `eventfd`+`epoll`; irrelevant to the ART engine.)
* The frame clock is the other half:
  `crates/acos-services/src/display_events.rs` `request_next_vsync()` bumps
  `pending` and `notify_all`s the writer `Condvar`; `writer_loop` writes the
  216-byte `DisplayEventReceiver::Event` on the BitTube `SOCK_SEQPACKET` pair.
  The guest's `Choreographer` registered the receive fd on the main looper, so
  that write *is* the looper wake. The prompt `since_invalidate_ms=16` shows the
  whole chain (invalidate → `scheduleTraversals` → `scheduleVsync` → host clock
  → BitTube → `doFrame` → draw) runs from a background-thread request.
* Therefore the reported symptom (NET6 §4: `runOnUiThread` from a guest binder
  thread; WV7 §4a: a decoded frame painted only on the next draw) is not a
  rel-030 looper/vsync defect. Both reports are from the **modules** line, and
  their trigger (module callback delivery / WebView broker frame) does not exist
  on rel-030. They remain **unverified on this branch** by design.

## 4. The defect that was real: the log wait judged a narrower slice than the assertion

`assert_log` with `since_previous_step: false` and `timeout_ms` polled
`all_logs[step_log_cursors[index-1]..]` (the per-step window) while the
assertion itself was evaluated on the whole stream. A line that was already
present before that window (e.g. flushed at the start of the current step, or
from an earlier step) could never satisfy the wait: the step polled for its
entire deadline and only then passed. Measured on rel-030:
`steps/07-assert_log.json` = **4069 ms** (`timeout_ms: 4000`) for a line the
host already held, in `before/steps/07-assert_log.json`; after the fix the same
step is **1 ms** (`after/steps/07-assert_log.json`).

Fix: `crates/acos-verify/src/executor.rs` — the wait uses exactly the slice the
assertion will use (`since_previous_step && index > 0 ? step_log_cursors[index-1]
: 0`). Regression test
`assert_log_wait_judges_the_same_scope_as_the_assertion` in
`crates/acos-verify/tests/scenarios.rs`:

```
pre-fix : panicked ... "got 25 fetches", test failed in 2.06s
post-fix: ok (3 fetches, 0.00s)
```

## 5. Acceptance

| item | result |
|---|---|
| `cargo test -p acos-vm -p acos-verify` | **PASS** — 35 green test binaries, 0 failed |
| `cargo test -p acos-art` (shared ART slot) | **PASS** — 166 + all suites, 0 failed |
| new scenario on rel-030 | **PASS** 11/11 (`after/verdict.json`), wake 0–1 ms, draws 16–17 ms |
| new verify test | **fails before** (25 fetches / 2.06 s), **passes after** (3 fetches / 0.00 s) |
| rel-030 affected by the reported wake defect | **NO** (see §2) |

## 6. Not done / smallest next action

1. Run `scenarios/looperwake-bg-post.yaml` and a NET6-shaped
   `netcb`-TextView assertion on **`modules`** (then WV8). If the callback's
   `runOnUiThread` really lags there, the difference is in *how the module
   delivers* (guest binder thread / broker socket thread), not in the looper
   natives or the vsync clock — the rel-030 evidence rules those out.
2. Nothing else: no host-side pump, looper fd or vsync change is justified by
   this branch's measurements, so none was made.
