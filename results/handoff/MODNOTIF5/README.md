# MODNOTIF5 — finish + integrate the notification module (merge, control path, fixture, ART)

Branch `modnotif5` from `modnotif4` (`065c2fd0`) + two merges of `modules`
(`8a3a4ec1` merge #1 of `modules`@`e1b9e3b9`; `9ea2ea25` merge #2 of `modules`@`cfed3eea`
per the orchestrator's MODTAIL2 update), 2026-11-16.

| Step | State |
|---|---|
| 1. Merge `modules` into the branch, unify onto the `modules` control path, keep both sides' tests, keep the MODNOTIF4 codec fixes | **DONE** (`8a3a4ec1`, `9ea2ea25`) |
| 2. `service_request` `control` arm + a control scenario step (reuse, no second path) | **DONE — reused**: `modules` already ships the one control path (`service_op`/`Backend::service_ctl`/`acos_service_ctl`/`ctl_op`/`service_ctl` step). No `services_control` step and no second arm were added; what this branch adds is the missing *assertion* (`service_ctl.expect`, a JSON subset of the module's reply) so a control op is a check, not just evidence. |
| 3. `fixtures/apps/notif-perm-probe` registered in `tools/build-apps.sh` | **DONE** |
| 4. Two guarded ART scenarios (`--engine art`): pre-granted and pre-denied | **DONE, both `pass`** |

## 1. What the real ART run found (and fixed) — the reason step 4 existed

MODNOTIF4 proved the `Notification` codec against a **Java-built** parcellable: both
sides were transcribed from the same reading of the image DEX, so a shared misreading
cancels out. A real `Notification.Builder` notification does not. The first ART run
crashed the app inside `notify()`:

```
java.lang.IllegalArgumentException: bad request for android.app.INotificationManager::
enqueueNotificationWithTag: arg 4 (TypedObject(android.app.Notification, nullable=true)):
object at 552 is not a binder (type 0xffffffff)
```

Two real bugs, both fixed at the cause (`codec-bugs.txt` has the byte-level evidence):

1. **`android.media.AudioAttributes` was transcribed from the wrong shape.** The
   MODNOTIF3 spec read a fixed `5×int + String[] + String + int + Bundle`. The real
   writer (`base/media/java/android/media/AudioAttributes.java:1349-1368`) **branches**
   on `FLATTEN_TAGS` — one tag form, not two — and writes the bundle only after an
   `ATTR_PARCEL_IS_VALID_BUNDLE` (1980) sentinel, with `ATTR_PARCEL_IS_NULL_BUNDLE`
   (-1977) meaning **no body**. A notification's default attributes are the
   non-flattened form with a null bundle, so the old list over-read 8 bytes and every
   later field was shifted: `vibrate` read `0` where the wire had `-1`, and
   `fullScreenIntent` read a binder where the wire had `-1`.
   Fix: a curated codec (`read_audio_attributes`/`write_audio_attributes`), like
   `Icon`, for both branches.

2. **`Parcel.writeValue`'s length prefix counts the binder stability records.** The
   guest's libbinder writes a 4-byte stability record after every `flat_binder_object`;
   `peer::import_parcel` strips them (the in-process format has none), but the length
   prefix the framework wrote still counts them. The notification's trailing
   `allPendingIntents` `ArraySet` (the MODNOTIF4 field) therefore arrived 4 bytes
   shorter than declared and was refused:
   `VAL_4 body consumed 80 bytes but declared 84`.
   Fix: the import records the rewritten object offsets
   (`Parcel::register_object`, also used by MODTAIL2's bluetooth handler) and
   `check_value_length` accepts **exactly** 4 bytes per object inside the body —
   anything else is still a refusal (`read_value_length`'s bound allows the same
   records, so a hostile length still cannot skip ahead).

Debugging seam added and kept (off by default): `ACOS_PARCEL_TRACE=1` prints one line
per parcelable field with its offset and value; `ACOS_PARCEL_DUMP=<dir>` writes the raw
bytes of an argument the codec could not decode, which is how the crash offset became
reproducible test data.

New tests (`crates/acos-services/tests/parcelable.rs`):
`audio_attributes_reads_the_image_wire_form` (byte blob captured from the run, values +
exact consumption + byte-identical re-encode),
`audio_attributes_reads_the_flattened_tag_form` (the other branch), and
`a_value_length_prefix_counts_the_stripped_stability_records` (the imported-parcel case
plus a forged length that must still be refused).

## 2. Merge conflicts (both merges, in order)

Merge #1 (`modules`@`e1b9e3b9` → `8a3a4ec1`), 2 files, both a union:

| File | Resolution |
|---|---|
| `crates/acos-services/src/modules/mod.rs` | `notifications` + `permissions` (this branch) and `sensors` (modules) |
| `crates/acos-services/src/module.rs` | `ModuleRegistry::standard()` lists both sides' modules |

Semantic repair for the same merge: `modules`' `sensors` manifest predated
`ModuleManifest::replaces` (added here), so it gained `replaces: &[]` — the sensors node
is `core`'s, the module installs a feed, it does not take the factory over.

Merge #2 (`modules`@`cfed3eea`, MODTAIL2 → `9ea2ea25`), 10 files:

| File | Resolution |
|---|---|
| `crates/acos-services/src/module.rs` | `ModuleManifest` = union of this branch's `replaces` with modules' `features` + `process_initializers`; `Module::control(&mut self, host: &mut ServiceHost, op, args)` (the single control path) |
| `crates/acos-services/src/host.rs` | both hooks in `register`: the `RegistrationScope` overwrite log (`end_scope` restore) **and** modules' `notify_registered` |
| `crates/acos-services/src/modules/mod.rs` | union: `biometric`, `bluetooth`, `notifications`, `permissions` |
| `crates/acos-services/src/core_module.rs`, `modules/{sensors,vibrator,telephony,biometric,bluetooth,tail_stubs}.rs` | every manifest gains the union of the three fields (`replaces: &[]` where nothing is taken over from `core`; the two `tail_stubs` macros too) |
| `crates/acos-services/src/ams.rs` | `Extra` = this branch's `Strings` (REQUEST_PERMISSIONS names) + modules' `Bytes`/`ByteArrays`/`Parcelable`; the bundle encoder keeps both |
| `crates/acos-runtime/src/ams_java.rs` | modules' `Bytes`/`ByteArrays`/`Parcelable` arms plus the `String[]` refusal arm |
| `crates/acos-binder-peer/src/peer.rs` | modules' `object_offsets`/`register_object` is the shared object-table seam; this branch's duplicate `note_flat_object` dropped, `register_object` keeps the table sorted (MODNOTIF5's length accounting reads it) |
| `crates/acos-mcp/src/{descriptors,help}.rs` | modules' dedicated `acos_service_ctl` is THE control tool; this branch's extra `ctl` op on `acos_services` removed (no second control path) |
| `docs/AGENT_GUIDE.md` | `service_ctl` row + paragraph name `acos_service_ctl` and the new `expect` |

Module signature adaptation: `notifications`/`permissions` implement
`control(&mut self, _host, op, args)`; tests updated to `registry.control(&mut host, …)`.
No test was weakened or removed on either side.

## 3. The control path used (step 2)

`service_ctl` scenario step → `Backend::service_ctl(ServiceCtlRequest{module, op, args})`
→ the engine's `service_op` → `{"cmd":"services","op":"ctl","id","ctl_op","args"}` →
`service_request`'s `ctl` arm → `ModuleRegistry::control(&mut host, id, op, args)` →
`Module::control`. The module's answer arrives under `value`
(`{ok, cmd, id, op, value, modules[], attached[]}`).

This branch adds **`expect`** to the step (`crates/acos-protocol/src/scenario.rs`,
`crates/acos-verify/src/executor.rs`): a JSON **subset** (objects by key, arrays with
equal length, scalars by equality) the reply must contain; a mismatch is
`ASSERTION_FAILED` with the failing path, so `list`/`tap`/`dismiss` are real checks.
Unit-tested (`executor::service_ctl_expect`), schema regenerated
(`crates/acos-protocol/schema/scenario.json`).

## 4. The fixture and the two scenarios

`fixtures/apps/notif-perm-probe` (registered in `tools/build-apps.sh`, built with
`ACOS_APP_ONLY=notif-perm-probe`): `MainActivity` creates channel `probe`, posts
`notify(tag="probe-notification", id=7)` with a `PendingIntent.getActivity` content
intent plus one `Notification.Action` (own PendingIntent, `Icon.createWithResource`),
and prints `areNotificationsEnabled()` before and after the post; `TargetActivity` is
the senders' explicit target. No exception handling: a broken stack crashes the app.

* `scenarios/notif-perm-granted.yaml` — `system.permissions` `{grant:
  [POST_NOTIFICATIONS]}` + `system.notifications`: screen `enabled: true` / `posted: id
  7` / `channel: probe`; `list` → channel `probe`/`Probe`/importance 3 and the record's
  `title`/`text`/`tag`/`actions[0].title=Open`; `tap` → `fired.result.start_result 0`
  with the content intent; `action {index:0}` → `start_result 0` with the action intent;
  `dismiss` → `remaining: 0`; `list` → `posted: []`.
* `scenarios/notif-perm-denied.yaml` — pre-denied: screen `enabled: false`, the app's
  post still attempted; `list` → `posted: []` and `dropped[0].reason` =
  `"android.permission.POST_NOTIFICATIONS is not granted (targetSdk 34);
  NotificationManagerService drops the notification"` (asserted exactly).

**No second window is asserted** (decision (a)): the tap's target activity is started by
`AmsState::start_activity` and queued as a system-originated `LaunchActivityItem`, which
is the MPROC delivery class; the assertion is the fired sender record `{start_result}`.

Decision: **mutable-intent fill-in is out of scope**. `IIntentSender.send` now reads the
fill-in marker: a non-null fill-in on a **mutable** sender is an honest
`UNSUPPORTED_ANDROID_API` naming `Intent.fillIn` (nothing is delivered, nothing is
recorded as sent); on an **immutable** sender the fill-in cannot alter the stored intent,
so it is dropped and named (`fill_in_ignored: true`) on the evidence. Tests:
`a_fill_in_on_a_mutable_sender_is_refused_not_silently_dropped`,
`a_fill_in_on_an_immutable_sender_is_ignored_and_named`.

## 5. Evidence

| File | Contents |
|---|---|
| `verify-granted.json` / `verdict-granted.json` | the guarded `--engine art` run of the granted scenario: **status pass, verdict pass, 10/10 steps** |
| `verify-denied.json` / `verdict-denied.json` | the denied scenario: **status pass, verdict pass, 5/5 steps** |
| `engine-granted.json` / `engine-denied.json` | the run's artifact index |
| `codec-bugs.txt` | the guest crash line, the field trace, and the byte dump the two codec fixes are based on |
| `services-tests.log` | `cargo test -p acos-services` — **54 suites ok, 0 failed** |
| `art-tests.log` | `cargo test -p acos-art --no-fail-fast` — **15 suites ok, 208 passed, 0 failed** |
| `art-lib-tests.log` | `cargo test -p acos-art --lib` — 154 passed, 1 ignored |
| `cli-tests.log` | `cargo test -p acos-cli --no-fail-fast` — 17 suites ok, 252 passed, 1 failed: `fb1_live_input::ambiguous_assertion_is_refused_with_candidates` (below) |
| `acceptance-tests.log` | the combined `cargo test -p acos-services -p acos-art -p acos-cli` run (see the note) |

Commands (all under the shared lock + guard):

```
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/modnotif5-run ACOS_SESSION_SOCKET=$ACOS_RUNTIME_DIR/session.sock
export ACOS_ART_BIN=$PWD/.cache/modnotif5-target/release/acos-art
tools/art-runtime/run-exclusive.sh --task MODNOTIF5 -- \
  tools/guarded.sh --mem 14G --time 600 -- \
  .cache/modnotif5-target/release/acos verify scenarios/notif-perm-granted.yaml \
    --engine art --artifacts .cache/modnotif5-run/granted --json
```

**Environment note (pre-existing, not this branch).** In a deep worktree some Unix-socket
tests cannot bind: `sun_path` is 108 bytes. `fb1_live_input`'s long-tag case builds
`<repo>/.cache/fb1-test-ambiguous_assertion_is_refused_with_candidates-<pid>/serve.sock`
= **139 bytes** here, so `acos serve` can never create the socket and the test times out
with `serve socket never appeared`. The other three tests in that file (shorter tags) and
every other `acos-cli` suite pass. The same class (7 `live-view` tests) is unaffected
with `ACOS_VIEW_TEST_DIR=$PWD/x`, which is how `art-tests.log` was produced; MODSENS
documented the identical worktree limitation. No assertion was weakened.

## 6. Peak resources

Host RAM peak 14 GiB / 60 GiB. Disk `/` 84% at the start, 78% at the end of the runs
(the shared host was being cleaned concurrently); this branch's scratch
(`.cache/modnotif5-target` ~2.6 GiB, `.cache/modnotif5-run`, `.cache/modnotif5-dump`) is
deleted at the end. Release binaries used for the ART runs:
`CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_INCREMENTAL=0 cargo build --release -p acos-cli
-p acos-art`.
