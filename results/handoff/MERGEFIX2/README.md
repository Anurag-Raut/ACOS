# MERGEFIX2 — the two v0.4.0 seams that must not be duplicated, and the scripts that ate evidence

Branch `mergefix2` from `merge040a` (`a819fd5c`), 2026-11-18. This file is a **resume**: the first
pass was paused by a host power-off at `c8e47fb1`; every number below was re-measured on the final
tip after the `sysui4` merge (`e826740b`), in this worktree, under the shared ART lock.

```
a819fd5c MERGE040A  (base: four module tracks merged, gaten first tap REGRESSED)
03589a01 MERGEFIX2  one launch, one delivering side (origin + process), build-fixtures.sh
4abc415a Merge branch 'sysui4' into mergefix2     (AOSP result ordering + its tests)
<final>  MERGEFIX2: evidence, the pre-granted A/B arm, the regenerated dexprog reference
```

Decision taken for both merge conflicts (the brief's rule): **both sides stay authoritative; the root
cause is fixed.** Nothing was reverted, no scenario was loosened, no test was marked `#[ignore]`.

Private run env for every ART run here: `ACOS_RUNTIME_DIR=$PWD/.cache/rt-mf2`,
`ACOS_SESSION_SOCKET=$PWD/.cache/rt-mf2/session.sock`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`,
`ACOS_ART_BIN=$PWD/.cache/mf2-target/release/acos-art`, all through
`tools/art-runtime/run-exclusive.sh --task MERGEFIX2 -- tools/guarded.sh --mem 6G -- <cmd>`.
`run-one.sh` (one scenario) and `run-acceptance.sh` (the corpus; one lock acquisition **per**
scenario) are the exact commands.

| Item | State |
|---|---|
| 1. gaten `*` first tap never settles after sysui3a | **DONE** — the exact sysui3a change is named (§1), the fix is one selector + one enum field, pinned by `crates/acos-services/tests/launch_origin.rs` (4 tests) |
| 2. `git merge sysui4` (AOSP result ordering + tests) | **DONE** — `4abc415a`, no conflict needed a hand decision (see §2), one implementation per seam |
| 3. `sysui1-permission-pregranted` | **PASS**, and the arm that changed it is measured: it is the merge040a lineage, **not** sysui4 and **not** this fix (§3) |
| 4. `tools/build-fixtures.sh` deleting tracked expectations | **DONE** — staging dir + publish-only-on-success (§4); 28/28 programs now build, including `28_sqlite_basic` |
| `cargo test -p acos-art -p acos-verify -p acos-services -p acos-wm -p acos-runtime -p acos-cli` | **PASS** (§5) |
| ART acceptance (19 scenarios) | **18 pass**, `gaten-bubble-overlay` reaches its screenshot step and fails there (§6) |

## 1. The gaten regression: one launch, one delivering side, by origin **and** by process

### 1.1 The exact change in sysui3a's host block

MERGE040A §5.1 bisected the merge to the sysui3a commit and proved the guest-side launch-completion
change was innocent, but the remaining host block (`ams.rs`/`atms.rs`/`peer.rs`/`client_transaction.rs`/
`host.rs`) was interdependent — no subset compiled, so it was left "needs a bisect inside it".

The culprit is one call, `crates/acos-art/src/launch.rs`:

```rust
// SYSUI3A: name the primary process the way `startProcessLocked` names it.
atms.set_primary_package(&package);          // tag 0 now answers `Peer::package_of(0)`
```

That is correct for its own purpose (a `startActivity` **into** the already-running primary — the
shade's `IIntentSender`, a notification's content sender — must reach it). What it also did was hand
tag 0 to the **other** half of the seam: `Peer::take_host_transactions` runs per *named* process and
encodes whatever `AtmsState::pending_launch_for(package)` returns as a host-built
`ClientTransaction`. Before the seed, `package_of(0)` was `None`, so that path never ran for the
primary. After it, the **guest's own** `startActivity` (Gaten's `LaunchMain.launchLocalActivity`,
which had *already* materialised the activity with the token it minted and adopted it) was delivered a
second time, as a second `MainActivity` — a second `Application`-level `React` root. The app
re-rendered every frame, so the strict settle could never bind: `settle.bound: "timeout"`,
`frame_ms: null`, pending `queue`/`newer-store` — exactly MERGE040A §5.1's measurement.

**Measured, same scenario, same metric** (`ReactNativeJS: Running "Gaten"` = one React root per line):

| tree | `gaten-backstack` | React roots | evidence |
|---|---|---|---|
| base `45a1feab` | pass | 1 | `MERGE040A/base45a1feab/gaten-backstack/` |
| +a11ypace `6954ded3` | pass | 1 | `MERGE040A/bisect-6954ded3/gaten-backstack/` |
| +sysui3a `8e367c28` | **fail** (timeout) | **2** | `MERGE040A/bisect-8e367c28/gaten-backstack/` |
| MERGE040A tip `623aaea4` | fail | 2 | `MERGE040A/corpus3/gaten-backstack/` |
| **this tip** | **pass** | **1** | `runs/gaten-backstack/` |

(`grep -c 'ReactNativeJS: Running "Gaten"'
 MERGE040A/bisect-{6954ded3,8e367c28}/gaten-backstack/art/logs/logcat.txt` → `1`, `2`;
 this tip's three gaten runs → `1`, `1`, `1`.)

### 1.2 The fix (root cause, for the whole class)

A launch is delivered by **one** side, decided by *who asked* and *which process*:

* `PendingLaunch.origin = Guest` — the guest's own `IActivityTaskManager.startActivity…`. The guest
  materialises it and adopts its own token (`ActivityManagerAdopt`), exactly as `ActivityThread` does
  after `scheduleTransaction`. The host must not mint a second token for it.
* `PendingLaunch.origin = Host` — `system_server` as the client (`start_activity_from_host`: the
  notification shade's row tap in `modules/notifications.rs`, the pending-intent `TYPE_ACTIVITY` path,
  the permission dialog's start). No guest `startActivity` exists, so only the host can deliver it.
* `AmsState::pending_launch_for_tag(tag, package)` is the one selector both delivery paths read
  (`Peer::take_host_transactions`, `AtmsState::take_launch_for_process`): **tag 0 (the primary) is
  offered only `Host` launches**; every spawned tag is `ACOSLAUNCH_HOST_SCHEDULES` and is offered every
  origin (MPROC7). `AmsState::adopt_launch_token` takes only `Guest` launches, so a shade launch stays
  queued for the peer instead of being bound to a token the guest invented.

Nothing was reverted: the shade, the permission dialog and the secondary-process scheduling keep
their host-built delivery (all green in §6), and the gaten scenarios settle again in ~11–14 s.

**The pin** — `crates/acos-services/tests/launch_origin.rs`, `cargo test -p acos-services --test
launch_origin`:

1. `a_guest_launch_of_the_primary_is_not_the_hosts_to_deliver` — fails before the change (the
   pre-fix selector answers `Some`), passes after;
2. `a_host_launch_of_the_primary_is_the_hosts_to_deliver` (the shade/notification path stays);
3. `a_guest_launch_of_a_spawned_secondary_is_the_hosts_to_deliver` (MPROC7 unchanged);
4. `the_primary_seed_does_not_make_the_host_deliver_the_guests_launch` — the exact sysui3a
   interaction: after `set_primary_package`, `package_of(0)` answers and the host path runs, and it
   must still not encode the guest's own launch.

The suite is state-level (no ART), so it fails deterministically instead of waiting for a 20 s settle
timeout. Measured A/B of the same selector on real ART (`runs/gaten-selector-reverted/`): the tip with
**only** `pending_launch_for_tag`'s origin/process rule reverted, rebuilt, fails `gaten-backstack`
again — `status=timeout`, `failed_step: tap`, `settle.bound: "timeout"`, pending reason `queue`,
1 of 2 guest acks — and `runs/armA/gaten-backstack/` (merge040a+sysui4, the whole MERGEFIX2 src
change reverted) fails the same way with **2** React roots. Restoring the rule turns both green, so
the pin and the ART behaviour are the same fact.

## 2. `sysui4` merged: the result delivery is AOSP's

`4abc415a` merges `e826740b` (`SYSUI4`). The seams that both sides touched:

* `crates/acos-services/src/atms.rs` — SYSUI4 replaces "superseded pause/stop withdrawn,
  `notifyAppResumed`, stop-for-resumed ignored" ordering rules. MERGEFIX2 touched the same file only
  to add `PRIMARY_PROCESS_TAG`, `pending_launch_for(tag, …)` and the launch bookkeeping; the merge
  kept **both** and the file compiles with one implementation per seam (no duplicated delivery rule:
  `deliver_result` is SYSUI4's, `pending_launch_for_tag` is MERGEFIX2's, and neither calls the other's
  selector).
* `crates/acos-services/tests/mproc5_top_of_task.rs` — updated by SYSUI4 for the typed queue.
* New: `crates/acos-runtime/tests/activity_result_delivery.rs` (offline `ActivityThread` reader) and
  `crates/acos-services/tests/sysui4_result_ordering.rs` (3 ordering tests). Both green (§5).

## 3. `sysui1-permission-pregranted`: it passes, and what made it pass is **not** this fix

`SYSUI4` left it failing on its own tree — the host delivered exactly one
`ActivityResultItem + ResumeActivityItem` to the caller and the guest executed it "with no callback"
(`SYSUI4/pregranted*/`, logcat ends at `AcosPermProbe: requesting code=7`). On this tip the same run
goes
 through:

```
AcosPermProbe: requesting code=7
… (pause/stop, the result transaction)
AcosPermProbe: onRestart / onStart / result request=7 n=2 m=2 CAMERA=0 LOCATION=0 / onResume
```

(`runs/sysui1-permission-pregranted/art/logs/logcat.txt`; scenario `pass`, 1.4 s.)

**Which change made it pass — measured, and it is not MERGEFIX2's launch fix.** I built the
pre-fix / pre-merge arms as separate release binaries (`.cache/nofix-target`, each arm a build +
one 2 s scenario run, same private runtime dir) and ran the identical scenario:

| arm (source) | `sysui1-permission-pregranted` | logcat | evidence |
|---|---|---|---|
| `e826740b` sysui4 on sysui3a's own branch (SYSUI4's tip) | **fail** | ends at `requesting code=7` | `SYSUI4/pregranted/` (that track's run) |
| `8e367c28` merge040a 3/4 (+sysui3a, **before** sysui4 and before the fix) | **pass** | `result request=7 n=2 m=2` | `runs/armC/` |
| `a819fd5c` merge040a tip (no sysui4, no fix) | **pass** | `result …` | `runs/armB/` |
| merge040a **+ sysui4**, MERGEFIX2's src change reverted (`git revert 03589a01`) | **pass** | `result …` | `runs/armA/` |
| same arm with the §1 selector reverted only | **pass** | `result …` | (see §1 note) |
| same arm with REL3I3's delivery wake also removed from `launch.rs` | **pass** | `result …` | `runs/armE/` |
| **this tip** | **pass** | `result …` | `runs/sysui1-permission-pregranted/` |

So the pre-granted arm stops failing as soon as sysui3a's content is merged **into the modules
lineage** (`8e367c28`), and it passes with or without sysui4's ordering work and with or without this
brief's launch fix. The remaining delta between the failing tree and `8e367c28` is the whole
modules lineage (139 files: MERGEFIX, merge030's launch/session wake, a11ypace, wvfair) plus the
two kept-both resolutions of MERGE040A §1.3/§1.4; the two single-change probes above (the launch
selector, the delivery wake) do **not** reproduce it, so naming the one hunk needs a bisect over that
139-file delta — out of this brief's scope and recorded here so nobody re-derives it.

What *is* pinned: the same binary proves the launch fix's causality for the gaten regression
(`runs/armA/gaten-backstack/` fails at step 1 with `settle.bound: "timeout"`, reason `queue`, and
**2** `ReactNativeJS: Running "Gaten"` roots, vs `runs/armA`'s sibling tip run passing with 1).

## 4. `tools/build-fixtures.sh` no longer deletes tracked expectations

Before: the script `find "$EXPECTED_DIR" -maxdepth 1 -type f -delete`d the checked-in references
first, then looped over the programs; `28_sqlite_basic`'s `javac` failed (no `android.jar` on the
classpath) and the script aborted with **every** `fixtures/expected/dexprogs/*` already gone (the
MERGE040A and every-worktree "run `git checkout -- fixtures/expected`" workaround).

After (`03589a01`):

* the references are generated into `fixtures/build/expected-staging` and copied over **only after
  every program compiled, ran and dexed** — a failure anywhere leaves the tracked files untouched;
* `javac -cp $ANDROID_JAR` (the same jar `tools/build-apps.sh` resolves) is what makes
  `28_sqlite_basic` compile at all — the fixture still cannot *run* on the host JVM, so its recorded
  reference stays the JVM's own "Unable to initialize main class" answer (exit 1), which is what
  `tools/oracle/run-framework-dexprog-art.sh` compares against on ART;
* an expectation with no fixture source is reported, not deleted.

Measured on this tip: `tools/build-fixtures.sh` → **`built 28 programs`**, `rc=0`, and
`git status --short fixtures/` shows **no deletion** — the only difference is
`fixtures/expected/dexprogs/25_security_digest.stdout`, which the script regenerated because the
checked-in file was stale (the fixture prints its NIST/FIPS-verified second half — SHA-224/384/512,
HmacSHA1, AES-128/256-ECB, AES-128-CBC/CTR — and those deterministic vectors were missing from the
reference). That regeneration is committed; it is the only tracked file the script touched.

## 5. Crate tests

`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`, `CARGO_PROFILE_{DEV,TEST}_DEBUG=0`, `CARGO_INCREMENTAL=0`, all
under one `run-exclusive` acquisition (log: `cargo-tests-1.log`, `cargo-tests-2.log`):

| command | result | log |
|---|---|---|
| `cargo test --no-fail-fast -p acos-services -p acos-verify -p acos-wm` | **PASS** — 93 test binaries, 0 failed, 0 ignored-but-skipped (incl. `launch_origin` 4/4, `sysui4_result_ordering` 3/3) | `cargo-tests-1.log` |
| `cargo test --no-fail-fast -p acos-art -p acos-runtime` | **PASS** — 50 test binaries, 0 failed; **no** `live_view` socket-path failures (the short `ACOS_VIEW_TEST_DIR` is what fixes the pre-existing environmental 7) | `cargo-tests-2.log` |
| `cargo test --no-fail-fast -p acos-cli` | **PASS** — 25 test binaries + lib/doc, 0 failed, rc=0 | `cargo-tests-3.log` |

(`grep -c "test result: ok"` → 93 / 50 / 26; `grep "test result: FAILED"` → none.)

## 6. ART acceptance (`runs/`, one lock acquisition per scenario)

`results/handoff/MERGEFIX2/run-acceptance.sh` — 19 scenario runs, 18 pass, 1 honest fail:

```
gaten-backstack               pass  12s     sysui1-permission-allow       pass  2s
gaten-lesson                  pass  13s     sysui1-permission-deny        pass  6s
gaten-bubble                  pass  10s     sysui1-permission-pregranted  pass  1s
gaten-bubble-overlay          FAIL step 21 (screenshot record) — §6.1
a11y-probe                    pass   5s     sysui1-shade                  pass  1s
mproc3-two-process            pass   7s     notif-perm-granted            pass  0s
back-layers                   pass   4s     notif-perm-denied             pass  0s
form-validation               pass   7s     sensors-scripted/off          pass 1s/0s
location-attach               pass   2s     location-geocoder             pass  0s
location-detach               pass   2s
```

### 6.1 `gaten-bubble-overlay`: it reaches the screenshot step, and the wall behind it is real

The scenario's steps 1–20 pass (module enable → `service_ctl accessibility enable` → onboarding →
Settings → the "Floating button" switch → `assert_windows` `[application, accessibility_overlay,
accessibility_overlay]` → the bubble's `content_desc` visible, the remove target not visible → the tap
on "Screen lookup", `handled: [true, true]` at (986, 1152), `settle: fast`, `expect_change: false`).
It then **fails at step 21**, the `service_ctl accessibility status` whose `expect` is
`screenshots: [{display_id: 0, status: 0}]`: the reply's `screenshots` is `[]`
(`runs/gaten-bubble-overlay/verify.json`, `failed_step.index 21`). The expectation is left exactly as
strict as it was — no wait was inserted into the tracked scenario and nothing was loosened.

Two facts about that wall, both measured:

* the accessibility module's screenshot path itself works on this tree — `a11y-screenshot-wrap`
  passes (also run here), so `takeScreenshot` is not broken by the merge;
* Gaten's lookup never *asks* for the screenshot within the tap's `settle: fast` window: with a
  `wait: {ms: 3000}` **added to a copy** of the scenario (`gaten-bubble-overlay-wait.yaml`,
  `runs/gaten-bubble-overlay-wait/`), the module logs `ACOSA11Y takeScreenshot success display=0
  1080x2400 stride=1080` and the guest then dies: `SIGABRT` (signal 6) on **RenderThread**,
  `__android_log_assert` in `/data/shim/libartshim.so` (`art/art-frames/native-crash.txt`), i.e. the
  gralloc-less, non-hardware-accelerated-canvas wall the scenario's own header documents
  ("RippleDrawable … not supported for a non-hardware accelerated Canvas" is in the same logcat). The
  run ends as `GUEST_EXITED`/`GUEST_KILLED_BY_SIGNAL` before the status query.

So the screenshot step is reached and the honest expectation fails there; giving it time converts the
failure into the documented guest crash rather than into a pass. **Left for the accessibility/GPU
track** (the `HardwareBuffer`/`getAllocationSize` gap in `results/handoff/GATENBOOT2/README.md` §5).

## 7. Evidence map

```
results/handoff/MERGEFIX2/
  README.md                 this file
  run-one.sh                one scenario in a private runtime dir (ACOS_ART_BIN included)
  run-acceptance.sh         the 19-scenario corpus, one lock acquisition per scenario
  runs/<scenario>/          verify.json, verify.err, art/{engine.json,events.jsonl,logs/logcat.txt,
                            launcher.stderr.log,art-frames/*,verdict.json,steps/}
                            (frames/ and live-frames/ PNGs trimmed to keep the commit small;
                             gaten-*/art/tree/ likewise — regenerate with --overwrite)
  runs/gaten-bubble-overlay-wait/   the wait arm: screenshot served, then the RenderThread crash
  runs/armA/                merge040a+sysui4 with MERGEFIX2's src change reverted: gaten-backstack
                            fails again (2 React roots), pre-granted still passes (§3)
  runs/armB/                merge040a tip a819fd5c (no sysui4, no fix): pre-granted passes
  runs/armC/                8e367c28 (merge040a 3/4, +sysui3a): pre-granted passes
  runs/armE/                8e367c28 with REL3I3's delivery wake removed: pre-granted passes
  runs/gaten-selector-reverted/  the tip with only the §1 selector reverted: gaten-backstack
                            times out at the first tap (`settle.bound: "timeout"`, reason `queue`)
  gaten-bubble-overlay-wait.yaml    the copy used by the wait arm (NOT the tracked scenario)
  cargo-tests-1.log, cargo-tests-2.log   the crate suites
```

## 8. What is still open (not this brief)

* `gaten-bubble-overlay` step 21 (§6.1) — accessibility/GPU track; the module serves the screenshot,
  the guest's `HardwareBuffer` wrap aborts on RenderThread.
* MERGE040A §5.2 `webview-probe` flake — not part of this acceptance list and **not re-run here**; no
  evidence for or against on this tip.
* the exact hunk that made `sysui1-permission-pregranted` pass is still unnamed (§3): it is somewhere
  in the modules lineage that sysui3a was merged into, and two single-change probes did not reproduce
  it.
