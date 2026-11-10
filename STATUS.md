# ACOS — Status

**As of 2026-11-11, HEAD `addd4e2`** ("Typed-text path …, on-disk boot image
cache (warm 94→66 ms), dogfood wave 3"). `git log` is authoritative. This
snapshot was rewritten after the V1 benchmark at `addd4e2`; **`BENCHMARKS.md` §9
is the authoritative measured table** and `results/bench/v1-2026-11-11-0214.json`
is its raw artifact. `TASKS.md` is the plan; `docs/V1.md` is the V1 definition;
`docs/FINDINGS.md` is the append-only findings log; `EXPERIMENTS.md` is history.

## One line

**Console execution is real and verified against real ART; framework boot
delivers `bindApplication`, `Application.onCreate` runs on 3/5 samples, and the
launch reaches `MainActivity.onCreate` (blocked in
`VectorDrawable.nCreateGroup`), but the launch `ClientTransaction` is delivered
0/5, so `onStart`/`onResume`, the first frame and input are not observed.** The
Rust VM runs the real Android 14 core-library bytecode, the real boot-classpath
DEX is extracted unprivileged, `acos inspect`/`run-dex`/`verify` exist, and the
Rust service layer (`acos-services`, `acos-wm`), the SQLite engine
(`acos-sqlite`), the confinement layer (`acos-sandbox`) and the renderer
(`acos-render`) have their own acceptance tests. The V1 benchmark at `addd4e2`
beats legacy on memory only (**2 PASS / 9 FAIL** vs owner directive D2; see
`BENCHMARKS.md` §9); everything downstream of `Activity.onCreate` — the rest of
the lifecycle, frames, UI tree, input, reset, V1 verdict — is unverified.

## Milestone table (M0..M5 + V1 gate)

The named gates come from `research/service-layer/BACKLOG.md` §"Milestone
summary" (M1..M5); M0 is the P0/P1 static-analysis foundation in
`docs/handoff/04_ROADMAP_AND_ACCEPTANCE.md`.

| Gate | Requirement | State | Evidence |
|---|---|---|---|
| **M0** static APK/DEX analysis | parse/verify/decode APK + DEX; differential vs `dexdump`/`aapt2` | **done** (static-analysis tier) | `crates/acos-dex/tests/diff_dexdump.sh` → 3 files / 380 methods / 4,253 insns / 0 mismatches (`docs/FINDINGS.md` F-0004); `crates/acos-apk/tests/differential_aapt2.rs`; `docs/FINDINGS.md` F-0002 |
| **M1** println on the real core library | console programs execute on real `core-oj`/`core-libart`/`core-icu4j` DEX and match the ART oracle | **done** (art-reference) | `results/m1/README.md` (20/20 byte-identical, normal + GC-stress); `results/vm/OS_NATIVES_AUDIT.md` §3 (`--corelib real`: 25 pass / 0 fail / 0 unsupported of 25); `results/vm/dexprogs-real-normal.json` |
| **M2** `Application` + `Activity.onCreate` | `bindApplication` delivered; lifecycle `onCreate→onStart→onResume` matches ART | **partial** | `bindApplication` delivered and `Application.onCreate` runs (3/5 at `addd4e2`, `BENCHMARKS.md` §9.1 M3), and the launch reaches `MainActivity.onCreate` (`results/m2/M2_LOG.md`; `results/m2/boot-release.json`); it then blocks in `VectorDrawable.nCreateGroup`, the launch `ClientTransaction` is delivered 0/5, and the full `onStart`/`onResume` order is unverified; `crates/acos-runtime/tests/m2.rs` |
| **M3** first frame in `acos-render` | real non-black frame with `frame_seq`/cause/hash from a launched Activity | **partial** | renderer standalone verified: 88 tests, text advance within 0.3% of the ART UI dump (`results/render/README.md`); frame handoff `hardwareRenderer→compositor` exists (`crates/acos-runtime/src/frame_handoff.rs`); the V1 run measured a standalone hello-views cold frame at **2.54 ms**, SSIM 0.9938 (`BENCHMARKS.md` §9.1 M3/§9.2); **no framework-bound frame yet** (M3 gate not met) |
| **M4** input + UI tree | `ui_initial.json` equality; tap → `clicks: 1` | **not started** (gate); pieces partial | input natives + WMS input dispatcher have unit coverage (`results/wm/README.md`); UI tree is protocol-only (`UiNode`/`UiTree`); no Activity runs, so no tap→state evidence |
| **M5** interaction trace | `lifecycle-summary.txt` + `ui_after_*` + native-diff | **not started** | depends on M2–M4 |
| **V1 gate** `acos verify` full verdict | `acos verify --apk … --scenario … --json --artifacts …` passes on the two fixtures with no Android boot | **not started** | runner exists (`crates/acos-verify`, `scenarios/`), but the native backend stops at the first UI step (`docs/DEV_WORKFLOW.md` §9; `scenarios/README.md`). The V1 benchmark measures the gap: `BENCHMARKS.md` §9.1 M10 |

## V1 benchmark (2026-11-11, `addd4e2`)

`BENCHMARKS.md` §9 is the authoritative measured table; raw per-round samples in
`results/bench/v1-2026-11-11-0214.json` (release, 5 rounds, loadavg 4.58–4.73,
42 205/61 908 MiB available). Reproduce with `tools/bench-v1.sh --rounds 5`.

Against owner directive D2 ("beat every legacy-tracked metric"): **2 PASS /
9 FAIL**. The two passes are memory — M1 fixed idle core **171.97 MiB** vs
206.0 MiB (W1) and M2 N=5 per-session **56.85 MiB** vs 78.7 MiB (v9). The nine
failures are the unmeasured capabilities (M3–M8, M10, M11) plus M9 (memory
measured, independent state not proven). ACOS-native launch at the same commit:
cold process→`Vm` **16.43 ms** p50, framework boot→attach **13.60 ms**,
`Vm::restore` **6.97 ms**, before/after launch **42.70 → 29.85 ms** (1.43×);
SessionPool create **1.58 ms** and reset **2.08 ms** (N=5); N=20 pool 105.1 MiB
total, 3.79 MiB/session marginal. VM throughput is behind `java -Xint` on 10/12
`Bench*` programs (only `BenchString` 0.205×; worst `BenchGc` 3.162×,
`BenchLoop` 2.111×). Renderer: cold hello-views frame 2.54 ms, warm 0.79 ms,
PNG `FastFiltered` 7.01 ms; `frame-diff` 280.4 ms SSIM 0.9938. MCP `tools/call`
5 ms p50 vs 2 ms bare CLI (3 ms server overhead); daemon ping reuse 0.009 ms vs
50.2 ms process spawn. The three noisy rows (spread > 15 %) are
`observe_tree_ms`, `frame_diff_ms`, `apk_inspect_ms`.

**Test counts.** 985 `#[test]`/`#[tokio::test]` functions are declared across
`crates/` at `addd4e2`; the full suite was **not** re-run in this V1 wave (the
tree was shared with other agents). Recorded green suites include `acos-mcp`
50/50 (`results/mcp/cargo-test.txt`), `acos-vm` snapshot 16/16 and the new
boot-image-cache 10 unit + 3 integration, `acos-runtime` session_pool 4/4, and
`tools/diff-dexprogs.sh --corelib real` 27/27 normal and GC-stress.

### Known gaps (V1)

1. **M2 Activity chain — `VectorDrawable`.** The launch reaches
   `MainActivity.onCreate` but
   `Landroid/graphics/drawable/VectorDrawable;->nCreateGroup()J` is
   unimplemented, so the launch `ClientTransaction` is 0/5 and
   `onStart`/`onResume`/first frame never happen. Working-tree WIP:
   `crates/acos-vm/src/natives/gfx_vector.rs` (uncommitted). Evidence:
   `BENCHMARKS.md` §9.1 M3; `results/m2/M2_LOG.md`.
2. **Renderer not framework-bound.** `acos-render` produces a correct
   hello-views frame (cold 2.54 ms, SSIM 0.9938 vs the ART capture) but no
   launched Activity publishes it through `frame_handoff`. Evidence:
   `results/render/README.md`, `results/render/M3_LOG.md`.
3. **UI tree / input / reset unimplemented.** `observe --tree` returns
   `not_ready`; `UiNode`/`UiTree` are protocol-only; no input injection; pool
   reset is core-VM-only. Evidence: `BENCHMARKS.md` §9.1 M6–M8.
4. **M9 not independent.** One process-global Binder transport: N sessions are a
   memory/throughput experiment, not N isolated workers.
5. **RN bridge.** Hermes/JNI confined-stack probes exist (`results/rn/`,
   `tools/rn-*`), but no React Native hello publishes a frame.
6. **Flutter first frame.** The Dart `main` executes in the confined engine
   (`results/flutter/FIRST_RUN.md`) but no Java `Surface`/`ANativeWindow` is
   delivered, so no first frame.
7. **IR tier default-off.** `ACOS_EXEC=ir` wins 2–2.7× on dispatch loops but is
   not default; `BenchGc`/`BenchAlloc` stay allocation-bound.
   Evidence: `BENCHMARKS.md` §8.9–§8.11.
8. **Committed artifacts lag.** Several `results/` files predate `addd4e2`;
   `results/vm/dexprogs-real-normal.json` (24/25) and
   `results/bench/baseline.json` (commit `fad5534`) are stale.

## Landed since the audit baseline (`9426b5b`, W67-A)

| Area | What landed | Evidence |
|---|---|---|
| **M2 Activity.onCreate** | real provider binder, `Path.approximate` triples, conditional parcelable codec and `@null` theme refs; launch reaches `MainActivity.onCreate` (`ad9c978`, `f84bd13`) | `results/m2/M2_LOG.md`, `crates/acos-runtime/tests/m2.rs` |
| **SQLite** | safe `acos-sqlite` engine (rusqlite + vendored C) behind the real `android.database.sqlite` natives; fixture 28 matches ART (`9114c58`) | `crates/acos-sqlite/tests/engine.rs`, `results/vm/W49A_SQLITE.md` |
| **Sandbox** | seccomp/Landlock/rlimits/namespaces confinement, fail-closed, loader requires confinement; hostile-process harness (`83d814d`) + W67-A hardening (extended seccomp posture, ABI-6 signal/abstract-socket scopes, hygiene-gated `confinement_active`) | `crates/acos-sandbox/tests/confine.rs`, `crates/acos-sandbox/src/linux/*` |
| **NDK** | `libandroid` surface: assets, bitmap, `ANativeWindow`, `ALooper`, `AChoreographer`, `AConfiguration`; GL traps (`a62c009`) | `crates/acos-jni/src/ndk_host.rs` (working tree), `fixtures/jni` |
| **JNI broker** | 227/233 `JNINativeInterface` slots with documented reasons; `CallNonvirtual*`, native OS-thread attach/detach (`b4d8a7b`) | `results/jni/JNI_COVERAGE.md`, `crates/acos-jni/tests/` |
| **Render corpus** | 12 hand-authored DisplayList/Canvas scene reconstructions vs ART API-34 captures (SSIM 0.948-0.995 on the widget corpus); geometry/colours were **measured off the reference** and per-scene thresholds are **calibrated post-hoc**, so this validates the rasteriser, not a VM-drawn frame (W94-A #12) | `results/render/W66A_CORPUS.md`, `results/render/W82A_CORPUS.md`, `results/render/corpus/` |
| **dexprogs** | 27/27 vs real ART (`--corelib real`, normal) | `results/vm/dexprogs-w45a-real.json`, `results/vm/dexprogs-real-normal.json` |

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
  The committed `results/vm/dexprogs-real-normal.json` (from the `40f075c` run)
  still records **24/25** (`25_security_digest` fails) and
  `results/vm/dexprogs-real-gc-stress.json` records 23/23; the committed artifacts
  lag the working tree.
- **Opcode coverage:** 123 DEX opcodes executed, 95 implemented but unexercised,
  6 unsupported (`invoke-polymorphic`, `invoke-custom`, `const-method-handle`,
  `const-method-type`) — `results/vm/opcode-coverage.json`.
- **Framework boot:** real `ActivityThread` comes up, `attachApplication` is
  received by the Rust ATMS, `bindApplication` is delivered, and a real
  `ClientTransaction` is scheduled (`results/m2/M2_LOG.md`). `Application.onCreate`
  runs on 3/5 samples; the launch then blocks in `VectorDrawable.nCreateGroup`
  (`BENCHMARKS.md` §9.1 M3).
- **Bench (native-runtime fidelity, not ART-verified):** `BENCHMARKS.md` §9 is
  the authoritative table (commit `addd4e2`, release, 5 rounds,
  `results/bench/v1-2026-11-11-0214.json`). Headline: process spawn → `Vm`
  16.43 ms p50, framework boot → attachApplication 13.60 ms, `Vm::restore`
  6.97 ms, before/after launch 42.70 → 29.85 ms, idle core 171.97 MiB booted /
  28.91 MiB Vm-only, N=5 per-session 56.85 MiB, SessionPool create 1.58 ms /
  reset 2.08 ms, hello-views cold frame 2.54 ms, `frame-diff` SSIM 0.9938. The
  earlier `results/bench/REPORT.md` numbers (run `201637`, commit `ced0d70`) are
  superseded for V1 purposes and kept only as wave history.
- **On-disk boot-image cache (T-43b, W96-A, `BENCHMARKS.md` §8.14):** a fresh
  process restores the framework-init `ACOSSNAP` from a content-addressed cache
  under `ACOS_CACHE_DIR`/`~/.cache/acos` in **65.676 ms p50** vs a full
  framework boot of **94.221 ms p50** (1.44x; 5 interleaved fresh-process
  rounds, `results/bench/w96a-cache.json`). The cold restore still pays the
  ~54 ms class relink, so it is not the 12.8 ms in-process figure of §8.13.
  `acos launch` is **not** wired yet: a restored post-attach snapshot leaves
  stale `BinderProxy` handles (`unknown binder handle 0x24` at
  `application_on_create`), so binder-identity re-seed (T-43) gates it. Launch
  control unchanged at 5465 ms p50 wall / 517 MiB peak RSS
  (`results/bench/w96a-launch.json`).

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

## Per-metric vs legacy

The authoritative ACOS-vs-legacy table is **`BENCHMARKS.md` §9.1** (commit
`addd4e2`, 2026-11-11, release, 5 rounds). Result: **2 PASS / 9 FAIL** against
owner directive D2 — memory (M1, M2) beats the legacy ACVM; every frame,
screenshot, reset, UI-tree and end-to-end metric is either unmeasured or not
comparable yet. The older `results/bench/REPORT.md` §2/§3 table (run
`run-2026-11-10-201637.json`, commit `ced0d70`) is retained as wave history but
is no longer the V1 record.

## Next 10 tasks

1. **Land the VectorDrawable/M2 activity chain** (`gfx_vector.rs` +
   `fwk_m2_chain6.rs` in the working tree), then record a full M2 lifecycle run
   with `app_created`/`tx_delivered` evidence. This is the single blocker for
   M3–M8 and M10 (`BENCHMARKS.md` §9.1; today `ClientTransaction` is 0/5).
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
9. Keep `tools/bench-v1.sh` as the reproducible V1 record; extend it to M3–M11
   as those capabilities land and run ≥20 reps for the latency rows when the
   host is quiet. Keep the regression gate (`tools/bench-acos.sh --check`).
10. Run the `scenarios/negative/*` suite (and a differential against the now
    captured `fixtures/reference-device/`) to prove zero false passes before any
    V1 verdict claim.

## Working tree / concurrency

HEAD `addd4e2` is the committed checkpoint (the V1 benchmark target). The
working tree carries uncommitted wave work from concurrent agents
(W57/W61/W64/W65/W66 native/JNI/render additions, W68-A `gfx_vector` and
`acos-runtime`/`acos-services`, W69-A frame/interp/vm/jni bisecting, W70-A
`acos-render`, W71-A's W67-A security/test-honesty fixes, and W96-A/W97-A
`gfx_vector`/M2-chain work: `crates/acos-vm/src/natives/gfx_vector.rs`,
`fwk_m2_chain6.rs`). Shared files (`heap.rs`, `reflect.rs`, `interp.rs`,
`vm.rs`, `snapshot.rs`) were edited in place; concurrent agents re-read before
editing. Validate on a scratch worktree if the shared tree does not build.

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
