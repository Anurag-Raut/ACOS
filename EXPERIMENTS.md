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

## Leaderboard (best measured configuration per metric; update on every accepted experiment)

Central objective: verified agent tasks per second per GiB (tasks/GiB-min).

| Metric | Best value | Config / commit | Experiment | Date |
|---|---|---|---|---|
| M3 cold start | 463 ms (482 evicted) | main P7/P8 | P7, P8 | 2026-11-12 |
| M4 warm launch | strict 440 ms / fast 184 ms | main P9 (pre-event re-measure pending) | P9 | 2026-11-12 |
| M5 screenshot | 24.4 ms | main P2/W1 | P2 | 2026-11-12 |
| M6 reset | strict 463 ms / fast 195 ms | main P9 | P9 | 2026-11-12 |
| M7 UI tree | strict 151 ms (pre-event) / fast 0.2 ms | main P3d/P10 | P3d, P10 | 2026-11-12 |
| M8 tap → settled | strict 955 ms (event, animations on); **182 ms p50 with `animations: off`** (P12 counter step incl. ~78 ms capture) / fast 70.9 ms | main P11, P12 | P1, P11, P12 E2.9 | 2026-11-13 |
| Memory / extra session | 100.9 MiB cgroup (pre-W1); zygote research 22.2 MiB Pss | O2; Z6 | O2, Z6 | 2026-11-12 |
| Agent loop | 2.21 s/task; 136.8 tasks/GiB-min (N=1), 188.0 (N=5) | main P5 (pre-P9/P10/P11) | P5 | 2026-11-12 |
| Agent loop N=10 (Z7 method, fresh launch/task) | 45.0 tasks/GiB-min main; **67.7 with exec-path early GC** (seam, off by default → H2); 103.6 zygote+early GC (research, trusted only) | Z7 `zygote-z7-lifetime` | Z7, E2.8 | 2026-11-12 |
| Agent loop N=10/N=5, H2 default-on | **85.9 tasks/GiB-min** (off 55.7, +54.2 %) / **135.6** (off 98.8, +37.2 %); 1-min interleaved reps, 0 false passes | `perf-h2-early-gc` `54b9c33` | H2, E2.10 | 2026-11-13 |
| Early-GC memory saving per session | **28.2–30.6 MiB ΣPss** at N=5/N=10 (27.1–32.5 %); 28.9–30.4 MiB at N=1 | H2 default `54b9c33` | H2, E2.10 | 2026-11-13 |
| Teardown (`acos_destroy`) | ~20 ms lifecycle, 300 ms bound then group kill | main GL1 | E2.7 | 2026-11-12 |
| Compat first-frame rate (K1 corpus, real ART, N=1) | **13/42 (31.0 %)**; tap 7/42 (16.7 %) | K2 `cdc1a69…c389090` | K2/R1–R3 | 2026-11-12 |
| False passes | 0 | all | P1–P11, K2, H2 (640 loop tasks + p11 gates + K1) | 2026-11-13 |

## Backlog (optimization hypotheses; rank = impact × confidence ÷ (cost × risk))

| ID | Hypothesis | Metric | Expected | Confidence | Cost | Risk | Status |
|---|---|---|---|---|---|---|---|
| H1 | Strict numbers drop after event settle; re-measure M3–M8 and take cheap wins (M7, M5 fast PNG) | M4/M6/M7/M5 | −30–60 % strict | high | low | low | queued |
| H2 | Early post-first-frame concurrent GC (Z7 finding) on the exec path | M1/M2 | −23–30 MiB/session | high (measured in Z7) | low | low | **accepted: product default `54b9c33`, E2.9** |
| H4 | CPU per idle session: remove 250 ms fallback poll and other timers | CPU, wakeups | large at N≥10 | medium | low | low | queued |
| H5 | Agent loop with animations:off + event settle | loop latency | 2.2 s → <0.8 s | medium-high | low | low | after P12 |
| H6 | Nightly metric regression bench | all | guard | high | medium | low | queued |
| H7 | Shrink per-session boot-image private memory (26.3 MiB decompressed per session) without a warm pool | M2 | −10–25 MiB | medium | medium | medium (ASLR/B3) | backlog |
| H8 | Faster PNG (SIMD deflate) or region/downscaled screenshots for agents | M5 | 24 → 10–15 ms | medium | medium | low | backlog |
| H9 | Form-validation N=5 under host load: 3/5 `:id/name` taps hit the 30 s bound (2/5 verified) **with early GC off and on**, 2/2 runs each; Z7 saw the same in a/a′/c′ | loop correctness | remove a pre-existing input/queue stall | high (reproduced) | medium | low | backlog (evidence `results/art-runtime/H2.md` §5) |
| H3 | Warm exec pool | M3/M4 | −73 ms launch | — | — | +idle memory | **declined by owner 2026-11-12** (no spare processes) |
| H9 | K2 next walls: AndroidKeyStore shim, GL/BLAST natives, WebView provider classification | compat first-frame | +3–5 apps | high (K1/K2 evidence) | medium | medium | queued |
| H10 | K2 PMS error class (`NameNotFoundException`) and `checkService` null for unknown names | compat | +2 apps (vlc, fdroid) | high | low | low | queued |
| H11 | K2 app provider delivery (androidx.startup/WorkManager `InitializationProvider`) | compat | +element, feeders | medium | high | medium | investigated (K2 §6) |
