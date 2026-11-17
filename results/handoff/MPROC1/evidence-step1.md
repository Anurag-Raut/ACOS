# MPROC1 step 1 evidence — extra APKs installed into a session

Commit: this one (branch `mproc1`, base `mproc0` @ 4d793e73).

## What the code now does

- `LaunchOpts::extra_apks` (`crates/acos-art/src/launch.rs`) + repeatable
  `acos-art launch --extra-apk FILE`.
- Each extra APK is staged into the run's private `app/` tree (H17: never the
  shared `/data/app` entry), its `/data/app/<name>` mount point is created,
  `native_libs::install` extracts `lib/x86_64` into a **per-package** tree
  (`/data/app-lib-extra/<pkg>`), the ABI gate refuses an unsupported extra
  package before any guest exists, and the sandbox binds APK + lib tree at the
  guest paths the package manager publishes.
- The host installs every extra package into the PMS at its own uid
  (`install_extra_package_from_apk_as`; the launched APK keeps its state) and
  seeds the ATMS manifest table with that same uid
  (`seed_manifest_from_apk_with_uid`). The launch reply carries
  `extra_packages: [{package, uid, apk, native_library_dir}]`.
- CLI/MCP/scenario surfaces: `acos launch --extra-apk FILE` (ART only; the Rust
  VM refuses instead of installing nothing), MCP `acos_launch {extra_apks:[...]}`
  (launch-time: a live session refuses a different set), scenario
  `extra_apks:` (paths resolved like `app:`, missing file = structured
  `APK_IO` before any guest). `extra_apks` is part of the zygote key.
- **Root cause fixed for the class**: `IPackageManager.getApplicationInfo`
  (code 9) was answered from a guest-built reply template that ignored the
  requested package, so with a second package installed
  `getApplicationInfo(other)` / `createPackageContext(other)` returned the
  **launched** app's ApplicationInfo. The template is gone; the host package
  manager answers the package the caller named.

## Evidence

### Positive: real ART run, two packages, probe on screen

`acos verify scenarios/mproc1-multi-package.yaml --engine art` →
`verify: pass (ok=true, backend=acos-native, fidelity=art-reference)`, 6 steps.
Guest stdout (launch replies of the same run, before the scenario run):

```
ACOSPROBE ok pkg=dev.acos.fixtures.counter version=1.0 src=/data/app/counter.apk uid=10193
ACOSPROBE ai pkg=dev.acos.fixtures.counter flags=0x30083e46 src=/data/app/counter.apk
ACOSPROBE at-flags pkg=dev.acos.fixtures.counter src=/data/app/counter.apk
ACOSPROBE ctx appInfo=dev.acos.fixtures.counter pkg=dev.acos.fixtures.counter label= app_name=Counter
ACOSPROBE uid 10193 -> [dev.acos.fixtures.counter]
ACOSPROBE done
```

`app_name=Counter` is the **other APK's** string resource, reached through
`createPackageContext(...).getResources()`, and `src=/data/app/counter.apk` is
the guest path the sandbox binds. Before the fix the same lines read
`ACOSPROBE ai pkg=dev.acos.fixtures.multipkg src=/data/app/multi-package-probe.apk`
and `ctx ... app_name=?` — the launched package's data, i.e. the template bug.

Launcher log line for the same run:

```
acos-art: launch: extra package installed dev.acos.fixtures.counter uid=10193 apk=/data/app/counter.apk native=/data/app-lib-extra/dev.acos.fixtures.counter/x86_64
```

### Negative: no extra APK ⇒ honest missing, never a fake second package

`acos-art launch --apk fixtures/build/apps/multi-package-probe.apk` (no
`--extra-apk`):

```
ACOSPROBE missing dev.acos.fixtures.counter
ACOSPROBE done
```

### Regression: single-app scenarios

- `scenarios/hello-views.yaml` (`--engine art`): 24 steps, 23 pass; the only
  non-pass is the pre-existing `back: strict settle timed out
  (no-draw-after-action)` documented in `results/handoff/ROT1.md` (§ "back
  still ends in TIMEOUT no-draw-after-action").
- `scenarios/counter.yaml` (`--engine art`): 22 steps, 21 pass; same
  pre-existing `back` timeout only.

## Fixture

`fixtures/apps/multi-package-probe/` (built by
`ACOS_APP_ONLY=multi-package-probe tools/build-apps.sh`): prints one
`ACOSPROBE` line per fact and shows the same text in the `probe` TextView, so
the result is readable from logs, tree and pixels.
