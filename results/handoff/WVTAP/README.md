# WVTAP — one input routing rule (touch by point, keys by focus)

Branch `wvtap`, resumed from the WIP commit `fb989d3a` ("paused for host
power-off", base merge040a `a819fd5c`). Worktree
`/home/Qui/Projects/acos/.cache/wt-wvtap`. Private `ACOS_RUNTIME_DIR`
(`.cache/rt-wvtap`) + `ACOS_SESSION_SOCKET`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`,
every ART run through `tools/art-runtime/run-exclusive.sh --task WVTAP` +
`tools/guarded.sh`, builds in `.cache/wvtap-target` (`*_DEBUG=0`, incremental
off). Fixtures built in the worktree with `tools/build-apps.sh` (111 APKs) and
`tools/build-dogfood-apps.sh` (12 APKs, needed by `acos-services`'s
`apk_facts_cache`).

## Status

| item | state |
|---|---|
| 1. Specified bug — A11YPACE/`webview-probe` step 11 `ELEMENT_NOT_FOUND` (WebView tap lost) | **FIXED at the cause** (§1); pinned by 3 tests that fail on the pre-change rule (§1.3) and `webview-probe` **10/10** (§4) |
| 1b. BK-1 `webview-fair` 10× | **10/10 PASS** (§4), with the latency tail measured and explained (§3) |
| 2. `cargo test -p acos-art -p acos-webview -p acos-wm -p acos-services` | **green** (§5) |
| 3. Regressions: `a11y-probe`, `touchdisp-*` | **PASS** (§4.3) |
| 4. Regressions: `gaten-bubble`, `rel3s-gaten-scroll` | **FAIL on this tree — pre-existing, not this track** (§6): measured identical failure with the routing change *reverted to the old rule* on the same tree, and independently on the base tree by MERGE040A (`MERGE040A/README.md` §5.1). Input is delivered and handled; the strict settle never completes (`reason: queue`). |
| 5. The "one view's first frame never decoded" (MERGE040A §5.2, sysui3a tree) | **not reproducible on this tree** (31/31 runs decode both views, §3.2); the failing log's mechanism is named and the open question is handed back with a smallest next action (§3.3) — the code it points at (broker session/engine generation) is not ATMS/permission/shade |

## 1. The root cause that was fixed: two rules for the target of an input event

`InputHost` published

* `tap` on **the newest live window** (`publish_on_live`), and
* `swipe`/`key`/`type_text` on **the focused window** (`publish_on_focused`,
  A11YPACE's fix for the dropped Back key).

`launch.rs` had a **second** implementation of the same question for the
scenario `tap` arm (a `wms.wm.hit_test` + local-coordinate conversion), so the
rule was implemented twice and the two disagreed. AOSP has exactly one rule
(`InputDispatcher`):

* **touch** (`tap`, swipe/scroll — any `DOWN`-carrying stream) goes to the
  window **under the point**: `findTouchedWindowAt` walks the layer stack from
  the top and takes the first *touchable* window whose frame contains the point;
* **keys/text** go to the **focused** window (`dispatchKeyLocked`).

With an accessibility overlay registered last (A11YPACE's own `back-layers`
setup) or a dialog holding focus, the "newest live window" rule sent the tap to
a window the user did not touch. On the merged tree the WebView's page tap
therefore never reached the guest's page: `webview-probe` step 11 stayed
`ELEMENT_NOT_FOUND` (`MERGE040A/bisect-6954ded3/`), while `webview-probe` on the
wvfair-only tree was 20/20 — the tap target was the difference, not the frames.

### 1.1 The fix (the WIP commit, kept and completed here)

* `crates/acos-services/src/input_dispatch.rs` — `InputHost` gains the session's
  layer stack (`with_window_stack`, wired once in `registry.rs`) and one
  `touch_target(x, y)` that returns the window under the point **and the point
  in that window's space**. `tap`/`swipe` use it; `key`/`type_text` keep
  `publish_on_focused`. A touch with no touchable window under it is a
  **structured refusal** (`ChannelError::NoTouchTarget`), never a delivery to
  some other window (`InputDispatcher` drops such a gesture).
* `crates/acos-wm/src/layers.rs` — `hit_test_candidates` (every touchable window
  under a point, topmost first) so a window whose input channel is gone is
  *skipped*, not replaced by "the newest window".
* `crates/acos-art/src/launch.rs` — the scenario `tap` arm's duplicate
  `hit_test` + local conversion is gone; it calls `InputHost::tap` like every
  other input path (`swipe` also stopped being routed by focus).

Nothing in ATMS / permission / shade / SystemUI code was touched.

### 1.2 Why one rule and not two

`swipe` used to be routed by focus while `tap` was routed by registration
order: a drag that started on an overlay went to the activity that held focus
(the A11YPACE slip). Both are touch streams, so both now come from
`touch_target`. Keys are the one event class AOSP routes by focus, and that arm
is untouched.

### 1.3 The pins (before/after, measured)

`crates/acos-services/src/input_dispatch.rs`, `mod touch_target_tests`:

| test | what it pins |
|---|---|
| `touch_goes_to_the_window_under_the_point_not_the_focused_or_newest` | the rule: activity under the point while a dialog holds focus and a `FLAG_NOT_TOUCHABLE` overlay is the newest window |
| `a_scenario_tap_delivers_through_the_channel_of_the_window_under_the_point` | **end to end through real `InputChannel` loopbacks**: the activity's channel gets `DOWN`+`UP`, the focused dialog's and the overlay's get **0** Motion events |
| `a_swipe_goes_to_the_window_under_its_start_point` | a swipe is a touch stream (start point), not a focus event |
| `a_touch_with_no_window_under_the_point_is_refused` | structured refusal (`no touchable window at (100000,100000)`, names the rule) |
| `keys_still_go_to_the_focused_window_on_the_same_host` | keys still by focus on the same host |

`crates/acos-wm/src/wms.rs::hit_test_candidates_lists_the_stack_from_the_top` —
ordered candidates, `FLAG_NOT_TOUCHABLE` excluded.

**They fail on the old rule.** Reproduced by rebuilding the same tree with the
old target choice switched on (`tap` → newest live window, `swipe` → focused
window) and re-running the module:

```
$ ACOS_WVTAP_LEGACY_TARGETING=1 cargo test -p acos-services --lib \
    window_session::input_dispatch::touch_target_tests
test ...::a_touch_with_no_window_under_the_point_is_refused ... FAILED
test ...::a_scenario_tap_delivers_through_the_channel_of_the_window_under_the_point ... FAILED
test ...::a_swipe_goes_to_the_window_under_its_start_point ... FAILED
test result: FAILED. 2 passed; 3 failed        # with the rule: 15 passed, 0 failed
```

(The switch was a temporary one-line experiment and is **not** in the tree; the
committed code has one rule and no env branch.)

## 2. What the WIP commit already contained (resume notes)

The WIP commit is the fix above plus its tests and ~100 MB of evidence
(`evidence/probe10/` = `webview-probe` 10/10, `evidence/fair10/` = `webview-fair`
9/10, `run1..3`, `priv1`, `tra..tre`, `fixf..fixh`, `hang1`). It carries no
README, so the experiment dirs are unlabelled; what I could reconstruct:

| dir | what it measurably is |
|---|---|
| `probe10/run1..10` | `webview-probe` 10/10 pass on the fixed tree, **both views decode** in every run |
| `fair10/run1..10` | `webview-fair` **9/10**: `run10` fails step 10 because the probe's own measurement was **5405 ms** against the scenario's `< 3000 ms` bound (§3.1) |
| `tre` | `webview-probe` pass (fixed binary) |
| `trc`, `trd` | `webview-probe` **fail at step 9** (`frame decoded view=2` never appeared; view 1 decoded twice) — the MERGE040A-§5.2 class, on whatever variant binary those arms used (§3.2) |
| `tra`, `trb` | never reached a verdict (`activity-not-resumed` marker); a launch experiment |
| `fixf`, `fixg`, `fixh`, `hang1` | `webview-probe` **pass**; unlabelled variants of the same run |

I re-ran the two acceptance scenarios from scratch on this tree (`evidence/probe10c`,
`evidence/fair10b`) instead of inheriting the WIP's numbers.

## 3. The two remaining questions, measured

### 3.1 `webview-fair`'s tap latency tail (explained, not fixed here)

`webview-fair` taps page A's button while page B runs a page that keeps the
engine's main thread busy in 1.9 s slices. On this tree, 10 fresh runs:

```
tap -> page click:  2, 2, 2, 3, 3, 4, 8 ms          (6 runs: the engine was free)
                    2004, 2519, 2525, 2619 ms        (4 runs: one phase paid a slice)
```

All ten pass (bound < 3000 ms, `evidence/fair10b/runs.log`); the WIP's
`fair10/run10` measured **5405 ms** (both phases paid a slice) and tripped the
scenario's own bound. Mechanism, from the code and the numbers: under
`--enable-begin-frame-control` a tap needs **two** compositor frames (one per
touch phase). Each of those input pulls waits behind at most one other view's
attempt (bounded, 750 ms) plus **its own** attempt, which deliberately keeps the
full 2 s budget — WVFAIR measured that cutting an input attempt short drops the
gesture (lost clicks). One phase inside a 1.9 s slice ≈ 2.0–2.7 s; both ≈ 5.4 s.
This is WVFAIR's own documented residual (`WVFAIR/REPORT.md` §7.1: "ART tap
latency variance … the knob is `BrokerConfig::frame_attempt` (750 ms), at the
cost of the slow page's own frames"). It is *not* the routing seam: the click
lands in every run, and the `tap` step's own settle passes. The bound is met
10/10 here but has ~13 % margin in the worst measured pass, so a loaded host can
trip it — an orchestrator decision (tighten the input attempt budget and accept
dropped gestures, or widen WVFAIR's measured bound), not something I changed.

### 3.2 The "one view's first frame never decoded" (MERGE040A §5.2)

Not reproducible on the fixed tree: **31/31** runs decode both views
(`evidence/probe10c/run1..10` v1=3 / v2=2..7 lines each, `evidence/fair10b` waits
on view 2 and passes 10/10, `evidence/final-smoke`, plus the WIP's own 10/10).

The failing log the merge left behind names a different mechanism than input or
frames-per-view (`MERGE040A/bisect-8e367c28/webview-probe/art/logs/logcat.txt`):

```
requestFrame view=1 … -> render #1        (no answer for 30 s)
frame decoded view=2 343x213 seq=1
W render failed: … NO_VIEW: no view 2      (repeated for the rest of the run)
W ax refresh failed: … NO_VIEW: no view 2
W render failed: … TIMEOUT: render answered nothing in 30 s
```

i.e. the broker's **session lost its view map** (`Session::view(id)` → `None`,
`NO_VIEW: no view <id>`, the `session.remove`/`create_view` shape) while one
render on the old view was still in flight — not a frame-production or input
fault. That is the webview broker's session/genbook-keeping seam, and the trigger
that makes it happen on a sysui3a-merged tree (heavier launch → timing) is
exactly what MERGE040A could not bisect either. I did not "fix" it by guesswork:
with no reproduction in 31 runs, a change here would be unverifiable.

**Smallest next action (for whoever owns the sysui3a block):** run
`scenarios/webview-probe.yaml` in a loop (≥ 20×) on the merge tree with
`ACOS_WEBVIEW_LOG`-style broker capture, and grep for `no view <id>` / a broker
`Session` being replaced mid-run; the two candidates are (a) a second
`hello` on the same session id replacing the `Session` object, (b) the gate
spawning a broker whose socket the provider had already attached to. Both are in
`crates/acos-webview` (`server.rs` `hello`, `broker.rs` `open_session`), not in
ATMS/permission/shade.

## 4. Acceptance runs (`results/handoff/WVTAP/run-acceptance.sh`)

One ART acquisition per run; `ACOS_ART_BIN=.cache/wvtap-target/release/acos-art`,
`./.cache/wvtap-target/release/acos verify … --engine art --artifacts …`.

### 4.1 `webview-probe` × 10 — **10/10 pass**

`evidence/probe10c/run1..10` (verdicts, steps, logcat), `runs.log`:

```
run1 pass … run10 pass        20 steps each; frame decoded view=1 (3 lines) and
                              view=2 (2–7 lines) in every run
```
The same scenario on the same binary again after the final rebuild:
`evidence/final-smoke/webview-probe` **pass** (20 steps).

### 4.2 `webview-fair` × 10 — **10/10 pass**

`evidence/fair10b/run1..10`, `runs.log`; the probe's own end-to-end numbers in
§3.1; `evidence/final-smoke/webview-fair` **pass** (13 steps) after the rebuild.

### 4.3 Regressions — `evidence/regress/`

| scenario | result |
|---|---|
| `a11y-probe` | **pass** 15/15 (paced gestures, drag, transient Back tap) |
| `touchdisp-gesture` | **pass** 3/3 |
| `touchdisp-dispatch-probe` | **pass** 4/4 |
| `touchdisp-posted-side-effect` | **pass** 5/5 |
| `gaten-bubble` | **timeout** at step 1 (pre-existing, §5) |
| `rel3s-gaten-scroll` | **timeout** at step 1 (pre-existing, §5) |

## 5. The gaten regressions are not this track (A/B, measured)

Both gaten scenarios fail at their **first tap's strict settle**, with the tap
delivered and handled — MERGE040A §5.1's sysui3a regression:

```
input.handled: true, handled_evidence: {acked: 2, published: 2}
settle: bound=timeout reason=queue ack_ms=15 first_change_ms=631 pixels_changed=true
```

To make the attribution a measurement rather than a guess, the same tree was
rebuilt with the **old** target choice restored (`tap` → newest live window,
`swipe` → focused window) and the two scenarios run again:

```
evidence/legacy-target/gaten-bubble          timeout [(0,launch,pass),(1,tap,timeout)]
evidence/legacy-target/rel3s-gaten-scroll    timeout [(0,launch,pass),(1,tap,timeout)]
```

Identical failure, same step, same `reason: queue`. Independent of that,
`MERGE040A/README.md` §5.1 measured the same four `gaten-*` scenarios failing
this way on the **base** tree (before WVTAP) in three separate runs
(`corpus1/2/3`). So: input routing is not the cause; the settle's pending
`queue` (guest main-looper message) is the sysui3a service-block symptom
MERGEFIX2 owns. Per the brief I did **not** touch ATMS/permission/shade.

## 6. `cargo test -p acos-art -p acos-webview -p acos-wm -p acos-services`

`.cache/logs/test-final.log`: every target green; `acos-webview`'s
engine-dependent tests are the ones gated by `ACOS_WEBVIEW_TEST=1` (as in
WVFAIR's sweep). One target, `acos-services --test apk_facts_cache`, needs the
dogfood APKs (`tools/build-dogfood-apps.sh`); without them it fails with
`dialogs-menus.apk: No such file or directory` — after building them it passes
(measured). No test was weakened, ignored or removed.

## 7. Files

* `crates/acos-services/src/input_dispatch.rs` — `TouchTarget`, `touch_target`,
  stack-carrying `InputHost`, touch-by-point `tap`/`swipe`, `NoTouchTarget`,
  `mod touch_target_tests` (+ the end-to-end channel test).
* `crates/acos-services/src/input_channel.rs` — `ChannelError::NoTouchTarget`.
* `crates/acos-services/src/registry.rs` — one wiring point
  (`with_window_stack(wms_state)`).
* `crates/acos-wm/src/layers.rs`, `crates/acos-wm/src/wms.rs` —
  `hit_test_candidates` (+ test).
* `crates/acos-art/src/launch.rs` — the scenario `tap` arm stops choosing a
  target (one rule, one owner).

## 8. Evidence map and cleanup

```
results/handoff/WVTAP/
  README.md                     this file
  run-acceptance.sh             the batch (fair ×10, probe ×10, 6 regressions)
  evidence/probe10c/            webview-probe 10/10 (this tree, final fixtures)
  evidence/fair10b/             webview-fair 10/10 (this tree)
  evidence/regress/             a11y-probe, touchdisp-*, gaten-bubble, rel3s-gaten-scroll
  evidence/legacy-target/       the two gaten runs with the OLD rule (attribution)
  evidence/final-smoke/         one run of each after the final rebuild
  evidence/probe10/, fair10/, run1..3, priv1, tra..tre, fixf..fixh, hang1
                                the WIP commit's runs (see §2)
```
Frame PNGs of the passing batches were pruned after the fact (as MERGE040A and
SCENHYG did); the verdicts, steps, logcat, engine and launcher logs are kept —
they are what the claims above are read from. Peak RSS ≈ 12 GiB (one ART run +
one release build), disk 76→78 % of `/`; `.cache/wvtap-target` (target) and the
`fixtures/build` copy are deleted at the end, per the disk budget.
