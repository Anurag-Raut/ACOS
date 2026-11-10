# ACOS — Status

**As of 2026-11-10** (after commits `5945a57` "Wave 1" and `1da205f` "Research:
real framework DEX feasibility"). Honest, dated snapshot. `TASKS.md` is the
plan; `docs/V1.md` is the V1 definition; `docs/FINDINGS.md` is the findings log;
`EXPERIMENTS.md` is the history.

## One line

**Static analysis works and is verified; nothing executes yet.** An agent can
`acos inspect` a real APK and get a typed report, and the DEX parser/verifier/
decoder matches `dexdump` on the committed fixtures. No DEX is interpreted on the
real core library, no framework bytecode is booted, no Activity runs, no frame is
rendered, and no JNI library is loaded. Execution status changes only when the
`acos-vm` C1 gate is accepted (20 dexprogs on DEX).

## What is verified (with evidence)

| Capability | State | Evidence |
|---|---|---|
| Static APK inspect (`acos inspect --json`) | **verified** | `crates/acos-cli`, `crates/acos-apk`; live run of the prebuilt binary produced a correct report (package, versionCode/Name, minSdk 26, targetSdk 34, launcher activity, `classes.dex` present) |
| DEX 035–039 parse + structural verify | **verified** (fixtures); boot-classpath env-gated pending | `crates/acos-dex`; `docs/FINDINGS.md` F-0002 |
| DEX decode/disassembly vs `dexdump` | **verified** on committed fixtures | `crates/acos-dex/tests/diff_dexdump.sh` → `files=3 methods=380 instructions=4253 mismatches=0`; `docs/FINDINGS.md` F-0004 |
| APK manifest/`aapt2` differential | **verified** (committed test) | `crates/acos-apk/tests/differential_aapt2.rs` |
| Real boot-classpath DEX obtainable unprivileged | **verified** (v039, unstripped) | `research/framework-dex/REPORT.md`; `tools/extract-bootclasspath.sh`; `.cache/boot/android-34/`; `docs/FINDINGS.md` F-0002 |
| ART oracle emulator boots headless | **verified once** | `results/oracle/emulator-start.log` → `BOOTED in 22s wall clock`; emulator now shut down (`adb devices` empty) |
| 20 dexprogs with `jvm-reference` expected outputs | **present** (weak tier) | `fixtures/dexprogs/`, `fixtures/expected/dexprogs/` |
| hello-views / hello-jni APK fixtures | **present** (built, debug-signed) | `fixtures/apps/`, `fixtures/build/apps/` (gitignored) |
| Artifact-reference captures | **empty** | `fixtures/expected-art/`, `fixtures/reference/` |

## What does NOT work yet

| Capability | State |
|---|---|
| DEX **execution** (interpreter) | not accepted (`acos-vm` scaffold, 0 tests at time of writing) |
| Real core-library boot / `println` path | not started |
| Framework boot / service layer | not started |
| Activity launch / lifecycle | not started |
| Rendering / UI tree / input | not started (`acos-render` placeholder) |
| JNI / native ELF loading | not started (`acos-native-loader` incomplete) |
| `acos verify` / scenario runner | not started |
| Snapshot/restore | planned, unproven (ADR-0010) |
| Concurrency / isolation | not started |
| Benchmarks vs legacy | not started (no ACOS row exists) |

Every `COMPATIBILITY.md` row except DEX parse/decode is `none`. There is no
verified Android runtime capability and no benchmark result.

## Working tree / concurrency

`git log` shows two commits (`5945a57`, `1da205f`). Concurrent subagents have
uncommitted work in `acos-dex` (hiddenapi + boot-classpath + lazy-table edits),
new crates (`acos-vm`, `acos-native-loader`, `acos-render`) and oracle tooling
(`tools/oracle/`, `tools/extract-bootclasspath.sh`, `results/`). At the moment of
reading, the **working tree did not compile** (`acos-dex` lazy-table edits, 14
errors), so the last committed state is the verified one. This is reported, not
hidden.

## Level

| Level | Available? |
|---|---|
| H0 host tooling inside ACOS | **partial** — static APK/DEX analysis only (no build/lint) |
| H1 host renderer | no |
| H2 native Rust runtime | no |
| H3 isolated native runtime | no |
| H4 ART oracle | tooling exists and booted once; references not yet captured |

## Decisions in force

Owner directives D1–D4 (`SPEC.md`) and ADR-0001…ADR-0017 (`DECISIONS.md`), in
particular:

- **ADR-0007** execute the real Android 14 boot-classpath bytecode; do not
  reimplement the framework.
- **ADR-0008** replace everything below/around the framework with thin Rust (no
  system_server/Binder/SurfaceFlinger/HWUI/zygote).
- **ADR-0009** handle/arena heap, VM frame stack, precise GC, one class provider.
- **ADR-0010** snapshot/restore is the planned start/reset strategy (unproven).
- **ADR-0011** Rust software 2D rendering; no GPU/display unless asked.
- **ADR-0014** Rust native loader/JNI; structured `UNSUPPORTED_NATIVE_SYMBOL`.
- **ADR-0015** ART oracle = installed API 34 x86_64 emulator, headless on demand,
  ≤2 GiB, one instance.
- **ADR-0016** runtime-only boot-jar sourcing; licence review open before
  distribution.
- **ADR-0017** DEX SHA-1 accepted by default (Android semantics); Adler-32
  enforced.

## Next checkpoint (C1)

The `acos-vm` C1 gate: the 20 `dexprogs` execute on DEX (temporary Rust bootstrap
built-ins), matching `fixtures/expected/dexprogs`, normal and GC-stress, with a
recorded artifact. Until then, `STATUS.md` keeps saying **nothing executes yet**.

## Blocked / open

- **ADR-0016 licence review** (human/legal) before any distribution; does not
  block development.
- **Production-image dex stripping** untested (only the `userdebug` image was
  available); could block the runtime-only boot-jar posture on production images.
- **`android-34` framework sources** not installed (only `android-36`), so
  source-line references are indicative, not byte-exact.
