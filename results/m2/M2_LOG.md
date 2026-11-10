# M2 log — `bindApplication` on the main looper (T-23/T-24)

**Status: M2 attach handshake + real `bindApplication` delivery; Application/
Activity callbacks not yet reached.** The real Android 14 `framework.jar`
bytecode now takes `IApplicationThread.bindApplication` from the Rust ATMS
(in-process, through the single ACOS Binder native), builds the real
`ActivityThread$AppBindData`, and posts `H.BIND_APPLICATION` to the main
looper. `ActivityThread.handleBindApplication` then starts executing the real
bytecode. It blocks before `Application.onCreate` in
`android.graphics.Typeface.<clinit>`, at
`com.android.icu.util.regex.PatternNative.compileImpl` — the font-config parse
needs ICU regex plus the file-I/O `/system` substrate, neither of which is
wired.

Test: `crates/acos-runtime/tests/m2.rs`. The full gate is `#[ignore]`d with
that exact reason; the two non-ignored tests pin the progress
(`bind_delivered == true`) and the honesty invariant (a transaction is never
reported as delivered unless the callbacks actually ran).

## What actually runs (evidence)

Release probe:

```
cargo run -q --release -p acos-runtime --example lifecycle_probe -- fixtures/build/apps/hello-views.apk
```

| metric | value |
|---|---:|
| wall, whole boot + bind + looper phase | ~124 ms |
| VM classes loaded at block | 592 |
| `bindApplication` delivered to `ApplicationThread` | yes |
| `ActivityThread.mInitialApplication` non-null | no (blocked first) |
| `ClientTransaction` delivered | no (structured unsupported) |
| first structured blocker | `com.android.icu.util.regex.PatternNative.compileImpl(Ljava/lang/String;I)J` |

Debug `cargo test -p acos-runtime --test m2` (three tests): wall ~0.98 s,
peak RSS (`VmHWM`) ~361 MiB, 592 classes. Under `ACOS_GC_STRESS=1`: wall ~3.0 s,
peak RSS ~384 MiB, 594 classes, same blocker. GC stress exposed (and this session
fixed) an unrooted-handle bug in the new `VMRuntime.properties()`/`list0` array
builders — `vm.alloc` could collect the strings before the array held them.

The path that runs:

```text
... Looper.prepareMainLooper() / new ActivityThread() / attach(false,1)
      -> IActivityManager.attachApplication      [W13-A handshake, still green]
looper thread:
  Looper.loop -> loopOnce -> H.handleMessage -> ActivityThread.handleBindApplication
    Compatibility.setTargetSdkVersion -> Canvas.nSetCompatibilityVersion      [fixed: recorded]
    LocaleList.setDefault(config.getLocales())                                [fixed: real en-US config]
    Typeface.setSystemFontMap / Typeface.<clinit>
      nativeGetReleaseFunc / preloadFontFile / nativeRegisterLocaleList       [fixed: no-ops]
      SystemFonts.getSystemPreinstalledFontConfig -> FontListParser.parse
        File.list / File.getCanonicalPath -> UnixFileSystem.list0/canonicalize0 [fixed: real host impls]
        ULocale / ICU -> PatternNative.compileImpl                            [BLOCKED: no ICU regex]
```

## Changes landed in this session

Continuing W13-A's uncommitted work (attach handshake + `bind_app.rs` /
`lifecycle.rs` scaffolding).

| file | change |
|---|---|
| `crates/acos-runtime/src/lifecycle.rs` | `deliver_launch_transaction` no longer returns `Ok(())` while the caller sets `transaction_delivered = true` (advisor finding, `docs/FINDINGS.md` 2026-11-10). It returns a structured `LifecycleError::Unsupported` naming the missing T-23 `ClientTransaction`/`scheduleTransaction` wiring. New `LifecycleError::Unsupported` variant. |
| `crates/acos-runtime/src/bind_app.rs` | `build_config`: the `Configuration` delivered to `bindApplication` now has the oracle's real locale (`en-US`), density 420, 411x842 dp portrait and fontScale 1.0. Previously the default (empty) locale list made `handleBindApplication` throw `IllegalArgumentException: locales is empty`. |
| `crates/acos-vm/src/natives/vmrt.rs` | `VMRuntime.properties()` returns the ART runtime `-D` assignment `user.dir=/` (real Android sets it in `AndroidRuntime::start`). Without it `jdk.internal.util.StaticProperty.<clinit>` throws `InternalError`, which corrupts the framework's class-loading exception path. |
| `crates/acos-vm/src/natives/fwk_jni.rs` (new) | `Runtime.nativeLoad` (ACOS serves framework JNI from Rust; a later unbound native still surfaces) and `StatsLog.writeImpl` (no statsd daemon → event discarded, as on a daemon-less device). |
| `crates/acos-vm/src/natives/fwk_graphics.rs` (new) | `Canvas.nSetCompatibilityVersion` (recorded), `Typeface.nativeGetReleaseFunc` (0 — no native typefaces), `Typeface.preloadFontFile`, `Typeface.nativeRegisterLocaleList` (no native font cache). Declared divergences; every drawing/measurement native stays `UNSUPPORTED_JAVA_API`. |
| `crates/acos-vm/src/natives/icu.rs` | `LocaleNative.setDefaultNative` (recorded identity), `PatternNative.getNativeFinalizer` (0 — no native regex objects). |
| `crates/acos-vm/src/natives/os_misc.rs` | `Process.setArgV0Native` (recorded; no separate process identity to rename). |
| `crates/acos-vm/src/natives/io.rs` | `UnixFileSystem.canonicalize0` (real host `realpath`; throws `IOException` on a missing path, like `canonicalize_md.c`) and `UnixFileSystem.list0` (real host `readdir`). |
| `crates/acos-vm/src/natives/mod.rs` | two `mod` lines + two `install` calls (my files only). `interp.rs`/`vm.rs` debug scaffolding left by W13-A was reverted. |
| `crates/acos-runtime/tests/m2.rs` (new) | the M2 acceptance test (below). |

## The M2 acceptance test

`crates/acos-runtime/tests/m2.rs`:

* `m2_lifecycle_create_start_resume_matches_reference` — **`#[ignore]`d**. Asserts
  `error.is_none()`, `bind_delivered`, `app_created` (real
  `ActivityThread.mInitialApplication`) and that the `onCreate`/`onStart`/
  `onResume` prefix of the `AcosFixture` log equals
  `fixtures/reference/hello-views/art-api34-x86_64/lifecycle-summary.txt`.
  Every event is emitted by the real `MainActivity` bytecode; nothing is a
  recorded mock call.
* `bind_application_is_delivered_to_the_real_application_thread` — **passes**.
  Regression guard for the part of M2 that is done.
* `lifecycle_never_claims_an_undelivered_transaction` — **passes**. If
  `transaction_delivered` is ever true, the log must start with `onCreate` and
  `app_created` must be true. This is the advisor's honesty invariant.

Run the gate (`--ignored --nocapture`) to see the current red state; run the
suite normally to see 2 pass / 1 ignored.

## Next blockers (in order)

1. **ICU regex** — `com.android.icu.util.regex.PatternNative.compileImpl` /
   `matcher*`. The font-config parse calls ICU regex. This needs either a real
   ICU regex binding or a Rust regex provider behind the `PatternNative` shim.
   It is the first blocker today.
2. **File I/O + `/system` path mapping** — ACOS has `readBytes`/`writeBytes`
   for the std streams only. `FileInputStream` needs `Linux.open`/`lseek`/
   `fstat`/`close` and an fd table that preserves the file offset; the font
   config also needs `/system/etc/fonts.xml` and `/system/fonts/*` mapped onto
   `.cache/boot/android-34/fonts/`. This is the substrate for both fonts and
   resources.
3. **`android.content.res` native shim** — the `acos-framework-natives` `res`
   engine is complete (`NATIVES.md`) but is not wired to the VM: no
   `AssetManager`/`ApkAssets`/`XmlBlock` natives are registered.
   `handleBindApplication` calls `ResourcesManager.applyConfigurationToResources`
   and builds the app `LoadedApk`/`Context` immediately after the font block.
4. **`ClientTransaction` delivery (T-23)** — build the real
   `ClientTransaction` + `LaunchActivityItem`/`ResumeActivityItem` and call
   `ApplicationThread.scheduleTransaction` on the main looper; then
   `MainActivity.onCreate`/`onStart`/`onResume` run. Currently a structured
   `Unsupported` (honest).
5. **`HardwareRenderer`** — `handleBindApplication` calls
   `HardwareRenderer.preload()`/`setupDiskCache`; this is the rendering
   boundary. Rendering stays out of scope for M2; `results/m2` should report it
   rather than fake it.

## Honest caveats

* The `Runtime.nativeLoad` and `Typeface` no-ops are declared divergences. They
  do not fake an app-observable result — no native objects exist to release or
  preload — but they do mean a JNI library that is never actually `dlopen`ed
  can be treated as loaded until one of its natives is invoked, at which point
  the VM raises the exact `UNSUPPORTED_JAVA_API`. A real ELF JNI loader stays
  ADR-0014 work.
* `StatsLog.writeImpl` dropping the event matches a device without statsd. The
  compat *decisions* are made in real Java before the event is built.
* `interp.rs`/`vm.rs` debug `eprintln!` scaffolding present in the tree at the
  start of this session was reverted; it is not part of the deliverable.
* Concurrent agents added `crates/acos-jni/` and
  `crates/acos-vm/src/natives/jni_glue.rs` while this work was in progress;
  those files are theirs and are untouched. `cargo clippy -p acos-vm -p
  acos-runtime --all-targets -- -D warnings` is clean; the workspace-wide clippy
  currently fails inside their in-progress `acos-jni` crate (~196
  `missing_debug_implementations` errors), not in any file this session touched.

## Reproduce

```sh
tools/guarded.sh --mem 6G --time 300 -- \
  cargo run -q --release -p acos-runtime --example lifecycle_probe -- \
  fixtures/build/apps/hello-views.apk

tools/guarded.sh --mem 6G --time 400 -- \
  cargo test -p acos-runtime --test m2 -- --nocapture          # 2 pass / 1 ignored

ACOS_GC_STRESS=1 tools/guarded.sh --mem 6G --time 400 -- \
  cargo test -p acos-runtime --test m2 -- --nocapture          # same, GC-stressed shims

tools/guarded.sh --mem 6G --time 400 -- \
  cargo test -p acos-runtime --test m2 -- --ignored --nocapture # the red M2 gate
```

---

# W19h session — ICU data substrate (critical path)

**Status: `bindApplication` delivered, still no `Application.onCreate`.** This
session cleared the `MissingResourceException` blocker named below, and left the
next blocker precisely characterised. The full M2 gate test is still `#[ignore]`d
(`app_created=false`, `tx_delivered=false`).

## What changed

| file | change |
|---|---|
| `crates/acos-runtime/src/lifecycle.rs` | `run_phase` no longer collapses an uncaught framework exception to `VmError::JavaException(handle)`. New `LifecycleError::JavaException(String)` renders `Class: message` + recorded frames + the last 15 first-invoked natives (`native_tail`). This is diagnostic only; it does not fake a callback. |
| `crates/acos-runtime/src/boot.rs` | new `seed_device_environment()` (called at the top of `boot_app_process`) sets `ANDROID_I18N_ROOT=/system/apex/com.android.i18n` and `ANDROID_TZDATA_ROOT=/apex/com.android.tzdata` when the host has not set them. `android.icu.platform.AndroidDataFiles.generateIcuDataPath()` joins `$ANDROID_TZDATA_ROOT/etc/icu/:$ANDROID_I18N_ROOT/etc/icu/`; the VFS already maps `/system/**` and `/apex/com.android.tzdata/**`. |
| `crates/acos-vm/src/natives/fwk_m2_filechannel.rs` (**new**) | `sun.nio.ch.FileChannelImpl.initIDs()J` → `4096` (ART returns the page size, stored in `allocationGranularity`). Honest: `map0`/`unmap0` are deliberately left unimplemented. |
| `crates/acos-vm/src/natives/mod.rs` | two one-line registrations: `mod fwk_m2_filechannel;` and `fwk_m2_filechannel::install(vm)?;` (the only edit outside my files, listed here). |

## Root cause of the first blocker (verified by disassembly)

The M2 block was:

```text
java.util.MissingResourceException: Could not find the bundle
  android/icu/impl/data/icudt72b/likelySubtags
```

`android.graphics.Typeface.<clinit>` (framework `classes.dex`, lines 1475–1478)
runs **unconditionally**:

```java
locale = SystemProperties.get("persist.sys.locale", "en-US");
ULocale ul = ULocale.forLanguageTag(locale);
ul = ULocale.addLikelySubtags(ul);      // <-- loads the likelySubtags bundle
String script = ul.getScript();
FontConfig config = SystemFonts.getSystemPreinstalledFontConfig();
```

Real ART's app trace does **not** show `addLikelySubtags`: `Typeface` is in
`preloaded-classes`, so its `<clinit>` runs in zygote before the app forks. ACOS
has no zygote, so it runs in-process and needs the ICU data.

The data lookup (core-icu4j `classes.dex`) is:

```text
ICUResourceBundleReader$ReaderCache.createInstance
  (baseName starts "android/icu/impl/data/icudt72b")
    -> ICUBinary.getData(loader, fullName, itemPath)
         -> ICUBinary.getDataFromFile(itemPath)   // memory-mapped .dat files
              (built by AndroidDataFiles.generateIcuDataPath())
         -> ICUData.getStream(loader, resourceName) // classpath fallback
```

Both the mapped-`.dat` path and the classpath fallback require data ACOS did not
have.

## Data extraction (not committed; gitignored `.cache`)

`.cache/boot/extract-icu-w19h.sh` (reuses the super → `lpunpack` → APEX
`original_apex` → `debugfs` method of `tools/extract-bootclasspath.sh`) produced:

```
.cache/boot/android-34/apex/com.android.i18n/etc/icu/icudt72l.dat
  size   28431696
  sha256 451bf49791196d9d9cf56c9f5526062b0a57207cb347f585f0e3c619d7e0bd53
```

**Durability:** `tools/extract-bootclasspath.sh` does **not** yet extract this
file, so a `--force` re-extraction (`rm -rf $OUT`) would drop it. The extraction
to extend is the i18n APEX block (§3): `debugfs dump /etc/icu/icudt72l.dat` from
`com.android.i18n`'s `apex_payload.img` into `$OUT/apex/com.android.i18n/etc/icu/`,
and the same for tzdata `.dat` files. Until then the file is a gitignored `.cache`
artifact produced by `.cache/boot/extract-icu-w19h.sh` (w19h scratch).

## Non-ignored M2 suite after this session

```
cargo test -p acos-runtime --test m2 -- --test-threads=1 --nocapture
test result: ok. 2 passed; 0 failed; 1 ignored
```

`peak_rss ~188 MiB`, `classes=606`, `bind=true app=false tx=false` for both.

## Evidence (each step is a real framework run)

```text
# before: no ICU path -> MissingResourceException
bind_delivered=true app_created=false tx_delivered=false classes=589
ERROR: java exception: java.util.MissingResourceException: Could not find the
  bundle android/icu/impl/data/icudt72b/likelySubtags

# with ANDROID_I18N_ROOT/ANDROID_TZDATA_ROOT set (shell): blocker moves
bind_delivered=true app_created=false tx_delivered=false classes=591
ERROR: vm: Lsun/nio/ch/FileChannelImpl;->initIDs()J is not implemented

# with seed_device_environment() + initIDs implemented (no shell env):
bind_delivered=true app_created=false tx_delivered=false classes=606
ERROR: java exception: java.lang.ArrayIndexOutOfBoundsException:
  Index -1 out of bounds for length 2
native-tail (last new natives):
  ... FileChannelImpl.initIDs / Linux.open / fstat / FileDescriptor.isSocket /
  Linux.close / AsynchronousCloseMonitor.signalBlockedThreads
```

The AIOOBE is reached while `ICUBinary.mapFile` opens/closes the `.dat`
(`FileInputStream.getChannel().map(...)`); it is the current M2 blocker.
Provenance note: other agents were editing the tree concurrently
(`wms_java.rs`, `transport.rs`), so the run that first showed the AIOOBE also
included their uncommitted changes; it is reproducible from a clean
`boot.rs`/`lifecycle.rs`/`natives/mod.rs` at the time of writing.

## Remaining ICU path (next owner)

`ICUBinary.mapFile` needs `FileChannelImpl.map0(IJJ)J` / `unmap0(JJ)I`, and the
Java `map` builds a `java.nio.DirectByteBuffer` over the returned address whose
gets go through `jdk.internal.misc.Unsafe.getByte/getChar/getInt(J)`. ACOS's VM
is `#![forbid(unsafe_code)]` and `natives/unsafe.rs` implements only
`addressSize`, so a Rust-owned read-only direct-buffer substrate is required
(synthetic address → Rust-side byte table + the `Unsafe.get*(J)` family).

A smaller, alternative route that avoids the whole direct-buffer track: parse
the ICU `.dat` TOC in a `java.lang.BootClassLoader.findResource` /
`ICUData.getStream` shim and serve `android/icu/impl/data/icudt72b/<bundle>` as
an `InputStream` over the bundle bytes (the fallback `ICUData.getStream` path
`ICUBinary.getData` already tries).

## Honesty

* No `Ok(())` was used for undelivered work; the unimplemented `map0` remains a
  structured `UNSUPPORTED_JAVA_API`.
* The M2 gate test is still red/ignored; no Application/Activity callback is
  claimed.
* `seed_device_environment` changes the guest-visible `ANDROID_I18N_ROOT` to
  `/system/apex/com.android.i18n` (real devices use `/apex/com.android.i18n`)
  because the VFS currently maps `/apex` only for tzdata; this is a declared
  divergence, not a hidden one.
