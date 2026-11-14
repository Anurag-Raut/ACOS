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

## E5.5 — K3b: an unknown package is a null PMS reply, not a parcel exception — MEASURED (accepted)

- **Date / commit:** 2026-11-13 `9df9e71`, `e4910f1` (branch `compat-k3-walls`, off `main` `1013832`).
- **Agent / bottleneck:** K3; K2's PMS error-class wall (`org.videolan.vlc`,
  `org.fdroid.fdroid`): ACOS answered `getPackageInfo`/`getApplicationInfo` for
  an uninstalled package with a parcel exception, so the app's own
  `catch (NameNotFoundException)` never ran.
- **Baseline (K2):** vlc's first failing call was
  `getPackageInfo("com.google.android.gsf")` raising ACOS's structured
  "unknown service name"; fdroid the same class.
- **Hypothesis + solution:** the image's PMS returns a **null**
  `PackageInfo`/`ApplicationInfo` and `ApplicationPackageManager` raises
  `NameNotFoundException` client-side (AOSP
  `ApplicationPackageManager.getPackageInfoAsUser`/`getApplicationInfoAsUser`).
  Answer the same: typed null, nothing else changes.
- **Method:** targeted batch (vlc, fdroid, element, termux) then the full
  corpus (`runs-default`), guarded, one `acos serve` per app.
- **Result:** both apps leave the wall and complete
  `Application.onCreate`; vlc's next wall is the `activity_onboarding`
  `ConstraintLayout` inflation, fdroid's is
  `IPackageManager.getSystemAvailableFeatures` (code 99, no `FeatureInfo`
  codec). No app regressed: the full corpus reproduces K2's funnel exactly
  (42 process start, 41 `onCreate` entered, **16** completed, 12–13 first frame,
  12 tree/screenshot, 7 tap; the 7 full-ladder apps are the same set as K2 and
  the only first-frame difference between two runs of the same binary is
  `com.libreweatherrn`'s React Native/WebView race).
- **Fidelity / evidence:** `results/compat/K3.md` §1,
  `crates/acos-services/tests/k3_walls.rs`
  (`unknown_package_is_a_null_package_info_not_an_exception`,
  `the_known_package_still_answers`).
- **Result:** accepted. **Trade-offs:** none measured; the
  `checkService`-for-unknown-name path was already `null` and is unchanged.

## E5.6 — K3c: deliver the app's own providers through the framework (measured, OFF by default) — MEASURED (seam; blocked by two named gaps)

- **Date / commit:** 2026-11-13 `40ffc61`, `6803a68`.
- **Agent / bottleneck:** K3; K2 §6's "missing app providers": the bind request
  carried an empty `ProviderInfoList`, so `ActivityThread.installContentProviders`
  never ran and no `<provider>` of the APK existed.
- **Baseline (K2):** `io.element.android.x` failed at
  `ProcessLifecycleInitializer cannot be initialized lazily`; FileProvider,
  WorkManager and Sentry providers did not exist in any app.
- **Hypothesis + solution:** use the framework's own path. The host parses the
  APK's providers from the same typed manifest PMS answers `getProviderInfo`
  from (dropping providers whose `android:process` is not this process, the
  `AMS.generateApplicationProvidersLocked` filter), the guest builds real
  `ProviderInfo` objects, and `handleBindApplication` instantiates, `onCreate`s
  and publishes them between `Application.attachBaseContext` and
  `Application.onCreate`. Two platform behaviours this exposed are now
  implemented: `publishContentProviders`' `List<ContentProviderHolder>` (the
  decoded table had the `java.util.List` erasure) and `BaseBundle`'s empty
  bundle (a single `0` int, no `BNDL` magic).
- **Method:** targeted batches (vlc, fdroid, element, termux) and a full corpus
  with delivery on (`runs-final`), then the shipping default with delivery off
  (`runs-default`).
- **Result (delivery ON):** the framework path works end to end.
  `io.element.android.x` now **runs `InitializationProvider`** — the next
  failure is the provider's own code raising `DSN is required`, which is
  manifest `<meta-data>` (`io.sentry.dsn`) that PMS does not send.
  `de.westnordost.streetcomplete` gains its first frame. vlc, fdroid and termux
  keep the K3b gain and reach activity inflation. But three apps regress
  because two gaps sit *after* delivery: editor and libreweatherrn die in
  `FileProvider.attachInfo` (`Missing android.support.FILE_PROVIDER_PATHS
  meta-data`), and joplin dies publishing its holder
  (`BinderProxy.transactNative` → "Unknown error"). Funnel with delivery on: 11/42 first
  frame (vs 13 with it off) — the three regressions above; with the shipping
  default (off) the corpus is K2's 13/42.
- **Fidelity / evidence:** `results/compat/K3.md` §2,
  `crates/acos-services/tests/k3_walls.rs` (bundle + holder decode),
  `.cache/k3/runs-final` (on) and `.cache/k3/runs-default` (off).
- **Result:** the delivery itself **accepted as correct and verified**, shipped
  **off** (`ACOS_ART_APP_PROVIDERS=on` enables it). **Why off:** it is a net
  regression until the two blockers land, and K3 must not regress K2.
  **Follow-ups (sized in K3.md §3):** (1) manifest `<meta-data>` → `Bundle` in
  `ApplicationInfo`/`ProviderInfo` and in PMS `resolveContentProvider`
  (recovers editor, libreweatherrn, element); (2) app-published
  `IContentProvider` binder marshal in `publishContentProviders` (recovers
  joplin).

## E5.7 — K3a: the remaining walls, classified with a capability and a cost — MEASURED (classified, not shimmed)

- **Date / commit:** 2026-11-13 (same branch; no code change beyond the naming
  in `tools/compat/k1-analyze.py`).
- **Agent / bottleneck:** K3; K1/K2's R5/R6 walls.
- **Result:** three families stay blocked, each named by the app's own
  exception and sized in `results/compat/K3.md` §3: **AndroidKeyStore**
  (dot.gallery, tailscale — needs keystore2 *and* a keymaster backend, so a
  shim would fabricate key material and break the core promise), **GL/BLAST**
  (shatteredpixeldungeon, mindustry, supertuxkart — needs a real EGL/GLES driver
  and BLAST producer), **WebView provider** (calendar, libreweatherrn — provider
  discovery plus a real Chromium provider). New next walls the corpus named:
  `IWindowSession.getWindowInsets` (113, fluffychat), layout/theme inflation
  (vlc, termux, gotify — R2's track).
- **Fidelity / evidence:** `results/compat/K3.md` §3 and the per-app crash
  lines in `.cache/k3/runs-default`.
- **Result:** classified honestly; no fake success anywhere. **Cost:** each is
  weeks-scale; `<meta-data>` (E5.6 follow-up) is the cheap one.

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

## E2.11 — L1: animations-off delayed updates were a `wait_for` replay, and H9 is the pre-P12 pixel guard — MEASURED (fixed)

- **Date / commit:** 2026-11-13; branch `fix-l1-looper-input` (off `main` `1013832`).
- **Question / goal:** with `animations: off`, `p11-background` (150 ms background
  thread post) and `p11-delay1000` (1000 ms `postDelayed`) never showed their
  later update through `wait_for`, even though animations on masked it. Is the
  guest main looper not dispatching, is the invalidate not drawing, or is the
  probe/frame pipeline not waking? Separately, H9: at N=5, 3/5
  `form-validation` `:id/name` taps hit the 30 s bound. Same cause?
- **Method:** real ART, release binary, one host-wide ART lock. Guest-side
  timestamps from `ACOS_ART_INPUT_TRACE` (`Looper.setMessageLogging` lines) plus
  the guest's `ACOSPROBE` samples (`time`, `draw_seq`, `draw_ms`, `settle`).
  `p11-background`/`p11-delay1000` run with `--animations off`; the P11/P12
  gate suite in both modes; H9 protocol `z7/loop.py --arm a --n 5 --cycles 1
  --app form-validation`, twice with the default rule and once with
  `ACOS_STRICT_SETTLE_PIXEL_GUARD=on`.
- **Result (a):** the guest is not idle. With animations off the background
  Runnable dispatches at +171 ms after the tap publish (465.2 ms in the run),
  draws (`draw_seq=6`, `draw_ms=…553271`) and emits probe
  `seq=12 … bg_text="bg-done", settled=true`; probes continue every 250 ms.
  The verifier made 9 `wait_for` polls over 3011 ms, reused `tree/3.json`,
  `last_frame_seq=3`, and no `tree` command reached `acos-art` after the tap.
  Cause: `TrustGuard` (wrapping every `acos verify` backend) forwarded
  `observe` but not `Backend::refresh`; the trait default replays the last
  completed frame, so `wait_for` never re-traversed. Fix: forward `refresh`
  (and fail closed for untrusted runs). After the fix the same update stores
  C1 generation 6 and `wait_for` returns in 179 ms with `bg-done`;
  `p11-delay1000` returns in 967 ms.
- **Result (b):** H9's failing guests show the tap dispatched (`View$PerformClick`
  on `ViewRootImpl$ViewRootHandler`, `draw_seq` advances) but no pixel change
  (focus into an already-focused empty `EditText`; in the H2 raw run some
  sessions had no C1 buffer at all). The evidence was taken **before P12's
  final review change**, when the pixel-unchanged guard was on by default, so
  the settle waited the full `ACOSLAUNCH_FRAME_WAIT_SECS=30`. On this branch:
  default (guard off) N=5 form **5/5 verified twice** (tap 110–176 ms);
  restoring `ACOS_STRICT_SETTLE_PIXEL_GUARD=on` on the same binary reproduces
  the 30 s bound (2/5 taps 30 028/30 048 ms). No input or looper bug.
- **Gates:** all 7 positive gates pass in both animation modes;
  `T03-render-wrong` fails as the expected control; all 12 `p11-*` pass in both
  modes including `p11-background` 5/5 and `p11-delay1000` 5/5 with animations
  off, 0 false passes; `p11-progress-timeout` stays a non-pass and
  `p12-progress-anim-off` passes 6/6; `cargo test` green for
  `acos-cli/-art/-services/-verify`. Evidence
  `results/art-runtime/L1.md` §L1 follow-up and
  `results/art-runtime/evidence/L1/`.
- **Conclusion:** `animations: off` is no longer bounded by "already-queued
  messages in the settle horizon"; a later update is visible to `wait_for`.
  Do not weaken settle: a genuine settle timeout still returns
  `settled:false` and `verify` still fails it as `TIMEOUT`.

## E3.2 — B2: re-baseline on current main (H2 + P12) and the GiB-denominator correction — MEASURED (accepted)

- **Experiment ID / name:** B2 — Stage-1 close-out re-baseline on merged main.
- **Responsible agent:** B2 (worktree `bench-b2-rebaseline`, off `main` `4958703`).
- **Bottleneck investigated:** the whole Stage-1 metric set was last measured on a build *before* H2's
  early-GC default and P12's settle/verify changes; the stored baseline could not gate regressions on
  the shipped tree, and the leaderboard mixed builds.
- **Baseline (p50/p95, pre-H2 `32a0708`, `results/bench/baseline-pre-h2.json`):** M3 406 ms, M4
  309/175, M5 24.3, M6 342/210, M7 41.1/0.44, M8 956/44.3 (counter) and 948/45.0 (form), tap ack 5,
  teardown 12, M1 98.9 MiB, M2 97.3/98.0/97.7 MiB cgroup (Pss 213/108/94), idle CPU 0.61/2.44/4.47 %,
  wakeups 34/90/161 s⁻¹, loop 1438.0 ms/task with 280.5 (N=1)/664.0 (N=5) tasks/GiB-min, 0 false passes.
- **Hypothesis + proposed solution:** nothing in P12 should move a p50 (it changes semantics and
  evidence), and H2's −29 MiB/session must show up as −32 MiB in cgroup anon+shmem at every N, with the
  loop's latency unchanged. Re-run the H6 bench unchanged apart from the fixes the current tree needs
  (`settled:true` gating, `pixels_changed`/`animations` evidence fields, `--task`/`--scratch-root`,
  `loop10`, an anon+shmem tasks/GiB-min metric), and accept the result as the new baseline.
- **Implementation + commits:** `tools/bench/acos_bench.py`, `tools/bench/acos-bench.sh`,
  `results/bench/B2.md`, `results/bench/BASELINE.json`, `results/bench/baseline-20261112T212855Z.{json,md}`,
  raw samples `results/bench/run-20261112T212855Z/`, kept-as-was `results/bench/baseline-pre-h2.json`.
  No `crates/**` change. Binary `acos` sha256 `c824013b…` (release, `main` `4958703`).
- **Benchmark method + environment + host load:** 3 interleaved repetitions, 7 phases, one
  `run-exclusive.sh --task B2` + `guarded.sh` (8G; memory 16G) acquisition per phase group, every
  acquisition < 10 min (max 219.6 s). Scratch `.cache/b2/`. Host: Ryzen 9 9950X, 32 threads, 61,908 MiB;
  9/21 phases `noisy` (load1 4.2–11.1 from four concurrent agents; rep 2's `latency` phase is the only
  materially distorted one), MemAvailable 45–47 GiB, queue waits 0–133 s. Pooled n = 60 per latency
  metric, 63/75 loop tasks, 3 samples per memory/CPU row.
- **Before/after p50/p95:** M1 **98.9 → 66.5 MiB**, M2 **97.3/98.0/97.7 → 65.0/65.2/65.6 MiB** cgroup
  anon+shmem (ΣPss 107.7/94.2 → 76.8/62.6 at N=5/N=10); everything else within its derived threshold:
  M3 411 (+1.3 %), M4 336/203 (clean reps 324/175 — tie), M5 24.9, M6 333/210, M7 41.1/0.48,
  M8 956/56.1 and 944.7/45.5 (clean-rep fast 44.3 — tie), tap ack 5, teardown 12, loop
  **1423.9 ms/task / 0.653 tasks·s⁻¹** (N=1) and 1428.1 / 2.679 (N=5); tasks/GiB-min **283.5/667.6**
  in the historical page-cache denominator (+1.1 %/+0.5 %) and **905.3/865.8** in the anon+shmem one
  (**+45.7 %/+49.8 %**); idle CPU 0.65/2.77/5.28 % (wakeups 34.1/90.8/161.7 unchanged); 0 false passes
  (P11 0/105, scenarios 0/12, loop 0, verified 63/63 N=1 and 75/75 N=5).
- **Result:** **accepted**; `results/bench/BASELINE.json` is now the current-main document and the
  pre-H2 one is kept verbatim. `--compare` pre-H2 → current exits 0 ("no regression beyond the
  per-metric noise thresholds").
- **Why it worked / what it found:** the only real change is H2's, and it lands exactly at the size H2
  claimed, in the *cgroup* anon+shmem number (not just Pss) — so it is returned anonymous memory, not
  shared-file accounting. P12 moved semantics only (event-only strict with no quiet escape hatch,
  pixel guard off with `pixels_changed` stated, verify failing a timed-out settle: the negative control
  is an honest `timeout`), which is visible in the reply fields the bench now records, not in p50s.
  The run also produced a **metric correction**: the historical tasks/GiB-min denominator (cgroup
  `memory.current`) is dominated by page cache charged to whichever phase read the image first
  (≈95 MiB average on a clean acquisition, ≈600 MiB when a latency phase precedes the loop in the same
  cgroup), which is why it showed H2 as +1 % while the label-matched anon+shmem denominator shows
  +46/+50 %. Both variants are now baseline rows.
- **Trade-offs / regressions:** none beyond noise. The two rows that look worse (M4 fast +16 %,
  M8 counter fast +27 %) are repetition 2 running at load1 6–11; their clean repetitions tie pre-H2
  exactly and the derived thresholds cap at 40 %, so they are reported as unresolved-under-load rather
  than as regressions. Idle CPU at N=10 is +18 % (4.5 → 5.3 %) inside a 38 % band with a 4.9–6.3 %
  rep spread — worth one quiet-host re-run before it means anything.
- **Follow-ups:** H14 (E3.3, same session); H17 (N=10 loop reliability); H15 (reset/relaunch) promoted
  to #1; H13 demoted to #2; re-run the P11 corpus with `animations:off`.
- **Fidelity / evidence:** `art-reference` (real ART, release), MEASURED; `results/bench/B2.md`,
  `results/bench/BASELINE.json`, `results/bench/baseline-20261112T212855Z.{json,md}`,
  `results/bench/run-20261112T212855Z/`. Regression: none. False pass: none.

## E3.3 — H14: `animations: off` for functional tests (agent-facing mode) — MEASURED (accepted as a mode, not as a default)

- **Experiment ID / name:** H14 — per-session animation scales off as the functional-check mode.
- **Responsible agent:** B2 (same worktree/build as E3.2).
- **Bottleneck investigated:** the strict tap→settled step was 65 % of the agent loop (927–966 ms) because
  the framework Button press/elevation `ValueAnimator` draws for ~0.9 s after every tap and the
  event-only strict settle waits for it. P12 shipped the switch (`acos_launch {animations:"off"}`),
  never measured end-to-end in the loop.
- **Baseline (p50):** animations on — M8 counter 956 ms, M8 form 945 ms, M4 strict 336 ms, M6 strict
  333 ms, loop 1438 ms/task at N=1 / 1432 at N=5 / 1437 at N=10, 0.65/2.55/3.31 tasks·s⁻¹,
  905/866 tasks/GiB-min (anon+shmem), 0 false passes.
- **Hypothesis + proposed solution:** with the animator scaled to 0, tap→settled drops to the
  state-change + one-frame time (~100–150 ms) and the loop falls to ~0.55 s (2.4–2.7×), with no
  verification-semantics change because every assertion still runs on a strict, settled tree.
  Measure both arms on the same binary with one dial (`animations`), interleaved, 3 reps.
- **Implementation + commits:** tool only — `--animations on|off` threaded to every launch
  (latency/memory phases, a wrapper on the P5 harness's launch call, P7's cold phase, `acos verify`
  for the scenario sweep) plus `loop10`; no product code, no default change. `tools/bench/**`,
  `results/bench/B2.md` §5.
- **Benchmark method + environment + host load:** 3 interleaved repetitions per arm
  (`on,off`/`off,on`/`on,off`), one 16G acquisition per arm per rep containing
  `latency,loop1,loop5,loop10` (n=20 per latency metric per rep, 21/25/50 loop tasks), task B2, real
  ART, release `c824013b`. Load1 0.85–10.4 across arms (rep 1's off arm ran at 10.4 and its fast-reset
  cohort failed 20/20 — reported, not deleted); queue 7–230 s; every acquisition < 10 min.
- **Before/after p50/p95:** M8 tap counter **960.5 → 130.9 ms (7.34×, p95 968 → 135.1)**, M8 form
  **945.6 → 113.3 ms (8.35×)**, M4 strict **310.6 → 225.9 ms (−27 %)**, M6 strict **334.0 → 267.5 ms
  (−20 %)**, M6 fast 211.0 → 181.5 ms. Agent loop **1437.5 → 507.0 ms/task (2.84×)** at N=1,
  **1432.0 → 536.5 (2.67×)** at N=5, **1436.6 → 529.8 (2.71×)** at N=10; throughput **0.65 → 1.79**,
  **2.55 → 6.28**, **3.31 → 10.22** tasks·s⁻¹. Verified tasks/GiB-min in the anon+shmem denominator
  **901 → 5239 (5.8×)**, **793 → 3942 (5.0×)**, **438 → 3798 (8.7×)** — medians of three interleaved
  reps; the off `loop5` r3 rep is a 64 s straggler cohort (one 1.47 s task plus a reset `GUEST_EXITED`,
  B2.md §5.3) and is not used as an effect. The page-cache-inclusive
  denominator cannot answer this (page cache is charged to the phase that read the image first) and is
  reported as invalid for the arms. 0 false passes in every arm.
- **Result:** **accepted as an agent-facing mode** ("use `animations:off` for functional checks"), with
  the product default unchanged (`on`). Also **accepted as a metric finding**: the loop becomes
  reset-bound (48 % reset, 22 % tap, 24 % three tree reads) instead of tap-bound, which is why H15 is
  now the #1 backlog item.
- **Why it worked:** the animator scale is the framework's own path (`ValueAnimator.setDurationScale`
  plus `IWindowManager.getCurrentAnimatorScale`), so press/elevation/ripple all stop; the state change
  still happens (`first_change_ms` 55–56 ms, unchanged) and strict settle still requires a real draw
  after the action (`pixels_changed:true` on every strict row).
- **Trade-offs / regressions:** nothing in the shipped default. The mode must not back animation or
  transition tests (documented in `docs/AGENT_API.md` §8.6.5). Under the page-cache GiB denominator
  there is no throughput gain to point at; under the anon+shmem one there is 5–8.7×. N≥8 verified rate
  is not reliable in either arm (`GUEST_EXITED`, 0 false passes; once at N=5 too) — H17.
- **Follow-ups:** H17 (N=10 concurrent reset/relaunch); H15 (reset is now the wall); P11 corpus with
  `animations:off`; consider exposing the mode as the default for `acos verify` functional scenarios
  (owner decision, not taken here).
- **Fidelity / evidence:** `art-reference` (real ART, release), MEASURED, 0 false passes;
  `results/bench/B2.md` §5, `.cache/b2/h14/arm-{on,off}-r{1,2,3}.{json,md}`,
  `.cache/b2/h14/run-arm-*/rep-1/loop*.json`, `.cache/b2/logs/h14-ab.sh`.

## E13.6 — Native-crash evidence on the product path: a signal death now names its fault address, `si_code`, thread and library frame — PASS (diagnosability); the Gaten 1/10 SIGSEGV did not recur in 40 launches

- **Date / commit:** 2026-11-13; R3 worktree `fix-r3-gaten-segv` (off `main` `b551dfa`).
- **Question / goal:** `results/render/R2.md` §2 lost 1 in 10 Gaten launches to `SIGSEGV`
  (`GUEST_KILLED_BY_SIGNAL`, no OOM) with no stack; `GR2.md` saw the same shape as a white screen.
  Make a native guest crash diagnosable on the product path (fault address, `si_code`, faulting
  thread, native backtrace with mapped library + offset, run dir preserved), reproduce at n≥20 with
  and without H2's early GC, and fix at the ACOS layer — not in Gaten.
- **Inputs / method:** same read-only APK (`f344272`, sha256 `e04c7ffc…`), phone-matched geometry.
  Read `art/runtime/runtime_android.cc` + `runtime_common.cc` + `fault_handler.cc`, `bionic/linker`
  and the image's `ld-android.so`/`libc.so`/`libsigchain.so` in `.cache/aosp` + `.cache/art/sysroot`.
  Two arms of 20 launches (`launch → tree → screenshot → 5 s → tree`), evidence kept; plus a
  deterministic fault seam (`ACOSLAUNCH_CRASH_TEST=segv`) and a new real-guest test.
- **Result:** the failure was **undiagnosable by construction**, three independent reasons, all in
  ACOS: (1) this image's `ld-android.so` links bionic's debuggerd *stub*, so no in-process crash
  handler exists at all; (2) ART's own unexpected-signal dumper is installed only when
  `getenv("ANDROID_ROOT") != "/system"`, and ACOS set exactly `/system`; (3) `RunDir::drop` deleted
  the run tree and `session-exit.json` carried only the signal number. Fix: export
  `ANDROID_ROOT=/system/` (path-equivalent, turns the runtime's dumper on; `ACOSART_CRASH_DUMP=0`
  restores `/system`), add a small async-signal-safe handler appended to ART's *own* sigchain
  (`AddSpecialSignalHandlerFn`, after the runtime's handlers, never claiming the signal) that prints
  one `ACOSCRASH {json}` line, and keep `native_crash`/`native-crash.txt` + the run tree on a signal
  death. Measured crash rate: **before 1/10** (R2, same APK; GR2 2/6 white screens), **after 0/40**
  (20 default, 20 `ACOS_ART_EARLY_GC=0`), 20/20 trees with `Set up Gaten` in both arms. Early GC is
  not implicated (0 crashes in either arm). Run-tree retention is **bounded by construction**
  (review correction, §1.4 of R3): a tree is kept only for a crash signal the guest raised itself
  (`SIGSEGV/SIGBUS/SIGILL/SIGFPE/SIGABRT/SIGTRAP/SIGSYS`) *and* whose `native_crash` record was
  parsed — never for `SIGKILL`/`SIGTERM`/`SIGHUP`/`SIGINT` (GL1's bounded destroy, timeouts, resets);
  at most `ACOS_ART_KEEP_CRASH_MAX` kept trees per data root (default 8, `0` = keep none), oldest
  pruned first and logged, `kept-for-crash.json` marking only those trees so explicit
  `ACOS_ART_KEEP_RUN`/`ACOS_ART_KEEP_SESSION` trees are never touched; `kept_run_dir` is recorded in
  `session-exit.json`. Verified end to end with cap=1 (two crashes -> exactly one tree, the older
  pruned and logged).
- **Finding (proven):** a deterministic post-`activity-resumed` fault now yields
  `SIGSEGV (11) fault_addr=0x0 in thread 15 "acos-launch-wat": /data/lib/libacos_binder_driver.so
  (Java_dev_acos_art_Nat_crashTest+39)` plus ART's own `*** *** ***` / `Fatal signal` / registers /
  `Backtrace` block, in `session-exit.json.native_crash` and on disk. `GUEST_KILLED_BY_SIGNAL` and
  the failure taxonomy are unchanged (the handler returns "not handled").
- **Not proven:** the Gaten crash's root cause. It did not recur (0/40; a true 1/10 rate would give
  zero at n=40 with p≈1.5 %), so "fixed" and "rarer" cannot be separated at this n, and nothing here
  changed the guest's drawing/threading/native paths. The change makes the next occurrence name its
  library and thread; it does not prevent it.
- **Fidelity / evidence:** `art-reference` (real ART guest, host-observed). `results/render/R3.md`
  §2–§4; loop artefacts `.cache/r3/loop/{on2,egcoff}/run1…20`; crash fixture
  `.cache/r3/crashtest/root3/{native-crash.txt,native-crash.json,session-exit.json}` (gitignored).
- **Regression:** `cargo test -p acos-cli -p acos-art -p acos-services -p acos-verify` 629 passed /
  0 failed at the first commit; after the retention correction `-p acos-art -p acos-cli` is 285
  passed / 0 failed (30 binaries) and the retention units are 9/9. The 8 mandatory gates re-run
  **under `ANDROID_ROOT=/system/`** (7 pass, T03-render-wrong fails as expected), which is the check
  that the trailing-slash value is inert for the framework; `v2-art-counter` and
  `w22-counter-reset` were re-run again after the retention change. The S6 signal test keeps S4's
  original clean-up contract (SIGSYS with no crash record keeps nothing); the one deliberate
  behaviour change is that a *guest crash with evidence* keeps one bounded tree.
- **Conclusion / next:** keep the loop at n≥100 to separate rare from fixed; every occurrence now
  arrives with a library + offset to bisect. `R2b` (`+1080 px`) and `R2c` (per-view canvas) untouched.

## E2.13 — H17: N=10 concurrent reset/relaunch reliability — two shared-input races (fixed) — MEASURED

- **Experiment ID / name:** H17 — concurrent reset/relaunch at N=10 in the P5 agent loop.
- **Responsible agent:** H17 (worktree `fix-h17-n10-reset`, off main `84fc8bc`).
- **Bottleneck investigated:** with 10 sessions under one `acos serve`, the P5 verified loop lost tasks
  with `steps.reset.error = GUEST_EXITED` and follow-on `tap`/`observe` `INTERNAL` (B2/E3.2–E3.3:
  14–50/50 verified, 0 false passes). The idle 10-session memory phase was clean, so the fault was in
  the concurrent launch/reset path, not capacity. A second symptom: under load1 10.4, 20/20 fast resets
  returned an error in 0.009 ms (B2 §6 item 2).
- **Baseline (p50):** loop10 verified rate 131/150 (87.3 %) at load1 7.3–8.5 and 148/150 (98.7 %) at
  load1 2.6, animations on; 149/150 (99.3 %) animations off at load1 1.1; 0 false passes. Loop p50
  1431–1487 ms/task (on), 520–531 ms (off).
- **Hypothesis + proposed solution:** the failures are two instances of one class — a file shared by
  every session of the cohort is rewritten in place (truncate + write) while another process reads or
  maps it. (1) `<data>/seccomp/landlock.plan` is rewritten by *every* launch and read by the guard at
  guest start; a torn read fails closed with exit 126/1 (`ERR landlock=plan-missing`). (2)
  `<data>/app/<apk>` is `fs::copy`ed by every launch and `mmap`ed by ART/`AssetManager`; a truncate
  under a live mapping is `SIGBUS` (exit 135 = 128+7 as reported by `systemd-run --scope`). Fix at the
  publication layer: atomic temp+rename (no-op when unchanged) for the shared inputs, and a per-run
  APK staged into the private run root and bound over `/data/app/<name>` (the shared path stays only
  as a write-once mount point). The 0.009 ms reset error is the session table refusing
  `SESSION_NOT_FOUND` after the harness ignored a failed launch — a correct refusal, not a distinct bug.
- **Implementation + commits:** `acos-art`: `publish_atomic` (lib.rs) used by `landlock::write_plan`,
  `Props::write`, `write_apex_info_list`; `app::stage_apk` + `ensure_apk_mountpoint` used by `launch`,
  `render-app` and `session`; `session-exit.json` now carries `guard`/`notes` so a guard refusal names
  its cause. `acos-cli`: trace-only dm6 events for spawn/stop/liveness (inert unless `DM6_RESET_TRACE`).
  Three deterministic unit tests (held-reader, concurrent-reader, re-staged APK) fail on the pre-fix
  code and pass after. Branch `fix-h17-n10-reset`.
- **Benchmark method + environment + host load:** real ART, release (after build `5b06ab9f`), P5 loop (6 loops/agent, 1 warm-up,
  50 measured tasks/rep, 3 reps/arm) in one `acos serve` per rep, one 16G `run-exclusive.sh` +
  `guarded.sh` acquisition per arm; `ACOS_ART_KEEP_RUN=1` on the diagnostic reps only. Load1 per rep
  recorded below; queue 3–205 s; every acquisition < 10 min.
- **Before/after (verified tasks / measured, 0 false passes in every row):**

  | arm | load1 | before (main) | after (fix) |
  |---|---|---|---|
  | on, quiet | 2.60 / 2.60 / 2.60 vs 1.05 / 1.68 / 1.66 | 49, 49, 50 = **148/150** | 50, 50, 50 = **150/150** |
  | on, final build `5b06ab9f` | — vs 3.79 / 4.15 / 3.96 | — | 50, 50, 50 = **150/150** |
  | on, loaded | 8.45 / 7.29 / 8.07 | 48, 50, 33 = **131/150** | not run at that load (no deliberate load generator) |
  | off, quiet | 1.10 / 1.10 / 1.09 vs 1.40 / 2.07 / 1.98 | 50, 49, 50 = **149/150** | 50, 50, 50 = **150/150** |
  | off, final build `5b06ab9f` | — vs 4.21 / 4.20 / 4.02 | — | 50, 50, 50 = **150/150** |

  Pooled **428/450 (95.1 %) → 600/600 (100 %)** over 12 after-reps; all before-failures were exit 1/126
  from the torn plan or exit 135 (SIGBUS) from the truncated mapped APK. Latency/throughput unchanged:
  on 1426–1441 ms/task and 4.6–5.2 tasks/s (before 1431–1487, 4.8–5.2); off 533–535 ms and 11.3–11.5
  tasks/s in two clean reps (before 520–531, 11.1–11.4); reset step p50 344–351 ms before/after.
- **Result:** **fixed.** N=5 stays 75/75 (3 × 25, load1 2.9–3.5). N=20: **40/40 verified**, 0 false passes,
  job cgroup `memory.current` peak 2186 MiB, guest anon+shmem peak 1329 MiB, host
  `MemAvailable` 47.25 GiB of 60.5 (≈22 % used, well under the 90 % rule), no `oom_kill`. Gates:
  v2-art-counter, w22-counter-reset, w54-dialog-pixels, w42-back-frame, w15-dialog-menu,
  form-validation, v1-offscreen-pager, T03-render-good all pass; T03-render-wrong fails
  `ASSERTION_FAILED` as required. P11 corpus: 0 false passes, 12/12 scenario expectations,
  `p11-progress-timeout` 5/5 honest timeouts (non-pass). `cargo test --release --offline -p acos-cli
  -p acos-art -p acos-verify`: 343 tests / 32 binaries, rc 0, no failures (serve_daemon flake not
  seen). The M6 fast-reset cohort after the fix: **20/20 ok, p50 195 ms** (strict 20/20, 329 ms) — the
  0.009 ms refusals do not reproduce; they were `SESSION_NOT_FOUND` after a launch the harness ignored.
- **Why it worked:** the shared files are read-only *inputs* to every guest, so replacing them atomically
  (temp + rename, skipped when unchanged) means a reader/mapper keeps a complete inode instead of
  racing an `O_TRUNC`; giving each session its own APK removes the mapped-file truncation and the
  same-name cross-session APK hazard at once, without changing the guest-visible path.
- **Trade-offs / regressions:** none measured (latency identical within noise; the per-run APK adds one
  small file copy per launch, replacing the shared copy it replaces). The high-load before rep is
  load-confounded (no deliberate load generator was run, to avoid perturbing other agents); the
  quiet-load comparison is matched and the mechanism is named by the exit codes and guard logs, not
  only by the rate.
- **Follow-ups:** `docs/ERROR_CODES.md` promises an exit-126-with-guard-`ERR` → `SECCOMP_UNAVAILABLE`
  re-classification that the launch path only applies when the outcome is otherwise Ok; the record now
  names the cause, but the re-classification is still worth doing. `acos_bench.py`'s M6 loop ignored
  `launch()`'s result, which is how a failed launch surfaced as 20 fast reset refusals — **closed by
  E3.6/B4** (fail-closed harnesses; the M6 negative proof now exits 1 with coded failures and zero
  latency samples from the failed launch).
- **Fidelity / evidence:** `art-reference` (real ART, release), MEASURED, 0 false passes;
  `results/bench/H17.md`, `.cache/h17/loop-{diag-on,before-on-quiet,before-off,after-on,after-off,after-off2,n5-after,n20-after}/`,
  `.cache/h17/{gates,corpus,latency,tests-full.log}`, kept guest run evidence
  (`ERR landlock=plan-missing`, exit 135) under `.cache/h17/loop-*/guest-evidence/`.

## E2.12 — H13: a per-serve ASLR shared boot image (`ACOS_ART_BOOT_IMAGE`) — MEASURED (memory pass; not default: cold-start cost)

- **Date / commit:** 2026-11-13; branch `mem-h13-shared-image` off `main` `f9f6ffa`;
  worktree `/home/Qui/.paseo/worktrees/3oxa6xao/h13`. Host: Ryzen 9 9950X, 62 GiB,
  load recorded per arm (`results/vm/H13.md`).
- **Question / hypothesis.** B3 proved a shared, file-backed uncompressed boot
  image saves ~12–13 MiB/session, but only with `-Xnorelocate` at the fixed base
  `0x70000000` (image ASLR off, trusted-only). Android instead randomises the
  boot image **once per boot** in the zygote and shares that one mapping with
  every app. Hypothesis: do the same once per **serve cohort** — rebuild the
  uncompressed image at a random page-aligned base on the first launch of a
  serve, pass `-Xnorelocate`, and let every session of that serve map it
  read-only and shared. Then the memory win survives without a fixed image base,
  and each new serve start gets a new base. The open question was the one-time
  relocation cost and whether rebuilding at a random base changes the runtime
  layout enough to eat the win.
- **Change.** New `crates/acos-art/src/boot_image.rs` + a launch/plumbing change:
  `ACOS_ART_BOOT_IMAGE=lz4|shared-fixed|shared-aslr` (unset = `lz4`, the product
  default). Shared modes build with the sysroot's stock `dex2oat64
  --image-format=uncompressed --base=<base>` in a staging bwrap (B2's recipe),
  store the tree at `<ACOS_ART_DATA>/boot-image/` (root `0700`, files `0444`),
  and `--ro-bind` it at `/data/img`. `shared-aslr` picks ART's own
  `0x70000000 ± 128 MiB` window on a 4 KiB grid (**16 bits**), keyed by the
  invoking process (parent pid + start time): one serve = one base, a new serve =
  a new base. `shared-fixed` keeps B2's `0x70000000`; `ACOS_SHARED_BOOT_IMAGE`
  remains its explicit-directory form. A bad value fails the launch; nothing
  falls back. `acos-art build-image` now shares the same builder. Documented in
  `docs/AGENT_API.md` §16.1 and `docs/SECURITY.md`.
- **Method.** Every ART arm went through `tools/art-runtime/run-exclusive.sh
  --task H13 -- tools/guarded.sh ...` (8G; 16G for N=10; scratch under
  `.cache/h13/`). M2: `tools/art-runtime/dm2/measure.py` with `--settle 60`,
  N=1/5/10, one held serve per arm, `ΣPss`, private dirty and cgroup
  anon+shmem from `/proc`/`memory.stat`; host load recorded before/after. M3/M4:
  `tools/bench/acos-bench.sh --phases cold|latency` (5 and 3 reps). Agent loop:
  `--phases loop1,loop5` (H6/P5), H2 early GC left on. Prep cost: two identical
  counter verifies per mode, first (build) minus second (reuse). P11:
  `--phases p11,scenarios`; correctness gates through `acos verify --engine art`.
  Two concurrent sessions were checked in raw `maps`/`smaps`; bases across serve
  starts were read from the same dumps.
- **A/B memory (MiB per session).** LZ4 is the default; `sf` is the B2/`shared-fixed`
  reference; `sfg` is `shared-fixed` rebuilt by this code; `sa` is `shared-aslr`
  (two independent serve cohorts at N=5/10; the table gives both, with the median
  in the Δ column). All rows are one held serve, 60 s settle, 0 survivors.

| arm | N | cgroup anon+shmem/session | Δ vs lz4 | ΣPss/session | private dirty/session | boot Pss | boot PrivDirty | boot SharedClean |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| lz4 | 1 | **65.45** | — | 201.1 | 47.4 | 40.6 | 26.34 | 0.0 |
| lz4 | 5 | **66.85** | — | 79.7 | 48.3 | 29.3 | 26.34 | 14.2 |
| lz4 | 10 | **64.41** | — | 61.7 | 46.0 | 27.8 | 26.34 | 14.3 |
| sf (B2) | 1 | 54.25 | −11.2 | 239.1 | 35.7 | 82.1 | 13.95 | 0.0 |
| sf (B2) | 5 | 53.59 | −13.3 | 76.8 | 35.5 | 27.7 | 13.95 | 68.9 |
| sf (B2) | 10 | 52.98 | −11.4 | 55.4 | 35.1 | 20.5 | 13.95 | 66.3 |
| sfg | 5 | 53.29 | −13.6 | 73.5 | 35.4 | 28.3 | 13.95 | 72.3 |
| sfg | 10 | 52.30 | −12.1 | 53.3 | 34.1 | 21.1 | 13.95 | 72.3 |
| sa | 1 | 56.74 | −8.7 | 242.4 | 38.2 | 83.2 | 13.95 | 0.0 |
| sa | 5 | 55.11 / 55.27 | **−11.6** (median) | 78.5 / 73.8 | 37.0 / 37.1 | 27.9 / 27.1 | 13.95 | 70.1 / — |
| sa | 10 | 55.60 / 52.58 | **−10.3** (median) | 57.2 / 53.0 | 37.2 / 34.5 | 20.2 / 20.4 | 13.95 | 62.9 / — |

The memory win is the `26.34 → 13.95 MiB` boot-image private-dirty drop plus
`Shared_Clean` (63–72 MiB/session at N=5/10) from the file-backed mapping. The
shared image is **worse at N=1 on ΣPss** (the whole image is charged once), as
B3 found; on the M2 metric it is still cheaper. `shared-aslr` lands 1–3 MiB
above `shared-fixed` on some cohorts: rebuilding at a random base changes the
image/class layout enough to shift `LinearAlloc`/region-space runtime
allocation (sa-n10 first cohort: linear 4.05 MiB vs 2.07 in the second). That
is real variance, not a leak; the 10–12 MiB median saving survives.

- **One-time prep and launch.** `shared-aslr` first verify **7.23 s**, repeat
  verify with the same cohort **3.60 s** → **3.63 s** prep (the second
  `shared-fixed` pair: 7.22 / 3.72 → 3.50 s; both `dex2oat64` builds, ~173 MiB
  of image files). M3 cold (new serve per rep, page cache evicted) therefore
  moves from **409.3 ms p50** (lz4, p95 461.6, load 1.1–1.7) to **4133.9 ms
  p50** (shared-aslr, p95 8473.1, load 8.0–11.0), **+3.72 s**. Warm M4 is
  unchanged: lz4 vs shared-aslr **strict 309.0 vs 308.0 ms**, **fast 175.5 vs
  160.8 ms** (3 reps, one serve per phase; no regression). M7 strict 41.1 vs
  41.2 ms. The prep is paid once per serve, not per session: the A/B was taken
  on the first launch of a cohort.
- **H6 agent loop / tasks per GiB-min.** H2 early GC on, P5 loop, 3 reps:
  lz4 **277.4 (N=1) / 664.7 (N=5)**; shared-fixed (prebuilt B2 image, no prep)
  **247.5 / 572.1**; shared-aslr **112.8 / 264.3**. The shared-aslr number is
  dominated by charging the first-launch `dex2oat64` (~400 MiB anon, ~4 s CPU)
  and the 173 MiB file-cache pages to the measured job cgroup. The same P5
  records the fairer **net anon+shmem** integral: lz4 ≈ **900 / 845**,
  shared-fixed ≈ **986 / 1065**, shared-aslr prep-inclusive ≈ **586 / 559**
  tasks/GiB-min. Verified task rate is **1.000** in every arm (0 false passes).
  Read: the shared image does not slow the steady-state loop; the one-time prep
  is the only regression, and it is visible only in short (per-serve) cohorts.
- **Correctness under `shared-aslr` (one cohort, image built once).** Pass:
  `v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`, `w42-back-frame`,
  `w15-dialog-menu`, `form-validation`, `suite/T03-render-good`,
  `v1-offscreen-pager`. Expected fail: `suite/T03-render-wrong` →
  `ASSERTION_FAILED` (568 029 differing bytes). P11: **0 false passes** over
  3 × the full settle-lab corpus; `progress` (p11-progress-timeout) is
  **not settled in 5/5**, i.e. a non-pass, never a false pass; the
  `scenarios/p11-*.yaml` sweep has **0 unexpected failures**.
  `cargo test --release --offline -p acos-art -p acos-cli` passes. Bad-knob
  control (`ACOS_ART_BOOT_IMAGE=bogus`) refuses the launch with the Layout
  error, no LZ4 fallback.
  Two concurrent sessions of one serve map the same inode at the same base;
  raw `smaps` of the five-session cohort has `boot.art` at
  `0x74690000–0x74af8000` in all five and `Shared_Clean` 2.99 MiB on the primary
  mapping alone (the full boot group is 70.1 MiB/session at N=5). Three separate
  serve starts chose `0x71fd7000`, `0x74690000`, `0x74a46000`. `gaten-home` is
  **TIMEOUT at its first `wait_for` on both arms** (lz4 and shared-aslr,
  reproduced twice on lz4); that is a pre-existing main/host-load issue, not a
  shared-image regression — the launch step passes and no shared-image code is
  in the settle path. Recorded rather than hidden.
- **Security analysis.** `shared-aslr` keeps the memory win with **16 bits** of
  image-base entropy per serve (ART's own ±128 MiB / 4 KiB window), the same
  shape as stock Android's per-boot zygote choice; a new serve is a new base.
  Sessions of one serve share the base, exactly as apps in one Android boot do.
  The image is a **read-only** file mapping (`--ro-bind`, files `0444`, dir
  `0700`): a session cannot write it or affect a sibling; the B3/S4 canary
  covers the read-only shared-input rule. `shared-fixed` remains fixed
  `0x70000000` and trusted-only. The new costs are operational (3.6 s first
  launch, 173 MiB disk per cohort), not a new host read/write surface.
- **Conclusion.** H13's hypothesis is **confirmed at the memory layer**: a
  per-serve relocated uncompressed image is correct, shares its pages, and
  saves ~10–12 MiB/session at N=5/10 with no warm-launch or false-pass
  regression. It is **not proposed as the default** because it adds **3.6 s**
  to every new serve's first launch (and inflates short-cohort `tasks/GiB-min`
  by charging the `dex2oat64` build to the cgroup). Recommendation: keep `lz4`
  as the default; expose `shared-aslr` as the opt-in for long-lived serves and
  leave `shared-fixed` trusted-only. The owner decides whether the cold-start
  cost is acceptable for untrusted runs. A cheap offline relocator (patch the
  image at a random base without re-running `dex2oat64`) is the follow-up that
  would remove the 3.6 s cost. Tables and raw JSON: `results/vm/H13.md`,
  `.cache/h13/`.

## E5.8 — K3d: manifest `<meta-data>` becomes the `PackageItemInfo.metaData` Bundle, gated on `GET_META_DATA` — MEASURED (accepted)

- **Date / commit:** 2026-11-13; K4 worktree branch `compat-k4-metadata` (off
  `main` `5d211a6`).
- **Agent / bottleneck:** K4; K3's E5.6 follow-up 1. With provider delivery on,
  `io.element.android.x` died in `SentryInitProvider` (`DSN is required`) and
  `org.billthefarmer.editor`/`com.libreweatherrn` died in
  `FileProvider.attachInfo` (`Missing android.support.FILE_PROVIDER_PATHS
  meta-data`) because nothing parsed `<meta-data>` and no reply carried a
  `metaData` `Bundle`.
- **Baseline (K3, providers on):** first frame 11/42; editor, libreweatherrn and
  joplin *below* their K3-off rungs.
- **Hypothesis + solution:** implement the platform's own path, in the platform's
  own order:
  1. **`acos-apk`** parses `<meta-data android:name android:value|android:resource>`
     under `<application>`/`<activity>`/`<service>`/`<receiver>`/`<provider>` into
     a typed model, transcribed from `.cache/aosp/base/.../PackageParser.java`
     `parseMetaData` (android-14.0.0_r75, line 5597): `android:resource` first and
     stored as `putInt(name, resourceId)`; `android:value` as `putString`
     (TYPE_STRING), `putBoolean` (TYPE_INT_BOOLEAN), `putInt`
     (TYPE_FIRST_INT..TYPE_LAST_INT = int dec/hex **and the four colour types**),
     `putFloat` (TYPE_FLOAT); any other type (including a reference in
     `android:value`) is not stored, exactly as `RIGID_PARSER=false` does.
  2. **`acos-services::pms`** carries the entries on `AppInfo`, `ActivityInfo`,
     `ProviderInfo`, `ServiceInfo` and writes them into the `metaData` parcel
     field **only when the call's `flags` (a `long` on API 34) contains
     `PackageManager.GET_META_DATA` = `0x80`**, which is what
     `PackageParser.copyNeeded`/`generateApplicationInfo`/`generateActivityInfo`/
     `generateServiceInfo`/`generateProviderInfo` do. `getPackageInfo` propagates
     the same flags into the nested `ApplicationInfo`.
  3. **`acos-art`** publishes the APK path the *guest* can open
     (`/data/app/<name>.apk`) as `ApplicationInfo.sourceDir`/
     `publicSourceDir`. `PackageManager.getResourcesForApplication` builds
     `Resources` from that field, so the host path ACOS used to publish made every
     `PackageItemInfo.loadXmlMetaData` fail: `ApkAssets.nativeLoad: Failed to load
     asset path <host path>` — FileProvider's own XML read, i.e. the second half of
     K3's FileProvider failure.
  4. **`acos-art`**'s guest `LaunchMain` also puts the `<application>` `<meta-data>`
     on the bind request's `ApplicationInfo` (`context.getApplicationInfo()
     .metaData`, which is how Sentry reads `io.sentry.auto-init`) and each
     `<provider>`'s `<meta-data>` on the `ProviderInfo` the framework hands to
     `ContentProvider.attachInfo`.
- **Method:** targeted batches with delivery on (element, editor,
  libreweatherrn, joplin), then the whole 42-app corpus **A/B** under
  `run-exclusive.sh --task K4` + `guarded.sh --mem 8G`, batches of 4
  (`ACOS_ART_APP_PROVIDERS=on|off`; the two arms ran back to back on the same
  release binary, one `acos serve` per app, APKs on the exec path), then the
  shipping default. Host state: 21–23 % RAM used (60 GiB box) at every batch
  start, `/` 79 % full, load 30+ from the four other agents sharing the ART lock
  (queue waits 25–150 s per batch, recorded in `.cache/art-locks/runs.log`); no
  batch exceeded the 90 % RAM gate. Same host state for both arms, so the A/B is
  paired in time.
- **Result (delivery ON, 42 apps, `.cache/k4/runs-on`):** first frame **17/42**,
  tree/screenshot 16/42, tap 9/42, `onCreate` completed 20/42; p50 808 ms,
  p90 1269 ms. `org.billthefarmer.editor` is a **full-ladder pass** again
  (416 ms, was a FileProvider failure), `org.jellyfin.mobile`,
  `net.gsantner.markor`, `com.money.manager.ex`, `com.github.muellerma.coffee`,
  `com.beemdevelopment.aegis`, `org.kde.kdeconnect_tp` keep theirs, and
  `com.perflyst.twire`, `de.danoeh.antennapod` join the tap rung.
  `io.element.android.x` leaves metadata entirely: `InitializationProvider` and
  `SentryInitProvider` both run and the app reaches `MainActivity`, where the
  next wall is the classified `AndroidKeyStore` one (K3a).
- **Finding (proven):** the metadata path is now the platform's: parse once at
  the lowest layer, carry it on the typed record, parcel it only under the flag,
  and let `FileProvider`/`androidx.startup`/Sentry read it through their own
  `resolveContentProvider`/`getProviderInfo`/`getApplicationInfo` calls. The
  `GET_META_DATA` gate is observable: the same reply without the flag carries
  `null` (`crates/acos-services/tests/k4_metadata.rs` asserts both; the guest's
  own trace shows the real calls carrying `flags=0x80`, e.g.
  `resolveContentProvider key=org.billthefarmer.editor.fileprovider flags=0x80
  get_meta_data=true meta_entries=1`). Proven separately: the failure that
  remained after the Bundle arrived was **not** the Bundle but
  `Resources` over a host path (item 3 above), which is why the fix is three
  layers and not one.
- **Fidelity / evidence:** `results/compat/K4.md`, machine-readable
  `.cache/k4/k4-on.json`, per-app evidence under `.cache/k4/runs-on/<pkg>/`;
  new tests `crates/acos-apk/src/manifest.rs` (typed values, resource reference,
  a real corpus APK) and `crates/acos-services/tests/k4_metadata.rs` (flag
  gating on every `PackageItemInfo` reply + Bundle parcel round trip).
- **Regression / false pass:** none measured: ON is >= OFF at every rung with
  **zero per-app regressions** (`tools` diff in `results/compat/K4.md` §2), and
  no app that passed at K3 dropped a rung in either mode.
- **Conclusion / next:** accepted; it is what lets E5.6's provider delivery
  become the default (K4 §3). Remaining, sized in K4 §6: `<instrumentation>`
  metadata and `<meta-data>` under `<manifest>`-level elements ACOS still does
  not model; the non-ART (`acos-runtime`) path materialises `metaData` as `null`
  because it builds `ApplicationInfo` through the Java bridge rather than through
  these parcelables.

## E5.9 — K3e: the userspace binder driver split a `BR_*` command at the 256-byte read-buffer edge — MEASURED (accepted)

- **Date / commit:** 2026-11-13; same branch.
- **Agent / bottleneck:** K4; K3's E5.6 follow-up 2. With provider delivery on,
  `net.cozic.joplin` (7 providers) died in
  `IActivityManager$Stub$Proxy.publishContentProviders` with
  `BinderProxy.transactNative` → `java.lang.RuntimeException: Unknown error`,
  and the guest log carried
  `E/IPCThreadState: *** BAD COMMAND -847180016 received from Binder driver`.
- **Baseline (K3, providers on):** joplin: no process start rung (crash during
  provider install); K3 classified it as "app-published `IContentProvider` fails
  to marshal".
- **Hypothesis + solution:** not a marshal bug. `publishContentProviders` exports
  one binder per provider; each new node makes the driver pend
  `BR_INCREFS` + `BR_ACQUIRE` (2 x 20 bytes). libbinder reads through one fixed
  256-byte Parcel (`IPCThreadState` ctor: `mIn.setDataCapacity(256)`, and
  `talkWithDriver` sets `read_size = mIn.dataCapacity()`), and the kernel driver
  never copies a *partial* command — a command either fits or stays queued.
  ACOS's `flush_pend` copied `min(pend_len, read_size)`, so 7 providers
  (7 x 40 + 8 = 288 bytes) were cut inside the 7th `BR_INCREFS`; the leftover
  pointer word was then read as a command
  (`IPCThreadState::executeCommand` default branch, `BAD COMMAND %d`).
  Fix: `flush_pend` walks `br_cmd_len()` (header + the payload of exactly the
  commands this driver pends: `BR_TRANSACTION_COMPLETE`/`BR_FAILED_REPLY`/
  `BR_NOOP`/`BR_SPAWN_LOOPER`/`BR_DEAD_REPLY` 4 B, `BR_INCREFS`/`BR_ACQUIRE`/
  `BR_RELEASE`/`BR_DECREFS` 20 B, `BR_TRANSACTION`/`BR_REPLY` 68 B) and copies
  only whole commands, leaving the rest in `pend` for the next call — the kernel's
  invariant, restored.
- **Method:** one-app runs with `E3G_DEBUG=1` (the driver's own trace printed
  `REQ seq=46 code=31 ... size=10740/64` = 8 flat objects: 7 providers +
  `IApplicationThread`, then 7 `node refs` lines = 280 bytes of `BR_INCREFS`/
  `BR_ACQUIRE`), then the full corpus A/B on the same host state as E5.8
  (21–23 % RAM, load 30+, one lock acquisition per 4-app batch).
- **Result:** `net.cozic.joplin` goes from "no start rung" to **first frame at
  1269 ms + tree + screenshot** (tap still fails later, on a wall it owned
  before). `io.element.android.x` also stops dying in
  `publishContentProviders` (it has 6 providers and was the K4 run that made the
  same failure visible there).
- **Finding (proven):** the 256-byte read buffer is a hard driver contract, not
  an implementation detail: any driver that splits a command produces libbinder's
  `BAD COMMAND` and a transact that fails with `Unknown error`. The Rust peer was
  never wrong; the marshal was correct and the driver's refill was not.
- **Fidelity / evidence:** `results/compat/K4.md` §4,
  `.cache/k4/runs-dbg-e3g/net.cozic.joplin/` (E3G trace + logd),
  `crates/acos-binder-peer/native/binder_driver.c` (`br_cmd_len`/`flush_pend`).
- **Regression / false pass:** none; the corpus ON funnel after the fix is
  >= the pre-fix one at every rung, and the K3-off default funnel is unchanged
  (`results/compat/K4.md` §2). A host-side C unit test for `flush_pend` would need
  a new harness (the driver is only built for the NDK target), so the evidence is
  the reproducer + the corpus.
- **Conclusion / next:** accepted. Follow-up: give the driver a C unit-test
  harness (feed a synthetic `pend`, assert only whole commands are copied), and
  count `BR_*` bytes per transaction so a future command type cannot silently fall
  outside `br_cmd_len`.

## E13.5 — W7: report the system-bar insets instead of carving the app window — PASS as an opt-in model; default stays `carve` (one strict gate changes by 1/255)

- **Date / commit:** 2026-11-13; worktree `/home/Qui/.paseo/worktrees/3oxa6xao/w7`, branch
  `wm-w7-insets` (off `main` `52def4f`). Real ART guest; no phone; all runs under the host-wide
  `run-exclusive.sh` lock and `guarded.sh --mem 8G`.
- **Question / goal:** GR1 E13.1(b) measured Gaten's footer 60 px (24 dp) low on ACOS: the WMS shim
  handed the app an already-carved window (`DecorView [0,108][1080,2400]`) and a real `InsetsState`
  calculated to zero, while the phone reported status/gesture insets the app consumed itself. Can
  the window manager model AOSP's full-display frame + reported insets, without special-casing an
  app, and without weakening a verification assertion?
- **Where the carve was:** `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java:2821`
  (`APP_H = PANEL_H - STATUS_INSET - NAV_INSET`, fed to `DisplayInfo.appHeight`) and
  `crates/acos-wm/src/wms.rs:742` (`compute_frame_for_requested`: `window_bounds =
  display.app_bounds`), built from `crates/acos-wm/src/display.rs:183`
  (`app_bounds = Rect(0,status,width,height-nav)`). Because the frame excluded the bars,
  `InsetsState.calculateInsets(mWinFrame, …)` returned zero; that is why `--nav-inset` alone never
  moved Gaten's footer.
- **Change:** new `ACOS_WM_INSETS=carve|report` selector (plus `acos launch --insets`,
  `acos-art launch --insets`, and the serve/MCP `acos_launch` field). `report` follows
  `WindowLayout.computeFrames`: start at the full display, subtract only the client's
  `fitInsetsTypes`/`fitInsetsSides`; `PhoneWindow.generateLayout` sends 0/0 for a non-floating
  Activity, so it gets the full display and DecorView consumes the reported insets, while a floating
  dialog keeps the default `systemBars()` fit. The guest derives `DisplayInfo` and the merged
  `Configuration` from the same selector; `acos-verify` publishes display-origin `[0,0]`. New
  edge-to-edge fixture `fixtures/apps/insets-probe` (targetSdk 35) prints the root insets + frames,
  with `scenarios/w7-insets-report.yaml` and `scenarios/w7-insets-carve-negative.yaml`. Default
  remains `carve`.
- **Result (window model):** under `report` on the API-34 profile the probe reads status 128,
  navigation 63, decor `[0,0][1080,2400]`, screen frame `[0,0][1080,2400]`; under `carve` it reads
  status 0, nav 0, decor `[0,0][1080,2209]`, screen `[0,128][1080,2337]`, and the negative scenario
  fails with `ASSERTION_FAILED`. Non-edge-to-edge content is unchanged: counter's app views are
  identical (`count [42,317,63,368]`, `inc [42,368,273,494]`, `reset [42,494,273,620]`, content
  `[0,275][1080,2337]`); only the faithful decor additions differ (`decor_content_parent` spans to
  y=0, `statusBarBackground`/`navigationBarBackground` appear). Same for the dialog fixtures.
- **Result (Gaten, phone reference):** stored device `elements.json` footer band is
  `[2022,2151]` in the 1080x2292 cropped reference = display `[2130,2259]`. `report` with the
  phone's actual bottom inset (`nav=80`, `ACOS_ART_{WIDTH,PANEL_HEIGHT,DENSITY,STATUS_INSET,NAV_INSET}
  =1080,2400,400,108,80`) puts the footer at display `[2130,2259]`: **delta 0 px**. SSIM vs the
  stored device reference rises from GR1's 0.84854 (or 0.74518 recomputed with the committed
  8x8-grayscale method) to **0.87960**; >16/255 differing pixels fall 14.56 % → 7.20 %, mean abs
  channel diff 18.79 → 9.97. `nav=60` (the synthetic 24 dp default) lands 20 px low because the
  reference phone's safe-area bottom is 80 px; `carve` with `nav=60` also matches the footer but
  keeps the zero-inset, carved-window model. Evidence:
  `results/render/W7-evidence/{metrics.json,welcome-report80-380.png,welcome-report60-380.png}`.
- **Why the default stays `carve`:** `w42-back-frame` changes under `report` by exactly one
  anti-aliased row (display y=283, 122 px, G 210→211; deterministic over 3 runs). Every other frame
  and the second region are byte-identical. Accepting that would require weakening a strict RGBA8
  hash, which is a verification-semantics change, so `report` ships opt-in and `carve` remains the
  default. `report` also changes frame metadata (display-sized PNG, origin `[0,0]`), which is more
  faithful but not compatible with every frozen frame-size assumption.
- **Gates, both arms:** all 7 non-Gaten gates pass under `carve`; under `report` 6/7 pass, with
  `w42` failing only for the 1/255 row above. `T03-render-wrong` → `ASSERTION_FAILED`,
  `negative/offscreen-pager-visible` → `ELEMENT_OFFSCREEN`, `negative/offscreen-pager-wait` →
  `TIMEOUT` in both arms. P11: 11 pass + the declared `p11-progress-timeout` non-pass in both arms,
  **0 false passes**; all 12 `scenarios/negative/*` fail correctly in both arms.
  `gaten-home` under the stock animated profile times out at the welcome capture on both arms, and
  the **main** binary reproduces that timeout (pre-existing continuous-animation/settle wall, R2);
  with `--animations off` + `ACOS_STRICT_SETTLE_CONFIRM_MS=0` + phone geometry nav 80 it passes
  **14/14 under `report`** (and stops at the Skip tap under `carve`, whose local visible rect clips
  the bottom row).
- **A/B (M3/M4, ART lock, host load recorded):** M3 cold 408.77 → 402.23 ms (−1.6 %, noise); M4
  strict 309.76 → 309.68 ms (−0.03 %); M4 fast 175.53 → 175.58 ms (+0.03 %); M5 24.24 → 24.55 ms.
  Load1 per M4 rep: carve 3.89/1.65/2.72, report 1.90/0.55/2.89; MemAvailable 46.4–47.5 GiB.
  The stock cold phase refuses the report tree (`EXPECT_NODES=12` vs the 14 nodes with the two
  system-bar backgrounds); the direct P7 run changes only that measurement constant.
- **Regression / false pass:** `cargo test --offline -j2 --no-fail-fast -p acos-wm -p acos-services
  -p acos-verify -p acos-art -p acos-cli` rc=0 (79 binaries). The full workspace run is still
  blocked by the pre-existing `acos-vm --test render_views` hang (hangs standalone too; `acos-vm`
  does not depend on any W7 crate) and the pre-existing `acos-arm64`/qemu + frozen-VM failures.
  **0 false passes.** No assertion was loosened.
- **Conclusion / next:** the window model is now a selector, and `report` is faithful enough to hit
  the device footer exactly (0 px) and improve SSIM. Making it the default needs either (a) the
  `w42` 1/255 AA difference accepted as a changed, more-faithful pixel expectation (owner call), or
  (b) the strict capture/hash path made tolerant of a single AA row without hiding real moves. The
  `gaten-home` continuous-animation capture is the other blocker: strict settle cannot bind the
  forever-looping welcome screen, and the main binary fails it too (R2's area).

## E3.4 — S3: Stage 3 scale — agent loop at N=1…30, K1 compat and Gaten at concurrency — MEASURED (2 reset defects found)

- **Experiment ID / name:** S3 — Stage 3 real-agent scale and compatibility (plan §7).
- **Responsible agent:** S3 (worktree `scale-s3-n20`, off `main` `15849a5`); product code untouched.
- **Bottleneck investigated:** the loop metrics stopped at N=10 (E3.2/E3.3) and H17 fixed N=10
  reliability at 10 sessions. Stage 3 asks whether the *verified* loop, the 42-APK compat ladder and
  Gaten hold at N=20/N=30 and at N=5/N=10 concurrency, and what the ceiling is. Two questions are
  separate: does throughput scale (it should) and does anything break only under N (H17 showed it can).
- **Baseline (p50):** E3.2/E3.3 current main: loop 1438.0 ms/task / 0.653 tasks·s⁻¹ with
  283.5 (page-cache) / 905.3 (anon+shmem) tasks/GiB-min at N=1 and 1428.1 / 2.679 / 667.6 / 865.8 at
  N=5; 1423.9 ms → 507–523 ms with animations off (E3.3); memory/session 64.96/65.22/65.59 MiB at
  N=1/5/10; H17 N=10 600/600 verified, N=20 40/40 (2 tasks/session). Compat N=1 (K3): 12–13/42 first
  frame, 12/42 tree, p50 647 ms / p90 1050 ms. Gaten was last exercised at N=1 only (GR1/GR2).
- **Hypothesis + proposed solution:** (a) latency and per-session footprint are already flat in N, so
  the loop should add throughput linearly up to RAM/cpu limits with 0 false passes; (b) the compat
  funnel is per-app-wall bound, so N=5/N=10 must not lose an app that frames at N=1; (c) Gaten is the
  RN stress case and must frame+tree at N=10 with no crash. Add `loop20`/`loop30` to the H6 bench on
  the existing `loop10` pattern, record per-cell p90/per-session ΣPss/anon+shmem/host CPU, and add one
  concurrency harness for the corpus and Gaten rather than hand-driving sessions.
- **Implementation + commits:** tool only (rule: code changes limited to `tools/bench/**`).
  `tools/bench/acos_bench.py`: `loop20`/`loop30` phases + `loop_n{20,30}_*` metrics; per-cell cohort
  block (p50/p90/p95/max, host CPU%, host cgroup, per-session ΣPss and anon+shmem from a 1 Hz sampler
  on the P5 serve tree, the raw per-sample series), no change to any existing metric. `tools/bench/acos-bench.sh`:
  `loop20|loop30` groups (24G, 700 s). **new** `tools/bench/s3_scale.py`: `--mode compat` (42-APK
  queue, N workers, launch→first frame→fast tree→destroy) and `--mode gaten` (N sessions behind a
  barrier, fast settle, immediate + 2 s tree, 5 s held for the memory read, crash scan of
  `session-exit.json`); reuses `d2_warm.Serve`, `WarmSampler`, `benchlib`. Results:
  `results/bench/S3.md`, `results/bench/S3-summary.json`,
  `results/bench/S3-evidence/loop-n20-reset-{nofreshframe,guestexited}.json`. Binary `acos` sha256
  `a90b839d…` (release, real ART).
- **Benchmark method + environment + host load:** 24 loop cells (N=1/5/10/20 × animations on/off × 3
  reps, arms interleaved `on,off`/`off,on`/`on,off`), 4 N=30 ceiling reps (3 on, 1 off), 4 compat runs
  (2 × N=5, 2 × N=10), 6 Gaten runs (2 × N=1/5/10), 1 P11/scenarios false-pass group, 2 failure
  reproductions (11 P5 reps at N=20). One `run-exclusive.sh --task S3 -- tools/guarded.sh --mem 24G`
  acquisition per cell, **lock hold (elapsed − queue) 8–151 s**, every acquisition < 10 min. Host:
  Ryzen 9 9950X, 32 threads, 61,908 MiB; scratch `.cache/s3/**`, `ACOS_FONT_DIR` set, `ACOS_BOOT_DIR`
  unset, no `/tmp`. **Interleaved with other agents** (recorded, not avoided): GR3 51 acquisitions,
  K4 20, H15 20, MG6 16, MG5 15, W7 8, H13 3, MG4 3, K5 2 in the same window; queue waits 0–483 s;
  every cell states its own load1 (loops 0.71–3.52; N=30 5.1–13.0; compat 2.7–4.1; Gaten 2.5–5.9) and
  `MemAvailable` (45.6–48 GiB throughout, host used ≤ 24 %).
- **Results, agent loop (medians of 3 reps; 0 false passes in every cell):**

  | animations | N | verified/total | ms/task p50 / p90 | tasks/s | tasks/GiB-min cgroup / anon+shmem | anon+shmem/session (peak) | ΣPss/session | host CPU |
  |---|---:|---:|---|---:|---|---:|---:|---:|
  | on | 1 | 63/63 | 1437.8 / 1451.9 | 0.65 | 278.3 / 887.6 | 87 MiB | 177 MiB | 2.1 % |
  | on | 5 | 75/75 | 1422.5 / 1443.7 | 2.68 | 663.4 / 847.8 | 77 MiB | 74 MiB | 5.8 % |
  | on | 10 | 149/150 | 1443.9 / 1465.0 | 4.96 | 651.2 / 721.2 | 73 MiB | 62 MiB | 15.6 % |
  | on | 20 | 298/300 | 1460.4 / 1501.8 | 7.41 | 471.9 / 603.0 | 73 MiB | 53 MiB | 21.2 % |
  | off | 1 | 63/63 | 496.4 / 517.4 | 1.82 | 774.7 / 5828.6 † | 81 MiB | 173 MiB | 6.4 % |
  | off | 5 | 75/75 | 510.7 / 537.5 | 6.68 | 1704.1 / 3543.1 † | 75 MiB | 77 MiB | 8.7 % |
  | off | 10 | 150/150 | 519.2 / 540.6 | 11.21 | 1520.3 / 3046.5 † | 68 MiB | 59 MiB | 15.3 % |
  | off | 20 | 300/300 | 543.8 / 575.3 | 10.49 | 711.7 / 1414.2 † | 65 MiB | n/m ‡ | 17.3 % |

  Latency is flat in N (±1.3 % p50 at N=1…20 on; p90 +1.5…+3.5 %), throughput is linear
  (0.65 → 7.41 tasks/s on, 11.4×), footprint is flat (73 MiB/session anon+shmem at N≥10; ΣPss/session
  falls 177→53 MiB because Pss divides the shared image by its mapping count). Pooled **1173/1176
  verified (99.74 %)** in the 24 cells. Off arm is 2.7–2.9× lower latency at every N.
  † off-arm anon+shmem GiB-min is **inflated by the sampler** (1 s guest-scope discovery vs a ~0.5 s
  guest lifetime): it is a tool defect (`S3c`), recorded rather than quoted. The sampling-independent
  view is flat too: at the measured 65.6 MiB/session the steady rate `60/(p50_s × 0.0641)` is
  651/658/648/641 tasks/GiB-min (on) at N=1/5/10/20 — **verified tasks per GiB is a constant of the
  loop shape, not of N**; scale buys throughput linearly and costs linearly.
  ‡ off N=20 ΣPss read 2.7 / 0.0 / 51.5 MiB/session across reps (sampler sees launchers between
  resets, `S3c`); the on-arm 53 MiB read is the usable one.
- **The ceiling — N=30 (largest probed; N=20 used < 50 % of host RAM, so the gate allowed it):**
  **600/600 verified (100 %)**, 0 false passes over 4 reps (3 on, 1 off); p50 1534–1541 ms on
  (706 off), p90 1613–2150 on (2396 off); 5.78–6.79 tasks/s on; **job cgroup peak 2.34–2.67 GiB**
  (≈ 4 % of 61.9 GiB), host `MemAvailable` ≥ 45.6 GiB (host used 23–24 %, far under the 90 % rule),
  no `oom_kill`. The ceiling is therefore **≥ N=30**, limited by latency stability at load1 10–13
  and not by memory or correctness. N>30 was not probed (packet cap).
- **K1 compat corpus at concurrency (42 APKs):** N=5 **13/42 first frame, 13/13 tree** in both clean
  runs (p50 548–549 ms, p90 1010–1093; tree p50 0.73–0.78 ms, 26 nodes); N=10 **13/42 / 13/13**
  (p50 593–603, p90 1152–1199; tree 0.79–0.91 ms, 26 nodes). Baseline K3 N=1: 12–13/42, p50 647,
  p90 1050. **No app that frames at N=1 fails at N=5 or N=10**; the only differences are the known
  per-app flickers (`com.libreweatherrn` frames in all runs; `org.wikipedia` framed in 2 of 4 N=10
  runs and 0 N=5 runs — same app-level `exit 10` wall). Failure class is **only `GUEST_EXITED`**
  (29/29 and 29/28/29 per run): the same per-app walls, no hang, no `native_crash`, no timeout.
  Job cgroup peak 4.9–5.9 GiB (page cache of 200 MB APKs), host CPU 15–24 %.
- **Gaten at scale (RN 0.86.3/Fabric/Hermes, fast observation, no strict assert):** **16/16 launches
  reached first frame and tree** (2 reps at each N): p50 810–867 ms at N=1, 1007–1104 at N=5,
  1431–1441 at N=10 (p90 1.48–1.50 s), CPU 7–16 / 22–28 / **47 %** of one core. The tree is the same
  welcome screen at every N — 243 nodes, `Look up Japanese anywhere / Study with Anki / Connect
  WaniKani / Turn on lookup mode` — while a tree read *immediately* after the first frame returns
  8 nodes with no text at every N (the RN content mounts up to ~2 s later; that is per-app, not
  scale). Memory: **182–194 MiB anon+shmem per session flat**, ΣPss/session 175–199 MiB at N≥5, job
  cgroup peak 617 MiB (N=1) → 1.63–1.77 GiB (N=5) → 3.66–4.11 GiB (N=10; the per-session heap + the
  per-run 115 MB staged APK's page cache). **0 native crashes** in 16 exits (all `outcome: ok`,
  `stage: teardown-evidence`, `native_crash: null`) — the first Gaten crash-evidence record at scale.
- **False-pass check:** loop `false_pass` **0** in all 28 cells (1 776 measured tasks, 1 773 verified;
  the three losses are one reset step at N=10 and two at N=20, counted as failures); P11 settle lab
  on the same binary **0 false passes** (12/12 cases, the progress case an honest `timeout`);
  `scenarios/p11-*.yaml` **0/12 failed**. The compat/Gaten harnesses make no assertion, so nothing
  they record can be a false pass.
- **Defects found (new Backlog rows):** `S3a` reset relaunch **hangs with no live frame**
  (`NO_FRESH_FRAME`/`LIVE_FRAME_UNAVAILABLE`, 60 048 ms, `activity_resumed=true probe=true
  c1_generation=0`; 1/100 N=20 bench + 1/100 reproduced); `S3b` reset relaunch **guest exits 1 in
  133–301 ms** (`GUEST_EXITED: … before its first live frame (status exit status: 1)`; 1/150 at N=10,
  1/300 at N=20, 1/100 reproduced; no signal/native_crash record, guard line not retained → cause
  open, H17's class must be ruled out); `S3c` **the bench's own anon+shmem/Pss sampler undercounts
  churn** (1 s discovery vs sub-second guests; Pss sees launchers between resets). Evidence:
  `results/bench/S3-evidence/`.
- **Why it worked / what it found:** the loop scales: latency and footprint are flat to ±1.3 %/±1 MiB
  per session from N=1 to N=30, so verified tasks/s grows 11.4× and the ceiling is set by host load
  (latency p90) rather than memory. The compat ladder is per-app-wall bound and does not lose an app
  at 5×/10× concurrency; Gaten's RN path frames and trees at 10× with no crash and flat per-session
  memory. What did **not** survive are two rare reset/relaunch failures at N≥10 that H17's 600/600
  did not sample: they are stochastic, appear as a 60 s no-frame hang and as an immediate exit 1, and
  cost 2/300 tasks at N=20.
- **Trade-offs / regressions:** none in the metrics (latency rows tie E3.2 within the same host-load
  band; N=30 is 5–8 % slower p50 because the host was at load1 5–13). The two reset failures are the
  regression to chase, not a metric change. The `S3c` finding corrects the *interpretation* of the
  off-arm GiB-min (E3.3's 5–8.7× ratio is a denominator artifact; the true ratio is 2.7–2.9×).
- **Follow-ups:** S3a (capture `c1_generation` + FrameStore state for the hanging relaunch),
  S3b (retain the guard/`session-exit.json` line for exit 1), S3c (sampler: walk guest cgroups every
  tick or on change; sample Pss per live session id), then re-run the N=20 on-arm cells to see whether
  the rate goes from 99.3 % to 100 %, and probe N>30 only after a quiet-host pass.
- **Fidelity / evidence:** `art-reference` (real ART, release), MEASURED, 0 false passes;
  `results/bench/S3.md` (all tables, per-rep spreads, method, limits), `results/bench/S3-summary.json`,
  `results/bench/S3-evidence/`, raw phase docs `.cache/s3/bench-run-*/`, `.cache/s3/logs/`.

## E3.5 — S3c: the bench memory sampler's churn undercount — FIXED, metric corrected (PASS on the tool, off/on GiB-min ratio re-explained)

- **Experiment ID / name:** S3c — trustworthy tasks/GiB-min for the agent loop (plan §7, backlog `S3c`).
- **Responsible agent:** S3c (worktree `s3c`, branch `bench-s3c-sampler`, off `main` `5cae694`);
  metric tooling only — no product code touched.
- **Bottleneck investigated:** E3.4 §1.2 could not quote the off-arm GiB-min at all. `WarmSampler`
  (d2_warm) re-walked `/proc` for guest cgroups once a second, so guests living < 1 s (every
  animations-off task, every short compat app) were absent from most samples, and `session_pss()`
  summed `acos serve`'s *current* children, so a relaunch burst read launchers without guests
  (off N=20: 2.7 / 0.0 / 51.5 MiB per session).
- **Baseline (p50):** main N=20 loop, off arm 530.8/534.6 ms/task, on arm 1467–1490 ms; published
  denominators 1414.2 (off) / 603.0 (on) tasks·GiB⁻¹·min⁻¹ (anon+shmem) and 65 MiB/session, of which
  only the footprint was trustworthy.
- **Hypothesis + proposed solution:** the defect is discovery latency, not the poll rate — so discover
  guest scopes **on creation** (one `listdir` of the slice the job scope lives in, ownership proved by
  walking the new scope's own ppid chain; the 1 s `/proc` walk kept as a backstop), read the kernel's
  monotone `memory.peak` per scope as a sampling-independent cross-check, and charge Pss **per session
  id** (run-tree path in the guest's cmdline, `<ACOS_SERVE_ARTIFACTS>/<session>/…`) instead of per
  current child. Report coverage and the sampler's own thread CPU beside every number.
- **Implementation + commits:** `tools/benchlib.py` (`ChurnSampler` + `_pid_chain_hits`, `cgroup_procs`,
  `thread_cpu_seconds`), `tools/bench/acos_bench.py` (`phase_loop` runs it *beside* the legacy sampler
  and records `cohort.memory_s3c`; new metric `loop_n<N>_tasks_per_gib_min_net_s3c`),
  `tools/tests/test-benchlib.py` (+4 tests, 232 pass). Commit on `bench-s3c-sampler`.
- **Method + environment + host load:** N=20 agent loop, counter fixture, 1 warm-up + 5 measured tasks
  per session, 20 sessions in one `acos serve`, real ART (`acos bench` release binary `7fb5e56064d3…`,
  unchanged main build, so before/after is a **paired sampler** comparison in the *same* cohort window
  rather than two campaigns). Host Ryzen 9 9950X, 32 threads, 61.9 GiB; one
  `run-exclusive.sh --task s3c -- tools/guarded.sh --mem 8G --time 400` acquisition per rep;
  load1 3.66–8.47, MemAvailable ≥ 44.4 GiB, host CPU 21.9–40.4 % (other agents K5h/S3AB held the lock).
- **Before/after (N=20; GiB-min anon+shmem, before → after):**
  off **1182.1–1225.6 → 553.5–557.0** tasks/GiB-min (0.0846–0.0816 → 0.1807–0.1795 GiB-min);
  on **664.7–768.1 → 427.2–474.3** (0.1504–0.1302 → 0.2341–0.2108). Per-session memory:
  before 2.75–43.8 MiB anon+shmem and 0.0–55.0 MiB ΣPss (a 16×/23× spread including a **0.0** reading);
  after **66.5–68.9 MiB and 53.0–57.0 MiB in every rep, both arms** — matching E3.2's 65.6 MiB/session.
  Coverage: **21 live guest scopes for 20 sessions, zero zero-guest ticks, 100 % of guests carry a
  kernel `memory.peak`, median 17–33 ticks observed per guest**; the launch+reset step rule reads
  141–155/200 and is an upper bound (a relaunch's old and new scope overlap). Correctness 500/500
  verified, **0 false passes**. Sampler cost 8.1–8.2 % of one core in the final tuning (Pss 1 Hz,
  `memory.peak` 2 Hz, `memory.stat` 10 Hz over ~21 scopes); the 20–37 % rows are an earlier
  20 Hz/10 Hz-Pss tuning that was rejected.
- **Result = accepted (tool) / needs-investigation (none).** The corrected metric is the one to quote.
- **Why it worked / why the ratio moved:** the old denominator was too *small* (it counted the
  daemon's ~83 MiB most of the time instead of the ~1.37 GiB actually resident), so published
  tasks/GiB-min were 2.5× (off) and 1.3–1.4× (on) too high. The **2.7–2.9× sampling-independent
  figure is the latency / per-busy-session ratio and it is confirmed exactly**: p50 ratio
  `1489.8/534.6 = 2.79`, `1467.1/530.8 = 2.76`. The *cohort* GiB-min ratio is **1.23–1.30×**, and that
  is correct too: at N=20 the denominator charges 20 resident sessions for the whole cohort while
  throughput is relaunch-bound (both arms idle ~76 % of the cohort wall), so GiB-min at an equal
  footprint (66.5–68.9 MiB both arms) must track throughput (8.87–9.20 vs 6.70–7.58 tasks/s =
  1.17–1.37×). S3's before ratio (2.34×) was manufactured by the *unequal* undercount
  (2.5× off vs 1.3× on), not by a property of the loop.
- **Trade-offs / regressions:** the sampler costs ~8 % of one core at N=20 (it scales with live
  scopes); its charge is subtracted from every sample but it is not zero. Two clean reps per arm
  (not 3): the lock was shared and a third off rep was load-contaminated (off p50 1303 ms at load1
  7.6 — reported, not used).
- **Follow-ups:** make `s3_scale.py` use `ChurnSampler` for the compat/Gaten memory record (that
  record still has the old bias); re-run E3.3's N=1/5/10 off cells with it to replace the 5239/3942/3798
  rows (single-session cohorts, where the old bias was largest); consider an N=30 sampler-cost check.
- **Fidelity / evidence:** real ART, MEASURED, 0 false passes; `results/bench/S3c.md` (tables,
  coverage, sampler cost, limits); raw phase docs `.cache/s3c/loop-n20-*.json`; `tools/tests/test-benchlib.py`.

## E3.6 — B4: fail-closed benchmark harnesses (H17 follow-up) — PASS on the tool; no regression

- **Experiment ID / name:** B4 — a broken launch can never be reported as a fast/good number (H17 follow-up, backlog `H17b`).
- **Responsible agent:** B4 (worktree `.paseo/worktrees/3oxa6xao/b4`, branch `bench-b4-harness` off `main`
  `ac65027`). Tooling only; no product code touched.
- **Bottleneck investigated:** `acos_bench.py`'s M6 loop ignored the initial `launch()` result, so an
  `APK_IO`/launch failure surfaced as 20 fast `SESSION_NOT_FOUND` reset refusals (~0.009 ms) and the
  phase still returned `ok=True`. The same “record the number, ignore the verdict” pattern existed in
  the P5 loop (failed tasks entered `loop_total`), `s3_scale.py` (failed launches/trees entered
  `launch_ms`, `main()` always returned 0, legacy sampler) and the per-task PNG writers (one PNG per
  task, never pruned; S3ab wrote 28 GB).
- **Baseline (p50/p95):** main N=5 loop, animations on, 45 measured tasks: **1431.8 / 1455.6 ms**,
  45/45 verified, 0 false passes. Main M6 with a missing M6 APK: **20 fast latency samples per mode,
  0.009–0.010 ms, `ok=True`, exit 0**, no failure record.
- **Hypothesis + proposed solution:** a harness must be fail-closed: every launch/reset/observe/step
  reply is checked; a failed step is counted with its error code and can never enter a latency array;
  any harness-level failure makes the phase/cohort exit non-zero; PNGs are capped; the S3c
  `ChurnSampler` is the one memory sampler, with coverage printed beside every GiB number.
- **Implementation + commits:** `tools/bench/acos_bench.py` (`StepFailures`, checked M4/M6/M7/M8/tap/
  destroy/M6 launch, latency-only-good `add()`, fail-closed `phase_loop`, `--keep-screens`,
  `--m6-apk` diagnostic), `results/art-runtime/evidence/P5/p5_loop.py` (bootstrap check, verified-only
  latency, error codes, ChurnSampler, coverage, screenshot cap), `tools/bench/s3_scale.py`
  (ChurnSampler, verified-only launch/tree latency, coverage, non-zero exit on missing APK/worker/
  sampler errors), `tools/art-runtime/{d2_warm,dm2/measure,agent_loop}.py`,
  `research/zygote-proto/z7/loop.py`, `tools/benchlib.py` (`prune_screens`, ChurnSampler
  `window`/`extras`), tests `tools/tests/test-benchlib.py` (+2) and new
  `tools/tests/test-bench-harness.py` (9 tests). Commit on `bench-b4-harness`.
- **Method + environment + host load:** real ART release binary (unchanged main build), one
  `run-exclusive.sh --task B4 -- tools/guarded.sh --mem 8G --time 300/540` acquisition per proof;
  negative run load1 1.83 (MemAvailable 44.8 GiB), positive before load1 2.61 / after 6.76
  (MemAvailable 44.9 / 42.9 GiB; after `noisy=true`, recorded). 5 sessions, 10 loops/agent,
  1 warm-up → 45 measured tasks per arm. Unit tests: **242 + 9 passed, 0 failed**.
- **Before/after:** negative M6 broken launch: main **20 + 20 fast samples (0.009–0.010 ms),
  ok=true, exit 0** → **0 latency values (bad=20 per mode), 170 failed steps
  (`APK_IO` 2, `SESSION_NOT_FOUND` 168), ok=false, exit 1**. Positive N=5: verified **45/45 → 45/45**,
  false passes **0 → 0**, p50 **1431.8 → 1448.9 ms (+1.2 %)**, p95 **1455.6 → 1470.6 ms (+1.0 %)**,
  throughput **2.934 → 2.898 verified tasks/s (−1.2 %)** — inside noise, and the after arm ran at the
  higher load1. Coverage now reported: after `coverage=0.571` (S3c 0.667), 60/105 guests observed,
  median 14 ticks/guest, 6 live scopes with **0 zero-guest ticks**, sampler CPU 2.6 % of one core.
- **Result:** **accepted (tool).** No product regression; the GiB-min denominator changed from legacy
  `WarmSampler` to `ChurnSampler`, so before/after GiB-min is a metric correction (S3c), not a win or loss.
- **Why it worked:** the failure is checked at the layer where the reply exists (every step), and the
  latency arrays enforce the verdict at insertion (`add()`), so no downstream summary can resurrect a
  failed sample. The phase/bundle exit code is derived from the failure counters, not from “did we
  produce a number”.
- **Trade-offs / regressions:** failed steps are recorded (capped at 500 rows) with `bad` counters;
  the after positive arm is load-contaminated (load1 6.76) and should be re-run quiet for a headline
  number. `--m6-apk` exists only for negative controls.
- **Follow-ups:** re-run E3.3's N=1/5/10 cells on ChurnSampler; consider a quiet N=5 confirmation;
  `s3_scale` compat/Gaten still needs a real ART run with the new sampler (code path fixed, not yet
  exercised end-to-end here).
- **Fidelity / evidence:** real ART, MEASURED, 0 false passes; `results/bench/B4.md`;
  raw `.cache/b4/neg-{before,after}.json`, `.cache/b4/pos-{before,after}.json`; unit-test output above.

## E13.7 — GR3: Gaten flows past Home on real ART (tabs, Settings, search/word, lesson gate) + the animating-screen/probe wake — PASS on the flows, the animation wall is honest and owner-decided

- **Date / commit:** 2026-11-13; worktree `/home/Qui/.paseo/worktrees/3oxa6xao/gr3`, branch
  `gaten-gr3-flows`, off `main` `10778f9`.
- **Question / goal:** GR1/GR2/E13.4 reached Home and Analytics. What is reachable next in the
  owner's app, can scenarios assert real state there, and what is the lowest ACOS layer behind the
  failures? (Orchestrator priority: the welcome screen never strictly settles; name what animates,
  root-cause the `probe-floor` starvation, and propose — not implement — a verification-semantics
  change for screens that animate forever.)
- **Inputs / method:** the same read-only APK (`f344272`, sha256 `e04c7ffc…`), `.cache/gaten/src-dev`
  read-only, one scenario per ART-lock invocation, `tools/guarded.sh --mem 8G`, scratch
  `.cache/gr3/`, `animations: off` for functional flows. Five scenarios + five negative controls;
  gates and crate tests under the same lock. No Gaten file was changed.
- **Screen map (verified in tree/pixels):** onboarding welcome + 1/3 lookup, 2/3 Anki, 3/3
  WaniKani; Home; Analytics; Settings hub + Lookup, Text recognition, Anki cards, WaniKani, Kanji
  reference, Appearance (Theme System/Light/Dark, accents, Haptics/Sounds), About, Support sheet;
  Home search idle/results/word details (Meanings, Other spellings, Example sentences, Add to
  Anki); Review Anki opening/blocked/decks/card/rating; WaniKani connect/hub/review/lesson quiz.
  Anki decks/cards and the WaniKani queue are **unreachable on this guest** (AnkiDroid not
  installed, no token), so the scenario proves the gate, blocked state, connect form and text input
  instead of faking a lesson.
- **Result (measured):** `gaten-home` **14/14, 3/3 runs** after removing the assert on the launch
  frame (a blank 1-node `first_frame` flake is recovered by the first strict tap's re-traversal).
  `gaten-tabs` **5/6** (one `GUEST_EXITED` SIGSEGV, known R2a, evidence names libc/thread 2);
  `gaten-settings` **3/3** (Theme Light -> Dark is proven by the hub's row changing to `Dark`);
  `gaten-search` **3/3** (type `neko` -> result 猫 -> Word details -> back); `gaten-lesson`
  **3/3** (Home Anki card -> “Anki is not installed”; WaniKani card -> connect form; typed dummy
  token is the field's text). Negatives: all five `scenarios/negative/gaten-*-wrong.yaml` fail
  `ASSERTION_FAILED` (0 false passes). Timing `gaten-tabs`: animations off 8.997 s wall p50, tap
  p50 1307 ms; on 10.356 s / 1339 ms; host load1 2.2–8.9, RAM 13.1–15.3 GB/61.9 GB.
- **Priority answers (a/b/c/d):**
  - **(a) what animates:** `WelcomeScreen.tsx:249` `withRepeat(withTiming(10, 10 s))` — a Reanimated
    timeline, unaffected by the three Android animator scales; `OnboardingFlow.tsx:156/1142/1157`
    re-arms a 200 ms auto-advance retry while a page is unverified, inside the 256 ms strict
    horizon, so the guest reports `queue` forever. Real phone: the same animation and retry run
    invisibly; the page is not visually static.
  - **(b) probe starvation:** the host strict-settle loop woke the guest every 20 ms and re-asked
    even after a sample arrived. Under a 60 fps Reanimated loop that is ~50 full tree walks/s on the
    main thread shared with the animation; the probe sample stops advancing (`probe-floor`,
    `tree_age_ms` ~12 s). **Fix:** wake at 50 ms and only when the newest probe sequence has not
    advanced since the previous request (`ACOS_STRICT_SETTLE_WAKE_MS`). A drawing screen uses its
    own dirty samples; an idle screen still gets a wake. This removes the host flood; it does not
    make an endlessly animating screen settle.
  - **(c) semantics:** options + false-pass analysis in `results/gaten/GR3.md` §4.3: (1) stable
    tree content ignoring frame generation; (2) region mask; (3) navigation-only fast input +
    strict capture/assert elsewhere; (4) explicit non-settle assertion. Only (3) is implemented
    here (scenario inputs may say `settle: fast`; assertions cannot). Owner decision needed for
    (1)/(2)/(4); nothing was weakened.
  - **(d) which screens settle:** Home, Analytics, Settings hub/pages, search results/word,
    Anki blocked and WaniKani connect settle strictly; welcome and the three onboarding pages do
    not.
- **ACOS changes:** scenarios can request the API's existing fast input rule (`tap`/`type`/`key`
  `settle: strict|fast`; protocol schema + executor + backend + `TrustGuard`); ART strict-settle
  probe wake pacing (above). No settle/selector/visibility/assertion semantics changed.
- **Fidelity / evidence:** `art-reference`, real ART guest, host-observed pixels. Evidence:
  `results/gaten/GR3.md` (screen map, step table, timings, walls), `results/gaten/evidence/GR3/`
  (21 captures: tabs, settings incl. dark, search results/word, Anki blocked, WaniKani connect and
  typed token). Per-run JSON/tree/frames in `.cache/gr3/runs/` (gitignored).
- **Regression / false pass:** 5/5 negative controls fail/`ASSERTION_FAILED`; all 8 positive gates
  pass and `T03-render-wrong` fails as required; `cargo test --release --offline` green for
  `acos-protocol`/`acos-verify`/`acos-art` and `acos-cli` with the same pre-existing
  `serve_daemon` socket flake (passes standalone). `gaten-tabs`' one failure is `GUEST_EXITED`
  (R2a), never a pass.
- **Conclusion / next:** Gaten's post-Home product surface is now executable and asserted on real
  ART; the onboarding animation wall is honest and instrumented. Remaining: owner decision on
  animating-screen semantics; R2a/R2b first-paint + SIGSEGV root cause; a real lesson needs
  AnkiDroid or a WaniKani account (data, not ACOS).

## E13.8 — G3d/G3c: the Gaten launch SIGSEGV is libc's GWP-ASan backtrace chase walking off the stack, and the launch's first frame is now the app's content frame — PASS (root cause named, 40/40 launches, 0 thin first frames) ; re-verified 2026-11-14: 30/30 launches, 0 SIGSEGV, 0 one-node frame, 9/30 labelled placeholder

- **Date / commit:** 2026-11-13; worktree `/home/Qui/.paseo/worktrees/3oxa6xao/g3cd`, branch
  `fix-g3cd-gaten-launch`, off `main` `11b917c` (branch head recorded in `results/gaten/G3cd.md`).
  Same read-only Gaten APK as GR1/GR2/GR3/R3 (sha256 `e04c7ffc…`). Gaten source/APK untouched.
- **Question / goal:** name and fix the root cause of Gaten's rare launch `SIGSEGV` (~1 in 6–10:
  `SEGV_ACCERR`, thread 2 `com.gaten`, libc frame — R2a/G3d), and make the launch's first frame
  never a near-empty tree without saying so (G3c).
- **Hypothesis (from R3's evidence, re-symbolised):** the R3 dump's `rip` is
  `libc.so android_unsafe_frame_pointer_chase+0x117` (`mov 0x8(%r12),%rcx`, the "return address"
  read of a frame record), and `SEGV_ACCERR` on a page-aligned address is a `PROT_NONE` stack guard
  page. That function is `LIBC_PRIVATE` and has exactly **one** consumer in libc: `MaybeInitGwpAsan`
  installs it as `Options::Backtrace`. So the process died inside **GWP-ASan's sampled-allocation
  backtrace capture**.
- **Method:** (a) symbolise the fault against the image's own libc (BuildId `fa3379…`), resolve the
  only GOT reference to the chase, and check every sysroot/Gaten `.so` for an import (none);
  (b) A/B the one input that decides GWP-ASan, `GWP_ASAN_PROCESS_SAMPLING` (libc's own lottery
  `arc4random % 128` unless set), with `ACOS_ART_GWP_ASAN={libc,on,off}`; (c) 60/3/40 guarded Gaten
  launches through `run-exclusive.sh --task G3CD` + `guarded.sh --mem 8G`, first frame + a 2 s-later
  tree per rep (`.cache/g3cd/loop.py`); (d) the 5 Gaten scenarios 3× and their 5 negative controls,
  the 8 positive gates + 3 fail-by-design controls.
- **Result 1 — root cause named (G3d).** The faulting instruction is the frame-pointer chase reading
  `frame->return_addr` at `frame+8`; the kernel reports `SEGV_ACCERR` because that address is in a
  stack guard page. The walk's own bound (`r15`, from `__get_thread()->stack_top`) is not this
  thread's stack at all, and x86_64 guest code (optimised libraries, JIT/AOT frames) carries no frame
  pointers, so the walk follows stale stack words. Forcing GWP-ASan on reproduces the exact recorded
  crash: **4/4 launches** die with `SIGSEGV (SEGV_ACCERR) fault addr <page>`, thread 2 `com.gaten`,
  `libc.so android_unsafe_frame_pointer_chase+0x117` — the same registers shape as GR3's `tabsb-2`
  (rbx = 0x80 = 128 entries, r12 = fault-8, end in `0x7ffc…`). That is the causal proof R3 could not
  get.
- **Result 2 — before/after (launches, guarded, ART lock).**

  | arm | n | pass | crash | thin (10-node skeleton) first frame | launch p50 / p90 | host load1 |
  |---|---:|---:|---:|---:|---|---|
  | before: libc's own GWP-ASan lottery (`ACOS_ART_GWP_ASAN=libc`) | 60 | 59 | 1 (`GUEST_EXITED`, exit 1 **before** the first frame — *not* the SIGSEGV shape; no record kept, unclassified) | **20/59 (34 %)** | 2.336 s / 2.408 s | 1.8–4.7 |
  | forced: GWP-ASan on (`ACOS_ART_GWP_ASAN=on`) | 3 (+1) | 0 | **4/4 SIGSEGV** at the chase | — | dies in launch | 2.4–2.5 |
  | after: GWP-ASan off (**new default**) + content-aware first frame | 40 | **40** | **0** | **0/40** | 2.433 s / 2.579 s | 3.3–10.7 |

- **Result 3 — G3c fixed.** `ArtSession::bind_first_frame` (both launch routes: `SessionBackend::launch`
  for verify and `serve.rs::ensure_launch_tree` for `acos launch`/`acos_launch`) re-traverses while
  the tree is still the **window skeleton** (no text/content-desc/hint anywhere and ≤ 40 nodes) for up
  to `ACOS_ART_FIRST_FRAME_MS` (900 ms) / 4 traversals. A retry that does not settle (an animating
  screen) rolls the frame *and* its settle verdict back to the last settled traversal, so the wait can
  never turn a passing launch into a `TIMEOUT`; the launch step's P9 contract ("a fresh frame, not a
  settled one") is byte-for-byte preserved. A frame that never gains content is published with
  `first_frame.placeholder: true` and `frame.meaningful: false` when it is still the bare skeleton
  (≤ 16 nodes), and is logged. Every reply carries `result.first_frame`
  (`traversals`/`waited_ms`/`nodes`/`placeholder`). Measured effect: thin first frames 34 % → **0**,
  at a launch p50 cost of +97 ms (2.336 → 2.433 s) on the ~1 in 3 launches that used to bind the
  skeleton.
- **Scenarios / negatives:** `gaten-home`, `gaten-tabs`, `gaten-settings`, `gaten-search`,
  `gaten-lesson` **3/3 each (15/15)**; `scenarios/negative/gaten-*-wrong.yaml` **5/5
  `ASSERTION_FAILED`** (0 false passes).
- **Gates:** v2-art-counter, w22-counter-reset, w54-dialog-pixels, w42-back-frame, w15-dialog-menu,
  form-validation, suite/T03-render-good, v1-offscreen-pager **pass**; suite/T03-render-wrong
  `ASSERTION_FAILED`; negative/offscreen-pager-visible `ELEMENT_OFFSCREEN`; p11-progress-timeout
  `TIMEOUT`. `cargo test --release --offline` per crate (`acos-art`, `acos-cli`, `acos-verify`) in
  `results/gaten/G3cd.md`.
- **False passes:** 0. The negative controls all fail; an unsettled or placeholder launch frame is
  never counted as app content, and the settle-verdict rollback keeps the launch step's verdict
  exactly as strict as it was (it does not add tolerance — see the checkpoint note in
  `results/gaten/G3cd.md` §"verify launch settle").
- **Decision:** land. GWP-ASan is a *field-sampling* debug allocator: libc enables it for 1 in 128
  non-zygote processes and, when enabled, every sampled allocation unwinds through a function bionic
  itself documents as unsafe; that is not something a deterministic verification sandbox may inherit
  (`ACOS_ART_GWP_ASAN=on` restores it for a debugging run, an explicit `GWP_ASAN_*` value is forwarded
  verbatim, `libc` reproduces the pre-fix lottery). The launch frame now comes from the app's content
  or says it is a placeholder.
- **Re-verification 2026-11-14 (independent repeat on `32baa78`, same source, fresh binaries and logs):**
  `acos-cli` 147/147, `acos-services` 302/302, `acos-verify` 66/66 green; `acos-art` 127 passed +
  1 failed in a single-process run — `sandbox_s5::allocator_bomb_…`, which loses the pid budget to the
  parallel `fork_bomb` test (48 `/bin/sh: fork: retry` lines in the window) and then sees the
  `systemd-run --user --scope` probe fail `Failed to connect to bus`; **each `sandbox_s5` test in its
  own process passes (7/7, rc=0)**, so the failure is the test process's parallelism, not the branch
  (the user systemd bus is up again: `systemctl --user status` → `State: running`). Gates 8 pass +
  `ASSERTION_FAILED` + `ELEMENT_OFFSCREEN` + `TIMEOUT` exactly as above; the 5 Gaten flows 15/15 and
  their 5 negatives `ASSERTION_FAILED`. Launch stability n=30: **30/30 pass, 0 SIGSEGV, 0 one-node
  frame**, p50 2.309 s / p90 2.430 s — but 9/30 bound the window skeleton as a **labelled placeholder**
  (`first_frame.placeholder: true`, `meaningful: false`, `waited_ms` ≈1.63 s, 243 nodes 2 s later)
  versus 0/40 in the after arm above, so the content-aware wait is budget-bound rather than
  unconditional (fail-closed either way, 0 false passes). Details: `results/gaten/G3cd.md` §Checkpoint 3.
  Also found (pre-existing, `cgroup.rs` untouched): `host_scope_capability()`'s per-process `OnceLock`
  makes `refuses_to_run_when_no_cgroup_can_be_created` pass only when it is the first probe in the
  process — a test-isolation defect in the S5 binary, not a product defect.
- **Open risks:** (1) the before arm's single loss is an exit-1 `GUEST_EXITED` before the first frame
  with no kept tree — a *different* class from the GWP-ASan SIGSEGV, unnamed and unmeasured at n>60;
  (2) the fix removes the crash by not using the sampled allocator, not by making bionic's chase
  safe — a guest that deliberately enables GWP-ASan (env) can still hit it; (3) the 900 ms/4-traversal
  budget is a heuristic on the measured skeleton shape (Gaten 10 nodes vs 243), documented and
  observable in `first_frame`; (4) the G3c wait only *adds* a wait where a text-free small tree was
  found — an app whose first screen is text-free and small pays one extra traversal and is labelled a
  placeholder (fail-closed, never a false pass).

## E13.9 — GR4: Gaten past the merged five after the density fix — a RN modal dismissed by Close *and* Android Back, a persisted Settings choice, a confirmation modal whose Cancel changes nothing, and a Back out of a page pushed on the *Analytics* stack — PASS 34/34 runs, 0 false passes

- **Date / commit:** 2026-11-14; worktree `/home/Qui/.paseo/worktrees/3oxa6xao/gr4`, branch
  `gaten-gr4-screens`, off `main` `d7dc8e2` (density fix `00804dc` included). APK read-only
  `gaten-dev-f344272-x86_64-release-unsigned.apk` (sha256 `e04c7ffc…`). Reported in
  `results/gaten/GR4.md`; evidence `results/gaten/evidence/GR4/`.
- **Question / goal:** extend verified Gaten coverage past the five merged flows, with
  deliberately-wrong twins, and record anything ACOS cannot reach or render as a wall (code, lowest
  layer, AOSP reference, bug vs missing feature). No fixes in `crates/`.
- **Inputs / method:** one scenario file per claim plus one negative twin each; batches under a
  single ART-lock acquisition per group (`tools/art-runtime/run-exclusive.sh --task gr4 --
  tools/guarded.sh --mem 8G --time …`), `animations: off`, scratch `.cache/gr4/`. Onboarding taps
  are `settle: fast` **navigation only**, exactly as GR3; every claim after them is a strict
  `wait_for`/`assert_*` on a bound capture frame. Gaten source read only (`src-dev`), no file
  changed, no `adb`.
- **Result (measured, all post-merge):** `gaten-support` **27 steps, 3/3 pass** (8.40 s wall p50),
  `gaten-kanji` **33 steps, 3/3** (11.56 s), `gaten-recognition` **33 steps, 3/3** (9.58 s),
  `gaten-backstack` **25 steps, 4/4** (8.34 s). Negative twins `ASSERTION_FAILED` **3/3 each** at the
  deliberate step (13, 19, 28, 23). The merged five still pass (`home` 14, `tabs` 22, `settings`
  33, `search` 22, `lesson` 28 steps) and their five twins still fail `ASSERTION_FAILED` (5/5).
  **26 accepted runs, 0 false passes** (the six superseded `gaten-kanji` v1 runs are excluded), every
  session `outcome=ok`/`guest_exit=0`, guest cgroup peak 236–429 MiB. R2a/G3d did not recur
  (0/40 launches since `d7dc8e2`).
- **What each new claim proves:** a RN `Modal` (`DialogShell`) is dismissed by the **Close** action
  **and by Android Back** (`onRequestClose` — the handoff's open risk 2 is answered: works, not a
  wall); a second Settings page's radio choice really persists through the RTK bridge — the hub
  row's own label flips `Kanji reference. Remembering the Kanji` → `Kanji reference. WaniKani` after
  choosing WaniKani and Back, then back again after choosing RTK and Back, with the opposite label
  asserted absent at each hop and the pixels agreeing (4604 / 4604 changed, 0 px on the return);
  a `fitToViewport` download confirmation's **Cancel** leaves both the page and the persisted value at `Standard`; and a Settings page pushed
  on the **Analytics** tab's own stack returns to Analytics on Back (strictly `Study activity`,
  `Accuracy`, “Answer accuracy will appear here.”, with the Settings hub row absent).
- **Density fix, judged on post-merge pixels only:** the 2.625× SVG regression is gone. Same screen,
  same APK: `evidence/GR3/gaten-tabs-analytics.png` (pre-fix) draws the wordmark and illustrations
  ~2.6× too large and clipped; `evidence/GR4/gaten-backstack-analytics-again.png` (post-fix) fits
  every element in its box. Measured post-fix: every wordmark glyph `SvgView` is exactly 116 px tall
  (`[58,188][105,304]`, whole wordmark 333×116) against GR1's expected 116 px viewBox height (a
  2.625× double-scale would be ~305 px); hub icons 58×57, chevrons 39×39, sheet icons 52×53, bottom
  bar icons 52×53, nothing overflowing its row. No exact-parity claim against the Moto G35.
- **A green draft that proved nothing, caught and rewritten:** `gaten-kanji` v1 was 3/3 pre-merge
  while asserting that the hub row read `Remembering the Kanji` *after* tapping RTK. But
  `KanjiReferencePreference.defaultFor(wanikaniConfigured = false) = RTK` (this guest has no account,
  so the default already was RTK) and `KanjiReferenceSettingsScreen.tsx::select()` returns early when
  the tapped option is already the source, so the tap wrote nothing. The tell was pixel-level: the
  before/after hub captures and the before/after page captures were **byte-identical** (0 changed
  pixels). v2 moves the value off the default and back; Backlog `G4c` files the harness gap (nothing
  notices a no-op tap; comparing the bound captures does).
- **Walls / defects found:** **no new blocking ACOS wall**; GR3's data walls (AnkiDroid absent, no
  WaniKani token) still stand and were not fabricated. Two observations, filed as Backlog rows and
  not fixed here: `G4a` — the tree reports `visible=false` for the bottom bar's active label whose
  glyphs the app **does** paint in the same bound frame (carve mode's 128 px bottom band; a
  verification false-negative, never a false pass; the rule for authors is “target the tab
  pressable's `content_desc`, never the label”); `G4b` — tree frame metadata says `density: 420` for
  a run whose `repro.json` says `density_dpi: 400`.
- **Scenario correction (selector, not semantics):** `gaten-backstack`'s draft last step asserted
  `text exact "Analytics"`, which is only ever the bottom-bar label; the strict run failed
  `ELEMENT_OFFSCREEN` on it while the Back itself passed. Re-targeted to Analytics-only copy plus
  `assert_not_visible` on the Settings hub row; the wrong twin now asserts the visible heading
  (`content_desc "Analytics"`) reads `Settings`. No assertion, visibility rule or settle horizon was
  weakened, and `run_suite.sh`'s step-count bug (`d["verdict"]` vs `result.verdict.steps`, `ok` vs
  `status`) was fixed in scratch so the reported step counts are real.
- **Conclusion / next:** Gaten's post-Home surface now includes modals, cross-page persistence and a
  non-Settings tab stack, all strictly asserted on real ART. Remaining: `G4a`/`G4b` classification,
  the owner decision on animating-screen semantics (G3e), R2a/R2b, and a real lesson (needs
  AnkiDroid or a WaniKani account — data, not ACOS).

## E2.14 — H15: M6 reset profiled — 117 ms of the strict bind is a framework animator run, and the 25 % target is not reachable without a strict-rule change — MEASURED (partial: −3.9 % fast, −0.6 % strict)

- **Experiment ID / name:** H15 — cut M6 reset (`strict` 333–342 ms / `fast` 210 ms p50); reset is 24 % of the agent loop with animations on and ~48 % with them off.
- **Responsible agent:** H15 (worktree `3oxa6xao/h15`, branch `perf-h15-reset`, off `main` `1ce336b`).
- **Bottleneck investigated:** where a reset's 341 ms actually goes, phase by phase, with the pre-existing `DM6_RESET_TRACE` (engine split), `ACOS_ART_INPUT_TRACE` (launcher stages + the guest's own `ACOSLAUNCH MARK` chain with host receipt times) and two new trace-only instruments (the guest's `in_touch_mode` probe field; `P10_PROBE_ANIM_TRACE=1` names the ANIMATION-queue callbacks via `AnimationHandler`'s callback list).
- **Baseline (p50/p95, main `1ce336b`, low load):** M6 strict **343.1 / 358.6 ms**, M6 fast **211.1 / 226.4 ms** (3 reps × 8 resets, load1 0.71–2.43); under load1 2.9–7.7 the same strict arm measured 344–392 ms. Loop: N=1 1437.6 ms/task, N=5 1441.0; animations off 490.8 / 518.3; verified rate 1.000, 0 false passes.
- **Hypothesis + proposed solution:** the reset is dominated by per-phase fixed costs that can be overlapped or skipped (D-M6 had blamed a 150 ms quiet window, a 60 ms PNG encode, a 200 ms guest sleep and a 50 ms stop poll — all of which have since been removed, so the split had to be re-measured). Candidate wins: overlap the old guest's teardown with the new launcher's setup; stop re-probing per-launch host constants; tighten the strict bind.
- **Result — the split (strict, animations on, rep-median):** stop **13 ms**; relaunch **185–188 ms** = bwrap+cgroup+`execve`+ART boot to first Java line **77**, `AndroidRuntime` **3**, overlays/binder-looper/attach **9**, `Application.onCreate` **46**, `LAUNCH_ACTIVITY`+first traversal/frame **18**, live probe+first sample **15**, ready line **~18**; strict frame bind **142 ms** = a framework animator run that keeps drawing **117 ms** (7 × 16.7 ms frames) + the 24 ms confirmation window (+30–41 ms including sample quantisation). `13 + 186 + 142 = 341`, observed 341 — nothing unaccounted.
- **Finding 1 — the 117 ms is a real animator run, and it draws nothing.** `AnimationHandler$1` pumps `RippleAnimationSession` ValueAnimators (80 ms enter, 450/7000 ms exit) and a `Button` `StateListAnimator` on every fresh Activity in touch mode (`in_touch_mode true`, no input). Each frame increments the guest's `draw_seq`, so the strict rule ("the same draw stayed newest for 24 ms") waits the run out. Four PNGs captured inside the window are **byte-identical** to the frame 1 s later (`sha 629611df…`, 9 545 882 bytes) — the picture never changes. With `animations: off` the same bind is **50 ms**. So the strict rule as written measures redraw cadence, not visible change.
- **Finding 2 — the 186 ms boot is cold-boot, not overhead.** 77 ms to the first Java line (systemd scope + bwrap + ART runtime init + boot-image map), 46 ms `BIND_APPLICATION`, 18 ms first traversal. A zygote/warm pool is the only lever and was declined by the owner (H3); the H13 shared image changes it by ≤ 10 ms (203 → 190 ms) for +3.6 s on a serve's first launch.
- **Finding 3 — 7 ms/launch was pure waste.** `RunCgroup::prepare` ran two `systemd-run --user --scope … true` capability probes on *every* guest start (4.2–4.9 + 2.2–2.8 ms measured) for per-boot constants.
- **Implementation + commits:** (a) **shipped**: `cgroup.rs` caches that capability (in-process `OnceLock` + a `<ACOS_ART_DATA>/cgroup-capability.json` stamp keyed by the kernel boot id, `publish_atomic`, boot-id/garbage/missing-key unit test; a stale positive still cannot create an unbounded run because `confirm()` reads back `memory.max`/`memory.swap.max`/`pids.max` before the guest continues), `launch.rs` points the cache at the resolved `ArtRoot`; (b) **diagnostics kept, inert by default**: the guest's `in_touch_mode` field and `P10_PROBE_ANIM_TRACE` animator naming (`LaunchMain.java`, `launch.rs` trace-once plumbing). Nothing else changed: no overlap handshake, no strict-rule change, no zygote.
- **Benchmark method + environment + host load:** real ART, release, AMD Ryzen 9 9950X (32 threads, 61 GiB), `fixtures/build/apps/counter.apk`, one `run-exclusive.sh --task H15` + `guarded.sh --mem 8G` acquisition per arm (all < 10 min), interleaved base/fixed arms in the same window; M6 = 3 reps × 8 resets per arm with `DM6_RESET_TRACE` + `ACOS_ART_INPUT_TRACE` on; loop = `tools/bench/acos-bench.sh --phases loop1,loop5`, 3 reps, 0 false passes. Load1 per arm recorded (0.71–2.43 for the headline window; 2.78–7.72 for the cross-check). `MemAvailable` ≥ 45 GiB throughout.
- **Before/after (M6, animations on, 24 resets/arm):** strict **343.1 / 358.6 → 341.1 / 347.8** (−0.6 % p50, p95 −3.0 %, inside rep noise); fast **211.1 / 226.4 → 202.9 / 213.0** (−3.9 % p50, −5.9 % p95) in the low-load window — but the fast effect is **mixed across four windows** (−8.6, −8.2, −15.9, −0.4 ms; pooled 96 resets/arm: **211.1 → 208.2 p50, p95 231.0 → 213.3**, −1.4 % p50), so the honest claim is the *mechanism* (two `systemd-run` probes ≈ 7 ms removed per launch; standalone launcher 166 → 151 ms), not a flat percentage. `acos` (engine) is byte-identical between arms (`fc53a8bc…`); only `acos-art` differs, which is exactly the change. Animations off: strict 238.7 → 224.7 (−5.9 %), fast 181.6 → 181.2 (tie). Loop: on N=1 **1437.6 → 1421.9** (−1.1 %), on N=5 **1441.0 → 1424.6** (−1.1 %), off N=1 490.8 → 501.7 (+2.2 %, inside the ±5 % threshold and inside the rep ranges), off N=5 518.3 → 516.9 (−0.3 %); `verified_rate` **1.000** in all 8 cohorts, 0 false passes.
- **Why it worked / why it cannot work further:** the shipped change removes a constant 7 ms from a `fast` reset that is 100 % launch, so it shows there (−8.2 ms); the strict reset is 42 % frame bind, so 7 ms is inside its noise. The remaining 25 % target (85 ms) lives in the 117 ms animator block (§Finding 1) — it is only removable by changing the documented strict rule (content-stable confirmation instead of draw-counter stability; prize −117 ms = −34 % strict M6 and −8 %/−22 % loop, cost = an async content hash because `capture_composite` is 10–30 ms) or by letting the *agent loop's* reset step use `settle: fast` (harness change, −142 ms/task, the next strict step is what backs the verdict). Both are owner decisions, not silent edits. Zygote/warm pool would cut the 186 ms boot and is declined.
- **Trade-offs / regressions:** none measured. `strict` p95 improved slightly; the animations-off N=1 loop +2.2 % is noise (overlapping rep ranges, and no code on that path does more work). One one-off `GUEST_EXITED` on `negative/offscreen-pager-wait` (`LOOPER_THREW: Adding window failed`) did not reproduce in 3 further fixed runs or 3 base runs — a fail-closed, named code, same family as the open R2a crash class.
- **Gates:** v2-art-counter, w22-counter-reset, w54-dialog-pixels, w42-back-frame, w15-dialog-menu, form-validation, suite/T03-render-good, v1-offscreen-pager, p11-background, p11-delay1000, p12-progress-anim-off **pass**; suite/T03-render-wrong `ASSERTION_FAILED`; negative/offscreen-pager-visible `ELEMENT_OFFSCREEN`; negative/offscreen-pager-wait and p11-progress-timeout `TIMEOUT`. `cargo test --release --offline -p acos-cli -p acos-verify -p acos-art` green (343 tests; new cgroup test included).
- **Fidelity / evidence:** `art-reference` (real ART, release), MEASURED; `results/bench/H15.md` (tables, budget, options, risks) and `.cache/h15/` (per-arm `profile.json`/`dm6.jsonl`/`trace.log`, the four probe scripts, the 8 H6 loop artifacts, gate JSONs, both binaries).

## E2.16 — H4: an idle session now wakes only on events — 98 % fewer wakeups and 72 % less idle CPU — MEASURED (accepted)

- **Experiment ID / name:** H4 — per-session periodic timers/wakeups on the idle path (B2 measured 161.7 wakeups/s and 5.28 % of one core at N=10, 0.53 %/session).
- **Responsible agent:** H4 (worktree `3oxa6xao/h4`, branch `perf-h4-idle-cpu`, off `main` `11b917c` + T1 merge `4737bb7`).
- **Bottleneck investigated:** S3/B2 attribution of the idle wakeup budget per session: `acos` serve 50 ms `poll` (20 s⁻¹), `acos-art` launcher ~77 s⁻¹ (7 × ~20 s⁻¹ timers + 2 × 9.6 s⁻¹), guest binder loopers 2 × 50 s⁻¹ (20 ms `pthread_cond_timedwait`), the 250 ms probe fallback (8 s⁻¹), guest main probe samples 4 s⁻¹.
- **Hypothesis + proposed solution:** every one of those is a *polled* timeout whose condition is also signalled (socket readability, child exit, transaction broadcast, view-dirty probe events), so the timeouts are only liveness bounds and can be replaced by event waits with a long bound. Replace them: event-driven serve/launch idle loops, untimed binder-looper `pthread_cond_wait`, probe fallback 250 ms → 5 s, serve `poll` 50 ms → ≤ 1 s.
- **Implementation + commits:** `6c3b01a` (`LaunchMain.java` probe fallback 5 s; `launch.rs` `ServeEvent` loop + `waitid(WNOWAIT)` child watch + 1 s peer timeout; `logd.rs` 1 s; `binder_driver.c` untimed looper wait; `serve.rs` ≤ 1 s idle poll), plus the **required** `launch.rs` fix in the final commit: `wait_for_event_settled` never advanced `last_requested_seq` when a probe sample arrived, so the host stopped asking and the 5 s fallback turned that into a deterministic `w42-back-frame` strict `TIMEOUT`. The fix records the arriving sequence and resets `last_wake`, keeping G3b's 50 ms probe floor. `watchdog.rs` names the RSS sampler thread (attribution only).
- **Benchmark method + environment + host load:** real ART, release, AMD Ryzen 9 9950X (32 threads, 61 908 MiB), `tools/bench/acos-bench.sh --groups "a:memory,latency:16G:620"`, **interleaved base/fixed arms** (base rep1, fixed rep1, base rep2, fixed rep2, base rep3, fixed rep3), 3 reps × N=1/5/10 over a 60 s idle window (`memory` phase) plus latency (3 reps × 8 samples) and an N=1 agent-loop arm. Every acquisition under `run-exclusive.sh --task H4` + `guarded.sh`. load1: base 0.80/0.68/1.11, fixed 0.78/0.74/0.69 (memory phases; loop 1.07–2.48); `MemAvailable` 46.5–47.1 GiB; no rep flagged noisy. Base binaries `acos 7fb5e560…` / `acos-art 568658342b…`; fixed `acos 740d8e54…` / `acos-art 71c1f10f…` (the same source as `df1d6088…`, which is the same release build under a different cargo feature unification; the whole fixed arm ran on one binary and `w42-back-frame` proves it carries the probe fix).
- **Before/after (idle, p50 of 3 interleaved reps; per-rep p50s in `results/bench/H4.md`):** wakeups/s **34.06 / 90.22 / 160.52 → 1.21 / 2.06 / 3.08** at N=1/5/10 (**−96.4 / −97.7 / −98.1 %**); idle CPU **0.614 / 2.543 / 4.915 % → 0.166 / 0.743 / 1.394 %** of one core (**−73.0 / −70.8 / −71.6 %**), i.e. **0.491 → 0.139 %** per session at N=10 (−71.7 %). Every row is reproduced in all three reps with disjoint rep ranges. Memory is unchanged (M1 66.49 → 65.82 MiB, −1.0 %; M2 N=10 66.27 → 64.83 MiB, −2.2 %; both inside noise).
- **Latency A/B (same interleaved window, 3 reps × 8 samples, animations on):** M4 strict 309.29 → 309.25 (−0.0 %), M4 fast 174.06 → 159.33 (−8.5 %), M5 24.69 → 24.40 (−1.2 %), M6 strict 322.81 → 325.99 (**+1.0 %**), M6 fast 190.83 → 189.90 (−0.5 %), M7 strict 41.12 → 41.20 (+0.2 %), M8 counter strict 952.90 → 950.53 (−0.2 %), M8 form strict 944.65 → 945.76 (+0.1 %), M8 fast +0.3…+0.6 %, tap ack 5 ms (tie), teardown 11 ms (tie). Largest regression **+1.0 %**, inside the ±5 % threshold and inside the overlapping rep ranges. Agent loop N=1: **1422.02 → 1406.18 ms/task (−1.1 %)**, 0.661 → 0.669 tasks/s, page-cache 278.95 → 284.61 tasks/GiB-min (+2.0 %), anon+shmem 910.77 → 890.91 (−2.2 %, noise), **verified rate 1.000 in all 6 cohorts (21/21 tasks each), 0 false passes**.
- **Gates + tests (final tree):** 15/15 expected gate outcomes (`v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`, `w42-back-frame`, `w15-dialog-menu`, `form-validation`, `suite/T03-render-good`, `v1-offscreen-pager`, `p11-background`, `p11-delay1000`, `p12-progress-anim-off` pass; `suite/T03-render-wrong` `ASSERTION_FAILED`; `negative/offscreen-pager-visible` `ELEMENT_OFFSCREEN`; `negative/offscreen-pager-wait` and `p11-progress-timeout` `TIMEOUT`), 0 false passes; `cargo test --release --offline -p acos-art` 149/0, `-p acos-cli` 146/0 (5 ignored), `-p acos-verify` 66/0 — 361 passed, 0 failed.
- **Why it worked / what is left:** each removed timer polled a condition that is also signalled, so the change is a pure polling→event conversion with a long liveness bound. The remaining per-session idle wakeups are the 50 ms memory watchdog (~40 s⁻¹; two threads that disappear under measurement-only `ACOS_ART_RSS_MAX=0`) and stock ART daemons (~1 s⁻¹); the watchdog period bounds launcher RSS overshoot and has no event source, so it is left alone.
- **Trade-offs / regressions:** none measured. Risks: the 5 s probe fallback is only safe together with the host `last_requested_seq` fix (`w42-back-frame` is the canary — it fails deterministically without it), and the binder-looper untimed wait is correct by construction (broadcast under `g.mu`) but not separately gate-proven (reverting only that file reproduces the same w42 timeout).
- **Fidelity / evidence:** `art-reference` (real ART, release), MEASURED; `results/bench/H4.md` (method, tables, attribution, risks), `results/handoff/H4.md` (pre-fix state), `.cache/h4/results/{ab,loop}/` merged JSON/MD + raw per-rep phase files, `.cache/h4/logs/` gate and test transcripts.

## E2.17 — ZPROD1: a persistent zygote and a reset that forks **one** child — MEASURED (mechanism; throughput pending P2)

- **Date / commit:** 2026-11-14; branch `zygote-prod-1` (off `main` `863def1`).
- **Question / goal:** Z7 measured the zygote arms at 30.0/30.1 tasks/min vs the exec arms'
  39.0/39.1 and named the cause: the prototype's reset restarted the **whole cohort**, re-paying the
  zygote's pre-fork core every round; Z7 §7 item 2 explicitly left "a product zygote that forks a
  new child per reset" unmeasured. Does a persistent zygote whose reset kills **one** session's
  child and forks one fresh child actually work, and does it leave every sibling untouched?
- **Method:** guest-side ART zygote (Z3/Z4 lineage) with a command FIFO (`spawn <tag>` /
  `kill <tag>` / `exit`) instead of a fixed cohort; each session is its own process group
  (`setpgid0`), so the zygote ends exactly that session; the driver then wipes that session's run
  root, starts a fresh foreign launcher (new peer + new contract) and asks the zygote for one new
  child. N=2 `counter.apk`, real ART, one release binary; both sessions verified `0` → tap → `1`
  before the reset, then one session reset and both re-verified.
- **Inputs:** `fixtures/build/apps/counter.apk`; zygote build `research/zygote-proto/z3/build.sh`
  (`ZBUILD=zprod1-build`); API-34 sysroot; x86_64 host; guard `--mem 16G --time 300`.
- **Result:** zygote pre-fork core **13 ms** once. Reset of tag 0: kill **20.2 ms** (`status=0x9`,
  SIGKILL on pid 8), fresh launcher **70.6 ms**, new child pid **62**, total **390.1 ms**. Session 0
  after the reset: counter **`0`** and first frame md5 **`39d1e4e3…`**, byte-identical to a
  never-tapped session's first frame (the old child's tapped frame was `9f3406c1…`). Sibling
  session 1: child pid **35 → 35**, counter **`1` → `1`**, launcher never restarted. Zygote pid
  **2 → 2** and boot epoch unchanged. Verdict PASS, 0 failures, 7.76 s end to end.
- **Finding (proven vs hypothesis):** proven — a reset can be scoped to one child of a persistent
  zygote, with a provable fresh state and siblings untouched. Not measured (no claim): any
  tasks/min or tasks/GiB-min number for this reset; the Z7 loop was not re-run, and the keeper is
  not yet owned by `acos serve`, so the flag-on gate/Gaten runs are not possible yet.
- **Fidelity / evidence:** MEASURED on real ART, `real-ART` fidelity.
  `results/art-runtime/ZPROD1.md`, `results/art-runtime/ZPROD1/{reset-n2.json,zygote.log,
  session-0.log,session-1.log}`; design `docs/decisions/2026-11-14-zygote-prod.md`; driver
  `research/zygote-proto/z4/reset.py`.
- **Regression / false pass:** every check is a fresh read from the new/old child's own `tree` and
  `screenshot` reply, never an ack; the freshness assertion compares PNG bytes with a brand-new
  session's first frame. 0 false passes. Two real defects found and fixed on the way: a zygote host
  that inherited the guest Landlock guard could not write its own contract dir (`EACCES`), and a
  silent `writeText` hid it.
- **Conclusion / next:** keep the mechanism; **P2** wires the keeper into `acos serve`
  (`ACOS_ART_ZYGOTE=on`), adds the per-child cgroup memory limit (F3) and then runs the Z7 loop for
  the ≥25 % gate. Exact commands and acceptance: `results/handoff/ZPROD1.md`.

## E2.18 — ZPROD2: the persistent zygote's host half (`PersistentZygote`) — MEASURED (unit/gateway level); engine wiring NOT done

- **Date / commit:** 2026-11-14; branch `zygote-prod-1` (merge of `main` `0c0b322`, then the step-1
  commit). Design `docs/decisions/2026-11-14-zygote-prod.md`. (E2.17 was claimed by ZPROD1's
  mechanism section in this file; E2.18 is used here to leave E2.17 free for S3PERF on the other
  branch.)
- **Question / goal:** P2 of `results/handoff/ZPROD1.md`: make the persistent zygote a *library* the
  engine can own — keeper launcher, plan dir, command FIFO, `spawn`/`kill`/`boot_epoch`/`reset`
  with file-based acks, every failure a structured `ZYGOTE_DEAD` — before any product path depends
  on it. (Step 2 of the same task, wiring it into `acos serve`, was not reached.)
- **Method:** a Rust port of `research/zygote-proto/z4/reset.py` steps 1–4 into
  `crates/acos-art/src/zygote_prod.rs`, tested two ways: (a) pure functions/paths (key naming, Z4
  contract parsing, run-root wipe, `binds`), (b) a **mock keeper** — a `sh` script that speaks the
  exact file protocol (`<plan>/ready`, `<plan>/<tag>/{spawned,reaped,pid}`, `cmd` FIFO) with no ART,
  so the host half is proven deterministically at 0.1 s instead of 8 s per ART boot.
- **Result:** 7/7 new tests green; `cargo test -p acos-art` 107 lib + all integration targets green
  (0 failed, the cgroup-conditional test green when run alone with `--exact`); `-p acos-cli` 147
  passed / 0 failed (that crate was not modified, so this is the flag-off baseline). Proven in the
  mock-keeper test: `spawn` → child pid 101; `reset` → `previous 101`, fresh child **102**, the old
  run root wiped, **boot epoch and zygote pid unchanged**; `shutdown` leaves no keeper process.
  Proven for failures: a dead keeper and a missing ack both surface as `ZYGOTE_DEAD: …` carrying the
  exit status and the last 20 log lines, and no write is attempted — there is no silent fallback to
  the exec path in the module.
- **Finding (proven vs hypothesis):** proven — the Z4 driver's steps 1–4 are a self-contained Rust
  component with file-acked verbs and a leak-free lifetime. Not measured (no claim): anything on
  real ART through this component, any gate or Gaten run, any latency/throughput number. The host
  ART lock was held by other agents' runs (`--exclusive` latency bench, `G3F1`, `K5C-T2`) for the
  whole window in which the gate list would have run, so no gate JSON exists for this tree.
- **Fidelity / evidence:** `real-ART` not exercised in this increment (mock keeper only);
  `results/handoff/ZPROD2.md` has the exact commands, the test table and the remaining task list.
- **Regression / false pass:** none; the only code added to the product path is a new module and one
  `acos_art::Error::Zygote` variant — no engine line changed, so the flag-off path is untouched by
  construction (and `-p acos-cli` 147/0 confirms it). 0 false passes; nothing is claimed at gate
  level.
- **Conclusion / next:** step 2 (engine wiring: `ZygoteHost`, `ArtSession` zygote branch,
  reset/status reply fields, `ErrorCode::ZygoteDead`, `shutdown_all`) is the immediate next task,
  then the flag-off gate list and the flag-on counter/reset/Gaten checks from the task brief.

## E13.10 — SE1: an opt-in per-step `expect_change` closes GR4's vacuous-flow gap (a no-op tap can no longer pass) — PASS (ADR-0026 decision 2 shipped; 0 false passes)

**Date:** 2026-11-14. **Commit:** the SE1 commit on branch `settle-se1-expect-change`
(base `main` `3e11d9c`). **Responsible agent:** SE1 (worktree `3oxa6xao/gr4`,
`.cache/gr4-target`, one ART-lock acquisition per batch).

- **Question / goal:** ADR-0026 decision 2: keep the P12 rule (a step whose
  action drew but changed no pixel still settles and passes, and the reply keeps
  reporting `settle.pixels_changed`) **and** add an opt-in per-step
  `expect_change: true` that fails the step when the settled frame's pixels did
  not change. That is GR4 Backlog `G4c`: a tap on a control that is already in
  the asserted state writes nothing, and every later assertion then holds on
  state the tap never produced (`gaten-kanji` v1 was 3/3 while its RTK tap was a
  no-op; only a hand-run byte-diff of the captures noticed).
- **Design (as shipped):**
  * the flag lives on the input step bodies that carry a strict settle record —
    `tap`, `type`, `key`, and `back` (the protocol's `EmptyStep` becomes
    `BackStep { expect_change }`, so `- back: {}` still parses) — and defaults to
    `false`, skipped on the wire, so the format and every existing scenario are
    unchanged;
  * after the strict settle, `settle.pixels_changed: true` passes;
    `false` fails the step with **`ASSERTION_FAILED`**,
    `details.reason="expect_change"`, `pixels_changed:false`, `step_kind`, and
    `details.before`/`details.after` frame refs (`frame_seq` + `frames/<n>.png`
    when that frame was captured). No new `ErrorCode`: the codebase's convention
    is "one code per outcome class, `details` for the situation"
    (`ELEMENT_OFFSCREEN`, `INSUFFICIENT_HOST_EVIDENCE` do the same), and an
    `expect_change` miss *is* an assertion that ran and did not hold;
  * a reply with **no** measured `pixels_changed` is `UNSUPPORTED_ASSERTION`
    (the engine cannot supply the evidence) — an unmeasured change never passes;
  * `expect_change: true` + `settle: fast` is rejected at scenario-validation
    time (`INVALID_SCENARIO` at `steps[i].<kind>.expect_change`, for tap/type/key)
    because a fast frame may be mid-animation and never backs a pass (ADR-0026
    decision 5). A non-bool is a parse error;
  * the same flag is on the serve/MCP input tools (`acos_tap`, `acos_key`,
    `acos_text`) with the same fast rejection before any input is injected and
    the same measured-fact rule.
- **Tests (green):** `cargo test -p acos-protocol` (3 new: bool+default, non-bool
  rejected for `yes`/`1`/`"true"`/`[true]`, fast rejected for tap/type/key with
  the stable path); `cargo test -p acos-verify` (4 new end-to-end on the
  vertical-slice backend: changed→pass with `settle.pixels_changed:true`,
  unchanged→`ASSERTION_FAILED` with the full details and frame refs, flag absent
  →the P12 pass with `pixels_changed:false`, no settle record→
  `UNSUPPORTED_ASSERTION`). `acos-cli`, `acos-services` and `acos-art` green (the
  `sandbox_s5` order flake run alone with `--exact`, as documented).
- **Gates (measured, one lock acquisition per batch, `--engine art`, animations
  on):** new `scenarios/se1-expect-change-tap.yaml` **pass** (5 steps, counter
  tap really moves 0→1) and `scenarios/negative/se1-expect-change-noop.yaml`
  **fail `ASSERTION_FAILED`** at its tap (3 steps) — the flipped flag flips the
  verdict, no other step involved. Existing gates unchanged:
  `v2-art-counter` 11/11, `w22-counter-reset` 9/9, `w54-dialog-pixels` 14/14,
  `w42-back-frame` 17/17, `w15-dialog-menu` 3/3, `form-validation` 28/28,
  `T03-render-good` 5/5, `v1-offscreen-pager` 9/9 pass; `T03-render-wrong`
  `ASSERTION_FAILED`, `negative/offscreen-pager-visible` `ELEMENT_OFFSCREEN`,
  `p11-progress-timeout` `TIMEOUT`. Gaten: `home`/`tabs`/`settings`/`search`/
  `lesson` pass (14/22/33/22/28 steps) and the five wrong twins fail
  `ASSERTION_FAILED` (at 7/7/12/16/10). **All 21 runs are step-for-step identical
  to the main-side run** in `.cache/wt-int/.cache/int/suite` (status, verdict,
  failure code, and every step's status/code compared programmatically) —
  0 false passes, 0 changed codes.
- **Negative-control note (honest, measured):** a tap on the counter's *blank*
  content area with `expect_change: true` does **not** fail with
  `ASSERTION_FAILED` — it never draws, so the strict settle times out
  (`TIMEOUT`, `reason: no-draw-after-action`). The gate twin therefore uses the
  measured pixel-identical-but-drawing case (focus the form's empty `Name`
  field: `settled:true, bound:strict-event, pixels_changed:false`, exactly the
  tap P12 recorded). Both are non-passes; the SE1 rule is the subtler one.
- **Conclusion / next:** `G4c` is addressed at the harness level: a flow that
  needs a state change can now say so per step, and the scenario that quietly
  proved nothing fails. Scenario authors (GR4/GR3) can add
  `expect_change: true` to the mutating tap of any claim they care about; no
  assertion, settle horizon or gate was weakened, and P12's no-change pass is
  still the default.

## E13.11 — W7b: `report` insets default, the red w7 gate, and the w42 one-row re-bless — PASS (gates, Gaten, crate tests; 0 false passes)

- **Date / branch:** 2026-11-14; worktree `/home/Qui/.paseo/worktrees/3oxa6xao/g3cd`,
  branch `fix-w7b-insets` (off `main` `3e11d9c`). Real ART guest; all runs under
  `tools/art-runtime/run-exclusive.sh --task W7B` + `guarded.sh`; scratch `.cache/w7b`.
- **Question / goal:** `scenarios/w7-insets-report.yaml` was red on main at
  `status=128` (got `status=0`). Bisect the delta from 5cae694 (density fix
  `0e569b5`, B4, S3c) to the exact hunk and name the root cause; then ADR-0026
  decision 6: default the app-window inset model to `report`, keep `carve`
  selectable, re-bless w42's one AA row with a pixel-diff proof.
- **Bisect (targeted A/B on the same binary):**
  - default (`carve`): probe prints `status=0`, decor `[0,128][1080,2337]` → the
    gate failure (`.cache/w7b/gates/w7-insets-report.json`).
  - `ACOS_WM_INSETS=report`: probe passes, `status=128`, decor `[0,0][1080,2400]`.
  - `ACOS_WM_INSETS=report` + `ACOSLAUNCH_APP_FLAGS=0x10808044` (the exact
    pre-`0e569b5` ApplicationInfo flag set, i.e. no
    `FLAG_SUPPORTS_SCREEN_DENSITIES`): **also passes**. The density hunk does
    not change the insets read; B4/S3c are bench-only by file scope. **Root
    cause: the scenario required `report` but nothing selected it, and the build
    default was `carve`.** Under AOSP `WindowLayout.computeFrames` +
    `InsetsState.calculateInsets`, a carved window frame excludes the bar
    sources, so the calculation is zero; the density flag only feeds
    `CompatibilityInfo` scaling, and ACOS's guest always binds
    `DEFAULT_COMPATIBILITY_INFO` (no translator), so it cannot move the rows.
    AOSP refs: `PackageParser.java:2500`, `CompatibilityInfo.java:295`,
    `ActivityThread.java:7218`, `ViewRootImpl.java:1407/2098`.
- **Change:** `DEFAULT_INSETS_MODE = Report` (and the enum default). New
  validated scenario field `insets: carve|report` threaded through `acos verify`
  (precedence CLI > scenario > `ACOS_WM_INSETS` > default; the resolved value is
  installed in the env before launch and published in the verdict). The two W7
  scenarios pin their model; the negative also asserts the carved frame first,
  then fails the report contract at `status=128` (without the pin it would
  become a false pass under the new default). `carve` remains selectable via
  env, `--insets`, and the scenario field. Docs updated (`AGENT_API` §8.6.6,
  `SECURITY` env table, `MORNING`, `TRACKS`, `results/render/W7.md` addendum).
- **w42 re-bless:** `w42-back-frame` pinned to `insets: report`; only the final
  first-Activity region-1 hash changed
  (`dee05db8…` → `5edac422…`). Pixel diff (`.cache/w7b/art-w42-*/frames/14.png`,
  `results/handoff/W7B-pixel-diff.txt`): exactly **one** differing row in the
  overlapping display area, display y=411 (carve PNG y=283), 122 pixels,
  x=58..1026, only `G -1` (carve 211 → report 210). Every other pixel identical;
  the second-activity regions and final region 2 are unchanged. No tolerance.
- **Result (gates, final binary):** all pass under `report`:
  `v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`, `w42-back-frame`,
  `w15-dialog-menu`, `form-validation`, `suite/T03-render-good`,
  `v1-offscreen-pager`, `w7-insets-report`; `w7-insets-carve-negative` fails
  `ASSERTION_FAILED` at its status assertion with `insets=carve`;
  `suite/T03-render-wrong` `ASSERTION_FAILED`; `negative/offscreen-pager-visible`
  `ELEMENT_OFFSCREEN`; `p11-progress-timeout` `TIMEOUT`. Gaten: home/tabs/
  settings/search/lesson pass (14/22/33/22/28 steps), the five negatives fail
  `ASSERTION_FAILED`. Evidence `.cache/w7b/gates/`, `.cache/w7b/gaten/`,
  `results/handoff/W7B.md`.
- **Tests:** `cargo test --release --offline -p acos-apk -p acos-cli
  -p acos-verify -p acos-services` green (the services carve wire-golden tests
  now pin `ServiceHost::standard_with_insets(Carve)`; DisplayInfo is built with
  the same explicit mode, so the display service can no longer disagree with the
  WMS). `acos-art`: skip the order-dependent cgroup test with
  `--test-threads=1 --skip refuses_to_run_when_no_cgroup_can_be_created` (green),
  then that test alone in a **fresh** lock acquisition (fresh `ACOS_ART_DATA`,
  green). Same-acquisition rerun reuses the cached `scope_ok=true` and fails.
- **Regression / false pass:** none; 0 false passes. The negative that would
  have false-passed without a model pin is now pinned. `report` changes the
  frame metadata for all ART gates (display-sized PNG, origin `[0,0]`) as W7
  measured; only w42 had a model-specific hash.
- **Not fixed:** GR4 Backlog G4a (visibility vs carved bottom bar) — `report`
  likely removes its carve-specific trigger for Activities; GR4 must re-measure.
  Any baseline that wants the old pixels must pin `insets: carve`.
- **Incident:** an external cleanup at ~14:06 removed `.cache/` from this and
  three sibling worktrees (~92 GB host-wide), including the required symlinks
  and the prebuilt target; they were recreated and the release rebuilt. Other
  tracks' `.cache` scratch evidence is gone.

## Leaderboard (best measured configuration per metric; update on every accepted experiment)

Central objective: verified agent tasks per second per GiB (tasks/GiB-min).

**Rows are current main `4958703` restated by B2 (E3.2): 3 repetitions of
`tools/bench/acos-bench.sh`, release, animations **on** (the product default),
`ACOS_STRICT_SETTLE` at the tree default (event-only, P12), early GC on (H2).
The pre-H2 rows they replace are in `results/bench/baseline-pre-h2.json` and
§3 of `results/bench/B2.md`.** Memory rows state their type: **cgroup
anon+shmem** (job scope + guest scopes, sampler charged back) and **ΣPss** are
never mixed. `tasks/GiB-min` is quoted in both denominators where they differ:
**page cache included** (cgroup `memory.current`, the bench's historical
headline) and **anon+shmem** (label-matched with M1/M2).

| Metric | Best value | Config / commit | Experiment | Date | Δ / why |
|---|---|---|---|---|---|
| M3 cold start | **411 ms (page cache evicted)** | main `4958703` (H2+P12) | B2, E3.2 | 2026-11-13 | pre-H2 406 ms — tie; P12 event settle already in the pre-H2 build's path |
| M4 warm launch | **strict 336 ms / fast 203 ms** (clean reps 324 / 175) | main `4958703` | B2, E3.2 | 2026-11-13 | pre-H2 309/175; rep 2 ran at load1 6–11, clean rep ties 175 ms |
| M5 screenshot | **24.9 ms** | main `4958703` | B2, E3.2 | 2026-11-13 | pre-H2 24.3 (tie) |
| M6 reset | **strict 341.1 ms / fast 202.9 ms** (H15 low-load window: 24 resets/arm, 3 reps, load1 0.71–2.43); strict 333 ms on main `4958703` (B2) is the same arm at a different load | `perf-h15-reset` (H15 cgroup-probe cache) off main `1ce336b` | H15, E2.14 | 2026-11-13 | vs main `1ce336b` interleaved: strict 343.1 → 341.1 (−0.6 %, noise), fast mixed across 4 windows (pooled 96/arm **211.1 → 208.2**, p95 231.0 → 213.3; −8…−16 ms in three windows, tie in one). 42 % of the strict reset is the frame bind and 117 ms of it a framework animator run that draws identical pixels (E2.14 §Finding 1) — the 25 % target is not reachable without a strict-rule change |
| M7 UI tree | **strict 41.1 ms / fast 0.48 ms** | main `4958703` | B2, E3.2 | 2026-11-13 | pre-H2 41.1/0.44 — tie |
| M8 tap → settled (animations on) | **counter 956 ms strict / 56.1 ms fast (clean-rep 44.3); form 944.7 / 45.5 ms** | main `4958703` | B2, E3.2 | 2026-11-13 | pre-H2 956/44.3/948/45.0 — tie; the ~0.9 s framework press animator is unchanged |
| M8 tap → settled (animations off) | **counter 131 ms strict (124–132); form 113 ms (110–154)** (7.34× / 8.35×) | main `4958703`, `--animations off` | H14, E3.3 | 2026-11-13 | P12 switch; `first_change_ms` 55–56 ms unchanged, the animator is what disappears |
| Tap acknowledgement | **5 ms** | main `4958703` | B2, E3.2 | 2026-11-13 | tie (P1) |
| M1 idle memory, 1 app | **66.5 MiB cgroup anon+shmem** (ΣPss 210 MiB) | main `4958703` (H2 on) | B2, E3.2 | 2026-11-13 | pre-H2 98.9 MiB — **−32.8 %** (H2 early GC) |
| Memory / extra session | **64.96 / 65.22 / 65.59 MiB cgroup anon+shmem** at N=1/5/10; ΣPss 195.4 / 76.8 / 62.6 MiB | main `4958703` (H2 on) | B2, E3.2 | 2026-11-13 | pre-H2 97.3/98.0/97.7 cgroup and 213/108/94 Pss — **−33 %** anon+shmem, −29/−33 % ΣPss at N=5/N=10 |
| Memory / extra session, opt-in shared boot image (H13) | **65.5 (N=1) / 66.9 (N=5) / 64.4 (N=10) MiB cgroup anon+shmem, lz4 + H2**; best opt-in shared image **56.7 / 55.2 / 54.1** (`shared-aslr`, N=5/10 median of 2 cohorts; `shared-fixed` 54.3/53.6/53.0) | `mem-h13-shared-image` off `f9f6ffa` | H13, E2.12 | 2026-11-13 | H2 on; shared image −8.7 / −11.6 / −10.3 MiB/session vs lz4, +3.6 s first launch. Pre-H2 H6 row: 97.3/98.0/97.7 |
| Idle CPU / wakeups | **0.166 / 0.743 / 1.394 %** of one core at N=1/5/10 (**0.139 %**/session at N=10); **1.21 / 2.06 / 3.08** wakeups/s | `perf-h4-idle-cpu` (H4) off `4737bb7` | H4, E2.16 | 2026-11-14 | interleaved 3-rep A/B vs `4737bb7`: wakeups **−96.4/−97.7/−98.1 %**, CPU **−73.0/−70.8/−71.6 %**; latency unchanged (worst +1.0 %, loop −1.1 %), 0 false passes. Supersedes the B2 row (0.647/2.77/5.28 %, 34.1/90.8/161.7 wakeups/s, main `4958703`) |
| Teardown | **12 ms** (`teardown_ms`, lifecycle) | main `4958703` | B2, E3.2 | 2026-11-13 | tie with GL1 12 ms (−33…−40 % vs 18–20) |
| Agent loop (animations on) | **1421.9 ms/task; 0.658 tasks/s, 281.6 (page cache) / 891.7 (anon+shmem) tasks/GiB-min at N=1; 1424.6 ms, 2.699 tasks/s, 681.3 / 846.6 at N=5** (H15 binary, 3 reps, verified 1.000); main `4958703` measured 1437.6 / 1441.0 ms in the same H15 window | `perf-h15-reset` (H15) off main `1ce336b`; B2 row for main `4958703` | H15, E2.14 (B2, E3.2) | 2026-11-13 | H15 vs its own interleaved base: **−1.1 % latency at N=1 and N=5**; 0 false passes, verified rate 1.000 |
| Agent loop (animations off, functional-check mode) | **507–523 ms/task, 1.79 tasks/s, 5239 tasks/GiB-min (anon+shmem) at N=1; 537 ms / 6.28 / 3942 at N=5; 530 ms / 10.22 / 3798 at N=10**; H15 re-measured 490.8 (base) / 501.7 (H15 binary) at N=1 and 518.3 / 516.9 at N=5 | main `4958703`, `--animations off` | H14, E3.3 (H15, E2.14) | 2026-11-13 | 2.71–2.84× latency; the 5.8×/5.0×/8.7× anon+shmem tasks/GiB-min ratios at N=1/5/10 were the S3c denominator bias — corrected (E3.5) the *latency* ratio 2.76–2.79× holds and the cohort GiB-min ratio tracks throughput (1.23–1.30× at N=20); loop becomes reset-bound (48 %); not a default change. H15's Δ is inside noise (±5 % threshold, overlapping rep ranges) |
| False passes | **0** — P11 0/105, scenarios 0/12 (negative control = honest `timeout`), loop 0; verified 63/63 (N=1) and 75/75 (N=5) | main `4958703` | B2, E3.2 | 2026-11-13 | H6's 1-in-75 strict-assert flake (H16) did not reproduce |
| Agent loop N=10 reliability | **100 %: 600/600 verified over 12 reps** (load1 1.0–4.6), 0 false passes; 428/450 (95.1 %) before the fix (exit 1/126 torn Landlock plan, exit 135 SIGBUS on the truncated mapped APK) | `fix-h17-n10-reset` off main `84fc8bc`, 10 sessions / one serve | H17, E2.13 | 2026-11-13 | fixed two shared-input races (atomic publish + per-run APK); see E2.13 |
| Compat first-frame rate (K1 corpus, real ART, N=1) | **17/42 (40.5 %)**; tree/screenshot 16/42 (38.1 %); tap 9/42 (21.4 %); `onCreate` completed **20/42** (47.6 %); first frame p50 **808 ms** / p90 **1269 ms** (K3 12/42, tap 7/42, 16/42, 647/1050 ms). Same binary with app providers off: 14/42 first frame, tap 8/42, 18/42, p50 546/p90 1043 ms — ON is >= OFF at every rung with zero per-app regressions, so ON is the default | K4 `compat-k4-metadata` (E5.8 K3d metadata+sourceDir, E5.9 K3e binder-driver refill) | K4/K3d–K3e, E5.8–E5.9 | 2026-11-13 |
| Agent loop at N=20 | **1460.4 ms/task p50 / 1501.8 p90; 7.41 verified tasks/s; 471.9 (page cache) / 603.0 (anon+shmem) tasks/GiB-min; 73 MiB anon+shmem and 53 MiB ΣPss per session; 21.2 % of one core; 298/300 verified, 0 false passes** | `scale-s3-n20` off main `15849a5` (H2+P12+H17), 20 sessions / one serve, 3 reps, animations on | S3, E3.4 | 2026-11-13 | latency +1.6 % vs N=1 and throughput 11.4×; the 2 losses are the new reset defects S3a/S3b. ‡ The **anon+shmem GiB-min is corrected (S3c/E3.5)**: 427–474 tasks/GiB-min (before 603–768), 66.7–68.1 MiB/session; at 65.6 MiB/session the per-busy-session rate stays 641 |
| Agent loop at N=20 (animations off, functional-check mode) | **543.8 ms/task p50 / 575.3 p90; 10.49 verified tasks/s; 711.7 / 1414.2 † tasks/GiB-min; 65 MiB/session anon+shmem; 300/300 verified, 0 false passes** | same branch, `--animations off` | S3, E3.4 | 2026-11-13 | 2.7× the on-arm throughput. † The old denominator undercounted guests (S3c): corrected N=20 value **553.5–557.0 tasks/GiB-min** (before 1182–1226) and **66.5–68.9 MiB anon+shmem / 53.1–57.0 MiB ΣPss per session** with 0 zero readings (before 2.7–43.8 / 0.0–55.0); E3.5, `results/bench/S3c.md` |
| Scale ceiling (verified loop, largest N probed) | **N=30: 600/600 verified (100 %), 0 false passes**; p50 1534–1541 ms on, 5.78–6.79 tasks/s; **job cgroup peak 2.34–2.67 GiB (≈4 % of 61.9 GiB), host used 23–24 %**, no oom_kill | same branch, 24G guarded arm, 4 reps (3 on / 1 off), load1 5.1–13.0 | S3, E3.4 | 2026-11-13 | N=20 used < 50 % of RAM so N=30 was probed and passed; the ceiling is ≥ N=30 and is set by latency stability under external load, not memory. Reset defects S3a/S3b are stochastic (seen at N=10/20, not in this 600) |
| Compat first-frame rate at concurrency (K1 corpus, 42 APKs) | **N=5: 13/42 (13/13 tree); N=10: 13/42 (13/13 tree), p50 593–603 ms / p90 1152–1199 ms** — same rung as N=1 (12–13/42, p50 647 / p90 1050); **no app lost vs N=1**, failure class only `GUEST_EXITED` | `scale-s3-n20` off main `15849a5`, real ART, `settle:fast`, 25 s first-frame budget, 2 runs per N | S3, E3.4 | 2026-11-13 | per-app walls, not concurrency: `com.libreweatherrn` (known K3 flicker) framed in every run; `org.wikipedia` flickers (2 of 4 N=10 runs, 0 N=5) at its app-level `exit 10` wall |
| Gaten at N=10 (RN 0.86.3 / Fabric / Hermes, real ART) | **10/10 first frame, 10/10 tree (243 nodes), p50 1431–1441 ms / p90 1.48–1.50 s; 185–192 MiB anon+shmem and 175–182 MiB ΣPss per session; 47 % of one core; 0 native crashes in 16 exits over 2 reps** | `scale-s3-n20` off main `15849a5`, fast settle (welcome screen never strictly settles; GR3), 2 reps at N=1/5/10 | S3, E3.4 | 2026-11-13 | the per-session footprint is flat from N=1 (187–194 MiB) to N=10; first frame precedes the RN content by up to ~2 s (immediate tree = 8 nodes, +2 s = 243) |
| Agent loop N=10 (Z7 method, fresh launch/task) | 45.0 tasks/GiB-min main; **67.7 with exec-path early GC** (seam, off by default → H2); 103.6 zygote+early GC (research, trusted only) | Z7 `zygote-z7-lifetime` | Z7, E2.8 | 2026-11-12 | post-H6 (current main) |
| Agent loop N=10/N=5, H2 default-on | **85.9 tasks/GiB-min** (off 55.7, +54.2 %) / **135.6** (off 98.8, +37.2 %); 1-min interleaved reps, 0 false passes | `perf-h2-early-gc` `54b9c33` | H2, E2.10 | 2026-11-13 | post-H6 (current main) |
| Early-GC memory saving per session | **28.2–30.6 MiB ΣPss** at N=5/N=10 (27.1–32.5 %); 28.9–30.4 MiB at N=1 | H2 default `54b9c33` | H2, E2.10 | 2026-11-13 | post-H6 (current main) |
| Gaten-vs-device footer delta | **0 px** (`report`, phone geometry nav 80); `report` nav 60 = +20 px; GR1 `carve` = +60 px | `wm-w7-insets` | W7, E13.5 | 2026-11-13 | first model that reproduces the device footer |
| Gaten-vs-device SSIM | **0.87960** (`report`, nav 80) vs 0.84854 GR1 / 0.74518 recomputed before | `wm-w7-insets` | W7, E13.5 | 2026-11-13 | >16/255 pixels 14.56 % → 7.20 % |
| False passes | **0**, class closed at the root: selectors/asserts/taps match user-visible nodes only; off-screen-only match = `ELEMENT_OFFSCREEN` | `verify-v1-visibility` | V1, E13.4 | 2026-11-13 | V1 (current main) |
| Gaten flows passing (real ART) | **15/15: gaten-home, gaten-tabs, gaten-settings, gaten-search, gaten-lesson 3/3 each**, 5/5 negatives `ASSERTION_FAILED`; launches **40/40, 0 crash, 0 thin first frames** (default config) | branch `fix-g3cd-gaten-launch` (off `main` `11b917c`) | E13.8, G3cd | 2026-11-13 | supersedes the GR3 row (E13.7): the tab loss was the GWP-ASan launch SIGSEGV, now named and removed; the launch frame is the app's content frame or a labelled placeholder |

## E9.1 — T1: workspace test health — PASS for the hang and the stale goldens; 5 real failures remain and are filed

**Date:** 2026-11-13. **Commit:** the T1 commit on this branch (base `c47bebb`).
**Hypothesis (T1):** the frozen-VM integration failures are all stale goldens
(PMS `app.flags`, framework bool) and `render_views` is an unrelated slow test.
**Method:** one guarded invocation per test binary
(`tools/guarded.sh --mem 12G --time 240 -- cargo test --release --offline -p
<crate> --test <name> --no-fail-fast`), then per crate; a debugger stack for
the hang (`timeout -s INT gdb --args …`, batch `thread apply all bt`); ART gates
under one `run-exclusive.sh --task T1` lock.

**What the hypothesis missed.** Three shared wire bugs produced most of the
"golden" failures: (1) `SurfaceControl.nativeReadFromParcel` read only an i64
handle while the WMS Rust codec emitted the full libgui body, leaving 796
unread bytes in `addToDisplayAsUser` and `insetsControlChanged` (no root view →
no first frame); (2) `KeyCharacterMap.nativeReadFromParcel` assumed
`[keyboardType, String8]` instead of the image's `deviceId, hasMap, cstring,
key records, remap tables`; (3) `render_views` hung on a **non-reentrant mutex
double-lock** in `gfx_vector::pp_can_morph` (`path_data()` twice in one
expression), reached by `CheckBox` vector-path inflation. The PMS golden was
stale (`0x3000_0046` = debuggable + allowClearUserData), and the framework bool
is AOSP's `0xFFFF_FFFF` true sentinel, not `1`. `services_t38` predated K2/K5's
faithful "notification post is recorded, not rejected" behaviour.

**Results.**

| Area | Before | After |
|---|---|---|
| `acos-runtime` (31 binaries) | 15+ failures, `counter`/`m2`/`wms` no first frame | **119 pass / 2 fail / 4 ignored** |
| `acos-vm` (31 binaries) | `render_views` unbounded hang; `inflate_layout` fail | **210 pass / 1 fail**; render_views 10.2 s |
| `acos-framework-natives` (8) | `theme_resolution` fail | **40 pass / 0 fail** |
| Whole workspace (22 other crates) | arm64/protocol/native-loader/mcp/jni failures | only **jni + mcp Flutter-pointer** (same root) and the 2 app-matrix cases |
| ART gates | — | v2-counter, w22, form-validation, T03-good pass; T03-wrong `ASSERTION_FAILED` |

Remaining known failures: app-matrix AlertDialog button-bar `stateListAnimator`
resource 0 (T1a), app-matrix programmatic list scroll not in the tree (T1b),
`inflate_layout`'s `MockContext.isRestricted` dispatch (T1c), Flutter pointer
packet never reaches `PlatformDispatcher.onPointerDataPacket` (T1d), and the
three `render_views` `PIPELINE-FAIL` coverage gaps (T1e). Full detail:
`results/tests/T1.md`.

**Decision:** keep the frozen-VM tests, refresh the two goldens, fix the shared
wire/native bugs, and keep a bounded `HostGuard` in `render_views` so a future
guest loop fails loudly. File the five real failures as T1a–T1e; do not weaken
any verify/assertion or mark a real failure `#[ignore]`.

## Backlog (optimization hypotheses; rank = impact × confidence ÷ (cost × risk))

Re-ranked by B2 (E3.2) on the measured objective: **verified tasks per second per
GiB of agent-visible (cgroup anon+shmem) memory**. The 2026-11-12 snapshot ranked
memory per session #1; H2 has since taken 32 MiB/session off it, the loop is
tap-bound at the default and reset-bound with animations off, and the GiB
denominator is now the per-task **boot/relaunch transient**, not steady state.

| ID | Hypothesis | Metric | Expected | Confidence | Cost | Risk | Status |
|---|---|---|---|---|---|---|---|
| S3a | **Reset relaunch can hang with no live frame (60 s).** `acos_reset` → relaunch → `NO_FRESH_FRAME`/`LIVE_FRAME_UNAVAILABLE` after 60 048 ms: `no live frame within 60s: activity_resumed=true probe=true c1_generation=0` (`crates/acos-art/src/launch.rs` startup-budget expiry). The guest resumed its Activity and the probe produced tree samples, but C1 stored no frame for 60 s, so the launcher killed it. 1/100 tasks at N=20 (bench r2-on) + 1/100 reproduced; one N=20 off rep also has a 60 s successful reset. Evidence `results/bench/S3-evidence/loop-n20-reset-nofreshframe.json` | loop verified rate at N≥10 | back to 100 % | high (reproduced, named path) | low (add C1/FrameStore state to the timeout record) | low | **open, found by S3 (E3.4)** |
| S3b | **Reset relaunch guest exits 1 in 133–301 ms.** `GUEST_EXITED: the guest ended before its first live frame (status exit status: 1)`; 1/150 tasks at N=10, 1/300 at N=20, 1/100 reproduced. No signal, no `native_crash` record (R3 retention only fires on a crash signal) and the guard line was not kept, so the cause is unnamed; it is the symptom class H17 fixed (torn `<data>/seccomp/landlock.plan` or mapped APK), which must be ruled out first. Evidence `results/bench/S3-evidence/loop-n20-reset-guestexited.json` | loop verified rate at N≥10 | back to 100 % | high (reproduced) | low (retain guard/`session-exit.json` for exit 1, then re-read) | low | **open, found by S3 (E3.4)** |
| S3c | **The bench's anon+shmem and Pss sampling undercounts churn.** `WarmSampler` rediscovers guest cgroups every 1 s, so guests that live < 1 s (every animations-off task, every short compat app) are missing from most samples: the anon+shmem GiB-min integral is inflated 5–10× on the off arm and ~1.4× on the on arm (E3.3's 5–8.7× off-arm ratio is this bias; the true ratio is 2.7–2.9×). `session_pss()` sums the serve's *current* children, so during a relaunch burst it sees launchers without guests (off N=20 read 2.7 / 0.0 / 51.5 MiB per session). | tasks/GiB-min, M2 | trustworthy GiB metric | high (measured both arms) | low (discover on change; Pss per session id) | low (metric only) | **open, found by S3 (E3.4)** |
| H15 | **Measured: the reset's 341 ms is 13 stop + 186 cold boot + 142 strict bind, and 117 ms of that bind is a framework animator run (`RippleAnimationSession`/`StateListAnimator`) that redraws byte-identical pixels for 7 frames.** Implemented (in the worktree, uncommitted): the per-boot cgroup capability cache (≈7 ms/launch; pooled fast reset −1.4 %, p95 −7.7 %). Strict reset does not move outside noise without a rule/architecture change | M6, loop | −150–200 ms/task → −10…14 % loop (on), −30…40 % (off) | high (split measured) | medium | medium | **done 2026-11-13, E2.14: strict −0.6 % (noise), fast −3.9 %, loop −1.1 % both modes, 0 false passes.** The 25 % target needs an owner decision: (A) content-stable strict confirmation (prize −117 ms = −34 % M6 strict, loop −8 %/−22 %; cost an async C1 content hash — `capture_composite` is 10–30 ms), or (B) let the agent loop's reset step use `settle: fast` (harness, −142 ms/task; the next strict step still backs the verdict). Zygote (C) would cut the 186 ms boot and is declined (H3) |
| H13 | **Memory per session (now 65–67 MiB with H2) is the GiB denominator.** B3's shared uncompressed image is implemented as `ACOS_ART_BOOT_IMAGE=lz4|shared-fixed|shared-aslr`; `shared-aslr` relocates once per serve (16-bit base) and saves 10–12 MiB/session at N=5/10 | M2, tasks/GiB-min | −10–12 MiB/session (shared image); zygote CoW still −25–60 % at N≥5 | high (shared image) / medium (zygote) | low (shared image) / medium | medium (fixed image address for `shared-fixed`; +3.6 s first launch) | **#1, implemented opt-in (E2.12); not default — owner decision. Next: offline relocator or zygote CoW** |
| H14 | **Strict tap 956 ms with the framework press/elevation animator.** Adopted the P12 `animations:off` switch as the agent-facing functional-check mode: 7.34×/8.35× taps, 2.71–2.84× loop, 5–8.7× tasks/GiB-min (anon+shmem), 0 false passes. Defaults unchanged | M8, loop | measured | high | low | medium (verification semantics: fast never backs an assertion) | **done: mode available, E3.3; use it for functional checks** |
| H17 | **N=10 verified loop: fixed.** Two shared-input races: `<data>/seccomp/landlock.plan` rewritten in place by every launch and read by the guard at start (`ERR landlock=plan-missing`, exit 126/1), and the shared `<data>/app/<apk>` truncated by `fs::copy` while ART had it mapped (SIGBUS, exit 135). Atomic `publish_atomic` + per-run APK. 428/450 → **600/600** verified over 12 after-reps, 0 false passes; N=5 75/75, N=20 40/40 | loop verified rate at N≥8 | 100 % | high | done | low | **done 2026-11-13, E2.13** |
| H17b | **The M6 bench harness ignored its launch result** (H17 follow-up): a failed launch became 20 fast reset refusals and `ok=True`; P5/s3_scale also admitted failed-task latency and always exited 0, and per-task PNGs grew without bound. Fixed by checking every step, excluding failed tasks from latency, exiting non-zero on harness failures, capping screenshots and porting all harnesses to `ChurnSampler` with coverage. Negative proof: main 20+20 fast samples (0.009–0.010 ms), `ok=true`, exit 0 → after 0 latency values, 170 coded failures, exit 1; N=5 45/45 verified, p50 1431.8 → 1448.9 ms (noise). | harness integrity | no false good numbers | high (measured) | done | low | **done 2026-11-13, E3.6** |
| H16 | **N=5 strict assert flake** (H6: 1/75). Not reproduced in B2 (63/63 N=1, 75/75 N=5, plus 150+ H14 tasks) | loop verified rate | — | low now | low | low | **not reproduced in B2** (H6 saw 1/75 at N=5: a strict post-tap count read that did not see the tap; false_pass stayed 0). Twelve B2/H14 loop cohorts, 0 failures of that shape; the N≥8 failure mode is H17 instead |
| H9 | Form-validation N=5 under host load: 3/5 `:id/name` taps hit the 30 s bound (2/5 verified) **with early GC off and on**, 2/2 runs each; Z7 saw the same in a/a′/c′ | loop correctness | remove a pre-existing input/queue stall | high (reproduced) | medium | low | **resolved 2026-11-13, E2.11/L1**: not input or the looper — the pre-P12 pixel-unchanged guard (on by default) timed out focus taps with no pixel change/no C1 buffer. P12 guard-off default + `TrustGuard::refresh`: N=5 form 5/5 ×2 at 110–176 ms, guard-on reproduces 30 s (2/5). Evidence `results/art-runtime/evidence/L1/` |
| H4 | Idle CPU **5.28 %** of one core and 161.7 wakeups/s at N=10 (0.53 %/session). Removed the 250 ms probe fallback and every per-session polled timeout on the idle path (serve `poll`, launcher `recv_timeout`, binder-looper timedwait, logd) | CPU, wakeups | lower wakeups; small at N=10 now | medium | low | low | **done 2026-11-14, E2.16 (`perf-h4-idle-cpu`): interleaved 3-rep A/B — wakeups 34.06/90.22/160.52 → 1.21/2.06/3.08 s⁻¹ and idle CPU 0.614/2.543/4.915 → 0.166/0.743/1.394 % at N=1/5/10 (−96…−98 % / −71…−73 %); latency unchanged (worst +1.0 %, loop −1.1 %), 0 false passes.** Needs the host probe-liveness fix (`last_requested_seq`) or `w42-back-frame` times out; remaining ~40 s⁻¹/session is the 50 ms RSS watchdog, left alone (safety bound, no event source) |
| H5 | Agent loop with animations:off + event settle | loop | 2.2 → <0.8 s | medium-high | low | low | **done (folded into H14)** |
| H6 | Nightly metric regression bench | all | guard | high | done | low | **done + re-baselined on current main (B2): `BASELINE.json`, anon+shmem GiB-min rows added** |
| H7 | Shrink per-session boot-image private memory | M2 | −10–25 MiB | medium | medium | medium | **folded into H13** (B3 is the measured first step) |
| H8 | Faster PNG (SIMD deflate) / region screenshots | M5 | 24 → 10–15 ms | medium | medium | low | backlog (5.5 % of the animations-off loop, 2 % on) |
| H3 | Warm exec pool | M3/M4 | −73 ms launch | — | — | +idle memory | **declined by owner 2026-11-12** |
| K3a | K2 next walls: AndroidKeyStore shim, GL/BLAST natives, WebView provider classification | compat first-frame | +3–5 apps | high (K1/K2 evidence) | medium | medium | **classified (K3, E5.7): all three need a real backend (weeks); no shim, no fake pass** |
| K3b | K2 PMS error class (`NameNotFoundException`) and `checkService` null for unknown names | compat | +2 apps (vlc, fdroid) | high | low | low | **accepted `9df9e71` (E5.5): both leave the wall; vlc → `ConstraintLayout` inflate (R2), fdroid → `getSystemAvailableFeatures`** |
| K3c | K2 app provider delivery (androidx.startup/WorkManager `InitializationProvider`) | compat | +element, feeders | medium | high | medium | **implemented + measured `40ffc61` (E5.6), OFF by default (`ACOS_ART_APP_PROVIDERS=on`); blocked by K3d/K3e** |
| K3d | manifest `<meta-data>` → `Bundle` in every `PackageItemInfo` reply under `GET_META_DATA`, plus the guest-openable `sourceDir` | compat | +editor full ladder, +joplin/unitto/antennapod/streetcomplete first frame; unblocks K3c | high (K3 measured the failure) | medium | low | **accepted K4 (E5.8): parsed in `acos-apk` (`parseMetaData` transcription), carried on the typed PMS records, gated on `0x80`, plus `sourceDir` = `/data/app/<name>.apk`; K3c delivery is now the default** |
| K3e | app-published `IContentProvider` binder in `publishContentProviders` fails to marshal (`BinderProxy.transactNative` "Unknown error", joplin) | compat | unblocks K3c for joplin | medium | medium | medium | **accepted K4 (E5.9): not a marshal bug — the userspace driver cut a `BR_*` command at libbinder's 256-byte read buffer (`*** BAD COMMAND`); `flush_pend` now copies whole commands only. joplin: no start rung → first frame 1269 ms + tree + screenshot** |
| T1 | **Legacy frozen-VM test health (mostly done, E9.1):** the 15 failure reports were three shared wire bugs (SurfaceControl body read short by 40 bytes; KCM native payload format; `render_views` non-reentrant mutex double-lock in `pp_can_morph`) plus two stale goldens (PMS `app.flags=0x3000_0046`; framework bool true `0xFFFF_FFFF`). Fixed; `acos-runtime` 119/2/4, `acos-vm` 210/1, `acos-framework-natives` 40/0. `render_views` now 10.2 s with a 180 s HostGuard. Remaining real failures split into T1a–T1e | test-suite health | `cargo test --workspace` green; no hang | high | done | low | **done 2026-11-13 (E9.1)**; remaining real failures T1a–T1e |
| T1a | app-matrix `dialogs_alert_opens_and_cancel_dismisses`: `AlertDialog` button-bar `Button` inflation raises `Resources$NotFoundException: Resource ID #0x0` from `AnimatorInflater.loadStateListAnimator`; dialog never reaches the tree | dialog compat | AlertDialog opens/cancels | high (deterministic) | medium | low | **open (T1, E9.1)** |
| T1b | app-matrix `list_scroll_button_moves_the_recycled_window`: the `Scroll to last` tap reports `state_changed` but the pumped tree still starts at Item 0 (smooth scroll not settled/observable) | list interaction | tail row visible after tap | high (deterministic) | medium | low | **open (T1, E9.1)** |
| T1c | `acos-vm --test inflate_layout`: real `MockContext.isRestricted()` throws `UnsupportedOperationException`; the test's `TestContext` override is present in the DEX but the framework `TextView.readTextAppearance` call still lands on `MockContext` (virtual-dispatch/context identity gap) | frozen-VM inflate coverage | TextView inflation succeeds | high (stack + DEX evidence) | medium | low | **open (T1, E9.1)** |
| T1d | Flutter `flutter_hello_under_confinement` and MCP `flutter_launch_observe_tap_round_trip`: pointer packet never reaches `PlatformDispatcher.onPointerDataPacket`; engine logs `Failed to set raster task runner priority` | Flutter product path | tap reaches Dart | high (both tests) | high | medium | **open (T1, E9.1)** |
| T1e | `render_views` pipeline coverage: `form-validation`/`text-input` hit `Settings$Config.getContentResolver` NPE; `widget-gallery` hits unsupported `PathParser.nInterpolatePathData` (test records `PIPELINE-FAIL`, does not false-pass) | render corpus coverage | 5/5 pipeline scenes | high | medium | low | **open (T1, E9.1)** |
| H1 | Strict numbers drop after event settle; re-measure M3–M8 and take cheap wins | M4/M6/M7/M5 | — | — | — | — | **done (H6)**: M7 151→41, M4 440→309, M6 463→342 |
| H2 | Early post-first-frame concurrent GC (Z7) on the exec path | M1/M2 | −23–30 MiB/session | high | low | low | **accepted: product default, E2.10** (H6 numbers are pre-H2; re-baseline) |
| R2a | Gaten launch flake: SIGSEGV after first frame (~1/10). **Fixed (E13.8):** the fault is libc's GWP-ASan sampled-allocation backtrace (`android_unsafe_frame_pointer_chase+0x117`), enabled for 1 in 128 guest processes by libc's own lottery; the frame-pointer walk follows a guard page (`SEGV_ACCERR`) on frame-pointer-free guest code. Guests now get `GWP_ASAN_PROCESS_SAMPLING=0` (`ACOS_ART_GWP_ASAN=on|libc` are the seams). Forced-on: 4/4 SIGSEGV; default: 40/40 launches, 0 crash. Residual: one exit-1 `GUEST_EXITED` before the first frame in the 60-launch before-arm is a different, unnamed class | verified-launch stability | name the frame, then 10/10 | high | low | low | **done, E13.8/G3cd** (root cause named; one separate exit-1 class remains open) |
| R2b | W6 `+1080 px` fragment offset: move `ACOSLAUNCH_GEOM_TRACE` to `application-created`, keep failing runs, n=20. Then a first-measure race vs a pager-translation source is decidable from `win_frame` vs `app_cfg` | layout correctness | catch + fix the root cause | medium | low | low | **next** (R2 §6) |
| W7a | `report` is faithful (Gaten footer 0 px, SSIM 0.87960) but cannot become the default while `w42-back-frame` changes one AA row (G: 210→211). Decide: accept the changed pixel expectation as more faithful, or make strict capture tolerant of a single AA row without masking real moves. `gaten-home`'s forever-looping welcome also needs a strict-settle answer | verification semantics / default | `carve` → `report` default | high (measured) | low (owner decision) / medium (settle) | medium | **decided (ADR-0026 decision 6); implemented W7b (E13.9): `report` default, w42 re-blessed with a one-row pixel proof** |
| R2c | Per-view canvas for the ART guest without a GPU: opt-in per-view nested `Bitmap` canvas + blit (R2 §4.3) vs a `libhwui`-shim `RenderNode`/`HardwareRenderer` | SVG/wordmark fidelity | `canvasIsViewPx=true` | medium | medium/high | medium (draw cost 1.5–3×) | **owner decision** (R2 §4) |
| G3a | **Scenario input could not request the API's existing fast navigation settle**, so every tap on Gaten's 200 ms-retry onboarding pages was a strict `queue` timeout. Fixed: `settle: fast` on scenario `tap`/`type`/`key`, forwarded through `TrustGuard`; assertions still reject fast | reachable owner flow | 3/3 after the fix | high | low | low | **done, E13.7/GR3** |
| G3b | **Strict-settle probe wake over-driven the guest**: 20 ms host requests re-asked even after a sample arrived (~50 full tree walks/s under the 10 s Reanimated loop); likely the `probe-floor` starvation. Fixed to 50 ms and no re-ask until the probe sequence moves | probe liveness / tree age | no probe-floor at long n | medium | low | low | **fixed in code, E13.7; still prove at a 60 s pathological n** |
| G3c | **First frame can be a near-empty tree** (1 node in GR3, **10 nodes in 34 % of 60 measured launches**, 243 two seconds later). Fixed (E13.8): `ArtSession::bind_first_frame` re-traverses while the tree is still the window skeleton (no text/content-desc/hint, ≤ 40 nodes) within 900 ms / 4 traversals; a retry that does not settle rolls the frame *and* its settle verdict back, so no launch turns into a TIMEOUT; a frame that never gains content is `first_frame.placeholder` + `meaningful:false` | first-frame predicate / Gaten first paint | launch frame is the app's content | medium | medium | medium | **done, E13.8** (34 % thin → 0/40; launch p50 2.336 → 2.433 s) |
| G3d | **Gaten launch SIGSEGV** (1 of 6 `gaten-tabs` runs in GR3): `SIGSEGV code 2 (SEGV_ACCERR)`, thread 2 `com.gaten`, libc frame. Root cause named (E13.8): GWP-ASan's sampled-allocation backtrace, `android_unsafe_frame_pointer_chase+0x117` (`mov 0x8(%r12),%rcx`) reading a stack guard page; the only GOT consumer of that function in libc is `MaybeInitGwpAsan`. Fixed by keeping the sampled allocator out of guests | ART guest native fault | name the faulting frame, then 10/10 | high | low | low | **done, E13.8** (forced-on 4/4 crash reproduces it; default 40/40 clean) |
| G3e | **No verification semantics for a screen that animates forever in a region that does not matter.** Options (stable tree content, region mask, navigation-only fast, explicit non-settle assertion) + false-pass analysis in `results/gaten/GR3.md` §4.3 | verify semantics | owner decision | — | — | — | **owner decision; do not weaken until then** |
| G4a | **The tree reports `visible=false` for a node whose pixels the app painted** (GR4). `tree/18.json`: the bottom bar's active label `[512,2214][642,2251]` is `visible: false` because it lies below the root's `visible_bounds` bottom (`2209`) in a `insets: carve` session, while the bound frame `frames/18.png` paints its glyphs at png `y 2088–2122`, `x 513–640` (bounds − `display_origin.y` 128). AOSP's `View.getGlobalVisibleRect` intersects with the display and ancestor clips, not with the nav-bar band, so a real a11y tree reports it visible (a tap there would still land on the system bar, so fail-closed is defensible — the *classification* is what is inconsistent). Lowest layer: per-node `visible`/`visible_bounds` vs the carved visible frame, surfaced as `ELEMENT_OFFSCREEN` (`crates/acos-verify/src/native.rs`). Verification false-negative only, **never a false pass**. Worked around in scenarios: target the tab pressable's `content_desc`, never the label (`results/gaten/GR4.md` §2.2, §5) | node visibility vs frame | classification matches the pixels | high (tree + frame, same bound frame) | low (decide the policy, then align) | low | **open, found by GR4 (E13.9)** |
| G4b | **Tree frame metadata reports `density: 420` for a `density_dpi: 400` run** (GR4): every `tree/*.json` frame record says 420 while `repro.json`/`verdict.json` echo the scenario's 400. Either the guest boots at the reference 420 on this path or the tree's `FrameMeta` is stamped from the reference default; not root-caused. Consequence: density-dependent geometry cannot be audited from tree metadata, which is exactly the density-fix area. Pixels are correct either way (116 px wordmark glyphs, GR4 §4) | tree/frame metadata integrity | metadata agrees with the launch spec | high (36 runs, all 420) | low (thread the launch density into the tree record) | low | **open, found by GR4 (E13.9)** |
| G4c | **A scenario can pass 3/3 while proving nothing, and nothing in the harness notices** (GR4). A `tap` on a control that is already in the asserted state writes nothing — Gaten's Kanji reference `select()` returns early when `next === source`, and the guest's default was already the asserted value — so the post-tap assertions hold on state that never changed. The only evidence was pixel-level: the before/after bound captures were byte-identical (0 changed px) where the claim needs a change. Candidate: an explicit step (`expect_change` / `assert_frame_differs`) or a writer-side rule that a `capture` pair around a mutating tap must differ, so a vacuous flow fails instead of passing. Worked around in GR4 by asserting the hub label's *other* value is absent at each hop and by diffing the captures by hand (`results/gaten/GR4.md` §2.3) | verify semantics / false-pass class | no vacuous pass | high (reproduced + rewritten) | medium (new step + schema + provenance rule) | medium (a legitimate no-op must stay expressible) | **open, found by GR4 (E13.9)** |
