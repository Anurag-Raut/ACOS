# Advisor: top-10 benchmark + agent-usability improvements — decided plan (2026-11-12)

Analysis only (no builds, no ART runs). Sources read: BENCHMARKS.md §1–3, §9, §9.8, §10;
results/art-runtime/MILESTONE-1.md; tools/bench-v1.sh; tools/art-runtime/bench-session.sh;
tools/art-runtime/run-exclusive.sh; crates/acos-cli/src/engine/art.rs;
results/art-runtime/evidence/W22-matrix.tsv; scenarios/negative/; acos_protocol::ErrorCode.

## 0. Validity findings (what is wrong today)

| # | Finding | Evidence |
|---|---|---|
| F1 | The authoritative D2 table ("9 PASS / 2 FAIL") measures the **frozen Rust VM**, not ART. | BENCHMARKS.md §9.8 method record `fidelity: native-runtime ... NOT ART-verified`; `tools/bench-v1.sh` never passes `--engine art` |
| F2 | All ART memory/latency numbers are **debug builds, one rep**. | `tools/art-runtime/bench-session.sh:126-127,135` (`target/debug/acos`, `cargo build` no `--release`); MILESTONE-1.md caveats |
| F3 | M11 is not the north-star metric: `60/(M10 wall × M2 GiB)`, verified_tasks=1, M2 = Rust-VM in-process anon+shmem. | `tools/bench-v1.sh:584-594`, BENCHMARKS.md §1 (north-star = tasks ÷ ∫ job GiB·min) |
| F4 | ART vs ACVM compares **different metric types** (cgroup peak + launcher + bwrap vs legacy anon+shmem 60 s settled). | MILESTONE-1.md "171.4 / 155 vs 130.3" |
| F5 | ART is cold-per-task (fresh guest per `ArtSession::start`), legacy M4/M6 are warm; the rows are not labelled cold/warm. | `crates/acos-cli/src/engine/art.rs:239` |
| F6 | The correctness matrix carries **no cost and no negatives**: TSV has name/pass/steps/error only; `scenarios/negative/*` (9 files) not in it. | `results/art-runtime/evidence/W22-matrix.tsv` |
| F7 | Host contamination and noise: concurrent agent builds during §9, §9.8, W12; M5 spread 22 %. | BENCHMARKS.md §9.8 method record, §10 caveats |
| F8 | No benchmark of what agents actually pay: per-verb latency inside one session, round-trips per task, failure classes. | absent |

## Global decisions (apply to every item)

| Decision | Choice | Reason |
|---|---|---|
| Build | **release only** (`cargo build --release -p acos-cli -p acos-art`), harness refuses debug | debug inflates wall and anon memory; no product runs debug |
| Engine | **`--engine art` only** for D2 rows; Rust VM rows move to an appendix labelled `frozen-reference` | ART is the product (STATUS.md) |
| Latency reps | **≥20 samples**, report p50/p95/min/max/spread | BENCHMARKS §2 rule 2 |
| Memory reps | **≥5 fresh-process reps per (mode,N)**, report median + max | one rep has no error bar |
| A/B | **ABBA, ≥3 pairs**, same host window | removes drift |
| Noise | noise = max(spread, 5 %); spread = (p95−p5)/p50; a row with spread >15 % is `noisy` and **cannot PASS** | stops wins hiding in noise |
| Host gate | refuse to start if loadavg1 > 4 or another ART lock holder or MemAvailable < 30 GiB; record loadavg1/5, MemAvailable, `git rev-parse HEAD`, `git status --porcelain | sha256sum`, binary sha256 | contamination (F7) |
| Exclusivity | every run under `tools/art-runtime/run-exclusive.sh --task bench-*` and `tools/guarded.sh` | one ART run at a time; RAM ceiling |
| Cold/warm | report **three states**: `first-run` (empty `ACOS_CACHE_DIR`, 1 sample/rep), `cold-process` (no ACOS process alive, caches warm), `warm` (session/pool already up). Gate cold rows vs legacy cold, warm vs legacy warm | F5; no root for drop_caches, so "cold" = cold process + warm page cache, stated |
| Primary memory accounting | **per-job cgroup `memory.current`**, sampled every 100 ms, reported as **peak** and **integral (GiB·min)**; launcher + bwrap processes outside the cgroup added as their **PSS** only when they are not in the cgroup (move them into the job cgroup instead — item 3) | §1: whole job charged; cgroup sees gralloc memfd pages PSS misses (§10) |
| Secondary accounting | Σ PSS across all sessions (share-aware) for N-scaling; **never summed with cgroup** | §1 rule |
| ACVM-comparison type | per-session cgroup `memory.stat` **anon + shmem**, settled 60 s after first frame, N=1 and N=5 | identical type/state to legacy §3 (130.3 / 78.7 MiB) |
| D2 verdict | **PASS** iff same type/state and ACOS p50 ≤ legacy × (1 − noise) and row not `noisy`; **TIE** within noise = not-pass; **FAIL** otherwise; missing = NOT_MEASURED = not-pass; any false pass in the run ⇒ whole report FAIL | §2 rule 7, "beat on every metric" |

---

## 1. Port the D2 table (M1–M11) to ART, release — `tools/art-runtime/bench-d2.sh`

**What:** new script `tools/art-runtime/bench-d2.sh` (bash wrapper + embedded python like bench-session.sh) producing `results/bench/art-d2-<UTC>.json` and a Markdown table; BENCHMARKS.md gets a new §11 "ART D2" that supersedes §9.8 for the verdict; §9.x stays as Rust-VM history.

**How (metric → ART measurement, all release, counter.apk sha256 recorded):**
- M1 idle core: start `acos-art` guest to `activity-resumed` with no interaction, wait 60 s, read session cgroup `memory.stat` anon+shmem; 5 reps.
- M2 per-session at N=5: launch N=1 and N=5 concurrent guests, settle 60 s, `(Σ anon+shmem(N=5)) / 5` and marginal `(Σ5 − Σ1)/4`; 5 reps.
- M3 cold start → first correct frame: wall from `acos verify` exec to the first frame whose `content_hash` equals the frozen reference for counter (store in `fixtures/reference/counter/art-api34-x86_64/frame0.hash`); 20 samples cold-process + 5 first-run.
- M4 worker create → correct frame: same as M3 but against a pre-booted pool (item 7); until item 7 lands, report `cold-process` value and mark `state: cold` (not comparable ⇒ NOT_MEASURED for D2).
- M5 fresh screenshot: in one warm session, 20 × `screenshot` request → reply with `seq > previous_seq`; measure without PNG write (return raw/shm handle) and with PNG write, both reported; gate on the no-write row (legacy measured retrieval).
- M6 reset → ready: `reset` request → first fresh frame with the initial hash; 20 samples.
- M7 UI tree with text: 20 × `tree` on warm session; gate vs legacy one-shot 538 ms and report vs persistent 4.1 ms.
- M8 action → state: `tap_text Increment` → tree shows count+1 **and** frame hash changes; 20 samples.
- M9 concurrent: N=1/5/10/20 cohorts of `full` mode; per-session cgroup peak, Σ PSS, all sessions pass with **independent state** (each session taps a different count k=1..N, assert each tree shows its own k).
- M10 end-to-end: wall of `acos verify` on the T01–T12 suite (item 2), p50 per task.
- M11: item 3 formula.
**Gate:** report must list all 11 rows with type/state columns; zero NOT_MEASURED by the end of item 7.
**Reason:** today's verdict is about the wrong engine (F1).
**Packet:** "Implement tools/art-runtime/bench-d2.sh: release build, ART engine, M1–M11 exactly as ADVISOR-benchmarks-top10.md §1, global decisions table, JSON schema §5; add BENCHMARKS.md §11; verify with `--quick` (2 reps) then full run under run-exclusive.sh."

## 2. Frozen agent-task suite T01–T12 with seeded-wrong twins — `scenarios/suite/`

**What:** `scenarios/suite/TNN-good.yaml` and `TNN-wrong.yaml` (24 files) + `scenarios/suite/MANIFEST.json` (apk sha256, scenario sha256, oracle, expected verdict). Mapping onto existing fixtures:
T01 build/install → `tools/build-apps.sh` + install counter, assert first frame; T02 state logic → counter; T03 component render → hello-views vs reference PNG (SSIM ≥ 0.98); T04 long text/large font/RTL → widget-gallery with `fontScale=1.3` + `locale=ar` (new launch params; mark `unsupported` until present — counts as not-verified, not skipped); T05 incremental install preserving data → sqlite-crud insert, reinstall, row still present; T06 tap→screen→back → two-activity; T07 type/scroll/submit → form-validation + list-scroll; T08 persistence → sqlite-crud; T09 crash + reset → broken-crash `assert_crash` then reset → counter 0; T10 two concurrent accounts → two todo-list sessions with distinct items, cross-check no leakage; T11 shared state two views → fragments-nav; T12 dialog + provider/intent → dialogs-menus + service-provider. Wrong twins: seed the wrong expected value (pattern of `scenarios/negative/`).
**How:** runner `tools/art-runtime/suite.sh --rounds 3` runs all 24 at N=1 then N=10 (10 concurrent jobs drawn round-robin from the 12 good + 12 wrong); writes `results/bench/suite-<UTC>.json`.
**Metrics:** `verified` = good twins passing; `false_pass` = wrong twins passing (**gate: must be 0**); `false_fail` = good twins failing for harness/infra reasons; `coverage` = verified/12.
**Reason:** M11 numerator must be real agent tasks, and false-pass rate must be measured, not assumed.
**Packet:** "Create scenarios/suite T01–T12 good/wrong twins per ADVISOR §2 mapping, MANIFEST.json with sha256s, and tools/art-runtime/suite.sh emitting verified/false_pass/false_fail; false_pass>0 exits 1."

## 3. True north-star M11 = verified tasks per GiB-minute at N=10

**What:** in `bench-session.sh` `derive()` (line 965) and the sampler (line 427ff, already reads `memory.current` at 511), add the integral; move launcher + bwrap into the job's cgroup scope so nothing is charged outside it.
**Formula (decided):**
`job_gib_min = Σ_samples memory.current_bytes/2^30 × Δt_s/60` per job cgroup, from job submit to cleanup complete (includes install, launch, assert, evidence write, reset, teardown, and any pool idle time attributed equally to the jobs it served).
`M11 = verified_good / Σ_jobs job_gib_min` over the item-2 suite at **N=10 concurrent**, 3 rounds, report median.
If `false_pass > 0`, **M11 := 0**.
**ACVM gate (decided, derived because legacy never published M11):** legacy per-task cost = ((206 MiB core / 10) + 112.1 MiB marginal) / 1024 × (19 s / 60) = **0.0410 GiB·min ⇒ 24.4 tasks/GiB·min** (labelled `legacy-derived-bound`). ACOS must exceed 24.4 × (1 + noise).
Also report the same formula at N=1 and N=5 to show scaling.
**Reason:** today's 937 is one task, Rust VM, wrong memory type (F3).
**Packet:** "Add --integral to bench-session.sh: per-job ∫memory.current dt in GiB·min, launcher/bwrap in job scope, M11 per ADVISOR §3 formula with false-pass zeroing and legacy bound 24.4."

## 4. Release-only, provenance-stamped harness

**What:** `bench-session.sh:126-127,132-135` → `target/release/...`, `cargo build --release`; add `--allow-debug` that stamps every row `profile: debug, not-gateable`. Same in `bench-n.sh`, `bench.sh`. `provenance()` (line 931) adds `profile`, binary sha256, `git status --porcelain` hash, `rustc -V`.
**Gate:** `derive()` refuses to emit a D2 verdict for any row with `profile != release` or dirty tree not recorded.
**Reason:** F2; cheapest fix with the biggest honesty gain. Expected: wall −20–50 %, anon −5–15 % (debug Rust allocs/symbols) — measure, do not assume.
**Packet:** "Switch tools/art-runtime/bench-*.sh to release binaries, add profile/sha/dirty-hash provenance, reject debug rows from verdicts."

## 5. Cost-carrying correctness matrix + one result schema

**What:** the W22 matrix runner writes JSON lines with this schema (also used by items 1–3, 9):
```json
{"run_id":"uuid","utc":"...","git":"sha","dirty_hash":"...","profile":"release","engine":"art",
 "scenario":"path","scenario_sha256":"...","apk_sha256":"...","variant":"good|wrong|negative",
 "expected":"pass|fail","verdict":"pass|fail|error","false_pass":false,
 "error":{"code":"ASSERTION_FAILED","class":"assertion","retryable":false,"hint":"..."},
 "state":"first-run|cold-process|warm","n_concurrent":1,
 "wall_ms":0,"first_frame_ms":0,"steps":[{"kind":"tap","ms":0,"status":"ok"}],
 "mem":{"cgroup_peak_mib":0,"cgroup_gib_min":0,"anon_shmem_settled_mib":0,"pss_mib":0},
 "host":{"loadavg1":0,"mem_available_mib":0}}
```
Include all `scenarios/negative/*.yaml` with `expected: fail`.
**Gate:** `false_pass` count printed first; non-zero exits 1.
**Reason:** every correctness run becomes a cost sample; regressions in cost show up on the corpus, not only on counter (F6).
**Packet:** "Make the scenario matrix runner emit the ADVISOR §5 JSONL schema, include scenarios/negative with expected fail, exit 1 on any false pass."

## 6. Same-type ACVM comparison columns

**What:** `derive()`/`render_table()` (bench-session.sh:965,1050) emit, per (mode,N): (a) cgroup anon+shmem settled 60 s — **the only column compared to legacy §3**; (b) cgroup peak; (c) PSS; (d) gib_min. MILESTONE-1.md's "171/155 vs 130.3" line gets replaced by the (a) value.
**Gate:** the D2 table shows legacy value and metric type in the same row; a type mismatch renders `NOT_COMPARABLE` (not-pass).
**Reason:** F4; current comparison mixes types.
**Packet:** "Add anon+shmem-settled-60s column to bench-session.sh analysis; D2 rows compare only same-type values, else NOT_COMPARABLE."

## 7. Warm ART pool (pre-booted guests) + honest cold/warm rows

**What:** `crates/acos-art` pool: keep **K = min(N, 2) spare guests** booted to "framework ready, no app bound" (zygote-like point before bindApplication); `acos verify --engine art` and `acos serve` take a spare, bind the APK, and the pool refills asynchronously. App reset = discard guest, take a spare, relaunch (fresh process — keeps the "real reset" property from W22). Pool idle memory is charged to M11 (item 3).
**Metrics:** M4 = take-spare → first correct frame (warm), M6 = reset → first initial-hash frame (warm); M3 stays cold-process.
**Gates:** M4 p50 < 1.297 s × (1 − noise); M6 p50 < 748 ms × (1 − noise); and M11 at N=10 with pool ≥ M11 without pool (pool must pay for itself).
**Reason:** agents pay launch latency on every task today (F5); warm rows are what legacy reported.
**Packet:** "Implement K=min(N,2) pre-booted ART guest pool in acos-art (framework-ready, pre-bind), use it in verify/serve, reset = swap to spare; bench M4/M6 warm vs cold and M11 with/without pool."

## 8. Variance, noise and host gate library — `tools/art-runtime/benchlib.py`

**What:** one module imported by bench-d2/bench-session/suite: `stats()` (p5/p50/p95/min/max/spread, bootstrap 95 % CI of p50 with 2000 resamples), `abba(a,b,pairs=3)`, `host_gate()` (loadavg1 ≤ 4, MemAvailable ≥ 30 GiB, no other `art.lock` waiter in `.cache/art-locks/runs.log`), `verdict(acos, legacy)` implementing the global D2 rule.
**Gate:** a run that fails `host_gate()` mid-way is marked `contaminated` and excluded from verdicts.
**Reason:** F7; M5 flipped PASS→FAIL across runs with 22 % spread.
**Packet:** "Create tools/art-runtime/benchlib.py with stats/bootstrap/ABBA/host_gate/verdict per ADVISOR global decisions; wire into bench-session.sh derive()."

## 9. Agent-loop benchmark (what an agent pays per decision) — `tools/art-runtime/bench-agent-loop.sh`

**What:** drive `acos serve` (the MCP/daemon path agents use) with a fixed 20-step script on counter and form-validation: launch → tree → tap → tree → type → key → screenshot → assert → logs → reset, ×2. Record per-verb p50/p95 (20 samples each), **round-trips per task** (requests an agent needs to reach a verdict), bytes returned per verb (tree JSON size, PNG size) since agent tokens/IO cost scale with them.
**Gates:** every verb p95 < 500 ms warm except launch/reset; tree payload ≤ 64 KiB for counter; round-trips per T-task ≤ 12. Report verbs > gate as items on the gap list.
**Reason:** F8; D2 rows are single-shot CLI timings, not the loop agents run.
**Packet:** "Write bench-agent-loop.sh over acos serve: fixed 20-step script, per-verb p50/p95, round-trips, payload bytes, JSON per §5 schema."

## 10. Structured failure taxonomy on the ART engine

**What:** every `BackendError` path in `crates/acos-cli/src/engine/art.rs` (start 239, read_line 430, command_raw 458, shutdown 692) and executor step failures map to `acos_protocol::ErrorCode` plus `class ∈ {assertion, app_crash, unsupported, harness, infra_timeout, host_guard}`, `retryable: bool`, `hint: string` (e.g. W22 `counter-full` bounds mismatch → `assertion`, not retryable, hint "expected [42,368][273,494] got [42,368][310,494]; update expected bounds or check layout"). Timeouts carry the last guest stderr tail (`stderr_tail()` 424).
**Gates:** in the item-5 matrix, `unclassified = 0`; `harness + infra_timeout ≤ 2 %` of runs; retries count as extra job GiB·min in M11.
**Reason:** an agent that cannot tell app-bug from harness-flake wastes whole tasks; this directly lowers cost per verified task.
**Packet:** "Map all ART engine/executor failure paths to ErrorCode + class/retryable/hint per ADVISOR §10; matrix tallies classes; unclassified must be 0."

---

## Order (decided)

1. **4, 8, 5** (S each, validity foundation, ~1 h each) — nothing else is trustworthy without them.
2. **6, 1, 3** — produces the first honest ART D2 + M11 verdict.
3. **2** — suite gives the real M11 numerator and false-pass rate.
4. **10, 9** — agent usability, measured on the same schema.
5. **7** — largest engineering item; needed to turn M4/M6 into gateable warm rows.

Re-publish the D2 verdict only after step 2; until then BENCHMARKS.md must state "ART D2 not yet measured; §9.8 is Rust VM reference".
