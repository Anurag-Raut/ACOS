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

**Hill-climb entries (from 2026-11-12, owner-mandated; every experiment, failed or not):**
add these fields to the template above: Experiment ID + name; responsible agent;
bottleneck investigated; baseline (p50/p95); hypothesis + proposed solution;
implementation + commits; benchmark method + environment + host load; before/after
p50/p95, memory, CPU, correctness (false passes); result = accepted | rejected |
inconclusive | needs-investigation; why it worked/failed; trade-offs/regressions;
follow-ups. Keep logs/artifacts under `results/hill/<ID>/`. Update the
**Leaderboard** and **Backlog** sections below in the same commit.

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

## E2.7 — GL1: session lifetime vs. `PR_SET_PDEATHSIG`, and bounded destruction — MEASURED

- **Date / commit:** 2026-11-12; branch `gaten-gl1-lifetime` off `main` `bf70bb7`.
- **Question / goal:** (a) must a kept ART session survive the client connection that launched it, without
  losing the "no orphan when the daemon is SIGKILLed" guarantee? (b) can explicit destruction be
  deterministic and fast for an app whose threads never exit (RN/Hermes), instead of waiting on them?
- **Method:** `acos serve --engine art`; `acos launch --keep` in process A (disconnects), then
  `acos session status/observe --tree/tap/observe --screenshot` in process B; `session destroy` timed;
  `kill -9` on the daemon with a live session. Two binaries from the same tree, one line apart
  (`command.spawn()` on the calling thread vs. `spawner::spawn(command)`), same fixture, same commands.
  Fixtures: `counter`, and a new `thread-heavy` (12 non-daemon threads that never exit), added to
  `tools/build-apps.sh`. All runs through `tools/art-runtime/run-exclusive.sh --task GL1 --
  tools/guarded.sh --mem 8G --time N`.
- **Result:** before, every client-B call answered `GUEST_EXITED … exited: signal 9` (the PDEATHSIG fired
  when the launch connection's *thread* ended); after, status/tree/tap/screenshot all pass, `0` → `1`
  with a 9.5 MB PNG, and `destroy` leaves no `acos-art`. Daemon `kill -9`: guests gone in **1 ms**.
  Destroy wall p50/p95: `counter` 18/19 ms, `thread-heavy` 20/20 ms; the forced-kill path
  (`ACOS_ART_TEARDOWN_GRACE_MS=0`) reports `lifecycle: "killed"` in 12 ms, still clean.
- **Finding:** `PR_SET_PDEATHSIG` is per parent **thread**; a per-connection thread is the wrong parent
  for a session owner. Spawning from one process-lifetime thread keeps the guarantee exact (process death
  → guest death) and costs nothing. Destruction never needed to join app threads: Android kills, and the
  10 s flat deadline was replaced by a 300 ms lifecycle grace (≈15× the measured 20 ms cost) followed by
  the same group kill.
- **Fidelity / evidence:** `art-reference`; `results/art-runtime/GL1.md`,
  `crates/acos-cli/tests/gl1_kept_session.rs` (4 ignored real-ART tests),
  `engine::art::tests::a_child_spawned_through_the_spawner_survives_the_asking_thread` (unit,
  with the direct-spawn negative control).
- **Regression / false pass:** the SIGKILL test asserts the orphan guarantee still holds; the destroy
  test asserts `survivors == []`, `session_leftovers == []`, `verified == true` and the id is unlisted for
  both fixtures; the forced-kill test proves the bound does not depend on the guest cooperating.
- **Conclusion / next:** session ownership is now process-scoped, not connection-scoped. Next: deliver
  the activity's own `onDestroy` before the kill (guest-side, `crates/acos-art/guest`), so the
  `lifecycle` label can mean "onDestroy returned" rather than "the launcher ended by itself".

## E3.1 — H6: reproducible all-metric baseline + regression bench (pre-H2) — MEASURED

- **Experiment ID / name:** H6 — Stage 1 baseline and the automated regression bench.
- **Responsible agent:** H6 (worktree `bench-h6-baseline`, branch of main after the GL1 merge).
- **Bottleneck investigated:** after P9/P10/P11/GL1 there was no single reproducible
  measurement of every tracked metric, no stored baseline and no automatic
  regression flag; the Leaderboard mixed numbers from different builds/methods.
- **Baseline (p50/p95, old Leaderboard):** M3 463 ms (warm cache), M4 440/184 ms,
  M5 24.4 ms, M6 463/195 ms, M7 151/0.2 ms, M8 955/70.9 ms, memory 100.9 MiB per
  extra session (O2 N=5), loop 2.21 s/task and 136.8 (N=1)/188.0 (N=5)
  verified tasks/GiB-min, teardown 18–20 ms, 0 false passes.
- **Hypothesis + proposed solution:** one script that (a) drives the real
  `acos serve` transport for every latency/loop metric, (b) measures M1/M2 at
  N=1/5/10 with both cgroup anon+shmem and Pss, (c) measures idle/active CPU and
  wakeups, (d) runs the P11 false-pass and scenario suites, and (e) stores a
  merged baseline whose per-metric thresholds are derived from the measured
  rep-to-rep spread. Reuse `benchlib.py`, `d2_warm.py`, `agent_loop.py`, P5/P7/P11
  harnesses; change no engine code.
- **Implementation + commits:** branch `bench-h6-baseline` (this commit):
  `tools/bench/acos-bench.sh`, `tools/bench/acos_bench.py`,
  `results/bench/BASELINE.json`, `results/bench/baseline-20261112T200322Z.{json,md}`,
  raw per-phase samples in `results/bench/run-20261112T200322Z/`,
  `results/bench/H6.md`. Measured build: `32a0708fcca3` (pre-H2), release
  `acos` sha256 `5d42c4f0…`, `ACOS_STRICT_SETTLE` at the tree default (event).
- **Benchmark method + environment + host load:** 3 interleaved repetitions;
  each ART phase is its own `run-exclusive.sh --task H6` +
  `guarded.sh --mem 8G` (memory phase 16G) acquisition, so the lock is never held
  across the bench and every acquisition stayed < 10 min. Scratch under
  `/home/Qui/Projects/acos/.cache/h6/`; `ACOS_FONT_DIR` set, no `ACOS_BOOT_DIR`.
  Host: Ryzen 9 9950X, 32 threads, 61,908 MiB; load1 ≤ 3.1 in 17/21 phases and
  5.8–8.2 in rep 3's loop/p11/scenarios group (the 4 `noisy` phases, recorded);
  MemAvailable 44–47 GiB. Pooled n = 60 per latency metric (3×20), 63/75 loop
  tasks, 3 samples per memory/CPU row.
- **Before/after p50/p95:** M3 **406 ms** (evicted; old 463 was warm-cache),
  M4 **309/175 ms**, M5 **24.3 ms**, M6 **342/210 ms**, M7 **41.1/0.44 ms**,
  M8 counter **956/44.3 ms** and form **948/45.0 ms**, tap ack **5 ms**,
  teardown **12 ms**, M1 **98.9 MiB**, M2 **97.3/98.0/97.7 MiB** cgroup at
  N=1/5/10 (Pss 213/108/94 MiB, labelled), idle CPU **0.61/2.44/4.47 %** of one
  core (0.61/0.49/0.45 per session), idle wakeups **34/90/161 s⁻¹**, active CPU
  during strict taps **45.8 %** of one core, loop **1.438 s/task**, **0.654
  tasks/s / 280.5 tasks/GiB-min** (N=1) and **2.677 tasks/s / 664.0
  tasks/GiB-min** (N=5). False passes **0/105 P11 reps**, P11 scenarios
  **0/12 failed**, loop **0 false passes**; loop N=5 verified **74/75** (one
  honest assert failure, see H12).
- **Result:** **accepted** baseline (`results/bench/BASELINE.json`); the tool and
  its `--quick` subset (measured 3m03s under the lock) are committed.
- **Why it worked:** the strict numbers dropped for the right reasons — P11's
  event settle removed the fixed 150 ms quiet window (M7 151 → 41 ms; M4 strict
  440 → 309 ms; M3/M6 strict down ~12–26 %), P10's event probe, P9's fast paths,
  W1/O2's memory work (M1 117.5 → 98.9 MiB) and GL1's bounded teardown
  (18–20 → 12 ms). The loop fell 34.9 % and throughput/GiB-min doubled at N=1.
- **Trade-offs / regressions:** M6 fast is +7.7 % vs P9 but inside its 14 %
  derived threshold (one rep 197 ms) — needs an A/B; M7 fast is +0.24 ms
  absolute (tie); rep 3 ran under load1 5.8–8.2 (recorded, p50s within 2 % of
  the clean reps). No engine code changed.
- **Follow-ups:** re-run the same command on merged main after H2 (early GC) and
  P12; H12 (N=5 assert flake); H9/H10/H11 below.
- **Fidelity / evidence:** `art-reference` (real ART, release); artifacts above.

## Leaderboard (best measured configuration per metric; update on every accepted experiment)

Central objective: verified agent tasks per second per GiB (tasks/GiB-min).

All rows below are the H6 baseline on **pre-H2** main `32a0708` (3 reps, release,
`ACOS_STRICT_SETTLE=event`), unless the row says otherwise. H2 (early GC) was
merged after that build; re-baseline on merged main.

| Metric | Best value | Config / commit | Experiment | Date | Δ / why |
|---|---|---|---|---|---|
| M3 cold start | **406 ms (page cache evicted)** | main `32a0708` pre-H2 | H6 | 2026-11-13 | −12.3 % vs 463 warm-cache (P11 event settle + P10 probe) |
| M4 warm launch | **strict 309 ms / fast 175 ms** | main `32a0708` pre-H2 | H6 | 2026-11-13 | −29.8 % / −5.1 % (event settle) |
| M5 screenshot | **24.3 ms** | main `32a0708` pre-H2 | H6 | 2026-11-13 | tie with 24.4 (P2/W1) |
| M6 reset | **strict 342 ms / fast 210 ms** | main `32a0708` pre-H2 | H6 | 2026-11-13 | strict −26.2 % (event settle); fast +7.7 % inside 14 % noise, A/B needed |
| M7 UI tree | **strict 41.1 ms / fast 0.44 ms** | main `32a0708` pre-H2 | H6 | 2026-11-13 | strict −72.8 % (P11 event settle removed the 150 ms quiet window); fast +0.24 ms absolute |
| M8 tap → settled | **counter 956 ms strict / 44.3 ms fast; form 948 / 45.0 ms** | main `32a0708` pre-H2 | H6 | 2026-11-13 | counter fast −37.6 %; strict unchanged (framework press/elevation animator) |
| Tap acknowledgement | **5 ms** | main `32a0708` pre-H2 | H6 | 2026-11-13 | tie (P1) |
| Memory / extra session | **97.3 (N=1) / 98.0 (N=5) / 97.7 (N=10) MiB cgroup anon+shmem**; Pss 213/108/94 MiB | main `32a0708` pre-H2 | H6 | 2026-11-13 | −2.8 % at N=5 vs O2 100.9; M1 98.9 vs 117.5 (W1/O2) |
| Idle CPU / wakeups | **0.61 / 2.44 / 4.47 %** of one core at N=1/5/10 (0.61/0.49/0.45 per session); **34 / 90 / 161** wakeups/s | main `32a0708` pre-H2 | H6 | 2026-11-13 | new rows; wakeups ~16 s⁻¹ per session at N≥5 |
| Teardown | **12 ms** (`teardown_ms`, lifecycle) | main `32a0708` pre-H2 | H6 | 2026-11-13 | −33…−40 % vs 18–20 (GL1) |
| Agent loop | **1.438 s/task; 0.654 tasks/s, 280.5 tasks/GiB-min (N=1); 2.677 tasks/s, 664.0 tasks/GiB-min (N=5)** | main `32a0708` pre-H2 | H6 | 2026-11-13 | −34.9 % latency; +105 % / +253 % tasks/GiB-min (P9/P10/P11/W1/O2) |
| False passes | **0** (P11 0/105; scenarios 0/12; loop 0) | main `32a0708` pre-H2 | H6 | 2026-11-13 | must stay 0; N=5 loop verified 74/75 (H12) |

## Backlog (optimization hypotheses; rank = impact × confidence ÷ (cost × risk))

| ID | Hypothesis | Metric | Expected | Confidence | Cost | Risk | Status |
|---|---|---|---|---|---|---|---|
| H9 | **Memory per session (98 MiB, flat N=1/5/10) is the GiB denominator.** Enable B3's shared uncompressed boot image first (−13.6 MiB/session, measured), then ART post-first-frame trim / zygote CoW (Z6: −25–60 % at N≥5) | M2, tasks/GiB-min | −14 % (B3) … −50 % | high (B3) / medium (zygote) | low (B3 flag) / medium | medium (fixed image address/ASLR; zygote security F1) | **#1** |
| H10 | **Strict tap is 65 % of the loop (926–940 ms):** the framework press/elevation `ValueAnimator` draws ~0.9 s. Adopt P12 `animations:off` for non-assertion steps and/or strict only for assertions (fast for navigation) | M8, loop | 1.44 s → ~0.55 s; +2.3× tasks/GiB-min | high | low | medium (verification semantics: fast never backs an assertion) | **#2, after P12** |
| H11 | **Reset is 24 % of the loop (342 ms):** fresh ART boot + framework + Activity + strict bind. Zygote/shared image, or fast reset for non-asserting steps | M6, loop | −150–200 ms (−10…14 % loop) | medium-high | medium | medium | **#3; overlaps H9's zygote arm** |
| H12 | **N=5 strict assert flake:** 1/75 tasks failed the post-tap count on a strict tree, false_pass 0, did not reproduce (25/25 in reps 2–3). Reproduce with the recorded failing-task steps; check probe/store race under concurrency | loop verified rate | 74/75 → 100 % | medium | low | low | needs-investigation |
| H1 | Strict numbers drop after event settle; re-measure M3–M8 and take cheap wins | M4/M6/M7/M5 | — | — | — | — | **done (H6)**: M7 151→41, M4 440→309, M6 463→342 |
| H2 | Early post-first-frame concurrent GC (Z7) on the exec path | M1/M2 | −23–30 MiB/session | high | low | low | **merged on main after this baseline** (pre-H2 numbers), re-baseline |
| H4 | Idle CPU: 0.45–0.61 % per session and ~16 wakeups s⁻¹ per session (N=10: 4.5 %, 161 s⁻¹). Remove the 250 ms probe fallback and per-session timers | CPU, wakeups | lower wakeups; small at N=10 now | medium | low | low | queued (after H9/H10) |
| H5 | Agent loop with animations:off + event settle | loop | 2.2 → <0.8 s | medium-high | low | low | **folded into H10** |
| H6 | Nightly metric regression bench | all | guard | high | done | low | **done**: `tools/bench/acos-bench.sh` + `BASELINE.json` |
| H7 | Shrink per-session boot-image private memory | M2 | −10–25 MiB | medium | medium | medium | **folded into H9** (B3 is the measured first step) |
| H8 | Faster PNG (SIMD deflate) / region screenshots | M5 | 24 → 10–15 ms | medium | medium | low | backlog (M5 is 2 % of the loop) |
| H3 | Warm exec pool | M3/M4 | −73 ms launch | — | — | +idle memory | **declined by owner 2026-11-12** (no spare processes) |
