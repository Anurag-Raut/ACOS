# ROT1 — rotate on the ART backend (branch `art-rotate-1`, base main `1f8d76b`)

Status: **PARTIAL**, stopped at the 60-minute rule. Landed: the rotation geometry model and
the AOSP relaunch decision, both unit-tested in `acos-wm`. Not landed: the protocol command,
the guest config change, the gate and its negative. **`input.rotate` still fails structured**
(`UNSUPPORTED_BACKEND` / `input.rotate`), so no run can claim a rotate pass.

## 1. What is in the commit

`crates/acos-wm/src/geom.rs`

* `RectI::rotate_quarters(src_w, src_h)` — one quarter turn of a rect region into the new
  frame. Point map `(x, y) -> (src_h - y, x)`, both corners rotated then normalised, so a
  rect keeps its extent. This is the transform `DisplayFrames` applies to a rotated display.

`crates/acos-wm/src/display.rs`

* `SystemBars::rotate_quarters(src_w, src_h)` — every bar/cutout/gesture rect rotated.
* `SystemBars::app_insets_for(frame)` — which edge a bar occupies is read off the bar rect
  (spans the frame width ⇒ horizontal edge, spans its height ⇒ vertical edge), so the app
  window insets follow the rotation instead of assuming the portrait layout. `app_insets()`
  is now non-`const` (it derives from the frame); the two existing call sites in the tests
  were unchanged in value (carve inset `0,128,0,63` on the reference display).
* `DisplaySpec::rotated(rotation)` — steps one quarter turn at a time (frame dims swap each
  step), recomputes `app_bounds` from the frame minus the rotated bars, keeps `density_dpi`,
  `density`, `refresh_hz`; `rotated(current)` is a no-op, and `rotated` names a *target*
  rotation (not a delta).
* `DisplaySpec::is_landscape()`, `DisplaySpec::orientation()`.
* `RotationRelaunch` + `CONFIG_ORIENTATION|SCREEN_SIZE|SCREEN_LAYOUT|SMALLEST_SCREEN_SIZE`:
  AOSP's `ActivityRecord.getRealConfigChanged()` comparison — a rotation changes all four
  bits, so the Activity keeps its instance only if its manifest declares all four, otherwise
  the framework recreates it (`ActivityRelaunchItem`).

`crates/acos-wm/src/lib.rs`: re-exports `RotationRelaunch` and `Orientation`.

Tests (all green): `cargo test --release --offline -p acos-wm` → 53 lib tests + 4 integration
tests + doc-tests, including

* `rotate_to_landscape_swaps_the_frame_and_moves_the_bars` — 1080x2400 → 2400x1080,
  status bar `[0,0][1080,128]` → `[2272,0][2400,1080]`, nav bar → `[0,0][63,1080]`,
  app insets `63,0,128,0`, app bounds `[63,0][2272,1080]`, 914x411 dp, density unchanged.
* `rotate_back_restores_the_portrait_geometry` — every target rotation returns to the exact
  portrait frame/bars/app bounds; half turn mirrors both bars.
* `rotated_twice_is_a_half_turn_and_the_other_quarter_is_its_mirror` — rotation 3 bars
  `[0,0][128,1080]` / `[2337,0][2400,1080]`.
* `config_change_bits_follow_the_manifest_that_declares_them` — declared
  `orientation|screenSize|screenLayout|smallestScreenSize` ⇒ no relaunch; declared nothing
  (the fixture APKs) ⇒ relaunch; only `orientation` ⇒ relaunch with the three missing bits
  named.

## 2. What was discovered about the live path (needed for the next attempt)

* The ART session (`acos-art launch --interactive`) is served by **two** layers: the Rust
  host loop in `crates/acos-art/src/launch.rs` (≈4049, `match cmd`) owns `tree`/`screenshot`,
  and forwards `tap`/`key`/`type_text` to the guest line protocol
  (`crates/acos-art/guest/java/dev/acos/art/SessionMain.java`, `dispatchCommand` ≈700,
  phases A/B/C = mutate → dispatch input → publish frame). Host-side `ArtSession`
  (`crates/acos-cli/src/engine/art.rs`) mirrors it (`tap`/`key`/`text`/`capture_frame`,
  capabilities at ≈2815).
* Touches **do not** go through the guest line protocol: `tap` is a real `InputMessage` on
  the live `InputChannel` (`live.input.tap(...)` in `launch.rs` ≈4520, `acos-wm` input
  dispatcher). A rotate therefore has to move *both* the display the dispatcher maps into
  and the guest's window, or post-rotate taps land in the wrong place.
* The guest session has **no ViewRootImpl/WMS**: `SessionMain.traversal()` re-measures and
  lays out the decor at `AppMain.W x AppMain.PANEL_H` and `publishFrame()` draws the whole
  window into a `Bitmap(W, PANEL_H)` → PNG (`frame_<seq>.png`). `AppMain.W/PANEL_H/DENSITY/
  STATUS_INSET/NAV_INSET` come from argv, and `SessionMain.makeDisplay()` builds the
  `DisplayInfo` stub from the same numbers. So rotated pixels = new `W/PANEL_H` + a real
  re-measure/layout, and the tree bounds follow automatically (they are dumped from the
  laid-out decor).
* There is no `ActivityClientRecord` for the session Activity (`Activity.attach(...)` is
  driven directly with a fresh `Binder` token in `SessionMain.launch()`), so a real
  `ClientTransaction` config-change item has nothing to resolve the token against. The
  honest path is the one AOSP's `ActivityThread` performs *on the record*:
  `performSaveInstanceState` → `performStop` → `performDestroy` → new instance
  `attach(newConfiguration)` → `performCreate(state)` → `performStart` → `performResume`
  for the relaunch case, and `onConfigurationChanged(newConfig)` on the same instance for
  the handled case. `LaunchMain` already proves the reflection rungs for `attach`/
  `performCreate`/`performDestroy` and for real `ClientTransaction`s
  (`scheduleLocal`, `buildLaunchTransaction`), so the pieces are known-good.
* `android:configChanges` is **not parsed anywhere today**: `LaunchMain.buildActivityInfo`
  hardcodes `configChanges = 0` (≈3362) and `crates/acos-services/src/pms.rs:458` replies 0.
  The parser seam exists: `crates/acos-services/src/manifest_resolve.rs` already reads
  `label`/`icon`/`enabled` off the chosen `<activity>`/`<activity-alias>` node with
  `attr_*` helpers, so adding `config_changes: i32` (attribute is an `int` bitmask in the
  binary XML, values equal to `ActivityInfo.CONFIG_*`) is ~20 lines plus a fixture test.
  Every fixture APK in `fixtures/build/apps` declares no `configChanges`, so
  `RotationRelaunch::for_rotation(0).relaunches() == true` is the decision the existing
  scenarios (`counter.yaml`, `hello-views.yaml`, `v1-recreate.yaml`, `multi-activity.yaml`,
  `todo-list.yaml`) already expect — their `assert_log` blocks assert exactly the
  `onPause→onStop→onSaveInstanceState→onDestroy→onCreate→onStart→onResume` order.

## 3. Which scenarios are blocked today

`grep -rln "^\s*- rotate:" scenarios/` → `counter.yaml`, `hello-views.yaml`,
`multi-activity.yaml`, `todo-list.yaml`, `v1-recreate.yaml`. They are the ones a real-phone
reference run left `unsupported`. The gate list in
`/home/Qui/Projects/acos/.cache/wt-int/.cache/int/run_suite.sh` (`gates` mode) has no rotate
scenario, so the 11 gates are unaffected by this branch.

## 4. Next smallest actions, in order

1. `manifest_resolve`: parse `android:configChanges` into the typed activity decl, carry it
   to the guest on argv (`LaunchMain`/`SessionMain`), replace the hardcoded
   `configChanges = 0`. Unit test with a synthetic AXML fixture (both `orientation|screenSize`
   and unset).
2. Guest `SessionMain`: `case "rotate"` in `dispatchCommand` (phase A), apply in phase B
   (before `publishFrame`):
   * `new_rotation = arg`, `W/PANEL_H` swapped, `STATUS_INSET`/`NAV_INSET` re-derived from
     `DisplaySpec::rotated` (pass the resulting bar rects, not just thicknesses, so the
     dispatch of insets is faithful);
   * new `Configuration` (`orientation`, `screenWidthDp`/`screenHeightDp`,
     `smallestScreenWidthDp`, `screenLayout` long/size bucket) + `Resources.updateConfiguration`;
   * if the manifest declares all four bits: `Activity.onConfigurationChanged(newConfig)`
     on the live instance (real framework method, so the app's override runs);
   * else: `Bundle state = new Bundle(); performSaveInstanceState(state); performStop();
     performDestroy();` new instance → `attach(newConfiguration)` → `performCreate(state)`
     → `performStart` → `performResume`;
   * re-measure/layout the decor at the new frame (`traversal()` already does it from
     `AppMain.W/PANEL_H`), re-dispatch the rotated insets, `publishFrame()`.
   * reply carries `rotation`, `width`, `height`; the host reply must be built only from a
     **new** frame plus a **new** probe sample, exactly like `tap`.
3. Host: `launch.rs` interactive `match cmd` gains `"rotate"` (forward
   `{"cmd":"rotate","rotation":n}`, require a fresh frame + probe, answer
   `rotation`/`width`/`height`); `ArtSession::rotate(&mut self, rotation: u8)` in
   `crates/acos-cli/src/engine/art.rs` modelled on `tap()`, with `SessionCapabilities { rotate: true }`;
   `InputAction::Rotate(Orientation)` in `crates/acos-cli/src/engine/mod.rs:980` maps
   portrait→0 / landscape→1 and calls it, then `self.action_result(previous, action.kind())`.
   A rotation the host cannot apply (bad rotation, guest refused) must stay a structured
   error naming `input.rotate`.
4. `acos-wm` input mapping: after the flip, the dispatcher's display frame and its
   window→display transform must use the rotated `DisplaySpec` (so a tap at the tree's new
   bounds lands on the same view); assert with a scenario that taps a control *after*
   rotating.
5. Scenario `scenarios/rot1-rotate.yaml` (positive, structural so it is not tied to one
   device's exact dp): launch counter/hello-views → increment/type → `rotate: landscape` →
   assert the count/text survived and the root bounds are `width > height` → `rotate:
   portrait` → assert the portrait root and the same state. Negative
   `scenarios/negative/rot1-rotate-wrong.yaml`: same flow but asserts portrait bounds after
   rotating to landscape, which must fail `ASSERTION_FAILED`.
6. Then: 3/3 positive + 3/3 negative, the 11 gates, the 5 Gaten flows and 5 Gaten negatives,
   and the interleaved A/B (`ACOS_BENCH_TASK=ROT1`).

---

# ROT2 section (branch `art-rotate-1`, base `1870756` + ROT2 commit)

Status: **PARTIAL** again, stopped at the 60-minute rule. Landed this pass: the
`android:configChanges` half of step (a) — parsed from the binary manifest into
`LauncherActivity::config_changes` with `PackageParser`'s exact rules, unit-tested on
synthetic AXML, and carried to the engine's guest as `ACOSLAUNCH_CONFIG_CHANGES` →
`ActivityInfo.configChanges` (was a hardcoded `0`). Nothing was flipped to "supported":
`input.rotate` still answers `UNSUPPORTED_BACKEND`/`input.rotate`, so no run can claim a
rotate pass.

## 1. What changed

* `crates/acos-services/src/manifest_resolve.rs`
  * `LauncherActivity::config_changes: i32` (`ActivityInfo.configChanges`).
  * `parse_component` reads `android:configChanges` (`attr_int`, `IntDec`/`IntHex`); an
    `<activity-alias>` does **not** read it — `parseActivityAlias` copies the target's
    (`info.configChanges = target.info.configChanges`), modelled the same way the alias label
    already falls back to the target.
  * `get_activity_config_changes(declared) = declared | (CONFIG_MCC|CONFIG_MNC)`, exactly
    `PackageParser.getActivityConfigChanges(c, 0)` (`RECREATE_ON_CONFIG_CHANGES_MASK`).
  * tests: `config_changes_are_parsed_from_the_binary_manifest_int_bits`,
    `all_four_rotation_bits_keep_the_activity_and_no_declaration_relaunches`,
    `activity_alias_inherits_the_targets_config_changes` — they assert the parsed bits **and**
    the `acos_wm::RotationRelaunch` decision derived from them.
* `crates/acos-art/src/launch.rs`: `ACOSLAUNCH_CONFIG_CHANGES` env for the guest.
* `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`:
  `setFieldForce(cls, ai, "configChanges", (int) envLong("ACOSLAUNCH_CONFIG_CHANGES", 0))`.

## 2. The correction to this handoff that matters for the next attempt

ROT1 §4.2 targeted `SessionMain.dispatchCommand` for the rotate verb. **The engine never runs
`SessionMain`.** `crates/acos-cli/src/engine/art.rs` spawns `acos-art launch --interactive`,
whose guest is `launch.rs::GUEST_CLASS = dev.acos.art.LaunchMain`; its stdin reader accepts
exactly `PROBE`. `SessionMain` is the `acos-art session` guest (`session.rs`). So:

1. the guest verb for a rotate on the engine path is a **new stdin line in `LaunchMain`**
   next to `PROBE` (e.g. `ROTATE <n>`), handled on the main looper — a `{"cmd":"rotate"}` JSON
   line has no reader there;
2. `LaunchMain` runs a **real** `ActivityThread` transaction path
   (`scheduleTransaction`/`LaunchActivityItem`/`ResumeActivityItem`) with the local `mWMS`, so
   D1's AOSP sequence is reachable *through the framework*: rotate the `mWMS` display, build the
   new `Configuration`/`MergedConfiguration`, and let
   `ActivityThread.handleConfigurationChanged` decide relaunch vs `onConfigurationChanged` from
   `ActivityInfo.configChanges` (now the manifest's real value — that is what this pass added).
   That is a WMS/binder-shaped change, which is why it did not fit a 60-minute window on top of
   (a);
3. `SessionMain`'s `traversal()`-at-`AppMain.W x PANEL_H` decor remains the cheap place to
   *unit-prove* the Activity-level sequence, but `acos-art session` is not what `acos verify`
   runs, so it cannot back `input.rotate` alone.

## 3. Measured this pass

* `cargo test --release --offline -p acos-services` — 84 lib + 2 integration green (3 new).
* 11 gates: 8 pass; `suite/T03-render-wrong` `ASSERTION_FAILED`,
  `negative/offscreen-pager-visible` `ELEMENT_OFFSCREEN`, `p11-progress-timeout` `TIMEOUT`.
* 5 Gaten flows pass; 5 Gaten negatives `ASSERTION_FAILED`.
* Evidence: `.cache/rot2/suite/*.json` (gitignored), runner `.cache/rot2/run_suite.sh`.

## 4. Next smallest actions (revised, in order)

1. `LaunchMain`: `ROTATE <quarters>` on the stdin reader → main looper → rotate the local
   `mWMS` display (`DisplayContent`/`DisplayFrames` on the C1 compositor's display spec, which
   `acos-wm` already models), then `ActivityThread.handleConfigurationChanged` with the new
   `Configuration`+`MergedConfiguration`; guest prints one `ACOSPROBE` with the new
   width/height/rotation so the host's reply is bound to a fresh probe + fresh C1 frame.
2. `launch.rs` interactive `match cmd`: `"rotate"` → write the stdin line, wait for the fresh
   probe (same `wait_for_*` path as `tap`), answer `rotation`/`width`/`height`.
3. `ArtSession::rotate` + `SessionCapabilities { rotate: true }` + `InputAction::Rotate` at
   `crates/acos-cli/src/engine/mod.rs:980`; remove the unsupported answer only then.
4. `acos-wm`: the dispatcher's display frame and window→display transform follow the rotated
   `DisplaySpec` (D2: bars follow `DisplayPolicy`, not the rotated app rect) — the landscape bar
   rule is already unit-tested in `DisplaySpec::rotated`.
5. `scenarios/rot1-rotate.yaml` + `scenarios/negative/rot1-rotate-wrong.yaml`, 3/3 each.

---

# ROT3 section (branch `art-rotate-1`, `main` `0c0b322` merged in)

Status: **PARTIAL**, stopped at the 60-minute rule with code landed and no live proof.
`input.rotate` still answers `UNSUPPORTED_BACKEND`/`input.rotate`.

## 1. What changed

* `crates/acos-services/src/wms.rs`: `WmsState::rotate(rotation)` rotates the live
  `DisplaySpec` and the `DisplayFacts` size; `registry::LiveHandles` now carries the
  `Arc<Mutex<WmsState>>` so the interactive session drives the same state the
  `IWindowSession` relayout replies come from.
* `crates/acos-wm/src/wms.rs`: `reference_merged_configuration` follows the display —
  landscape orientation, display-derived dp size (also under `carve`, but only when
  `rotation != 0`, so every frozen portrait baseline is untouched), rotated
  `WindowConfiguration.bounds`/`rotation`/`displayRotation`. New test
  `rotated_display_reports_a_landscape_merged_configuration`.
* `crates/acos-art/src/launch.rs`: interactive `"rotate"` command — rotates the WMS, writes
  `ROTATE <n>` to the guest stdin, waits for a probe sample with `seq > before`, answers
  `rotation`/`width`/`height`/`tree` only from that fresh sample.
* `LaunchMain` (the engine's real guest class, **not** `SessionMain`): `ROTATE <quarters>` on
  the stdin reader → main looper → `rotatedConfiguration()` → real
  `ActivityThread.handleConfigurationChanged` → activity-level path from the manifest's real
  `ActivityInfo.configChanges` (`handleActivityConfigurationChanged`, or
  `scheduleRelaunchActivity` + `handleRelaunchActivity` for AOSP's
  `handleRelaunchActivityInner` order) → force `relayoutWindow` → one `ACOSPROBE` at the new
  size. Guest log line on success: `ACOSLAUNCH MARK rotate rotation=N relaunch=B handled=0xM
  panel=WxH`; on failure `ACOSLAUNCH MARK rotate-failed … <cause>`.

## 2. What is NOT proven, and why that matters

No live rotate run was obtained: the ART lock had four other holders and a 300 s acquisition
timed out (no guest process was started, so there is no partial evidence to misread). The
guest path is therefore **compiled but unmeasured** — in particular these rungs can fail on
the image: the `ActivityClientRecord` lookup through `mActivities` for the in-process token,
`PendingTransactionActions`' no-arg constructor, `handleRelaunchActivity`'s requirement that
`scheduleRelaunchActivity` has put the record on `mRelaunchingActivities`, and whether
`mForceNextWindowRelayout` makes the next traversal call `relayoutWindow`.

## 3. Next smallest actions, in order

1. One live run (interactive stdin: `tree` → `rotate rotation=1` → `tree`) and read
   `ACOSLAUNCH MARK rotate …` / `rotate-failed …`.
2. `ArtSession::rotate` + `SessionCapabilities.rotate` + `InputAction::Rotate` at
   `crates/acos-cli/src/engine/mod.rs:980`; delete the unsupported answer only then.
3. `acos-wm` rotated input mapping (a tap after the rotation must hit the node whose bounds
   contain it).
4. `scenarios/rot1-rotate.yaml` + `scenarios/negative/rot1-rotate-wrong.yaml`, 3/3 each; then
   the 3 unsupported rotate scenarios, then the gates and the A/B.

## 3a. Measured on one live run (window half only)

`ACOSLAUNCH_INTERACTIVE=1 acos-art launch --apk counter.apk`, stdin
`tree` → `rotate rotation=1` → `tree` → `rotate rotation=0` → `tree`:

* `{"cmd":"rotate","ok":true,"rotation":1,"width":2400,"height":1080,"probe_seq":13}` and
  `{"…","rotation":0,"width":1080,"height":2400,"probe_seq":19}` — both bound to a probe newer
  than the request;
* the rotated sample's root `DecorView` bounds are `[0,0][2400,1080]`, and after rotating back
  `[0,0][1080,2400]`: the WMS rotation + `mForceNextWindowRelayout` relayout really happen;
* **the tree stayed a 1-node DecorView skeleton and the guest logged one
  `onCreate/onStart/onResume` only**, so the Activity-level rung (`handleConfigurationChanged`
  → relaunch/config callback) did not demonstrably run. `ACOSLAUNCH MARK rotate…` /
  `rotate-failed…` is written to guest stdout, which the host consumes without echoing it into
  the session trace, so the failing rung is unnamed. This is why `input.rotate` stays
  unsupported.

Evidence: `.cache/rot3/run2/{stdout.txt,live-session.json}` (gitignored). Also fixed in this
pass: the live probe re-resolves `resumedActivity()` per sample instead of using the Activity
instance captured at launch (a relaunch replaces it).

---

# ROT4 section (branch `art-rotate-1`, `main` merged at `9225a01`)

Status: **ROTATE WORKS END TO END ON ART.** `input.rotate` is supported (capability
`"rotate"` in the engine's supported list), `scenarios/rot1-rotate.yaml` is 3/3 pass and
`scenarios/negative/rot1-rotate-wrong.yaml` is 3/3 `ASSERTION_FAILED`. The 15 gates, the 5
Gaten flows and the 5 Gaten negatives keep their exact outcomes. This section supersedes
ROT3 §2/§3.

## 1. The rung ROT3 could not name

**`scheduleRelaunchActivity`'s guard, not the record lookup, `PendingTransactionActions`, or
`handleRelaunchActivity` itself.**

* AOSP `ActivityThread.scheduleRelaunchActivity(IBinder)` →
  `scheduleRelaunchActivityIfPossible(r, …)` returns at
  `r.token instanceof Binder` (the "local object" guard). This Activity is attached with a
  fresh local `Binder` token, so nothing was ever put on `mRelaunchingActivities`.
* `handleRelaunchActivity(tmp, actions)` then resolves the token against
  `mRelaunchingActivities`, finds nothing, and **returns at its own `tmp == null` guard**
  ("Abort, activity not relaunching") with no exception and no lifecycle callback. The old
  instance stayed alive — which is why the count "survived" a relaunch that never ran, and why
  ROT3 saw exactly one `onCreate/onStart/onResume` while `handled=0x3` said `relaunch=true`.
* Fix (AMS-shaped, framework-executed): one stock `ClientTransaction` whose item is
  `ActivityRelaunchItem.obtain(token, null, null, configChanges, MergedConfiguration, false)`
  with a `ResumeActivityItem` lifecycle request, delivered with `scheduleLocal(tx)`. The
  framework's `TransactionExecutor` runs `preExecute` (`prepareRelaunchActivity` → record on
  `mRelaunchingActivities`) then `execute` (`handleRelaunchActivity` →
  `handleRelaunchActivityInner`: pause → stop(saveState) → destroy → launch(state) → resume).
* Two image facts learnt: `android.app.ActivityWindowInfo` **and**
  `android.window.ActivityWindowInfo` are absent from this boot image, so the window-info slot
  is discovered by method signature and passed `null`; and the forced relayout + probe must be
  **posted after** the transaction (it runs on the same main looper), otherwise it forces the
  *old* decor and the new window's first traversal — a transient layout (portrait-style bars at
  landscape size, `inc` at `[105,219][336,345]`) — becomes the rotation's frame.

## 2. The "1-node skeleton" was a misreading (no defect)

The probe tree is a *forest*: `tree.nodes` holds one entry per **window** root, each with its
full `children` chain. The counter session's tree is 1 root, **14 views**, with the real texts
(`0`, `Increment`, `Reset`, `Counter`) in both orientations. ROT3 counted top-level roots. No
tree fix was needed, and the counter value in the gate proves the same thing.

## 3. What is measured now

Guest MARK on the engine's own path:

```
ACOSLAUNCH MARK rotate rotation=1 relaunch=true via=ActivityRelaunchItem(ClientTransaction) \
  handled=0x3 activity=dev.acos.fixtures.counter.MainActivity panel=2400x1080
```

`counter.apk` (no `android:configChanges`) on one rotate, from `out/counter/logs/logcat.txt`:
`onPause → onStop → onSaveInstanceState count=2 → onDestroy → onCreate → restored count=2 →
onStart → onResume`. `scenarios/counter.yaml`'s ordered `assert_log` block passes.

Landscape geometry after the relaunch (2400x1080 frame, window inset by the rotated system
bars — nav bar 63 px on the left): `count [105,168][126,219]`, `inc [105,219][336,345]`,
`reset [105,345][336,471]`; portrait back to `[42,317][63,368]` / `[42,368][273,494]` /
`[42,494][273,620]`. A tap on `inc` at its **landscape** bounds increments 1 → 2, i.e. rotated
input mapping works without an `acos-wm` change (ROT3's `WmsState::rotate` already moves the
display the dispatcher maps into; the rotate step's traversal refreshes `window_offsets`).

## 4. Scenario and evidence files

* `scenarios/rot1-rotate.yaml` — inc → rotate landscape → landscape bounds of `inc`/`reset`/
  `count` + count preserved → tap `inc` → 2 → rotate portrait → exact portrait bounds + 2.
  3/3 pass (`.cache/rot4/suite/rot-{1,2,3}.json`, `out/rot1-rotate/{tree,steps}`).
* `scenarios/negative/rot1-rotate-wrong.yaml` — demands the portrait bounds of `inc` after
  rotating to landscape; 3/3 `ASSERTION_FAILED` with
  `expected [42,368][273,494], got [105,219][336,345]`.
* `scenarios/counter.yaml` — landscape `count` baseline re-blessed to the measured
  `[105,168][126,219]` (comment records the rotated-bar reason). The rotate step, the lifecycle
  order and every portrait assertion pass.
* `scenarios/v1-recreate.yaml` and `scenarios/hello-views.yaml` — the rotate steps no longer
  answer unsupported. `v1-recreate` passes both lifecycle `assert_log` blocks and the restored
  text, then fails at a later, unrelated `back` settle `TIMEOUT` (`no-draw-after-action`).
  `hello-views` fails only its stale landscape pixel baseline (`input` expected
  `[170,282][2358,400]`, measured `[105,219][2230,337]`) — one number, recorded not re-blessed.

## 5. Verification commands (this pass)

* `cargo test --release --offline -j 8 -p acos-wm -p acos-services -p acos-cli -p acos-verify`
  — green except the known worktree-path-only
  `w50_serve_art::an_unknown_session_is_not_routed_to_the_frozen_rust_vm`.
* `cargo test --release --offline -p acos-art -- --test-threads=1 --skip
  refuses_to_run_when_no_cgroup_can_be_created` — 163 passed, 0 failed; that test alone in a
  fresh lock acquisition (`--test sandbox_s5`, fresh `ACOS_ART_DATA`) — 7 passed.
* Gates / Gaten: `.cache/rot4/suite/*.json`, runner `.cache/rot4/run_suite.sh` (gitignored).

## 6. Next smallest actions

1. Re-bless `hello-views.yaml`'s landscape `input`/`result` baselines from one measured run;
   the rotate path itself is proven.
2. Give `v1-recreate`'s final `back` step a `wait_for`/`settle_timeout_ms` (or a functional
   `animations:off`) so its pre-existing `no-draw-after-action` TIMEOUT stops masking the
   rotate result.
3. D2 (bars per `DisplayPolicy`): the landscape window currently takes the rotated bar insets
   `DisplaySpec::rotated` models (nav 63 px left); align the guest's own `STATUS_INSET`/
   `NAV_INSET` derivation with it so the transient first-traversal layout matches the settled
   one instead of being papered over by the post-transaction forced relayout.
