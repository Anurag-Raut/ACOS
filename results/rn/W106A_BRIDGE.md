# W106-A — RN legacy bridge: get past `getThemeFreeFunction`

Worktree: `.cache/wt-w106a` (HEAD `6286d7d`), applied to `/home/Qui/Projects/acos`
with `git apply --check` + `git apply --binary` (patch `/tmp/w106a.patch`,
`/tmp/w106a-dex.patch`). Not committed.

## What runs

| Suite / check | Result | Notes |
|---|---|---|
| `cargo test -p acos-sandbox --lib` | **7/7 pass** | includes new `landlock::tests::device_nodes_accept_read_only_path_rules` and `directory_only_rights_are_masked_for_files` |
| `cargo test -p acos-jni --test rn_bridge` | **2/2 pass** | child confined; bridge now gets past `AssetManager.getThemeFreeFunction` |
| `cargo test -p acos-jni --test rn_hermes` | **2/2 pass** | |
| `cargo test -p acos-jni --test rn_native_stack` | **2/2 pass** | |
| `cargo test -p acos-jni --test flutter_hello` | **2/2 pass** | |
| `cargo clippy -p acos-jni -p acos-sandbox --all-targets --no-deps -- -D warnings` | **clean** | |
| `cargo check --workspace --all-targets` | **clean** | |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** | |

## How far the bridge gets

`BridgeProbe` under confinement:

1. `AssetManager.addAssetPath` / context / `UIManager` module / `PlatformConstants` /
   `DeviceInfo` / `AppState` all OK (`modules count=4`).
2. `SoLoader.init` OK; sources registered:
   `BackupSoSource`, `ApplicationSoSource`, `DirectorySoSource`, `DirectorySoSource`.
3. `SoLoader.isInitialized=true`.
4. `HermesExecutorFactory.create` → `HermesExecutor.<clinit>` →
   `SoLoader.loadLibrary("hermes_executor")` fails:
   - `ExceptionInInitializerError` wrapping
     `com.facebook.soloader.NoBaseApkException: Base apk does not exist: .../rn-hello.apk`
   - underlying `java.io.IOException: canonicalize0(<scratch>/lib): EACCES (Permission denied)`.
5. Bridge therefore does **not** reach `CatalystInstanceImpl.Builder.build`,
   `initializeBridge`, `loadScriptFromAssets`, `runJSBundle`, or
   `AppRegistry.runApplication("rnhello")`.
6. **Captured UIManager ops: 0.** No `ACOS_RN_OP` lines; `libraries=[]`,
   `registered_natives=0`.

## Fixes landed in this run

### 1. `AssetManager.getThemeFreeFunction` blocker — resolved
The real DEX says `android.content.res.AssetManager.getThemeFreeFunction()J` is a
**Java static** method (access `0x0008 STATIC`, no `NATIVE`) whose body calls
`nativeGetThemeFreeFunction()J`. The probe DEX carried a compile-time
`AssetManager` stub (only `<init>`/`addAssetPath`) and the VM's `load_class`
found that stub **before** the lazy `framework.jar`, so the VM raised
`UNSUPPORTED_JAVA_API: ...getThemeFreeFunction()J`.

Fix in `crates/acos-jni/tests/rn_bridge.rs`: preload
`Landroid/content/res/AssetManager;` from the lazy boot provider before adding
the APK/probe DEXs. The real framework class then wins and
`ResourcesImpl.<clinit>` completes. The speculative
`fwk_res` registration for `getThemeFreeFunction` was removed; no VM native
registration is needed.

### 2. Landlock non-directory rights — resolved
`crates/acos-sandbox/src/linux/landlock.rs`: added `DIRECTORY_ONLY_RIGHTS`
(`READ_DIR`, `REMOVE_DIR`, `REMOVE_FILE`, `MAKE_CHAR/DIR/REG/SOCK/FIFO/BLOCK/SYM`,
`REFER`) and mask them per opened object type (`fstat` → `S_IFDIR`), so a
read-only grant for `/dev/urandom` and `/dev/null` carries only file-applicable
rights. Unit test opens a real ruleset and adds rules for both device nodes
without applying it to the test process. The confined child now reports the
Landlock layer active.

### 3. SoLoader path/symlink handling — partial
The child now copies the fixture libs and APK into the writable scratch dir and
resolves symlinks before building the `ConfineProfile`, because SoLoader's
`File.canonicalize0` walks the real ancestor chain. This removed the
worktree-symlink variant of the failure but not the remaining temp-dir ancestor
grant.

## Open blockers

| Blocker | Owner | Detail |
|---|---|---|
| `SoLoader` `canonicalize0(<scratch>/lib)` EACCES | `acos-sandbox` (Landlock) / `acos-jni` (test) | The child grants `/tmp` read-only and the scratch subtree read-write, but `File.canonicalize0` still gets EACCES resolving the scratch lib dir. Likely needs the `/` ancestor readable (too broad to grant) or a pre-confine canonical path / a dedicated `/tmp` subdir whose ancestor chain is already granted. |
| `NoBaseApkException` (consequence) | `acos-sandbox` | SoLoader's recovery checks the APK after the lib-dir canonicalization fails; both paths live under the same confined temp tree. |
| `HermesExecutorFactory.create` not reached past SoLoader | `acos-jni` bridge harness | Once canonicalization is fixed, next expected blocker is `SoLoader.loadLibrary("hermes_executor")` → `nativeLoad` → merged-So mapping. |
| `CatalystInstanceImpl.Builder.build` / `initializeBridge` / `loadScriptFromAssets` / `runJSBundle` / `AppRegistry.runApplication` | `acos-jni` / `acos-vm` bridge | Not exercised yet; depends on the SoLoader blocker. |
| UIManager op capture (`LoggingUIManagerModule` JSON op list) | `acos-jni` / `acos-vm` | No ops captured; the test currently only asserts confinement. |
| `ExceptionsManagerModule`/`TimingModule` NPE (`devSupportManager` null) | `acos-jni` fixture | Probe skips these modules; not on the UIManager path. |

## Notes

- `AssetManager.getThemeFreeFunction` is **not** native in the real framework;
  registering it in `fwk_res` would have been the wrong fix. The real
  `nativeGetThemeFreeFunction()J` registration already existed and is enough.
- The probe DEX's `AssetManager` stub is the source of the class-shadowing bug;
  a cleaner long-term fix is to keep compile-time stubs out of the runtime DEX.
- Landlock fix stays tight: no broad `/dev` grant; only `/dev/urandom` and
  `/dev/null` are granted, with directory-only rights masked.
