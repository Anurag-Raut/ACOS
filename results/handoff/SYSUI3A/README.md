# SYSUI3A — finishing SYSUI2: the shade tap, the settle contract, the pre-granted arm

Branch `sysui3a` (from `sysui2`, `2f8d2141`), 2026-11-17. All ART runs are private
(`ACOS_RUNTIME_DIR=$PWD/.cache/rt-sysui3a`, `ACOS_SESSION_SOCKET=$PWD/.cache/sysui3a-session.sock`,
`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`, `ACOS_ART_BIN=$PWD/.cache/sysui3a-target/release/acos-art`) and
every one went through `tools/art-runtime/run-exclusive.sh --task SYSUI3A` + `tools/guarded.sh`.

| Item | State |
|---|---|
| 1. Shade tap: the primary package is authoritative, so the content sender's start into tag 0 is encoded and delivered | **FIXED**, `sysui1-shade` **12/12 pass** |
| 2. The settle contract for a tap that closes the tapped window (host-recorded removal = handled) | **FIXED** in one place, with the negative guard; unit tests in both crates |
| 3. Pre-granted arm (`sysui1-permission-pregranted`) | **STILL FAILING** — offline wire round trip passes, the guest executes the delivered transaction and no callback runs (below) |
| `cargo test -p acos-services -p acos-art -p acos-wm -p acos-verify -p acos-cli` | **PASS**: 1328 passed, 0 failed (`.cache/cargo-test.log`) |
| Regressions `sysui1-permission-allow` / `-deny`, `notif-perm-granted` / `-denied`, `a11y-probe`, `mproc3-two-process`, `form-validation` | **PASS** (7/7) |

## 1. Shade tap, part 2: the primary's package is the launcher's fact (fixed)

`attachApplication` carries no package (`IActivityManager.attachApplication(IApplicationThread,
long)`), and the primary process never sends the peer's connection handshake (it is the launcher's
own guest), so its `ProcessRecord` was created **unnamed**. `Peer::package_of(0)` returned that empty
name, `pending_launch_for("")` found nothing, and the `startActivity` the notification's content
`PendingIntent` performed into the already-running primary was recorded and never encoded.

* `AtmsState` carries `primary_package` and `set_primary_package` — the record is named the way
  `ActivityManagerService.startProcessLocked` names it, *before* the attach. A record that already
  has a (different) name is never overwritten; an empty seed is not a name; a process the launcher
  did not name stays unnamed rather than invented.
* `AtmsState::attach_application_for` fills an empty package from the record's own name first, then
  (tag 0 only) from the seeded primary package.
* `ServiceHost::seed_primary_package` is the launcher's seam (`AtmsService::set_primary_package`),
  called in `acos-art`'s launch right after the peer host is built, with the same package that goes
  into `ACOS_PROCESS_NAME`.
* `Peer::package_of` treats an **empty** recorded package as no answer and falls back to the
  handshake name.

Evidence (`shade/launcher.stderr.log`): the shade's row tap now produces

```
acos-binder-peer: process 0: host-built launch for dev.acos.fixtures.notifpermprobe/dev.acos.fixtures.notifpermprobe.TargetActivity (token=0x7a, 2236 bytes)
acos-binder-peer: process 0: delivering host-built ClientTransaction (2260 bytes, IApplicationThread.scheduleTransaction) to java binder wire=1
```

and the scenario's own `assert_log` sees both app lines
(`tapped key=7:probe-notification action=-1`, `TargetActivity opened from=content …`), then `posted: []`.
Tests: `crates/acos-services/tests/primary_package.rs` (3 tests, fail before the change: the record
keeps `""`).

## 2. The settle contract: a tap that closed the window it landed on is a pass (fixed)

SYSUI2 called this a harness bug and it was one: the guest's strict settle names `detached` (later
`no-draw-after-action`) when the tapped window has gone away, and the host turned a **completed
interaction** into `TIMEOUT`. The rule now lives in exactly two places, one per concern:

* `acos-cli` `engine::art::removed_window_handled` — the backend proves the removal against its
  **own** record of the window stack: the tap must be a tap, the gesture must be **delivered**
  (host-observed) with at least one **handled** guest ack, the tap's target window (the top-most
  touchable window in the *pre-action* record whose bounds contain the point) must be **absent**
  from the observation the action completed, and something must still be on screen. Only then does
  the settle record become `bound: "window-removed", handled: "window-removed"` with the window id
  and label; otherwise the guest's timeout stands untouched.
* `acos-verify` `eval_tap_change` — the single place the tap contract says so: a `window-removed`
  handled settle passes with `rule: "window_removed"` and the removal as evidence; a bare timeout
  still fails.

The orchestrator's negative is a test: a window that vanishes **without any input delivered** (or
without a handled ack, or with no input record at all) keeps its timeout
(`a_window_that_vanishes_without_a_delivered_gesture_stays_a_timeout`), as does a tap beside the
window that actually closed, a non-touchable top window, a non-tap command, and "everything
vanished" (`only_the_tapped_window_going_away_counts`). The positive is
`a_tap_that_closed_the_window_it_landed_on_is_handled` and, on the verify side,
`a_handled_window_removal_passes_the_tap_contract`.

Shade verdict: step 9 (`tap`) is `pass` with `settle.bound: "window-removed"`,
`handled: "window-removed"`, `handled_window: "30c8561"`, `acked: 1`, and the removal named as the
interaction's evidence (`shade/verdict.json`).

## 3. Pre-granted arm: what the offline work proved, and where it still stops

The orchestrator asked for the offline `ActivityResultItem`/`ResultInfo` round trip and golden
**first**. It exists in this branch and it **passes**:

```
cargo test -p acos-runtime --test client_transaction_java        # real Java, both directions
test real_java_client_transaction_matches_the_host_encoder ... ok
```

That case builds the *same* logical `ResultInfo`/`ActivityResultItem` with the **real framework
classes** through the ACOS VM, marshals with the real `writeToParcel`, and compares byte for byte
with `acos_services::client_transaction` in both directions (`write.activity_result_item=…`,
`read.activity_result_item=ok`, `roundtrip.…`). So the host's bytes are exactly what this image's own
Java writes: the item carries only `writeTypedList(mResultInfoList)` (image dex
`ActivityResultItem.writeToParcel`, confirmed with `examples/scan_parcelables`), and there is no item
token to "fix".

The image's executor (`TransactionExecutor.execute`/`executeCallbacks`, image dex) looks the record
up by the **`ClientTransaction`'s** token and passes that token to the item, i.e. exactly the
transaction the host builds (`client_transaction(Some(BinderId(caller)), Some(resume), vec![item])`).
That is also what the passing dialog arms deliver, and the guest-side `deliverScriptedResult`
(`LaunchMain.java`) builds the same shape.

What still fails, precisely, after this branch: the caller receives the transaction (it dispatches one
`H 159`, `pregranted/launcher.stderr.log`) and neither `onRequestPermissionsResult` nor
`onActivityResult` runs and no `PendingTransactionActions.StopInfo`/lifecycle marker follows
(`pregranted/logs/logcat.txt` ends at `AcosPermProbe: requesting code=7`). One run also reported
`UNSUPPORTED_ANDROID_API … activityStopped: illegal transition Stop from Resumed` for tag 0, i.e. the
caller's `activityStopped` arrived while the host still modelled the record as `Resumed` — the
dialog finishes **during** `onCreate`, so the caller's pause/stop (queued by `top_of_task`) and the
result delivery race, which the interactive arms (dialog finishes after the user taps) do not.

**Smallest next action** (no ART needed first): in `acos-runtime/tests/client_transaction_java.rs`'s
harness, add a case that boots the real `ActivityThread`, delivers
`ActivityResultItem + ResumeActivityItem` through the real `scheduleTransaction` for a record that is
`Stopped`, and read `ActivityThread.mActivities` + `Activity.mCalled` before/after; that names the
exact blocking call (null record, `SuperNotCalled`, or `handleSendResult` short-circuit) offline.
Then fix that one call in the host's delivery (`AtmsState::deliver_result` / `queue_lifecycle`) rather
than the encoder, which the differential has cleared.

## 4. Evidence (this directory)

| file | what |
|---|---|
| `shade/` | `verdict.json`: all 12 steps `pass`, the tap step's `window-removed` settle record |
| `pregranted/` | the still-failing arm: verdict, logcat (ends at `requesting code=7`), launcher stderr |
| `sysui1-permission-allow/`, `-deny/`, `notif-perm-granted/`, `notif-perm-denied/`, `a11y-probe/`, `mproc3-two-process/`, `form-validation/` | regressions, all `pass` |
| `cargo-test.log` (in `.cache/`) | 1328 tests, 0 failures for the five acceptance crates |

Commands (from the worktree, after the test-env symlinks):

```
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/rt-sysui3a ACOS_SESSION_SOCKET=$PWD/.cache/sysui3a-session.sock
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv ACOS_ART_BIN=$PWD/.cache/sysui3a-target/release/acos-art
tools/art-runtime/run-exclusive.sh --task SYSUI3A -- tools/guarded.sh --mem 14G --time 900 -- \
  env -u ACOS_HOME .cache/sysui3a-target/release/acos verify scenarios/sysui1-shade.yaml \
  --engine art --json --artifacts results/handoff/SYSUI3A/shade --overwrite
```

`back-layers` is not a scenario in this branch (`scenarios/` has `gaten-backstack`, `w42-back-frame`,
`layer0-overlay-stack`); the regression was not runnable under that name.
