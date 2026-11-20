# MODNOTIF2 — service layers for notifications and permissions (PARTIAL)

Branch `modnotif2` from `modnotif` (ee2d2bbf), 2026-11-16. Scope per the brief: **option C** of
the ADR-0029 amendment (the service layers), no window/dialog/shade UI.

| Item | State |
|---|---|
| 3. framework: a module replaces a `core` name; detach restores core's answer; fixed and tested in the framework | **DONE** |
| 2. `permissions` module: one shared runtime-permission store behind both `checkPermission` paths, grant/deny/revoke, never-ask-again + rationale, launch-config pre-grant/pre-deny, `requestPermissions` → honest structured result | **DONE** (one honest limit: the *pre-granted* request cannot complete, see below) |
| 4a. conformance kit for the permissions module | **DONE** (`module_kit.rs` + `modnotif2_permissions.rs`); the notifications module does not exist yet, so its kit row is not written |
| 1. `notifications` module (channels, notify/cancel, POST_NOTIFICATIONS gating, FGS notifications, posted notifications with title/text/actions/channel/PendingIntent handles, control ops list/tap/action/dismiss) | **NOT BUILT** — blocked on two pieces of decoding/dispatch work that are named below with their exact next step |
| 4b. fixture `notif-perm-probe` + scenarios + real ART run | **NOT BUILT** (depends on item 1 for the notification arms) |

Commits: `4a27bdb0` (item 3 + item 2 + tests), `e0623cac` (conformance + ADR note + `String[]`
extras in the AMS bridge).

## 1. What was wrong before (root causes, not symptoms)

* **`end_scope` did not restore overwritten factories.** `ServiceHost::register` on a descriptor that
  already exists replaced `entries[descriptor]`, but `RegistrationScope` only snapshotted the *names*
  and the *keys* of the entry map, so `end_scope` deleted what a scope had **added** and reinstalled
  the name table — the overwritten factory stayed installed. A module that takes a `core` name over
  (`IPermissionManager`) therefore kept answering after detach, and a stale binder id.
  Fix: `RegistrationScope` carries a per-scope overwrite log (`(descriptor, old binder, old factory)`);
  `end_scope` frees the module's binder and reinstates core's factory + binder, and restores the
  host's `notification`/`permission_store` slots by pointer identity. `before-after-endscope.txt` is
  the failing/passing pair.
* **Three permission tables, no runtime one.** `PmsState::check_permission` and
  `AtmsState::check_permission` each consulted their own install-time `granted_permissions` copy, and
  `IPermissionManager.grantRuntimePermission` threw `UNSUPPORTED_ANDROID_API`. Fix: one
  `permissions::PermissionStore` created in `registry::install`, shared by both states and published
  on the host so the module writes the same table. A permission with no runtime entry still answers
  from the manifest (install-time grants unchanged); a runtime permission starts **not** granted.
* **`requestPermissions` had no honest resolution.** `Activity.requestPermissions` builds an Intent at
  `com.android.permissioncontroller` (`PackageManager.java:7049`) and starts it for a result. ACOS
  installs no such package, so the intent fell through to activity resolution and answered
  `START_NOT_FOUND` (an app-visible `ActivityNotFoundException`). Fix: the ATMS start path recognises
  the intent (`permissions::permission_request`) and answers the structured
  `PERMISSION_UI_UNAVAILABLE` (new `ServiceError` variant + `codes::PERMISSION_UI_UNAVAILABLE`),
  carrying the configured state (`granted` when every permission is already granted — what
  `GrantPermissionsActivity` returns without drawing — else `denied`), the permissions, and the
  system app that is missing. Nothing is granted.

## 2. What the permissions module is

`crates/acos-services/src/modules/permissions.rs`, id `system.permissions`, profile `real`,
`replaces: ["permission"]` (the `permissionmgr` alias is core's second name for the same descriptor,
so it follows automatically). It registers the **same descriptor** as core, which is what makes the
takeover an overwrite that `end_scope` can undo.

* `getPermissionFlags` → `FLAG_PERMISSION_USER_SET` (0x1) / `FLAG_PERMISSION_USER_FIXED` (0x2) /
  `FLAG_PERMISSION_GRANTED_BY_DEFAULT` (0x20), the API-34 values
  (`base/core/java/android/content/pm/PackageManager.java:5090`, `:5100`, `:5146`).
* `shouldShowRequestPermissionRationale` → `true` only when the app asked, the user denied, it is not
  "don't ask again" and it is still not granted (the documented contract of
  `Activity.shouldShowRequestPermissionRationale`, `Activity.java:5761`).
* `checkPermission`, `grantRuntimePermission`, `revokeRuntimePermission`, `updatePermissionFlags`
  (a mask bit ACOS does not model is refused, not dropped), `isPermissionRevokedByPolicy`,
  the auto-revoke/allowlist list getters.
* Control ops `grant {permission}`, `deny {permission, never_ask_again?}`, `revoke {permission}`,
  `state` (every permission with state/flags/rationale/declared, the requests, the refusals, the
  mutations).
* Launch config `{"grant": [...], "deny": [...], "deny_never_ask_again": [...]}`.

`state` after a fixture-like run shows both halves:

```json
{"package":"…","permissions":[{"permission":"android.permission.POST_NOTIFICATIONS",
  "state":"granted","granted":true,"user_set":true,"never_ask_again":false,
  "requested":true,"rationale":false,"flags":1,"declared":false}, …],
 "requests":[], "refused_requests":[], "mutations":["grant:android.permission.POST_NOTIFICATIONS"]}
```

## 3. The honest limit on `requestPermissions` (actor: orchestrator/owner)

The brief asked: "pre-granted -> granted, else a structured `PERMISSION_UI_UNAVAILABLE`". The second
half is done. The first half cannot be completed at the service layer: even when every requested
permission is already granted, the *result* has to reach
`Activity.onRequestPermissionsResult`, which arrives as a system→app activity result. ACOS has no
system-side activity-result delivery (`IApplicationThread` `scheduleTransaction` with an
`ActivityResultItem`; the repo already documents `ClientTransaction` parcel encoding as a V1 gap), and
answering `START_SUCCESS` without delivering a result would leave `mHasCurrentPermissionsRequest`
set forever — a hang dressed as success. So the pre-granted arm answers the same structured error with
`state: "granted"`, which is the truth an agent can act on ("already granted; the controller would
return without drawing"). Activity-result delivery is the first thing the permission-controller
system app needs (option B), and it belongs to the multi-process work.

## 4. Item 1 (notifications): what is missing and the exact next step

Nothing was written for `notifications` because two pieces of *dispatch/decoding* are missing, and
shipping a module that cannot receive a real `notify()` would be the fake progress ADR-0029 forbids:

1. **The `Notification` parcel has no codec.** `INotificationManager.enqueueNotificationWithTag`
   (`base/core/java/android/app/INotificationManager.aidl:58`) carries `in Notification`, and
   `parcelable::read_payload` has no spec for `android.app.Notification`, so today a real
   `NotificationManager.notify()` fails with `SERVICE_UNSUPPORTED_PARCELABLE`. Title/text live in the
   notification's `extras` (`android.title`, `android.text`), actions in a typed array, and the
   channel in `mChannelId`, all *after* ~22 nullable fields in `Notification.writeToParcelImpl`
   (`Notification.java:3185-3360`). The repo already has the oracle for this:
   `cargo run -p acos-services --example scan-parcelables -- .cache/boot/android-34/dex/framework
   android.app.Notification 'android.app.Notification$Action' android.graphics.drawable.Icon`
   prints the exact `writeToParcel` call order; `FieldOp::{Cond, Nested, TypedArray, ByteCond}`
   already express that shape, and the fixture controls which nested types are non-null.
2. **A `PendingIntent` cannot be created or fired.** `PendingIntent.writeToParcel` writes only a
   strong binder (`PendingIntent.java:1500`), so "a PendingIntent handle" is an `IIntentSender`.
   ACOS answers neither `IActivityManager.getIntentSender*` (table code 62/63,
   `crates/acos-aidl/src/generated/tables.rs:2068`) nor `IIntentSender.send`, so an app cannot create
   the content intent a `tap` would fire. The work: a sender record + a dynamic node per sender
   serving `IIntentSender.send(code, intent, resolvedType, whitelistToken, finishedReceiver,
   permission, options)`, whose handler calls the existing `AmsState::start_activity` with the stored
   intent — then `tap` is `transact(sender_binder, send)` and the app's target activity opens through
   the path LAYER1 already exercises.

With those two in place the module is mostly state: channels (the codecs exist — see
`tests/k2_notification.rs`), posted notifications with `(package, id, tag, title, text, channel,
actions[], content PendingIntent binder)`, `areNotificationsEnabled` =
`POST_NOTIFICATIONS` granted **when `targetSdk >= 33`** (else enabled), cancel/cancelAll,
`getActiveNotifications`, FGS notifications, and control ops `list`/`tap {id|text}`/`action {id,
index}`/`dismiss`. The POST_NOTIFICATIONS arm can read the shared store this change introduced, and
`PmsState::package.application.target_sdk_version` is already parsed.

Deliberately **not** done here: any window, dialog, shade, or host-painted pixel (option A), and any
`system_apps` entry (option B) — both are outside this track.

## 5. Evidence files

| File | Contents |
|---|---|
| `permissions-tests.txt` | `cargo test -p acos-services --test modnotif2_permissions` — 4/4 ok |
| `conformance-kit.txt` | `cargo test -p acos-services --test module_kit` — 11/11 ok, plus the per-check detail for `system.permissions` and `feedback.vibrator` |
| `before-after-endscope.txt` | the framework fix: the same test failing with the restore disabled and passing with it |
| `full-suite.txt` | `cargo test -p acos-services` — 98 lib + every integration test |
| `seams.txt` | the greps for the claims above (codes, AIDL, flags, missing sender/notification paths) |

## 6. Where the tests live

* `crates/acos-services/tests/modnotif2_permissions.rs`
  `a_module_that_takes_over_a_core_name_restores_core_on_detach` (item 3),
  `both_check_permission_paths_read_the_one_store` (item 2, incl. the rationale contract),
  `request_permissions_resolves_to_a_structured_permission_ui_unavailable`,
  `the_launch_config_pre_grants_and_pre_denies`.
* `crates/acos-services/tests/module_kit.rs` — `the_permissions_module_passes_the_conformance_kit`.
* `crates/acos-services/src/permissions.rs` — 3 unit tests for the store's semantics.
* `crates/acos-services/src/modules/permissions.rs` — manifest validity + takeover declaration.
