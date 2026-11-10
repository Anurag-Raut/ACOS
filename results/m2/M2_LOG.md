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

---

# W21a session — Rust-owned native memory, `map0`, and the ICU `.dat` path

**Status: the ICU data path now runs end to end; the block moved out of ICU
into font construction.** `bindApplication` is still delivered and
`app_created=false`, but `Typeface.<clinit>` gets past
`ULocale.addLikelySubtags` / `ICUBinary.getDataFromFile` (the entire reason
W19h existed) and the M2 block is now

```
android.graphics.fonts.Font$Builder.nInitBuilder()J  [not implemented]
  at android.graphics.fonts.FontListParser.readFont / Typeface.<clinit>
```

Lifecycle probe (`--corelib real`, release): `classes=690`,
`bind=true app=false tx=false`, first structured blocker
`Landroid/graphics/fonts/Font$Builder;->nInitBuilder()J`.

## What actually changed

| file | change |
|---|---|
| `crates/acos-vm/src/natives/native_mem.rs` (**new**) | Rust-owned native-memory region table. Synthetic 64-bit addresses `(region_id << 32) \| offset` (`region_id == 0` is null); heap-array aliases set `0x8000_0000` in the region id so `VMRuntime.addressOf` yields a *live* address for a Java array. Bounds/liveness/read-only checked; OOB/use-after-free becomes a Java `IndexOutOfBoundsException`/`IllegalStateException`, never UB. Global cap 256 MiB (`ACOS_NATIVE_MEM_MAX`). Implements all `libcore.io.Memory` peek/poke natives (`peekByte`/`peekInt`/`peekShort`/`peekLong`, the `*Native` variants, `peek*Array`/`poke*Array` with byte-swap flags, `memmove`). Unit tests cover round-trip, bounds, read-only, cap and realloc. |
| `crates/acos-vm/src/natives/unsafe.rs` | Full long-address `Unsafe`/`jdk.internal.misc.Unsafe` surface: `get/put Byte/Short/Char/Int/Long/Float/Double(J)`, `allocateMemory`/`freeMemory`/`reallocateMemory`, `setMemory(JJB)`/`setMemory(Object,JJB)`, `copyMemory(JJJ)`/`copyMemory(Object,J,Object,J,J)`/`copyMemory0`/`copyMemoryFrom/ToPrimitiveArray`. The `(Object,J)` forms now bounds-check array access through `native_mem` (the old `/4` slot math was wrong for `long[]`/`char[]`/`short[]`). |
| `crates/acos-vm/src/natives/fwk_m2_filechannel.rs` | `FileChannelImpl.map0(IJJ)J` copies the VFS-resolved file bytes into a region (read-only for `MAP_RO`, private copy otherwise) and returns its address; `unmap0(JJ)I` frees it; `FileDispatcherImpl.size0(fd)J` reads the VFS `fstat` (`size()` needs it before `map`); `NativeThread.current()J`/`signal(J)V`. |
| `crates/acos-vm/src/natives/icu.rs` | **Root-cause fix**: `CharsetDecoderICU`/`CharsetEncoderICU` read `data[0]`/`data[1]` as *absolute* array offsets and then `setPosition` adds them to the buffer position, so the native must write back per-call **consumed/produced counts** (AOSP's JNI subtracts the starting offsets). ACOS wrote absolute offsets back, which produced `CoderMalfunctionError: Bad position 8192/8189` once `map0` worked. |
| `crates/acos-vm/src/natives/vmrt.rs` | `VMRuntime.addressOf(Object)` returns a live heap-array alias (see above) instead of the raw handle, so `ByteBuffer.allocateDirect` (`newNonMovableArray(byte.class, n)` + `addressOf`) reads/writes the real array through `Memory`. |
| `crates/acos-vm/src/vm.rs` | `make_thread` initialises `Thread.blockerLock` (a field initializer a VM-constructed green thread never runs; without it `AbstractInterruptibleChannel.begin` NPEs) **and** roots the in-construction thread across that allocation. The un-rooted allocation was a latent GC bug: under `ACOS_GC_STRESS=1` the GC collected the half-built `Thread` and the new lock reused its slot, so a later `iget` indexed a 2-field object at offset 28. This fixed the pre-existing `res_natives_gc_stress` panic too. |
| `crates/acos-vm/src/natives/mod.rs` | two registration lines (`mod native_mem;`, `native_mem::install_memory(vm)?;`) — the only edit outside the owned files, listed here. |
| `crates/acos-vm/tests/native_mem.rs` + `tests/java/NativeMemMain.java` + `tests/java/native_mem_stubs/` (**new**) | Java-level acceptance: `ByteBuffer.allocateDirect` typed round-trip, `Unsafe` allocate/put/get/set/copy/free, `libcore.io.Memory` peek/poke (scalar + byte array), a freed region raising `IllegalStateException`, and read-only `FileChannel.map` (full + sub-range) over a VFS file. |
| `tools/extract-bootclasspath.sh` | extracts `com.android.i18n` and `com.android.tzdata` `/etc/icu/*.dat` into `$OUT/apex/<apex>/etc/icu/` (the paths the VFS maps), so a forced `--force` re-extraction keeps `icudt72l.dat` and the tzdata data. The old `.cache/boot/extract-icu-w19h.sh` scratch script is no longer needed. |

## Evidence

Native-memory Java case (real corelib, `cargo test -p acos-vm --test native_mem`):

```
direct.int=1020304  direct.long=1122334455667788  direct.short=a0b  direct.byte=7f
direct.double=1.5   direct.float=2.5              direct.char=41
unsafe.long=102030405060708  unsafe.int=deadbeef  unsafe.byte=9a  unsafe.short=f0e
unsafe.float=3.25  unsafe.double=6.5  unsafe.set=5a5a5a5a5a5a5a5a  unsafe.copy=5a5a5a5a5a5a5a5a
mem.int=7f6e5d4c  mem.byte=33  mem.bytes=146  freed=IllegalStateException
map.full=1001  map.sub=50044  map.size=16,8  map.remain=16  done
```

`cargo test -p acos-vm`: all suites green except
`surface_control_natives_on_the_real_framework` (another agent's in-progress
`fwk_surface.rs`: `SurfaceControl.mNativeObject` is null after `release()`).
`res_natives` is green again after the GC root fix.

`tools/diff-dexprogs.sh --corelib real` and
`tools/diff-dexprogs.sh --gc-stress --corelib real` (clean worktree at HEAD +
this session's files, so the tree compiles): **23 pass / 2 fail / 0 unsupported**
in both modes. The two failures are pre-existing and unrelated:
`24_zip_roundtrip` (the HEAD commit records “zip fixtures 24/25 pending”) and
`25_security_digest` (`Unable to configure default providers`, already in
`results/vm/dexprogs-real-normal.json`).

## Blocker chain (each step verified by the probe)

1. `Thread.blockerLock == null` in a VM-constructed thread →
   `AbstractInterruptibleChannel.begin` NPE at `Thread.blockedOn` → the
   `FileChannelImpl.size` catch handler's `remove(-1)` masked it as
   `ArrayIndexOutOfBoundsException: Index -1 out of bounds for length 2`.
   Fixed by initialising/rooting `blockerLock`.
2. `FileDispatcherImpl.size0` unimplemented → implemented from `Vfs::fstat`.
3. `FileChannelImpl.map0` unimplemented → region copy of the VFS file.
4. `DirectByteBuffer` reads through `libcore.io.Memory`/`Unsafe` → the whole
   native-memory substrate above; `ByteBuffer.allocateDirect` needs the live
   `addressOf` alias.
5. `ICUBinary.getDataFromFile` then decoded chunks via
   `CharsetDecoderICU`; the absolute-vs-delta offset bug surfaced as
   `CoderMalfunctionError: Bad position 8192/8189`. Fixed in `icu.rs`.
6. New blocker, out of scope: `Font$Builder.nInitBuilder()J` (and the
   `nAddAxis`/`nSetSource`/`nGetFont` family). This needs a real TTF/font
   parser; a fake handle would be a hack, so it is left as the structured
   `UNSUPPORTED_JAVA_API` it is. `Typeface.<clinit>` is the next gate.

## Honesty

* No fakes: the font block is a structured `UNSUPPORTED_JAVA_API`, not a stub
  handle. `MAP_RW`/`MAP_PV` maps are private copies (no write-back), a declared
  divergence from a live `mmap`.
* The M2 gate test remains red/ignored (`app_created=false`); no Application or
  Activity callback is claimed.
* `VMRuntime.addressOf` mines a live alias into the Java array rather than a
  host pointer; that is the honest equivalent in a `#![forbid(unsafe_code)]`
  VM.


## W24-A — conscrypt boot classpath + `NativeCrypto` (fixture 25 green)

**Status: `25_security_digest` now passes `--corelib real` byte-for-byte, normal
and `ACOS_GC_STRESS=1`; `Security.getProviders()` returns the four Android
providers.** The `java.security` provider chain is no longer a blocker for M2.

### Boot classpath

`conscrypt.jar` (the `com.android.org.conscrypt` provider classes) lives in the
`com.android.conscrypt` APEX, not the ART/system boot jars. It is now merged
into `.cache/boot/android-34/BOOTCLASSPATH` by `tools/extract-bootclasspath.sh`
from the APEX's `etc/classpaths/bootclasspath.pb` (placed after the ART core
jars, so the lazy boot-jar loader finds `OpenSSLProvider`/`NativeCrypto` before
parsing `framework.jar`). The cached `BOOTCLASSPATH` was updated in place.

`java.security.Security.<clinit>` reads `java/security/security.properties`
from `core-oj.jar`; ART's fallback `Security.initializeStatic()` hardcodes the
identical four providers, so the fixture does not depend on boot-jar resource
lookup. (Verified from the real `Security` bytecode: the resource-null path
still yields `AndroidOpenSSL`, `CertPathProvider`, `BC`, `HarmonyJSSE` in that
order.)

### `NativeCrypto` (`natives/fwk_conscrypt.rs`, new)

Audited pure-Rust RustCrypto primitives only, no host OpenSSL, no home-grown
crypto: `md-5`, `sha1`, `sha2` (via `digest::DynDigest`), `hmac`, `aes`
(ECB/CBC block transform + CTR keystream), `getrandom` for `RAND_bytes`. The
OpenSSL `EVP_*`/`HMAC_CTX` handle table preserves the `NativeRef` object
identity that conscrypt relies on. `EVP_MD_CTX_copy_ex`,
`EVP_DigestSign/Verify*`, TLS/SSL `SSL_*`, `EVP_PKEY_*`, `RSA_*`, `EC_*`,
`X509_*` and AES-GCM (conscrypt's AEAD path) stay structured
`UNSUPPORTED_JAVA_API`, never faked. `NativeCryptoJni.init()`'s
`System.loadLibrary("javacrypto")` is satisfied by the framework-native loader
(`fwk_jni.rs`), exactly as the other framework JNI libraries are.

### Reflection needed by `Provider$Service.newInstance`

`sun.security.jca.GetInstance` instantiates providers through
`Class.getConstructor(Class[0]).newInstance(Object[0])`. `natives::reflect`
returned an empty constructor array and `null` for the internal lookup, so the
provider chain failed with `NoSuchMethodException` even with conscrypt present.
`fwk_reflect.rs` now materialises real `java.lang.reflect.Constructor` mirrors
for `Class.getDeclaredConstructorInternal(Class[])` and implements the no-arg
`Constructor.newInstance0(Object[])` (a non-empty argument list is a structured
`UNSUPPORTED_JAVA_API`). This is a general VM capability, listed here as an
out-of-owned-file edit.

### Fixture 25 extension (tier: NIST-verified)

`fixtures/dexprogs/25_security_digest/Main.java` gained SHA-224/384/512,
HmacSHA1 and AES-128/256-ECB, AES-128-CBC, AES-128-CTR lines. Their expected
values are the published FIPS 180-4 / FIPS 197 / NIST SP 800-38A / RFC 2202
vectors, independently reproduced with Python `hashlib`/`hmac`; these added
lines are marked **NIST-verified, not ART-captured**. The original
prefix (providers/digests/HmacSHA256/Base64) is unchanged and remains the ART
reference. `fixtures/expected-art/dexprogs/25_security_digest.stdout` is
updated accordingly; `tests/conscrypt_natives.rs` asserts the match in normal
and GC-stress modes plus the vector integrity.

### Verification

```
cargo test -p acos-vm --test conscrypt_natives   # 3 passed
cargo test -p acos-vm --lib fwk_conscrypt        # 4 passed
cargo test -p acos-vm --test dexprogs            # 2 passed
tools/diff-dexprogs.sh --corelib real            # 25 PASS; 24 pre-existing FAIL
ACOS_GC_STRESS=1 ... 25_security_digest           # byte-identical to expected-art
```

`cargo clippy -D warnings` on `fwk_conscrypt.rs`/`fwk_reflect.rs` is clean (the
remaining `acos-vm` warnings are pre-existing, in `fwk_input.rs`).

### Lifecycle probe

The probe described above could not be re-run: the shared tree currently does
not compile (`fwk_dexfile.rs`, another agent's in-flight file) and the clean
worktree at `HEAD` does not contain the uncommitted M2 runtime. Security
providers are, however, no longer a blocker for the M2 attach path: the
`Security`/provider/`MessageDigest`/`Mac` chain executes end to end. The first
M2 blocker remains `android.graphics.Typeface.<clinit>` ->
`PatternNative.compileImpl` (ICU regex / font config), unchanged.

### Edits outside the owned files (listed)

* `crates/acos-vm/src/natives/fwk_reflect.rs` — minimal constructor reflection.
* `crates/acos-vm/src/natives/mod.rs` — one-line module registration (as agreed).

---

# W23-A session — the `android.graphics.fonts.*` native surface; the font block is cleared

**Status: `Typeface.<clinit>` + `loadPreinstalledSystemFontMap` now build the
system font map from real `/system/etc/fonts.xml` and real
`/system/fonts/*.ttf`.** The M2 block moved out of the font path entirely.
Before this session the first structured blocker was

```
android.graphics.fonts.Font$Builder.nInitBuilder()J  [not implemented]
  at android.graphics.fonts.SystemFonts.createFontFamily / Typeface.<clinit>
```

After it, the debug probe's first blocker is `Landroid/hardware/OverlayProperties;->nGetDestructor()J`
(classes 690 -> 735) and the release probe on the concurrently-moving tree
runs to `java.lang.RuntimeException: bad array lengths` (classes 748) — both
after the whole `Typeface`/system-font-map path has completed.

## What runs now

`handleBindApplication` -> `Typeface.setSystemFontMap(null)` ->
`loadPreinstalledSystemFontMap()`:

1. `SystemFonts.getSystemPreinstalledFontConfig` -> `FontListParser.parse`
   (real `fonts.xml`; unchanged).
2. `SystemFonts.buildSystemFallback`/`createFontFamily`:
   `FontChannel.map0` copies each font file into
   [`natives::native_mem`], then `Font$Builder.nBuild` reads that buffer back,
   resolves the face in `acos-render`'s `FontCollection`, and stores a
   `FontRec`; `FontFamily$Builder.nBuild` stores the `FontFamilyRec`.
3. `SystemFonts.buildSystemTypefaces` -> `Typeface.initSystemDefaultTypefaces`
   -> `createFromFamilies` -> `nativeCreateFromArray`, which materialises a
   `TypefaceRec` carrying the concrete `FontRef` in `gfx_text`'s handle table.
4. `Typeface.setSystemFontMap` -> `nativeForceSetStaticFinalField`, which
   actually writes `DEFAULT`/`DEFAULT_BOLD`/`SANS_SERIF`/`SERIF`/`MONOSPACE`
   into the real `Landroid/graphics/Typeface;` class statics.

`native-tail` from the release probe (end of the run) proves the path executed:

```
Landroid/graphics/Typeface;->nativeGetWeight(J)I
Landroid/graphics/Typeface;->nativeCreateWeightAlias(JI)J
Landroid/graphics/Typeface;->nativeSetDefault(J)V
Landroid/graphics/Typeface;->nativeForceSetStaticFinalField(Ljava/lang/String;Landroid/graphics/Typeface;)V
Landroid/graphics/Typeface;->nativeCreateFromTypeface(JI)J
Landroid/graphics/Typeface;->nativeRegisterGenericFamily(Ljava/lang/String;J)V
```

A temporary instrumentation run of the same probe logged **384**
`Font$Builder.nBuild` calls, every one `resolved=true` against the real
collection (all `Roboto-Regular.ttf`, weights 100..900 + the fallback
families). The instrumentation was removed before the final build.

## Java-level acceptance (real corelib)

`cargo test -p acos-vm --test font_natives` compiles
`tests/java/FontNativesMain.java` (with `tests/java/font_stubs` supplying the
hidden `Typeface.loadPreinstalledSystemFontMap` at compile time only —
`getDeclaredMethodInternal` is still `null`, so reflection cannot reach it) and
runs it on `--corelib real` with the ICU roots seeded. Output:

```
default.present=true
default.family=sans-serif
default.style=0
bold.present=true
bold.style=1
measure.24=57.0
measure.48=113.0
family.size=1
font.weight=700
font.index=0
font.file=Roboto-Regular.ttf
font.axes=0
font.ascent=-44.53125
font.descent=11.71875
glyph.advance=21.28125
glyph.rect=true,true
done
```

The test asserts `Typeface.DEFAULT` is non-null with family `sans-serif`,
`Typeface.create("sans-serif", BOLD)` has style 1, `Paint.measureText("Hello")`
at 24/48 px is positive and scales, and the 24 px width agrees with
`acos-render`'s `TextEngine` to within 1 px (Minikin quantises per-glyph
advances to 1/64 px; acos-render keeps the float sum). The `Font`/
`FontFamily` natives (`getMetrics`, `getGlyphBounds`, `getStyle`, `getFile`,
`getAxes`) are exercised directly and produce a real ascent/descent/advance.

## Files

| file | change |
|---|---|
| `crates/acos-vm/src/natives/fwk_fonts.rs` (**new**) | Font/Font$Builder/FontFamily/FontFamily$Builder registries and natives; `Typeface.nativeCreateFromArray`, `nativeForceSetStaticFinalField`, `nativeGetSupportedAxes`; buffer->`acos-render` face resolution. |
| `crates/acos-vm/src/natives/gfx_text.rs` | `TypefaceRec` gained `font: Option<FontRef>`; `style()` uses the concrete face when present; `new_typeface` compares family/weight/italic. No native signature changed. |
| `crates/acos-vm/src/natives/mod.rs` | `mod fwk_fonts;` + one `fwk_fonts::install(vm)?;` after `gfx_text` (the two `Typeface` natives override the W19-B placeholders). |
| `crates/acos-vm/Cargo.toml` | added `ttf-parser = "0.25"` (already in the workspace lock via acos-render) for glyph advances/bounds and variable-axis tags. |
| `crates/acos-vm/tests/font_natives.rs`, `tests/java/FontNativesMain.java`, `tests/java/font_stubs/android/graphics/Typeface.java` (**new**) | Java-level acceptance + compile-time hidden-API stub. |

The extractor already emits `fonts/` (`fonts.xml` + `*.ttf`/`*.otf`); no change
was needed. `tools/diff-dexprogs.sh --corelib real` on the current
(concurrently-moving) tree: **24 pass / 1 fail / 0 unsupported of 25** — the
only failure is the pre-existing `25_security_digest` provider issue. The
delta from W21-A's 23 pass is the concurrently-landed zip fixture
(`24_zip_roundtrip` now passes), not this session: fonts are not on the console
`run-dex` path.

## Honesty / divergences

* `Font.nNewByteBuffer`, `Typeface.nativeReadTypefaces`, `Typeface.nativeWriteTypefaces`
  remain structured `UNSUPPORTED_JAVA_API` — ACOS cannot synthesise a
  `DirectByteBuffer` over an arbitrary native address from a native, and the
  serialised font-map transport is unused by the preinstalled path. No fake
  handle is returned.
* `FontFamily`'s custom/default-fallback flags are accepted and discarded (the
  Java `FontFamily` surface never reads them back).
* Variable-font axes are stored (`getAxes`) and reported
  (`getSupportedAxes`); `rustybuzz` applies them to shaping, while the FreeType
  A8 mask path still renders the default instance (pre-existing ADR-0019
  limitation, unchanged).
* `cargo clippy -D warnings` is clean on the three files touched; the
  crate-wide run is currently blocked by warnings in the concurrent
  `fwk_conscrypt.rs`, not by this session.
* The M2 gate is still red: `app_created=false`, `tx_delivered=false`; no
  Application/Activity callback is claimed.

---

# W24b session — app class loader + real `ClientTransaction` delivery (T-23)

**Status: the app class loader is real and tested end to end; the
`ClientTransaction` is built and executed by the real framework, but
`Application.onCreate`/`Activity.onCreate` are still blocked outside this
task's files by the display-service `DisplayInfo` reply.**

## 1. App class loader (`dalvik.system.DexFile`)

New `crates/acos-vm/src/natives/fwk_dexfile.rs` (one-line registration in
`natives/mod.rs`) implements the class-loader DEX natives against the
sandboxed VFS and the VM's real DEX providers:

| native | behaviour |
|---|---|
| `openDexFileNative(sourceName, outputName, flags, loader, elements)` | opens the **guest** path through `Vfs` (never a host path), accepts a plain `.dex`, a zip/APK (`classes*.dex`, sorted) or a directory of `.dex` files, parses each with `acos_dex`, registers it with `add_shared_dex`, and returns ART's cookie type — a Java `long[]` of provider indices. |
| `openInMemoryDexFilesNative(bufs, arrays, starts, ends, ...)` | reads the `[position, limit)` window from the non-direct backing array, or from a direct buffer's `memoryRef.allocatedAddress` region. |
| `defineClassNative(name, loader, cookie, dexFile)` | converts the binary name to a descriptor, resolves it **only in the cookie's providers** (`Vm::load_class_from_dex`), and returns the real `java.lang.Class` (`Vm::class_object`). Not found → `null`, exactly like ART (the Java wrapper records `NoClassDefFoundError`). |
| `getClassNameList(cookie)` | enumerates the cookie's real `class_defs`, dotted names. |
| `closeDexFile` | returns true. Declared divergence: ACOS has no class unloading, so a provider is permanent (the cookie is dropped). |
| `setTrusted` / `verifyInBackgroundNative` | no-ops (ACOS never runs a background verifier). |
| `isDexOptNeeded` / `getDexOptNeeded` | report "not needed": ACOS runs DEX directly and has no oat pipeline, i.e. the same answer a device gives for an already-optimised APK. Compiler-filter predicates report `assume-verified`/valid. |
| OAT introspection (`getDexFileOutputPaths`, `getDexFileStatus`, `getDexFileOptimizationStatus`, `getStaticSizeOfDexFile`) | deliberately unregistered → structured `UNSUPPORTED_JAVA_API`. |

`Vm::load_class_from_dex` is a new `pub(crate)` method in `acos-vm/src/vm.rs`
(a public wrapper over the existing `load_dex_class`); it returns an already
loaded class by descriptor first, matching `defineClassNative`'s repeat-call
semantics.

`crates/acos-runtime/src/apk_loader.rs` stages the fixture APK into the guest
sandbox at `<dataDir>/base.apk` and rewrites `ApplicationInfo.sourceDir` /
`publicSourceDir` to that guest path. From then on the framework's
`LoadedApk`/`ClassLoaderFactory`/`ResourcesManager` use a `/data/...` path.
`fwk_res.rs`'s path mapper now maps `/data/**` onto the same VFS session root,
so `ApkAssets.nativeLoad` can open the app APK.

### Acceptance

`crates/acos-runtime/tests/dex_loader.rs` + `tests/java/DexLoaderMain.java` (+ a
signature-only `MainActivity` stub in `tests/java/stubs/`) runs on the real
API-34 core library and proves, in this order:

1. the counter APK is staged at
   `/data/user/0/dev.acos.fixtures.counter/base.apk`;
2. `dev.acos.fixtures.counter.MainActivity` is **not** in the VM's providers
   before the loader runs (asserted, so the test cannot pass trivially);
3. a real `new PathClassLoader(apkPath, ClassLoader.getSystemClassLoader())`
   resolves the class through `BaseDexClassLoader` → `DexPathList` →
   `DexFile.defineClassNative`;
4. the app class's own static `MainActivity.probe()` (call site compiled against
   the d8 `--lib` stub, never bundled) returns **4242**;
5. after the run the VM has an APK-derived provider and the class descriptor is
   defined.

`cargo test -p acos-runtime --test dex_loader`: **1 passed**.

## 2. `ClientTransaction` delivery (T-23)

New `crates/acos-runtime/src/client_transaction.rs` builds the real API-34
objects through the Java bridge:

* `ActivityInfo` from the manifest's `MAIN`/`LAUNCHER` activity (via
  `pms_java::activity_info`, so it carries the staged `ApplicationInfo`);
* a real `Intent.setClassName(package, activity)`;
* `LaunchActivityItem.obtain(intent, ident, info, config, config, …)` (21 args,
  `PROCESS_STATE_TOP`);
* `ResumeActivityItem.obtain(false, false)`;
* `ClientTransaction.obtain(appThread, token)` with a real `android.os.Binder`
  activity token, `addCallback(launchItem)` and
  `setLifecycleStateRequest(resumeItem)`;
* the real `ActivityThread$ApplicationThread.scheduleTransaction(ClientTransaction)`
  bytecode (→ `ActivityThread.scheduleTransaction` → `H.EXECUTE_TRANSACTION`).

The `Parcel` boundary between two in-process objects is skipped (there is no
remote process), the same documented invariant `bind_app.rs` follows; every
object and every consuming method is the real framework.

`LifecycleReport` now separates two facts:

* `transaction_scheduled` — `scheduleTransaction` accepted the transaction;
* `transaction_delivered` — `ActivityThread.mActivities[token].activity` is
  non-null (i.e. `Activity.onCreate` returned). The honesty test keeps its old
  meaning.

`crates/acos-runtime/tests/client_transaction.rs` boots `ActivityThread`,
enters `Looper.loop()`, stages the counter APK and delivers the transaction.
Evidence (`--nocapture`):

```
client_transaction: scheduled=true record=false run=Err(UnsupportedApi { api: "Landroid/net/TetheringManager;", ... })
exact blocker after handleLaunchActivity entry, before onCreate: Landroid/net/TetheringManager; is not implemented (at )
```

The test asserts `ActivityThread.mSomeActivitiesChanged == true`, which
`handleLaunchActivity` sets at entry — the real `LaunchActivityItem.execute`
ran and called `handleLaunchActivity` → `performLaunchActivity`. It then stops
in `LoadedApk.makeApplication` because `android.net.TetheringManager` is not in
the extracted boot classpath (`framework.jar` references it but does not define
it; it lives in `framework-connectivity.jar`, which
`tools/extract-bootclasspath.sh` does not extract). That is boot-classpath
coverage, not the transaction machinery.

## 3. Cross-scope blockers found (documented, not faked)

The full `bindApplication` chain now gets past `Typeface.<clinit>` (fonts
landed) and past `OverlayProperties.<clinit>` (W24b added
`crates/acos-vm/src/natives/fwk_display.rs` + `nativeGetThemeFreeFunction` in
`fwk_res.rs`: ACOS themes/overlay properties are Rust handles or `null`, so the
`NativeAllocationRegistry` free function is genuinely absent). It then throws:

```
java.lang.RuntimeException: bad array lengths
  at android.os.Looper.loopOnce(220)
  at android.os.Looper.loop(294)
```

from `IDisplayManager.getDisplayInfo` / `getOverlaySupport` during
`ResourcesManager.applyConfigurationToResources`. The reply encoder
`acos-services/src/display.rs::DisplayInfo::to_parcelable` (with
`FieldOp::IgnoredNamed` cutout/shape and a bare `Int(1)` for
`supportedModes`) does not match the real `DisplayInfo.writeToParcel` /
`Mode[]` / `DisplayCutout` reader, so the `Parcel` desynchronises and
`readIntArray`/`readBinderArray` sees a bad length. This is the display
service's parcelable codec (WMS/display owner), not a native; it is the next
gate for `Application.onCreate`.

Second cross-scope gate, after the transaction: `Landroid/net/TetheringManager;`
is absent from the boot-classpath cache. The extractor should include
`/system/framework/framework-connectivity.jar` (or the APEX that exports it).

## 4. Honesty

* No fake class loader: the dex is opened from the guest VFS, `defineClassNative`
  resolves only in that `DexFile`'s providers, and the test asserts the class was
  not preloaded.
* No fake transaction: the framework builds the transaction through its own
  `obtain` factories; `transaction_delivered` is only true when the
  `ActivityClientRecord.activity` field is set (onCreate returned).
* `closeDexFile`/`isDexOptNeeded`/`nativeGetThemeFreeFunction` return
  structurally-correct values for an interpreter with no unload/oat/malloc'd
  theme; the divergences are listed above rather than hidden.
* No `Ok(())` was used for the unimplemented OAT introspection natives; they
  remain structured `UNSUPPORTED_JAVA_API`.

## 5. Files

New: `crates/acos-vm/src/natives/fwk_dexfile.rs`,
`crates/acos-vm/src/natives/fwk_display.rs`,
`crates/acos-runtime/src/apk_loader.rs`,
`crates/acos-runtime/src/client_transaction.rs`,
`crates/acos-runtime/tests/dex_loader.rs`,
`crates/acos-runtime/tests/client_transaction.rs`,
`crates/acos-runtime/tests/java/DexLoaderMain.java`,
`crates/acos-runtime/tests/java/stubs/dev/acos/fixtures/counter/MainActivity.java`.
Edited: `natives/mod.rs` (registration), `vm.rs` (`load_class_from_dex`),
`Cargo.toml` (`zip`), `fwk_res.rs` (`/data/**` mapping +
`nativeGetThemeFreeFunction`), `lifecycle.rs`, `bind_app.rs` (`build_config`
visibility), `pms_java.rs` (`activity_info` visibility), `tests/m2.rs`,
`examples/lifecycle_probe.rs`, `fixtures/apps/counter/.../MainActivity.java`
(static `probe()` test hook).

## 6. Verification

```
cargo test -p acos-runtime --test dex_loader           # 1 passed
cargo test -p acos-runtime --test client_transaction   # 1 passed
cargo test -p acos-runtime --test m2                   # 2 passed, gate ignored
```

The M2 gate test stays `#[ignore]`d with the current exact reason
(`DisplayInfo` reply / `bad array lengths`), never a silent pass.

## 7. W26-A: DisplayInfo reply, APEX boot classpath, M2 chain natives

HEAD `40f075c`. The two cross-scope blockers recorded in §3 are fixed, and the
gate now reaches a new, precise blocker.

### 7.1 `IDisplayManager` replies are real framework parcelables

API 34's `DisplayInfo.writeToParcel` writes **49** fields. The previous
`acos-services::display::DisplayInfo::to_parcelable` emitted a 21-field prefix
whose order did not even match (`layerStack, flags, type, displayId,
displayGroupId` come first, then `name`), and wrote `supportedModes` as a bare
`Int(1)`. `DisplayInfo.CREATOR` then read a `Mode[]` out of the following
fields and the `Parcel` desynchronised (`RuntimeException: bad array lengths`).

New `crates/acos-runtime/src/display_java.rs` materialises the real Java
`android.view.DisplayInfo` / `android.hardware.OverlayProperties` and calls
their own `writeToParcel` into a real `Parcel`, following the §3.6 invariant.
The transport routes `android.hardware.display.IDisplayManager` replies through
it exactly like `IPackageManager` (`transport.rs`). Values: display 1080x2400,
app 1080x2209, @420dpi, `Display.Mode(1,1080,2400,60.000004)`,
`DisplayCutout.NO_CUTOUT`, and an `OverlayProperties` with the ACOS "no
overlay" native handle. The `overlay` case also exposed that the old two-bool
`OverlayProperties` codec never matched the real `int` native-object marker.

Evidence: `cargo test -p acos-runtime --test display` drives the real
`IDisplayManager$Stub$Proxy` over the Binder transport; `DisplayMain.java`
asserts every field after the real `DisplayInfo.CREATOR` read, including the
`Mode[]` and the overlay object. It passes.

### 7.2 Boot classpath merges every APEX classpath fragment

`android.net.TetheringManager` is defined in `framework-connectivity.jar`
inside `com.google.android.tethering.apex`, not `framework.jar`.
`tools/extract-bootclasspath.sh` now extracts **every** `/system/apex/*.apex`,
merges all `*_bootclasspath.pb` / `*_systemserverclasspath.pb` fragments (not a
hard-coded source list), and emits `BOOTCLASSPATH` plus a new
`SYSTEMSERVERCLASSPATH`. The re-extracted cache's `BOOTCLASSPATH` grew from 14
to 35 jars (framework-connectivity, framework-tethering, framework-permission,
framework-wifi, framework-uwb, framework-adservices, updatable-media,
framework-sdkextensions, framework-bluetooth, android.net.ipsec.ike, ...).

Evidence: `cargo test -p acos-runtime --test boot_classpath` resolves six
classes that live in APEX boot jars through the real VM boot loader.

### 7.3 M2 chain: new `ActivityThread` / class-loader natives

`crates/acos-vm/src/natives/fwk_m2_chain.rs` (registered from `natives/mod.rs`)
lands the small natives the real `handleBindApplication` needs, each documented
with its framework call site:

| native | real semantics |
| --- | --- |
| `ActivityThread.nInitZygoteChildHeapProfiling()V` | arms heapprofd only when profiling is configured; no ART heap in ACOS, so structural no-op |
| `ClassLoaderFactory.createClassloaderNamespace(...)String` | installs a dlopen namespace; ACOS has none, so `null` = success (the real `PathClassLoader` is already built by `apk_loader`) |

`natives/vmrt.rs`'s `VMRuntime.properties()` now also returns
`java.library.path=/system/lib64:/system/lib`. API 34
`LoadedApk.createOrUpdateClassLoaderLocked` reads it **unconditionally**
(`System.getProperty("java.library.path").contains("/vendor/lib")`), so the
missing property was a fatal NPE during `handleBindApplication`
(`LoadedApk.createOrUpdateClassLoaderLocked:897`).

### 7.4 Exact current blocker

```
M2[normal] wall=16.7s peak_rss=452068kB classes=1537 bind=true app=false
           txscheduled=false tx=false quit=false events=[]
error: vm: Llibcore/io/Linux;->setxattr(Ljava/lang/String;Ljava/lang/String;[BI)V
  is not implemented
    at libcore.io.Linux.setxattr
    at libcore.io.ForwardingOs.setxattr
    at libcore.io.BlockGuardOs.setxattr
    at android.system.Os.setxattr
    at android.app.ContextImpl.ensurePrivateDirExists
    at android.app.ContextImpl.ensurePrivateCacheDirExists
    at android.app.ContextImpl.getCacheDir
    at android.app.ActivityThread.setupGraphicsSupport
    at android.app.ActivityThread.handleBindApplication
```

`ContextImpl.ensurePrivateDirExists` sets a security xattr on the newly created
cache dir and catches `ErrnoException`, so the correct implementation is for
`Llibcore/io/Linux;.setxattr` to raise
`android.system.ErrnoException(ENOTSUP)` (the guest VFS models no xattrs), not
to silently succeed. That native lives in `natives/io_linux.rs`, which the
file-I/O W25 workstream is actively extending, so it was recorded rather than
edited here. Owner: the file-I/O/W25 workstream.

`classes=1537` is the class count at the blocker. The gate stays `#[ignore]`d
with exactly this reason; `Application.onCreate` has still not run
(`app=false`), and no lifecycle callback is faked. The display and boot-class
gates before it are now honest passes.

### 7.5 Verification (W26-A)

```
cargo test -p acos-services -p acos-runtime      # all non-ignored pass
cargo test -p acos-runtime --test display         # 1 passed (real CREATOR round trip)
cargo test -p acos-runtime --test boot_classpath  # 1 passed (APEX boot classes)
cargo test -p acos-runtime --test m2 -- --ignored # fails at the exact setxattr blocker
tools/diff-dexprogs.sh                            # real 25/25 unchanged
```

New: `crates/acos-runtime/src/display_java.rs`,
`crates/acos-runtime/tests/display.rs`,
`crates/acos-runtime/tests/boot_classpath.rs`,
`crates/acos-runtime/tests/java/DisplayMain.java`,
`crates/acos-runtime/tests/java/stubs/android/view/DisplayInfo.java`,
`crates/acos-runtime/tests/java/stubs/android/hardware/OverlayProperties.java`,
`crates/acos-runtime/tests/java/stubs/android/hardware/display/IDisplayManager.java`,
`crates/acos-vm/src/natives/fwk_m2_chain.rs`.
Edited: `crates/acos-services/src/display.rs` (served-value cache),
`crates/acos-runtime/src/{lib,pms_java,transport}.rs`,
`crates/acos-vm/src/natives/{mod,vmrt}.rs`, `tools/extract-bootclasspath.sh`,
`crates/acos-runtime/tests/m2.rs` (ignore reason).

---

# W29-A session — M2 chain: `Application.onCreate` reached; launch into `performLaunchActivity`/`PhoneWindow`

Started at `1cc85a3`; the tree later advanced to `da35f21`. The `setxattr`
blocker in §7 is cleared, and the chain now runs
`ApplicationThread.bindApplication` → `Application.onCreate`
(`app_created=true`) and the `ClientTransaction` launch into
`ActivityThread.performLaunchActivity` → `Activity.attach` →
`PhoneWindow.<init>`. The full gate is still `#[ignore]`d; the exact blocker is
below.

## What runs now (evidence)

```
cargo run -q -p acos-runtime --example lifecycle_probe -- fixtures/build/apps/hello-views.apk
```

| metric | value |
|---|---:|
| `bindApplication` delivered | yes |
| `ActivityThread.mInitialApplication` non-null (`Application.onCreate` ran) | **yes** |
| `ClientTransaction` scheduled | yes |
| launch reached `performLaunchActivity`/`Activity.attach`/`PhoneWindow.<init>` | yes |
| classes at block | ~1759 |
| first structured blocker | `Settings.getPublicSettingsForClass` NPE |

## Blockers cleared (each verified by the probe)

| native/service | change | real semantics |
| --- | --- | --- |
| `Llibcore/io/Linux;.setxattr` | new VFS `user.*` xattr store (`vfs.rs`) + `setxattr`/`getxattr`/`listxattr`/`removexattr` (`io_linux.rs`); `/data/user_de/**` mapped and `data/user_de/0` created | the device data partition carries `user.inode_cache`; reads return exactly what was written |
| `ApplicationInfo` data-dir siblings | `deviceProtectedDataDir`/`credentialProtectedDataDir` populated (`pms.rs`, `pms_java.rs`) | a device-protected `Context` resolves `/data/user_de/0/<pkg>` |
| `Parcel` exception header | `write_exception` writes `code, String msg, int stackSize(0)[, errorCode]` for every failing code (`exception.rs`) | AOSP `writeException`/`readException` symmetry; a message-less header desynchronised the reply |
| `ApplicationInfo` reply layout | spec + `to_parcelable` gained the two dir fields (`parcelable.rs`) | staging codec for the real-Java rewrite |
| `HardwareRenderer.setupShadersDiskCache` | recorded no-op (`gfx_hwui.rs`) | no native GPU/Skia shader cache exists; the paths are recorded |
| `NetworkUtils.getBoundNetworkHandleForProcess` | `0` (`fwk_m2_chain2.rs`) | the process is never bound; `getProcessDefaultNetwork` returns null |
| `am_get_assigned_package_identifiers` | builds the real `SparseArray` fields directly (`fwk_res.rs`) | exact result; avoids the Java `put` → `GrowingArrayUtils.insert` → `Array.newInstance`/`System.arraycopy` path, which can unwind past a native frame |
| `LaunchActivityItem.obtain` | `deviceId`/`procState` args swapped to the API 34 order (`client_transaction.rs`) | API 34 order is `… overrideConfig, deviceId, referrer, voiceInteractor, procState, …`; ACOS passed `2` as `deviceId`, so `ContextImpl.updateDeviceId` NPE'd on a null `VirtualDeviceManager` |
| `Field.getAnnotationNative` | `null` (`fwk_m2_chain2.rs`) | ACOS does not parse annotation metadata (the same declared divergence as `Class.getDeclaredAnnotation`) |

## Exact current blocker (root cause + owner)

```text
handleLaunchActivity -> performLaunchActivity -> Activity.attach
  -> PhoneWindow.<init> -> Settings.Global.<clinit>
  -> Settings.getPublicSettingsForClass   (pc=27: value.getClass())
```

`getPublicSettingsForClass` reads each static `String` field with `Field.get`
and dereferences it; `Field.get` returned `null` for
`Settings.Global.DEFAULT_DNS_SERVER` (declaration index 154). Root cause:
`fwk_reflect::make_field` stores the **declaration index** in the `Field`
mirror's `artFieldIndex`, but `Vm`'s `statics` vector is **slot-indexed** (a
`long`/`double` reserves two slots), so every static field after a wide field
reads the wrong slot. Fix: set `artFieldIndex` to `FieldDef::offset` (the slot
offset) for static fields, or index `read_field`/`write_field` by
`(offset - FIELD_BASE)/4`. Owner: reflection.

With that one-line fix applied (not landed), the chain proceeds further and
hits a second, VM-level bug: an exception thrown inside a nested `<clinit>`
(via `initialize_class`) is caught by a handler below the synchronous Rust
call's base frame, so `handle_exception_inner` unwinds past
`initialize_class`/`execute` and the interpreter continues with a stale frame
index (`set_ref` panics: `frames.len()=3, fi=13`). Fix: stop exception
unwinding at the innermost `call_method`/`initialize_class` base (an
`exception_floor`). W29-A prototyped the floor; it is correct in shape but
regressed early boot, so it was not landed and the offset fix was reverted to
keep the non-ignored suite green. Owner: interpreter/scheduler.

## `client_transaction` test

`cargo test -p acos-runtime --test client_transaction` still hangs
(pre-existing, flagged by the coordinator). New finding: `interp::run` returns
`Ok(())` in under a second and `parked=false`; the spin is **after** the run, in
the test's post-run Java/reflection calls (`ActivityThread.getActivityClient`
and the `Throwable.getCause` walk in the test helper). That is outside the
lifecycle chain. No bound was landed: the `HostGuard` checks added to
`run_current_until`/`native_poll_once` never reached the spin and were
reverted.

## Verification (W29-A, clean worktree `.cache/wt-w29a` @ HEAD)

```
cargo test -p acos-services -p acos-runtime -- \
  --skip client_transaction_is_built_and_delivered_to_the_real_application_thread
# all non-ignored pass (m2: 2 passed / 1 ignored; acos-services lib 25/25, etc.)
cargo clippy -p acos-vm -p acos-runtime -p acos-services --all-targets
# no warning in any touched file (one unrelated `IIT` warning in services_t38.rs)
```

Files: `crates/acos-services/src/{exception,pms,parcelable}.rs`,
`crates/acos-runtime/src/{client_transaction,pms_java}.rs`,
`crates/acos-vm/src/{vfs.rs,natives/{io_linux,gfx_hwui,fwk_res,fwk_m2_chain2,mod}.rs}`.

## W33-A: reflection slot fix landed, exception floor, spin root-caused

**Status:** the static-field slot bug that blocked `Settings.Global.<clinit>`
is fixed and the M2 chain moves one blocker further. The new block is the
`Settings$Readable` runtime annotation, which is a genuine missing feature
(annotation metadata → Java annotation proxy), not a slot bug.

### 1. Reflection static-slot fix (`natives/reflect.rs`)

`Field.artFieldIndex` is the *declaration index* (`getNameInternal`,
`getDeclaredFields` order) but the VM's static storage is **slot-indexed**
(`long`/`double` occupy two slots), so any static field declared after a wide
field read the wrong slot. `Settings.Global.DEFAULT_DNS_SERVER` (after the
`long serialVersionUID`) therefore read a neighbouring slot and returned
`null`, NPE'ing `Settings.getPublicSettingsForClass` before `Application.onCreate`.

The fix resolves every read/write through the field definition's own `offset`
(`field_slot`), which is already the correct slot index for both statics
(`statics.len()`) and instance fields (`instance_slot_count`). It also adds
ART-accurate boxing/unboxing for `Field.get`/`set` (`valueOf` / `xValue`) and
handles wide halves. Regression test:
`crates/acos-runtime/tests/reflect_fields.rs` +
`tests/java/ReflectFieldsMain.java` (interleaved wide/narrow static *and*
instance fields, typed and boxed reads, typed sets, `getDeclaredFields`
grouping).

### 2. Exception floor + `ExceptionInInitializerError`

Two independent fixes:

* **Pre-existing pending exception.** `run_current_until_body` spun forever
  when `vm.pending_exception` was already set: `run_batch` returns 0
  immediately (it bails on a pending exception) without popping the frame
  `call_method` just pushed, so `while frames.len() > base_depth` never
  terminated. It now unwinds to the call's base (`pop_frame_quiet`) and returns
  `Err(JavaException)`. This is the real cause of the `client_transaction`
  spin; W29-A's `HostGuard` in `run_current_until` never reached it because the
  loop never made progress.
* **`<clinit>` wrapping.** A nested synchronous invocation now installs an
  `EXCEPTION_FLOOR` (`0` at the top-level scheduler run; the call base for
  `call_method`/`initialize_class`), so a `<clinit>` failure unwinds only to
  the Rust→Java call base instead of past it (the old `set_ref` panic,
  `frames.len()=3 fi=13`). `initialize_class` wraps the cause in
  `ExceptionInInitializerError` and marks the class `Failed` so a later access
  throws `NoClassDefFoundError`. API 34's
  `ExceptionInInitializerError.getCause()` overrides `Throwable.getCause()` to
  read its own `exception` field, so the wrapper sets **both** `exception` and
  `cause` (the bootstrap corelib has no `exception` field and inherits
  `Throwable.cause`). Tests:
  `crates/acos-runtime/tests/exception_floor.rs` and
  `crates/acos-vm/tests/java_cases.rs::exception_floor_rethrows_at_the_rust_call_base`.

### 3. `client_transaction` spin

Root cause as above (pre-existing pending exception + pushed frame). The test
helper also had to clear `vm.pending_exception` *before* calling Java methods
on the throwable (`describe`), because the VM refuses to run Java while an
exception is pending. `cargo test -p acos-runtime --test client_transaction`
now finishes in ~1.4 s (was an unbounded spin) and still asserts
`LaunchActivityItem.execute` reached `handleLaunchActivity`.

### 4. Next blocker: `Settings$Readable` runtime annotation

The lifecycle probe now gets past `Settings.Global.<clinit>` and dies here:

```
java.lang.RuntimeException: Unable to start activity
  ComponentInfo{dev.acos.fixtures.helloviews/...MainActivity}:
  java.lang.SecurityException: Settings key: <render_shadows_in_compositor>
  is not readable. From S+, settings keys annotated with @hide are restricted
  to system_server and system apps only, unless they are annotated with @Readable.
```

`Settings$NameValueCache.getPublicSettingsForClass` iterates
`Class.getDeclaredFields()`, reads each `String` static, then calls
`field.getAnnotation(Settings$Readable.class)`. API 34's annotation is
**`Landroid/provider/Settings$Readable;`** (a nested annotation), not
`android.annotation.Readable`; the field `DEVELOPMENT_RENDER_SHADOWS_IN_COMPOSITOR`
*is* `@Readable`. ACOS's `Field.getAnnotationNative`
(`natives/fwk_m2_chain2.rs`) honestly returns `null` (no annotation metadata is
parsed), so `mReadableFields` never contains the key and the framework throws
`SecurityException` for a key that real Android allows.

This is a real missing feature, and it is medium-sized, not a one-liner:

* `Field.getAnnotationNative(Class)` must return an object assignable to the
  requested annotation interface. `libcore.reflect.AnnotationFactory.createAnnotation(Class, AnnotationMember[])`
  is **public static** and would build the real proxy, but ACOS must first
  decode the field's `annotations_directory_item` (`acos-dex` already parses
  `AnnotationsDirectory::field_annotations`), map each `FieldDef` to its DEX
  `field_idx`, box the `EncodedValue` elements, and construct
  `libcore.reflect.AnnotationMember[]`.
* Until then the honest options are to keep returning `null` (documented
  divergence) or to raise `UNSUPPORTED_JAVA_API`; returning a synthetic
  annotation for all fields would make restricted keys readable and is
  dishonest.

The remaining natives on the tail are
`nativeGetAssignedPackageIdentifiers` / `flushPendingCommands` /
`nativeThemeApplyStyle` / `HardwareRenderer.nSetDisplayDensityDpi` etc., all
already implemented.

### 5. W32-B JNI `native_roots` finding (extra item)

The "latent VM root-accounting bug" recorded in `results/jni/JNI_COVERAGE.md`
is **not** a VM accounting bug. A focused probe
(`ThrowNew` → `ExceptionCheck` → `ExceptionClear` → drop, and
`ThrowNew` → `ExceptionOccurred` → `DeleteLocalRef` → `ExceptionClear` → drop)
shows `native_roots` is exactly balanced, in both plain and
`ACOS_GC_STRESS=1` builds:

```
base=3 after_throw=4 after_clear=3         # set_pending pushes 1, take_pending removes 1
base=5 after_delete=4 after_clear=3        # local + pending roots released one-for-one
```

The corruption seen by W32-B's reduced tests comes from calling
`JniRuntime::bind(&mut vm)` **before** the `Vm` reaches its final address (e.g.
inside a `setup()` that then moves the `Vm` out of its return tuple). `attach`
deliberately does not retain a `*mut Vm` for exactly this reason, but `bind`
necessarily stores it, so binding an about-to-be-moved `Vm` leaves a stale
pointer and the JNI calls mutate/free the moved-from `Vm`'s `Vec` fields. With
the canonical pattern (`attach` in `setup`, `bind` after the move, as in
`crates/acos-jni/tests/jni_errors.rs`) the accounting is sound. The correct
documentation fix is to call out the footgun next to `bind`, not to add a
VM-side accounting change.

## W38-A — DEX annotation reflection (fixture 27) and the next M2 blocker

### What was implemented

`Field.getAnnotationNative` no longer returns `null`. DEX annotation metadata
was already parsed into `acos_dex::AnnotationsDirectory`; the missing piece was
the reflection surface. New module `crates/acos-vm/src/natives/fwk_annotations.rs`:

* `Class.getDeclaredMethodsUnchecked` / `getDeclaredConstructorsInternal` /
  `getDeclaredConstructorInternal` / `getDeclaredMethodInternal` now build real
  `Method` / `Constructor` mirrors (the `Executable` layout: `declaringClass`,
  `accessFlags`, `artMethod`, `dexMethodIndex`).
* `Executable.getMethodNameInternal` / `getMethodReturnTypeInternal` /
  `getParameterTypesInternal` / `getParameterCountInternal` /
  `compareMethodParametersInternal`, and `Method.getExceptionTypes` /
  `getDefaultValue`.
* `Class` / `Field` / `Executable` `getAnnotationNative`,
  `getDeclaredAnnotations(Native)`, `isAnnotationPresentNative`,
  `getParameterAnnotationsNative`, `getSignatureAnnotation`.
* Annotation instances are materialised exactly as ART does: the encoded
  elements are decoded (ints, strings, enums, classes, arrays, nested
  annotations, defaults) into `libcore.reflect.AnnotationMember[]` and passed to
  the **real** `libcore.reflect.AnnotationFactory.createAnnotation(Class,
  AnnotationMember[])`, which returns the real `java.lang.reflect.Proxy`.
  `Method.getDefaultValue` reads `@dalvik.annotation.AnnotationDefault` from the
  annotation interface class (where d8 actually puts it).
* New module `crates/acos-vm/src/natives/fwk_proxy.rs` implements the native
  `java.lang.reflect.Proxy.generateProxy(...)`: it synthesises a class that
  extends the real `Proxy`, implements every interface method plus the `Object`
  contract, stores the handler in `Proxy.h`, and dispatches each call to
  `InvocationHandler.invoke(proxy, method, args)`. `Constructor.newInstance0`
  now accepts non-empty arguments. This also makes hand-written
  `Proxy.newProxyInstance` work.

`Method.equals` compares `getName()` results by **reference**, so method names
are interned (`intern_str`), matching ART. Without that, `AnnotationFactory`
rejected every element method with `IllegalArgumentException`.

### Evidence

* New fixture `fixtures/dexprogs/27_annotations_reflection` (RUNTIME annotations
  with string/int/boolean/enum/class/int[]/String[]/nested elements and
  defaults on a class, field, method and parameter; `getAnnotation`,
  `isAnnotationPresent`, `getDeclaredAnnotations`, `toString`/`equals`/
  `hashCode`/`annotationType`, `getGenericSuperclass`).
  Captured from the API 34 oracle emulator (3 runs, deterministic) into
  `fixtures/expected-art/dexprogs/27_annotations_reflection.{stdout,stderr,exit}`.
  `acos run-dex --corelib real` is **byte-for-byte identical** to ART, normal and
  `ACOS_GC_STRESS=1`.
* The full real-corelib corpus (01–27) matches `fixtures/expected-art/dexprogs`
  byte-for-byte and by exit code, normal and GC-stress.
* `cargo test -p acos-dex -p acos-vm -- --skip thousand_events` passes except the
  pre-existing `res_natives` failure (see below). The bootstrap `dexprogs` test
  excludes 27 as `REAL_CORELIB_ONLY` (annotations require the real corelib and
  the ART reference).

### M2 progress

`acos launch fixtures/build/apps/counter.apk` now gets **past**
`Settings$Readable` (the W33-A blocker) and reaches `application_on_create`:

```
[    blocked] application_on_create
    JAVA_EXCEPTION: java.lang.ExceptionInInitializerError
[not-reached] client_transaction
```

The next blocker is an `ExceptionInInitializerError` raised while
`Application.onCreate` runs. The launch diagnostic does not yet unwrap the
wrapper's `cause`, so the exact failing `<clinit>` is not identified; the
immediate next step is to unwrap `ExceptionInInitializerError.exception` in the
probe/diagnostic and read the cause's class + first stack frame, then implement
whatever native it names.

### Caveats

* At the time of writing the shared worktree did not compile: a concurrent
  agent's in-progress `crates/acos-vm/src/exec.rs` has `apply_sign` private,
  `Program.field_refs` missing and a non-exhaustive `Op` match. All test
  evidence above was collected in a clean `git worktree` at HEAD (`ccd7ee7`)
  with only this change applied.
* `crates/acos-vm/tests/res_natives.rs` fails at HEAD (`resolve=false` vs the
  expected `resolve=true`); it is untouched by this change and pre-existing.

## W40-A — launch diagnostics, ICU data path, per-APK PMS, content-provider arg order

Worked in a clean `git worktree` of HEAD (`66e4232`) at
`~/Projects/acos-w40a` with `CARGO_TARGET_DIR=.cache/target-w40a`,
`CARGO_PROFILE_DEV_DEBUG=0`; the shared tree's W39-A `exec.rs` WIP was not
touched. Evidence commands below use
`ACOS_BOOT_DIR=~/Projects/acos/.cache/boot/android-34`.

### Fix 1 — cause-chain diagnostics (the requested Step 1)

`Vm::throwable_cause` follows `ExceptionInInitializerError.exception` (API 34
overrides `getCause()`) / `Throwable.cause`, treating `cause == this` as "no
cause" (the default `Throwable` constructor self-reference). `throw_new` now
captures the stack at construction for VM-thrown exceptions, and
`lifecycle::describe_throwable` walks up to 8 causes and prints each
`caused by: <class>: <message>` plus frames.

Before: `java exception: java.lang.ExceptionInInitializerError` (frame-less).
After: the blocker that had been hidden was
`java.util.MissingResourceException: Could not find the bundle
android/icu/impl/data/icudt72b/likelySubtags` from
`android.graphics.Typeface.<clinit>`.

Regression: `crates/acos-runtime/src/lifecycle.rs`
`describe_throwable_unwraps_exception_in_initializer_error`.

### Fix 2 — ICU data under the real device path

`AndroidDataFiles.generateIcuDataPath()` reads `ANDROID_I18N_ROOT` (guest
`System.getenv` -> `libcore.io.Linux.getenv` seeds `/apex/com.android.i18n`)
and opens `<root>/etc/icu/icudt72l.dat`. The VFS mapped `/system/**` and
`/apex/com.android.tzdata/**` but not `/apex/com.android.i18n/**`, so the open
was `ENOENT` and every `Typeface.<clinit>` failed. Added the
`/apex/com.android.i18n/**` -> `<boot>/apex/com.android.i18n/**` read-only
mapping (the `/system/apex/...` spelling still works) and aligned the host env
in `boot.rs` / `font_natives.rs` to the device spelling.

Evidence: `acos launch fixtures/build/apps/counter.apk` moved from
`blocked application_on_create / ExceptionInInitializerError` to
`ok application_on_create; ok client_transaction` (classes 591 -> 1795).

Regression: `crates/acos-vm/src/vfs.rs`
`reads_i18n_apex_icu_data_under_the_real_device_path`.

### Fix 3 — per-APK package manager

`ServiceHost::standard()` seeds the PMS with the `hello-views` fixture, so
launching any other APK made `handleBindApplication` query
`getApplicationInfo(<launched pkg>)` and raise
`unknown service name "dev.acos.fixtures.counter"`. Added
`ServiceHost::install_package_from_apk` (via a `ServiceObject::as_any_mut`
downcast) and `Runtime::run_lifecycle_inner` now re-points the PMS at the APK
before boot. A missing APK is a structured error, never a fallback to the
previous package.

Regression: `crates/acos-services/tests/host.rs`
`installing_a_package_from_apk_repoints_the_package_manager`.

### Fix 4 — `getContentProvider` argument order

`IActivityManager$Stub$Proxy.getContentProvider` params are
`(caller, callingPackage, name, userId, stable)` (confirmed from the API 34
bytecode); the framework passes `context.getOpPackageName()` as
`callingPackage` and the requested authority as `name`. `AtmsService` read the
**first** string as the authority, so `Settings$NameValueCache`'s
`acquireProvider("settings")` got `null` and NPE'd at
`Settings$ContentProviderHolder.getProvider` before `Activity.onCreate`.
Now reads the second string.

Evidence: the NPE became a real framework reply problem —
`IllegalStateException: Bundle length is not aligned by 4: 778923875` in the
`android.app.ContentProviderHolder` parcel (see the blocker table).

Regression: `crates/acos-services/tests/replay.rs`
`get_content_provider_resolves_the_authority_from_the_name_argument` (the
recorded `IActivityManager.getContentProvider` case was corrected to the real
`[caller, package, settings, ...]` order).

### Evidence

* `acos launch fixtures/build/apps/counter.apk` (release, W40-A tree):
  `reached client_transaction`; stages `apk_parsed`, `process_booted`,
  `bind_application`, `application_on_create`, `client_transaction` all `ok`;
  `activity_on_create` blocked with `JAVA_EXCEPTION` (cause chain printed).
* `cargo test -p acos-runtime --test m2`: `bind_application_is_delivered...`
  and `lifecycle_never_claims_an_undelivered_transaction` pass;
  `app_created=true`, `transaction_scheduled=true`. The gate stays `#[ignore]`d
  with the new (current) blocker.
* `cargo test -p acos-vm --lib vfs::...`, `cargo test -p acos-services --test
  host --test replay` (14 tests) and the lifecycle unit test all pass.
* `tools/diff-dexprogs.sh --corelib real`: **27 pass / 0 fail / 0 unsupported
  of 27** (normal; ART expectations).

### Blocker table (furthest stage reached)

| Stage | Status | Blocker |
|---|---|---|
| bind_application | ok | — |
| application_on_create | ok | — |
| client_transaction | ok | — |
| activity_on_create | blocked | `android.app.ContentProviderHolder` parcel spec is missing `connection` (IBinder), `noReleaseNeeded` (int) and `mLocal` (int), and its `ProviderInfo.info` must be written inline (not as a typed object); Java's `ContentProviderHolder.CREATOR` therefore mis-reads `ProviderInfo.metaData` and throws `IllegalStateException: Bundle length is not aligned by 4`. Next step: model `ProviderInfo`/`ContentProviderHolder` exactly (or materialise a real Java instance and call `writeToParcel`, per the `pms_java` pattern). |

### Caveats

* `crates/acos-vm/tests/res_natives.rs` still fails at HEAD (`resolve=false`
  vs expected `resolve=true`), untouched and pre-existing.
* The shared tree did not compile at the time of writing (W39-A `exec.rs` WIP).
  All evidence was collected in the clean worktree.

---

# W43-A session — resources/layout inflation (`AssetManager`/`TypedArray`)

**Status: the `res_natives` gate is green and real `LayoutInflater` now
inflates a compiled fixture layout under the VM. A full `*`-fixture pass is
blocked by two non-res subsystems (`EditText`'s ICU break-iterator data and
`Button`'s framework drawable XML).** All work is on a clean `git worktree` of
HEAD `66e4232`; the diff was copied back only after `git apply --check` passed.

## What changed

| file | change |
|---|---|
| `crates/acos-framework-natives/src/res/theme.rs` | `TypedArray` output was rewritten to the **real native layout**. `mIndices[0]` is now the count of found attributes and `mIndices[1..=count]` are their **request/styleable indices** (what `TypedArray.getIndex` returns), replacing the old raw per-request XML indices. `nativeApplyStyle` now resolves `@resid` references and `?attr` attributes to the concrete resource (via `build_typed_value`, so the asset cookie/pooled string are right) — without this `getDimension`/`getColor` throw `type=0x1`. `STYLE_RESOURCE_ID` is set to the referenced target so `TypedArray.getResourceId` returns the resource, not the attribute id. |
| `crates/acos-framework-natives/src/res/manager.rs` | `theme_get_attribute_value` resolves `TYPE_REFERENCE` when `resolve=true`; `build_typed_value` sets `resource_id = final_id` / `source_resource_id = requested_id`; new config-aware `resolve_references_with` helper. |
| `crates/acos-vm/src/natives/fwk_res.rs` | `nativeApplyStyle`'s `outValues`/`outIndices` are `TypedArray.mDataAddress`/`mIndicesAddress` (`VMRuntime.addressOf` synthetic addresses), not heap handles: decode them with `array_handle_from_address` before reading/writing. Without this every `applyStyle` output was silently dropped. |
| `crates/acos-vm/src/natives/native_mem.rs` | new `array_handle_from_address(addr)` (the inverse of `array_address`). |
| `crates/acos-vm/tests/res_natives.rs`, `tests/java/ResMain.java` | corrected to real ART semantics + stronger theme checks. `Theme.DeviceDefault` does **not** define `android:attr/textColor`, so `resolveAttribute(textColor, tv, true)` correctly reports `false` (verified on the physical API-35 device and from the API-34 `framework-res` bag chain). Added `colorBackground` -> `true type=29` and `textColorPrimary` -> `true type=3`. |
| `crates/acos-framework-natives/tests/xmlblock_diff.rs` | `retrieve_attributes_through_manager` updated to the `[count, styleable_index...]` layout. |
| `crates/acos-vm/tests/inflate_layout.rs` (**new**) + `tests/java/InflateMain.java` + `tests/java/inflate_stubs/` | the LayoutInflater acceptance test (below). |

## The inflation test

`crates/acos-vm/tests/inflate_layout.rs` compiles `InflateMain.java` with the
host `javac`/`d8` and runs it on the real core library + framework DEX. It
builds a real `Resources`/`Theme` over a fixture APK, a `Context` backed by
those resources, and inflates the APK's compiled `res/layout/main.xml` through
the real `com.android.internal.policy.PhoneLayoutInflater`. It asserts the
resulting view tree's class names, ids, text and dimension units.

The test uses `fixtures/build/apps/canvas-shapes.apk` (a real fixture APK whose
layout has a `LinearLayout`, a `TextView` with `android:padding="8dp"` and
`android:text="@string/status_idle"`, and a custom `ShapeView`). Its output:

```
view=android.widget.LinearLayout id=0 pad=0
  view=android.widget.TextView id=canvas_status pad=21
    text=tap the canvas
  view=dev.acos.fixtures.canvasshapes.ShapeView id=shape
```

`pad=21` is the real `8dp` dimension at density 2.625; the id text is resolved
through `TypedArray.getResourceId` -> `AssetManager.nativeGetResourceEntryName`;
`android:text` is the resolved `@string` reference.

`android.test.mock.MockContext` (real framework test double, loaded from
`.cache/boot/android-34/dex/android.test.mock/classes.dex`) supplies the
abstract `Context`; the test overrides the resource/theme/layout entry points and
returns real `MockPackageManager`. A minimal `BinderTransport` makes
`ServiceManager` answer `null` for absent services instead of NPE-ing. The ICU
break-iterator roots are seeded like `acos-runtime`'s
`seed_device_environment`.

## Honest blockers (not faked)

* **`hello-views`/`multi-activity`/`form-validation` (`EditText`)**: the
  `Editor` loads ICU break-iterator bundles (`brkitr/en_US.res`) through the
  `/apex/com.android.i18n` VFS root, which the VM does not map (the same ICU
  substrate blocker as W19h). The bundle lookup raises a real
  `MissingResourceException`.
* **`counter`/`todo-list` (`Button`)**: `Widget.Button`'s background is the
  framework `btn_default_material.xml` ripple/selector, whose nested drawable
  references (`@drawable/btn_default_mtrl_shape` -> a `TYPE_REFERENCE` with no
  concrete path) are not resolvable by the current `Resources.loadDrawable`
  path. It raises a real `Resources$NotFoundException`.
* Both are out of the `android.content.res` native surface (fonts/ICU and
  graphics/drawable); nothing is stubbed to hide them.

## Evidence

```sh
# clean worktree of HEAD 66e4232, CARGO_TARGET_DIR=.cache/target-w43a,
# CARGO_PROFILE_DEV_DEBUG=0, via tools/guarded.sh
cargo test -p acos-vm --test res_natives --test inflate_layout   # 2 + 1 pass
cargo test -p acos-framework-natives                             # all pass
cargo test -p acos-vm -- --skip thousand_events                  # 28 bins ok
tools/diff-dexprogs.sh --corelib real                            # 27 pass / 0 fail / 0 unsupported
```

`diff-dexprogs --corelib real` is byte-identical to ART (27/27), unchanged.

## Caveats

* The `TypedArray` index-layout change is a **behavioural correction**; the
  engine's own `xmlblock_diff` test had encoded the old raw-index layout and was
  updated. No `res` consumer relied on the old layout.
* `results/m2/M2_LOG.md` is the only artifact touched; no benchmark or
  `results/vm/*.json` file is claimed by this session.

# W51-A session — content-provider holder binder, Settings reads, Path.approximate

Base: clean worktree of HEAD `ad2ba9b` (includes W52-A `runtime/src/ams_java.rs`,
W43-A resources, W49-A sqlite) at `~/Projects/acos/.cache/wt-w51a`, with
`CARGO_TARGET_DIR=.cache/target-w51a`, `CARGO_PROFILE_DEV_DEBUG=0`, via
`tools/guarded.sh`. W52-A had already materialised `ProviderInfo` /
`ContentProviderHolder` through the Java bridge, so the W40-A "Bundle length is
not aligned by 4" parser failure was gone; this session fixed the *semantic*
holes left in that path and drove the first real `Activity.onCreate`.

## What changed

### Fix 1 — `getContentProvider` authority is arg 2, not arg 1

W52-A's `ams_java` read `args.get(1)` as the authority. The AIDL signature is
`getContentProvider(IApplicationThread caller, String callingPackage, String
name, int userId, boolean stable)`, so arg 1 is the **calling package** and arg
2 is the requested authority. `Settings` therefore asked for
`dev.acos.fixtures.counter`, got `null`, and threw a `NullPointerException` in
`Settings$NameValueCache.getStringForUser`. Now reads arg 2 (the same correction
W40-A made in `acos-services::atms`).

### Fix 2 — the `settings` provider binder is real

`AmsState::get_content_provider("settings")` returns a hardcoded record with
`binder: 0` (the provider is not published by an app). The reply therefore
carried `provider = null`, and `Settings$ContentProviderHolder.getProvider`
returned null. `registry::install` now records the binder it registered for the
`android.content.IContentProvider` service
(`provider::set_settings_provider_binder`), and `ams_java` substitutes it when
the record has no binder. The reply now carries the real binder id.

### Fix 3 — a real `ProviderInfo` and a real `IContentProvider`

`ProviderInfo.writeToParcel` splices `ComponentInfo` -> `PackageItemInfo` and
dereferences `applicationInfo` unconditionally, so the W52-A minimal object
(4 fields) could not be marshalled. The bridge now sets the full API-34 field
set and a real `ApplicationInfo` (`pms_java::application_info`). It also wraps
the provider binder in the framework's own
`android.content.ContentProviderProxy` instead of a bare `BinderProxy`, so
`ContentProviderHolder.writeToParcel`'s `provider.asBinder()` resolves and the
app receives a usable `IContentProvider`.

### Fix 4 — no transport re-entrancy while writing the reply

Materialising the holder runs inside `ServiceTransport::transact` (the
`TRANSPORT` `RefCell` is already borrowed). `writeStrongBinder` re-entered
`with_transport` and panicked with `RefCell already borrowed`.
`natives::binder::binder_id_of` now reads a `BinderProxy`'s `mNativeData`
directly (exactly what the transport did) before consulting the transport.

### Fix 5 — `Path.approximate` returns `(fraction, x, y)` triples

`DecorView` loads the window interpolator through `PathInterpolator.initPath`,
which requires `Path.approximate` to return interleaved
`(fraction, x, y)` triples whose first point is `(0,0)` and last is `(1,1)`.
`gfx_path::path_approximate` returned `(x, y)` pairs only, so every launch threw
`IllegalArgumentException: The Path must start at (0,0) and end at (1,1)`.
It now computes the cumulative distance fraction and emits triples.

## Evidence

`acos launch fixtures/build/apps/counter.apk` (release, W51-A tree):

```
reached client_transaction (classes=1918, wall=761ms)
  [ok] apk_parsed  [ok] process_booted  [ok] bind_application
  [ok] application_on_create  [ok] client_transaction
  [blocked] activity_on_create
      Resources$NotFoundException: File res/color/system_bar_background_semi_transparent.xml
      from ComplexColor resource ID #0x1060309
      caused by UnsupportedOperationException: Can't convert value at index 0 to color: type=0x1
      at android.content.res.Resources.getColor(1083)
      at com.android.internal.policy.DecorView.<init>(309)
      ...
      at dev.acos.fixtures.counter.MainActivity.onCreate(41)
  [not-reached] on_resume  [not-reached] first_frame
```

The `Settings.Global` reads now complete: the transport log shows two
`android.content.IContentProvider` transactions (code 21 = `call`) after the
`getContentProvider` reply, and execution reaches `MainActivity.onCreate`
itself (`setContentView`). Before this session it stopped inside
`Settings$NameValueCache.getStringForUser`.

## Tests

* `crates/acos-runtime/tests/ams_java.rs` — 5 pass:
  `get_content_provider_reads_the_authority_and_carries_the_provider` (regression
  for fixes 1-3: authority is arg 2 and `holder.provider` is non-null) and
  `get_content_provider_does_not_treat_the_calling_package_as_the_authority`.
* `crates/acos-vm/src/natives/gfx_path.rs` —
  `approximation_is_fraction_x_y_triples_from_origin_to_unit` passes (fix 5).
* `crates/acos-runtime/tests/m2.rs` — the two non-ignored tests still pass; the
  gate remains `#[ignore]`d with the updated (real) blocker reason.

## Blocker table (furthest stage reached)

| Stage | Status | Blocker |
|---|---|---|
| bind_application | ok | — |
| application_on_create | ok | — |
| client_transaction | ok | — |
| activity_on_create | blocked | `Resources.getColor(@android:color/system_bar_background_semi_transparent)` (ComplexColor XML) raises `UnsupportedOperationException: Can't convert value at index 0 to color: type=0x1`; owned by W43-A resources (`Resources`/`ComplexColor`/`ColorStateList` XML inflation). |
| on_resume | not reached | after the color XML; the next blocker will be observable once it lands. |
| first_frame | not reached | — |

## Handoff

* **Next blocker is W43-A's.** `DecorView.<init>` reads framework
  `@android:color/system_bar_background_semi_transparent`; `Resources.getColor`
  -> `ComplexColor` cannot convert the `<color>` XML value (`type=0x1`, i.e.
  `TYPE_REFERENCE`) at index 0. That is the `android.content.res` XML/typed-value
  path, not the AMS/content-provider path.
* **`publishContentProviders` is still unimplemented.** W52-A explicitly left
  it for W51-A (`ams_java::transact` returns a structured error). It is not on
  the `counter` launch path (that app declares no providers), but an app with
  its own provider needs the inbound `List<ContentProviderHolder>` decoded with
  each holder's own `CREATOR` before it can be recorded. This is the remaining
  provider-parcel item from the W51-A brief.
* **`IContentProvider.query/insert/update/delete`** remain structured
  `UNSUPPORTED_ANDROID_API` (W52-A): they need a real `CursorWindow` /
  `BulkCursorDescriptor` marshaller. `call` is implemented and now exercised by
  `Settings`; `getType` returns null.
* **Coordination:** this session edited W47-A's
  `crates/acos-vm/src/natives/gfx_path.rs` (fix 5) and W48-A-adjacent
  `crates/acos-vm/src/natives/binder.rs` (fix 4). Both are small and isolated;
  they should be merged before those workstreams change the same functions.

## Caveats

* `cargo check --workspace --all-targets` and `tools/diff-dexprogs.sh --corelib
  real` were run in the W51-A worktree (results in the session report).
* The `m2` gate is not un-ignored: `onResume` is not reached yet.

# W60-A session — `nativeRetrieveAttributes` chases `@resid` (framework ComplexColor XML)

Base: clean git worktree of HEAD `ad9c978` at `.cache/wt-w60a`, with
`CARGO_TARGET_DIR=.cache/target-w60a`, `CARGO_PROFILE_DEV_DEBUG=0`, via
`tools/guarded.sh`. The W51-A blocker was root-caused to the **wrong native**:
`Resources.obtainAttributes` with a null theme (exactly what
`ColorStateList.inflate` uses) calls `AssetManager.retrieveAttributes`
(`nativeRetrieveAttributes`), **not** `applyStyle`, and that native returned the
raw `TYPE_REFERENCE` instead of the referenced resource.

## What changed

### Fix 1 — `nativeRetrieveAttributes` resolves `@resid` like `applyStyle`

`crates/acos-framework-natives/src/res/theme.rs::retrieve_attributes` copied the
XML `Res_value` verbatim, so `android:color="@0x01060035"` came back as
`type=0x1` (`TYPE_REFERENCE`). API-34 `ColorStateList.inflate` calls
`TypedArray.getColor` on that slot, which only accepts a concrete int type and
throws:

```
java.lang.UnsupportedOperationException: Can't convert value at index 0 to color: type=0x1, theme=null
    at android.content.res.TypedArray.getColor(535)
    at android.content.res.ColorStateList.inflate(366)
    at android.content.res.ColorStateList.createFromXmlInner(296)
    at android.content.res.ComplexColor.createFromXml(...)
    at android.content.res.Resources.getColor(1083)
    at com.android.internal.policy.DecorView.<init>(309)
```

The fix mirrors `fill_outputs` (the `applyStyle` path): when a retrieved XML
attribute is `TYPE_REFERENCE`/`TYPE_DYNAMIC_REFERENCE`, chase it with
`AssetManager::resolve_references(.., true)` and materialise the concrete
`TypedValueOut` with `build_typed_value` (which also sets the right asset cookie
so a resolved string reads the ARSC pool instead of the XML pool). Non-reference
values keep the exact previous raw layout.

`Resources.obtainAttributes` (theme == null) is also used by every
`<declare-styleable>` read from a raw XML element, so this was a general hole in
the retrieve path, not a color-specific one.

## Evidence

Before (HEAD `ad9c978`, same `acos launch`):

```
[    blocked] activity_on_create
    UNSUPPORTED_JAVA_API: ... Resources$NotFoundException: File res/color/system_bar_background_semi_transparent.xml
    ... caused by UnsupportedOperationException: Can't convert value at index 0 to color: type=0x1
    at android.content.res.TypedArray.getColor(535)
    at android.content.res.ColorStateList.inflate(366)
```

After (W60-A tree, `acos launch fixtures/build/apps/counter.apk`):

```
launch: reached client_transaction (classes=1929, wall=4999ms)
  [ok] apk_parsed  [ok] process_booted  [ok] bind_application
  [ok] application_on_create  [ok] client_transaction
  [blocked] activity_on_create
      JAVA_EXCEPTION: ... IllegalArgumentException: bad request for
      android.app.IActivityClientController::setTaskDescription: arg 1
      (TypedObject(android.app.ActivityManager$TaskDescription, nullable=true)):
      bad string: String16 is not NUL terminated
  [not-reached] on_resume  [not-reached] first_frame
```

So `MainActivity.onCreate -> setContentView -> PhoneWindow.installDecor ->
DecorView.<init>` (the ComplexColor read) now completes; execution proceeds into
`PhoneWindow.generateLayout`.

## Tests

* `crates/acos-framework-natives/tests/xmlblock_diff.rs` —
  `retrieve_attributes_resolves_framework_color_reference` (new): opens
  `framework-res.apk:res/color/system_bar_background_semi_transparent.xml`,
  retrieves `android:color`/`android:alpha`, and asserts the reference
  `@0x01060035` resolves to `type=0x1d` (RGB8) `data=0xff161c20` while
  `STYLE_RESOURCE_ID` stays `0x01060035`. `retrieve_attributes_through_manager`
  still passes (raw XML values are unchanged).
* `cargo check --workspace --all-targets` and
  `tools/diff-dexprogs.sh --corelib real` were run in the W60-A worktree.

## Blocker table (furthest stage reached)

| Stage | Status | Blocker |
|---|---|---|
| bind_application | ok | — |
| application_on_create | ok | — |
| client_transaction | ok | — |
| activity_on_create | blocked | `IActivityClientController.setTaskDescription` can't unmarshall `ActivityManager$TaskDescription`; see below. |
| on_resume | not reached | after `generateLayout`. |
| first_frame | not reached | — |

### Next blocker root cause (API-34 `TaskDescription.writeToParcel`)

`DecorView`'s status-bar colour update path is
`Activity$1.updateStatusBarColor` -> `Activity.setTaskDescription` ->
`ActivityClient.setTaskDescription` -> `IActivityClientController.setTaskDescription`.
The real API-34 `ActivityManager$TaskDescription.writeToParcel` (disassembled
from `dex/framework/classes.dex`) is **conditional**, which the current curated
6-field spec cannot express:

```
writeInt(mLabel != null ? 1 : 0); if present writeString(mLabel)
if (mIcon == null || (inMemoryIcon != null && inMemoryIcon.isRecycled()))
    writeInt(0)
else { writeInt(1); mIcon.writeToParcel(dest, 0); }   // raw Icon, no marker
writeInt(mColorPrimary); writeInt(mColorBackground)
writeInt(mStatusBarColor); writeInt(mNavigationBarColor)
writeBoolean(mEnsureStatusBarContrastWhenTransparent)
writeBoolean(mEnsureNavigationBarContrastWhenTransparent)
writeInt(mResizeMode); writeInt(mMinWidth); writeInt(mMinHeight)
writeInt(mIconFilename != null ? 1 : 0); if present writeString(mIconFilename)
writeInt(mColorBackgroundFloating)
```

`crates/acos-services/src/parcelable.rs` declares
`iconRes, colorPrimary, colorBackground, colorSecondary, label, iconFilename`
(an old/lossy layout), so the first `Str16` read lands on a length word and the
parcel is rejected. Fixing this needs a conditional-aware parcelable (or a
custom `TaskDescription` reader) plus an `Icon` payload model; the current
linear `FieldOp` list cannot represent the three presence branches. It is a
`acos-services` parcelable/codec task, not a `res` task.

## Handoff

* The `res` copy of the ComplexColor blocker is closed. The next blocker is the
  `acos-services` `TaskDescription` codec above.
* `crates/acos-vm/src/natives/fwk_res.rs` (`nativeRetrieveAttributes`) needed no
  change: the entire fix is in `acos-framework-natives/src/res/theme.rs`.

## Caveats

* `m2` remains `#[ignore]`d with the updated (real) blocker reason; `onResume`
  is still not reached.
* `cargo check --workspace --all-targets` and `diff-dexprogs` were run in the
  W60-A worktree and are reported in the session report.

# W65-A session — conditional parcelable codec + `@null` theme reference

Base: clean git worktree of HEAD `00a429b` (W47-A render landed; the brief's
`fcfa346` base moved) at `.cache/wt-w65a`, with
`CARGO_TARGET_DIR=.cache/target-w65a`, `CARGO_PROFILE_DEV_DEBUG=0`, via
`tools/guarded.sh`. Two blockers were closed, both on the `counter` launch path.

## What changed

### Fix 1 — conditional-aware `FieldOp` for `ActivityManager$TaskDescription`

`crates/acos-services/src/parcelable.rs` declared a stale **linear** six-field
`TaskDescription` spec (`iconRes, colorPrimary, colorBackground, colorSecondary,
label, iconFilename`). The real API-34 `writeToParcel` (disassembled from
`classes.dex`, see `research/service-layer` scanner) is **conditional**:

```
int labelPresent;       if (labelPresent)       writeString(label)
int iconPresent;        if (iconPresent)        Icon.writeToParcel
int colorPrimary; int colorBackground;
int statusBarColor; int navigationBarColor;
boolean ensureStatusBarContrastWhenTransparent;
boolean ensureNavigationBarContrastWhenTransparent;
int resizeMode; int minWidth; int minHeight;
int iconFilenamePresent; if (iconFilenamePresent) writeString(iconFilename)
int colorBackgroundFloating
```

The old spec read the icon-presence marker (`0`) as a `String16` length, so
`IActivityClientController.setTaskDescription` failed with
`bad string: String16 is not NUL terminated` inside
`DecorView.updateStatusBarColor -> Activity.setTaskDescription`.

The linear `FieldOp` list could not express the three presence branches, so it
gained a conditional form:

* `FieldOp::Cond(&'static [FieldOp])` — a `writeInt` presence marker, then the
  nested fields when the marker is non-zero; reads/writes `Value::Cond
  { present, values }`.
* `FieldOp::Unsupported(&'static str)` — a payload ACOS cannot model; raises a
  structured `UnsupportedParcelable` rather than silently desynchronising the
  parcel. The icon branch uses it (an `Icon` is not on the V1 launch path).

The `TaskDescription` spec is now the real 13-slot conditional layout. This
fixes the *expressiveness* class of problem, not just the one class.

### Fix 2 — `@null` theme values must be `TYPE_NULL`, not `TYPE_REFERENCE` to 0

After fix 1, the launch reached `DecorView.onResourcesLoaded`, which inflates
`android:layout/screen_toolbar`. `ActionBarOverlayLayout.init` calls
`context.getTheme().obtainStyledAttributes(ATTRS)` and then
`TypedArray.getDrawable(1)` for `android:attr/windowContentOverlay`
(`0x01010059`, `@null` in the device themes). `nativeApplyStyle` left the raw
value as a `TYPE_REFERENCE` with target id `0`, so `Resources.loadDrawable`
called `getResourceName(0)` and threw
`Resources$NotFoundException: Unable to find resource ID #0x0`.

`crates/acos-framework-natives/src/res/theme.rs` now materialises a reference
whose target id is `0` (`@null`/`?null`) as `TYPE_NULL` in both the
`applyStyle` (`fill_outputs`) and `retrieveAttributes` paths via
`null_or_raw_typed_value`. A `TYPE_NULL` makes `Resources.loadDrawable` return
`null`, exactly as the platform does.

## Evidence

Before (HEAD `00a429b`):

```
[    blocked] activity_on_create
    JAVA_EXCEPTION: ... IActivityClientController::setTaskDescription: arg 1
    (TypedObject(android.app.ActivityManager$TaskDescription, nullable=true)):
    bad string: String16 is not NUL terminated
```

After fix 1 (W65-A tree) — the TaskDescription error is gone; the next blocker
is the `@null` drawable:

```
[    blocked] activity_on_create
    JAVA_EXCEPTION: ... InflateException: Binary XML file line #21 in
    android:layout/screen_toolbar: Error inflating class
    com.android.internal.widget.ActionBarOverlayLayout
    caused by: Resources$NotFoundException: Unable to find resource ID #0x0
      at android.content.res.ResourcesImpl.getResourceName(273)
      at android.content.res.TypedArray.getDrawable(991)
      at com.android.internal.widget.ActionBarOverlayLayout.init(182)
```

After fix 2 (`acos launch fixtures/build/apps/counter.apk`, W65-A tree):

```
launch: reached client_transaction (classes=1989, wall=5052ms)
  [ok] apk_parsed  [ok] process_booted  [ok] bind_application
  [ok] application_on_create  [ok] client_transaction
  [blocked] activity_on_create
      UNSUPPORTED_JAVA_API: Landroid/graphics/drawable/VectorDrawable;->nCreateGroup()J
      is not implemented
      at android.graphics.drawable.VectorDrawable$VGroup.<init>
      at ...VectorDrawableState.<init>
      at android.graphics.drawable.VectorDrawable.<init>
      at android.graphics.drawable.DrawableInflater.inflateFromTag
      at android.graphics.drawable.Drawable.createFromXmlForDensity
      at android.content.res.ResourcesImpl.loadXmlDrawable
      at android.content.res.Resources.loadDrawable
      at android.content.res.TypedArray.getDrawable
      at com.android.internal.widget.ToolbarWidgetWrapper.<init>
      at com.android.internal.widget.ActionBarOverlayLayout...
```

So `MainActivity.onCreate -> setContentView -> PhoneWindow.installDecor ->
DecorView.onResourcesLoaded -> screen_toolbar -> ToolbarWidgetWrapper` now
completes all the resource/theme resolution; execution stops at the
VectorDrawable native tree.

## Tests

* `crates/acos-services/tests/parcelable.rs` —
  `task_description_reads_the_api34_conditional_layout` (new): decodes the exact
  launch-path byte layout (label/icon absent) and a label + iconFilename-present
  layout, asserting full consumption and field values.
  `every_spec_round_trips_through_a_parcel` still passes.
* `crates/acos-services/tests/replay.rs` — the
  `IActivityClientController.setTaskDescription` row now uses the real 13-slot
  conditional value; `replay_all_measured_outgoing_calls` passes.
* `crates/acos-framework-natives/tests/theme_resolution.rs` —
  `null_theme_reference_becomes_type_null` (new): applies
  `Theme.DeviceDefault.Light.DarkActionBar` and asserts
  `windowContentOverlay` resolves to `TYPE_NULL`/`data=0`/`resource_id=0`.
* `crates/acos-framework-natives/tests/xmlblock_diff.rs` — 4 pass (the W60-A
  `@resid` regression still holds).
* `crates/acos-runtime/tests/m2.rs` — 2 pass, 1 ignored (the gate) with the
  updated real blocker reason.
* `cargo check --workspace --all-targets` — clean.
* `tools/diff-dexprogs.sh --corelib real` — 27 pass / 0 fail / 0 unsupported.

## Blocker table (furthest stage reached)

| Stage | Status | Blocker |
|---|---|---|
| bind_application | ok | — |
| application_on_create | ok | — |
| client_transaction | ok | — |
| activity_on_create | blocked | `VectorDrawable.nCreateGroup()J` unimplemented; `DecorView.onResourcesLoaded` -> `screen_toolbar` -> `ToolbarWidgetWrapper` inflates a VectorDrawable. W47-A render/gfx owns the VectorDrawable native tree; next chain file `fwk_m2_chain5.rs`. |
| on_resume | not reached | after the VectorDrawable tree is constructed. |
| first_frame | not reached | — |

## Parcelable spec survey (ranked suspects)

`cargo test -p acos-services --test parcelable -- --nocapture` diffs every spec
against the real `writeToParcel` bytecode (`matched/spec`, `real` op count).
Ranked by "used on the launch path and clearly stale":

1. `android.app.ActivityManager$TaskDescription` — was 2/6, real=18. **Fixed**
   (conditional layout).
2. `android.content.res.Configuration` — Curated 0/20, real=27. The
   `IWindowSession.addToDisplayAsUser` reply carries a `Configuration`; the
   curated spec cannot express the API-34 field order. **Next parcelable fix.**
3. `android.window.SizeConfigurationBuckets` — Curated 0/1, real=5.
   `IActivityClientController.reportSizeConfigurations` argument.
4. `android.app.servertransaction.ClientTransaction` — Curated 0/1, real=5
   (known V1 gap; the framework's own `writeToParcel`).
5. `android.content.pm.ProviderInfo` — Prefix 7/14, real=27 (the wire copy is
   Java-bridged in `ams_java`; the table is still stale).
6. `android.view.DisplayInfo` — Prefix 21/21, real=50 (replies Java-bridged in
   `display_java`).
7. `android.app.ContentProviderHolder` — Curated 0/3, real=5 (Java-bridged).
8. `android.view.InputDevice` — Curated 3/15, real=26.
9. `android.app.ActivityManager$RunningAppProcessInfo` — Curated 0/2, real=15.
10. `android.content.res.CompatibilityInfo` — Curated 1/1, real=4.

The general direction is the one the project invariant prefers: consume the
framework class through its real `CREATOR` (as `pms_java`/`ams_java`/
`display_java` already do for replies) or express the conditional wire form.
The `Cond`/`Unsupported` extension makes the services codec able to express the
presence branches; it does not yet replace the stale `Configuration` spec.

## Handoff

* The next launch blocker is **W47-A's**: `VectorDrawable` native peers
  (`nCreateGroup`, `nCreateFullPath`, `nAddChild`, `nSetPathString`,
  `nUpdatePath`, `nDraw`, ...) reached through `ToolbarWidgetWrapper`. A
  faithful implementation belongs in `crates/acos-vm/src/natives/fwk_m2_chain5.rs`
  (the chain file named for this session) or in `gfx_*` under W47-A.
* `android.content.res.Configuration` is the next `acos-services` parcelable to
  fix if the first-frame `addToDisplayAsUser` reply is reached before the
  VectorDrawable tree lands.
* `crates/acos-runtime/tests/m2.rs` remains `#[ignore]`d: `onResume` is still
  not reached.

## Caveats

* This session edited `crates/acos-services/src/parcelable.rs`,
  `crates/acos-framework-natives/src/res/theme.rs`, the corresponding tests, and
  the `m2.rs` ignore reason. The W60-A/earlier work in the main checkout was not
  touched (an initial edit landed in the main checkout by mistake and was
  reverted; the changes live in the W65-A worktree only).

# W107-A session — `TypedArray` stale-slot leak (the real NPE) + launch progress

Worktree: clean HEAD `6409190`, `CARGO_TARGET_DIR=.cache/target-w107a`,
`CARGO_PROFILE_DEV_DEBUG=0`. No commits.

## What the blocker actually was (W68-A's ThreadLocal diagnosis was wrong)

The launch died in `ResourcesImpl.loadDrawableForCookie` with
`Resources$NotFoundException: ... ic_ab_back_material`, whose deepest cause was
a frameless `NullPointerException` at `loadDrawableForCookie(909)`. W68-A read
line 909 as the `finally { stack.pop(); }` and concluded `mLookupStack.get()`
returned null.

Temporary instrumentation (since removed) of `handle_exception_inner` and
`Vm::capture_stack` showed:

* at handler entry `v14` (the `LookupStack`) was **non-null** — the real
  ThreadLocal path is fine; line 909 is a downstream `finally` that NPE'd while
  wrapping the real exception;
* the real root-cause chain (innermost first) is
  `ResourcesImpl.loadComplexColor(1057)` -> `TypedArray.getComplexColor` ->
  `VectorDrawable$VFullPath.updateStateFromTypedArray(2048)`;
* `loadComplexColor(1057)` is `value.string.toString()`, and that `TypedValue`
  had `type=5` (TYPE_DIMENSION), `data=0x1801` (24dp),
  `resourceId=0x01010159` (`android:width`), `string=null` — i.e. the
  `VectorDrawablePath_fillColor` slot held the parent `<vector>`'s
  `android:width` value.

A minimal dexprog (`ThreadLocal.withInitial(...)`, corpus-style) passes on
`--corelib real` in both `ACOS_EXEC` modes and under `ACOS_GC_STRESS=1`,
confirming the VM's ThreadLocal/ThreadLocalMap/lambda path is correct. Source
and DEX are kept under `results/m2/w107a-threadlocal-repro/` (not added to the
globbed `fixtures/dexprogs/` corpus so the 27/27 ART differential is unchanged).

## Root cause and fix

`android.content.res.TypedArray` is pooled and its `int[] mData` is reused.
`nativeRetrieveAttributes` (used by `Resources.obtainAttributes` when the theme
is null, e.g. `Drawable.obtainAttributes(... theme == null)` for the `<path>`
tag) wrote only the attributes found on the current tag and left the other
`mData` slots untouched. `TypedArray.getValueAt` treats any non-zero
`STYLE_TYPE` as "present", so the stale `android:width` slot was read back as
`fillColor`. `applyStyle` already cleared missing slots
(`fill_outputs(zero_missing = true)`); `retrieveAttributes` did not.

Fix (`crates/acos-framework-natives/src/res/theme.rs`): zero the first
`n * STYLE_NUM_ENTRIES` `out_values` slots at the start of
`AssetManager::retrieve_attributes`.

## Evidence

| Item | Result |
|---|---|
| Regression `retrieve_attributes_clears_stale_slots_from_a_recycled_typed_array` | pass: pre-fills every slot with the width `TypedValue`, asserts the absent `height` slot is 0 and `fillColor` resolves to `#ffffffff` |
| `cargo test -p acos-framework-natives` | all pass (xmlblock_diff 5/5) |
| `cargo test -p acos-vm --lib` | 120/120 pass |
| `cargo test -p acos-runtime --test m2` | 2 passed / 1 ignored |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | clean (also fixed 6 pre-existing lints in `gfx_vector.rs`) |
| `cargo check --workspace --all-targets` | clean |
| `diff-dexprogs.sh --corelib real` | 27/27 pass |
| `diff-dexprogs.sh --corelib real --gc-stress` | 27/27 pass |
| `acos launch fixtures/build/apps/counter.apk` | NPE gone; reaches `MainActivity.onCreate` -> `setContentView` -> `PhoneWindow.installDecor` -> `DecorView.onResourcesLoaded`; `ic_ab_back_material` now inflates |

## New blocker (still `activity_on_create`)

`VectorDrawable.applyTheme(636)` -> `ResourcesImpl.loadDrawable(696)` ->
`loadComplexColorForCookie`: `java.lang.UnsupportedOperationException:
Can't convert to ComplexColor: type=0x1`. The vector's
`android:tint="?0x01010429"` (`attr/colorControlNormal` =
`?attr/textColorSecondary`) is resolved and then loaded as an XML complex color,
but `loadComplexColorForCookie` sees `value.string == null` (or the loaded tag
is neither `selector` nor `gradient`). Next session: instrument
`loadComplexColorForCookie` / `nativeResolveAttrs` to see which `TypedValue`
reaches it; `Theme.resolveAttributes` (`am_resolve_attrs`) does not chase a
resolved `TYPE_REFERENCE` to its concrete color the way `fill_outputs` does.

## Caveats

* The `m2.rs` gate stays `#[ignore]`d; its reason now names the new blocker.
* No `VectorDrawable`/ThreadLocal VM bug was found: the W68-A ThreadLocal
  hypothesis was a mis-read of the stack.
* The `gfx_vector.rs` clippy fixes are mechanical
  (`map(|x| x.clone())` -> `cloned()`, one
  `#[allow(clippy::too_many_arguments)]`) and behaviour-neutral.

---

# W110-A session — `nativeResolveAttrs` reference chasing + static `nativeAsset*` ABI

Worktree: clean HEAD `d18f066`, `CARGO_TARGET_DIR=.cache/target-w110a`,
`CARGO_PROFILE_DEV_DEBUG=0`. No commits.

## Bottom line

Two launch-chain blockers are fixed and regression-tested. `counter.apk` now
inflates `android:drawable/ic_ab_back_material` and reaches the default
launcher-icon decode inside `PhoneWindow.installDecor`; the single remaining
blocker is `android.graphics.ImageDecoder.nCreate` (image/render, W104-A).

## Blocker 1 — `nativeResolveAttrs` returned a raw `@resid`

`VectorDrawable.applyTheme(636)` -> `ResourcesImpl.loadDrawable(696)` ->
`loadComplexColorForCookie` raised
`UnsupportedOperationException: Can't convert to ComplexColor: type=0x1`.

`AssetManager::resolve_attrs` selected the theme value through `Theme::get`
(which follows `?attr` chains) and then copied the resulting `ResValue`
verbatim. For `?attr/colorControlNormal` the chain ends at a `@color`
`TYPE_REFERENCE`, which the framework's `TypedArray.getColorStateList` handed
to `Resources.loadColorStateList` -> `loadComplexColor`, where a
`TYPE_REFERENCE` is not a valid ComplexColor.

Fix (`crates/acos-framework-natives/src/res/theme.rs`): mirror
`AssetManager2::ResolveAttributeReference` + `ResolveReference`. Follow any
remaining `?attr` chain through the theme, then chase a `@resid` through the
resource table with `resolve_references` + `build_typed_value`, so the output
carries the concrete value, the right asset cookie and pooled string.

Regression:
`xmlblock_diff::resolve_attrs_chases_a_theme_reference_to_a_concrete_color`
resolves `Theme.DeviceDefault.Light.DarkActionBar` +
`?attr/colorControlNormal` and asserts the result is not `TYPE_REFERENCE` /
`TYPE_ATTRIBUTE`. It resolves to
`res/color/text_color_secondary_device_default_light.xml` (cookie 1).

## Blocker 2 — static `nativeAsset*` natives read a phantom second argument

With blocker 1 fixed, the launch aborted in a Rust panic:
`corelib/mod.rs:76 index out of bounds: the len is 2 but the index is 2` at
`am_asset_get_length`, called from
`ApplicationPackageManager.getDefaultActivityIcon`.

The real `android.content.res.AssetManager` declares the static asset natives
as `(J)...` and passes only the `AssetInputStream` pointer, but the engine's
asset ids are per-manager. Every `am_asset_*` implementation read
`mgr = arg0, handle = arg1`, so the first real asset stream
(`AssetInputStream.<init>` -> `nativeAssetGetLength`) read past the register
frame.

Fix (`crates/acos-vm/src/natives/fwk_res.rs`): encode the owning manager in the
high 32 bits of the Java-visible pointer (`pack_asset(mgr, handle)`) at
`nativeOpenAsset`/`nativeOpenNonAsset`, and `unpack_asset` it in
`nativeAssetDestroy`/`ReadChar`/`Read`/`Seek`/`GetLength`/
`GetRemainingLength`, whose argument indices now match the real signatures
(`read(ptr, b, off, len)`, `seek(ptr, offset, whence)`).

Regression: `ResMain.java` opens `res/layout/main.xml` through
`AssetManager.open`, then `available()`, `read(byte[4])` (AXML magic `0x03`)
and `close()`. `res_natives` asserts `asset.ok=true` in both normal and
`ACOS_GC_STRESS=1` runs.

## Current blocker (still `activity_on_create`)

```
android.app.ApplicationPackageManager.getDefaultActivityIcon()
  <- com.android.internal.policy.PhoneWindow.installDecor()
  <- PhoneWindow.setContentView <- Activity.setContentView
  <- MainActivity.onCreate
Resources.getDrawable(sym_def_app_icon)
  -> AdaptiveIconDrawable.inflateLayers -> BitmapDrawable.inflate
  -> ImageDecoder.decodeBitmap -> ImageDecoder$InputStreamSource.createImageDecoder
  -> android.graphics.ImageDecoder.nCreate(InputStream, byte[], boolean, Source)
ERROR: vm: Landroid/graphics/ImageDecoder;->nCreate(...) is not implemented
```

This is the image/render surface owned by W104-A; ACOS has no `ImageDecoder`
natives yet. `acos-render` already exposes `bitmap_create_from_bytes`
(PNG/JPEG/WebP), so the decoder can be wired there.

## Gates (this worktree)

| Item | Result |
|---|---|
| `cargo test -p acos-framework-natives` | 8 unit + 24 integration pass |
| `cargo test -p acos-vm --lib` | 120/120 |
| `cargo test -p acos-vm --test res_natives` (normal + gc-stress) | pass, `asset.ok=true` |
| `cargo test -p acos-runtime --test m2` | 2 passed / 1 ignored |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | clean |
| `diff-dexprogs.sh --corelib real` | 27/27 |
| `diff-dexprogs.sh --corelib real --gc-stress` | 27/27 |

## Overlap / coordination

`crates/acos-framework-natives/src/res/manager.rs`, `theme.rs`,
`crates/acos-framework-natives/tests/theme_resolution.rs` and
`crates/acos-vm/src/natives/fwk_res.rs` are also modified in the uncommitted
`.cache/wt-w104a` worktree (same `resolve_attrs` reference chase, the same
`pack_asset` asset ABI, plus `resolve_references` `?attr` chaining and a
`retrieve_attributes` clear). This session's `fwk_res.rs` fix is that
worktree's `pack_asset` implementation verbatim, to avoid a divergent
duplicate; the `theme.rs` fix here is the `resolve_attrs` part only. W104-A's
`manager.rs`/`fill_outputs` extras are not included.

## Honesty

* The M2 gate stays `#[ignore]`d; its reason now names `ImageDecoder.nCreate`.
  No Application/Activity callback is claimed beyond what the non-ignored
  tests pin.
* No `Ok(())`/stub was used for `ImageDecoder`; it remains a structured
  `UNSUPPORTED_JAVA_API` and is reported, not faked.

## Copy-back

Main advanced to `2392dcd` (W105-A parcelable/session-handler commit) while this
session ran. The patch was generated from the `d18f066` worktree, applied to
main with `git apply` (clean), and re-confirmed there:

* `cargo test -p acos-framework-natives --test xmlblock_diff` -> 6/6;
* `cargo test -p acos-vm --test res_natives` -> 2/2 (normal + gc-stress);
* `lifecycle_probe counter.apk` -> same `ImageDecoder.nCreate` blocker.

The worktree and its `target-w110a` scratch were deleted after the copy-back.
