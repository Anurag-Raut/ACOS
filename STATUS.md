# ACOS — Status

**As of 2026-11-10, HEAD `9426b5b` + working tree** (50 commits; checkpoint
`ced0d70`/`40f075c`/`e64897f`). Honest, dated snapshot. `TASKS.md` is the plan;
`docs/V1.md` is the V1 definition; `docs/FINDINGS.md` is the append-only
findings log; `EXPERIMENTS.md` is the history; `BENCHMARKS.md` §8 and
`results/bench/REPORT.md` are the measured benchmark record.

## One line

**Console execution is real and verified against real ART; framework boot
reaches `bindApplication` and a real `ClientTransaction`; the first
Application/Activity callback, frame and input are not yet observed.** The
Rust VM runs the real Android 14 core-library bytecode, the real boot-classpath
DEX is extracted unprivileged, `acos inspect`/`run-dex`/`verify` exist, and the
Rust service layer (`acos-services`, `acos-wm`) and renderer (`acos-render`)
have their own acceptance tests. Everything downstream of Activity launch —
lifecycle callbacks, frames, UI tree, input, reset, V1 verdict — is unverified.

## Milestone table (M0..M5 + V1 gate)

The named gates come from `research/service-layer/BACKLOG.md` §"Milestone
summary" (M1..M5); M0 is the P0/P1 static-analysis foundation in
`docs/handoff/04_ROADMAP_AND_ACCEPTANCE.md`.

| Gate | Requirement | State | Evidence |
|---|---|---|---|
| **M0** static APK/DEX analysis | parse/verify/decode APK + DEX; differential vs `dexdump`/`aapt2` | **done** (static-analysis tier) | `crates/acos-dex/tests/diff_dexdump.sh` → 3 files / 380 methods / 4,253 insns / 0 mismatches (`docs/FINDINGS.md` F-0004); `crates/acos-apk/tests/differential_aapt2.rs`; `docs/FINDINGS.md` F-0002 |
| **M1** println on the real core library | console programs execute on real `core-oj`/`core-libart`/`core-icu4j` DEX and match the ART oracle | **done** (art-reference) | `results/m1/README.md` (20/20 byte-identical, normal + GC-stress); `results/vm/OS_NATIVES_AUDIT.md` §3 (`--corelib real`: 25 pass / 0 fail / 0 unsupported of 25); `results/vm/dexprogs-real-normal.json` |
| **M2** `Application` + `Activity.onCreate` | `bindApplication` delivered; lifecycle `onCreate→onStart→onResume` matches ART | **partial** | `bindApplication` delivered + `ClientTransaction` scheduled (`results/m2/M2_LOG.md`; `results/m2/boot-release.json`); M2 acceptance test `#[ignore]`d/red (`app_created=false`, `tx_delivered=false`); `crates/acos-runtime/tests/m2.rs` |
| **M3** first frame in `acos-render` | real non-black frame with `frame_seq`/cause/hash from a launched Activity | **partial** | renderer standalone verified: 88 tests, text advance within 0.3% of the ART UI dump (`results/render/README.md`); frame handoff `hardwareRenderer→compositor` added in the working tree (`crates/acos-runtime/src/frame_handoff.rs`); **no framework-bound frame yet** (M3 gate not met) |
| **M4** input + UI tree | `ui_initial.json` equality; tap → `clicks: 1` | **not started** (gate); pieces partial | input natives + WMS input dispatcher have unit coverage (`results/wm/README.md`); UI tree is protocol-only (`UiNode`/`UiTree`); no Activity runs, so no tap→state evidence |
| **M5** interaction trace | `lifecycle-summary.txt` + `ui_after_*` + native-diff | **not started** | depends on M2–M4 |
| **V1 gate** `acos verify` full verdict | `acos verify --apk … --scenario … --json --artifacts …` passes on the two fixtures with no Android boot | **not started** | runner exists (`crates/acos-verify`, `scenarios/`), but the native backend stops at the first UI step (`docs/DEV_WORKFLOW.md` §9; `scenarios/README.md`) |

## What is verified vs claimed (tier labels)

Fidelity tiers are defined in `DECISIONS.md` ADR-0005 (updated by ADR-0015).

| Tier | Meaning | Evidence in repo |
|---|---|---|
| `jvm-reference` (weak) | host OpenJDK 17 output for the same source | `fixtures/expected/dexprogs/` (51 files) |
| `art-reference` (authoritative) | real ART on the API-34 `google_apis` x86_64 emulator | `fixtures/expected-art/dexprogs/` (75 files, 25 programs × stdout/exit/stderr); `fixtures/reference/{hello-views,hello-jni,counter,todo-list,multi-activity,form-validation}/art-api34-x86_64/`; `results/oracle/jvm-vs-art.md`; `results/oracle/dexprogs-runs.json` |
| `physical-device` | real phone capture | **captured, not interchangeable** — `fixtures/reference-device/` (moto g35 5G, Android 15/API 35; `DEVICE.md`/`COMPARISON.md`, commit `50f2f5f`). Kept separate from the API-34 emulator `art-reference`; density/API differ. |

**Verified (reproducible artifact exists):** DEX decode (`dexdump` differential),
APK manifest/AXML (`aapt2` differential), M1 console corpus on real core libs
(`results/m1/README.md`, `results/vm/OS_NATIVES_AUDIT.md`), ART-reference
captures for 6 apps, `acos-render` text metrics vs the ART UI dump
(`results/render/README.md`), `acos-wm`/`acos-services` acceptance suites
(`results/wm/README.md`, `results/services/REPORT.md`), snapshot/restore PoC
16/16 tests and 23/23 real dexprogs (`results/snapshot/REPORT.md`), the ELF/JNI
native loader (`results/native-loader/README.md`), `hello-jni` output matching
the ART reference (`crates/acos-jni/tests/hello_jni.rs`), and the measured
benchmark rows (`results/bench/REPORT.md`).

**Claimed / not yet verified:** Activity lifecycle order; any rendered frame from
a real Activity; UI tree; input; reset; concurrency with independent state;
`verified tasks / GiB-minute`; any end-to-end `acos verify` pass. The
`physical-device` tier is captured but only as a reference, not yet run as a
differential fixture.

## What runs

- **Console corpus:** 25 `fixtures/dexprogs` execute on the real Android 14 core
  library (`--corelib real`). The original 20-program M1 gate is byte-identical
  to `fixtures/expected-art` normal and GC-stress (`results/m1/README.md`);
  W25-B records **25 pass / 0 fail / 0 unsupported** (`results/vm/OS_NATIVES_AUDIT.md` §3).
  The committed `results/vm/dexprogs-real-normal.json` (at HEAD `40f075c`) still
  records **24/25** (`25_security_digest` fails) and
  `results/vm/dexprogs-real-gc-stress.json` records 23/23; the committed artifacts
  lag the working tree.
- **Opcode coverage:** 123 DEX opcodes executed, 95 implemented but unexercised,
  6 unsupported (`invoke-polymorphic`, `invoke-custom`, `const-method-handle`,
  `const-method-type`) — `results/vm/opcode-coverage.json`.
- **Framework boot:** real `ActivityThread` comes up, `attachApplication` is
  received by the Rust ATMS, `bindApplication` is delivered, and a real
  `ClientTransaction` is scheduled (`results/m2/M2_LOG.md`). No
  `Application.onCreate`/`Activity.onCreate` yet.
- **Bench (native-runtime fidelity, not ART-verified), from
  `results/bench/REPORT.md` §1 (p50/p95):** process spawn → Vm ready
  16.929/18.229 ms; `Vm::new` 1.34/1.636 ms; boot → attachApplication
  13.052/14.933 ms; attach → bindApplication delivered 2.311/2.732 ms; idle core
  (Vm only) 26,984 KiB anon+shmem; booted framework 161,384 KiB anon+shmem
  (PSS 166,739 KiB); N=10 sessions 293,612 KiB anon+shmem; marginal N=5→10
  14.314 MiB/session; parked idle CPU 0 ticks / 5 s. Interpreter (authoritative
  `results/bench/REPORT.md` §1, run `205045`): `02_int_arith` 16.048 ms /
  3.86 M insn/s; `05_branches_loops` and `09_strings` per that table. Earlier
  runs quote 6.361 ms / 9.8 M (run `201637`) and `BENCHMARKS.md` §8.1 quotes
  22.8 ms / 2.73 M (run `191849`); the spread reflects host load (loadavg 4.8),
  so no single best-of-three number is the claim. Full per-metric legacy
  comparison in §"Per-metric vs legacy" below.

## What is unsupported (`UNSUPPORTED_JAVA_API` sites, by area)

Unregistered natives and explicit `unsupported_api` sites raise a structured
`UNSUPPORTED_JAVA_API` naming `Lclass;->name(sig)` — never a fake result.

| Area | Unsupported surface | Source |
|---|---|---|
| Graphics `android.graphics` | unregistered Canvas/Paint/Path/Matrix/Bitmap/Region/Text/HWUI entry points | `crates/acos-vm/src/natives/gfx_{canvas,paint,path,matrix,bitmap,text,hwui}.rs` |
| Fonts | `Font.nNewByteBuffer`, `Typeface.nativeReadTypefaces/nativeWriteTypefaces`; variable-font axes shape but the A8 mask renders the default instance | `crates/acos-vm/src/natives/fwk_fonts.rs`; ADR-0019 |
| ICU regex | constructs that cannot be matched exactly raise instead of silently mis-matching | `crates/acos-vm/src/natives/icu_regex.rs` |
| Parcel | ashmem blob (`nativeReadBlob`); unknown codec fields | `crates/acos-vm/src/natives/parcel.rs` |
| Native memory | `MAP_RW`/`MAP_PV` are private copies (no write-back) | `crates/acos-vm/src/natives/native_mem.rs`, `fwk_m2_filechannel.rs` |
| JNI / OAT | OAT introspection natives unregistered; JNI library loader raises | `crates/acos-vm/src/natives/fwk_dexfile.rs`, `jni_glue.rs` |
| `libcore.io.Linux` | 134 declared: 64 implemented, 9 pending, 61 unsupported-by-design (process control, networking, `mmap`, xattrs) | `results/vm/OS_NATIVES_AUDIT.md` §2 |
| Reflection | non-empty `Constructor.newInstance(Object[])` | `crates/acos-vm/src/natives/fwk_reflect.rs` |
| Vsync | `DisplayEventReceiver.nativeInit` on a dead `MessageQueue` | `crates/acos-vm/src/natives/fwk_vsync.rs` |
| Conscrypt | `SSL_*`/TLS, `EVP_PKEY`/`RSA_*`/`EC_*`/`X509_*`, AES-GCM | `crates/acos-vm/src/natives/fwk_conscrypt.rs` |
| Framework services | one process-global Binder transport → N sessions are a memory experiment, not N isolated workers | `results/bench/MEMORY_PROFILE.md` §5 |

## Known risks

- **Interpreter slower than `java -Xint`.** Final `tools/bench-vm.sh`
  comparison (`crates/acos-vm/PERF.md` "Final comparison"): BenchFib 1.55×
  slower, BenchLoop 4.13× slower, BenchAlloc 1.73× slower; BenchString 3.30×
  faster. vs JIT it is 4–42× slower (no JIT, by design).
- **Alloc/Loop speed targets missed.** Target was ≥4× Fib (met, 4.17×), ≥4×
  Alloc (3.58×) and ≥5× Loop (3.48×) — not met (`crates/acos-vm/PERF.md`).
- **ICU regex approximations.** Approximated constructs are a fidelity risk for
  Android-semantics regex (`crates/acos-vm/src/natives/icu_regex.rs`).
- **Variable fonts.** Axes are stored/applied to shaping, but the FreeType A8
  mask path renders the default instance (`results/m2/M2_LOG.md` W23-A;
  ADR-0019).
- **`OsConstants` in progress.** W25-B reports 538/538 `OsConstants` fields and
  a 64/9/61 `libcore.io.Linux` split in the working tree
  (`results/vm/OS_NATIVES_AUDIT.md`); not yet committed at HEAD.
- **M2 display/frame work uncommitted.** The real `DisplayInfo` parcel codec and
  APEX boot-classpath merge exist in the working tree (`display_java.rs`,
  `tests/display.rs`, `tests/boot_classpath.rs`) with no recorded full-lifecycle
  run.
- **Committed artifacts lag.** `results/vm/dexprogs-real-normal.json` (24/25)
  and `results/bench/baseline.json` (commit `fad5534`) predate the current tree.

## Fixture-dependent tests in CI

`fixtures/build/` and `.cache/boot/` are git-ignored, so differential tests
silently `SKIP` on a fresh clone. Setting `ACOS_REQUIRE_FIXTURES=1` turns every
skip into a **failure** (`crates/acos-vm/tests/fixtures_required.rs`);
`tools/diff-dexprogs.sh` and `tools/bench-acos.sh` set it by default. CI must run
`tools/build-fixtures.sh` / `tools/build-apps.sh` and the boot-cache extractor
first (`HONESTY_AUDIT.md` #11).

## Per-metric vs legacy — `results/bench/REPORT.md` §2/§3

Numbers below are copied from `results/bench/REPORT.md` (run
`run-2026-11-10-201637.json`, commit `ced0d70`). Legacy values are ACVM
reference only; no legacy value is an ACOS result.

| Row | Legacy (ACVM, reference) | ACOS | Verdict |
|---|---|---|---|
| M1 fixed idle core | 206.0 MiB (W1) / 303.9 MiB (v9); reads 259–322 MiB | 157.6 MiB booted / 26.4 MiB Vm-only | `better (-23.5%)` |
| M2 incremental N=5 | 78.7 MiB (v9) / 33.2 MiB (W2) | 43.032 MiB/session | `better (-45.3%)` |
| M3 cold → first frame | 3.620 s (v12) | not measured | `not-measurable` |
| M4 worker → frame | 1.251–1.343 s p50 | not measured | `not-measurable` |
| M5 fresh screenshot | 0.066–0.067 s p50 | not measured | `not-measurable` |
| M6 reset-to-ready | 110.3 ms / 748.1 ms | not measured | `not-measurable` |
| M7 UI tree with text | 538–768 / 4.1 / 14–87 ms | not measured | `not-measurable` |
| M8 action-to-state | not cleanly published | not measured | `not-measurable` |
| M9 concurrent workloads | N=5 scorecard, N=10 | memory measured (N=10 286.7 MiB; marginal 14.314 MiB/session); independent state **not** | `measured` (memory only) |
| M10 end-to-end verify | ~19 s install-bound | not measured | `not-measurable` |
| M11 tasks / GiB-minute | not published | not measured | `not-measurable` |
| L-idle-core | 278 MiB anon | 26.4 MiB Vm-only anon+shmem | `better (-90.5%)` |
| L-session-n1 | 130.3 MiB | 157.602 MiB/session | **`worse (+21.0%)`** |
| L-marginal N=5→10 | ~99.3 MiB workset | 14.314 MiB/session | `better (-85.6%)` |
| L-guest-boot | 1.517 s `boot_completed` | 0.013 s attach handshake | different thing (no guest boot) |
| L-idle-cpu | 3.86–5.94% (unreliable) | 0.0% of one core | `measured` |
| A-process-start | no legacy metric | 16.929 ms | `no-legacy-baseline` |
| A-attach-bind | no legacy metric | 2.311 ms | `no-legacy-baseline` |

**Worse-than-legacy list (explicit):** only `L-session-n1` (N=1 per-session
memory) — the first session pays the whole boot classpath; plan = finish the
zygote-like template process (see `results/bench/MEMORY_PROFILE.md` §5).
The legacy units differ (workset vs anon+shmem) on several rows; only
same-type/same-state pairs are called `better`/`worse`.

## Next 10 tasks

1. Land the working-tree display parcel (`DisplayInfo.writeToParcel`) + APEX
   boot-classpath merge, then record a full M2 lifecycle run with
   `app_created`/`tx_delivered` evidence (`crates/acos-runtime/src/display_java.rs`,
   `tests/display.rs`, `tests/boot_classpath.rs`).
2. Close the M2 gate: `Application.onCreate` + `MainActivity`
   `onCreate→onStart→onResume` matching
   `fixtures/reference/hello-views/art-api34-x86_64/lifecycle-summary.txt`.
3. Bind `HardwareRenderer` → `frame_handoff` → completed-frame store and wire
   `acos-render` to the graphics natives; record the M3 `frame_seq=1` non-black
   frame (`crates/acos-runtime/src/frame_handoff.rs`).
4. Implement input injection + the UI-tree bridge (`UiNode`/`UiTree`) and
   record the M4 tap → `clicks: 1` transition.
5. Finish the framework file/`/system` I/O substrate (FileChannel, `/system`
   fonts/resources) so `handleBindApplication` runs without a structured block.
6. Commit the W25-B `OsConstants` + `libcore.io.Linux` work and clear the 9
   pending Linux natives (`results/vm/OS_NATIVES_AUDIT.md` §2).
7. Cover ICU regex constructs that currently raise, or record each as a declared
   divergence when it cannot change fixture output.
8. Give each session its own Binder transport so N workers are independently
   usable (M9/V1 §5.1; `results/bench/MEMORY_PROFILE.md` §5).
9. Extend the benchmark harness to M3–M11 and record every tracked metric with
   ≥20 reps p50/p95; keep the regression gate (`tools/bench-acos.sh --check`).
10. Run the `scenarios/negative/*` suite (and a differential against the now
    captured `fixtures/reference-device/`) to prove zero false passes before any
    V1 verdict claim.

## Working tree / concurrency

HEAD `9426b5b` (the honesty/host-safety audit) is the committed checkpoint. The
working tree carries uncommitted wave work: W33-A (reflection fields, exception
floor, lifecycle), W28-A (interpreter/dispatch/vm performance), W34-B
(`acos-cli` session/launch, `acos-mcp`) and W35-A (the audit fixes: guest-sized
allocation bounds, parcel counts, AppOps/UserManager grants, guest environment,
surface decode errors, green-thread `park`/`unpark`, native-loader path
confinement). Shared files (`heap.rs`, `reflect.rs`, `interp.rs`, `vm.rs`) were
edited in place; concurrent agents re-read before editing. Validate on a scratch
worktree if the shared tree does not build.

## Decisions in force

Owner directives D1–D5 (`SPEC.md`, `docs/FINDINGS.md`) and ADR-0001…ADR-0019
(`DECISIONS.md`), in particular ADR-0007 (execute the real boot-classpath
bytecode), ADR-0008 (thin Rust around the framework), ADR-0009 (handle/arena
heap, precise GC), ADR-0010 (snapshot/restore), ADR-0014 (Rust JNI/ELF),
ADR-0016 (boot-jar sourcing; licence review open), ADR-0017 (Android DEX
loading semantics), ADR-0018 (host RAM ceiling), ADR-0019 (FreeType/HarfBuzz
text).

## Blocked / open

- **ADR-0016 licence review** (human/legal) before any distribution; does not
  block development.
- **Production-image dex stripping** untested (only `userdebug` available).
- **`android-34` framework sources** not installed (only `android-36`), so
  source-line references are indicative.
- **`physical-device` differential** not run: the capture exists
  (`fixtures/reference-device/`) but no scenario compares against it yet.
