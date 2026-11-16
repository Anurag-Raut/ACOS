# MODNOTIF — BLOCKED (architectural): no host-rendered window class in ACOS

Branch `modnotif` (from `modules` = d5170146), 2026-11-16. **No feature code was
written**: both required deliverables need an architecture ACOS does not have,
and the brief says to stop and report instead of inventing it. This file is the
evidence and the two options; `aosp-permission-contract.txt`, `acos-seams.txt`
and `gaten-probe.txt` are the raw greps behind every claim.

## What the brief asked for, and where it stops

| Item | Verdict |
|---|---|
| `notifications` module: `INotificationManager` full enough for real apps (channels, notify/cancel, POST_NOTIFICATIONS, `areNotificationsEnabled`, FGS notifications, PendingIntent content/actions) | **possible** at the service layer, but it is half the deliverable |
| A **real notification shade window** (system type from the LAYER1 table), rendered **by ACOS (C1 window)**, with tree nodes per notification, agent taps firing the PendingIntent, control ops `shade open\|close`, `list`, `tap`, `dismiss` | **blocked**: C1 has no host-rendered layer, the tree has no host-node seam, and a host window has no input channel |
| `permissions` module: runtime state, `checkSelfPermission` / `shouldShowRequestPermissionRationale` semantics, **requestPermissions shows a real permission dialog** in the system-dialog band that the agent taps, `onRequestPermissionsResult` delivered exactly as on a phone | **blocked** for the dialog: AOSP's request path is an *Intent to a second installed package* (the permission controller app), which ACOS neither installs nor can run in this worktree; the decided alternative (ATMS intercepts it and shows a host dialog) is an ATMS/system-UI decision |
| Proof fixture `notif-perm-probe` + two scenarios + Gaten | **blocked** by the two rows above (a scenario cannot tap a dialog or read a shade that does not exist). Gaten was checked: see below |

## The two walls, with evidence

### 1. `requestPermissions` / POST_NOTIFICATIONS require a second package

AOSP API 34 (`aosp-permission-contract.txt`):

* `Activity.requestPermissions` builds `PackageManager.buildRequestPermissionsIntent`
  and calls `startActivityForResult(REQUEST_PERMISSIONS_WHO_PREFIX, intent, requestCode, null)`
  (`base/core/java/android/app/Activity.java:5673-5701`).
* That intent is `setPackage(getPermissionControllerPackageName())`
  (`base/core/java/android/content/pm/PackageManager.java:7049-7058`) — i.e. the
  dialog is an **Activity in the permission controller package**
  (`com.android.permissioncontroller`, `GrantPermissionsActivity`), not a system
  window the platform can draw by itself.
* The same intent is used for `POST_NOTIFICATIONS`
  (`base/services/core/java/com/android/server/policy/PermissionPolicyService.java:1329`):
  the notification permission prompt is the same contract.
* The Allow / Don't allow / "While using the app" / "Only this time" button set is
  `GrantPermissionsActivity`'s UI; the sparse AOSP cache has only
  `packages/modules/Permission/framework*` (AIDL), no controller app source
  (`find aosp -name GrantPermissionsActivity.java` → nothing).

ACOS today: `pms.rs:1708` answers `com.android.permissioncontroller` as the
permission-controller package name (K2/R3 pinned that), and the session installs
**one** APK (`acos launch <apk>`), with a single-package PMS and no system apps
(`git grep "system_apps: &\[" crates/` → every module `&[]`, `core` included).
So an app that calls `requestPermissions` today starts an implicit intent whose
package does not exist; there is no window to tap and no result to deliver.

The plan's decided shape ("runtime permission dialogs are real system windows
that agents answer with `tap`", PLAN §3.3) means ACOS's ATMS must (a) recognise
the `REQUEST_PERMISSIONS` intent, (b) show a host-owned dialog in the
system-dialog band, (c) resolve Which permission / Which answer, and (d) deliver
`onRequestPermissionsResult` back to the requesting Activity through the normal
activity-result path. (b) is wall 2; (a), (c), (d) are an ATMS + system-UI
architecture decision (who owns system dialogs, how a system dialog reports a
result to a guest `ActivityThread`, and how `Activity.requestPermissions`'s own
`mHasCurrentPermissionsRequest` state is reset).

### 2. C1 has no host-rendered window: pixels, tree nodes and input all come from the guest

* **Pixels.** `SurfaceState::capture_composite` composites only layers whose row
  carries a guest `FrameBuffer`; a layer without a buffer is skipped
  (`crates/acos-services/src/surface_client.rs:1041-1046`, "No pixels (a window
  surface or a destroyed layer)"). Layers are created by the guest's
  `ISurfaceComposerClient` transactions; there is no API to hand C1 a host-drawn
  bitmap, and the composite geometry is the vendored `acos_sfgeom` kernel fed
  from the guest's own layer placements.
* **Tree nodes.** The UI tree is the guest probe's read-only view walk
  (`ACOSPROBE`, `crates/acos-art/src/launch.rs:2074`, parsed at `:2884`;
  `ui_tree_from_json` at `crates/acos-cli/src/engine/mod.rs:1294`), and
  `apply_window_tags` only tags nodes the probe produced. Nothing can append a
  node that has no guest View behind it — which is exactly what "tree nodes for
  each notification, so agents read and tap them" needs, and what the scenario
  selectors (`assert_text`, `tap: {selector}`) read.
* **Input.** A WMS window is created by a guest `IWindowSession` client with an
  `IWindow` binder (`crates/acos-wm/src/wms.rs:291`) and a
  real `InputChannel`; LAYER1's tap routing hit-tests the stack and publishes on
  **that window's** channel (`crates/acos-services/src/input_dispatch.rs:575`,
  `crates/acos-art/src/launch.rs:5478`). A host window has no channel, so a tap
  on it is `INPUT_UNAVAILABLE`, not a PendingIntent.

So "the shade is a real window rendered by ACOS in C1" is a new **window class**
(host-owned content: pixels + nodes + input) in the compositor/window model.
That is on the brief's architectural-walls list ("compositor, … binder/
service-layer design"); the same applies to the permission dialog window.

## Options (orchestrator/owner decision)

**A — host-window class in ACOS** (what the brief/plan literally asks for).
Add to `acos-wm` a `HostWindow`: a `TYPE_NOTIFICATION_SHADE` / `TYPE_SYSTEM_DIALOG`
record whose z comes from the existing `policy_layer` table, whose pixels ACOS
paints (a small layout/paint pass into the composite, or a C1 "host layer"
buffers source), whose nodes the owning module emits into the tree as a second
root with `window_id`/`window_type` tags, and whose taps are dispatched to the
module's `control("tap")` instead of an `InputChannel`. Cost: a compositor
change (walls list → ADR), tree/verdict shape change (nodes not from the guest,
`source: "host-window"` in `verdict.json`), input routing branch in
`acos-art/src/launch.rs`. Benefit: shade and dialogs work with no second
process, and the same class serves the chooser (intents track) and any future
system UI.

**B — a system UI / permission controller app in the guest** (recommended).
Ship a tiny ACOS system app (`com.android.permissioncontroller` providing
`GrantPermissionsActivity`, plus `com.acos.systemui` providing the shade
Activity) installed like a real package and run in its own ART process. Then:
the framework's own `requestPermissions` intent resolves and
`GrantPermissionsActivity` draws the real dialog with the real button set; the
shade is a real Android window; tree nodes and taps need **no new compositor
code** (LAYER1's stack, probe and input path already handle multiple windows
with real types); `onRequestPermissionsResult` arrives through the normal
activity-result path. Cost: depends on the multi-process session foundation
(`mproc0`/`mproc1`) and a system-app install; the app must be built into
`fixtures/`/the image.

**C — service layer only, now.** `notifications` and `permissions` modules own
the state (channels, posted notifications with title/text/actions + a PendingIntent
handle, per-package runtime permission state consulted by
`ActivityManager.checkPermission`/`PackageManager.checkPermission`,
`shouldShowRequestPermissionRationale` flags, launch-config pre-grant/pre-deny)
with `list`/`tap`/`dismiss`/`grant`/`revoke`/`state` control ops and the
conformance kit. No shade window and no dialog, so the v0.4.0 acceptance row
("app posts a notification, agent opens the shade and taps it; app asks a
permission, agent grants and denies") is **not** met and must not be marked met.
This is the safe increment that makes A or B a smaller step, and it is what I
would do next if the orchestrator wants progress on the branch before deciding.

My recommendation: **B**, with **C** landing first (it is independent of the
window decision and every option needs it). A is only preferable if ACOS wants
host system UI as a product feature (it would also give the chooser for free).

## Also verified while reading (useful for whoever implements this)

* `INotificationManager` today is a recording stub: `NotificationState.posted`
  keeps only `(package, id)` and `areNotificationsEnabled*` always answers
  `false` (`crates/acos-services/src/declared.rs:591-596`, `:684-710`) — so it
  cannot back `list`/`tap`, and no `Notification` parcel (title/text/actions/
  `PendingIntent`) is decoded anywhere.
* `IPermissionManager` today answers `shouldShowRequestPermissionRationale =
  false` and *throws* `UNSUPPORTED_ANDROID_API` after recording
  `grantRuntimePermission`/`revokeRuntimePermission`
  (`crates/acos-services/src/declared.rs:306-349`).
* Permission checks are install-time only: `PmsState::check_permission` /
  `AtmsState::check_permission` consult `granted_permissions` from the APK
  manifest (`crates/acos-services/src/pms.rs:1244`, `atms.rs:829`); there is no
  runtime store either of them reads, so "grant changes `checkSelfPermission`"
  needs one shared store in `core` (a root-cause-shaped change: both check paths
  read the same table).
* Gaten (`gaten-probe.txt`): targetSdk 36, minSdk 29, no `POST_NOTIFICATIONS` in
  its manifest, **no notification posting** (`NotificationManagerCompat` only for
  `getEnabledListenerPackages` in `MediaPauser.kt`), and **no permission ask at
  onboarding** — one `requestPermissions` call on the "Add to Anki" button
  (`GatenAnkiModule.kt:409`) for AnkiDroid's `READ_WRITE_PERMISSION`. So the
  Gaten half of this brief has nothing to run until the fixture exists, and that
  call would hit wall 1 today.

## Disk / RAM

No build and no ART run were made (nothing to build). `df -h /` at start:
945G total, 139G free, **85 % used** (the rules' stop line is 85 %; this report
files did not change it). No target directory was created, nothing to delete.
