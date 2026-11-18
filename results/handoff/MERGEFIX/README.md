# MERGEFIX — the two ART regressions the v0.4.0 merge introduced

Branch `mergefix` from `modules` @ `b7cd7ce9` (the merge of `main`/v0.3.0 into
`modules`). Both regressions are fixed at their cause, with a test that fails
before the change and passes after.

## 1. `mproc3-two-process` — `wait_for id=="finish"` timed out (cross-process top)

**Symptom** (reproduced on `b7cd7ce9`, `before/mproc3-two-process/`): step 3
`wait_for` timed out after 30 s; the served tree was proc-a's (`package:
dev.acos.fixtures.proca`, every node `visible:false`) even though proc-b had
resumed and its window was the top one, and no `AcosProcB` line ever reached the
session log. `launcher.stderr.log` showed the secondary process started, bound,
got its host-built launch transaction and reported `activityResumed`.

**Root cause — a merge artifact, not a lifecycle change.** `crates/acos-art/src/launch.rs`
declared the session's probe state **twice**:

```rust
let probe_state = Arc::new(Mutex::new(ProbeState {          // modules (MPROC5):
    top_process: Arc::clone(&live.top_process),             // the Arc ATMS writes
    ..ProbeState::default()
}));
let probe_state = Arc::new(Mutex::new(ProbeState::default()));  // main (PERF4)
```

`main` had `ProbeState::default()`; `modules` had the wired one. The merge kept
both statements, so the wired binding was **shadowed by a private, default
`ProbeState`**. Its `top_process` is its own `Arc<AtomicU32>` starting at 0 and
nothing ever writes it, while ATMS's `publish_top` writes the *other* `Arc`. The
launcher serves `latest_probe()` = `by_process[top_process]`, so every tree reply
described the primary process however far the foreground had moved — and, because
the served sample stayed proc-a's, the second process's log window was never
exercised (its `ACOSPROBE` lines are what the last two assertions read).

This is exactly the "one implementation per seam" hazard: two parents each added
a probe-state constructor and the merge kept both lines.

**Fix.** One construction, through `ProbeState::for_session(top)` — the only
constructor, which **takes the `Arc` ATMS publishes into**, so it cannot silently
become a private copy again. ATMS's settle/launch semantics are untouched: the
`PeerWake`/`settle_change` wiring (main's PERF4) and `by_process`/`seq_offsets`
(modules' MPROC5/MPROC7) are unchanged, and the `mproc3` multi-process chain
(spawn → bind → host-built launch → top-of-task → finish → result replay) still
runs end to end.

**Pinned test** (`crates/acos-art/src/launch.rs`,
`probe_line_tests::the_served_sample_is_the_foreground_processes`): two processes'
samples, then `top_process.store(1)` — the *same* store ATMS performs — must
switch the served tree to proc-b's. Fails before (`left: "proca"`), passes after:

```
thread 'launch::probe_line_tests::the_served_sample_is_the_foreground_processes' panicked:
assertion `left == right` failed: a cross-process launch serves the new foreground process's tree
  left: "proca"   right: "procb"
```

## 2. `a11y-probe` step 14 — `assert_log` failed with every line present

**What each side did.** `assert_log`'s *ordered* matcher was identical in both
parents (`assertions::ordered_log_match`: one monotonic cursor over a single log
snapshot; `ordered: true` is the default). The semantics that differed is the
**scope** (`executor::log_scope_start`):

| | modules (`7ad8dcb0`) | main / v0.3.0 (LOOPERWAKE `381ec7ad`) |
|---|---|---|
| `since_previous_step: false` (default: one snapshot for the whole scenario) | wait judged the **per-step window**, assertion the **whole stream** | wait judges exactly the assertion's slice |
| `since_previous_step: true` | previous step's start | previous step's start |

So modules let a `timeout_ms` wait burn its whole deadline on a line the host
already held (measured 4.07 s of a 4 s deadline) — a slow-effect lie — while the
assertion still passed. The correct (AOSP-honest) rule is main's, and it is the
one the merged tree has, now in **one place**: `executor::log_scope_start` is the
only function that decides where a log slice starts, and both the wait and the
assertion call it, so they cannot diverge again. Inside the slice the ordered
cursor is monotonic (each pattern must match at or after the previous pattern's
match position, same snapshot); nothing resets the scope to the current step's
own start, and only `since_previous_step: false` reads the whole scenario's
snapshot. LOOPERWAKE's `assert_log_wait_judges_the_same_scope_as_the_assertion`
is kept green.

Pinned by `crates/acos-verify/tests/scenarios.rs`
`the_ordered_log_cursor_starts_at_the_previous_steps_boundary` (scripted log
backend): with `since_previous_step: true` a line the previous step produced is
visible, a line from before that boundary is **not** (a later step cannot
re-match history it did not opt into), and the default reads one snapshot.
`assertions::ordered_logs_require_order` keeps the intra-snapshot rule.

**The actual step-14 failure** was not the cursor: the last `assert_log` (the
`performGlobalAction(GLOBAL_ACTION_BACK)` effect) had **no `timeout_ms`**, so it
judged the log at the instant the tap's settle bound its frame — one looper
iteration *before* the app reads the published key. That is a genuinely
asynchronous effect (the key is published from inside the app's own callback and
read on a later looper turn), so the scenario now waits for it:

```yaml
# MERGEFIX: ... the app reads it on a later looper iteration than the one that ran
# its click handler. `timeout_ms` is the wait-for-effect semantic (a missing line
# still fails at the deadline) ...
timeout_ms: 10000
```

No pattern was removed or loosened: `key_down`/`key_up`/`back_pressed` are the
same three claims, the step still fails at the deadline if they never appear, and
the ordered `button_text … click handled=true` step (the one the merge note
suspected) was never ambiguous — `since_previous_step` defaults to **false**, so
it already reads the whole scenario snapshot, and it passes in every run below.

## Verification

Worktree env: private `ACOS_RUNTIME_DIR=$PWD/.cache/rt-mergefix`,
`ACOS_SESSION_SOCKET=…/session.sock`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`,
`ACOS_FONT_DIR` per the common brief; every ART run under
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`.

### Build + unit/integration sweep
`cargo test -p acos-art -p acos-verify -p acos-services -p acos-wm -p acos-binder-peer`
(under `run-exclusive`): **110 test binaries, 1184 tests passed, 0 failed**
(`nonart-tests.log`, `nonart-summary.txt`).

### ART acceptance corpus — `corpus1/` (runner: `run-acceptance.sh`)
`a11y-probe`, `mproc3-two-process`, `looperwake-bg-post`, `back-layers`,
`form-validation`, `sensors-scripted`, `sensors-off`, `notif-granted`,
`notif-denied`, `location-probe`, `location-geocoder`, `location-detach`,
`tail-telephony`, `tail-biometric`, `tail-bluetooth`, `tail-vibrator`,
`tail-detached`, `gaten-backstack` — **pass** (`rc=0`).
`webview-probe` failed in corpus1 at step 10 (`assert_text text=="tapped"` — the
tap inside the WebView content did not register) and **passed** on the immediate
rerun (`webview-rerun/`, `rc=0`); it also passed in MERGE030's corpus3 on the same
tree. Recorded as a pre-existing flake in the WebView content tap, not a
regression of this change (nothing here touches WebView or input delivery).

### Before/after
`before/` holds the two failing envelopes on `b7cd7ce9` (same binaries' tree
before the two fixes): `a11y-probe` step 14 `assert_log`, `mproc3-two-process`
step 3 `wait_for` timeout.

## Files changed
- `crates/acos-art/src/launch.rs` — one `ProbeState::for_session(top)`; the
  shadowed second declaration is gone; the wiring test.
- `crates/acos-verify/src/executor.rs` — `log_scope_start()` (one rule), used by
  the wait and by the assertion's slice.
- `crates/acos-verify/tests/scenarios.rs` — ordered-scope test with a scripted
  log backend.
- `scenarios/a11y-probe.yaml` — `timeout_ms: 10000` on the asynchronous Back
  assertion, with the reason.
