# SYSUI1 — system UI from guest system apps: the permission dialog (PARTIAL)

Branch `sysui1` (from `modules` `7ad8dcb0`), 2026-11-17. All ART runs are private
(`ACOS_RUNTIME_DIR=$PWD/.cache/rt-sysui1`, `ACOS_SESSION_SOCKET=$PWD/.cache/sysui1-session.sock`)
and every one went through `tools/art-runtime/run-exclusive.sh --task SYSUI1` +
`tools/guarded.sh`.

| Item | State |
|---|---|
| 1. Permission dialog from a `com.android.permissioncontroller` system app (staged by the module's `system_apps`, lazy process, host-built launch, `ActivityResultItem` result, store updated, `onRequestPermissionsResult` AOSP shape, multiple permissions, already-granted short-circuit, deny + don't-ask-again, `system_ui` config) | **DONE for the dialog** (the pre-granted arm hits a lazy-spawn launch race, below) |
| 2. Notification shade (`com.android.systemui`, `open_shade` control op, rows, tap fires the sender) | **NOT BUILT** |
| 3. Scenarios on ART: request → dialog visible → tap Allow → result + store; Deny → denied | **PASS** (`sysui1-permission-allow`, `sysui1-permission-deny`) |
|    Shade scenario (post → open_shade → tap → target activity) | **NOT RUN** (item 2 missing) |
| `cargo test -p acos-services -p acos-art -p acos-wm` | **PASS** |
| Module regressions (`notif-perm-granted`, `notif-perm-denied`, `a11y-probe`, `mproc3-two-process`) | **PASS** (4/4) |

## 1. The dialog, end to end (item 1)

`Activity.requestPermissions` builds the framework's own request
(`PackageManager.buildRequestPermissionsIntent`, `ACTION_REQUEST_PERMISSIONS` addressed to
`com.android.permissioncontroller`). The pieces that make it a real guest system app:

* **Module + staging.** `system.permissions` declares
  `system_apps: ["com.android.permissioncontroller"]`; `module::system_apks` resolves it through one
  shared `modules::system_app_apk` (webview / permissions / notifications own their own app). The
  launcher stages it as an extra APK exactly like `webview`'s provider, so the PMS/ATMS know the
  package before the guest boots.
* **Routing.** `ams::dispatch`'s `startActivity` arm no longer answers
  `PERMISSION_UI_UNAVAILABLE` unconditionally: `AmsState::permission_dialog_available` asks three
  real facts (the shared table exists, the module's `system_ui` is `dialogs`, an installed package
  resolves the intent). If any is missing, MODNOTIF2's structured refusal stands — never a silent
  grant.
* **Lazy process + host-built launch.** `start_activity` records the need; the launcher spawns
  `com.android.permissioncontroller` lazily (MPROC) and the peer delivers a host-encoded
  `LaunchActivityItem` + `ResumeActivityItem` to it.
* **The result.** The dialog sets the AOSP result shape and finishes; the host applies it to the one
  runtime table and delivers `ActivityResultItem` + `ResumeActivityItem` to the caller with
  `ResultInfo.mResultWho = "@android:requestPermissions:"`, so the framework's own
  `Activity.dispatchActivityResult` → `dispatchRequestPermissionsResult` runs
  `onRequestPermissionsResult(requestCode, permissions, grantResults)`.
* **`system_ui: dialogs|off`** (`real` default `dialogs`), documented in the module docs and in
  `DECISIONS.md` (ADR-0029 SYSUI1 amendment note).

### Defects found and fixed (each at its cause, each with a test)

1. **`resultWho` was dropped.** MPROC7 hard-coded `ResultInfo(mResultWho = null)`; the permission
   path is the one real non-null `who` (`Activity.startActivityForResult(who, …)`). Added
   `StartActivityArgs.result_who` → `PendingLaunch.result_who` → `ActivityRecord.result_who` →
   `result_info`. Test:
   `the_request_launches_a_real_dialog_recording_the_resolved_component_and_result_who`.
2. **An implicit launch recorded no component.** `ActivityThread.performLaunchActivity` reads
   `r.intent.getComponent()`; the first ART run of the dialog crashed the controller process
   (`NullPointerException … ComponentName.getClassName() on a null object`). AOSP's
   `ActivityStarter` sets the component before scheduling; `start_activity` now does the same on the
   recorded intent. (Same test.)
3. **`EXTRA_REQUEST_PERMISSIONS_RESULTS` had no wire type.** `Extra::Ints` (VAL_INTARRAY) added to
   the AMS reduction, the launch encoder, the JSON capture and the in-process VM refusal arm. Test:
   `the_grant_results_extra_is_a_val_intarray_on_the_wire`.
4. **The runtime table named the wrong app.** `registry` binds `PermissionStore.package` at the first
   `ensure("package")`, which happens while the PMS still holds the `hello-views` seed, so a dialog
   answered "Allow dev.acos.fixtures.helloviews …" (measured). `PmsService::set_package_from_apk_as`
   now re-points the table's package identity (and install-time grants) at the real install. Evidence:
   the first allow run's `launch` step vs the passing run.
5. **`com.android.permissioncontroller`'s filter needed `CATEGORY_DEFAULT`.** The framework's request
   carries no category, and ACOS implements Android's rule that an implicit activity intent then needs
   `DEFAULT` on the filter (the real controller's activity is reached by package+class; declaring the
   category is what makes the documented resolution rule true on ACOS).
6. **A host-initiated launch could not reach an idle process.** `Peer::tick` (the idle turn) now
   drains host transactions, and the launcher's control pump hands a control op's spawn request to
   the launcher, so a module that starts an activity works like a guest `startActivity`. (Plumbing for
   `open_shade`; not yet consumed by a control op — see item 2.)

## 2. What is not done

* **The notification shade (item 2).** Not built: no `com.android.systemui` app, no `open_shade`
  control op, no shade scenario. The design the notification module's shape points to (and the
  smallest next action) is in `docs/handoff`-style detail below.
* **The pre-granted arm of the scenarios (already-granted short-circuit).** The controller is
  launched and returns the granted results without drawing (the code path is in
  `GrantPermissionsActivity`: `nothing to ask, returning results granted,granted`), but in that arm
  the freshly spawned controller's guest times out waiting for the host transaction
  (`ACOSLAUNCH MARK host-transaction-timeout timeout_ms=12000`; process exits 0, no
  `AcosPermController: created`). The same spawn path succeeds in the two dialog arms. Reproduced
  twice; evidence kept in `pregranted-finding/` (`scenario-not-in-suite.yaml`, `verdict.json`,
  `launcher.stderr.log`). This is the MPROC lazy-spawn/launch delivery race, not a permission
  decision, and it is reported rather than papered over (the scenario is deliberately **not** in
  `scenarios/`).
* **`PermissionInfo.loadLabel`.** `IPermissionManager.getPermissionInfo` is unimplemented, so the
  dialog's labels fall back to the permission's last name segment (`CAMERA`,
  `ACCESS_FINE_LOCATION`) and says so in the log. Real labels need `getPermissionInfo` +
  `PermissionInfo.loadLabel` resource resolution.

## 3. Evidence (this directory)

| file | what |
|---|---|
| `allow/verdict.json`, `deny/verdict.json` | the two dialog scenarios, `verdict: pass` |
| `allow/logs/logcat.txt`, `deny/logs/logcat.txt` | the app's and the controller's own log lines the scenarios assert |
| `allow/art-frames/session-exit.json` | cgroup/guard facts of the session (two guest processes) |
| `regressions/*/verdict.json` | `notif-perm-granted`, `notif-perm-denied`, `a11y-probe`, `mproc3-two-process` — all `pass` |
| `pregranted-finding/` | the open lazy-spawn race (scenario, verdict, launcher stderr) |
| `verdict-summary.txt` | one line per scenario |

Commands (from the worktree, after the test-env symlinks):

```
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/rt-sysui1 ACOS_SESSION_SOCKET=$PWD/.cache/sysui1-session.sock
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv ACOS_ART_BIN=$PWD/.cache/sysui1-target/release/acos-art
tools/art-runtime/run-exclusive.sh --task SYSUI1 -- tools/guarded.sh --mem 10G --time 700 -- \
  env -u ACOS_HOME .cache/sysui1-target/release/acos verify scenarios/sysui1-permission-allow.yaml \
  --engine art --json --artifacts results/handoff/SYSUI1/allow --overwrite
```

Scenario step highlights (`sysui1-permission-allow`): the dialog window is the served tree
(`assert_windows` `order: [application]`, `focus_type: TYPE_BASE_APPLICATION`), its rows carry the two
requested permissions' labels, both answers are present; tapping Allow gives the app
`result request=7 n=2 m=2 CAMERA=0 LOCATION=0`, `state after: CAMERA=0 LOCATION=0`, and the store
`state` control op reports both granted with `user_set: true`. `sysui1-permission-deny` additionally
ticks "Don't ask again" and asserts `never_ask_again: true` / `rationale: false` for both.

## 4. The smallest next action for item 2 (shade)

1. `fixtures/apps/systemui` (package `com.android.systemui`, `ShadeActivity`): renders one row per
   notification from launch extras the module builds (`acos.shade.keys/titles/texts/actions`), and on
   a row tap sets a result Intent (`acos.shade.tapped = "id:tag"`, `acos.shade.action = index`) and
   finishes.
2. `notifications` module: `open_shade` control op builds those extras from its posted records,
   starts the activity through the host (the `Peer::tick`/pump plumbing above is already in), and
   `system_apps: ["com.android.systemui"]`; a `ShadeTapSink` published on the host (like
   `FireSender`) fires the notification's content sender and dismisses it when the activity result
   arrives (`AtmsState::finish_activity`).
3. `scenarios/sysui1-shade.yaml`: post (notif-perm-probe), `service_ctl open_shade`, tap
   `row_0`, assert the target activity's log line and `list` → `posted: []`.
