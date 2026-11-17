# MPROC0 design note (v0.4.0 foundation 3)

Status: design + step 1 (multi-package PMS) implemented on branch `mproc0`.
Written from the code at `main` (`3c2e0750`), not from the plan text.

## 1. How a session launches its single app process today

Production path (`--engine art`):

1. `acos-cli/src/engine/art.rs` spawns **one** `acos-art launch --interactive --apk <apk>`
   child in its own process group and speaks line-JSON over its stdin/stdout.
2. `acos-art/src/launch.rs::launch_session` does the session prefix:
   - `root.new_run_dir("launch")` (or the dir named by `ACOS_ZYGOTE_SESSION`),
     `stage_apk` copies the APK to `<run>/app/<name>`, `ensure_apk_mountpoint`
     creates the shared `/data/app/<name>` mount point. The guest path is
     `/data/app/<name>`; the host copy is bound there read-only (H17: the shared
     `/data/app` entry is never written).
   - `native_libs::install_from` extracts `lib/<abi>/*.so` into `<run>/app-lib`
     (bound at `/data/app-lib`), `apk_facts::prime_from` caches manifest facts,
     `PmsState::from_opened` parses the package once.
   - `ServiceHost::new()` + `registry::install_with_handles` build the standard
     service graph (lazy `register` closures, ~50 services; PMS seeded with the
     `hello-views` fixture by `registry.rs`).
   - `host.install_package_from_apk_as(apk, Some("/data/app/<name>"))` replaces
     the PMS state with the real APK, and `atms.seed_manifest_from_apk` seeds the
     ATMS `ManifestDb` with the same package.
   - the sandbox (`acos-sandbox` bwrap + seccomp + landlock) binds the shims,
     the APK, `<run>/socket` → `/dev/socket`, and the guest is
     `dalvikvm64 ... dev.acos.art.AppMain /data/app/<name> <package> ...`.
3. The guest's `libacosart.so` binder driver connects to `<run>/sock/peer.sock`
   (guest `/dev/socket/peer.sock`). The launcher's peer thread runs
   `acos-binder-peer::Peer::with_host(ServiceHost)`; `Peer::serve` accepts
   **one** client and is the only path into the service layer.

Zygotes: `zygote_prod.rs` (`ACOS_ZYGOTE_PLAN_DIR`, owner `acos serve`) is a
keeper that forks tagged children from a pre-fork ART core; `host_zygote.rs`
(`ACOS_HOST_ZYGOTE_PLAN`) forks whole launchers from a warm host template. Both
are env-gated; with the variables unset the launch is exactly the direct exec
above. Either way **one launcher serves exactly one guest** (one peer
connection, one `ServiceHost`).

## 2. How binder transactions are routed by the Rust service layer

- Guest → host: driver sends `F_REQ` (48-byte header: type, seq, code, flags,
  target, data_size, offsets_size; parcel data + flat object table + fds in
  `SCM_RIGHTS`) on the `SOCK_SEQPACKET` socket (`acos-binder-peer/src/protocol.rs`).
- `peer.rs` maps `target` (peer node id, `0` = servicemanager) to an
  `acos_parcel::BinderId` (`NODE_TAG` for host service nodes, `JAVA_TAG` for
  guest local binders) and calls `ServiceHost::transact(binder, code, data,
  reply, flags)`.
- `acos-services/src/host.rs::transact_inner` resolves the descriptor from the
  interface token, finds the `ServiceObject` for that interface in the registry
  (`registry.rs`, descriptor → factory closure), decodes args from the parcel
  via the generated method table (`parcelable.rs` specs, scanned from the
  image) and calls `ServiceObject::call(interface, method, args)`; the reply is
  encoded back into the reply parcel (or a `Parcelable::Null` typed object that
  `pms_java`/`ams_java` rewrite through the real Java `writeToParcel`).
- Host → guest: `ServiceHost::take_deliveries` becomes `F_TXN` frames addressed
  at the guest binder's driver handle. Unknown `(descriptor, code)` is a
  structured `UNSUPPORTED_ANDROID_API` log line + exception reply, never a
  success.

So the service layer is already the system_server: it owns every service
object, every binder handle, and the only socket into the guest. A second guest
process needs no new broker design — it needs (a) a second connection, (b) a
process tag on the connection, (c) a per-process routing table for deliveries
and for the `BinderId`s that name a guest binder.

## 3. What PMS knows today

`acos-services/src/pms.rs`:

- `PmsState` is **one** package: `app: AppInfo`, `package: PackageInfo`,
  `providers`, `services`, `granted_permissions`, `installer_package`,
  `apk_path`, plus session-wide `features`, `uid_packages`, `dex_loads`,
  `component_settings`. Built by `PmsState::from_opened(path, &Apk)`; uid is the
  constant `FIXTURE_UID = 10192`.
- `PmsService::lookup` matches `Some(p) if p == state.package.package_name`,
  `""`/`None` → the single package, anything else → `UnknownService`, which the
  handlers turn into a **null** `ApplicationInfo`/`PackageInfo` (the image's
  `IPackageManager` contract; `ApplicationPackageManager` raises
  `NameNotFoundException`).
- `acos-services` cannot see the VM, so `PmsService::call` stashes
  `PmsDescription { app, package }` in a thread-local (`cache_description`) and
  `acos-runtime/src/pms_java.rs::rewrite_reply` materialises the real Java
  `ApplicationInfo`/`PackageInfo`/`ActivityInfo`/`ResolveInfo` from it and calls
  the object's own `writeToParcel`. The cache holds **the one** package, so a
  reply for any other package would materialise the wrong `sourceDir`/uid.
- `queryIntentActivities`/`resolveIntent` answer with the launcher
  `ResolveInfo` only (`launcher_resolve_info`: the activity whose name ends in
  `.MainActivity`), ignoring the intent argument; `resolveService`/
  `queryIntentServices` are `UNSUPPORTED_ANDROID_API`.
- ATMS already has the multi-package model PMS lacks:
  `ams.rs::ManifestDb` holds `Vec<PackageRecord>` (activities/services/receivers/
  providers with reduced `IntentFilter`s) and `IntentFacts::from_parcelable`
  decodes `android.content.Intent`. `atms.seed_manifest_from_apk` seeds exactly
  one record today.

## 4. Design for the five MPROC0 steps

**Step 1 — multi-package PMS (implemented).**
`PmsState` holds `packages: Vec<InstalledPackage>` (`app`, `package`,
`providers`, `services`, `granted_permissions`, `installer_package`, `apk_path`,
and the package's `ManifestDb` record) plus `primary: usize`. The primary keeps
today's semantics for `None`/`""` lookups; every handler resolves across all
records and caches the **resolved** record for `pms_java`, so
`getPackageInfo`/`getApplicationInfo`/`getActivityInfo`/`getProviderInfo`/
`getPackageUid`/`getPackagesForUid`/`getInstallerPackageName` are per package.
`queryIntentActivities` decodes the `Intent` (`IntentFacts`), matches every
package's declared filters through the package's `ManifestDb` record, and caches
the matched `(package, class)` list for `pms_java` to materialise one
`ResolveInfo` per match. `createPackageContext` needs nothing beyond this: the
framework calls `getApplicationInfo(otherPackage)` + `getResourcesForApplication`
(`sourceDir`), so the other APK must be staged and bound at
`/data/app/<name>.apk` in the guest, and its `ApplicationInfo.sourceDir` must be
that guest path. Extras are installed at launch (`--extra-apk`, repeatable;
`LaunchOpts::extra_apks`) with a distinct uid per package (10192+n, AOSP gives
each package its own uid). System packages "baked into the image" are the same
mechanism with a fixed guest path; no such package exists in the image yet, so
the launch plumbing is the only missing piece and it is the same call.

**Step 2 — intent broker / capture target.**
Resolution failure for an implicit `ACTION_SEND`/`ACTION_VIEW http`/
`ACTION_PICK image` must not become `ActivityNotFoundException` while the plan
promises a capture target. Add a broker module in `acos-services` (registered on
the existing seam) that ATMS/PMS consult after `ManifestDb` returns no match:
it records `IntentFacts` (action, data, type, extras, caller, requestCode) and
replies with a synthetic component `com.acos.capture/.Capture` that has no
process. Control op `acos_service_ctl {module:"intents", op:"last"}` reads the
record; `op:"result"` scripts the reply for `startActivityForResult` (the photo
a picker returns), delivered as the ATMS result transaction the framework
already models.

**Step 3 — second process on demand.**
`launch_session` keeps the one-guest fast path. On the first intent/bind/
provider call that resolves to a component of an installed extra APK (or a
declared `android:process` of the launched APK), the launcher spawns a tagged
child through the existing zygote contract (`zygote_prod` keeper FIFO
`spawn <tag>`; the child gets its own mount/pid namespace and run root, as
ZPROD P2 already does), with the second APK bound at `/data/app/<name>.apk`.
The peer listener accepts N connections, each prefixed with its process tag;
`Peer` keeps `tag -> BinderId` tables so deliveries and reverse transactions are
addressed to the owning process, and `ServiceHost` gains a process id on the
binder records it exports. ATMS gains one `ProcessRecord` per tag (activities
stacked per process; foreground = the process of the resumed activity) so
`ActivityTaskManager` state and `IActivityManager` process state agree.

**Step 4 — idle kill.**
The launcher owns a per-tag idle clock driven by ATMS foreground transitions
(default 30 s, `--idle-kill-seconds`). On expiry: deliver
`IApplicationThread.scheduleSaveInstanceState` (and the `onSaveInstanceState`
lifecycle the framework already runs), wait for the ack, `kill <tag>` through
the zygote FIFO (never `pkill`), drop the tag's binder tables and report
`killed: {tag, reason:"idle", saved:true}` in the reply and in `verdict.json`.
The next call that resolves to that package re-spawns the tag from the zygote.

**Step 5 — cost.**
Nothing above runs when no second package is installed and no intent resolves
outside the launched package: one extra `Vec` entry, one `IntentFacts` decode
per `queryIntentActivities`, and no extra process, thread or socket. The A/B
arms are single-app launch + verify on `main` and on `mproc0`; the driver is
`tools/bench` (prepared, not run here per the common brief).
