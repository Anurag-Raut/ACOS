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

## E13.1 — Gaten (RN 0.86.3 / Fabric / Hermes) on real ART: routing, then a
viewBox defect and an insets defect — MEASURED (two defects open)

- **Date / commit:** 2026-11-12; GR1 worktree `gaten-gr1-rn-art` (GR1 commits 1–4),
  engine base `bf70bb7`.
- **Question / goal:** G1 (§3.1–3.2) concluded that a React Native ≥0.82 APK can
  never reach real ART and needs a rewritten RN driver. Is that true of the
  *engine* or only of the *route*?
- **Inputs / method:** Gaten `dev` `f344272` x86_64 release (unsigned,
  sha256 `e04c7ffc…`); `acos-art launch` interactive, one guest per run, all runs
  under `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh --mem 8G`.
  Routing read in `crates/acos-mcp/src/session_backend.rs` and
  `crates/acos-cli/src/engine/serve.rs`. Reference device: moto g35 ZA222WGJPP,
  same source commit rebuilt as `com.gaten.acosref` (arm64, identical JS bundle
  sha256 `3c5428ea…`).
- **Baseline (before GR1):** every RN APK is routed to `RnSession` (frozen Rust
  VM + hand-written `BridgeProbe.java`), `serve` *refuses* `engine: "art"` when
  an `rn`/`flutter`/`native` flag is present, and the CLI has no way to ask for
  ART at all; Gaten release dies at `UNSUPPORTED_RN_DRIVER` in 401–488 ms.
- **Hypothesis:** the ART engine already boots an arbitrary APK's own launcher
  Activity and loads its `lib/<abi>/` closure, so an RN APK needs *routing*, not
  a new driver.
- **Result:** with `acos launch --engine art` (new flag) the request goes to the
  ART launcher: `stage_reached=first_frame`, `engine:"art"`,
  `engine_frozen:false`, **764 ms** (daemon) / 714 ms (launcher ready line).
  The guest installs **23 native libs** (`libhermesvm`, `libfbjni`,
  `libappmodules`, `libonnxruntime`, …) and Gaten's own `MainApplication` /
  Fabric mount real `android.view.View`s: 100+ node tree with
  `com.gaten:id/action_bar_root`, `SvgView` nodes and Japanese text; C1
  screenshots are 1080×2209 PNGs; a `tap` is accepted
  (`gesture.acks[0].handled=true`, `latency_us≈15 100`).
- **Second measurement (visual verification, phone-matched geometry):** ACOS
  boots `1080x2292 @ 400 dpi` with the phone's 108 px status inset after the
  geometry plumbing below. Welcome screen SSIM `0.8485`, 14.6 % of pixels differ
  by >16/255, and two defects explain all of it:
  **(a)** the wordmark's ink is 631×152 px where the device draws 252×60 px, and
  `fixtures/apps/svg-viewbox` proves why: `canvas=1080x2292` while
  `view=270x116` (`canvasIsViewPx=false`, `scale=0.2357` vs the correct
  `0.0589`). The launch guest draws through one window-sized **software** canvas
  (`FLAG_HARDWARE_ACCELERATED` deliberately cleared, P4b §4), so there is no
  per-view `RenderNode` and `Canvas.getWidth()` is the window's;
  react-native-svg sizes its viewBox transform from exactly that number. The
  boxes are identical on both systems (270×116 px), so this is native drawing,
  not layout or metrics. Requesting the flag makes the guest exit status 1 —
  there is no HWUI path to fall back on.
  **(b)** the footer is exactly 60 px (24 dp) lower on ACOS although the button
  is pixel-identical in size (387.6×80.4 dp): the device gives the app a
  full-display window (`frame=[0,0][1080,2400]`, edge-to-edge) and *reports*
  status 108 / gesture-bar 24 dp, while ACOS **carves** the window
  (`DecorView [0,108][1080,2400]`) and reports zero insets; `--nav-inset 60`
  cannot help because the frame is carved, not inset.
- **Finding (proven):** G1's "RN cannot reach ART" is a routing statement, not a
  capability statement — RN 0.86 Fabric/Hermes runs on real ART with no RN
  driver. Two defects remain, both ACOS-side and both at the graphics/window
  layer: (i) the launch guest has no per-view RenderNode canvas (wordmark/SVG,
  and any library reading `canvas.getWidth()`); (ii) the WMS shim carves the app
  window instead of reporting insets (all bottom-anchored layout, and
  `screenHeightDp` consumers).
- **Fidelity / evidence:** `art-reference` (real ART guest, host-observed
  pixels); `results/gaten/GR1.md`,
  `results/gaten/GR1-visual-metrics.md`,
  `results/gaten/evidence/GR1/` (launch envelope, trees, PNGs,
  `svg-probe.json`, `visual/welcome/{device,acos,diff,side-by-side}.png`,
  `metrics.json`, `elements.json`).
- **Regression / false pass:** new `fixtures/apps/svg-viewbox` +
  `scenarios/svg-viewbox.yaml` (asserts `canvasIsViewPx=true`,
  `inkInsideView=true`; **red today by design**, kept out of the gate list);
  `scenarios/gaten-home.yaml` defines the owner flow end to end. No Gaten
  source, bundle or APK was modified.
- **Conclusion / next:** (1) give the launch guest the real HWUI path
  (`FLAG_HARDWARE_ACCELERATED`, RenderThread, `beginRecording(w,h)` per view,
  replay into the C1 buffer) — that is the single fix for the wordmark and for
  every SVG/canvas-drawing RN library; (2) model the app window as
  full-display + reported insets in `acos-wm` (108 px status, 24 dp gesture
  bar); (3) then the onboarding `tap` that lands but does not advance is the
  next wall to attack (it is delivered and `handled`, so the input channel is
  fine — RN's JS responder path or the app's `GatenAppInfo.setOnboardingStep`
  native call is next).

## E2.8 — Exec-path early heap release: one post-first-frame ART collection — MEASURED

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
  run-cycles.sh,run-form.sh,summarize.py,summary.json,table.txt}`; bulky raw JSONs were removed
  after the batch, the committed summary carries every cell.
- **Regression / false pass:** every task verified `0` → real tap → `1` plus a non-empty C1
  screenshot; a task that only acknowledges is not a pass. No false passes in 210+210 tasks.
- **Conclusion / next:** keep the hook as an off-by-default research seam
  (`ACOS_ART_GC_AFTER_FIRST_FRAME`, listed in `docs/SECURITY.md`). It applies to production's exec
  path without a zygote; the zygote comparison (Z7) uses it as the fair denominator, because the
  gate must compare against an optimised current implementation, not an unoptimised one.

## E5.1 — K2/R4: guest crash evidence carries the cause chain and stack — MEASURED (accepted)

- **Date / commit:** 2026-11-12 `6a08341`.
- **Agent / bottleneck:** K2; the K1 corpus had 9 apps whose first failing call
  was hidden behind a framework wrapper (`Unable to create application …`).
- **Baseline:** K1 `guest_crash` was one line, host-truncated at 600 chars; no
  cause chain, no stack, no logd tail.
- **Hypothesis + solution:** print `exception`, `stack` and `causes[]` from the
  fatal throwable in `LaunchMain`, keep the whole line in `acos-art`, and attach
  `logd_tail`/`stdout_tail`/`stderr_tail` to `session-exit.json`.
- **Method:** targeted re-run of 9 hidden-cause apps (`runs-r4`), one guarded
  batch, host RAM ≤ 27 %.
- **Result:** all 9 reclassified. `com.fsck.k9` → missing `power` service;
  `com.nextcloud.client` → missing `account`; `com.dot.gallery`,
  `de.westnordost.streetcomplete`, `org.breezyweather`, `org.fdroid.basic` →
  `jobscheduler`; `me.zhanghai.android.files` → `lateinit application`;
  `io.element.android.x` → `ProcessLifecycleInitializer`; `net.mullvad` →
  notification channels. No funnel change by design.
- **Fidelity / evidence:** `results/compat/K2.md` §2,
  `.cache/k2/runs-r4/<pkg>/art-sessions/*/art-frames/session-exit.json`.
- **Result:** accepted. **Trade-offs:** larger `session-exit.json` (K2 lines are
  still bounded at 32 KiB), no behaviour change. **Follow-up:** include the
  signal/native trail for the remaining SIGABRT/exits.

## E5.2 — K2/R1: publish the services Android always publishes — MEASURED (accepted)

- **Date / commit:** 2026-11-12 `cdc1a69` (+`11fabb7`, `c389090`).
- **Agent / bottleneck:** K2; R1 was K1's largest wall (13/42 apps), a null
  where Android never returns one.
- **Baseline:** K1 first frame 5/42; 13 apps died on
  `IStorageManager.getVolumeList` null (5), `getSystemService(JobScheduler)`
  null (5), `ShortcutManager` null (1), `PowerManager`/`AccountManager` null
  (2 through DI wrappers).
- **Hypothesis + solution:** real `StorageVolume` model (one mounted emulated
  volume), real `IJobScheduler`/`IShortcutService`/`IPowerManager`/
  `IAccountManager`/`IThermalService`/`IAlarmManager` stubs, `netstats` binder
  and `permissionmgr` alias. Offline single-user answers, never fabricated
  passes.
- **Method:** R1-only targeted batch (13 apps), then full corpus after the
  follow-up codec fix; guarded 8G, batch 42.
- **Result:** all 13 left their original null call. Full ladder gained editor,
  coffee, money.manager, markor; AppManager and mullvad reached
  tree+screenshot; antennapod now completes onCreate and resumes. Full corpus
  funnel after the combined wall chain: first frame **13/42 (31.0 %)** from
  5/42, tap **7/42 (16.7 %)** from 2/42.
- **Fidelity / evidence:** `results/compat/K2.md` §3,
  `.cache/k2/runs-final2/`, `results/compat/k2-results.json`.
- **Result:** accepted. **Trade-offs/regression:** publishing JobScheduler and
  Shortcut exposed missing `JobInfo`/`ShortcutInfo` payload codecs; `11fabb7`
  consumes those list arguments opaquely and the platform answer (success) is
  returned. Feeder and Aegis were restored (Aegis now full pass).
  **Follow-up:** real filesystem provisioning for
  `/storage/emulated/0/Android/data/<pkg>` (plain).

## E5.3 — K2/R2: notification channel parcels and get-channel queries — MEASURED (accepted)

- **Date / commit:** 2026-11-12 `3484c3b`, correction `e3d0c47`, dispatch fallback `fab6f64`.
- **Agent / bottleneck:** K2; 6/42 apps (14 %) were rejected at
  `INotificationManager.createNotificationChannels`.
- **Baseline:** `IllegalArgumentException: bad request for …createNotificationChannels`
  (no `ParceledListSlice`/`NotificationChannel` codec); get-channel 42/53
  unsupported.
- **Hypothesis + solution:** implement `ParceledListSlice` inline decoding and
  the API-34 `NotificationChannel`/Group `writeByte`-presence layouts, store
  channels per package, answer 42/53/list/counts, accept cancel/enqueue.
- **Method:** targeted 6-app batch, then 19-app R2/R3 batch, full corpus final;
  parcel round-trip tests (`tests/k2_notification.rs`) run without the corpus.
- **Result:** all 6 left the create call; kdeconnect reached the full ladder.
  gotify → MaterialButton inflate; anki → app exit 10; tailscale →
  AndroidKeyStore; vlc → PMS error-class/`com.google.android.gsf`; wikipedia →
  alarm resolved then signal; fdroid.basic → permission/IntentSender.
- **Fidelity / evidence:** `results/compat/K2.md` §4,
  `crates/acos-services/tests/k2_notification.rs`.
- **Result:** accepted. **Why it worked:** the first correction (AudioAttributes
  field order) was exactly the image-reader mismatch that produced K1's "bad
  string" error. **Trade-offs:** channels are stored per app run (no persistent
  notification DB, like a fresh device); no status bar is rendered.

## E5.4 — K2/R3: answer the read-only AMS/PMS/ATMS/connectivity queries — MEASURED (accepted)

- **Date / commit:** 2026-11-12 `35374c2`.
- **Agent / bottleneck:** K2; 7/42 apps blocked on a read-only transaction the
  tables knew but no handler answered.
- **Baseline:** 7 first-failing `UNSUPPORTED_ANDROID_API` transactions
  (`getMemoryInfo` 77, `getRunningAppProcesses` 86, `getTaskForActivity` 22,
  `getRequestedOrientation` 30, `getPackageInstaller` 133,
  `getPermissionControllerPackageName` 140, `getNetworkCapabilities` 16).
- **Hypothesis + solution:** answer each with the device's real offline state:
  `/proc/meminfo`, empty process list, task id 1/-1, unresolved orientation,
  empty installer, the AOSP permission-controller package, null network
  capabilities; parcel codecs for MemoryInfo/InstallSourceInfo/Network/Messenger.
- **Method:** full corpus final2, guarded; unit round-trips in `tests/k2_r3.rs`.
- **Result:** money.manager full ladder; AppManager and joplin render + tree +
  screenshot; fluffychat/termux/unciv/owncloud/aurora moved to their next
  (mutating/app) wall. No new false pass.
- **Fidelity / evidence:** `results/compat/K2.md` §5, `tests/k2_r3.rs`.
- **Result:** accepted. **Trade-offs:** `requestNetwork`, `setRequestedOrientation`
  and installer mutations remain structured unsupported (they change device
  state or need a codec/callback that does not exist). **Follow-up:** PMS
  `NameNotFoundException` error class, `TaskDescription` drawable codec,
  `IPackageInstaller` read methods.

## E13.2 — Gaten past onboarding on real ART: the press runs, and the tree lies about the
screen — MEASURED (false pass found; one layout wall open)

- **Date / commit:** 2026-11-13; GR2 worktree `gaten-gr2-onboarding` (off `main` `5b8189b`, GR1
  merged). Engine unchanged.
- **Question / goal:** GR1 §7 item 1 said the onboarding press is delivered and `handled` but the
  screen never advances, suspecting `GatenAppInfo.setOnboardingStep` (a `SharedPreferences` write),
  a missing binder call, a JS thread that does not drain, or an animation that never commits. Which
  layer is responsible, and does the owner flow (welcome → Skip ×3 → Home → Analytics) verify?
- **Inputs / method:** the same read-only x86_64 release APK (`f344272`, sha256 `e04c7ffc…`);
  `acos launch --engine art --keep --reference` + `tap`/`observe`/`logs` per step, `acos verify
  --engine art scenarios/gaten-home.yaml`; the app's own JS read (read-only) in `.cache/gaten/src-dev`
  (`WelcomeScreen`, `OnboardingFlow.write`, `GatenAppInfoModule.kt`); the guest prefs file on the
  host; logcat via `acos logs`. Six launches, all under `run-exclusive.sh` + `guarded.sh --mem 8G`.
- **Baseline (before GR2):** GR1 stopped at the welcome screen; every tap on it reported
  `NO_FRESH_FRAME` and its "after" capture was the welcome screen with its own loop animation
  running, which looked like "the tap did nothing".
- **Hypothesis:** the native write or the JS press is broken.
- **Result:** rejected. `tap "Set up Gaten"` advances to onboarding page 1 (`1 / 3`) and the skip
  pages advance 1→2→3→Home; after the first Skip the guest's own
  `<data>/runs/<id>/app-data/com.gaten/shared_prefs/gaten_settings.xml` holds
  `<int name="onboarding_step" value="1" />`, so `apply()` and the promise both work.
  `scenarios/gaten-home.yaml` ran **14/14 steps, rc=0** (15.7 s, 14 frames, `artifact_validation ok`)
  and reached Home. GR1's "after" image was the welcome screen's highlight loop, not a transition.
- **Second measurement (the false pass):** the Analytics step passed while its own bound frame was
  still Home. Two ACOS defects do it: **(a)** `tap {text: contains "Analytics"}` resolves to a
  *duplicate* label row at `y 2151..2188` (a hidden copy of the bar) whose centre `(351,2169)` lies
  inside the **Home** segment, whereas the real pressable is `content_desc "Analytics"` at
  `[516,2151][690,2316]` (tap `(603,2233)` selects the tab for real); **(b)** the guest tree reports
  the pager's off-screen, warm-mounted Analytics page (`x=1080..2160`) with `visible: true`, so
  `wait_for "Accuracy"` passes in 13 ms on content that is not on screen. The `tap` step then passes
  with "no visual change; no state change claimed" — the verifier records that assertion but does
  not enforce it.
- **Finding (proven):** the onboarding "wall" was an observation artefact, not a defect: press, JS
  handler, `SharedPreferences` write and page transitions all work on real ART. The flow now verifies
  end to end, but Analytics is proven only by pixels, not by the scenario. Three ACOS-side walls are
  named and sized: the tree's `visible` semantics (off-screen pages), the duplicate bar row that makes
  text selectors ambiguous, and a tap step that may pass with no change.
- **Third finding (new, open):** 2 of 6 launches paint wrong — a text-fragment layout offset of
  exactly one display width (`+1080 px`, the pager's next-page offset) so a button reads "Set up" and
  the highlighted `<Text>` spans vanish (`evidence/GR2/gaten-welcome-broken.png`), and one launch
  painted nothing at all for 60 s with an empty text tree (`gaten-white.png`,
  `verify3/frames/1.png`). Tree and pixels agree with each other and both are wrong: layout, not the
  probe, in the same layer as GR1 §5.2(A) (the guest has no per-view canvas). One launch also wedged
  on the second Skip tap with no reply for >7 min (no guest crash marker).
- **Fidelity / evidence:** `art-reference` (real ART guest, host-observed pixels);
  `results/gaten/GR2.md`, `results/gaten/evidence/GR2/*.png` (welcome, pages 1–3, Home, the false
  Analytics capture, the real Analytics tab, the two broken paints). Verdict, trees, steps, logcat in
  `.cache/gr2/{verify1,verify3,run1…run5}/` (gitignored).
- **Regression / false pass:** the scenario's Analytics tap now names the tab bar's own pressable
  (`content_desc: {exact: Analytics}`) instead of a text node; the false pass itself needs the tree
  fix below. No Gaten source, bundle or APK changed; no ACOS engine change was needed for the
  functional wall.
- **Conclusion / next:** (1) make the in-guest tree probe visibility-aware (clipped bounds vs the
  display) so off-screen pager pages are not `visible` and selectors/`wait_for` cannot match them —
  this kills the whole false-pass class; (2) give the scenario `tap` a way to demand a state change
  (or enforce the `state_change` assertion it already records); (3) then the +1080 px fragment
  layout / first-paint defect, which is the same work item as the per-view HWUI canvas, and is the
  reason a green run is still luck-dependent.

## E2.10 — H2: the post-first-frame ART collection as the exec-path product default — MEASURED (accepted)

- **Date / commit:** 2026-11-13; `54b9c33` on `perf-h2-early-gc` (off `main` `1f21ea7`, includes Z7).
- **Question / goal:** Z7 measured the exec-path early release behind an off-by-default seam. Does
  making it the product default (one `VMRuntime.requestConcurrentGC()` after the first live frame,
  no `System.gc()`, no trim) keep the memory/throughput win without hurting the first agent action,
  correctness or compatibility?
- **Method:** one release binary. The only arm difference is `ACOS_ART_EARLY_GC` (off `0`, on `1`;
  unset is now on). Memory: Z7 `lifetime.py`, whole-tree ΣPss at 0.5/2/5/15 s after each child's own
  first frame, N=1/5/10, **3 interleaved reps** per arm/cell. Loop: Z7 `loop.py`, fresh launch per
  task, N=5 and N=10, **3 interleaved 1-minute reps** per arm; whole-tree PSS integral as
  denominator. First action: one fresh launch per sample, first request cycled strict tree / fast
  tree / tap, n=20 per arm for counter and form-validation. K1: the 5 corpus apps that reach a first
  frame. Host load recorded in `.cache/h2/runs/host.log`; every ART run under
  `run-exclusive.sh` + `guarded.sh` (8G, 24G at N=10).
- **Inputs:** `fixtures/build/apps/{counter,form-validation}.apk`; the 5 K1 APKs; API-34 sysroot;
  x86_64 host; `CARGO_TARGET_DIR=.cache/h2-target`, release.
- **Result:**
  * **Memory per session (ΣPss, median of 3 reps).** N=1: **−28.9 to −30.4 MiB** (12.8–13.3 %);
    N=5: **−144.0 to −153.0 MiB = −28.8 to −30.6 MiB/session** (27.1–28.2 %); N=10: **−281.8 to
    −300.2 MiB = −28.2 to −30.0 MiB/session** (31.2–32.5 %) at every age. `ΣPrivate_Dirty` falls
    with it (N=10, 0.5 s: 750.4 → 468.7 MiB); `SwapPss` is 0.000 in every cell.
  * **Agent loop.** N=5: off 98.8 → on **135.6 tasks/GiB-min (+37.2 %)**, net anon+shmem 119.1 →
    171.0; N=10: 55.7 → **85.9 (+54.2 %)**, net 56.2 → 84.8. Throughput and latency are unchanged
    (N=5 p50/p95 3669.8/5979.6 → 3705.8/5991.3 ms, +1.0 %/+0.2 %; N=10 7072.3/11695.6 →
    7044.9/11623.8, −0.4 %/−0.6 %). **0 false passes** in 640/640 verified tasks.
  * **First action.** Counter strict tree 142.0→141.7 ms, fast 0.2→0.3 ms, tap 945.1→938.9 ms;
    form-validation strict 141.9→141.6 ms, tap 170.8→170.2 ms. 0 failures; 20/20 “on” launches
    logged the release marker. No delay knob was needed.
  * **Compatibility/validation.** 552 cargo tests pass, 0 fail; the 7 gates are green except the
    expected-fail `T03-render-wrong` (rc 1, `ASSERTION_FAILED`); the 11 `p11-*` gate scenarios pass under
    real ART with 0 false passes (progress-timeout is the documented non-pass); K1's 5 first-frame apps keep every stage they reached (first frame
    513/541/1006/1136/584 ms, no ladder regression).
- **Finding (proven vs hypothesis):** proven for these fixtures/image/host. The Z7 finding survives
  the mechanism cleanup and is not an off-by-default curiosity: the default saves ~29 MiB/session
  (not just at N=10) and +37 %/+54 % tasks/GiB-min at N=5/N=10, with no first-action, correctness or
  stage regression. **Hypothesis, not measured:** heavier apps (React Native/Gaten/Compose) at scale,
  a production reset that issues the request without the guest hook, and the 30 s form-validation
  input hang.
- **Fidelity / evidence:** MEASURED on real ART, `real-ART` fidelity, 0 false passes.
  `results/art-runtime/H2.md`; `results/art-runtime/evidence/H2/{summary.json,table.txt}`;
  `research/h2/{first_action.py,run-batch.sh,summarize.py}`. Raw runs under `.cache/h2/runs/`.
- **Regression / false pass:** every loop task is tree → real tap → asserted value → screenshot md5;
  no acknowledgement is a pass. 0 false passes in 640 loop tasks, 0 in the p11 gate corpus, 0 in K1.
  The Z7 form-validation N=5 30 s `:id/name` tap timeout reproduces **with the GC off** (2/5 verified
  in both off runs and both on runs; taps 30.008–30.026 s); it is pre-existing and recorded as H9.
- **Conclusion / next:** **accepted as the product default**; `ACOS_ART_EARLY_GC=0` is the escape
  hatch and the A/B off arm. `System.gc()` and the `trimHeap` branch were removed (Z7 measured both
  inert). Next: H9 (input hang under load), then the same A/B on a heavier app under P12's settle
  work.

## E2.9 — event-only strict settle + per-session animation scales — MEASURED

- **Date / commit:** 2026-11-12; P12 branch `perf-p12-event-only-anim`.
- **Question / goal:** remove the P11 quiet escape hatch (strict = event rule
  only) and let the agent turn Android's three animation scales off for
  functional checks, with every reply stating which mode ran.
- **Method:** real ART, release binary, `results/art-runtime/evidence/P12/`;
  7 core gates + the 12 `p11-*` scenarios in both modes; counter A/B n=30 taps
  per mode (`v2-art-counter` steps 4/7/9); settle-lab indeterminate ProgressBar.
- **Result:** core gates green in both modes (`T03-render-wrong` fails as the
  expected control). Counter tap step **1010 ms p50 / 1031.5 p95 with animations
  on vs 182 ms / 201.6 off** (5.5×). Indeterminate ProgressBar: strict timed out
  at 3098 ms with animations on, **settled in 205 ms with animations off** (the
  bar stops at animator scale 0). Review correction: `verify` fails a
  timed-out settle step as `TIMEOUT` (agent observe still returns the latest
  tree); `p11-progress-timeout` is the non-pass negative control and
  `p12-progress-anim-off` passes 6/6.
- **Finding (review rebase, `main` 1f21ea7):** with P11's event pixel guard on,
  only **4 of the 7 core gates pass**; `w54`, `w42` and `form-validation` fail
  as `TIMEOUT` at a tap that changes no pixels (first focus into an EditText
  with no IME/cursor change). This was latent because `verify` used to accept a
  settle timeout as a pass. Owner decision (2026-11-12): **default the guard
  off** — the four event conditions already prove the draw is after the action;
  requiring different pixels only adds false timeouts. `ACOS_STRICT_SETTLE_PIXEL_GUARD=on`
  restores P11 for experiments. Every strict input reply and verify step now
  reports `pixels_changed:true|false`; a tap with no draw after the action still
  times out. With the adopted default: **7/7 gates green, p12-progress-anim-off
  pass, p11-progress-timeout non-pass TIMEOUT, P11 corpus 0/12 false passes.**
- **Finding:** with animations off, a 150 ms background-thread UI post and a
  1000 ms `postDelayed` scheduled *after* the fast settle were not observed by
  subsequent `wait_for` polling (tree stayed at the pre-update text, no C1
  store). With animations on the ~0.9 s framework animator kept the settle
  alive until they landed, which is why P11 did not see it. Suspect main-looper
  delayed-message dispatch while the guest is idle; H6 should measure it.
- **Fidelity / evidence:** `art`, live; `results/art-runtime/P12.md`,
  `results/art-runtime/evidence/P12/{gates-off.txt,gates-and-ab.txt,ab.json}`.
- **Regression / false pass:** 7 core gates both modes; assertions refuse
  `settled:false`; `T03-render-wrong` still fails. The P11 settle-lab false-pass
  corpus with animations off was not re-run (follow-up: pass `animations:off`
  through the P11 harness). The animations-on corpus is 0/12 false passes under
  the adopted default.
- **Conclusion / next:** animations:off is the right default for functional
  checks (5.5× on a Button tap) and fixes the forever-animating screen;
  investigate idle main-looper timers, then re-run the P11 corpus with `off`.

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

## E13.3 — Gaten launch stability (W6) instrumented; the +1080 px offset did not reproduce and the
real failure is a post-first-frame SIGSEGV — MEASURED (partial: one half reproduced, one not)

- **Date / commit:** 2026-11-13; R2 worktree `render-r2-gaten-layout` (off `main` `2f9bf4a`).
  Diagnostic only: an env-gated guest geometry trace plus its forwarding.
- **Question / goal:** GR2 §4 W6 — in 2 of 6 launches the welcome screen lays out one display width
  off (`+1080 px`, so the button reads "Set up" and the highlighted `<Text>` spans vanish) or paints
  nothing at all for 60 s. Is the first measure pass seeing a different display width/density
  (`DisplayInfo`/`Configuration`/`WindowMetrics` race vs `ACOS_DISPLAY_OVERRIDE`/`LaunchMain`
  geometry), a Fabric/Yoga read of 0 or 2×, a race with the WMS relayout, or a wrong `ReactTextView`
  width — and can ACOS be fixed at that layer without patching Gaten?
- **Inputs / method:** same read-only APK (`f344272`, sha256 `e04c7ffc…`), phone-matched geometry
  (`1080x2400 @ 400 dpi`, 108 px status). New `ACOSLAUNCH_GEOM_TRACE=1` starts a read-only watcher in
  `LaunchMain` that samples `DisplayMetrics`, `Configuration` (`screenWidthDp/HeightDp/densityDpi/`
  `smallestScreenWidthDp`), `WindowManager` window metrics + insets, the decor's measured/laid-out
  size and screen origin, and `ViewRootImpl.mWinFrame`/`mWidth`/`mHeight`, printing one `ACOSGEO`
  line per change and writing `<run>/art-frames/geometry-trace.txt`. 10 launches, each: launch → 2 s →
  `tree` + `screenshot` → trace → destroy, under `run-exclusive.sh` + `guarded.sh --mem 8G`.
- **Result:** **9/10 correct** (243-node tree with `Set up Gaten` and `なるほど、`; frames identical
  ink 0.0939) and **1/10 lost its guest to SIGSEGV (`signal 11`, `GUEST_KILLED_BY_SIGNAL`,
  `exit_code 1`, no OOM) between the first frame and the next observation** — the same shape as GR2's
  empty-tree/white-screen launch and GR1's "flag request exits 1". The `+1080 px` layout offset did
  **not** reproduce. In every correct run the trace shows one consistent geometry from the first
  sample on (`app_dm=1080x2292@400`, `app_cfg=432x917@400`, `win_frame=[0,108][1080,2400]`,
  `decor=1080x2292 at=0,108`) with no second width and no re-layout.
- **Finding (proven):** the failure class GR2 measured is **two defects, not one**: (a) a rare native
  crash of the ART guest after the window is up (reproduced 1/10, stack not yet captured — the run's
  own directory was removed by the loop's `session destroy`), and (b) the `+1080 px` layout offset,
  which is not reproducible at n=10 on current `main`. The trace cannot yet see a first-measure race
  because it starts at `activity-resumed`; it must move to `application-created` (a 5-line change).
- **Fidelity / evidence:** `art-reference` (real ART guest, host-observed pixels);
  `results/render/R2.md` §2–§3 (the 10-launch table, the exit record quoted verbatim, the good-run
  geometry); loop artifacts in `.cache/r2/loop/run1…run10` (gitignored).
- **A (per-view canvas):** **not implemented, design only.** The hardware per-view canvas
  (`RenderNode.beginRecording(w,h)`) needs HWUI's RenderThread/EGL/Skia-GPU path; the ART guest's
  `HardwareRenderer` natives come from stock `libhwui.so`, and ACOS's `RenderNode`/
  `HardwareRenderer` replacement is an `acos-vm` native the ART process cannot resolve. The honest
  bounded alternative (~250–400 lines of guest Java: nested per-view `Bitmap` canvas + blit, opt-in)
  changes the drawing model for every app and costs 1.5–3× draw time on text-heavy screens, so it is
  an owner decision, not a rendering-task edit. `scenarios/svg-viewbox.yaml` stays red.
- **Regression / false pass:** no product behaviour changed (trace is off by default); no gate
  re-run because nothing in drawing/WMS/verify moved. The negative result is kept deliberately: it
  narrows W6 to a crash + an unreproduced offset instead of "a fragmented layout layer".
- **Conclusion / next:** (1) start the trace at `application-created` and keep the failing run's
  `session-exit.json`/`stdout_tail`/`stderr_tail`/`/proc/<pid>/maps`; (2) capture the SIGSEGV with a
  guest SIGSEGV handler (faulting address + mapped path) and bisect the app's 23 native libs and the
  gralloc shim; (3) owner decision on the opt-in per-view canvas vs the full `libhwui` shim.

## E13.4 — Tree visibility means *visible to the user*: GR2 W4's false-pass class closed —
PASS (1 documented false pass -> 0; one live Gaten false pass re-measured as a real pass)

- **Date / commit:** 2026-11-13; worktree `/home/Qui/.paseo/worktrees/3oxa6xao/v1`, branch
  `verify-v1-visibility` (off `main` `2f9bf4a`, GR2 merged). Engine unchanged (ART guest probe +
  host selector semantics only).
- **Question / goal:** GR2 W4 proved a false pass: the Gaten pager keeps the Analytics page
  warm-mounted at `x=1080..2160` and every node there reported `visible: true`, so
  `wait_for {text: contains "Accuracy"}` passed in 13 ms on content the user cannot see; W3's text
  selector likewise matched an off-screen/phantom label and the tap landed in the neighbouring tab.
  Can tree visibility mean what Android means by it, so no selector, assertion or tap can act on
  content the user cannot see — without dropping off-screen nodes from the tree?
- **Inputs / method:** AOSP `View.isVisibleToUser()` / `isShown()` / `getGlobalVisibleRect()` /
  `AccessibilityNodeInfo.isVisibleToUser` (android-14.0.0_r75 in `.cache/aosp`); the in-guest probe
  (`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`, `appendViewJson`), the host mapping
  (`acos-cli` `ui_node_from_json`), `acos-protocol` selectors, `acos-verify` assertions/executor,
  and the Rust VM walker (`acos-runtime/src/view_tree.rs`). New fixture
  `fixtures/apps/offscreen-pager` (two display-width pages in a `HorizontalScrollView`, page two
  warm-mounted off screen with the text "Hidden") + a positive scenario and two negative ones.
  Re-ran the 7 gates, all `p11-*`, and `gaten-home` on real ART.
- **Result (measured):** each node now carries `visible` (framework `isVisibleToUser`: attached with
  a visible window, it and all ancestors VISIBLE with alpha > 0, global visible rect not clipped
  away) and `visible_bounds` (that rect, translated to display pixels and clipped to the display).
  The offscreen-pager fixture: pre-scroll `Hidden` at `[1122,317][1239,368]`, `visible:false`,
  no visible rect; post-"Next" the same node at `[42,317][159,368]`, `visible:true` with
  `visible_bounds`; `assert_not_visible Hidden` passes with "present in the tree but not visible to
  the user (bounds …)". Negatives: `assert_visible Hidden` -> `ELEMENT_OFFSCREEN` naming the bounds
  (rc=1), `wait_for Hidden` -> `TIMEOUT` + `offscreen_match {bounds [1122,317][1239,368]}` (rc=1).
  Gaten (unchanged APK): `gaten-home` pass 14/14; the Analytics `tap` resolves the only **visible**
  `content_desc "Analytics"` (tab bar `[516,2151][690,2316]`, tap centre `(603,2233)`), and
  `wait_for "Accuracy"` matches `Accuracy` at `[58,1758]` with `visible:true` — the capture frame is
  now the Analytics frame (`4018c190…`), byte-different from Home (`2d0ecc20…`). All 7 gates as
  required (6 × rc=0, `T03-render-wrong` rc=1); 11/12 `p11-*` pass (see below).
- **Finding (new, small):** `p11-delay1000` exposed the reverse case the old tree hid — a
  `TextView` with `layout_width="0dp"`, `visibility="gone"` made VISIBLE after 1 s is a **zero-width**
  node (`bounds [1059,676][1059,792]`), which a device also reports `isVisibleToUser: false`. The
  scenario had been passing on a node with no visible area. Fixture fixed
  (`wrap_content`); `p11-delay1000/200/40` pass.
- **Fidelity / evidence:** `art-reference`, real ART guest, host-observed pixels.
  `results/verify/V1.md`; artifacts `.cache/v1/{offscreen,negative-*,gaten-2,gate-*,p11-*}/`
  (gitignored): trees, step verdicts, frames.
- **Regression / false pass:** false passes **1 documented before -> 0 after** (every artifact
  re-run here; the two offscreen negatives are pinned as expected failures, and a pass there is the
  bug). `cargo test --release -p acos-cli -p acos-art -p acos-services -p acos-verify`: 625 passed /
  1 failed (`serve_daemon` socket-existence flake, passes standalone under the same guard with and
  without the change). One **pre-existing** red test left alone: `acos-protocol --test scenarios`
  requires every scenario to be `1080x2400@420` and GR2 added `gaten-home` at `@400`.
  `p11-progress-timeout`'s "non_pass produced pass" reproduces identically on the unmodified
  baseline (pre-existing settle semantics, P12's area).
- **Conclusion / next:** the W3/W4 class is closed at the root: `visible` is the framework's own
  answer, selectors default to visible nodes, taps use the visible-rect centre, and an
  invisible-only match is `ELEMENT_OFFSCREEN` (never a pass). W5 (a tap step may pass with no
  change) is unchanged and still the other half of that false-pass pair; W6 (intermittent `+1080 px`
  layout / a launch that never paints) remains the reason a green Gaten run is luck-dependent (1 of
  2 re-runs here hit the white-paint flake at step 1, as GR2 measured 2/6).

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
| False passes | **0** (P11 0/105; scenarios 0/12; loop 0) | main `32a0708` pre-H2 | H6 | 2026-11-13 | must stay 0; N=5 loop verified 74/75 (H16) |
| Agent loop N=10 (Z7 method, fresh launch/task) | 45.0 tasks/GiB-min main; **67.7 with exec-path early GC** (seam, off by default → H2); 103.6 zygote+early GC (research, trusted only) | Z7 `zygote-z7-lifetime` | Z7, E2.8 | 2026-11-12 | post-H6 (current main) |
| Agent loop N=10/N=5, H2 default-on | **85.9 tasks/GiB-min** (off 55.7, +54.2 %) / **135.6** (off 98.8, +37.2 %); 1-min interleaved reps, 0 false passes | `perf-h2-early-gc` `54b9c33` | H2, E2.10 | 2026-11-13 | post-H6 (current main) |
| Early-GC memory saving per session | **28.2–30.6 MiB ΣPss** at N=5/N=10 (27.1–32.5 %); 28.9–30.4 MiB at N=1 | H2 default `54b9c33` | H2, E2.10 | 2026-11-13 | post-H6 (current main) |
| Compat first-frame rate (K1 corpus, real ART, N=1) | **13/42 (31.0 %)**; tap 7/42 (16.7 %) | K2 `cdc1a69…c389090` | K2/R1–R3 | 2026-11-12 | post-H6 (current main) |
| M8 tap → settled | strict 955 ms (event, animations on); **182 ms p50 with `animations: off`** (P12 counter step incl. ~78 ms capture) / fast 70.9 ms | main P11, P12 | P1, P11, P12 E2.9 | 2026-11-13 | P12 animations:off (current main) |
| False passes | **0**, class closed at the root: selectors/asserts/taps match user-visible nodes only; off-screen-only match = `ELEMENT_OFFSCREEN` | `verify-v1-visibility` | V1, E13.4 | 2026-11-13 | V1 (current main) |

## Backlog (optimization hypotheses; rank = impact × confidence ÷ (cost × risk))

| ID | Hypothesis | Metric | Expected | Confidence | Cost | Risk | Status |
|---|---|---|---|---|---|---|---|
| H13 | **Memory per session (98 MiB, flat N=1/5/10) is the GiB denominator.** Enable B3's shared uncompressed boot image first (−13.6 MiB/session, measured), then ART post-first-frame trim / zygote CoW (Z6: −25–60 % at N≥5) | M2, tasks/GiB-min | −14 % (B3) … −50 % | high (B3) / medium (zygote) | low (B3 flag) / medium | medium (fixed image address/ASLR; zygote security F1) | **#1** |
| H14 | **Strict tap is 65 % of the loop (926–940 ms):** the framework press/elevation `ValueAnimator` draws ~0.9 s. Adopt P12 `animations:off` for non-assertion steps and/or strict only for assertions (fast for navigation) | M8, loop | 1.44 s → ~0.55 s; +2.3× tasks/GiB-min | high | low | medium (verification semantics: fast never backs an assertion) | **#2, after P12** |
| H15 | **Reset is 24 % of the loop (342 ms):** fresh ART boot + framework + Activity + strict bind. Zygote/shared image, or fast reset for non-asserting steps | M6, loop | −150–200 ms (−10…14 % loop) | medium-high | medium | medium | **#3; overlaps H9's zygote arm** |
| H16 | **N=5 strict assert flake:** 1/75 tasks failed the post-tap count on a strict tree, false_pass 0, did not reproduce (25/25 in reps 2–3). Reproduce with the recorded failing-task steps; check probe/store race under concurrency | loop verified rate | 74/75 → 100 % | medium | low | low | needs-investigation |
| H1 | Strict numbers drop after event settle; re-measure M3–M8 and take cheap wins | M4/M6/M7/M5 | — | — | — | — | **done (H6)**: M7 151→41, M4 440→309, M6 463→342 |
| H2 | Early post-first-frame concurrent GC (Z7) on the exec path | M1/M2 | −23–30 MiB/session | high | low | low | **accepted: product default, E2.10** (H6 numbers are pre-H2; re-baseline) |
| H4 | Idle CPU: 0.45–0.61 % per session and ~16 wakeups s⁻¹ per session (N=10: 4.5 %, 161 s⁻¹). Remove the 250 ms probe fallback and per-session timers | CPU, wakeups | lower wakeups; small at N=10 now | medium | low | low | queued (after H9/H10) |
| H5 | Agent loop with animations:off + event settle | loop | 2.2 → <0.8 s | medium-high | low | low | **folded into H14** |
| H6 | Nightly metric regression bench | all | guard | high | done | low | **done**: `tools/bench/acos-bench.sh` + `BASELINE.json` |
| H7 | Shrink per-session boot-image private memory | M2 | −10–25 MiB | medium | medium | medium | **folded into H13** (B3 is the measured first step) |
| H8 | Faster PNG (SIMD deflate) / region screenshots | M5 | 24 → 10–15 ms | medium | medium | low | backlog (M5 is 2 % of the loop) |
| H9 | Form-validation N=5 under host load: 3/5 `:id/name` taps hit the 30 s bound (2/5 verified) **with early GC off and on**, 2/2 runs each; Z7 saw the same in a/a′/c′ | loop correctness | remove a pre-existing input/queue stall | high (reproduced) | medium | low | backlog (evidence `results/art-runtime/H2.md` §5) |
| H3 | Warm exec pool | M3/M4 | −73 ms launch | — | — | +idle memory | **declined by owner 2026-11-12** (no spare processes) |
| K3a | K2 next walls: AndroidKeyStore shim, GL/BLAST natives, WebView provider classification | compat first-frame | +3–5 apps | high (K1/K2 evidence) | medium | medium | queued |
| K3b | K2 PMS error class (`NameNotFoundException`) and `checkService` null for unknown names | compat | +2 apps (vlc, fdroid) | high | low | low | queued |
| K3c | K2 app provider delivery (androidx.startup/WorkManager `InitializationProvider`) | compat | +element, feeders | medium | high | medium | investigated (K2 §6) |
| R2a | Gaten launch flake: SIGSEGV after first frame (~1/10). Guest SIGSEGV handler dumping fault address + mapped path, then bisect the app's 23 native libs / the gralloc shim | verified-launch stability | 9/10 → 10/10 | high (reproduced) | low | low | **next** (R2 §6) |
| R2b | W6 `+1080 px` fragment offset: move `ACOSLAUNCH_GEOM_TRACE` to `application-created`, keep failing runs, n=20. Then a first-measure race vs a pager-translation source is decidable from `win_frame` vs `app_cfg` | layout correctness | catch + fix the root cause | medium | low | low | **next** (R2 §6) |
| R2c | Per-view canvas for the ART guest without a GPU: opt-in per-view nested `Bitmap` canvas + blit (R2 §4.3) vs a `libhwui`-shim `RenderNode`/`HardwareRenderer` | SVG/wordmark fidelity | `canvasIsViewPx=true` | medium | medium/high | medium (draw cost 1.5–3×) | **owner decision** (R2 §4) |
