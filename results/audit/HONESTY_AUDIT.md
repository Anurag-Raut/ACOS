# ACOS honesty / host-safety audit

**Scope:** `crates/acos-vm/src/natives/**`, `crates/acos-vm/src/corelib/**`,
`crates/acos-runtime/src/**`, `crates/acos-services/src/**`, `crates/acos-wm`,
`crates/acos-verify`, `crates/acos-cli`, plus the reference/benchmark docs and
the native loader.

**Tree:** `HEAD e64897f`, working tree dirty (`interp.rs`, `natives/reflect.rs`,
`tests/java_cases.rs` modified; two untracked test Java files).
**STATUS.md** still says `HEAD 40f075c` (10 commits behind).

**Read-only audit.** No source file was edited. Failing-test proposals are
patches under `results/audit/patches/` (not applied).

**Ground rules used for "legitimate" vs "dishonest" (from `DECISIONS.md`
ADR-0005/0018 and `docs/handoff/01_PHILOSOPHY_AND_REQUIREMENTS.md` § "Zero
silent compatibility holes"):** a native/service may return success without
doing work only when (a) Android also does nothing observable in an
equivalent app process (no debugger, no native heap, no shader cache, no
hardware), or (b) the call is a pure cache/config warm-up with no app-visible
getter. Anything an app can read back, anything that changes a frame, a
permission, a lifecycle transition or a file/network effect must either act or
raise `UNSUPPORTED_JAVA_API` / `UNSUPPORTED_ANDROID_API`.

---

## 1. Summary table (ranked by severity)

Severity: **S1** = sandbox escape / host crash / false-pass on a security
decision; **S2** = wrong app-visible behaviour or silent loss of evidence;
**S3** = stale/over-strong documentation, or latent risk once the frame path is
bound.

| # | Sev | Area | file:line | Issue | Suggested fix | Owner suggestion |
|---|-----|------|-----------|-------|---------------|------------------|
| 1 | S1 | native loader | `acos-native-loader/src/bionic/dl.rs:89-103`, `:216-226`; `src/loader.rs:225-236`, `:356-372` | `dlopen`/`android_dlopen_ext` accept **any absolute host path** (`resolve_library` returns absolute paths unchanged) and `dlsym` resolves symbols from loaded host objects. Guest ELF is mapped `PROT_EXEC` and run in-process; **no seccomp/landlock/namespace exists** in the crate. An APK-supplied `.so` (or `dlopen("/lib/x86_64-linux-gnu/libc.so.6")` + `dlsym("system")`) gets host syscalls. | Restrict paths to the extracted APK `lib/<abi>/` root (canonicalise + `starts_with`), reject absolute host paths, resolve only through the bionic shim table, and run native libraries in a separate seccomp/landlock'd process. Document the loader as **not safe for untrusted APKs** until then. | Native-loader owner + security review |
| 2 | S1 | VM heap / reflect | `acos-vm/src/heap.rs:261-279`, `natives/reflect.rs:393-405`, `:427-445`; `vm.rs:1303` | Guest sizes reach host `vec![…; len]` / `Vec::with_capacity(n)` **before** `Vm::alloc`'s heap and `HostGuard` checks. `java.lang.reflect.Array.newInstance(String.class, Integer.MAX_VALUE)` (or a large `int[] dims`) aborts the host allocator instead of throwing `OutOfMemoryError`. | Add a checked `check_array_len(len, elem_size)` before any backing-store allocation; reject against `heap.max_bytes` **and** a hard per-allocation cap; return Java OOME. Cap `dims.len()` for recursion. | VM-heap owner |
| 3 | S1 | parcel decode | `acos-parcel/src/parcel.rs:715-725`; `acos-services/src/parcelable.rs:629-639` | `read_string16_array` and `TypedArray` do `Vec::with_capacity(n)` from a guest `int32` **without** `self.check(...)`. A binder request parcel with `writeInt(Integer.MAX_VALUE)` then a `String[]`/typed-array read attempts a multi-GB host allocation. (Sibling readers `read_int32_array`/`read_byte_array`/`read_float_array`/`read_double_array` correctly call `check` first.) | Bound `n` by the remaining buffer length before allocating (`n <= available/4` for strings; typed array `n <= available/min_payload`), or read incrementally with a growth cap. | Parcel owner + services owner |
| 4 | S1 | services | `acos-services/src/declared.rs:113-145` | `IAppOpsService.checkOperation` / `checkOperationRaw` / `checkPackage` return `MODE_ALLOWED` (0) **unconditionally** — the op code, uid and permission are ignored. On Android a restricted op (`OP_CAMERA`, `OP_RECORD_AUDIO`, `OP_FINE_LOCATION`, …) without the permission returns `MODE_ERRORED`/`MODE_IGNORED`. A permission test can falsely pass. | Only allow the fixed non-restricted op set actually observed on the launch path; for permission-gated ops consult PMS grants and return `MODE_ERRORED`/`MODE_IGNORED`, else `UNSUPPORTED_ANDROID_API`. | Services owner |
| 5 | S1 | VM native | `acos-vm/src/natives/io.rs:76-86` | `libcore.io.Linux.getenv` calls `std::env::var(name)` — the **host** environment (tokens, API keys, `SSH_*`, paths) is readable by guest Java (`Os.getenv` / `System.getenv`). Android returns the app process env. Also `setenv`/`unsetenv` (`:88-94`) are silent no-ops, so a guest write is lost. | Serve a fixed, seeded guest environment table (as `os_props` does for properties); `setenv`/`unsetenv` mutate that table or raise unsupported. | VM-native owner |
| 6 | S1 | VM native | `acos-vm/src/natives/fwk_surface.rs:730-743`, `:1283` | `decode_ops` reads a raw `u64` count and does `Vec::with_capacity(count)` from a parcel-derived buffer; `native_read_transaction_from_parcel` uses `read_bytes_from_parcel(...).unwrap_or_default()`, so a malformed transaction silently becomes an **empty transaction** (success) instead of an error. | Bound `count` by `bytes.len()/min_op_size`; return `Err`/throw and never replace a failed decode with an empty transaction. | Surface/WM owner |
| 7 | S2 | VM native | `acos-vm/src/natives/unsafe.rs:96-101` | `Unsafe.park` / `Unsafe.unpark` are `noop`. Real `LockSupport` (real core-library bytecode) therefore never blocks; contended locks/queues/futures busy-spin or return early → wrong concurrency semantics and a host CPU-spin class (same failure mode as the F-0008 incident). | Implement real park/unpark in the green-thread scheduler (like `natives/looper.rs`), or make `LockSupport` paths structured-unsupported. | VM scheduler owner |
| 8 | S2 | VM native | `acos-vm/src/natives/memintarray.rs:52-58`; `natives/parcel.rs:452-464` | Guest `FileDescriptor.descriptor` is trusted: `open("/proc/self/fd/{fd}")` re-opens an **arbitrary host fd** read-write (`MemoryIntArray`) or dups it into a Parcel. Bypasses the VFS table. | Validate the fd against the VFS table before `/proc` re-open; never accept a raw guest fd number. | VM-native owner + VFS owner |
| 9 | S2 | VM native | `acos-vm/src/natives/lang.rs:469-478` | `String.repeat(n)` does `Vec::with_capacity(base.len() * n)` with a guest `int`; debug builds panic on overflow, release builds wrap and then grow unbounded instead of `OutOfMemoryError`. | `checked_mul`, compare to heap cap, throw OOME / `IllegalArgumentException` (Android). | VM-native owner |
| 10 | S2 | VM native | `acos-vm/src/natives/fwk_input.rs:664-680` | `MotionEvent.obtain(..., pointerCount, …)` uses `Vec::with_capacity(pointer_count.max(0) as usize)` and loops to `pointer_count` from guest input. | Clamp to the coords/props array length; otherwise throw. | Input-native owner |
| 11 | S2 | tests / CI | see §3 (`crates/acos-vm/tests/dexprogs.rs:68`, `snapshot.rs:28`, `acos-runtime/tests/m2.rs:61`, `acos-jni/tests/hello_jni.rs:58`, `acos-framework-natives/tests/*`) | Dozens of differential tests **return/pass** when `fixtures/build` (gitignored) or `.cache/boot` is absent. A fresh clone / CI run is green while M1/M2/APK/JNI evidence is never exercised. | Add `ACOS_REQUIRE_FIXTURES=1` (CI) that turns every skip into a failure; make fixture build a required CI step. | Test/CI owner |
| 12 | S2 | docs | `STATUS.md:3,44,60,189,217`; `COMPATIBILITY.md:26,102,175` | Says `physical-device` is "not captured / do not exist", but `fixtures/reference-device/` is a committed **physical-device** capture (moto g35 5G, Android 15/API 35) from commit `50a2f5f`, and STATUS is 10 commits behind HEAD. Tier labelling is wrong (under-claim) and the "honest, dated snapshot" claim is stale. | Regenerate STATUS at HEAD; record `physical-device` (and its density/API caveats) as captured-but-not-interchangeable. | Docs owner |
| 13 | S2 | VM native (frame) | `acos-vm/src/natives/gfx_canvas.rs:116` | `nEnableZ` is a `noop`. Android defers draw order for Z/elevation; overlapping `View.setZ`/`setElevation` content will composite in call order once the renderer is bound. | Record Z enable in the recording-canvas state and sort/segment display ops accordingly. | Graphics-native owner |
| 14 | S2 | VM native (frame) | `acos-vm/src/natives/gfx_hwui.rs:176-218` | `nSetOpaque`, `nSetForceDark`, `nSetHighContrastText`, `nSetDisplayDensityDpi`, `nSetContentDrawBounds`, `nSetLightAlpha`, `nSetSdrWhitePoint`, `nSetTargetSdrHdrRatio` are all `h_noop` and **not recorded**, despite the module comment saying values are recorded. Dark-mode / density / contrast fixtures will silently render wrong once `frame_handoff` is wired. | Store these on the proxy and feed the renderer; otherwise declare them as a documented frame-fidelity limitation in STATUS. | Graphics-native owner + docs |
| 15 | S2 | benchmarks | `BENCHMARKS.md:284` vs `:266-272`; `results/bench/REPORT.md` §1 | `§8.2` M9 says "N=10 memory measured (**1536.7 MiB**)" while `§8.1`/REPORT (run `191849`/`201637`) say N=10 total = 293,640 KiB = **286.7 MiB**. 1536.7 MiB comes from an older run (`run-2026-11-10-193539.json`). | Correct the M9 row to the cited run and state which run each number comes from. | Bench owner |
| 16 | S2 | VM native | `acos-vm/src/natives/io.rs:180-201` | `getpid`/`gettid` in the base module return the **host** process id; `os_misc` overrides uid/gid/tid but not `getpid` (`os_misc.rs:258-266`). Non-deterministic host value leaks to guest and can break fixture comparison. | Seed a virtual pid like `APP_UID`/tid. | VM-native owner |
| 17 | S2 | tests | `acos-runtime/tests/m2.rs:154-176` | `lifecycle_never_claims_an_undelivered_transaction` asserts only **inside** `if report.transaction_delivered`. Today `transaction_delivered == false`, so the "honesty invariant" test passes without exercising anything. | Assert the progress fact explicitly (e.g. `assert!(!report.transaction_delivered || report.app_created)`) and add a negative-path test with a fabricated report. | Runtime test owner |
| 18 | S2 | VM native | `acos-vm/src/natives/vmrt.rs:12-38` | 31 `VMRuntime` no-ops. Most are legitimate for a heapless/JIT-less VM, **but** `clampGrowthLimit`/`clearGrowthLimit`/`setTargetHeapUtilization` silently accept while `Runtime.maxMemory/totalMemory` are served elsewhere — guest heap-tuning APIs report success with no effect. | Keep the honest no-ops; return the VM's real heap values from the getters, or document the divergence next to the no-op list. | VM-native owner |
| 19 | S3 | docs / bench | `STATUS.md` "What runs" | Quotes interpreter `02_int_arith` 6.361 ms / 9,806,216 insn/s (run `201637`) while the authoritative `results/bench/REPORT.md` §1 records 16.048 ms / 3.86 M (run `205045`) and `BENCHMARKS.md` §8.1 records 22.8 ms / 2.73 M (run `191849`). Same metric, best run quoted without the spread. | Quote the REPORT row in STATUS and mention the run-to-run spread (host loadavg 4.8). | Docs owner |
| 20 | S3 | benchmark method | `results/bench/REPORT.md` §2 (`M1`, `L-marginal`) | The report states "better/worse are computed only for the pairs whose metric type and state match", then marks `M1` better (legacy idle core vs ACOS **blocked** post-bind framework) and `L-marginal` better (legacy **workset** vs ACOS **anon+shmem**). | Downgrade those two rows to `not-comparable`/`different-state`; keep the numbers. | Bench owner |
| 21 | S3 | services | `acos-services/src/declared.rs:340-360` | `IUserManager` returns `true` for `isUserUnlocked`/`isUserForeground` for **any** user id, and `getUserInfo` for arbitrary ids — only one user exists. | Return per-id state (`true` only for id 0; unknown id → unsupported/`null`). | Services owner |
| 22 | S3 | services | `acos-services/src/display.rs:233` | `isUidPresentOnDisplay` returns `true` for every display id, including nonexistent ones. | Return `true` only for display 0 / known displays. | Services owner |
| 23 | S3 | tests | `acos-vm/tests/dexprogs.rs:44-48` | `expected()` uses `unwrap_or_default()`: a missing `.stdout` becomes `""` and a missing `.exit` becomes `0`, so a program producing no output passes with no reference. | Treat missing expected files as a hard failure when the program is present. | Test owner |
| 24 | S3 | services | `acos-services/src/declared.rs:432-434` | `getNotificationChannels*` replies `Parcelable::Null` where Android returns a `ParceledListSlice` (array/list). | Return an empty list slice or unsupported. | Services owner |
| 25 | S3 | VM native | `acos-vm/src/natives/native_mem.rs:503-531` | `peek/poke` build `Vec::with_capacity(count * scale)` from guest `int`; range checks run in helpers but the capacity is allocated first for `poke`. | Move the bounds check before the capacity allocation. | VM-native owner |
| 26 | S3 | VM native | `acos-vm/src/natives/fwk_dexfile.rs:250` | `Vec::with_capacity(file.size())` from a zip entry header (guest APK-controlled). Bounded by the actual archive only after `read_to_end`. | Cap by the remaining archive bytes / a fixed DEX max. | VM-native owner |
| 27 | S3 | docs | `acos-vm/src/natives/gfx_hwui.rs` module/setter comments | Comments say setters are recorded; the registrations are `h_noop`. | Align comment with code (see #14). | Graphics-native owner |
| 28 | S3 | docs | `acos-vm/src/natives/gfx_state.rs` / `gfx_canvas.rs` | `nIsOpaque` returns constant `false`, `nGetMaximumTexture{Width,Height}` return 4096 — plausible defaults but not device values. | Derive from the seeded display config. | Graphics-native owner |
| 29 | S3 | tests | `acos-vm/tests/snapshot.rs:28` | Full real-corpus snapshot test is release-only and **skips under the default debug `cargo test`**. | Gate on `ACOS_REQUIRE_FIXTURES`, or make the release job mandatory. | Test/CI owner |
| 30 | S3 | benchmark harness | `tools/bench-acos.sh:22-24` vs `results/bench/baseline.json` rows | Script header says default tolerance `max(5 %, spread)` but the committed baseline rows hard-code `0.25` (25 %). A 24 % regression passes silently. | Make the gate use `max(0.05, spread)` per row, or document 25 % as the chosen tolerance. | Bench owner |

### Legitimate no-ops (classified, not findings)

These return success/0 but are justified by Android semantics in this sandbox
and should be kept (and ideally cited in the file):

* `natives/fwk_debug.rs` DDM chunk discard (no debugger attached).
* `natives/vmrt.rs` heap-task/JIT/hidden-api counters (no ART heap/JIT) — except
  the growth-limit setters in #18.
* `natives/os_misc.rs:134-181` `Trace`/`nativeGetEnabledTags` returning 0
  (no systrace/Perfetto sink) — matches F-0008.
* `natives/gfx_canvas.rs` `nFreeCaches`/`nFreeTextLayoutCaches` (cache maintenance),
  `gfx_bitmap.rs` `nativePrepareToDraw`/`nativeSetHasMipMap`,
  `gfx_path.rs` `nIncReserve` (capacity hint only).
* `natives/binder.rs` `flushPendingCommands`/`joinThreadPool`/`setMaxThreads`
  and `lang.rs` `ThreadGroup.add/remove` (in-process binder / green threads).
* `natives/io_linux.rs` `chown` (no root), `natives/lang.rs` monitor hooks
  (Java-side), `natives/zip.rs` preset dictionary (unused).
* `services/*` state-changing notification/app-op/permission **mutators** —
  these correctly return `UnsupportedAndroidApi` after recording.
* `services/declared.rs` connectivity reads (no network) — correct, but the
  empty `getCaptivePortalServerUrl` (#24-adjacent) is a type/default choice.

---

## 2. Host-safety detail (top 30, ranked)

The table above already contains the S1/S2 host-safety rows. Full ranked list of
the 30 highest-risk guest-triggerable items, with a concrete trigger:

| Rank | file:line | Trigger (guest input) | Effect |
|---|---|---|---|
| 1 | `acos-native-loader/src/bionic/dl.rs:89,216` + `src/loader.rs:225,356` | `System.loadLibrary` with an APK `.so`, or `dlopen("/lib/x86_64-linux-gnu/libc.so.6")` | host code execution / host file read; no OS sandbox |
| 2 | `acos-vm/src/heap.rs:261`, `natives/reflect.rs:400` | `Array.newInstance(String.class, Integer.MAX_VALUE)` | host OOM/abort |
| 3 | `acos-vm/src/natives/reflect.rs:427-445` | `Array.newInstance(T, new int[]{1,1,…})` (large dims) | host OOM + deep recursion |
| 4 | `acos-parcel/src/parcel.rs:715-724` | parcel `int32 = 2^31-1` then read `String[]` | host OOM |
| 5 | `acos-services/src/parcelable.rs:633` | parcel typed-array count `2^31-1` | host OOM |
| 6 | `acos-vm/src/natives/fwk_surface.rs:732` | crafted surface transaction bytes with `count = 2^40` | host OOM |
| 7 | `acos-vm/src/natives/io.rs:78` | `Os.getenv("AWS_SECRET_ACCESS_KEY")` | host secret disclosure |
| 8 | `acos-services/src/declared.rs:99-115` | `AppOpsManager.checkOp(OP_CAMERA)` | false "allowed" |
| 9 | `acos-vm/src/natives/unsafe.rs:99` | `LockSupport.park()` / `Future.get()` | spin / wrong concurrency |
| 10 | `acos-vm/src/natives/memintarray.rs:54` | `MemoryIntArray` created over fd N | host fd read/write |
| 11 | `acos-vm/src/natives/parcel.rs:454` | `Parcel.writeFileDescriptor(fd=N)` | host fd duplication |
| 12 | `acos-vm/src/natives/lang.rs:473` | `"x".repeat(2_000_000_000)` | overflow / host OOM |
| 13 | `acos-vm/src/natives/fwk_input.rs:667` | `MotionEvent.obtain(..., 2^31-1, ...)` | host OOM / long loop |
| 14 | `acos-vm/src/natives/native_mem.rs:509,530` | `Unsafe.peek/poke` with `count = 2^31-1` | host OOM |
| 15 | `acos-vm/src/natives/reflect.rs:435-441` | `Array.newInstance` multi-dim with negative/huge dims | recursion / OOM |
| 16 | `acos-vm/src/natives/io.rs:190` | `Os.getpid()` | host pid leak / non-determinism |
| 17 | `acos-vm/src/natives/fwk_surface.rs:1284` | malformed transaction parcel | silently empty transaction (wrong success) |
| 18 | `acos-vm/src/natives/io.rs:88,92` | `Os.setenv` then `Os.getenv` | silent loss (wrong success) |
| 19 | `acos-vm/src/natives/os_props.rs:283` | boot dir `build.prop` read | host-file read gated by `ACOS_BOOT_DIR` (host-controlled) |
| 20 | `acos-vm/src/natives/os_log.rs:341` | logcat tag/file read | host `/proc` read (low) |
| 21 | `acos-vm/src/natives/os_clock.rs:78` | `/proc/thread-self/stat` read | host read, no guest control |
| 22 | `acos-vm/src/natives/fwk_dexfile.rs:250` | zip entry with huge declared size | allocation bounded by archive; still cap |
| 23 | `acos-runtime/src/apk_loader.rs:44-58` | host-supplied APK path | writes APK into host data dir (path is host caller, not guest) |
| 24 | `acos-services/src/parcelable.rs:1216,1306` | boot-DEX scan | host dir read (repo path, not guest) |
| 25 | `acos-vm/src/natives/io_linux.rs:454+` | file path | correctly routed through `Vm::vfs` (verified; keep) |
| 26 | `acos-vm/src/natives/gfx_hwui.rs:213` | `nTrimMemory` | no-op (low) |
| 27 | `acos-vm/src/natives/zip.rs:266+` | `off/len` from guest | `.max(0)` then range-checked by zip layer (verified) |
| 28 | `acos-vm/src/natives/icu.rs:233` | offsets from guest | checked against array bounds (verified) |
| 29 | `acos-vm/src/natives/parcel.rs:345` | position/size from guest | checked by parcel table (verified) |
| 30 | `acos-vm/src/natives/icu_regex.rs:88` | regex pattern | step budget via `ACOS_REGEX_MAX_STEPS`; parser nesting capped (verified) |

Notes that bound the severity honestly:
* #1 is **not reachable from the product `acos run`/`verify` path today**:
  `jni_glue::load_library_impl` raises `UNSUPPORTED_JAVA_API` when no JNI host is
  installed (`natives/jni_glue.rs:180-186`), and `set_host` is called only from
  `acos-jni::JniRuntime::attach` (`acos-jni/src/state.rs:413`). No workspace
  crate depends on `acos-jni`, so only its own tests/examples exercise the
  loader. It becomes S1 the moment native loading is wired into the product.
* #2–#6 are reachable from ordinary guest bytecode/parcel traffic through the
  normal VM path; they are the priority to fix.
* The `getenv` leak (#7) is reachable from ordinary guest bytecode today.

---

## 3. Tests that cannot fail / silently skip

**Systemic (highest impact).** `fixtures/build/` is git-ignored; `.cache/boot/`
is git-ignored. The following differential/acceptance tests print `SKIP …` and
**pass** when those are absent, so a fresh clone or CI run is green with none of
the evidence exercised:

* `crates/acos-vm/tests/dexprogs.rs:68-76,115-119` (M1 console corpus)
* `crates/acos-vm/tests/snapshot.rs:28,99-124,173-245` (snapshot/restore)
* `crates/acos-runtime/tests/m2.rs:57-63,110-113,146-151` (M2 gate + progress)
* `crates/acos-jni/tests/hello_jni.rs:52-60` (the only native-loader acceptance)
* `crates/acos-framework-natives/tests/{smoke,xmlblock_diff,theme_resolution,differential_aapt2}.rs`
* `crates/acos-runtime/tests/{dex_loader,vsync_natives,wms}.rs`
* `crates/acos-vm/tests/{java_cases,surface_natives}.rs`
* `crates/acos-cli/tests/run_dex.rs:75,98`
* `crates/acos-render/tests/text_coverage.rs:293-307` (opt-in `ACOS_RENDER_SKIP_GOLDEN`)

They are "loud" (they print), but loud ≠ failing. **Recommendation:** a
`ACOS_REQUIRE_FIXTURES=1` CI mode that converts every `SKIP` into a panic, plus
a CI job that runs `tools/build-fixtures.sh` / `tools/build-apps.sh` and the
boot-cache extractor first.

**Vacuously passing:**
* `crates/acos-runtime/tests/m2.rs:154-176` — the honesty invariant only
  asserts when `transaction_delivered` is true; it is false today. Passes
  trivially.
* `crates/acos-vm/tests/dexprogs.rs:44-48` — missing expectations become
  empty string / exit 0.

**No misuse of `#[ignore]`:** the only `#[ignore]` is
`crates/acos-runtime/tests/m2.rs:108` and it carries a precise, current reason.
(One stale doc comment above it still says "blocks in `Typeface.<clinit>`",
which predates the current `Settings.getPublicSettingsForClass` blocker; fix
while renaming.)

**Assertions on construction success only:** none material found in the audited
crates beyond intentional `is_ok()` API checks (`acos-render/tests/contract.rs`,
`acos-protocol` validation). No `assert_eq!(x, x)` self-comparisons found.

**Expected-file provenance:** `fixtures/expected/` = host JVM
(`jvm-reference`, documented in `fixtures/README.md`); `fixtures/expected-art/`
= real ART (`tools/oracle/run-dexprogs-art.sh`); `fixtures/reference/**` = ART
emulator; `fixtures/reference-device/**` = real phone (`DEVICE.md`). No expected
file was generated by ACOS itself. `tools/oracle/gen_regex_matrix.py` generates
only `Main.java`; its expected stdout is captured from ART. **No self-generated
oracle found.**

---

## 4. Tier labelling

* **Correct:** `jvm-reference` vs `art-reference` are consistently labelled in
  `fixtures/README.md`, `STATUS.md`, `COMPATIBILITY.md`, `results/oracle/*`.
  The 6 app references and the 25 console programs are real ART captures.
* **Violation (under-claim):** `STATUS.md:44,60,189,217` and
  `COMPATIBILITY.md:26,102,175` say `physical-device` is "not captured / do not
  exist", but `fixtures/reference-device/` is a committed `physical-device`
  capture with its own `DEVICE.md`/`COMPARISON.md` and capture script
  (`50a2f5f`). It is correctly *not* mixed with `art-character` references, but
  the docs are stale.
* **Violation (stale HEAD):** `STATUS.md:3` says `HEAD 40f075c`; actual HEAD is
  `e64897f` (+10 commits incl. the physical-device capture and the JNI work).
* **Minor:** `STATUS.md` "M1 25/25" while the committed
  `results/vm/dexprogs-real-normal.json` is 24/25 — STATUS admits this in "Known
  risks", which is honest, but the headline table should not be read as 25/25.

---

## 5. Benchmark honesty

* **Strong:** metric types are separated (RSS / PSS / anon+shmem / private+
  shared); "not measured" rows are not guessed; the regression baseline records
  its source run/commit; no `/usr/bin/time` dependency.
* **Finding:** `BENCHMARKS.md:284` M9 = 1536.7 MiB contradicts the same
  document's §8.1 (293,640 KiB = 286.7 MiB) and the REPORT; it is an older run's
  number (see #15).
* **Finding:** the REPORT's own "same type/state" rule is broken by `M1`
  (legacy idle core vs ACOS partially-booted framework) and `L-marginal`
  (workset vs anon+shmem) — see #20.
* **Finding:** STATUS quotes the best of three interpreter runs (§1 #19).
* **Not found:** cherry-picked *legacy* baselines — the legacy numbers are cited
  with their source (`ACVM_INVENTORY`, v5/v9/v12) and explicitly labelled
  "reference only".
* **Minor:** the gate tolerance is 25 % in `baseline.json` while the script
  header describes `max(5 %, spread)` (#30).

---

## 6. Verdict on current `STATUS.md` claims

**Supported:**
* M1 console execution on the real core library matching ART (artefacts exist;
  committed JSON lags at 24/25 as admitted).
* M2 only up to `bindApplication` + scheduled `ClientTransaction`, with the
  lifecycle gate red/`#[ignore]`d. The document does **not** claim the callbacks
  or a frame.
* M3–M5, UI tree, input, reset and end-to-end `verify` are correctly marked
  not-started/not-verified.
* Tier definitions and the jvm/art provenance are honest.

**Not supported / needs correction:**
* The snapshot is **stale**: 10 commits behind HEAD and directly contradicts
  committed `physical-device` captures.
* The "never fake behaviour" invariant is **violated in concrete, guest-visible
  places**: `IAppOpsService.checkOperation` unconditional allow, `getenv` host
  leak, `Unsafe.park/unpark`, `setenv/unsetenv`, renderer config setters,
  failed surface-transaction decode → empty success. These are not declared in
  the "Unsupported surface" table.
* The host-safety posture implied by "untrusted APKs cannot run with
  host-root-equivalent privileges" is **not demonstrated** for the native
  loader (no seccomp/landlock; arbitrary `dlopen` paths). Today it is test-only,
  but STATUS lists the loader as "verified" without that caveat.
* Benchmark rows contain one stale number and two scope-mismatched "better"
  verdicts, and STATUS quotes a favourable interpreter run not in the REPORT.

**Bottom line:** STATUS is directionally honest about *milestones* (it refuses
to claim frames/input/V1) but is **not a current, fully supported snapshot**,
and the codebase has real "fake success" holes in the services/VM surface and
real guest-reachable host-safety bugs in allocation and identity handling.

---

## 7. Failing-test proposals

Patches in `results/audit/patches/` add tests (new files or small additions)
that encode the desired honest behaviour and **fail against `e64897f`**. They are
proposals, not applied. Each file header states the finding it pins.

Written and self-contained (new test files; apply with `git apply`):

| Patch | Finding | Expected now |
|---|---|---|
| `01_appops_restricted_op.patch` | #4 AppOps unconditional allow | FAIL (`MODE_ALLOWED` returned for `OP_CAMERA`) |
| `02_iusermanager_unknown_user.patch` | #21 all user ids "unlocked" | FAIL (`Bool(true)` for id 99) |
| `03_parcel_string_array_count.patch` | #3 parcel capacity bomb | FAIL (returns `Underflow`, not `BadLength`) |
| `04_fixtures_required.patch` | #11 silent CI skips | FAIL when `ACOS_REQUIRE_FIXTURES=1` and fixtures are absent |

The remaining findings are described with file:line and trigger in §1–§2 above
and were **not** written as patches because they need the Java/DEX boot harness
(`.cache/boot/android-34`) or would allocate gigabytes before the fix, so a
naive test would abort the host rather than fail cleanly:

* #2/#6/#9/#10 (guest-length allocations) — propose a checked
  `check_array_len`/`decode_ops(count)` unit seam first; the parcel patch #03
  covers the same class safely.
* #5 `getenv` leak, #7 `Unsafe.park`, #5/#18 `setenv` — add to
  `crates/acos-vm/tests/os_natives.rs` (real corelib + `android.system.Os`),
  following the existing `OsNativesMain.java` pattern; assert `Os.getenv("PATH")`
  is null, `Os.setenv` round-trips, and `LockSupport.park` blocks.
* #17 vacuous M2 invariant — strengthen
  `crates/acos-runtime/tests/m2.rs:154-176` to assert a progress fact even when
  `transaction_delivered == false`.
* #12/#15/#19/#20 — documentation/benchmark corrections, not tests.
