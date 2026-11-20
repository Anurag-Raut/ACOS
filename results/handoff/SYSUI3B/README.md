# SYSUI3B — real permission labels: `IPermissionManager.getPermissionInfo` / `getPermissionGroupInfo`

Branch `sysui3b` (from `sysui2`, `2f8d2141`), 2026-11-17. Every ART run was private
(`ACOS_RUNTIME_DIR=$PWD/.cache/rt-sysui3b`, `ACOS_SESSION_SOCKET=$PWD/.cache/sysui3b-session.sock`,
`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`) and went through `tools/art-runtime/run-exclusive.sh --task SYSUI3B`
+ `tools/guarded.sh`.

| Item | State |
|---|---|
| 1. `PermissionStore` owns labels/groups/descriptions for the 42 dangerous platform permissions + 16 groups (AOSP source `file:line` per row, generated) | **DONE** |
| 2. `IPermissionManager.getPermissionInfo` / `getPermissionGroupInfo` (`+ queryPermissionsByGroup`, `getAllPermissionGroups`) answer from it | **DONE** |
| 3. App-declared `<permission>` labels from the manifest (`acos-apk` parses them, PMS publishes them into the store) | **DONE** |
| 4. `android.content.pm.PermissionGroupInfo` codec, verified against the image dex **and** the real Java `writeToParcel` | **DONE** (`Full 17/17`, Java differential passes) |
| 5. Dialog shows real labels, last-segment fallback only when the store answers `null` (still logged) | **DONE** |
| 6. `sysui1-permission-allow` / `-deny` pass with the real label text; `notif-perm-granted` / `-denied` pass | **PASS (4/4)** |
| `cargo test -p acos-services -p acos-wm` | **PASS** (726 passed, 0 failed) |
| `cargo test -p acos-art` | **PASS** (225 passed, 0 failed, 1 pre-existing ignore) |
| `cargo test -p acos-runtime --test parcelable_java` (real-Java differential) | **PASS** (2/2) |

## 1. What was wrong

The dialog fixture already did the right thing:

```java
PermissionInfo info = getPackageManager().getPermissionInfo(permission, 0);
CharSequence label = info.loadLabel(getPackageManager());
```

but the service answered `UNSUPPORTED_ANDROID_API` at
`android.permission.IPermissionManager code 3 (getPermissionInfo)` (SYSUI2 §3), so the
`catch` fell through to `permission.substring(lastIndexOf('.') + 1)` and the screen said
`CAMERA` / `ACCESS_FINE_LOCATION` instead of the platform's labels.

`PackageManager.getPermissionInfo(String, int)` reaches the hidden interface through
`ApplicationPackageManager` -> `PermissionManager` ->
`IPermissionManager.getPermissionInfo(String, String, int)` (code 3) — **not**
`IPackageManager`; that is why the fix belongs in the permissions module, which already
owns the `permission` service name.

## 2. The catalog (AOSP's own words, with sources)

`crates/acos-services/src/permission_catalog.rs` is **generated** by
`tools/gen-permission-catalog.py` from the AOSP source cache (tag `android-14.0.0_r75`):

* `base/core/res/AndroidManifest.xml` — every `<permission android:protectionLevel="dangerous…">`
  (42 of them) with its `android:label` / `android:description` resource, the
  `android:protectionLevel` bits and `android:backgroundPermission`; and every
  `<permission-group>` (16, including `UNDEFINED`).
* `base/core/res/res/values/strings.xml` — the `permlab_*` / `permdesc_*` /
  `permgrouplab_*` / `permgroupdesc_*` strings (`product="default"` or untagged = the
  English phone). AAPT's whitespace collapsing is applied (multi-line values become one
  space).

Every row carries `source` and `string_source` (`file:line`), so the text is checkable;
`cargo test -p acos-services` asserts they are present and that CAMERA is
`"take pictures and videos"` / group `CAMERA` / `dangerous|instant`.

**Permission → group membership** is the permission controller's platform table
(`packages/modules/Permissions`, `PermissionGroups`/`Utils`): the API-34 core manifest
declares *every* runtime permission with
`android:permissionGroup="android.permission-group.UNDEFINED"`, and the controller is what
maps names to groups. The module's source is **not** in the sparse cache; the mapping is
transcribed in the generator (`TRANSCRIBED_GROUPS`) and the group names, labels and
descriptions around it are sourced from `AndroidManifest.xml`/`strings.xml`. This is the
one deviation from "everything from the cache" and it is visible in the file.

## 3. The store, not a second table

`PermissionStore` gains the *description* half (the runtime state is unchanged):

* `permission_description(name)` — catalog first, then the installed package's own
  `<permission>` declaration; `None` for an unknown permission (honest: the framework
  turns `null` into `NameNotFoundException`).
* `group_description(name)`, `permissions_in_group(name)`, `group_descriptions()` — all
  from the same catalog.
* `set_declared_permissions(package, declarations)` — published by the PMS at the same
  moment as `set_install_time` (registry install + `set_package_from_apk_as`), so a custom
  permission and `checkPermission` never disagree about the package.

`acos-apk` now parses `<permission>` (`name` normalised like
`PackageParser.parsePermissionName`, `label`, `description`, `permissionGroup`,
`protectionLevel`, `backgroundPermission`) into `Manifest::permissions`; a `@string/…`
label reference stays unset (resolving it needs the app's resource table — a documented
gap, and the dialog's logged fallback is what a device would do for a permission it cannot
describe).

## 4. The wire (`PermissionGroupInfo`) — image dex + real Java

`crates/acos-services/src/parcelable.rs` gains
`android.content.pm.PermissionGroupInfo`: the 9-field `PackageItemInfo` prefix this image
writes (`name`, `packageName`, `banner`, `label`, `icon`, `labelRes`, `metaData`,
`showUser`, `showBadge`) then `descriptionRes`, `requestRes`,
`requestDetailResourceId`, `backgroundRequestResourceId`,
`backgroundRequestDetailResourceId`, `nonLocalizedDescription`, `flags`, `priority`.

Two independent checks, both green:

* image dex scanner (`crates/acos-services/tests/parcelable.rs::codec_field_orders_match_the_framework_dex`):
  `android.content.pm.PermissionGroupInfo  Full  17/17  real=17`;
* real Android-14 Java (`crates/acos-runtime/tests/parcelable_java.rs`, new
  `permission_group_info` case): the real `PermissionGroupInfo.CREATOR` reads the bytes the
  store writes, the real `writeToParcel` re-emits them byte-for-byte, and the decoded
  `name` / `loadLabel(null)` / `descriptionRes` / `priority` are asserted
  (`results/handoff/SYSUI3B/parcelable-java.log`).

The reply `PermissionInfo` sets `nonLocalizedLabel` / `nonLocalizedDescription` (and
`labelRes`/`descriptionRes` 0), which is exactly what makes the framework's own
`PermissionInfo.loadLabel`/`loadDescription` answer the text with no resource resolution.
`knownCerts` uses the same leading int the already-verified `PermissionInfo` codec writes.

## 5. The dialog (and what it now shows)

`fixtures/apps/permissioncontroller/.../GrantPermissionsActivity.java`:

* the row text is `info.loadLabel(pm)` — the store's `nonLocalizedLabel`;
* the group title is resolved through `getPermissionGroupInfo(info.group, 0)` and logged
  (so the group codec is exercised end to end, and a group failure is visible);
* the last-segment fallback is kept **only** for a permission the store answers `null`
  for, and it still logs `label fallback for …`.

ART logcat, `sysui1-permission-allow`:

```
AcosPermController: permission android.permission.CAMERA label=take pictures and videos group=Camera
AcosPermController: permission android.permission.ACCESS_FINE_LOCATION label=access precise location only in the foreground group=Location
```

No `group fallback` / `label fallback` line appears in any of the four arms.

## 6. Evidence in this directory

| file | what |
|---|---|
| `sysui1-permission-allow/`, `sysui1-permission-deny/` | the dialog arms: `verdict.json` = `pass` (17/17 and 13/13 steps), the real labels asserted on `permission_0`/`permission_1`, logcat has the label+group lines |
| `notif-perm-granted/`, `notif-perm-denied/` | regressions: `verdict.json` = `pass` |
| `cargo-test-services-wm.log` | `cargo test -p acos-services -p acos-wm`: 726 passed, 0 failed |
| `cargo-test-art.log` | `cargo test -p acos-art`: 225 passed, 0 failed, 1 pre-existing ignore |
| `parcelable-java.log` | the real-Java differential including the new `permission_group_info` case |
| `cargo-test-permission-info.log` | the new `sysui3b_permission_info` integration test (full service round trips) |

Reproduce (from the worktree, after the test-env symlinks):

```
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/rt-sysui3b ACOS_SESSION_SOCKET=$PWD/.cache/sysui3b-session.sock
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv ACOS_ART_BIN=$PWD/.cache/sysui3b-target/release/acos-art
tools/art-runtime/run-exclusive.sh --task SYSUI3B -- tools/guarded.sh --mem 10G --time 600 -- \
  env -u ACOS_HOME .cache/sysui3b-target/release/acos verify scenarios/sysui1-permission-allow.yaml \
  --engine art --json --artifacts results/handoff/SYSUI3B/sysui1-permission-allow --overwrite
```

## 7. Not touched, and open items

* The shade / pre-granted launch path is SYSUI3A's; nothing in this change touches
  `atms.rs`, the launcher, or `sysui1-permission-pregranted`.
* `getPermissionInfo`'s `packageName` argument is accepted and not used to scope the
  catalog (the store describes one installed package); the platform answer does not depend
  on it for a platform permission.
* `queryPermissionsByGroup`/`getAllPermissionGroups` answer the catalog only (plus
  app-declared permissions in the requested group); the dialog does not use them today.
* A custom permission whose label is a `@string/…` resource is reported with
  `nonLocalizedLabel = null` (the app's resource table is not resolved); the dialog then
  logs its fallback.
