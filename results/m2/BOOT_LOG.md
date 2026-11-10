# M2 boot log — `android.app.ActivityThread` comes up in the Rust VM

**Status: M2 attach handshake reached.** The real Android 14 `framework.jar`
bytecode runs `Looper.prepareMainLooper()` → `new ActivityThread()` →
`attach(false, 1)`, and the private `attach` reaches
`IActivityManager.attachApplication(mAppThread, startSeq)` through the single
ACOS Binder native. The in-process Rust ATMS side receives it with a **valid,
non-null `IApplicationThread` binder** (`BinderId(24)`, `startSeq = 1`).

This is the task goal ("Stop when attachApplication is received by the Rust
ATMS side with a valid IApplicationThread binder"). It is **not** the full M2
gate: `Application.onCreate` / `Activity.onCreate` still need the
`bindApplication` delivery (T-24) — see [Next blockers](#next-blockers).

Evidence: `results/m2/boot-release.json` (one full run: every native invoked,
class counts, wall time). Test: `crates/acos-runtime/tests/boot.rs`.

## Measured result (hello-views.apk)

Release build, same host, no emulator, `cargo run -q --release -p acos-runtime
--example boot_probe -- fixtures/build/apps/hello-views.apk`:

| metric | run 1 | run 2 | run 3 |
|---|---:|---:|---:|
| wall, whole boot | 99.5 ms | 97.5 ms | 93.4 ms |
| VM classes loaded | 332 | 332 | 332 |
| boot-classpath classes before first framework class | 40 | 40 | 40 |
| native methods invoked | 75 | 75 | 75 |
| ATMS `attachApplication` binder | `BinderId(24)` | — | — |

Debug build (the `cargo test` profile): **939 ms**, 334 classes, 76 natives
(the extra class/native versus release are W10-A's `MemoryIntArray` /
`Class.forName` work landing concurrently).

Per-stage wall time (release, one run):

| stage | wall |
|---|---:|
| `open-apk` + `load-app-dex` | ~0.1 ms |
| `prepare-main-looper` (`Looper.prepareMainLooper`) | 75.7 ms |
| `instantiate-activity-thread` (`ActivityThread.<init>`) | 11.9 ms |
| `attach-application` (`ActivityThread.attach(ZJ)V`) | 11.7 ms |

The 75 ms in `prepareMainLooper` is framework class loading + `<clinit>`
(`ResourcesManager`, `Configuration`, `LocaleList`, `Locale`,
`ConcurrentHashMap`, `MessageQueue`, `Binder`, …), not the looper itself.

## The path that ran

```text
android.os.Looper.prepareMainLooper()                 // real DEX
new android.app.ActivityThread()                      // real <init>, 249 classes
ActivityThread.attach(false, 1)                       // real DEX (private)
  ConfigurationController.<init>
  UserHandle.myUserId()
  DdmHandleAppName.setAppName("<pre-initialized>", uid)
  RuntimeInit.setApplicationObject(mAppThread.asBinder())
  ActivityManager.getService()                        // ServiceManager + IActivityManager$Stub$Proxy
  IActivityManager.attachApplication(mAppThread, 1)   // BinderProxy.transactNative
      -> acos-runtime ServiceTransport
      -> acos-services ServiceHost (recording ATMS decorator)
  BinderInternal.addGcWatcher(new ActivityThread$3(this))
```

The VM stops after `attach` returns (the real `main` then blocks in
`Looper.loop()`, which is out of scope here).

## Iteration log (symptom → fix)

| # | symptom | root cause | fix |
|---|---|---|---|
| 1 | `ActivityThread.<init>` → `ResourcesManager.getInstance` → `LocaleObjectCache` → `ConcurrentHashMap.<clinit>` threw `java.lang.InternalError` from `Unsafe.objectFieldOffset(Class,String)` | `java.lang.Class.getDeclaredFields()` was the M1 stub `cl_empty_field_array` (returns `[]`), so the DEX loop in `Unsafe.objectFieldOffset(Class,String)` never found the field and threw `InternalError` | new `crates/acos-vm/src/natives/fwk_reflect.rs`: real `getDeclaredFields`, `getDeclaredFieldsUnchecked(Z)`, `getPublicDeclaredFields` materialising `java.lang.reflect.Field` mirrors (same layout `natives::reflect` already reads back) |
| 2 | after (1), boot hung forever inside `ConcurrentHashMap.initTable` (billions of instructions) | `natives/unsafe.rs` decoded any offset `>= ARRAY_BASE (16)` as an array slot. Instance field offsets are `8 + slot*4`, so `sizeCtl` (slot ≥ 2) was read/written at the wrong place and `compareAndSetInt` never matched → the `initTable` CAS retry loop spun | `unsafe.rs`: decode instance offsets with `instance_slot()` and array offsets with `array_slot()` — the object type already selects the branch; the byte value no longer decides |
| 3 | boot failed: `Landroid/os/BinderProxy;->...` no; `Unsafe.getReferenceVolatile` `UNSUPPORTED_JAVA_API` | the reference volatile/acquire/release/opaque variants were never registered (only `getObjectVolatile`) | `unsafe.rs`: register `getReferenceVolatile`, `putReferenceVolatile`, `getReferenceAcquire`, `putReferenceRelease`, `getReferenceOpaque`, `putReferenceOpaque` |
| 4 | boot failed: `no method android.app.ActivityThread.attach(ZJ)V` | `attach` is **private**; `Vm::call_method` uses virtual dispatch and cannot see it | `javabridge.rs`: new `Vm::call_method_direct` (resolves on the exact class, like `invoke-direct`); `boot.rs` uses it |
| 5 | boot failed: `DdmServer;->nativeSendChunk(I[BII)V` `UNSUPPORTED_JAVA_API` | `DdmHandleAppName.setAppName` sends a DDM chunk; ACOS has no JDWP/DDM transport | new `crates/acos-vm/src/natives/fwk_debug.rs`: `DdmServer.nativeSendChunk` is an **honest no-op** — with no debugger attached Android also discards DDM chunks. Declared divergence, no app API observes it |
| 6 | boot succeeded | — | `attachApplication` recorded by ATMS |

A temporary `eprintln!` probe was added to `interp.rs` to locate (1)/(2) and
then reverted with `git checkout crates/acos-vm/src/interp.rs`; **interp.rs,
frame.rs and heap.rs are unchanged**.

## Natives actually invoked (75, first-seen order in `boot-release.json`)

Top classes: `java.lang.String` (12), `android.os.Parcel` (9),
`jdk.internal.misc.Unsafe` (8), `java.lang.Class` (7),
`dalvik.system.VMRuntime` (6), `libcore.io.Linux` (5),
`libcore.icu.ICU`/`android.os.SystemClock` (3 each), then `AtomicInteger`,
`Reference`, `Binder`, `Double`, `System`, `SystemProperties`,
`Thread`, `MessageQueue`, `OsConstants`, `Float`, `VMStack`, `Object`,
`Field`, `DdmServer`, `BinderInternal`, `BinderProxy`.

## Files changed for this stage

New (mine):

* `crates/acos-runtime/src/boot.rs` — `Runtime::boot_app_process`, `BootReport`, `BootError`.
* `crates/acos-runtime/src/atms_boot.rs` — recording ATMS decorator (`AttachRecord`).
* `crates/acos-runtime/tests/boot.rs` — the M2 acceptance test.
* `crates/acos-runtime/examples/boot_probe.rs` — manual probe / JSON evidence writer.
* `crates/acos-vm/src/natives/fwk_reflect.rs` — real declared-field reflection.
* `crates/acos-vm/src/natives/fwk_debug.rs` — DDM no-op (declared divergence).
* `results/m2/**` — this log + `boot-release.json`.

Edited (minimal, listed):

* `crates/acos-runtime/src/lib.rs` — `pub mod boot; pub mod atms_boot;` + re-exports.
* `crates/acos-runtime/Cargo.toml` — add `acos-apk` (read `classes.dex` from the APK).
* `crates/acos-vm/src/natives/mod.rs` — two `mod` lines + two `install` calls (my files only).
* `crates/acos-vm/src/natives/unsafe.rs` — the offset-decode bug fix + the missing reference-volatile registrations.
* `crates/acos-vm/src/javabridge.rs` — add `Vm::call_method_direct`.

Not touched (other owners): `pms.rs`, `provider.rs`, `memintarray.rs`,
`settings*`, `reflect.rs`, `vmrt.rs`, `transport.rs`, `interp.rs`, `frame.rs`,
`heap.rs`.

## Test / evidence commands

```sh
cargo test -p acos-runtime --test boot -- --nocapture
cargo test -p acos-runtime -p acos-vm
cargo clippy --workspace --all-targets -- -D warnings
tools/diff-dexprogs.sh --corelib real
```

`boot.rs` skips loudly (never silently passes) when the fixture APK or the
boot-classpath cache is missing; `missing_apk_is_a_structured_error` pins the
structured failure path.

### Final verification (all green)

```
cargo test -p acos-runtime -p acos-vm --no-fail-fast   # all suites pass
cargo clippy --workspace --all-targets -- -D warnings # clean
tools/diff-dexprogs.sh --corelib real                 # 20 pass / 0 fail / 0 unsupported
```

The debug boot is ~0.94 s; the release boot is ~0.09 s. The M1 gate did not
regress despite the `unsafe.rs` offset fix.

## Next blockers

1. **`bindApplication` delivery (T-24).** The ATMS builds `AppBindData` but the
   full framework parcelables (`ApplicationInfo`, `ProviderInfoList`,
   `CompatibilityInfo`, `Configuration`) are not yet written, and the
   `IApplicationThread` transaction is not yet delivered into the app's
   `ApplicationThread.bindApplication`. Until that lands, `Application.onCreate`
   cannot run.
2. **`ActivityThread.main` preamble.** This stage runs the `attach`-equivalent
   path directly (`prepareMainLooper` + `new` + `attach`). Running the whole
   `main` additionally needs `ActivityThread$AndroidOs.install`,
   `CloseGuard`, `Environment.initForCurrentUser`,
   `TrustedCertificateStore.setDefaultUserDirectory`,
   `ActivityThread.initializeMainlineModules`, `Process.setArgV0`, and then
   `Looper.loop()` (which blocks). No new natives were required for those on the
   direct path; they will be hit when `main` is run end-to-end.
3. **`Looper.loop()` / `MessageQueue` handoff.** Needed once `attach` is
   followed by the real loop. `MessageQueue.nativeInit`/`nativePollOnce` and the
   epoll pump already exist (T-26); the framework's `bindApplication` delivery
   must wake the main looper (`LooperHandle::wake`).
4. **`Context`/`LoadedApk` and resources.** `bindApplication` +
   `performLaunchActivity` will pull `ResourcesManager`, `AssetManager`,
   `ApkAssets`, `XmlBlock`/theme resolution, and `ActivityThread`'s
   `handleBindApplication` DEX — the resource engine exists (T-04..T-06) but is
   not yet wired to the app's `LoadedApk`.
5. **`IActivityThread` interface list.** `attach` only proves
   `attachApplication`; the follow-up `finishAttachApplication`,
   `setRenderThread`, `getContentProvider`, `getActivityClientController`, and
   the `ClientTransaction` delivery are the next transaction rows.

## Honest caveats

* The ATMS `activity` service is wrapped by a small **recording decorator**
  (`atms_boot.rs`) so the test can observe the handshake from Rust; it forwards
  to the real `AtmsService`, so the state machine still runs. The wrapper uses a
  fresh `AtmsState`, so it is not yet shared with `activity_task` /
  `IActivityClientController`.
* The boot-classpath loader still eagerly loads only the `core-oj`,
  `core-libart`, `core-icu4j` trio; `framework.jar` is parsed lazily on the
  first `android.*` class, which is why the first framework class costs tens of
  milliseconds.
* `DdmServer.nativeSendChunk` is a no-op by design (no debugger). It is not an
  app-visible fake and is recorded here as a divergence.
