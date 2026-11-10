# ACOS — Compatibility ledger

**Live ledger.** Every cell starts at **`none`** and may only advance when a
reproducible evidence artifact exists (a committed test, a `results/` file, or a
run recorded in `EXPERIMENTS.md`). **No claim may precede evidence.** This file is
the single source of truth for what ACOS can and cannot do.

Status values:

| Status | Meaning |
|---|---|
| `none` | no implementation and no evidence |
| `partial` | some cases work; failures/limits enumerated in the evidence pointer |
| `verified` | a defined fixture set passes, with negative cases detected |

Every non-`none` row records the **evidence pointer**, **fidelity**, **API
level**, **ABI** and **date**. `verified` on *decode* is not `verified` on
*execute*. The legacy ACVM results are **not** ACOS results and are not recorded
here.

> **As of 2026-11-10 (HEAD `9426b5b` + working tree): static DEX/APK analysis is
> verified; the 25 console `dexprogs` execute on the real Android 14 core library
> (`art-reference`); framework boot reaches `bindApplication` + a real
> `ClientTransaction`; Activity callbacks, frames, UI tree and input are still
> `none`.** Tiers follow ADR-0005: `jvm-reference` (weak), `art-reference`
> (authoritative, API-34 x86_64 emulator) and `physical-device` (real phone;
> captured under `fixtures/reference-device/`, kept separate from the emulator
> tier because its API/density differ).

---

## 1. DEX format and opcode families

Fidelity for all `verified` decode rows: **`static-analysis`**. API 34 fixture
context; x86_64 host. Date 2026-11-10.

| Area | Parse/verify | Decode | Execute | Evidence |
|---|---|---|---|---|
| DEX versions 035–039 | verified | verified | verified (console) | `crates/acos-dex/tests/{fixtures,corruption}.rs`; execution: `results/m1/README.md` |
| Header / Adler-32 / SHA-1 | verified (Adler enforced; SHA-1 recorded, accepted by default per ADR-0017) | n/a | n/a | `crates/acos-dex/src/lib.rs` (`ParseOptions`, `signature_valid`); `hash.rs` |
| String / type / proto / field / method / class tables | verified | verified | verified (console) | `diff_dexdump.sh` → 380 methods / 4,253 instructions / 0 mismatches |
| `class_data` / `code_item` / debug_info / annotations | partial | partial | partial (console) | decoder covers `code_item`/annotations; debug_info partial |
| `hiddenapi_class_data_item` (`0xF000`) | partial | n/a | n/a | `crates/acos-dex/src/hiddenapi.rs` |
| Move family | verified | verified | verified (console) | `diff_dexdump.sh`; `results/vm/opcode-coverage.json` |
| Constants (`const*`, `const-string`, `const-class`, …) | verified | verified | verified (console) | `diff_dexdump.sh` |
| Arithmetic/logic incl. `-wide`/`-float`/`-double`/`lit*`/`2addr` | verified | verified | verified (console) | `diff_dexdump.sh` |
| Branches (`if-*`, `goto*`) | verified | verified | verified (console) | `diff_dexdump.sh` |
| Switch (`packed`/`sparse`) | verified | verified | verified (console) | `diff_dexdump.sh` |
| Invokes (`invoke-*`, `/range`, `/polymorphic`, `/custom`) | verified | verified | partial | `diff_dexdump.sh`; `invoke-polymorphic`/`invoke-custom` remain unsupported at runtime (`results/vm/opcode-coverage.json`) |
| Instance/static fields (`iget`/`iput`/`sget`/`sput*`) | verified | verified | verified (console) | `diff_dexdump.sh` |
| Arrays (`new-array`, `filled-new-array`, `array-length`, `aget`/`aput`) | verified | verified | verified (console) | `diff_dexdump.sh` |
| Checks/casts/instance checks (`check-cast`, `instance-of`, conversions) | verified | verified | verified (console) | `diff_dexdump.sh` |
| Object creation (`new-instance`) | verified | verified | verified (console) | `diff_dexdump.sh` |
| Monitors (`monitor-enter`/`exit`) | verified | verified | verified (console) | `diff_dexdump.sh`; `Sync.java` fixture |
| Exceptions / `try`/`catch` | verified | verified | verified (console) | `diff_dexdump.sh`; `TryCatch.java` fixture |
| Payloads, `nop`, `throw`, `return*` | verified | verified | verified (console) | `diff_dexdump.sh` |
| Verification agreement with the Android verifier | none | n/a | n/a | structural verifier only; no differential vs ART verifier |
| Opcode coverage report | n/a | n/a | **present** | `results/vm/opcode-coverage.json`: 123 executed / 95 implemented-unexercised / 6 unsupported |

**Execute fidelity:** `art-reference` for the 25 console programs
(`results/vm/OS_NATIVES_AUDIT.md` §3; `results/m1/README.md` for the original
20-program gate). No framework/Activity bytecode is executed end to end yet.

---

## 1b. APK ingest

Fidelity **`static-analysis`**; date 2026-11-10.

| Area | Status | Evidence |
|---|---|---|
| ZIP central directory / entry inventory | verified | `crates/acos-apk/src/apk.rs`; `tests/fixture_apk.rs` |
| Binary `AndroidManifest.xml` (AXML) DOM + typed manifest model | verified | `tests/differential_aapt2.rs` (vs `aapt2 dump badging` + `dump xmltree`) |
| `resources.arsc` parsing | partial | `crates/acos-apk/src/arsc.rs` |
| Signature-scheme detection | verified (detection only) | `crates/acos-apk/src/signing.rs` |
| ABI / native-lib / asset inventory | partial | `crates/acos-apk/src/apk.rs` |

---

## 2. Java core-library surface

Strategy: ADR-0007 (real core-library DEX + Rust natives). The `acos-vm`
built-ins remain a temporary bootstrap; the M1/M2 paths use the real DEX.

| Package / area | Status | Fidelity | Evidence |
|---|---|---|---|
| `java.lang.Object` / `String` / wrappers / `Math` | verified (console) | art-reference | `results/m1/README.md`; `results/vm/dexprogs-real-normal.json` |
| `java.lang.Class` / reflection | partial | art-reference (console) | console corpus; constructor reflection in `crates/acos-vm/src/natives/fwk_reflect.rs` |
| `Throwable` / exceptions / stack traces | verified (console) | art-reference | `13_exceptions`, `20_uncaught` in `results/m1/README.md` |
| Threads / JMM / `synchronized` | verified (console) | art-reference | `19_threads_sync`; `results/m1/README.md` |
| `java.lang.ref` | none | — | — |
| `java.util` collections | partial | art-reference (console) | console corpus; `HashMap` is linear-scan (`crates/acos-vm/PERF.md`) |
| `java.util.concurrent` | none | — | — |
| `java.io` / `java.nio` | partial | art-reference (console) | FileChannel/VFS work in `results/m2/M2_LOG.md` (W21a) |
| `java.util.regex` (ICU) | partial | art-reference (console) | fixtures 21–23 (`results/vm/OS_NATIVES_AUDIT.md` §3); unsupported constructs raise |
| `javax.*` | none | — | — |
| `java.security` providers / digests | partial | NIST-verified + art-reference | fixture 25 (`results/m2/M2_LOG.md` W24-A) |
| class initialization / `ClassLoader` / multi-DEX | partial | art-reference | real `dalvik.system.DexFile` class loader (`results/m2/M2_LOG.md` W24b) |

**Tier note.** The original 20 `dexprogs` have `jvm-reference` expected outputs
under `fixtures/expected/dexprogs/` and authoritative `art-reference` outputs
under `fixtures/expected-art/dexprogs/`; `results/oracle/jvm-vs-art.md` records
where ART and the JVM differ. `physical-device` captures are real-phone
references under `fixtures/reference-device/` (moto g35 5G, Android 15/API 35);
they are not interchangeable with the API-34 emulator references.

---

## 3. JNI and native code

| Area | Status | Evidence |
|---|---|---|
| `JavaVM` / `JNIEnv` tables in Rust | partial | `crates/acos-jni/` (table/dispatch/refs); `crates/acos-jni/tests/jni_errors.rs` |
| `JNI_OnLoad` / `RegisterNatives` | verified (fixture) | `hello-jni` matches the ART reference (`crates/acos-jni/tests/hello_jni.rs`) |
| local/global/weak refs | partial | `crates/acos-jni/src/refs.rs` |
| strings / arrays / field & method IDs | partial | `crates/acos-jni/src/jni.rs` |
| exception propagation across the boundary | partial | `crates/acos-jni/tests/jni_errors.rs` |
| native thread attach/detach | partial | `crates/acos-jni/src/state.rs` |
| ELF loading / relocations / symbol resolution | verified (fixture) | `results/native-loader/README.md`; 475 Bionic symbols (383 forwarded / 92 shimmed) in `results/native-loader/coverage.json`; `libhellojni.so` load p50 8,466 ns |
| ABI translation (ARM-only on x86_64) | none (`WRONG_ABI`, ADR-0002/0014) | `results/native-loader/README.md` §2 |

---

## 4. Android API surface

The real framework bytecode supplies most of this (ADR-0007/0008) with the
Rust in-process service layer replacing system_server/Binder. State is **partial**
where the framework path executes and `none` where it has never run.

| Area | Status | Evidence |
|---|---|---|
| app/Activity lifecycle + Task/back stack | partial — `attachApplication`/`bindApplication` delivered, no `onCreate` | `results/m2/M2_LOG.md`; `results/m2/boot-release.json` |
| `Intent` / resolution | partial | `crates/acos-runtime/src/client_transaction.rs` |
| resources / configuration / `DisplayInfo` | partial | `crates/acos-runtime/src/display_java.rs` + `tests/display.rs` (working tree) |
| storage (files, `SharedPreferences`, SQLite) | partial | VFS + `MemoryIntArray` (`results/m2/M2_LOG.md`); Settings provider in `results/services/REPORT.md` |
| permissions | partial | PMS objects in `results/services/REPORT.md` |
| Binder/services/providers/broadcast | partial — instructions are decoded from the real DEX; one process-global transport | `results/services/REPORT.md`; `results/bench/MEMORY_PROFILE.md` §5 |
| View system / layout / measurement | partial (renderer). No live Activity tree | `results/render/README.md` |
| rendering (offscreen, text, images, frame lifecycle) | partial (renderer). No framework-bound frame | `results/render/README.md`; `crates/acos-runtime/src/frame_handoff.rs` |
| input (touch/key/gesture/IME/focus) | partial (Rust dispatcher). No fixture evidence | `results/wm/README.md` |
| vsync / Choreographer | partial | `crates/acos-vm/src/natives/fwk_vsync.rs`; `results/wm/README.md` |
| WebView | none | — |
| notifications / alarms / jobs | none | — |
| network (DNS/TLS/sockets) | none — refused by design | `results/vm/OS_NATIVES_AUDIT.md` §2 |
| media/camera/sensors/HALs | none | — |
| GMS / DRM / Play Integrity | none (licence/device-gated) | — |

---

## 5. App classes

| App class | Status | Evidence |
|---|---|---|
| Plain Java/Kotlin View app | partial — APKs build, inspect and get ART references; no Activity runs | `fixtures/apps/{hello-views,counter,todo-list,multi-activity,form-validation}`; `docs/DEV_WORKFLOW.md` |
| Compose app | none | out of V1 scope (`docs/V1.md`) |
| Flutter / React Native / WebView | none | out of V1 scope |
| NDK/JNI app | partial — `hello-jni` output matches the ART reference | `crates/acos-jni/tests/hello_jni.rs` |
| ARM-only native app | none (deferred) | `results/native-loader/README.md` |
| Split APK / multi-DEX | partial | `dalvik.system.DexFile` loader (`results/m2/M2_LOG.md` W24b) |
| Multi-process app | none | — |
| SurfaceView/OpenGL/Vulkan | none — software 2D only (ADR-0011) | `results/render/README.md` |
| GMS/DRM/integrity/OEM-dependent | none — expected unsupported on the native path | — |

---

## 6. Fixture corpus ledger

ACOS authors its own licensable fixtures (Apache-2.0). No third-party APKs are
redistributed.

| Fixture | Class | Status | Fidelity | Evidence |
|---|---|---|---|---|
| `fixtures/dexprogs/01..25` (Java console programs) | pure-Java semantics | **verified** on real core libs | art-reference | `fixtures/expected-art/dexprogs/`; `results/m1/README.md`; `results/vm/OS_NATIVES_AUDIT.md` §3 |
| `fixtures/apps/hello-views` | Views Activity | present (APK built, debug-signed); Activity not executed | art-reference captured | `fixtures/reference/hello-views/art-api34-x86_64/` |
| `fixtures/apps/hello-jni` | NDK JNI (x86_64 + arm64-v8a) | verified (fixture output) | art-reference | `crates/acos-jni/tests/hello_jni.rs`; `fixtures/reference/hello-jni/art-api34-x86_64/` |
| `fixtures/apps/{counter,todo-list,multi-activity,form-validation}` | dogfood View apps | present; Activity not executed | art-reference captured | `fixtures/reference/*/art-api34-x86_64/` |
| Seeded-wrong variants | negative/false-pass | **present** (scenarios, not complete APKs) | art-reference replay | `scenarios/negative/*`; `scenarios/README.md` |
| `physical-device` captures | oracle | **captured, not interchangeable** | physical-device | `fixtures/reference-device/DEVICE.md`; `fixtures/reference-device/COMPARISON.md` |

---

## 7. How to advance a cell

1. Add a fixture (own source, licence recorded) and state the fidelity level.
2. Implement the smallest slice that exercises it on **real input**.
3. Add a differential/oracle test and a negative case.
4. Run it and capture a reproducible artifact under `results/` or a committed test.
5. Update exactly one row with status, evidence pointer, fidelity, API level, ABI
   and date. Record negative results in `EXPERIMENTS.md`/`docs/FINDINGS.md`.

A `verified` cell requires the negative case to be detected. A launch that does
not crash is **not** functional compatibility.
