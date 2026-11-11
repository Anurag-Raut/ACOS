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
