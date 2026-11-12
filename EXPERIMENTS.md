# ACOS — Experiment log

Everything ACOS tries, what happened, what was learned, and the conclusion.
Failures, wrong early readings, false passes and negative findings are recorded
too — a correction is an entry.

Owner: docs/reference. Newest entries at the **bottom**.

## Format

Each entry uses this template:

```markdown
## E<N>.<M> — <short title> — <MEASURED | READ-ONLY | UNVERIFIED | FAILED | NEGATIVE>

- **Date / commit:** <date> <acos git sha or "no commit">
- **Question / goal:** ...
- **Method:** what was read/run, host state, limits
- **Inputs:** fixture paths + sha256, image/toolchain versions, API/ABI
- **Result:** raw numbers/observations, p50/p95 where applicable, or exact error
- **Finding:** what this means (distinguish proven vs hypothesis)
- **Fidelity / evidence:** label + artifact path
- **Regression / false pass:** yes/no + why
- **Conclusion / next:** what changes, or why the path is stopped
```

Rules: no fixed sleeps in timed paths; state the memory metric type; pair/
interleave A/B; zero false passes; never exceed the 85 % RAM guard; record the
host state; label fidelity on every result. Read-only audits must not mutate the
subject.

---

## E0.1 — Reference inventory of the frozen legacy engine — READ-ONLY

- **Date / commit:** 2026-11-10; no ACOS commit (`~/Projects/acos` has no
  commits yet); subject read at `main` = `d005a0d`.
- **Question / goal:** reconcile the legacy ACVM engine (`/home/Qui/Work/
  android-container`) against `docs/handoff/05_OLD_ACVM_REFERENCE.md`, extract
  measured baselines, inventory fixtures, capture the agent contract and
  methodology lessons, and document later oracle commands — without mutating the
  legacy repo.
- **Method:** read-only inspection. `git status --short` (empty before and
  after), `git log`/`git branch`/`git tag`, file reads, and
  `aapt2 dump badging` (`~/Android/Sdk/build-tools/35.0.0/aapt2`) on existing
  `helper/build/*.apk`. No container start/stop, no Docker, no benchmarks, no
  git mutation, no sudo, no downloads.
- **Inputs:** the full handoff package; the legacy tree; `iter/*.md`,
  `EXPERIMENTS.md`, `ITERATIONS.md`, `STATE.md`, `PLAN.md`, `TASKS.md`,
  `agent/README.md`, `agent/SCHEMA.md`, `docs/AGENT-LOOP.md`,
  `research/PLAN-task-first.md`, `research/acvm-lite-direction-2026-11-10.md`,
  `iter/compat-v2.json`, `bench/lib/harness.py`, `bench/score.py`.
- **Result (key measurements, all read from committed artifacts, not re-run):**

  | Metric | Value | Type | Cold/warm | Source |
  |---|---|---|---|---|
  | Bare core legacy (stripped) | 278 MiB | anon | settled | `EXPERIMENTS.md` §5 |
  | Bare core v9 → W1 diet | 303.9 → **206.0** MiB | anon+shmem | settled 60 s | `iter/W1.md` |
  | Per-session N=1 / N=5 (v9) | 130.3 / 78.7 MiB | anon+shmem | warm | ITERATIONS v9 |
  | Cold core→first shot (v12) | **3.620 s** (v11 4.260) | wall | cold, interleaved n=5 | `iter/V12.md` |
  | Guest boot (v10) | **1.517 s** (v9 4.221) | wall | cold | ITERATIONS v10 |
  | Worker create→frame | 1.251–1.343 s p50 | wall | warm | `EXPERIMENTS.md` §21/§23 |
  | Fresh screenshot | 0.066–0.067 s | wall | warm | ITERATIONS v1/v9 |
  | `reset --app` cmd / → frame | 110.3 / 748.1 ms p50 | wall | warm | `iter/D2.md` |
  | UI tree structure / one-shot text / persistent text | 14–87 ms / 538–768 ms / 4.1 ms | wall | warm | `EXPERIMENTS.md` §12/§25 |
  | W7 shared-process extra view | ≈21–29 MiB | anon+shmem | warm, no isolation | `iter/W7.md` |
  | R1 host renderer | 6.27 s p50, 1.26 GiB RSS | wall/RSS | warm, host-only | ITERATIONS R1 |

- **Finding (with discrepancies):**
  1. **Major unmentioned change:** the latest legacy commit (`d005a0d`) records a
     user decision to build an "own lightweight Android testing runtime"
     ("ACVM Lite") and **archives the AOSP/redroid track** (tag
     `aosp-track-final` = `31da75a`). The legacy engine is frozen; it is an
     oracle/baseline, not a live track. `docs/handoff/05` predates this.
  2. Handoff's "281 agent tests" is stale/ambiguous (D2 said 272; latest main
     state W5 = 308 agent + 138 bench).
  3. **U2/R2/W2b/D3/D4 are NOT merged to `main`** — they exist only on branches
     (`git merge-base --is-ancestor` returns non-zero). `main` contains
     W1/W4/W5/R1-report/v12/D2/task-first docs.
  4. `spikes/` and `runtime/` referenced by the docs are **absent** from the
     checkout.
  5. The 23-app P2b compatibility corpus is **not present** (only a 10-app
     `iter/compat-v2.json`); W7's marginal is honestly 21–29 MiB, not 11–22.
  6. Bare-core reads vary **259–322 MiB** across host states; only within-session
     A/B is valid.
  7. The fixture tree is small (4 utility APKs, one uses the public AOSP platform
     test key); no Compose/Flutter/RN/NDK/split fixtures.
- **Fidelity / evidence:** `docs/reference/ACVM_INVENTORY.md` (full audit, all
  citations). Read-only; no ACOS measurement.
- **Regression / false pass:** none. No subject was executed.
- **Conclusion / next:** use the legacy engine as a frozen behavioural/benchmark
  oracle only, subject to explicit authorisation and the RAM guard; carry over
  its agent contract, error taxonomy, frame-freshness contract and measurement
  discipline. ACOS must not inherit its redroid/privileged-container
  architecture. Next: land `acos-apk`/`acos-cli` parse-only and `acos-dex`
  decode, then the first real compiled DEX execution (P1).

### E0.1a — Negative result: no usable committed app corpus — NEGATIVE

- **Result:** searching the legacy tree for APKs yields only the four
  self-authored helper/test APKs plus a build-time overlay; the 23-app P2b corpus
  and all F-Droid APKs are absent. No `.so`/JNI fixture, no Compose/Flutter/RN
  APK, no split-APK fixture is present.
- **Conclusion:** ACOS cannot start from a reusable licensed corpus; it must
  author its own fixtures (with SDK build tools per ADR-0004) and record licences
  and hashes. Legacy APK numbers are therefore not directly re-runnable as
  fixtures without re-acquisition.

---

## E1.1 — acos-dex disassembly differential vs the official `dexdump` — MEASURED

- **Date / commit:** 2026-11-10; `5945a57` (Wave 1).
- **Question / goal:** does the Rust DEX decoder produce the same instruction
  stream as the reference tool, so the VM can trust it?
- **Method:** `crates/acos-dex/tests/diff_dexdump.sh` compares per-method
  instruction counts and opcode mnemonic sequences against SDK
  `dexdump -d` (`/home/Qui/Android/Sdk/build-tools/35.0.0/dexdump`). Re-run with
  the prebuilt example binary because the working tree was mid-edit by a
  concurrent subagent (`acos-dex` lazy-table edits did not compile at the time).
- **Inputs:** `fixtures.dex`, `fixtures_api19.dex`, `fixtures_invokecustom.dex`.
- **Result:** `files=3 methods=380 instructions=4253 mismatches=0`.
- **Finding:** the decoder is instruction-exact on the committed fixture corpus.
  (Opcode families `move`/`const`/arith/branch/switch/invoke/fields/arrays/
  checks/new-instance/monitor/try-catch/payloads are all covered.)
- **Fidelity / evidence:** `static-analysis`; `docs/FINDINGS.md` F-0004;
  `crates/acos-dex/tests/diff_dexdump.sh`.
- **Regression / false pass:** none; 0 mismatches.
- **Conclusion / next:** DEX decode is trusted for fixtures. Boot-classpath
  coverage is env-gated (`ACOS_BOOT_JARS`, `boot_jars.rs`) and pending; the
  working tree must compile again before commit.

## E1.2 — acos-apk manifest/AXML differential vs `aapt2` — MEASURED

- **Date / commit:** 2026-11-10; `5945a57`.
- **Question / goal:** does the Rust APK/manifest model agree with the SDK?
- **Method:** `cargo test -p acos-apk --test differential_aapt2` compares the
  ACOS manifest model against `aapt2 dump badging` and the AXML DOM against
  `aapt2 dump xmltree --file AndroidManifest.xml` over the fixture APKs (and the
  read-only legacy APKs when present).
- **Inputs:** `crates/acos-apk/tests/data/{hello-views,hello-jni}.apk`.
- **Result:** `1 passed; 0 failed` (aapt2 found via SDK path).
- **Finding:** static APK ingest agrees with the reference tool on the fixtures;
  `acos inspect --json` emits a correct typed report (package, versionCode/Name,
  minSdk 26, targetSdk 34, launcher activity, `classes.dex`).
- **Fidelity / evidence:** `static-analysis`; `crates/acos-apk/tests/differential_aapt2.rs`;
  `crates/acos-cli/tests/cli.rs`.
- **Regression / false pass:** none.
- **Conclusion / next:** `resources.arsc` and ABI inventory remain `partial` in
  `COMPATIBILITY.md`; extend the differential before claiming them verified.

## E1.3 — R-API: real Android framework DEX feasibility spike — READ-ONLY / MEASURED

- **Date / commit:** 2026-11-10; `1da205f` (research).
- **Question / goal:** can ACOS execute the *real* Android 14 framework instead
  of reimplementing it, and how big is the native work-list?
- **Method:** extracted the boot-classpath jars unprivileged from the installed
  API 34 x86_64 system image (`dd`/`lpunpack`/`debugfs`/APEX ZIP); measured DEX
  inventory and ran a conservative call-graph closure (`dexclosure.py`) at two
  dispatch approximations (`VIRT=0` declared-only, `VIRT=1` CHA). No emulator,
  container, sudo, mount, download or write to the legacy repo.
- **Inputs:** 17 boot dex / 55.5 MiB; `services.jar` (system_server) separately.
- **Result:** 41,829 classes / 328,352 methods / **5,607 natives**; all boot dex
  v039, no cdex, not dex-stripped. `println` **152** natives (39 classes) lower /
  **663** (79) upper. Minimal Activity **612** (105) lower / **2,270** (300)
  upper; of 612: 175 core, 419 `android.*`, 18 other. ~10 AIDL interfaces on the
  launch path, all satisfiable by in-process Rust stubs for a deterministic task.
  layoutlib (real framework on a JVM, 66 `*_Delegate`) is prior art.
- **Finding:** executing the real framework on the Rust VM with Rust natives is
  feasible and bounded; the decisive risk is dynamic native coverage and
  system-server coupling, not bytecode availability.
- **Fidelity / evidence:** `research/framework-dex/REPORT.md`;
  `research/framework-dex/data/*`; `docs/FINDINGS.md` F-0002/F-0003.
- **Regression / false pass:** none (static analysis).
- **Conclusion / next:** ADR-0007/0008 accepted. Recommend implementing the
  `println` native work-list first, then the Activity closure, reporting the
  first `unsupported` native per fixture as the progress metric.

## E1.4 — Boot-classpath DEX SHA-1 signatures do not validate — MEASURED (finding)

- **Date / commit:** 2026-11-10; in progress at reading.
- **Question / goal:** why did every boot-classpath dex fail to parse?
- **Method:** parse + structural verify of the extracted boot dex with
  `acos-dex` (`ACOS_BOOT_JARS=… cargo test -p acos-dex --test boot_jars`); inspect
  `ParseOptions`/`signature_valid`.
- **Result:** every boot dex had a stored SHA-1 signature that did not match the
  computed one → `DEX_BAD_SIGNATURE` on the default path. Android does not verify
  the DEX SHA-1 signature at load time (platform hiddenapi tooling rewrites boot
  dex without updating it).
- **Finding:** rejecting on SHA-1 mismatch is stricter than Android and blocks
  the real boot classpath. `acos-dex` now enforces **Adler-32** but records and
  accepts the **SHA-1** mismatch by default (`signature_valid=false`), with
  `ParseOptions::STRICT` to enforce it.
- **Fidelity / evidence:** `crates/acos-dex/src/lib.rs` (`ParseOptions`,
  `signature_valid`); `crates/acos-dex/tests/boot_jars.rs`
  (`signature_mismatches_accepted`); ADR-0017; `docs/FINDINGS.md` F-0005.
- **Regression / false pass:** none, but the policy must be explicit in results
  (`signature_valid=false` reported, never hidden).
- **Conclusion / next:** ADR-0017. Ensure the boot-classpath test is green and
  committed once the working tree compiles.

---

*No execution experiment has been run yet: no DEX is interpreted on the real
core library, no Activity runs, no frame is rendered. The next entry must be a
real execution artifact (the E2.1 C1 gate), not a plan.*

---

## E2.1 — M1: the real Android 14 core library on the Rust VM — MEASURED

- **Date / commit:** 2026-11-10; through `ced0d70`/`40f075c`.
- **Question / goal:** execute the real boot-classpath bytecode for the console
  tier and match the ART oracle.
- **Method:** `tools/diff-dexprogs.sh --corelib real` (normal and
  `ACOS_GC_STRESS=1`), against `fixtures/expected-art/dexprogs/`.
- **Result:** the original 20-program gate passes byte-identical stdout + exit
  (`results/m1/README.md`); the corpus is now 25 programs, and W25-B records
  `25 pass / 0 fail / 0 unsupported` (`results/vm/OS_NATIVES_AUDIT.md` §3).
- **Finding:** the Rust VM can run the real `core-oj`/`core-libart`/`core-icu4j`
  DEX with Rust natives; the console tier is `art-reference`-verified.
- **Fidelity / evidence:** `art-reference`; `results/m1/README.md`,
  `results/vm/dexprogs-real-normal.json`, `results/vm/OS_NATIVES_AUDIT.md`.
- **Regression / false pass:** none; per-program PASS/FAIL recorded in JSON.
- **Conclusion / next:** framework boot (E2.2). The committed
  `results/vm/dexprogs-real-normal.json` still records 24/25
  (`25_security_digest`); re-record the artifact.

## E2.2 — M2: `bindApplication` delivered, no lifecycle callback — MEASURED (partial)

- **Date / commit:** 2026-11-10; `40f075c` + working tree.
- **Question / goal:** reach `Application.onCreate`/`Activity.onCreate` on the
  real framework.
- **Method:** `cargo run -p acos-runtime --example lifecycle_probe` and
  `cargo test -p acos-runtime --test m2` (gate `#[ignore]`d).
- **Result:** `ActivityThread` boots, `attachApplication` reaches the Rust ATMS,
  `bindApplication` is delivered, and a real `ClientTransaction` is scheduled
  (`transaction_scheduled=true`); the gate stays red (`app_created=false`,
  `tx_delivered=false`). Blockers were worked through in order: ICU
  `likelySubtags` → `FileChannel.map0` → `Font$Builder.nInitBuilder` →
  `OverlayProperties` → display `DisplayInfo` parcel → `TetheringManager`.
- **Finding:** the framework path is real (no faked callbacks); the remaining
  gate is the display reply codec + Activity transaction delivery.
- **Fidelity / evidence:** `art-reference` target; `results/m2/M2_LOG.md`,
  `results/m2/boot-release.json`, `crates/acos-runtime/tests/m2.rs`.
- **Regression / false pass:** the honesty invariant refuses to report a
  delivered transaction unless `ActivityClientRecord.activity` is set.
- **Conclusion / next:** land the working-tree display parcel + APEX boot
  classpath, then record the lifecycle gate.

## E2.3 — Snapshot/restore proof of concept — MEASURED

- **Date / commit:** 2026-11-10; commit `0cdfacd` + task changes.
- **Question / goal:** does a VM snapshot restore into a fresh VM with identical
  behaviour (ADR-0010 gate 2)?
- **Method:** `cargo test --release -p acos-vm --test snapshot`.
- **Result:** 16/16 pass; 23/23 real dexprogs run identically after restore,
  normal and GC-stress; restore p50 13.34 ms real / 222 µs bootstrap; snapshot
  size 6,633 B real / 3,328 B bootstrap.
- **Finding:** the dominant restore cost is re-parsing boot DEX (~13 ms), not
  copying the arena (memcpy surrogate 581 ns); T-43/T-44 must share the parsed
  provider.
- **Fidelity / evidence:** `native-runtime`; `results/snapshot/REPORT.md`,
  `results/snapshot/probe-{bootstrap,real}.json`.
- **Regression / false pass:** determinism/idempotence/fingerprint-refusal
  invariants each tested.
- **Conclusion / next:** T-43 framework snapshot needs a shared boot-DEX provider
  and re-seed hooks.

## E2.4 — V1 benchmark harness + first measured rows — MEASURED

- **Date / commit:** 2026-11-10; `7f7b729`…`ced0d70`.
- **Question / goal:** measure every metric the harness can support vs the
  legacy ACVM baseline.
- **Method:** `tools/bench-acos.sh` (`>=20` samples, p50/p95), artifacts under
  `results/bench/`.
- **Result (from `results/bench/REPORT.md`):** M1 157.6 MiB vs 206.0 MiB
  (`-23.5%`), M2 N=5 43.032 MiB/session vs 78.7 MiB (`-45.3%`), per-session N=1
  157.602 MiB vs 130.3 MiB (`+21.0%`, worse); M3–M8/M10/M11 not measurable.
- **Finding:** memory at N≥2 beats legacy; N=1 is worse; no correctness/frame
  metric can be measured yet.
- **Fidelity / evidence:** `native-runtime` (not ART-verified);
  `results/bench/REPORT.md`, `run-2026-11-10-201637.json`,
  `results/bench/MEMORY_PROFILE.md`.
- **Regression / false pass:** gate `tools/bench-acos.sh --check` against
  `results/bench/baseline.json`.
- **Conclusion / next:** measure M3–M11 as the capabilities land.

## E2.5 — Interpreter performance vs `java -Xint` — MEASURED (targets missed)

- **Date / commit:** 2026-11-10; `2563a86` (perf merge).
- **Question / goal:** close the interpreter gap (ADR-0013 step 1) and hit the
  ≥4×/≥5× speedup targets.
- **Method:** `tools/bench-vm.sh`, 10 reps, host OpenJDK 17; `crates/acos-vm/PERF.md`.
- **Result:** BenchFib 4.17× (528 ms vs 341 ms `-Xint`, 1.55× slower);
  BenchLoop 3.48× (1,696 ms vs 411 ms, 4.13× slower); BenchAlloc 3.58×
  (2,109 ms vs 1,216 ms, 1.73× slower); BenchString 3.30× **faster** than
  `-Xint`. Target ≥4× Fib met; ≥4× Alloc and ≥5× Loop not met.
- **Finding:** per-opcode dispatch (~2.7–3.3 ns/insn) and call overhead
  (~50–63 ns) are the floor; `unsafe` bounds-check removal gave no gain.
- **Fidelity / evidence:** `jvm-reference`; `crates/acos-vm/PERF.md`,
  `results/vm/bench-2026-11-10-160048.json`.
- **Regression / false pass:** fast-dispatch path equal to general path
  (`35846b8`/`3c6d3a5`).
- **Conclusion / next:** compact pre-decoded ops, zeroing-free frames, and a
  hash-backed `HashMap` corelib.

## E2.6 — ART oracle captures + render text parity — MEASURED

- **Date / commit:** 2026-11-10; `ced0d70`/`40f075c`.
- **Question / goal:** authoritative references and text fidelity.
- **Method:** `tools/oracle/*` (emulator, capture, `compare-jvm-art`);
  `cargo test -p acos-render`.
- **Result:** art-reference captures for `hello-views`, `hello-jni`, `counter`,
  `todo-list`, `multi-activity`, `form-validation`; JVM vs ART: 19/20 identical
  stdout, `20_uncaught` stderr differs; renderer 88 tests, text advance within
  0.3% of the ART UI dump (height delta 15.6% is widget padding).
- **Finding:** ART and the JVM agree on the pure-Java console tier; the
  authoritative tier is now available for app references.
- **Fidelity / evidence:** `art-reference`; `fixtures/reference/**`,
  `results/oracle/jvm-vs-art.md`, `results/render/README.md`.
- **Regression / false pass:** renderer golden/contract tests; ART captured 3×
  with no non-determinism (`results/oracle/dexprogs-nondeterminism.txt`).
- **Conclusion / next:** `physical-device` still not captured; bind the renderer
  to the framework graphics natives (M3).

## E2.7 — Exec-path early heap release: one post-first-frame ART collection — MEASURED

- **Date / commit:** 2026-11-12; `d1a541e` + the Z7 worktree changes (`zygote-z7-lifetime`).
- **Question / goal:** the Z6 review found the zygote's memory win only appears after ART's
  +10 s post-fork footprint schedule; can the *exec path itself* (today's production path) be given
  its own cheap heap release, and how much whole-system memory and verified-task throughput does
  it recover without a zygote?
- **Method:** one release binary, `counter.apk`, real ART. Arm (a) = today's exec path, no knobs.
  Arm (a') = the same path with `ACOS_ART_GC_AFTER_FIRST_FRAME=1`: right after the guest's first
  live frame a daemon thread calls `System.gc()` then
  `VMRuntime.getRuntime().requestConcurrentGC()`. Lifetime-matched whole-system ΣPss at 0.5/2/5/15/
  60 s after each child's first frame at N=5 and N=10 (two interleaved reps, N child-anchored
  snapshots per cell); a P5-style agent loop (launch → inspect → tap → assert → reset/relaunch,
  N=10, 5.2 min per arm, fresh launch per task) with the whole-tree PSS integral as denominator.
- **Inputs:** `fixtures/build/apps/counter.apk`; API-34 sysroot `/home/Qui/Projects/acos/.cache/art`;
  x86_64 host; `CARGO_TARGET_DIR=.cache/z7-target`, release.
- **Result:**
  * Whole-system ΣPss at N=10, 0.5/2/5/15/60 s: (a) 900.6 / 903.9 / 925.1 / 904.0 / 956.8 MiB;
    (a') 613.3 / 617.3 / 617.2 / 619.4 / 630.7 MiB → **−31.9 % / −31.7 % / −33.3 % / −31.5 % /
    −34.1 %**. At N=5: 508.3…540.8 → 381.1…390.3 MiB (−24.9 to −27.8 %).
  * Loop N=10 ≥5 min: (a) 210/210 tasks, 45.0 tasks/GiB-min, p50/p95 9.04/15.07 s;
    (a') 210/210 tasks, **67.7 tasks/GiB-min** (+50.4 %), p50/p95 8.85/15.06 s; 0 false passes.
  * `System.gc()` alone is inert on this image: `art.gc.gc-count` 3 → 3, 0 ms, no ΣPss change.
    `requestConcurrentGC` moves the count 3 → 4 and returns the pages within one 1 s sample.
- **Finding (proven vs hypothesis):** an explicit ART background collection at the first frame is
  a real exec-path optimisation: it recovers ~287 MiB whole-system at N=10 and ~50 % more verified
  tasks/GiB-min. Proven for the counter fixture, this image and this host. Hypothesis, not measured:
  the same gain on heavier apps, whether a target-footprint argv flag is cheaper or safer, and
  whether a production reset can issue the request without a guest-side hook.
- **Fidelity / evidence:** MEASURED on real ART, `real-ART` fidelity, 0 false passes.
  `results/art-runtime/Z7.md`; `results/art-runtime/evidence/Z7/{run-lifetime.sh,run-loop.sh,
  summarize.py,summary.json,table.txt}`; raw JSONs in `.cache/z7-runs/`.
- **Regression / false pass:** every task verified `0` → real tap → `1` plus a non-empty C1
  screenshot; a task that only acknowledges is not a pass. No false passes in 210+210 tasks.
- **Conclusion / next:** keep the hook as an off-by-default research seam
  (`ACOS_ART_GC_AFTER_FIRST_FRAME`, listed in `docs/SECURITY.md`). It applies to production's exec
  path without a zygote; the zygote comparison (Z7) uses it as the fair denominator, because the
  gate must compare against an optimised current implementation, not an unoptimised one.
