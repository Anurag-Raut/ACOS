# ACOS — Compatibility ledger

**Live ledger.** Every cell starts at **`none`** and may only advance when a
reproducible evidence artifact exists under `results/` (or a merged test that
prints it) and is reviewed. **No claim may precede evidence.** This file is the
single source of truth for what ACOS can and cannot do.

Status values:

| Status | Meaning |
|---|---|
| `none` | no implementation and no evidence |
| `partial` | some cases work; failures/limits are enumerated |
| `verified` | a defined fixture set passes under a stated fidelity level, with negative cases detected |

Every non-`none` row must record: **evidence pointer**, **fidelity** (`host-proof`
/ `preview-only` / `real-apk-native-runtime` / `jvm-reference` / `art-reference`),
**API level**, **ABI**, and the **date**.

> The legacy ACVM results are *not* ACOS results and are not recorded in this
> ledger. They are baseline/oracle inputs only (`BENCHMARKS.md`,
> `docs/reference/ACVM_INVENTORY.md`).

---

## 1. DEX format and opcode families

Per ADR-0002 the parser declares DEX **035–039** as accepted *parse* versions.
"Accepted to parse" is not "verified to execute".

| Area | Parse/verify | Execute | Evidence |
|---|---|---|---|
| DEX versions 035–039 | none | none | — |
| Header/checksum/signature validation | none | n/a | — |
| String / type / proto / field / method / class tables | none | n/a | — |
| `class_data` / `code_item` / debug_info / annotations | none | n/a | — |
| Move family (`move`, `move/from16`, `move/16`, `move-wide`, `move-object`, result variants) | none | none | — |
| Constants (`const`, `const/4..16`, `const-string`, `const-class`, `const-wide`, …) | none | none | — |
| Arithmetic/logic (`add/sub/mul/div/rem/and/or/xor/shl/shr/ushr`, `neg`, `not`, …) incl. `-wide`, `-float`, `-double`, `lit*`, `2addr` | none | none | — |
| Branches (`if-*`, `if-eqz`, `goto`, `goto/16`, `goto/32`) | none | none | — |
| Switch (`packed-switch`, `sparse-switch`) | none | none | — |
| Invokes (`invoke-virtual/super/direct/static/interface`, `/range`, `/polymorphic`, `/custom`) | none | none | — |
| Instance/static fields (`iget`/`iput`/`sget`/`sput`, `-wide`/`-object`/`-boolean`/…) | none | none | — |
| Arrays (`new-array`, `filled-new-array`, `array-length`, `aget`/`aput` families) | none | none | — |
| Checks/casts/instance checks (`check-cast`, `instance-of`, `int-to-*`, `*to-*`) | none | none | — |
| Object creation (`new-instance`, `new-array`) | none | none | — |
| Monitors (`monitor-enter`, `monitor-exit`) | none | none | — |
| Exceptions / `try`/`catch` | none | none | — |
| Payloads, `nop`, `throw`, `return*`, `filled-new-array/range` | none | none | — |
| Verification against Android verifier (valid/invalid fixtures) | none | n/a | — |
| Opcode coverage report | none | none | — |

---

## 2. Java core-library surface

Strategy per ADR-0006: hand-written Rust intrinsics first, real core-library DEX
later (licence review pending).

| Package / area | Status | Evidence |
|---|---|---|
| `java.lang.Object` (identity, `equals`/`hashCode`/`toString`, monitors) | none | — |
| `java.lang.String` / `StringBuilder` | none | — |
| primitive wrappers, `Number`, `Math` (exact arithmetic) | none | — |
| `java.lang.Class` / reflection (`reflect`, annotations) | none | — |
| `java.lang.Throwable` / exceptions / stack traces | none | — |
| `java.lang.Thread` / `Runnable` / thread-local / `synchronized` semantics (JMM) | none | — |
| `java.lang.ref` (weak/soft/phantom, `ReferenceQueue`) | none | — |
| `java.util` collections (List/Map/Set) | none | — |
| `java.util.concurrent` | none | — |
| `java.io` (streams, files) | none | — |
| `java.nio` | none | — |
| `javax.*` as required by fixtures | none | — |
| class initialization ordering / static init | none | — |
| `ClassLoader` hierarchy / `findClass` / multi-DEX resolution | none | — |

---

## 3. JNI

| Area | Status | Evidence |
|---|---|---|
| `JavaVM` / `JNIEnv` function tables in Rust | none | — |
| `JNI_OnLoad` / `RegisterNatives` | none | — |
| local/global/weak references | none | — |
| strings (`GetStringUTFChars`, `NewStringUTF`, …) | none | — |
| arrays (primitive/object, `Get/Release*ArrayElements`) | none | — |
| field/method ID lookup and call-in/call-out | none | — |
| exception propagation across the boundary | none | — |
| native thread attach/detach | none | — |
| JNI version negotiation | none | — |
| ELF loading, relocations, symbol resolution, Bionic behaviours | none | — |
| ABI translation (ARM-only guests on x86_64) | none (deferred, ADR-0002) | — |

---

## 4. Android API surface

| Area | Status | Evidence |
|---|---|---|
| app/Activity lifecycle + Task/back stack | none | — |
| `Intent` / explicit + implicit resolution | none | — |
| resources / `resources.arsc` / configuration (density, theme, locale, font scale) | none | — |
| storage: files, `SharedPreferences`, SQLite | none | — |
| permissions (query/grant/deny/prompt) | none | — |
| Binder/services/content providers/broadcast | none | — |
| `View` system + layout + measurement | none | — |
| rendering (offscreen surface, text, images, frame lifecycle) | none | — |
| input (touch/key/gesture/IME/focus) | none | — |
| WebView | none | — |
| notifications / alarms / jobs / background restrictions | none | — |
| network (DNS/TLS/sockets) | none | — |
| media/camera/sensors/HALs | none | — |
| GMS / DRM / Play Integrity | none (licence/device-gated; expected `REQUIRES_GMS`/`NEEDS_REAL_DEVICE`) | — |

---

## 5. App classes

| App class | Status | Evidence |
|---|---|---|
| Plain Java/Kotlin View app (Activity) | none | — |
| Compose app | none | — |
| Flutter app | none | — |
| React Native app | none | — |
| WebView app | none | — |
| NDK/JNI app (x86_64 `.so`) | none | — |
| ARM-only native app | none (deferred) | — |
| Split APK / multiple DEX | none | — |
| Multi-process app (providers/services) | none | — |
| SurfaceView/OpenGL/Vulkan | none | — |
| Apps relying on GMS/DRM/integrity/OEM | none — expected unsupported on native path | — |

---

## 6. Fixture corpus ledger

ACOS starts with no fixtures. The legacy corpus is inventoried in
`docs/reference/ACVM_INVENTORY.md` §3, but its third-party APKs are **not in that
repo** and its helper APKs use a research platform key. New fixtures are
self-authored, licensable and built with SDK build tools (ADR-0004).

| Fixture | Class | Source | Status | Evidence |
|---|---|---|---|---|
| *(none yet)* | — | — | — | — |

Minimum corpus to target (from `docs/handoff/01`): Java Views Activity; background
worker; app lifecycle/process death; Compose navigation + Material + fonts +
animations; Flutter; React Native; WebView; multi-process + provider + receiver;
NDK JNI (x86_64 and ARM-only); multimedia/SurfaceView/GL/Vulkan; camera/mic
permission flows; launch modes/affinities/document mode/noHistory; split APKs /
multi-DEX. Each needs a known-good and a seeded-wrong variant with an
independent oracle.

---

## 7. How to advance a cell

1. Add a fixture (own source, licence recorded) and a stated fidelity level.
2. Implement the smallest slice that executes it on **real compiled DEX**.
3. Add a differential/oracle test (`jvm-reference` first; `art-reference` only
   when authorised) and a negative case.
4. Run it, capture a reproducible artifact under `results/`.
5. Update exactly one ledger row with status, evidence pointer, fidelity, API
   level, ABI and date. Record negative results in `EXPERIMENTS.md` too.

A `verified` cell requires the negative case to be detected. A launch that does
not crash is **not** functional compatibility.
