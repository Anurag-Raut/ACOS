# ACOS — Status

**As of 2026-11-11, HEAD `fd1a887`** (W165-A; `main` also carries W161-A through
W171-A). `git log` is authoritative, and `tools/status-check.sh` fails when this
line drifts more than 3 commits (`tools/verify-all.sh` now **fails** on it, not
warns). **`BENCHMARKS.md` §9 is the authoritative measured table**; **§9.8 (W176-A) is
the newest V1 run** (`results/bench/v1-w176a.json`) and reports **9 PASS /
2 FAIL** (M5, M9) with all 11 metrics measured. It was built from `fd1a887`
plus two then-untracked W172-A pieces (`private_root.rs` + one `lib.rs`
declaration) because `fd1a887` did not compile standalone; W172-A has since
committed them and `main` advanced to `af4ab94` (§9.8 records it). §9.1
(W160-A) and §9.7 (W165-A) are earlier tables/history and their M8/M10/M11
notes are stale: the tap now lands (`results/m2/M2_LOG.md` §W161-A,
`crates/acos-runtime/tests/counter_interaction.rs`) and W167-A added the post-tap
repaint invalidation. `TASKS.md` is the plan;
`docs/V1.md` is the V1 definition; `docs/FINDINGS.md` is the append-only
findings log; `EXPERIMENTS.md` is history; `docs/SECURITY.md` lists the
safety-relevant `ACOS_*` knobs and the private-root rule.

## One line

**`counter.apk` reaches `first_frame` through a real launched Activity
(`ThreadedRenderer.draw` → `FrameStore`), and a tap on it now lands: the app's
own `onClick` runs and the live tree shows `count=1` (W161-A,
`crates/acos-runtime/tests/counter_interaction.rs`, un-ignored). `hello-views`,
`form-validation` and `list-scroll` reach `first_frame` at W147-A; the W166-A
fixtures `dialogs-menus`, `list-adapter` and `fragments-nav` reach it too. The
default boot-cache **restore** path is fixed (W154-A carried native regions,
W160-A records their file source and re-reads it): a warm `acos launch`
reaches `first_frame` **5/5** at 0.806 s p50 with a **25.58 MB** cache entry. A
live `View` tree is observable (M7 49.8 ms p50). The pressed-state **ripple** is
a **declared flat-colour fallback** (W167-A implemented the AGSL/RippleShader
natives to draw the declared flat colour, not the agsl shader); it is a named
visual divergence, not a fake frame.** The
Rust VM runs the real Android 14 core-library bytecode, the real boot-classpath
DEX is extracted unprivileged, `acos inspect`/`run-dex`/`verify` exist, and the
Rust service layer (`acos-services`, `acos-wm`), the SQLite engine
(`acos-sqlite`), the confinement layer (`acos-sandbox`) and the renderer
(`acos-render`) have their own acceptance tests. The V1 benchmark at
`20663b3`+W160-A beats legacy on **5 of 11** metrics (**5 PASS / 6 not-pass** vs
owner directive D2; see `BENCHMARKS.md` §9); the frame, live UI tree and fresh
screenshot now land, while input, reset, independence and the V1 verdict remain
unverified.

## Milestone table (M0..M5 + V1 gate)

The named gates come from `research/service-layer/BACKLOG.md` §"Milestone
summary" (M1..M5); M0 is the P0/P1 static-analysis foundation in
`docs/handoff/04_ROADMAP_AND_ACCEPTANCE.md`.

| Gate | Requirement | State | Evidence |
|---|---|---|---|
| **M0** static APK/DEX analysis | parse/verify/decode APK + DEX; differential vs `dexdump`/`aapt2` | **done** (static-analysis tier) | `crates/acos-dex/tests/diff_dexdump.sh` → 3 files / 380 methods / 4,253 insns / 0 mismatches (`docs/FINDINGS.md` F-0004); `crates/acos-apk/tests/differential_aapt2.rs`; `docs/FINDINGS.md` F-0002 |
| **M1** println on the real core library | console programs execute on real `core-oj`/`core-libart`/`core-icu4j` DEX and match the ART oracle | **done** (art-reference) | `results/m1/README.md` (20/20 byte-identical, normal + GC-stress); `results/vm/OS_NATIVES_AUDIT.md` §3 (`--corelib real`: 25 pass / 0 fail / 0 unsupported of 25); `results/vm/dexprogs-real-normal.json` |
| **M2** `Application` + `Activity.onCreate` | `bindApplication` delivered; lifecycle `onCreate→onStart→onResume` matches ART | **done for counter** (gate enabled W147-A) | `bindApplication` + `Application.onCreate` run; the launch reaches `on_resume` and `first_frame` on `counter.apk` (`results/m2/M2_LOG.md`; `BENCHMARKS.md` §9.1 M3); `hello-views`/`form-validation`/`list-scroll` reach `first_frame` at W147-A (`f3120d5`); `crates/acos-runtime/tests/m2.rs` |
| **M3** first frame in `acos-render` | real non-black frame with `frame_seq`/cause/hash from a launched Activity | **done for counter** (W133-A) | `results/render/M3_LOG.md`, `results/render/m3-hello-views-vs-art.json`, `crates/acos-runtime/src/frame_handoff.rs`; framework-bound `acos launch counter.apk` → `first_frame` in **0.806 s p50** on the **default warm path** (5/5; cold control 1.073 s p50), `BENCHMARKS.md` §9.1 M3; the launched frame is not yet ART-scored |
| **M4** input + UI tree | `ui_initial.json` equality; tap → `clicks: 1` | **partial** — tree live, tap lands on `counter` | live `View` tree via `acos observe --tree` (M7 49.9 ms, `results/wm/README.md`); `acos tap --text Increment` on a kept session runs the app's `onClick` and the tree shows `count=1` (`results/m2/M2_LOG.md` §W161-A, `crates/acos-runtime/tests/counter_interaction.rs`); still missing: `ui_initial.json` equality against ART and the real AGSL shader (the ripple is a declared flat-colour fallback, W167-A). The §9.1 M8 row predates the tap fix (§9.5 item 3 is stale); §9.7 (W165-A) is the newer measurement |
| **M5** interaction trace | `lifecycle-summary.txt` + `ui_after_*` + native-diff | **not started** | depends on M2–M4 |
| **V1 gate** `acos verify` full verdict | `acos verify --apk … --scenario … --json --artifacts …` passes on the two fixtures with no Android boot | **partial (W180-A)** | `NativeBackend` now drives the product `AgentSession`: `counter.apk` core loop **passes** 13/13 (`scenarios/v1-counter.yaml`, 16.8 s, fresh capture frame 10, `results/v1/w180a/counter/verdict.json`); negative suite `fail`/`crash`/`unsupported` with **0 false passes** (`crates/acos-cli/tests/negative_verify.rs`); `hello-views` is blocked after typing by a >120 s traversal spin; `hello-jni` reports `UNSUPPORTED_NATIVE_SYMBOL` for an exported symbol (W93-A); rotation and ART bounds are open (W179-A). Full item-by-item status in `docs/V1.md` §W180-A and `results/v1/W180A.md` |

## V1 benchmark (2026-11-11, W160-A)

`BENCHMARKS.md` §9 is the authoritative measured table; raw per-round samples in
`results/bench/v1-2026-11-11-061354.json` (release, 3 rounds, loadavg 1.99,
35.8 GiB available, `.cache/target-w160a`, guarded). Built from `20663b3` + the
uncommitted W160-A patch; `main` moved to `b467193`/`65856f2` during the run.
Reproduce with `tools/bench-v1.sh --rounds 3`.

Against owner directive D2 (W176-A, §9.8): **9 PASS / 2 FAIL / 0 NOT_MEASURED**
(was 7 PASS / 4 FAIL at W165-A). Passes: M1 fixed idle core **133.90 MiB** vs
206.0 (W1); M2 N=5 per-session **39.40 MiB** vs 78.7 (v9); M3 framework-bound
`acos launch counter.apk` → `first_frame` **0.937 s p50** on the **default warm
path** (5/5) vs 3.620 s; M4 worker create→frame **0.719 s** vs 1.297 s; M6
reset→frame **714.7 ms** vs 748.1 ms; M7 live `View` tree **49.97 ms p50** vs
538 ms one-shot; M8 tap→state **249.2 ms**, `accepted=true`
`state_changed=true`; M10 end-to-end verify wall **1.664 s** vs ~19 s with the
assert passing (frame hash `b6759…`→`f79465…`, PNG diff 448 pixels); M11
**937.3 verified tasks/GiB-min** (`verified_tasks=1`). Fails: **M5** daemon
screenshot **99.35 ms** vs 66.5 ms (regressed from §9.1's 50 ms at HEAD;
`noisy`), **M9** per-session peak **PSS 528.2 MiB** (Pss_Anon 525.9 +
Pss_File 3.5; the ~114 MiB file-backed RSS is shared page cache) with input
independence still unproven. ACOS-native: process→`Vm` 16.37 ms, boot→attach
11.88 ms, `Vm::restore` 50.8 ms, SessionPool create 1.22 ms / reset 2.32 ms
(N=5), renderer cold frame 2.15 ms, PNG 7.37 ms, `frame-diff` 145 ms, live tree
50.0 ms. VM throughput is still behind `java -Xint` on 11/12 `Bench*` programs
(only `BenchString` 0.203×; worst `BenchGc` 2.55×). Noisy rows:
`observe_tree_ms`, M5 screenshot. Raw: `results/bench/v1-w176a.json` (built from
`fd1a887` + two then-untracked W172-A pieces; §9.8 records why).
`results/perf/BOOTIMAGE_CACHE.md` §W160-A).

**Test counts.** The full suite is **not** re-run every wave (the tree is shared
with other agents); each wave records its own gates. W160-A's gates were green
at `20663b3`+W160-A (`acos-vm` lib native_mem 6/6, vfs 18/18, snapshot 16/16,
`acos-runtime` boot_image_cache 6/6, `acos-cli` session 11/11 incl. the warm-cache
regression). W172-A adds: `acos-native-loader` lib 39/39 (sem membership + guest-stack
validation), `acos-jni` lib 30/30 (`NewDirectByteBuffer` range validation),
`acos-runtime` lib 35/35 (private-root + boot-cache rejection), boot-cache
6/6, `counter_interaction` 3/3, `m2` 3/3, `acos-cli` session 11/11,
`native_session` 1/1, `serve_daemon` 1/1, clippy `-D warnings --no-deps` clean
on the touched crates, `cargo check --workspace --all-targets` pass and
`tools/diff-dexprogs.sh --corelib real` **27 pass / 0 fail**, all re-run at
`fd1a887` in `.cache/target-w172a` through `tools/guarded.sh`. W172-A also
strengthened the `native_session`/`serve_daemon` frame assertions, gated the
boot-cache and RN-pixel skips on `have_fixture`, and made
`verify-all.sh`'s status-check step **fail** instead of warn.

### Known gaps (V1)

1. **[CLOSED] Boot-image restore for launch.** W154-A carried the native
   regions; W160-A records their file *source* and re-reads it, failing closed
   on a changed file. Warm `acos launch` reaches `first_frame` **5/5** at
   0.806 s p50 with a **25.58 MB** cache entry. Evidence: `BENCHMARKS.md` §9.1
   M3, `results/perf/BOOTIMAGE_CACHE.md` §W160-A. Regression guard:
   `crates/acos-cli/tests/session.rs::warm_boot_cache_reaches_first_frame_on_every_launch`
   (1 cold process + 5 warm processes).
2. **Launched frame not ART-scored.** `acos-render` produces a correct
   hello-views frame (cold 2.22 ms, SSIM 0.9938 vs the ART capture) and a
   launched Activity publishes through `frame_handoff` (M3), but no ART-diff
   has been recorded for the launched frame. Evidence:
   `results/render/README.md`, `results/render/M3_LOG.md`.
3. **[CLOSED for `counter`] Tap / input.** `acos tap --text Increment` now runs
   the app's `onClick` and the live tree shows `count=1` (W161-A; the W152-A
   blocker was a sync-barrier traversal, fixed by cancelling the pending
   traversal after input delivery and re-anchoring the vsync grid).
   `results/m2/M2_LOG.md` §W161-A, `crates/acos-runtime/tests/counter_interaction.rs`.
   The pressed-state **ripple** is a **declared flat-colour fallback**: W167-A
   implemented the AGSL/RippleShader natives well enough to draw the declared
   flat colour rather than the shader, which is a named visual divergence (not a
   fake frame). Pool reset is still core-VM-only (M6). Evidence:
   `BENCHMARKS.md` §9.8 M6/M8 (tap→state **249.2 ms**, frame hash and
   before/after PNGs change; measured after the tap fix), `results/m2/M2_LOG.md`
   §W161-A.
4. **M9 not independent.** One process-global Binder transport: N sessions are a
   memory/throughput experiment, not N isolated workers. Per-session peak is
   **PSS 528.2 MiB** at N=20 (private anon 525.9, shared file 3.5); the
   ~114 MiB file-backed RSS is shared page cache, not private cost. Evidence:
   `BENCHMARKS.md` §9.8.2.
5. **RN bridge.** Hermes/JNI confined-stack probes exist (`results/rn/`,
   `tools/rn-*`), but no React Native hello publishes a frame.
6. **Flutter first frame.** The Dart `main` executes in the confined engine
   (`results/flutter/FIRST_RUN.md`) but no Java `Surface`/`ANativeWindow` is
   delivered, so no first frame.
7. **IR tier default-off.** `ACOS_EXEC=ir` wins 2–2.7× on dispatch loops but is
   not default; `BenchGc`/`BenchAlloc` stay allocation-bound.
   Evidence: `BENCHMARKS.md` §8.9–§8.11.
8. **Committed artifacts lag.** Several `results/` files predate `2c682c7`;
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
  runs, the launch reaches `on_resume` and `first_frame` on `counter.apk`, and
  `VectorDrawable.nCreateGroup` is implemented (`BENCHMARKS.md` §9.1 M3).
- **Bench (native-runtime fidelity, not ART-verified):** `BENCHMARKS.md` §9 is
  the authoritative table (measured at `2c682c7`, release, 3 rounds,
  `results/bench/v1-2026-11-11-051824.json`). Headline: process spawn → `Vm`
  13.95 ms p50, framework boot → attachApplication 12.44 ms, idle core
  133.85 MiB booted / 21.71 MiB Vm-only, N=5 per-session 39.45 MiB,
  SessionPool create 1.24 ms / reset 2.22 ms, renderer cold frame 2.98 ms,
  `frame-diff` 171 ms, live View tree 49.9 ms, framework launch → `first_frame`
  0.903 s (boot cache off). D2 result **4 PASS / 7 FAIL**. The earlier
  `results/bench/REPORT.md` numbers (run `201637`, commit `ced0d70`) are
  superseded for V1 purposes and kept only as wave history.
- **On-disk boot-image cache (T-43b, W96-A, `BENCHMARKS.md` §8.14):** a fresh
  process restores the framework-init `ACOSSNAP` from a content-addressed cache
  under `ACOS_CACHE_DIR`/`~/.cache/acos` in **65.676 ms p50** vs a full
  framework boot of **94.221 ms p50** (1.44x; `results/bench/w96a-cache.json`).
  **`acos launch` is wired** (the old "not wired yet" claim is false at this
  HEAD): `acos launch counter.apk --json` reaches `first_frame` in **0.903 s
  p50** with `ACOS_NO_BOOT_CACHE=1`, 617 MiB peak RSS (`BENCHMARKS.md` §9.2).
  The cache **restore** path is **fixed** (W154-A carried the native regions,
  W160-A records their file source): a warm launch reaches `first_frame` 5/5 at
  0.806 s p50 (`BENCHMARKS.md` §9.1 M3; regression guard
  `crates/acos-cli/tests/session.rs::warm_boot_cache_reaches_first_frame_on_every_launch`).
  Forcing the cold path (`ACOS_NO_BOOT_CACHE=1`) is no longer required.

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

The authoritative ACOS-vs-legacy table is **`BENCHMARKS.md` §9.1** (measured at
`2c682c7`, 2026-11-11, release, 3 rounds;
`results/bench/v1-2026-11-11-051824.json`). Result: **4 PASS / 7 FAIL** against
owner directive D2 — M1/M2 memory, M3 framework-bound first frame and M7 live UI
tree beat the legacy ACVM; M4/M5/M6/M8/M9/M10/M11 are missing or fail. Note the
table predates W161-A, so its "tap does not land" rows (M8, §9.5 item 3) are
stale at HEAD: the tap lands on `counter.apk` (`results/m2/M2_LOG.md`
§W161-A). The next `tools/bench-v1.sh` run must refresh them. The older
`results/bench/REPORT.md` table (commit `ced0d70`) is retained as wave history
but is no longer the V1 record.

## Next 10 tasks

Closed since the last refresh (do not re-open): **boot-image warm restore**
(W154-A/W160-A) and the **`counter` tap** (W161-A). Current list:

1. Re-run `tools/bench-v1.sh` on the committed post-W161-A HEAD so the V1 table
   reflects the tap fix, M8 and the ripple gap; record the exact committed SHA in
   the artefact (W170-A C1/C2). Owner: bench wave, not this doc.
2. Score the launched `counter` frame against the ART reference and route
   `observe --screenshot` evidence into a scenario (M5/M3 correctness).
3. Close the M2 gate with an ART-order lifecycle diff now that `on_resume` and
   `first_frame` are reached (`crates/acos-runtime/tests/m2.rs`), and decide the
   post-`onResume` error tolerance explicitly.
4. Finish the framework file/`/system` I/O substrate (FileChannel, `/system`
   fonts/resources) so `handleBindApplication` runs without a structured block.
5. Replace the W167-A flat-colour ripple fallback with real AGSL/RippleShader
   output, and keep the divergence declared in every interaction frame until then
   (`results/m2/M2_LOG.md` §W161-A/W167-A).
6. Commit the W25-B `OsConstants` + `libcore.io.Linux` work and clear the 9
   pending Linux natives (`results/vm/OS_NATIVES_AUDIT.md` §2).
7. Cover ICU regex constructs that currently raise, or record each as a declared
   divergence when it cannot change fixture output.
8. Give each session its own Binder transport so N workers are independently
   usable (M9/V1 §5.1; `results/bench/MEMORY_PROFILE.md` §5).
9. Keep the fixture-gate coverage honest: every fixture-dependent test calls
   `have_fixture` and is listed in `tools/verify-all.sh`'s `FIXTURE_TESTS`
   (W172-A extended it to the boot cache, M2, interaction and the RN/Flutter
   product targets).
10. Run the `scenarios/negative/*` suite (and a differential against the now
    captured `fixtures/reference-device/`) to prove zero false passes before any
    V1 verdict claim.

## Working tree / concurrency

HEAD `fd1a887` (W165-A) is the committed checkpoint (W172-A below it). The tree is shared with
concurrent waves (bench/pool, RN `fs` natives, ripple/tap, resources/input,
ICU), so the working tree carries their in-flight edits; each agent builds into
its own `CARGO_TARGET_DIR` (`.cache/target-w<N>`) through `tools/guarded.sh`.
Shared files (`heap.rs`, `reflect.rs`, `interp.rs`, `vm.rs`, `snapshot.rs`,
`vfs.rs`, `natives/*`) are edited in place; re-read before editing and never
`git stash`. Validate on a scratch worktree if the shared tree does not build.

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
