# MPROC0 status (run 1, branch `mproc0`)

Commit: `e4460d4b` — MPROC0 step 1: PMS holds N installed packages; intent
queries match across them. Design: `results/handoff/MPROC0/DESIGN.md`.

## Done and verified

Step 1 (PMS part) — `crates/acos-services/src/pms.rs`,
`crates/acos-runtime/src/pms_java.rs`, `crates/acos-services/src/ams.rs`:

- `PmsState` is `packages: Vec<InstalledPackage>` + `primary`, not one package.
  `InstalledPackage` = app, package, providers, services, granted_permissions,
  installer_package, apk_path, and the package's `ams::PackageRecord`.
- `PmsState::install_extra(apk, guest_source_dir)` installs an extra package at
  the next free uid (10192+n); a duplicate package name is a structured
  `BadRequest`, never a silent replace.
- `getApplicationInfo`, `getPackageInfo`, `getActivityInfo`, `getServiceInfo`,
  `getProviderInfo`, `getPackageUid`, `getPackagesForUid`,
  `getInstallerPackageName`, `checkPermission` and `resolveContentProvider`
  resolve the *named* package. An unknown package still answers the wire's
  `null` (K3b), unchanged.
- `queryIntentActivities`/`resolveIntent` decode the `Intent`
  (`ams::IntentFacts`) and match every installed package's declared filters via
  `ManifestDb::matching_activities` (new); each match carries its own `AppInfo`
  and `DeclaredComponent`, so `pms_java` materialises a real `ResolveInfo` with a
  real `IntentFilter` per match (`launcher_*` helpers removed).
- `pms_java` materialises per-package `ActivityInfo`/`ProviderInfo` and the
  `queryContentProviders` list (each entry with its own application record).

Evidence:

- `evidence-step1-mproc_pms.txt`: `cargo test -p acos-services --test
  mproc_pms` → 9 passed, 0 failed.
- `cargo test -p acos-services` (all targets) and `cargo test -p acos-runtime`
  (all targets) pass on the shared ART slots; the pre-existing
  `k3_walls::unknown_package_is_a_null_package_info_not_an_exception` and
  `the_known_package_still_answers` still pass, so the wire contract for the
  single-package case is unchanged.

## Not done (next actions, in order)

1. **Launch plumbing for extra APKs** (~1 h): `LaunchOpts::extra_apks`,
   `acos-art launch --extra-apk` (repeatable), stage each into
   `<run>/app/<name>`, `ensure_apk_mountpoint`, `sb.ro_bind` at
   `/data/app/<name>`, `host.install_package_from_apk_as` for each, and
   `atms.seed_manifest_from_apk` for each; forward the flag from
   `acos-cli/src/engine/art.rs` and `--extra-apk` on `acos launch`.
2. **Guest fixture test** for the step-1 acceptance ("an app calls
   getPackageInfo/createPackageContext for a second installed package"): new
   `fixtures/apps/multi-package-probe` that calls
   `getPackageManager().getPackageInfo("dev.acos.fixtures.counter", 0)` and
   `createPackageContext("dev.acos.fixtures.counter", 0)`, prints
   `ACOSPROBE` lines; run it under `--extra-apk fixtures/build/apps/counter.apk`
   on an ART slot. This is the test that proves `createApplicationContext`
   works in the guest; nothing in the framework path needs changing beyond the
   per-package `ApplicationInfo.sourceDir` (already published as
   `/data/app/<name>`).
3. Step 2 (capture target + broker control ops), step 3 (second process +
   peer routing), step 4 (idle kill), step 5 (A/B driver, arms prepared but not
   run — bench owner runs it) as designed in DESIGN.md §4.

## Notes for the orchestrator

- Step 3 is the only architectural question: it needs the peer listener to
  accept N tagged connections and `ServiceHost` to carry a process id on the
  binder records it exports. That is a change inside the existing
  `acos-binder-peer` / `acos-services` seam (not a new broker design), and it
  is what DESIGN.md §4 proposes; confirm before it is built.
- Step 1's "system packages baked into the image" has no instance today: the
  image ships no package list, so `install_extra` is the whole mechanism; when
  an image package exists it is one more `install_extra` call with a fixed
  guest path.
