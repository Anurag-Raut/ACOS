# MODNOTIF4 — the notification module and the PendingIntent senders (items 1-3)

Branch `modnotif4` from `modnotif3` (`a8a7c633`), 2026-11-16. Scope per the brief
and the orchestrator's decision (a): build items 1-3 for real, and assert the
tap arm at the delivery seam rather than fake a second system-originated launch.

| Item | State |
|---|---|
| 1. Java differential `notification` case in `ParcelableCompareMain` (no `PendingIntent`, all fields byte-compared; then with one) | **DONE** |
| 2. `getIntentSender` sender records + a dynamic `IIntentSender` node whose `send` really starts the target | **DONE** |
| 3. `notifications` module (channels, notify/cancel/cancelAll, `areNotificationsEnabled`, `POST_NOTIFICATIONS` gate, FGS notifications, control ops `list`/`tap`/`action`/`dismiss`, hot attach, core takeover + restore, conformance kit) | **DONE** |
| 4. `notif-perm-probe` fixture + `services_control` scenario step + guarded ART run | **NOT BUILT** — see §5 |

The Java differential found two real codec bugs and one framework fact; all three
are fixed in `parcelable.rs` (§1).

## 1. What the differential changed (codec bugs, root causes)

1. **`Icon`'s resource branch carried two fields this image does not write.** The
   MODNOTIF3 spec was transcribed from AOSP `android-14.0.0_r75` source, which
   writes `writeBoolean(useMonochrome)` + `writeFloat(insetScale)`. This image's
   boot DEX writes only `writeString(resPackage)` + `writeInt(resId)`, then the
   tint marker and the `BlendMode` value (`icon-codec-fix.txt`, the scan of
   `Icon.writeToParcel`: `0x002f writeString`, `0x0036 writeInt`, `0x0057
   writeInt(tint)`, `0x0069 writeInt(BlendMode)`). Every real `setSmallIcon`
   notification would have desynchronised the parcel after the icon; the
   differential failed at exactly that offset before the fix.
2. **`allPendingIntents` was a documented "gap" that is on the wire.** MODNOTIF3
   wrote the null encoding (`-1`) for the trailing `writeArraySet`. A notification
   with a content `PendingIntent` really writes `count = 1` + one `writeValue`
   (`VAL_PARCELABLE` + class name + payload), so the field is now
   `FieldOp::ArraySet` (read/write through the existing `read_dynamic`/
   `write_dynamic` codec). The scanner records the call as `writeArraySet`
   instead of pretending it is a lone `writeInt`.
3. **Two image defaults are not AOSP defaults** and the Java case pins them, so
   the comparison is about the codec and not about a constructor:
   `Notification()` seeds a non-null `audioAttributes` with `audioStreamType =
   STREAM_DEFAULT (-1)`, and `mAllowSystemGeneratedContextualActions` is `true`.
   `audioStreamType`/`audioAttributes` are explicitly nulled in the case;
   `mAllowSystemGeneratedContextualActions` is kept `true` in the Rust value.
   The reverse direction (`createFromParcel` → re-emit) is *not* byte-identical
   for that pair because the image's reader rebuilds `audioAttributes`; the test
   asserts the exact shape of that one 32-byte window and byte equality
   everywhere else (§2), rather than silently skipping the comparison.

PendingIntent without a system service: `PendingIntent(IIntentSender)` is
`@hide`, so the harness obtains it by reflection and wraps an
`IIntentSender.Stub` signature stub (`tests/java/stubs/android/content/
IIntentSender.java`, `IIntentReceiver.java`). The guest binder layer allocates
the strong-binder handle, so the harness emits it (`intent_sender`) and the Rust
value carries the same id: every other byte of the with-`PendingIntent`
notification is the codec's, compared byte-for-byte.

## 2. Evidence

| File | Contents |
|---|---|
| `java-differential.txt` | `cargo test -p acos-runtime --test parcelable_java`: `real_java_write_to_parcel_matches_the_rust_codec ... ok` (forward bytes equal for `notification` **and** `notification_intent`, the reverse `read.notification=ok`, and the audio-window assertion) |
| `icon-codec-fix.txt` | the boot-DEX scan of `Icon.writeToParcel` the fix is based on |
| `module-tests.txt` | `modnotif4_notifications` 5/5 + `module_kit` 12/12 (incl. `the_notifications_module_passes_the_conformance_kit`) |
| `full-suite.txt` | `cargo test -p acos-services`: **49/49 test binaries ok, 0 failed** |

## 3. Item 2: the `PendingIntent` sender layer

`crates/acos-services/src/pending_intent.rs`:

* `SenderTable` is `ActivityManagerService.mIntentSenderRecords`: one
  `PendingIntentRecord` per live sender keyed by `(type, package, requestCode,
  filter-equal intents, flags-without-the-three-ephemeral-bits)`
  (`PendingIntentRecord.Key`; `Intent.filterEquals` ignores extras, so the key
  does too).
* `get_intent_sender` implements the flag semantics: `FLAG_NO_CREATE` answers the
  existing sender or `null` and never creates; `FLAG_CANCEL_CURRENT` cancels the
  matching record and allocates a new one; `FLAG_UPDATE_CURRENT` keeps the record
  and the binder and replaces the stored extras; without either flag an equal
  key returns the same sender (`PendingIntent` identity).
* 16 `IIntentSender` nodes are registered with `ServiceHost::register_dynamic` at
  install (`registry::install`) and bound into the table; the node serves
  transaction 1 (`send`) itself (`raw_transact`, the AIDL is not in the decoded
  tables).
* `AmsState::send_pending_intent` replays the stored intent: `start_activity`
  for `INTENT_SENDER_ACTIVITY`, `AmsState::broadcast` for broadcast,
  `start_service` for service/foreground-service, applying
  `FLAG_UPDATE_CURRENT` extras and recording the evidence (`SendEvidence`) on the
  record. A cancelled record refuses (AOSP logs and returns); `FLAG_ONE_SHOT`
  cancels after the first send.
* `IActivityManager` codes 62/63 (`getIntentSender[WithFeature]`), 64
  (`cancelIntentSender`) and 65 (`getInfoForIntentSender`) are handled in
  `ams::dispatch` from the decoded argument list.
* `ServiceHost::set_pending_intent_fire` publishes the delivery closure
  (`Arc<dyn Fn(binder, code) -> Result<SendEvidence>>`) so the `notifications`
  module can `tap` without reaching into the AMS state; `begin_scope`/`end_scope`
  save and restore the slot like the notification/permission slots.

**Honest gap:** a *mutable* intent's fill-in (`PendingIntent.send(context, code,
intent)`) is not merged into the stored intent — the node records `fill_in_ignored`
in its docs and the evidence path reports the record as fired without it. No
consumer needs it yet (a notification tap passes no fill-in), and merging needs
`Intent.fillIn` over our reduced `IntentFacts`.

## 4. Item 3: the `system.notifications` module

`crates/acos-services/src/modules/notifications.rs`, id `system.notifications`,
profile `real`, `replaces: ["notification"]` (same descriptor as `core`, so the
takeover is an overwrite `end_scope` restores — proven by
`attaching_the_module_takes_the_name_over_and_detach_restores_core`).

* Posting: `enqueueNotificationWithTag` decodes the MODNOTIF3 notification
  (`decode_notification`, field indices from the codec) into
  `PostedNotification { package, id, tag, channel, title, text, flags, actions[],
  content_sender, foreground_service }`; `notify(tag, id)` replaces the previous
  record with the same key.
* `POST_NOTIFICATIONS` gate: `notifications_enabled()` reads the shared
  `PermissionStore` (the table `IPermissionManager`, `PMS.checkPermission` and
  `AMS.checkPermission` read) and is exempt below `targetSdk 33`, like AOSP. A
  denied `notify` is **dropped and recorded** (`dropped[]`), never an exception —
  what a real device does and what `areNotificationsEnabled()` then answers.
* `cancelNotificationWithTag` / `cancelAllNotifications` remove records; a
  foreground-service notification is kept while its service runs (AOSP
  `cancelNotificationWithTag`).
* Channels: `createNotificationChannels[ForPackage]` stores
  `(package, id, name, importance)`; `getNotificationChannels[ForPackage]` answers
  a `ParceledListSlice` of synthesized `NotificationChannel` payloads.
  `getActiveNotifications` answers the empty `StatusBarNotification[]` (ACOS has
  no status-bar service to build one) and says so in the module report.
* Control ops: `list`/`state`, `tap {id|tag|title|text}`, `action {id, index}`,
  `dismiss {id|tag}`. `tap`/`action` fire the sender through the published
  delivery closure and report `{start_result}` plus the fired intent; a sender the
  session never created is a structured error, never a fake success.
* Launch config: `{"package": …, "target_sdk": …}`.

## 5. Item 4: what is left and the exact next action

1. **`services_control` is not a scenario step yet.** `service_request`
   (`crates/acos-art/src/launch.rs:6451`) implements `list`/`status`/`attach`/
   `detach`; it needs one more arm:
   `"control" => modules.control(id, request["op"], request["args"])` with the
   same `BAD_REQUEST` shape (and the same `{"ok":true,"cmd":"services","op":op}`
   envelope), plus the scenario step that forwards `services_control
   {id, op, args}` to it. The module side is done and unit-tested
   (`module-tests.txt`).
2. **The fixture** (`fixtures/apps/notif-perm-probe`): an app that calls
   `NotificationManager.createNotificationChannel`, `notify(tag, id, …)` with a
   `PendingIntent.getActivity` content intent and one action, then renders
   `areNotificationsEnabled()` and the POST_NOTIFICATIONS denial on screen.
   `fixtures/build-hostile-probe.sh` is the build script to copy (javac + d8, the
   toolchain is present).
3. **The guarded ART run** then asserts: pre-granted → post → `services_control
   {op:"list"}` → `tap` → the sender's `{start_result}` evidence (the target
   activity's launch is queued by `start_activity`; **a second
   system-originated `LaunchActivityItem` is still the MPROC delivery class** and
   the peer delivers exactly one per session) → `dismiss`; pre-denied → `posted`
   empty, `dropped[0].reason` names `POST_NOTIFICATIONS`, the app's screen shows
   the denial. Decision (a) applied: the tap arm asserts the fired sender record
   (`state.fired[]`, `{start_result}`), not a window that does not open.

## 6. Peak resources

RAM peak observed 14 GiB / 60 GiB (host); disk `/` 81% at the start, 85% at the
peak build (`.cache/modnotif4-target`, 1.4 GiB, deleted at the end), 83% after.
