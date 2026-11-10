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

> **As of 2026-11-10: static DEX/APK analysis is verified on the committed
> fixture corpus; nothing executes.** All execute/verify rows are `none`.

---

## 1. DEX format and opcode families

Fidelity for all `verified` decode rows: **`static-analysis`**. API 34 fixture
context; x86_64 host. Date 2026-11-10.

| Area | Parse/verify | Decode | Execute | Evidence |
|---|---|---|---|---|
| DEX versions 035–039 | verified | verified | n/a | `crates/acos-dex/tests/{fixtures,corruption}.rs`; the three checked-in dex (incl. api19 = 035) |
| Header / Adler-32 / SHA-1 | verified (Adler enforced; SHA-1 recorded, accepted by default per ADR-0017) | n/a | n/a | `crates/acos-dex/src/lib.rs` (`ParseOptions`, `signature_valid`); `hash.rs` |
| String / type / proto / field / method / class tables | verified | verified | n/a | `diff_dexdump.sh` → 380 methods / 4,253 instructions / 0 mismatches |
| `class_data` / `code_item` / debug_info / annotations | partial | partial | n/a | decoder covers `code_item`/annotations; debug_info partial |
| `hiddenapi_class_data_item` (`0xF000`) | partial | n/a | n/a | `crates/acos-dex/src/hiddenapi.rs` (uncommitted, in progress) |
| Move family | verified | verified | none | `diff_dexdump.sh` |
| Constants (`const*`, `const-string`, `const-class`, …) | verified | verified | none | `diff_dexdump.sh` |
| Arithmetic/logic incl. `-wide`/`-float`/`-double`/`lit*`/`2addr` | verified | verified | none | `diff_dexdump.sh` |
| Branches (`if-*`, `goto*`) | verified | verified | none | `diff_dexdump.sh` |
| Switch (`packed`/`sparse`) | verified | verified | none | `diff_dexdump.sh` |
| Invokes (`invoke-*`, `/range`, `/polymorphic`, `/custom`) | verified | verified | none | `diff_dexdump.sh`; `fixtures_invokecustom.dex` |
| Instance/static fields (`iget`/`iput`/`sget`/`sput*`) | verified | verified | none | `diff_dexdump.sh` |
| Arrays (`new-array`, `filled-new-array`, `array-length`, `aget`/`aput`) | verified | verified | none | `diff_dexdump.sh` |
| Checks/casts/instance checks (`check-cast`, `instance-of`, conversions) | verified | verified | none | `diff_dexdump.sh` |
| Object creation (`new-instance`) | verified | verified | none | `diff_dexdump.sh` |
| Monitors (`monitor-enter`/`exit`) | verified | verified | none | `diff_dexdump.sh`; `Sync.java` fixture |
| Exceptions / `try`/`catch` | verified | verified | none | `diff_dexdump.sh`; `TryCatch.java` fixture |
| Payloads, `nop`, `throw`, `return*` | verified | verified | none | `diff_dexdump.sh` |
| Verification agreement with the Android verifier | none | n/a | n/a | structural verifier only; no differential vs ART verifier |
| Opcode coverage report | none | none | none | not produced |

---

## 1b. APK ingest

Fidelity **`static-analysis`** (ADR all `none`); date 2026-11-10.

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
built-ins are a temporary bootstrap (`none` for runtime verification).

| Package / area | Status | Evidence |
|---|---|---|
| `java.lang.Object` / `String` / wrappers / `Math` | none (bootstrap only, unverified) | `crates/acos-vm/src/corelib/` |
| `java.lang.Class` / reflection | none | — |
| `Throwable` / exceptions / stack traces | none | — |
| Threads / JMM / `synchronized` | none | — |
| `java.lang.ref` | none | — |
| `java.util` collections | none | — |
| `java.util.concurrent` | none | — |
| `java.io` / `java.nio` | none | — |
| `javax.*` | none | — |
| class initialization / `ClassLoader` / multi-DEX | none | — |

**Tier note.** The 20 `dexprogs` have `jvm-reference` expected outputs under
`fixtures/expected/dexprogs/` (weak tier, ADR-0005). Matching them is **not**
Android verification.

---

## 3. JNI

| Area | Status | Evidence |
|---|---|---|
| `JavaVM` / `JNIEnv` tables in Rust | none | `acos-jni` planned |
| `JNI_OnLoad` / `RegisterNatives` | none | — |
| local/global/weak refs | none | — |
| strings / arrays / field & method IDs | none | — |
| exception propagation across the boundary | none | — |
| native thread attach/detach | none | — |
| ELF loading / relocations / symbol resolution | none (scaffold) | `crates/acos-native-loader/` |
| ABI translation (ARM-only on x86_64) | none (`WRONG_ABI`, ADR-0002/0014) | — |

---

## 4. Android API surface

All `none`. See ADR-0007/0008 — the real framework bytecode will supply most of
this, with the Rust in-process service layer replacing system_server/Binder.

| Area | Status |
|---|---|
| app/Activity lifecycle + Task/back stack | none |
| `Intent` / resolution | none |
| resources / configuration | none |
| storage (files, `SharedPreferences`, SQLite) | none |
| permissions | none |
| Binder/services/providers/broadcast | none |
| View system / layout / measurement | none |
| rendering (offscreen, text, images, frame lifecycle) | none |
| input (touch/key/gesture/IME/focus) | none |
| WebView | none |
| notifications / alarms / jobs | none |
| network (DNS/TLS/sockets) | none |
| media/camera/sensors/HALs | none |
| GMS / DRM / Play Integrity | none (licence/device-gated) |

---

## 5. App classes

All `none`.

| App class | Status |
|---|---|
| Plain Java/Kotlin View app | none |
| Compose app | none |
| Flutter / React Native / WebView | none |
| NDK/JNI app | none |
| ARM-only native app | none (deferred) |
| Split APK / multi-DEX | none |
| Multi-process app | none |
| SurfaceView/OpenGL/Vulkan | none |
| GMS/DRM/integrity/OEM-dependent | none — expected unsupported on native path |

---

## 6. Fixture corpus ledger

ACOS authors its own licensable fixtures (Apache-2.0). No third-party APKs are
redistributed.

| Fixture | Class | Status | Evidence |
|---|---|---|---|
| `fixtures/dexprogs/01..20` (Java console programs) | pure-Java semantics | **present**; execution not verified | sources + `jvm-reference` expected outputs; build via `tools/build-fixtures.sh` |
| `fixtures/apps/hello-views` (`dev.acos.fixtures.helloviews`) | Views Activity | **present** (APK built, debug-signed); not executed | `fixtures/apps/README.md`; `tools/build-apps.sh` |
| `fixtures/apps/hello-jni` (`dev.acos.fixtures.hellojni`) | NDK JNI (x86_64 + arm64-v8a) | **present** (APK built); not executed | `fixtures/apps/README.md` |
| Seeded-wrong variants (wrong text / crash / missing symbol) | negative/false-pass | **not built** | planned for V1 §4 |
| ART-reference captures | oracle | **empty** | `fixtures/expected-art/`, `fixtures/reference/` |

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
