# A11YPACE — paced gestures, a real no-change tap claim, and the screenshot wrap

Branch `a11ypace` (from `modules` @ `b7cd7ce9`, with `gatenboot2` @ `6f146a46`
merged — the bubble scenario and the enable-set bind are the base the drag/tap
work needs). Worktree `.t3/worktrees/acos/t3-6f9a4261-d278f2d5fb0234a5`,
2026-11-17/18. Private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` under
`.cache/a11ypace-run`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`, every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`, builds in
`.cache/a11ypace-target` (dev/test debug=0, incremental off).

## Status

**PASS for items 1 (paced gestures), 2 (`expect_change: false`) and the
SIGSEGV half of item 3; gaten-bubble-overlay is BLOCKED by the GPU wall (item
3's residual), reported with options in §5 — not worked around.**

| Brief item | State |
|---|---|
| 1. Paced `dispatchGesture` on the peer's deadline-driven idle turn | **DONE** — §1; a 400 px swipe scrolls 649 px (400 + fling), a 300 px drag is seen as **−300 px over 20–23 MOVEs** |
| 1b. Gaten bubble drag in `gaten-bubble-overlay.yaml` | **NOT FEASIBLE** in the single-app scenario model (the gesture must be dispatched by *some* bound service, and only its own app can launch) — §5.1; the probe-app drag proof ships instead (a11y-probe) |
| 1c. Host swipe/`scroll_until` batch path | **already paced** (`inject_swipe` sleeps to each sample's time) — verified: `rel3s-gaten-scroll` pass 12/12 with `expect_change: true` |
| 2. `tap … expect_change: false` (tree/window changes) | **DONE** — §2; the async Back tap in `a11y-probe` uses it and its verdict records `input.expect_change: false` + `change_claimed: false` |
| 3. `takeScreenshot` SIGSEGV in `GraphicBufferMapper::getAllocationSize` | **DONE at the cause** — §3/§4; `Bitmap.wrapHardwareBuffer` returns `HARDWARE 1080x2400`, `getAllocationByteCount()=10368000`, no crash (`a11y-screenshot-wrap` pass 3/3) |
| 3b. *drawing* the wrapped HARDWARE bitmap | **BLOCKED (architectural)** — no OpenGL ES driver in the image; libEGL aborts the RenderThread. §5.2, options for the owner |
| Tests: `cargo test -p acos-art -p acos-verify -p acos-services -p acos-wm -p acos-protocol` | **PASS** — §6 (240 art, 119+39 verify, 639 services, 95+… wm, protocol; schemas regenerated) |

Evidence: `results/handoff/A11YPACE/regression/<scenario>/` (verdict, steps,
frames, logs) plus `<scenario>-host-trace.log` (`ACOS_TRACE_FILE`: the module's
paced-gesture timeline and the input targets).

## 1. Paced `dispatchGesture`: the stroke's declared timeline is its arrival timeline

A11YNATIVE2 measured the defect and named the shape: the module published all
15 `MotionEvent`s of a 400 px swipe in one turn, the guest's
`InputEventReceiver` batched them, and the `ScrollView` applied only the
slop-crossing delta (`list_scrolled y=11`). AOSP's `MotionEventInjector` posts
each event with `delay = eventTime - now`, so the cadence a finger produces is
what `VelocityTracker`/touch slop/`OverScroller` see.

`crates/acos-services/src/misc.rs`

* `PacedGesture` — one stroke in flight: its guest-sampled `MotionEvent`s, the
  `downTime` anchor, the next index, and `due(i)` / `next_due()`: the host
  `Instant` event *i* is due is the stroke's start plus that event's own
  `eventTime` offset. Nothing is resampled or invented; the guest's
  `GestureDescription` sampling **is** the schedule.
* `A11yGestureRecord` grew `accepted` (the boolean `dispatchGesture` answered
  now, the way AOSP answers the request before the gesture runs) and `published`
  (how many events have gone out), so the module report never implies a stroke
  was delivered while it is still being paced.

`crates/acos-services/src/a11y_bind.rs`

* `A11yConnectionNode` is the module's **dynamic** object — the one the binder
  peer polls per idle turn — so it owns `paced: Vec<PacedGesture>`.
* `dispatch_gesture` now validates exactly as before (capability, bound service,
  display, focused/eligible channel, live channel), queues the stroke and returns
  `accepted: true` immediately. It also traces
  `ACOSA11Y dispatchGesture seq=N paced events=… span_ms=… token=…`.
* `advance_paced(now)` publishes every event whose `due()` has passed (one
  `InputMessage` each, with a fresh `eventId`), updates the record and, after the
  **last** event, queues
  `IAccessibilityServiceClient.onPerformGestureResult(sequence, true)` — AOSP's
  own order (`injectEvents` → result after the final event). A publish failure
  ends the stroke with `completed=false` and a structured reason.
* `ServiceObject::next_delivery_deadline` returns the next sample's instant and
  `advance_timed` publishes the due events. That is the whole producer: the
  peer's `poll(2)` sleeps exactly until the next sample (or the guest speaks), so
  an idle session wakes at the stroke's sample times and never polls.

Measured (`a11y-probe-host-trace.log`): a 40 ms tap = 4 events at +0/16/32/40 ms,
`completed=true` after the fourth; a 200 ms swipe = 14 events at +0…+200 ms; a
400 ms drag = 20–23 MOVEs. The app's own evidence:

```
list_scrolled y=648/649        (400 px commanded + the ScrollView's own fling)
drag_end dx=-300 dy=0 steps=20 (the commanded 300 px drag, app-side)
```

### 1b. Why the Gaten bubble drag is not in `gaten-bubble-overlay.yaml`

`dispatchGesture` is served by the **accessibility service bound to the app that
calls it**, and a scenario launches exactly one app (`extra_apks` are installed,
not launched). Gaten's own code only taps its bubble (a lookup); nothing in it
dispatches a *drag*, and the probe app cannot share the session. The probe-app
drag (a view whose `onTouch` reports the delta, dragged by the probe's own
service) is the equivalent proof and ships in `scenarios/a11y-probe.yaml`.

The harness *could* grow this the honest way: a `drag`/`swipe` step that injects
through the session's own accessibility module (the harness is a peer, not an
app) — the producer above is exactly what it needs; the missing piece is a
scenario-level gesture driver in `acos-verify`, not a service change.

## 2. `tap … expect_change: false`: "delivered, no change claimed"

A tap carries an implicit tree/screenshot rule (`state_change`, reported as
`expect_change: "auto"`). That is wrong for a control whose effect is transient:
Gaten's bubble hides for its lookup and re-shows, and the accessibility Back tap
hands `performGlobalAction` to a worker thread, so the post-settle tree can be
the pre-tap tree and the rule samples a race (GATENBOOT2 measured 1 failure in 20
for the bubble tap; this branch measured the same on the Back tap).

`crates/acos-protocol/src/scenario.rs`: `TapStep::expect_change` is now
`Option<bool>` — `None` (unset) = the auto rule, `Some(true)` = a pixel claim
(existing SE1 semantics, still strict-only: `true` + `settle: fast`/`content` is
`INVALID_SCENARIO`), `Some(false)` = **no change claimed**. Both spellings are
the same field, so `expect_change:` reads the same on every input step.

`crates/acos-verify/src/executor.rs`: `eval_tap_change` takes the claim.
`Some(false)` produces the `input_delivery` assertion instead of the tree
comparison — *"input delivered (N guest ack(s), handled: …); no change claimed"*
— with `change_claimed: false` and `expect_change: false` in the assertion's
actual, and the step's `input.expect_change` reads `false` (not `"auto"`). It is
never a silent pass:

* the step still fails if the host did not observe the delivery (a `false`
  claim is a claim about the input, not about the screen);
* the unset default and `true` keep requiring the change (unit-tested: the same
  unchanged tree fails with `state_change` under the default and passes under
  `Some(false)`);
* the step's `input` block and the verdict say which claim was made.

Used by `scenarios/a11y-probe.yaml`'s async Back tap (`settle: fast` +
`expect_change: false`, with an `assert_log timeout_ms: 30000` that owns the
effect) and by `scenarios/gaten-bubble-overlay.yaml`'s bubble tap (GATENBOOT2
§5's smallest next action). Schema regenerated
(`crates/acos-protocol/schema/scenario.json`), `docs/AGENT_GUIDE.md` documents the
three values.

## 3. The screenshot SIGSEGV: the shim was stale, and a stale shim is now impossible

`Bitmap.wrapHardwareBuffer(buffer, colorSpace)` asks libhwui for the buffer's byte
size through `GraphicBufferMapper::getAllocationSize` (libui). In a gralloc-less
guest libui's mapper object (`this->mMapper`) is null, so its own copy faults at
`+3` (`mov (%rdi),%rdi` with `rdi = mMapper = 0`). The shim implements the entry
point — **but the sysroot's built `libacoshw.so` predated that source change**
(`gralloc_shim.hash` = the SHA-256 prefix of the source it was built from;
A11YNATIVE2 added the function after the sysroot was built). So this call
resolved libui's own copy exactly as the brief observed.

`crates/acos-art/src/lib.rs`

* `GRALLOC_SHIM_SOURCE = include_str!("../shim/gralloc_shim.c")` — the shim
  source is part of the **host binary**, and `gralloc_shim(&ArtRoot)` compares
  `shim/gralloc_shim.hash` with the embedded bytes' SHA-256 prefix.
  * stamp matches → the sysroot's prebuilt `.so` is used (an installed layout
    with no NDK keeps working);
  * stamp stale/missing → the embedded source is compiled once into the
    content-addressed build cache (`build_cache::ensure`, one compile per source
    change, then reused) and installed atomically into `<data>/shim/`;
  * no NDK — a structured `Error::Tool` naming
    `tools/art-sysroot.sh --shim`, never a crash.
* Both launcher paths (`session.rs`, `app.rs`) and the launch/verify secondaries
  (`launch.rs`: `SessionShims.gralloc`, `SecondaryShared.gralloc`) bind that
  file; `acos-cli`'s prewarm list includes the data copy. The `ACOSART_SHIM_TRACE=1`
  diagnostic prints the decision.

Measured on the *shared, stale* sysroot (the interesting case):

```
acos-art: gralloc shim: sysroot=…/shim/libacoshw.so stamped=false want=c2af8693… prebuilt=true
I AcosA11yProbe: wrap_hardware_buffer ok=true width=1080 height=2400 config=HARDWARE
I AcosA11yProbe: wrap_hardware_buffer allocation=10368000
```

and `scenarios/a11y-screenshot-wrap.yaml` passes 3/3 (the wrap + the shim's
`getAllocationByteCount` + `assert_no_crash`). The probe logs the wrap outcome
rather than skipping it, so the app-side path is exercised on every a11y-probe
run.

## 4. `Bitmap.wrapHardwareBuffer` works; *drawing* it is the GPU wall

The wrap returning a `HARDWARE` bitmap is now the truth of this image: the shim
owns the memfd, and libhwui's allocation-size call has a real implementation. But
libhwui draws a HARDWARE bitmap through GL, and this image has no OpenGL ES
driver. Measured on Gaten's real lookup (`results/handoff/A11YPACE/regression/
gaten-bubble-overlay/`):

```
Fatal signal 6 (SIGABRT) … Thread: "RenderThread"
#00 pc 000013f4  /data/shim/libartshim.so (__android_log_assert+452)
```

(the assertion text is libEGL's `couldn't find an OpenGL ES implementation`,
A11YNATIVE2 §4's stack). Interposing libEGL's `eglGetDisplay`/`eglInitialize` at
the PLT did not stop it, and neither did answering `eglGetProcAddress` for the
EGL names: Skia assembles its GL interface elsewhere and libhwui's GL path still
reaches the loader's fatal. I removed that unproven interposition again rather
than ship dead code.

This is the reason `gaten-bubble-overlay` is **not green** on this branch:

* the app's lookup now gets *past* the SIGSEGV (that is fixed) and dies in
  libEGL a moment later;
* the scenario reads the module's `screenshots` record before the app's
  (worker-thread, log-starved) lookup makes the request — GATENBOOT2's 24/24 run
  arrived in time; on this tree the guest takes ~5 s (measured with a 10 s wait),
  and any wait long enough for the request is also long enough for the crash.

## 5. What is not done, and the options the owner must pick

### 5.1 The Gaten bubble drag

Not expressible in a single-app scenario (§1b). Options: (a) a harness
scenario gesture driver through the session's accessibility module (the paced
producer is ready; ~a day in `acos-verify`/`acos-cli`), (b) launch support for a
second APK's activity so the probe's service can drag the bubble (its
`getWindows()` already sees the overlay), (c) accept the probe-app drag proof.

### 5.2 Drawing a wrapped HARDWARE bitmap (BLOCKED — architectural)

1. **Software EGL/GLES (or a CPU-backed `SkImage` for
   `MakeFromAHardwareBuffer`)** — the graphics track (A11YNATIVE2 option 2,
   weeks). This is the only option where an app can *use* a wrapped screenshot.
2. **Make the screenshot buffer CPU-only** — drop `USAGE_GPU_SAMPLED_IMAGE` from
   the `HardwareBuffer` the module returns (`SCREENSHOT_USAGE`). Then
   `SkImage::MakeFromAHardwareBuffer` refuses it, `wrapHardwareBuffer` throws its
   documented `IllegalArgumentException`, Gaten's lookup fails cleanly and the
   bubble re-shows. Cost: a deliberate deviation from AOSP's `usage=257` (and
   from `a11ynative.rs`/two scenarios' assertions), justified only if "no GPU"
   is the permanent guest contract (HWBITMAP).
3. **Keep apps off the wrap path** (A11YNATIVE2 option 1: host-side pixel claim)
   and accept that a real app that wraps a screenshot cannot draw it — i.e. the
   Gaten lookup sheet stays out of the v0.4.0 kit until 1 or 2.
4. A targeted `wrapHardwareBuffer` failure needs an hwui-level hook (its JNI
   function is static); the libEGL interposition is not it (§4).

Until one of these is chosen, `scenarios/gaten-bubble-overlay.yaml` is
`expectation: pass` but **fails** at the screenshot-record step on this branch,
with the run's own evidence under `results/handoff/A11YPACE/regression/
gaten-bubble-overlay/`. The scenario keeps GATENBOOT2's shape plus item 2's
`expect_change: false` on the bubble tap; nothing in it was weakened.

## 6. Tests (schemas regenerated)

```
cargo test -p acos-art       240 passed, 0 failed, 1 ignored   (16 targets; cleanup 8/8)
cargo test -p acos-verify    119 + 39 passed, 0 failed
cargo test -p acos-services  639 passed, 0 failed
cargo test -p acos-wm        95 lib + 10 targets, 0 failed
cargo test -p acos-protocol  95 passed (incl. schema freshness)
```

New teeth: `a11ypace_expect_change_false_passes_on_an_unchanged_tree_with_delivery_evidence`
(the same unchanged tree fails under the default and passes under `Some(false)`;
an undelivered input still fails), `a11ypace_tap_step_reports_the_explicit_no_change_claim`,
`a11ypace_tap_expect_change_false_is_an_explicit_no_change_claim` (protocol:
parses, round-trips, and is valid with `fast`/`content`, while `true` stays
strict-only), `a_paced_gesture_is_due_on_its_declared_timeline` (the schedule),
`keys_go_to_the_focused_window_not_the_newest` (the key target; see below).

`snippets`-class regressions found and fixed on the way:

* `InputHost::key`/`type_text`/`swipe` used "the newest live window". Measured
  with an accessibility overlay present: the harness `back` went to the overlay's
  `ViewRootImpl`, which logged `VRI[]: Dropping event (no window focus)`, and the
  app never saw it. `publish_on_focused` now targets the focused window
  (`InputDispatcher::dispatchKeyLocked`'s rule), with the newest live window only
  as the no-focus fallback; `back-layers` passes 22/22 with the overlay-free and
  dialog paths.

## 7. Evidence and repro

```
results/handoff/A11YPACE/
  run-regression.sh                  the batch below (shared lock + guard)
  regression/a11y-probe/             pass 15/15  (paced swipe y=649, drag −300/20 MOVEs,
                                                  transient Back tap expect_change:false)
  regression/a11y-probe-host-trace.log
  regression/a11y-screenshot-wrap/   pass  3/3   (wrap ok, allocation 10368000, no crash)
  regression/rel3s-gaten-scroll/     pass 12/12  (host swipe/scroll path)
  regression/back-layers/            pass 22/22  (keys go to the focused window)
  regression/gaten-bubble-overlay/   fail 21/22  (§5.2: the GPU wall, evidence kept)
```

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
results/handoff/A11YPACE/run-regression.sh            # the five scenarios above
```

## 8. Files

* `crates/acos-services/src/a11y_bind.rs` — `A11yConnectionNode.paced`,
  `dispatch_gesture` queues, `advance_paced`, `next_delivery_deadline`,
  `advance_timed`, `gesture_result_delivery` after the last event.
* `crates/acos-services/src/misc.rs` — `PacedGesture`, `A11yGestureRecord
  {accepted, published}`, `note_gesture_published`/`finish_gesture`,
  `gesture_channel_is_live`, `publish_back` trace.
* `crates/acos-services/src/input_dispatch.rs` — `publish_on_focused`,
  `InputHost::is_live`, focused keys/text/swipe, `set-focus` trace, tests.
* `crates/acos-protocol/src/scenario.rs` (+ `schema/scenario.json`) — trip-state
  `TapStep::expect_change`, validation, tests.
* `crates/acos-verify/src/executor.rs` — the `change_claim` path, tests.
* `crates/acos-art/src/lib.rs` — `GRALLOC_SHIM_SOURCE`, `gralloc_shim`, `ndk_clang`;
  `launch.rs`/`session.rs`/`app.rs` bind it; `acos-cli/src/prewarm.rs`.
* `fixtures/apps/a11y-probe/…` — the drag target + touch delta, the wrap probe,
  one-gesture-at-a-time dispatch.
* `scenarios/a11y-probe.yaml`, `scenarios/a11y-screenshot-wrap.yaml`,
  `scenarios/gaten-bubble-overlay.yaml`, `docs/AGENT_GUIDE.md`.

## 9. Environment

Target `.cache/a11ypace-target` (dev/test/release `debug=0`, incremental off);
`target` and the hardlinked `fixtures/build` copy are deleted at the end. The
private sysroot `.cache/a11ypace-sysroot` (hardlink-free `shim/`, used to test
the rebuilt shim before the launcher-side verification existed) is deleted too.
Peak launcher RSS in the a11y runs was 17–266 MiB; disk stayed at 76–78 % of `/`.

The shared `.cache/art/sysroot/shim/libacoshw.so` is **still the stale Nov-12
build** (not modified from here). It no longer matters for correctness: the
launcher detects the stale stamp and rebuilds from the embedded source (measured
in every run above). `tools/art-sysroot.sh --shim` refreshes the prebuilt copy
when convenient.
