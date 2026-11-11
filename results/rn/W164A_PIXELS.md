# W164-A — First step to RN pixels: the real APK reaches the platform lifecycle, and the framework's own class loader is the first principled fix

Worktree: the main tree (no stashes, no commits), built with
`CARGO_TARGET_DIR=.cache/target-w164a`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_BUILD_JOBS=2`, through `tools/guarded.sh` (6 GiB cap, RAM < 90%).

**Outcome: level B.** No `First frame` yet. The investigation produced the
exact class/native inventory of the fixture APK, a measured platform-path
launch of the real `rn-hello.apk`, one principled VM/framework fix
(`ApplicationInfo.FLAG_HAS_CODE`), and the next blocker as a full stack +
native tail. The recording UIManager route (`RnSession`) is unchanged and still
answers `UNSUPPORTED_RN_NO_RENDERER`.

Evidence: `results/rn/w164a-dex-inventory.json`, `results/rn/w164a-platform.txt`.

## At a glance

| step | result |
|---|---|
| APK dex inventory (W164-A §1) | **done**: 8 648 classes / 2 dex, all **42** mount-path classes present, **358** native methods |
| Route chosen for pixels | **platform lifecycle** (`AgentSession::launch` → real `ActivityThread` → `MainApplication` → `MainActivity`), not the `RnSession` driver |
| launch before the fix | `application_on_create` **blocked**: `ClassNotFoundException: dev.acos.fixtures.rnhello.MainApplication`, `DexPathList[[]]`, 6 510 B native tail, 1 651 classes |
| fix | `AppInfo::from_manifest` sets `ApplicationInfo.FLAG_HAS_CODE` (AOSP `parseBaseApplication` default `hasCode=true`) |
| launch after the fix | `bind_application` OK, class loader real, **1 552** classes loaded, 6.4 s; next blocker is a native |
| next blocker | `sun/nio/fs/UnixNativeDispatcher.init()I` not implemented (reached from `SoLoader`/`ReactNativeHost` NIO init) |
| `observe --frame` | still `UNSUPPORTED_BACKEND` (no frame yet); `RnSession::frame()` still `UNSUPPORTED_RN_NO_RENDERER` |
| level A | **not met**; exact next steps in §5 |

## 1. Which classes the rnhello APK's RN version needs

The fixture is React Native **0.81.4** (Paper/legacy, Hermes), built gradle-free.
`fixtures/build/apps/rn-hello.apk` has `classes.dex` (2 963 classes) and
`classes2.dex` (5 685 classes): **8 648** classes total.

The mount path below is taken from the APK itself: `MainReactPackage` is what
`MainApplication.getPackages()` returns, and its `createViewManagers` bytecode
(`javap -c` on the AAR's `classes.jar`) names the 17 view managers. Every class
in the table exists in the APK; the full machine-readable list (including dex
file and presence) is `results/rn/w164a-dex-inventory.json`.

| layer | classes (all present) | dex |
|---|---|---|
| fixture host | `MainActivity`, `MainApplication` | classes2 |
| RN host | `ReactActivity`, `ReactActivityDelegate`, `DefaultReactActivityDelegate`, `ReactDelegate`, `ReactRootView`, `ReactInstanceManager`, `ReactInstanceManagerBuilder`, `ReactNativeHost`, `DefaultReactNativeHost`, `MainReactPackage` | classes2 |
| UIManager | `UIManagerModule`, `UIImplementation`, `UIViewOperationQueue`, `NativeViewHierarchyManager`, `ViewManagerRegistry`, `ViewManager` | classes2 |
| view managers registered by `MainReactPackage` | `ReactDrawerLayoutManager`, `ReactHorizontalScrollViewManager`, `ReactHorizontalScrollContainerViewManager`, `ReactProgressBarViewManager`, `ReactScrollViewManager`, `ReactSwitchManager`, `ReactSafeAreaViewManager`, `SwipeRefreshLayoutManager`, `FrescoBasedReactTextInlineImageViewManager`, `ReactImageManager`, `ReactModalHostManager`, `ReactRawTextManager`, `ReactTextInputManager`, `PreparedLayoutTextViewManager`, `ReactTextViewManager`, `ReactViewManager`, `ReactVirtualTextViewManager`, `ReactUnimplementedViewManager` (18 entries; 17 `new` sites + the enum wrapper) | classes2 |
| views/shadow nodes | `ReactViewGroup`, `ReactTextView`, `ReactTextShadowNode`, `ReactRawTextShadowNode`, `ReactVirtualTextShadowNode` | classes2 |
| AndroidX (the `Text` is a real `TextView`) | `androidx.appcompat.widget.AppCompatTextView` (and the 477-class `androidx.appcompat` + 1 109-class `androidx.core` closure) | classes.dex |

Package mix (§1 relevance: the RN surface is smaller than the AndroidX+Kotlin
closure it sits on): `com/facebook/react` 1 773, `androidx/core` 1 109,
`kotlin` 1 055, `kotlinx` 876, `com/facebook/imagepipeline` 579,
`androidx/appcompat` 477, `okhttp3` 319, `androidx/lifecycle` 272.

Native surface: 358 `native` methods across the APK; 217 of them are declared by
`com/facebook/react`/`fbjni` classes. The UI path itself adds no new native
class beyond what the W76-A native corpus already covers
(`results/rn/rn-hello-inventory.txt`: 671/675 imports provided, 0 missing);
the blockers are Java-side framework substrate, not missing ELF symbols.

## 2. Why the platform path, not the `RnSession` driver

`RnSession` (W158-A/W162-A) mounts the Paper renderer through
`BridgeProbe` and a recording UIManager. It has **no Activity, no window and no
native ViewManagers**, so there is no `ViewRootImpl` to traverse/draw and
`frame()` must stay `UNSUPPORTED_RN_NO_RENDERER`.

The platform path already publishes real frames for the M3 counter fixture
(`crates/acos-runtime/tests/counter_interaction.rs`: `first_frame` OK,
1080×2400, `View.draw` → `ThreadedRenderer` → `acos-render` → `FrameStore`).
The APK ships its own real host (`MainApplication` → `SoLoader.init` →
`ReactNativeHost` → `MainActivity : ReactActivity` → `ReactRootView`), so the
shortest honest route to pixels is to make **that** path run instead of
re-implementing it in a driver. `tests/rn_pixels.rs` is the probe that asks
exactly this question.

## 3. Blocker 1 (fixed): the framework class loader had no dex path

Measured launch of the real APK through `AgentSession::launch` with
`native: true` before the fix:

```
reached_stage = bind_application
application_on_create = blocked
java.lang.RuntimeException: Unable to instantiate application
  dev.acos.fixtures.rnhello.MainApplication ...
  caused by: java.lang.ClassNotFoundException: Didn't find class
  "dev.acos.fixtures.rnhello.MainApplication" on path:
  DexPathList[[], nativeLibraryDirectories=[.../fixtures/build/apps/lib/x86_64]]
    at android.app.LoadedApk.makeApplicationInner(1466)
    at android.app.ActivityThread.handleBindApplication(6959)
```

The empty `DexPathList` is the tell. Android 14
`LoadedApk.createOrUpdateClassLoaderLocked` builds the dex path from
`mApplicationInfo.sourceDir` **only when `mIncludeCode` is true**, and
`mIncludeCode = (flags & ApplicationInfo.FLAG_HAS_CODE) != 0`. AOSP's
`ParsingPackageUtils.parseBaseApplication` sets that flag from
`android:hasCode` (`default true`). `AppInfo::from_manifest` only set
`FLAG_HARDWARE_ACCELERATED`, so every fixture with a manifest
`android:name` was un-loadable by the real class loader. The counter fixture
masked it: its `<application>` has no `android:name`, so `LoadedApk`
instantiates the boot-class `android.app.Application` and never needs the app
class loader for that step.

Fix (`crates/acos-services/src/pms.rs`): `flags` starts at
`FLAG_HAS_CODE` (0x0000_0001); `FLAG_HARDWARE_ACCELERATED` is OR-ed in as
before. The manifest model has no `android:hasCode` override yet, so the AOSP
default applies. Locked by
`pms::tests::application_info_flags_include_has_code`, and end-to-end by
`tests/rn_pixels.rs::rn_platform_launch_probe` (it fails if the blocker
mentions `ClassNotFoundException` again).

After the fix the same launch runs the real `PathClassLoader` over the staged
`/data/user/0/dev.acos.fixtures.rnhello/base.apk`, loads **1 552** classes in
**6.4 s**, and gets past `application_on_create` into the app's own
`MainApplication`.

## 4. Blocker 2 (open): `sun.nio.fs.UnixNativeDispatcher.init()I`

The first app-side call after the class loads is the NIO default filesystem
(SoLoader / ReactNativeHost use `java.nio.file.Paths`). The VM stops at:

```
UNSUPPORTED_JAVA_API: vm: Lsun/nio/fs/UnixNativeDispatcher;->init()I is not implemented
    at Lsun/nio/fs/UnixNativeDispatcher;.init pc=0
    at Lsun/nio/fs/UnixNativeDispatcher;.<clinit>()V
    at Lsun/nio/fs/UnixFileSystem;.<init>(...)V
    at Lsun/nio/fs/LinuxFileSystem;.<init>(...)V
    at Lsun/nio/fs/LinuxFileSystemProvider;.newFileSystem(...)
    at Lsun/nio/fs/FileSystems$DefaultFileSystemHolder;.<clinit>()V
    at Lsun/nio/fs/FileSystems;.getDefault()
    at Lsun/nio/fs/Paths;.get(...)
```

`research/framework-dex/REPORT.md` ranks `sun/nio/fs/UnixNativeDispatcher` as
the 31st largest native class (**47 natives**): `open`, `stat`, `lstat`,
`read`, `write`, `close`, `access`, `mkdir`, `rename`, `readlink`, `opendir`,
`readdir`, … A faithful implementation is not a one-line shim; `init()`
returns the capability mask AOSP's `UnixNativeDispatcher` stores, and the
class cannot be half-implemented without lying about the others. That is a
work item of its own (VM file substrate), and it is the next rung, not this
slice.

Route note: `java.io.UnixFileSystem` is already implemented
(`crates/acos-vm/src/natives/io_linux.rs`); only the `java.nio.file` SPI is
missing. The exact next step is either (a) implement the
`UnixNativeDispatcher` class against the VFS (principled, sized like the
`io_linux` port), or (b) prove the RN 0.81.4 host does not need `Paths` on the
`MainApplication` path — unlikely, `SoLoader.init` resolves the native-library
directory through it. Do (a) with a round-trip test per syscall, not a shim.

## 5. What is still between here and a `Hello ACOS` pixel

| # | rung | state |
|---|---|---|
| 1 | APK dex into framework class loader | **fixed** (`FLAG_HAS_CODE`) |
| 2 | `java.nio.file` substrate for `SoLoader.init` | **blocked** (`UnixNativeDispatcher`) |
| 3 | `SoLoader` discovers/loads `libreactnative.so`, `libhermes.so`, `libjsi.so`, `libfbjni.so`, `libc++_shared.so` through the ACOS native loader | unproven on this path (the executor path is proven by W120–W140) |
| 4 | `ReactNativeHost`/`ReactInstanceManager` builds the Catalyst instance on `ActivityThread`'s main thread (the `RnSession` harness builds it directly) | unproven |
| 5 | `ReactActivity.onCreate` → `ReactRootView` → `setContentView` → `ViewRootImpl.addToDisplayAsUser` | `ViewRootImpl`/WMS path proven by the counter fixture |
| 6 | real `MainReactPackage` view managers: `createView` → real `ReactTextView`/`AppCompatTextView`/`ReactViewGroup` | not reached (no native ViewManagers in the recording path) |
| 7 | measure/layout/draw the `Text "Hello ACOS"` → `ThreadedRenderer.nSyncAndDrawFrame` → `acos-render` → `FrameStore` | renderer proven by M3/counter; needs 5–6 |
| 8 | `RnSession::frame()` / `acos observe --frame` returns the frame instead of `UNSUPPORTED_RN_NO_RENDERER` | after 7 |

Rungs 3–7 are the real remaining work; the investigation shows each of them
has a working precedent in the repo, which is why the platform path is the
right first slice.

## 6. Files touched (uncommitted, not committed)

* `crates/acos-services/src/pms.rs` — `FLAG_HAS_CODE` in
  `AppInfo::from_manifest` + `application_info_flags_include_has_code`.
* `crates/acos-runtime/tests/rn_pixels.rs` — **new** fixture-gated probe/e2e:
  confined child → `AgentSession::launch(rn-hello.apk)` with native enabled →
  stage ladder, blocker, frame probe; regression assertion for the class
  loader.
* `results/rn/w164a-dex-inventory.json` — **new** dex inventory (8 648
  classes, 42/42 mount-path classes, 358 natives, package buckets).
* `results/rn/w164a-platform.txt` — **new** measured platform-launch evidence.
* `results/rn/W164A_PIXELS.md` — this file.

No `flutter_*`, tap/input pump (`acos-wm` input), boot-cache
(`boot_image_cache.rs`), or CLI ergonomics (`session_backend.rs`, `parity.rs`,
`launch.rs`) file was edited. The pre-existing uncommitted changes in the main
tree were left untouched.

## 7. Acceptance

| check | result |
|---|---|
| `cargo test -p acos-services --lib` | **45 passed** (new flag test included) |
| `cargo test -p acos-runtime --test rn_pixels` | **1 passed** (child worker is inert without its env var) |
| `cargo test -p acos-runtime --test rn_pixels -- --exact rn_platform_launch_probe` | **1 passed** (`reached_stage=bind_application`, 1 552 classes, no `ClassNotFoundException`) |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | **clean** (`exit=0`) |
| `cargo check --workspace --all-targets` | **clean** (`exit=0`) |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported of 27** (`exit=0`) |

## 8. Exact next session

1. Implement `sun.nio.fs.UnixNativeDispatcher` over the VFS (all 47 natives
   the class declares, `init()` first), with a native round-trip test per
   syscall mirroring `io_linux.rs`.
2. Re-run `rn_platform_launch_probe`; expect the next blocker at
   `SoLoader.init` → record it the same way.
3. When `MainApplication.onCreate` completes, assert `MainActivity` reaches
   `onCreate` and `ReactRootView` is in `WindowManagerGlobal` (the counter
   `view_tree` walk can read it).
4. Only then switch `RnSession::frame()`/`acos observe --frame` to the
   `FrameStore`; until a real frame is published the structured
   `UNSUPPORTED_RN_NO_RENDERER` stays.

---

# W168-A — `sun.nio.fs` on the VFS: the NIO rung is done, and the app class loader is the next rung

Follow-on to W164-A (rung 2 of §5). The NIO substrate for `java.nio.file`
is now implemented and independently verified; the RN platform probe executes
`UnixNativeDispatcher.init()`/`getcwd()`, and the next blocker is the app
`PathClassLoader`/`DexPathList` — not `SoLoader` yet.

## At a glance

| step | result |
|---|---|
| `sun.nio.fs.UnixNativeDispatcher` (47 natives) | **implemented** over the VFS (`init`, `getcwd`, `open0`/`openat0`, `stat0`/`lstat0`/`fstat`/`fstatat0`/`stat1`/`exists0`, `read`/`write`, `opendir0`/`fdopendir`/`readdir`/`closedir`, `mkdir0`/`rmdir0`/`unlink0`/`unlinkat0`/`mknod0`, `rename0`/`renameat0`/`link0`/`symlink0`/`readlink0`/`realpath0`, `access0`/`chmod0`/`fchmod`/`chown0`/`lchown0`/`fchown`, `utimes0`/`futimes`, `statvfs0`, `strerror`, `getpwuid`/`getgrgid`/`getpwnam0`/`getgrnam0`, `dup`/`close`, `fpathconf`/`pathconf0`/`fopen0`/`fclose`) |
| `sun.nio.ch.IOUtil` + `FileDispatcherImpl` | **implemented** (`iovMax`/`fdLimit`/`fdVal`/`setfdVal`/`configureBlocking`/`makePipe`/`drain`/`randomBytes`; `read0`/`write0`/`pread0`/`pwrite0`/`readv0`/`writev0`/`close0`/`closeIntFD`/`force0`/`truncate0`/`preClose0`/`release0`/`lock0`) |
| `LinuxNativeDispatcher` | **implemented** (`init` no-op, mount-table end-of-stream, `user.*` xattr via the VFS) |
| Java acceptance | **passes**: `crates/acos-vm/tests/nio_fs.rs` (`Files.exists`/`readAllBytes`/`newDirectoryStream`/`toRealPath`, attrs + owner, rename/delete, ENOENT/EISDIR) |
| RN platform probe | NIO rung **passed**; next blocker is `ClassNotFoundException` on `MainApplication` with `DexPathList[[]]`, 1 673 classes, 6.3 s |

## 1. Implementation notes (principled, not a shim)

`crates/acos-vm/src/natives/fwk_nio_fs.rs` is the NIO peer of
`io_linux.rs`: every path argument is a `NativeBuffer` NUL-terminated C string
in the synthetic native-memory table, read with a new
`native_mem::read_cstr`, and resolved through the **same** [`crate::vfs`] as
`java.io`. Errors are raised as the real `sun.nio.fs.UnixException(errno)`;
`errorString()` calls the real `UnixNativeDispatcher.strerror` bytecode, which
this module also supplies.

* `init()` returns the AOSP Linux mask `SUPPORTS_OPENAT | SUPPORTS_FUTIMES |
  SUPPORTS_XATTR`. Each set bit is backed: `openat`/`futimes`/`fgetxattr0`
  are all implemented. `SUPPORTS_FUTIMENS`/`SUPPORTS_LUTIMES`/
  `SUPPORTS_BIRTHTIME` are **not** returned, matching the dex (Android's
  class declares no `futimens`/`lutimes`, and the kernel exposes no birthtime
  to `stat`).
* `opendir`/`readdir`/`closedir` add a per-session directory-stream table to
  the VFS (`Vfs::opendir`/`fdopendir`/`readdir`/`closedir`); entries are
  snapshotted at open time and `.`/`..` are reported first, exactly as
  `readdir` does, and filtered by the real `UnixDirectoryStream`.
* `read`/`write`/`readv0`/`writev0` resolve synthetic addresses through
  `native_mem::read_bytes_at`/`write_bytes_at`, so both `Unsafe.allocateMemory`
  regions and `VMRuntime.addressOf` heap-array aliases work.
* **Honest refusals**: `chown`/`fchown` report `EPERM` (ACOS has no capability
  layer), `mknod` reports `EOPNOTSUPP` (no device nodes), non-`user.*` xattr
  names report `EOPNOTSUPP`, `lock0` succeeds because the VM runs one guest
  process so advisory ranges cannot conflict, and `getFileStores()` yields an
  empty list because there is no mount namespace.

## 2. Java round-trip evidence

`crates/acos-vm/tests/java/NioFsMain.java` runs on the real Android 14 core
library (no Rust shortcut) and prints:

```
dir_exists=true dir_is_dir=true
rw_same=true rw_size=10
list_has_probe=true list_size=1
realpath=/data/user/0/com.example.nio/files/probe.bin realpath_abs=true
attr_is_reg=true attr_size=10 attr_uid=1000 attr_owner=system
old_gone=true new_present=true deleted=true
fonts_prefix=<?xml version=
enoent_reason=null enoent_file_present=true eisdir_caught=true
```

`attr_owner` proves `getpwuid`/`strerror` are reached by the real
`UnixFileAttributes.owner()` path (the platform's `UnixException` passes a
`null` reason, so the `getReason()` observed is the literal `"null"`, matching
Android's `translateToIOException`). The `fonts_prefix` line reads `/system`
through the NIO SPI, i.e. the same confined boot cache `java.io` uses.

## 3. Measured platform-probe advance (the W164-A blocker is gone)

`results/rn/w168a-platform.txt` (re-run of `rn_platform_launch_probe`):

```
reached_stage=bind_application  classes_loaded=1673  wall_ms=6301
native-tail includes: Lsun/nio/fs/UnixNativeDispatcher;->init()I
                      Lsun/nio/fs/UnixNativeDispatcher;->getcwd()[B
                      Lcom/android/internal/os/ClassLoaderFactory;->createClassloaderNamespace(...)
```

The previous `UNSUPPORTED_JAVA_API: Lsun/nio/fs/UnixNativeDispatcher;->init()I`
is no longer the root blocker. The next blocker is the app class loader, and
it is **not** the `FLAG_HAS_CODE` regression the probe guard looks for:

```
JAVA_EXCEPTION: Unable to instantiate application
  dev.acos.fixtures.rnhello.MainApplication ...
  ClassNotFoundException: Didn't find class
  "dev.acos.fixtures.rnhello.MainApplication"
  on path: DexPathList[[], nativeLibraryDirectories=[.../fixtures/build/apps/lib/x86_64, /system/lib64, /system/lib]]
```

A one-off diagnostic in `bind_app.rs` (not kept) read the materialised
`android.content.pm.ApplicationInfo` at `handleBindApplication` time:

```
rust_flags=0x20000001  java_flags=536870913 (0x20000001)
  source=/data/user/0/dev.acos.fixtures.rnhello/base.apk
```

So `FLAG_HAS_CODE` **is** set and `sourceDir` is the staged APK. Dumping the
real `LoadApk` bytecode (Android 14 `classes.dex`) shows why the W164-A
narrative is incomplete: `ActivityThread.getPackageInfoNoCheck(ai)` passes
`includeCode=1` to `LoadedApk.<init>(…, includeCode, …)` unconditionally, and
`LoadedApk.makePaths(...)` unconditionally does `outZipPaths.add(sourceDir)`.
The empty `DexPathList` therefore comes from a later step in
`LoadedApk.createOrUpdateClassLoaderLocked` / `ApplicationLoaders.getClassLoader`
or from `DexFile.openDexFileNative`, not from the flag.

**Correction to W164-A §3/§4.** The prose “gets past `application_on_create`
into the app's own `MainApplication`” is not supported by
`results/rn/w164a-platform.txt`: its own stage ladder marks
`application_on_create` **Blocked** with the NIO `init()` exception. The NIO
call is reached from `LoadedApk` class-loader setup, so the app was never
instantiated in that run. The `ClassNotFoundException` was masked by the NIO
blocker, not fixed by `FLAG_HAS_CODE`. (The flag fix itself is still correct
and remains locked by `pms::tests::application_info_flags_include_has_code`
and the probe's `!ClassNotFoundException` guard, which now points at the
class-loader rung.)

One concrete VFS signal in the same run (worth the next session):
`LoadedApk` logs `Failed to ensure
/data/user_de/0/dev.acos.fixtures.rnhello/code_cache: mkdir failed: ENOENT`
and the same for `cache`. The device-protected tree **is** mapped
(`Vfs::target` → `<root>/data/user_de/**`), so this is an `mkdir`
parent-existence / `File.mkdirs()` interaction to pin down alongside the
class loader.

## 4. Exact next session

1. Trace `LoadedApk.createOrUpdateClassLoaderLocked` by setting a native
   breakpoint on `ApplicationLoaders.getClassLoader` / `DexFile.openDexFileNative`;
   log the `zipPaths`/`libPaths` it receives. If `zipPaths` is empty, the bug is
   in `makePaths`/`updateApplicationInfo`; if non-empty, it is
   `DexFile.openDexFileNative` over the staged path.
2. Fix the `/data/user_de/0/<pkg>` `mkdirs` ENOENT (the `ensureDataDir`
   warning) if it participates.
3. Only once `MainApplication` instantiates: expect the next blocker at
   `SoLoader.init` (rung 3) and record it the same way.

## 5. Acceptance

| check | result |
|---|---|
| `cargo test -p acos-vm --lib --test file_io --test native_mem` | **129 + 1 + 1 passed** |
| `cargo test -p acos-vm --test nio_fs` | **1 passed** |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | **clean** (`exit=0`) |
| `cargo check --workspace --all-targets` | **clean** (`exit=0`) |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported of 27** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 pass / 0 fail / 0 unsupported of 27** |
| `cargo test -p acos-runtime --test rn_pixels` | **advances to `ClassNotFoundException`** (documented next rung; the probe's legacy `ClassNotFoundException` guard fails by design until the class loader is fixed) |

## 6. Files touched (uncommitted, not committed)

* `crates/acos-vm/src/natives/fwk_nio_fs.rs` — **new**: 47
  `UnixNativeDispatcher` natives + `LinuxNativeDispatcher`.
* `crates/acos-vm/src/natives/mod.rs` — install the new module.
* `crates/acos-vm/src/natives/native_mem.rs` — `read_cstr`.
* `crates/acos-vm/src/natives/fwk_m2_filechannel.rs` — `IOUtil` +
  remaining `FileDispatcherImpl`.
* `crates/acos-vm/src/vfs.rs` — directory streams + `ERANGE`.
* `crates/acos-vm/tests/nio_fs.rs`, `crates/acos-vm/tests/java/NioFsMain.java`
  — **new** Java acceptance.
* `results/rn/w168a-platform.txt`, this section.

No `pms.rs`, `flutter_*`, `bench/pool`, tap/ripple, ICU/fonts/AVD, or
`boot_image_cache.rs` file was edited.

---

# W174-A — the app class loader rung is closed: `FLAG_HAS_CODE` is `0x4`, not `0x1`

Follow-on to W168-A. The W168-A blocker (empty `DexPathList` while
`sourceDir`/`FLAG_HAS_CODE` looked right) is fully rooted: the flag value W164-A
wrote was `0x1`, which is `ApplicationInfo.FLAG_SYSTEM`; the real
`FLAG_HAS_CODE` is `1 << 2 == 0x4`. With the correct bit the framework's own
`PathClassLoader` receives the staged APK dex path, `MainApplication` is
instantiated, `Application.onCreate` runs, `SoLoader.init` prepares the APK's
`lib/x86_64`, and the launch advances to the Activity construction.

## At a glance

| step | result |
|---|---|
| W168-A blocker | **root-caused**: `LoadedApk.mIncludeCode` false because `flags & 4` was 0 |
| fix | `FLAG_HAS_CODE = 0x0000_0004` in `AppInfo::from_manifest` (+ test) |
| new native | `BaseDexClassLoader.computeClassLoaderContextsNative()` returns `String[0]` (dexopt class-loader-context protocol, no dexopt pipeline) |
| launch before | `reached_stage=bind_application`, `application_on_create=Blocked` (`ClassNotFoundException`, `DexPathList[[]]`) |
| launch after | `reached_stage=client_transaction`; `bind_application`/`application_on_create`/`client_transaction` all **Ok**, 1 892 classes, 6.5 s |
| SoLoader rung | **reached**: `Preparing SO source: DirectApkSoSource[root = [.../base.apk!/lib/x86_64]]` |
| next blocker | `ComponentActivity.<init>` → `androidx.lifecycle.SavedStateHandleSupport.enableSavedStateHandles` — the class is **not packaged in the fixture APK** |
| `/data/user_de/0/<pkg>` `mkdir` ENOENT | **gone** (`apk_loader::ensure_app_data_dirs` runs on the `AgentSession::launch` path; no `Failed to ensure` record in the run) |

## 1. Root cause, from the framework DEX (not from prose)

`.cache/boot/android-34/dex/framework/classes.dex`,
`ActivityThread.getPackageInfo(ApplicationInfo;CompatibilityInfo;ClassLoader;ZZZZ)`
builds the `LoadedApk` `includeCode` argument as:

```
009c: if-eqz v20, 0x00a6          # the caller's includeCode boolean
009e: iget v1, v10, ApplicationInfo.flags
00a0: and-int/lit8 v1, v1, #int 4 # ApplicationInfo.FLAG_HAS_CODE == 1 << 2
00a2: if-eqz v1, 0x00a6          # no FLAG_HAS_CODE -> includeCode = 0
00a4: move v7, v2                # includeCode = 1
```

`LoadedApk.createOrUpdateClassLoaderLocked` then branches on `mIncludeCode`
(`01b2: iget-boolean mIncludeCode; if-nez -> 0x01fc`):

* `mIncludeCode == true` → `0x01fc`: join the `makePaths` zip list and call
  `ApplicationLoaders.getClassLoaderWithSharedLibraries(dexPath, …)`;
* `mIncludeCode == false` → `0x01b6`: build `mDefaultClassLoader` with the
  literal empty dex path `const-string v10, ""`, while `makePaths` still filled
  the native-library list. That is exactly the observed
  `DexPathList[[],nativeLibraryDirectories=[…]]`.

The W164-A constant `FLAG_HAS_CODE: i32 = 0x0000_0001` is
`ApplicationInfo.FLAG_SYSTEM`. `flags=0x20000001` therefore had bit 2 clear, and
`mIncludeCode` was false on every fixture whose manifest names an application
class. The counter fixture masked it because its `<application>` has no
`android:name`.

Fix: `crates/acos-services/src/pms.rs` now uses `0x0000_0004`, with the value
and the compiled mask documented at the call site;
`pms::tests::application_info_flags_include_has_code` was updated to the real
bit so it can no longer lock in `FLAG_SYSTEM`.

## 2. The next native: `BaseDexClassLoader.computeClassLoaderContextsNative`

With the dex path non-empty the framework builds a real `PathClassLoader` and,
at the end of `BaseDexClassLoader.<init>` (`002d`), calls
`reportClassLoaderChain()`. With a non-null `DexLoadReporter` that reaches the
native `computeClassLoaderContextsNative()[Ljava/lang/String;`, which ACOS did
not implement.

`reportClassLoaderChain` maps the returned array two-by-two and hands it to
`DexLoadReporter.report`; a **zero-length** array is an explicit
"nothing to report" early return (`array-length v1, v0; if-nez v1 -> return`).
ACOS runs the DEX directly with no dexopt/oat pipeline (`DexFile.isDexOptNeeded`
is already `false`), so the honest answer is an empty array, not a fabricated
context that would only fail in `DexLoadReporter`/`VMRuntime.notifyDexLoad`
downstream. `crates/acos-vm/src/natives/fwk_dexfile.rs` registers it next to the
other `dalvik.system` natives and documents the divergence.

## 3. Measured advance

`results/rn/w174a-platform.txt` (re-run of `rn_platform_launch_probe`):

```
reached_stage=client_transaction  classes_loaded=1892  wall_ms=6500
stages: apk_parsed Ok, process_booted Ok, bind_application Ok,
        application_on_create Ok, client_transaction Ok, activity_on_create Blocked
W164A_EVENTS:
  Recording new base apk path: /data/user/0/dev.acos.fixtures.rnhello/base.apk
  Preparing SO source: DirectApkSoSource[root = [/data/user/0/dev.acos.fixtures.rnhello/base.apk!/lib/x86_64]]
```

The native tail now contains `Ldalvik/system/DexFile;->openDexFileNative` (the
class-loader rung), `Ldalvik/system/BaseDexClassLoader;->computeClassLoaderContextsNative`,
and `Ldalvik/system/VMRuntime;->registerAppInfo` — i.e. the real
`PathClassLoader` is built over the staged multi-dex APK and the app's
own `MainApplication` + `Application.onCreate` ran. Rung 3 of W164-A §5
(`SoLoader` discovers the APK's native libraries) is now **proven on this
path**: `SoLoader.init` found `base.apk!/lib/x86_64`.

## 4. New blocker: the fixture APK is missing an AndroidX class

`activity_on_create` is blocked at

```
androidx/activity/ComponentActivity.<init>()V
  -> Landroidx/lifecycle/SavedStateHandleSupport;.enableSavedStateHandles(
         Landroidx/savedstate/SavedStateRegistryOwner;)V   (not implemented)
```

`SavedStateHandleSupport` is **not defined in either dex of the fixture**.
`dexdump` over the APK's `classes.dex`/`classes2.dex`
(`/tmp/rn-classes1.txt`) finds only references; there is no
`class Landroidx/lifecycle/SavedStateHandleSupport;`. `androidx.lifecycle` (model)
and `androidx.savedstate` are packaged, but the
`lifecycle-viewmodel-savedstate` artifact (which owns
`SavedStateHandleSupport`) is absent. `ComponentActivity.<init>` calls it
unconditionally, so the APK would also fail on a real device: this is a fixture
packaging gap, not a VM bug.

`tools/rn-hello-deps.py` output confirms it: the resolver emits
`lifecycle-viewmodel`, `lifecycle-viewmodel-android`, and `savedstate`, but no
`lifecycle-viewmodel-savedstate`. The Gradle cache has that artifact
(2.5.1/2.6.1/2.6.2/2.8.3/2.9.3/2.10.0), so the fix is to add an explicit root
(the activity/fragment POM edge is not being followed) and rebuild
`fixtures/build/apps/rn-hello.apk`. That rebuild was **not** done in this slice
to stay inside the file-in-flight / 45-minute bounds; it is the first item of
the next session.

## 5. `rn_pixels` guard changed to the current rung

The W164-A guard conflated two facts (`classes_loaded > 1000` **and** no
`ClassNotFoundException`) with the same failure. W174-A replaces it with an
assertion of the rung actually reached: `bind_application` and
`application_on_create` are `Ok`, the root blocker is not a
`ClassNotFoundException`, and the `SoLoader` evidence
(`Preparing SO source` + `base.apk!/lib/x86_64`) is present in the launch
events. It deliberately does not pin the next blocker, which is a fixture
packaging gap owned by the fixture workstream.

## 6. Files touched (uncommitted, not committed)

* `crates/acos-services/src/pms.rs` — `FLAG_HAS_CODE = 0x0000_0004` (was
  `0x0000_0001` = `FLAG_SYSTEM`) + corrected test constant.
* `crates/acos-vm/src/natives/fwk_dexfile.rs` —
  `BaseDexClassLoader.computeClassLoaderContextsNative` → empty `String[]`.
* `crates/acos-runtime/tests/rn_pixels.rs` — guard asserts the
  class-loader/SoLoader rung instead of the legacy conflation.
* `results/rn/w174a-platform.txt` — measured platform-launch evidence.
* `results/rn/W164A_PIXELS.md` — this section.

No `flutter_*`, `bench/pool`, tap/ripple (`acos-wm`), JNI/loader hardening,
dialogs/DatePicker/`fwk_res`, ICU/fonts/AVD, `boot_image_cache.rs`, CLI
(`session_backend.rs`/`parity.rs`/`launch.rs`) or fixture-build file was edited.

## 7. Acceptance

| check | result |
|---|---|
| `cargo test -p acos-services --lib` | **45 passed** (corrected flag test included) |
| `cargo test -p acos-vm --lib` | **129 passed** |
| `cargo test -p acos-runtime --test rn_pixels` | **1 passed** (child inert without its env var) |
| `cargo test -p acos-runtime --test rn_pixels -- --exact rn_platform_launch_probe` | **1 passed** (`reached_stage=client_transaction`, `application_on_create=Ok`, SoLoader evidence asserted) |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | **clean** (`exit=0`) |
| `cargo check --workspace --all-targets` | **clean** (`exit=0`) |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported of 27** (`exit=0`) |

## 8. Exact next session

1. Add `androidx.lifecycle:lifecycle-viewmodel-savedstate` as an explicit root
   in `tools/rn-hello-deps.py` (pick a version whose main artifact carries the
   classes) and rebuild `rn-hello.apk`.
2. Re-run `rn_platform_launch_probe`; expect `activity_on_create` to complete
   (`ReactActivity.onCreate`) and the next blocker at `ReactRootView` /
   `ReactInstanceManager` — record it the same way.
3. Only once the real `ReactRootView` is in `WindowManagerGlobal` (the counter
   `view_tree` walk can read it) switch `RnSession::frame()` /
   `acos observe --frame` to the `FrameStore`; until a real frame is published
   the structured `UNSUPPORTED_RN_NO_RENDERER` stays.
