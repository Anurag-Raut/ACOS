# Learning guide: how Android runs apps, and where ACOS does the same thing

Read this top to bottom. Each section has three parts: **the idea** (how real Android works),
**where ACOS does it** (files in this repo) and **go deeper** (external reading).
For the project's current state, see `STATUS.md`. For the rules we work by, see
`docs/OPERATING_DIRECTIVES.md`.

## 0. The one-paragraph picture

A real Android phone runs Linux. On top of it sits the **Android framework** (Java code: `Activity`,
`View`, `ActivityManager` and so on). That Java runs on **ART**, a virtual machine that executes
**DEX bytecode**. Apps talk to system services (window manager, package manager, ...) through
**Binder** IPC. Pixels are drawn by the framework and composited by **SurfaceFlinger**.

ACOS keeps Google's real framework Java and replaces everything underneath it with Rust:
the VM, the Binder transport, the services, the renderer and the input path. That is why the
framework "just works" once the low-level pieces behave exactly like Android's.

```
 App (APK: DEX + resources + manifest)
   |  runs on
 Android framework (real Java, from the API-34 boot jars)
   |  calls "native" methods and Binder services
 ACOS: Rust VM + natives + services + renderer      <- we write this part
   |
 Linux host (no phone, no emulator)
```

## 1. The APK: what an app is on disk

**Idea.** An APK is a zip file. It holds `classes.dex` (code), `resources.arsc` (compiled
resources), `AndroidManifest.xml` (binary XML: activities, permissions, target SDK), plus assets
and native `.so` libraries.

**In ACOS.** `crates/acos-apk/` parses the zip, the binary manifest (`manifest.rs`) and the
resource table. `crates/acos-runtime/src/apk_loader.rs` loads an APK into a session.

**Go deeper.**
- Try `unzip -l fixtures/build/*.apk` and `apktool d` on one of the fixture apps.
- AOSP `frameworks/base/libs/androidfw/` (the `ResTable` and `AssetManager2` code we ported).

## 2. DEX and the VM (ART's job)

**Idea.** Java and Kotlin compile to `.class` files, then to **DEX**: a register-based bytecode
(not stack-based like the JVM). A VM loads classes, runs `<clinit>` static initialisers, interprets
bytecode, allocates objects, and garbage-collects.

**In ACOS.**
- `crates/acos-dex/`: the DEX file parser.
- `crates/acos-vm/src/interp.rs`: the bytecode interpreter (one `match` arm per opcode).
- `crates/acos-vm/src/heap.rs`: object heap and GC.
- `crates/acos-vm/src/class.rs`: class loading and linking.
- `crates/acos-vm/src/natives/`: the Rust implementations of Java `native` methods. This is the
  biggest "replace what Android does underneath" surface.
- `crates/acos-vm/src/snapshot.rs`: freeze and restore a booted VM. This is how the warm boot cache works.

**Check yourself.** `tools/diff-dexprogs.sh --corelib real` runs 27 small Java programs on both the real
JVM and our VM and compares the output. It is our most basic correctness test.

**Go deeper.**
- <https://source.android.com/docs/core/runtime/dex-format>
- <https://source.android.com/docs/core/runtime/dalvik-bytecode>
- <https://source.android.com/docs/core/runtime>
- `dexdump -d classes.dex` (Android build-tools) to read real bytecode.

## 3. Boot: Zygote, `system_server`, and our boot image

**Idea.** On Android, the first Java process (**Zygote**) preloads the framework classes, then
**forks** for each new app, so apps start fast. `system_server` hosts the system services.

**In ACOS.** There is no fork. We boot the framework once and **snapshot** it.
- `crates/acos-runtime/src/boot.rs`: the boot sequence.
- `crates/acos-runtime/src/boot_image_cache.rs`: the on-disk cache of the booted state (our "Zygote").
- `crates/acos-runtime/src/session_pool.rs`: pre-restored sessions for fast worker creation.
- `results/perf/BOOTIMAGE_CACHE.md`: a good bug story. A memory-mapped ICU file was not
  included in the snapshot, so a warm restore read garbage.

**Go deeper.** `ZygoteInit.java` and `ActivityThread.java` on <https://cs.android.com>.

## 4. Binder and system services

**Idea.** An app does not touch the window manager directly. It sends a **Binder** transaction
(a serialised `Parcel`) to a service. Services are defined by **AIDL** interfaces. Examples are
`IActivityTaskManager`, `IWindowManager` and `IPackageManager`.

**In ACOS.**
- `crates/acos-parcel/`: the `Parcel` wire format.
- `crates/acos-aidl/`: generated tables from AIDL.
- `crates/acos-runtime/src/transport.rs`: our replacement for the Binder driver.
- `crates/acos-services/src/`: the services themselves (`atms.rs`, `wms.rs`, `pms.rs`,
  `input.rs`, ...).
- `crates/acos-services/src/parcelable.rs`: codecs for system `Parcelable` types such as `ServiceInfo`.
  They are checked byte-for-byte against the real framework. See `results/parcel/coverage.md`.
- `crates/acos-runtime/src/*_java.rs`: the glue that exposes each service to Java.

**Go deeper.**
- <https://source.android.com/docs/core/architecture/aidl>
- On a real device, `adb shell service list` and `adb shell dumpsys activity`.

## 5. Starting an Activity (the lifecycle)

**Idea.** Launching an app goes: the launcher asks `ActivityTaskManager` to start it. A process is
created. `ActivityThread.main` runs. The system sends a **ClientTransaction** telling the app to
`handleLaunchActivity`. That calls `Application.onCreate`, then `Activity.onCreate`,
`onStart` and `onResume`.

**In ACOS.**
- `crates/acos-runtime/src/atms_boot.rs`, `bind_app.rs`, `client_transaction.rs`, `lifecycle.rs`:
  the same steps as Android.
- `crates/acos-runtime/src/session.rs`: one running app.
- `results/m2/M2_LOG.md`: the step-by-step log of getting `counter.apk` to run. The entries read like detective stories.

**Try it.** `acos launch fixtures/build/counter.apk`, then look at the stage ladder it reports
(`activity_on_create`, `first_frame`, ...).

**Go deeper.** `ActivityThread.handleLaunchActivity`, `ClientTransaction` and `ActivityTaskManagerService` on cs.android.com.

## 6. Windows, drawing and frames

**Idea.** `ViewRootImpl` is the bridge between an app's `View` tree and the window manager.
Each frame: **Choreographer** (driven by vsync) triggers a traversal: measure, layout, draw.
The result goes into a **Surface**, and SurfaceFlinger composites surfaces onto the display.

**In ACOS.**
- `crates/acos-wm/` and `crates/acos-runtime/src/wms_java.rs`: window management and `relayout`.
- `crates/acos-render/`: draws the frame on the CPU (a software renderer).
- `crates/acos-gles/`: OpenGL ES, needed by Flutter.
- `crates/acos-vm/src/natives/fwk_surface.rs`: `SurfaceControl` and surface natives.
- `crates/acos-runtime/src/frame_handoff.rs`: how a finished frame reaches `acos observe`.

**Bug stories worth reading** (in `results/m2/M2_LOG.md`):
- The null `attrs` in `relayout` caused a stale-leash crash.
- The theme did not resolve until `?attr` references went through the `TypedArray`'s theme.

**Go deeper.**
- <https://source.android.com/docs/core/graphics>
- `ViewRootImpl.java` (`performTraversals`) on cs.android.com.

## 7. Resources and themes

**Idea.** `R.layout.main` is an integer id that indexes the compiled resource table. A **theme**
is a stack of style bags, and `?attr/textColor` means "look this attribute up in the current theme".
Getting this subtly wrong changes colours and sizes.

**In ACOS.** `crates/acos-framework-natives/src/res/` (`manager.rs`, `theme.rs`). It is a port of AOSP
`AssetManager2`.

## 8. Input

**Idea.** A touch becomes a `MotionEvent`, is delivered to the window under the finger in
**window-local coordinates**, and reaches the `View`, which may call `performClick()`.

**In ACOS.**
- `crates/acos-services/src/input.rs`: injects events.
- `crates/acos-vm/src/natives/fwk_input.rs`: `MotionEvent`, `KeyCharacterMap` and `InputDevice` natives.
- `crates/acos-vm/src/natives/kcm.rs`: a `.kcm` keyboard layout parser.

**Current open problem.** The tap is delivered correctly, but `performClick` does not fire yet.
Reading `ViewRootImpl`'s input pipeline (`processPointerEvent`) is the best way to follow along.

## 9. Native code and other app frameworks

**Idea.** Some apps carry their own engine as `.so` files. **Flutter** draws everything itself with
Skia/Impeller and Dart. **React Native** runs JavaScript on Hermes and sends UI instructions to
native view managers.

**In ACOS.**
- `crates/acos-native-loader/`: loads real ARM and x86 `.so` files (a mini bionic libc).
- `crates/acos-jni/`: the JNI API (`FlutterSession`, RN bridge).
- `crates/acos-jni/src/flutter_session.rs`: runs the Flutter engine.
- `crates/acos-runtime/src/rn_session.rs`: runs a React Native app and builds its UI tree.
- `crates/acos-sandbox/`: confines these native engines.

**Go deeper.**
- <https://docs.flutter.dev/resources/architectural-overview>
- <https://reactnative.dev/architecture/overview>
- <https://github.com/facebook/hermes/tree/main/doc>

## 10. How agents use it

**Idea.** ACOS exists so an AI agent can build an app, run it, look at it and verify it.

**In ACOS.**
- `crates/acos-cli/`: the `acos` command (`launch`, `observe`, `tap`, `parity`).
- `crates/acos-mcp/`: the same tools over MCP.
- `docs/AGENT_QUICKSTART.md`, `docs/AGENT_API.md`, `docs/MCP.md`.
- `crates/acos-verify/`: comparing our output with a reference (SSIM image similarity, tree diff).

## 11. Suggested learning path (about one week of evenings)

| Day | Do this |
|---|---|
| 1 | Read section 0 and the Android platform architecture page. Unzip an APK and read its manifest. |
| 2 | Read the DEX format page. Run `dexdump -d` on a fixture. Skim `interp.rs`. |
| 3 | Read about Binder and AIDL. Run `adb shell service list` (the moto g35 is on adb). |
| 4 | Follow `startActivity` to `handleLaunchActivity` on cs.android.com. Then read `lifecycle.rs`. |
| 5 | Read `ViewRootImpl.performTraversals`. Run `acos launch counter.apk` and look at a frame. |
| 6 | Read two bug stories in `results/m2/M2_LOG.md`. Try to guess the fix before you read it. |
| 7 | Read the Flutter and RN architecture pages. Then read `STATUS.md` end to end. |

## 12. Books and sites

- *Android Internals* by Jonathan Levin (<https://newandroidbook.com>). The best single book.
- <https://source.android.com> (architecture, ART, graphics).
- <https://cs.android.com> (searchable AOSP source with cross-references). Use it daily.
- Gityuan's blog (<https://gityuan.com>). It is in Chinese, so use browser translation.
  It has detailed Zygote, Binder and app-start walkthroughs.

## Glossary

| Term | Plain meaning |
|---|---|
| AOSP | Android Open Source Project: Google's open source Android code |
| ART | Android Runtime: the VM that runs app code |
| DEX | The bytecode format inside an APK |
| Binder | Android's inter-process call mechanism |
| AIDL | The language used to define Binder interfaces |
| Parcel | A byte buffer holding serialised values sent over Binder |
| Zygote | The preloaded process that Android forks for each app |
| `system_server` | The process hosting the system services |
| ATMS / WMS / PMS | ActivityTaskManager / WindowManager / PackageManager services |
| Choreographer | Schedules drawing work on each vsync |
| SurfaceFlinger | Composites windows onto the screen |
| JNI | Interface between Java and native C/C++ code |
| SSIM | A score for how similar two images are (1.0 = identical) |
