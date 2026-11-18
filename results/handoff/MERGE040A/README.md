# MERGE040A — the four v0.4.0 module tracks merged into `modules`

Branch `merge040a` from `modules` @ `45a1feab` (MERGEFIX). One commit per merge,
in the order the brief set, each conflict resolved to **one** implementation per
seam. This file lists, per conflict, what each side wanted and what the merged
tree does; then the verification and the two regressions the merge surfaces.

```
45a1feab modules (MERGEFIX)                      <- base
c05b4539 MERGE040A(1/4) a11ypace  73879689      (contains gatenboot2 6f146a46)
6954ded3 MERGE040A(2/4) wvfair    1237365f
8e367c28 MERGE040A(3/4) sysui3a   c2e9c9e9      (contains sysui1+sysui2 history)
623aaea4 MERGE040A(4/4) sysui3b   f89d2ad3      <- HEAD
```

`cargo check --workspace --all-targets`: green after every merge.

## 1. Conflicts and resolutions

### 1.1 `scenarios/a11y-probe.yaml` (merge 1, a11ypace)

Both parents made the async Back tap's log assertion wait, with the same intent
and different prose/values: MERGEFIX added `timeout_ms: 10000` under the
`expect_change: false` tap ("the app reads the published key on a later looper
iteration"), A11YPACE had `timeout_ms: 30000` and its own "~1 s guest
log-pipeline lag" note.

**Kept:** one `timeout_ms: 30000` (the larger — both are *waits*, a missing line
still fails at the deadline, so nothing is weakened) and one comment carrying
both reasons. No pattern added, removed or loosened.

### 1.2 `tools/build-apps.sh` (merge 3, sysui3a)

The pre-merge file carried **two** `for app in …` headers (a MERGE030 artifact:
the second, with `looper-wake`, shadowed the first and only the second loop body
ran). Consequence, measured after the merge and now fixed: `sensor-probe` and
`notif-perm-probe` were **never built** by `tools/build-apps.sh` — a stale
`fixtures/build` hid it. sysui3a's side added `permissioncontroller`, `perm-probe`
and `systemui` to that list.

**Kept:** exactly **one** loop, the **union** of both sides —
`counter todo-list multi-activity finish-start form-validation settle-lab
thread-heavy svg-viewbox offscreen-pager looper-wake sensor-probe
notif-perm-probe permissioncontroller perm-probe systemui` — plus a comment
recording why there must never be two. Deleting either side's names loses
fixtures (13 apps would stop being built), so neither side's list survives
verbatim.

### 1.3 `crates/acos-art/src/launch.rs` (merge 3)

- **modules/parent:** REL3I3's session **delivery wake** (`set_delivery_wake` +
  `wms.attach_delivery_wake`).
- **sysui3a:** SYSUI3A's **`seed_primary_package(&package)`** — the primary
  process (tag 0) is named the way `startProcessLocked` names it, so a
  `startActivity` into the already-running primary is delivered.

**Kept: both**, in that order (they are distinct facts, not two spellings of
one). Everything else in the file auto-merged, including MERGEFIX's single
`ProbeState::for_session(top)` (re-checked, §2).

### 1.4 `crates/acos-services/src/atms.rs` (merge 3)

- **modules/parent:** REL3I3's `on_back_pressed(token, callback)` — the server
  half of API 34's back path, returning the queued `IRequestFinishCallback`.
- **sysui3a:** SYSUI1/SYSUI2's result appliers — `apply_permission_dialog_result`
  (AOSP extras → the one runtime-permission table) and `apply_system_ui_result`
  (the module's shade sink → `IIntentSender.send`).

**Kept: both.** Two different entry points into the same state; keeping one
would drop either the framework's back path or the dialog/shade result path.

### 1.5 `crates/acos-verify/src/executor.rs` (merge 3)

`eval_tap_change` — the one place the tap contract is decided — was touched by
both tracks:

- **a11ypace:** `expect_change: false` means "my effect is transient"; the step
  claims **delivery** instead of a state change and records
  `change_claimed: false` (never a silent pass: no delivery record ⇒
  `UnsupportedAssertion`, undelivered ⇒ `NoFreshFrame`).
- **sysui3a:** `settle.handled == "window-removed"` is a **completed
  interaction** whose evidence is the removal; the backend only publishes it for
  a delivered, guest-handled gesture whose target window it recorded before and
  no longer records (`removed_window_handled`, with its negative guard).

**Kept: both, in this order:** (1) `expect_change: false`, (2) window-removed,
(3) the shared `handled: false` unhandled shortcut. The precedence the brief
pinned — **host window removal > unhandled shortcut** — is untouched: the
window-removal branch is still above the `handled == Some(false)` path, and the
`expect_change: false` branch only fires when the *scenario itself* opted in.
The test module is the union of both sides' tests: SYSUI3A's
`a_handled_window_removal_passes_the_tap_contract` was adapted to the widened
`InputDelivery` (`handled`, `published`, `window_removed`, `target`) and to the
4-argument signature (`…, None`), keeping both of its claims (the removal passes;
a bare settle timeout still fails).

### 1.6 `crates/acos-cli/src/engine/art.rs` (merge 3)

Both sides appended tests at the top of `mod tests` (REL3H2/REL3A's
settle-reason-and-capture refusals vs SYSUI3A's settle-rule helpers and
`a_tap_that_closed_the_window_it_landed_on_is_handled`).
**Kept: both**; SYSUI3A's `delivered(handled)` helper now spells out the fields
REL3E/REL3I3 added (`handled`, `published`, `window_removed`, `target`) instead
of relying on a default.

### 1.7 `DECISIONS.md` (merge 3)

Both sides appended ADR notes at the same anchor (NETSHARE/NET3/WVB/MODSENS/
MPROC4-amendment vs SYSUI1/3A notes). **Kept: both**, unchanged — different
tracks, different decisions.

### 1.8 merge 4, sysui3b — no textual conflict

sysui3b's permission-label/PMS additions (`permission_catalog.rs`,
`PermissionStore::permission_description`, `getPermissionInfo`/
`getPermissionGroupInfo`, `InstalledPackage::declared_permissions`) sit **beside**
SYSUI1/2/3A's launch/guard/result path rather than on top of it, so both survive:
`registry.rs` still fills install-time grants **and** the package's own
`<permission>` declarations from the one `PermissionStore` the dialog path
mutates (`set_install_time` + `set_declared_permissions`, one store, one
registry factory).

## 2. Merge-seam audit (the MERGEFIX lesson: `cargo check` cannot see a shadowed duplicate)

Checked by hand after each merge, in the seams that hurt last time:

| seam | result |
|---|---|
| `ProbeState` construction (`crates/acos-art/src/launch.rs`) | **one**: `let probe_state = Arc::new(…ProbeState::for_session(…))`; no `ProbeState::default()` shadow in the launcher (only in tests) |
| `executor::log_scope_start` | **one** definition; both the wait and the assertion call it |
| PMS state factory (`crates/acos-services/src/pms.rs`, `registry.rs`) | **one** `PmsState::new()` in the registry's `package` service closure; one `PermissionStore::new()` shared with ATMS and the `permissions` module |
| registry wiring (`registry.rs`) | one `host.register(…)` per service name (`activity`, `package`, `notification`, `webviewupdate`, …); no name twice |
| adjacent duplicate `let` in the merged hot files (launch/atms/ams/host/registry/wms/pms/executor/art.rs/a11y_bind/module/input_dispatch/misc) | only legitimate rebindings (`let intent = …` then `let intent = match permission_request(&intent)`, `let ime = shared(); let mut ime = ime.lock()`), no shadowed second construction |
| landed-additions check (every non-comment line each branch added, searched in the merged tree) | the only "missing" lines are the two intentional ones above (the 4-arg `eval_tap_change` call sites in the adapted SYSUI3A test, the unified fixture list, and MERGE030's stale one-line note) |

Settle precedence: unchanged, §1.5.

## 3. Schemas

`UPDATE_SCHEMA=1 cargo test -p acos-protocol --test schemas` → `schemas_are_up_to_date`
and `every_command_has_a_request_schema` pass with **no diff** (a11ypace's
`expect_change` was already checked in on its branch). `cargo test -p
acos-protocol` re-verifies freshness in the sweep below.

## 4. Verification

Worktree env per the common brief: `ACOS_FONT_DIR`, `.cache/boot/android-34`
(+ `art`, `gaten`) symlinks, private `ACOS_RUNTIME_DIR=$PWD/.cache/rt-merge040a`,
`ACOS_SESSION_SOCKET=…/session.sock`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`.
Fixtures rebuilt from scratch (`tools/build-apps.sh`, 109 APKs, then
`tools/build-dogfood-apps.sh`); every ART run under
`tools/art-runtime/run-exclusive.sh --task MERGE040A` + `tools/guarded.sh`.
Builds: `CARGO_PROFILE_{DEV,TEST,RELEASE}_DEBUG=0 CARGO_INCREMENTAL=0`,
`CARGO_TARGET_DIR=$WT/.cache/merge040a-target`.

### 4.1 `cargo check --workspace --all-targets`
Green on `623aaea4` (`Finished dev profile … target(s)`).

### 4.2 Non-ART sweep (`nonart-tests.log`, `nonart-rerun.log`)
`cargo test --no-fail-fast -p acos-art -p acos-verify -p acos-protocol
-p acos-services -p acos-wm -p acos-binder-peer -p acos-mcp -p acos-cli
-p acos-webview -p acos-netd`: **183 test binaries, 1851 passed / 8 failed** on
the first pass; the 3 fixture-dependent failures went green after the fixtures
were built (`nonart-rerun.log`: 99 binaries, 826 passed, then
`apk_facts_cache` green once `tools/build-dogfood-apps.sh` had produced
`dialogs-menus.apk`).

The 5 remaining failures are **all in `crates/acos-art/tests/cleanup.rs`** and
they do not run in this worktree, for an environment reason, not a merge one:
that suite seeds `/data` under `<repo>/.cache/h1-tests/<tag>-<pid>/data`, and
this worktree's path is 53 characters long, so the launcher's unix socket path
exceeds `SUN_LEN`. Reproduced directly:

```
$ .cache/merge040a-target/debug/acos-art render-app --data $PWD/.cache/h1-tests/render-manual/data …
acos-art: io: path must be shorter than SUN_LEN          # rc=2, 0.08 s (what the test sees)
$ .cache/merge040a-target/debug/acos-art render-app --data $PWD/.cache/h1-manual2/data …
frame=…/out/frame.png … nodes=13 1080x2400@420           # rc=0
```
Nothing in the merge touches the socket path, the scratch naming or these five
tests.

### 4.3 ART acceptance corpus — `corpus3/` (final tree `623aaea4`, runner `run-acceptance.sh`)

`corpus3.log` holds the one-line summary per scenario; `corpus3/<name>/` holds
`verify.json` + the run's `art/` (verdict.json, steps, logs, and — for the
failing arms only — the live-frame PNGs).

**PASS (28):** `a11y-probe`, `mproc3-two-process`, `back-layers`,
`back-layers-ime`, `form-validation`, `looperwake-bg-post`,
`rel3h-unhandled-input`, `rel3h2-expect-handled-false`, `touchdisp-dispatch-probe`,
`touchdisp-gesture`, `touchdisp-posted-side-effect`, `webview-probe` (!),
`webview-fair`, `sysui1-permission-allow`, `sysui1-permission-deny`,
`sysui1-permission-pregranted`, `sysui1-shade`, `notif-granted`, `notif-denied`,
`location-attach`, `location-geocoder`, `location-detach`, `sensors-scripted`,
`sensors-off`, `tail-telephony`, `tail-biometric`, `tail-bluetooth`,
`tail-vibrator`, `a11y-screenshot-wrap`.

**FAIL (5):**
| scenario | verdict | attribution |
|---|---|---|
| `gaten-backstack`, `gaten-lesson`, `gaten-bubble`, `gaten-bubble-overlay` | `TIMEOUT` at the **first onboarding tap** (`Set up Gaten`), settle pending `queue` (or `newer-store` with a longer deadline) | **regression introduced by the sysui3a merge** — bisected, §5 |
| `tail-detached` | `TIMEOUT` `wait_for text*="TELEPHONY unavailable=NullPointerException…"` | **pre-existing**: identical failure on the base tree (`tail-detached-base/`, same step, same selector) |

**Negative twins (expected to fail, and they fail for the right reason):**
`rel3h-expect-change` → `ASSERTION_FAILED` + `details.reason: expect_change` on
`settle.pixels_changed: false`; `rel3h2-expect-handled-true` → `INPUT_NOT_HANDLED`
+ `details.reason: expect_handled`. Their expectations were not touched.

### 4.4 `sysui1-permission-pregranted` — the brief expected a failure; it passes

The brief said to keep its expectation honest and not weaken it. On this merged
tree it **passes, three runs out of three** (`corpus1`, `corpus3`, plus the
`/tmp` rerun), with the dialog arms, so nothing was weakened and nothing was
changed. The most likely reason is SYSUI2/3A's launch-completion fix
(`launchedActivity`/`mCalled` + `finish_during_create`), which is exactly the
"controller answers and finishes inside `onCreate`" case the pre-granted arm
exercises; SYSUI4 owns that item and should re-check the expectation rather than
inherit the old one. This is *not* a fake pass: the scenario's own assertions
run (dialog arms unchanged, `corpus3/sysui1-permission-pregranted/art/verdict.json`).

### 4.5 `gaten-bubble-overlay` and the known GPU wall
The brief expected a possible failure at the screenshot-record step (GPU wall,
owner decision pending). On the merged tree the scenario never reaches that
step: it fails earlier, at step 5's onboarding tap, for the reason in §5. The
scenario's expectations were **not** touched.

## 5. Regressions this merge surfaces (need a decision)

### 5.1 The gaten onboarding tap no longer settles (deterministic) — sysui3a merge

`gaten-backstack`, `gaten-lesson`, `gaten-bubble`, `gaten-bubble-overlay` fail at
their first tap in 3/3 runs (`corpus1`, `corpus2`, `corpus3`). The tap itself is
delivered and handled (`handled: true`, 2 guest acks); the **strict settle**
never completes: `settle.bound: "timeout"`, `frame_ms: null`,
`first_change_ms: 558`, `pixels_changed: true`, last pending reason `queue`
(main-looper message due inside the horizon) — with `settle_timeout_ms: 20000`
it still fails, at 40 s, with pending `newer-store`
(`probe-settle/`). Pre-merge, the same tap settled in 5.5 s and the scenario ran
to completion.

Bisected by rebuilding each merge commit and running the identical scenario
(`.cache/bisect-<sha>` binaries, `bisect-<sha>/` evidence):

| tree | `gaten-backstack` |
|---|---|
| base `45a1feab` | **pass** |
| `c05b4539` (+a11ypace) | **pass** |
| `6954ded3` (+wvfair) | **pass** |
| `8e367c28` (+sysui3a) | **fail** (timeout) |
| `623aaea4` (+sysui3b) | fail (timeout) |

Narrowed further, all reverted-then-rebuilt on the merged tree:

* SYSUI3A's **guest** launch-completion change (`launchedActivity`/`mCalled`,
  `finish_during_create`) is **not** the cause: reverting only
  `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` still fails
  (`exp-no-launchedActivity/`). The merged run's launcher log shows the normal
  `activity-resumed … resume_ready=first-traversal` marker, no
  `finish_during_create`.
* SYSUI3A's host-side service-layer changes are **interdependent** (reverting
  `ams.rs`/`atms.rs`/`client_transaction.rs` or `host.rs`/`registry.rs` alone no
  longer compiles), so the culprit is somewhere in that block and needs a
  bisect inside it — not something to decide unilaterally here.
* sysui3a's own branch never ran the gaten scenarios (its tree predates
  gatenboot2: `gaten-*` there dies with `GUEST_EXITED` before the first frame,
  `bisect-c2e9c9e9/`), so this interaction was never covered on either side.

**Smallest next action:** on a tree that is `6954ded3` + sysui3a's *service*
changes only, run `scenarios/gaten-backstack.yaml` and split that block
(`peer.rs` package_of → `ams.rs` process start → `atms.rs` transactions); the
settle's own pending reasons (`queue`, `newer-store`) point at the guest's main
looper being kept busy, not at input delivery.

### 5.2 `webview-probe` — flaky, and it fails deterministically on the a11ypace lineage

`webview-probe` **passed** in `corpus3` (all steps), after failing the same
scenario at step 8 (`assert_log`: no `frame decoded view=1`) in `corpus1` and
`corpus2` — so on the tip it is a flake (WVFAIR documents the same flake class
and their "wait for each view's first frame" fix). What is *not* a flake is where
it fails per tree:

| tree | `webview-probe` |
|---|---|
| base `45a1feab` (older scenario: no per-view frame waits) | pass |
| `1237365f` (wvfair alone) | **pass, 20/20** (`bisect-1237365f/`) |
| `6954ded3` (+a11ypace) | **fail**, step 11 `ELEMENT_NOT_FOUND` (`bisect-6954ded3/`) |
| `8e367c28` (+sysui3a) | fail, step 8 (`view=1` never decoded) |

So the **a11ypace merge** loses the tap inside the WebView content on that tree
(1/1), and the **sysui3a merge** adds the missing second view's frame on top.
Both are input/frame-cadence interactions with A11YPACE's
`InputHost::publish_on_focused` and WVFAIR's fair-pull budget; nothing here was
"fixed" by weakening a scenario. **Smallest next action:** run
`scenarios/webview-probe.yaml` 5× on `6954ded3` and 5× on the tip to separate the
flake from the `ELEMENT_NOT_FOUND`, and if it reproduces, check
`InputHost::tap`'s target selection against a full-bleed WebView child.

## 6. Other findings (not from this merge, recorded so nobody re-diagnoses them)

* `tools/build-fixtures.sh` cannot build `fixtures/dexprogs/28_sqlite_basic`
  (its `javac` has no `android.jar` on the classpath: `package
  android.database.sqlite does not exist`, 26 errors). It aborts the script on
  the last program and **deletes** that fixture's checked-in expectations, so
  the script must be followed by `git checkout -- fixtures/expected`. Pre-existing
  on every branch that has the SQLite track; not in this brief's scope.
* `fixtures/build` needs **two** scripts now: `tools/build-apps.sh` (scenario
  APKs) and `tools/build-dogfood-apps.sh` (`dialogs-menus.apk` and the rest),
  or `acos-services`'s `apk_facts_cache` test fails with a missing file.

## 7. Evidence map

```
results/handoff/MERGE040A/
  README.md                     this file
  run-acceptance.sh             the corpus runner (36 scenario runs, --artifacts)
  corpus3.log, corpus3/         FINAL tree 623aaea4 (verdicts, steps, logs)
  corpus1.log, corpus1/         first run on the same tree (same failures)
  corpus2/                      gaten + tail-detached + webview-probe rerun
  base45a1feab/                 base tree: gaten-backstack PASS, webview-probe PASS,
                                gaten-bubble GUEST_EXITED (predates gatenboot2)
  bisect-c05b4539/ …            per-merge-commit rebuilds + runs
  bisect-1237365f/              wvfair alone: webview-probe PASS 20/20
  exp-no-launchedActivity/      guest launch-completion reverted: still fails
  exp-peer/, exp-ams/, exp-host/  subset reverts that no longer compile (kept: they
                                prove the sysui3a service block is interdependent)
  probe-settle/                 20 s deadline arm: still fails (40 s, newer-store)
  tail-detached-base/, tail-detached-merge/   the pre-existing tail-detached failure
  nonart-tests.log, nonart-rerun.log          the crate sweep
```
