# RELGATE5 — v0.4.1 release gate (evidence only)

> Raw logs/JSON referenced below are not tracked (repo layout rule, `tools/verify/check-layout.sh`); they are archived at `~/acos-evidence/v041-relgate5-raw-20261123.tar.zst` on the lead host.

Worktree `/home/Qui/.t3/worktrees/acos/t3-8432ccd8-1d58d051095d020b`, branch
`relgate5` created from `origin/modules` HEAD **`36859df75`** (`36859df75342bb42969da822c076717d96904365`).
Baseline comparison arm: tag **`v0.4.0`** (`8e020cee3`, `8e020cee3b33e3663232b7341ec29ca99b2b2574`).
**No product code, tests, or scenarios were changed by this pass**; this
directory and `results/release/v0.4.1-NOTES-DRAFT.md` are evidence only.
Version was not bumped, no tag was created, `~/.local` was never touched,
`~/Projects/gaten` was never modified, Waydroid was not touched, and the
physical phone was not touched.

**Verdict: NO-SHIP v0.4.1** — **2 gate failures** (Steps 0, 2 [skip hygiene], 3, 4, 5, 6, 8, 9 PASS):

1. **Step 1 FAIL (1 genuine product/test failure):** `acos-services::mproc_pms::query_intent_activities_matches_every_installed_package` (`crates/acos-services/tests/mproc_pms.rs:219-221`) panics with `the generic reply is a placeholder for the materialiser`. Root cause: commit `0f930c5af` (`LAUNCHER2`) upgraded `PmsService::call("queryIntentActivities")` to return a real `ParceledListSlice<ResolveInfo>` instead of `Parcelable::Null` without updating the stale assertion in `mproc_pms.rs:219`. (Also causes `test-crates.sh` for `acos-services` to exit `rc=101` in Step 2, though with `SKIP COUNT: 0`.)
2. **Step 7 FAIL on idle CPU (`ab-compare.py` `rc=1`; lattice/strict latency/memory all PASS):** `cpu_idle_n5_pct` (`2.10% -> 2.49%`, `+0.40 pp`, `+18.9%`) and `cpu_idle_n10_pct` (`3.59% -> 4.61%`, `+1.03 pp`, `+28.6%`) regressed beyond their `±10.00% / 0.20` gate thresholds on 3/3 interleaved reps (every v0.4.1 rep exceeds every v0.4.0 rep, scaling linearly at `~+0.10%` of one core per idle session while `v0.4.0` reproduced RELGATE4's `2.10% / 3.58%` to `0.01 pp` and `wakeups_idle_n{5,10}_per_s` stayed flat). All 4 ADR-0028 lattice metrics, all strict latencies, and all memory metrics passed with 0 regressions.

**Secondary operational finding (for lead follow-up, non-blocking on its own):**
- **`acos-netd` fd-leak onto `art.lock`:** When `acos-netd` daemonizes lazily inside `tools/art-runtime/run-exclusive.sh`, it inherits fd 10 (`/home/Qui/Projects/acos/.cache/art-locks/art.lock`) without `O_CLOEXEC`, holding the `FLOCK` indefinitely after the scenario exits until `acos-netd` is stopped. This caused our initial Step 7 `--exclusive` acquisition to wait until we stopped our worktree's own `acos-netd` PIDs (verified via `/proc/<pid>/exe`).

## Environment (per `BRIEF_COMMON_0316.md`)

- `source tools/agent-env.sh` → `.cache/env.sh`; `ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts`;
  `.cache/{art,gaten,boot/android-34,webview,bionic-arm64,qemu-user,flutter-1,corpus,rn-0.81.4}`,
  `.cache/arm64-guard/libacos_seccomp_guard.so`, and `libexec/pasta` symlinked
  to the main checkout's assets.
- `tools/fetch-camerax-deps.sh` fetched and sha256-verified 37 CameraX 1.3.4
  Maven artifacts into `.cache/maven-camerax` (`RC=0`, `--verify-only` `RC=0`).
- `ACOS_BOOT_DIR`, `XDG_RUNTIME_DIR`, `ACOS_HOME`, `TMPDIR` unset throughout
  (and `env -u ACOS_HOME` from an empty `/tmp` working directory for Step 8).
- Private `ACOS_RUNTIME_DIR` per step.
- Cargo target dirs `.cache/rg5-target` (RC) and `.cache/rg5-v040/target`
  (`v0.4.0` arm), both deleted at the end.
- Every ART and emulator run went through `tools/art-runtime/run-exclusive.sh`
  + `tools/guarded.sh`; all timing acquisitions (Steps 6 and 7) took
  `--exclusive`. No other agents ran concurrently during timing.
- Host: AMD Ryzen 9 9950X, 60 GiB RAM (`61,908 MiB`).
- Peak host RAM observed: **33.2 %** (`20,550 / 61,908 MiB`, during Step 6 live
  emulator `n1-warm-tree`; ACOS A/B peak was **24.6 %**). Disk remained
  **83 %** (`156–161 GiB` free, under the 85 % pause threshold).

## Summary

| Step | Gate | Result | Evidence |
|---:|---|---|---|
| 0 | Fixtures (`build-apps`, `dogfood`, `negative`, `webview-provider`, `rn-hello debug`, `fetch-camerax-deps`) + `acos doctor` | **PASS** (73 APKs; 37 CameraX deps sha256-verified; `acos doctor` 22/22 `ok`, 0 failures, incl. `pasta`, `metro`, `gles`) | `00-fixtures/` |
| 1 | `cargo build --release --workspace --offline` + `cargo test --workspace --release --no-fail-fast` | **FAIL** (**1 genuine test/product failure**: `acos-services::mproc_pms::query_intent_activities_matches_every_installed_package`; plus 1 known runner limitation `spike_net_boundary`, 9 documented opt-ins passing when enabled, 3 shared-cache `runs/` env artifacts passing with clean `ACOS_ART_WARM_DATA`, 2 missing-fixture reruns passing after seeding `widget-gallery.apk`) | `01-tests/` |
| 2 | `tools/audit-silent-skips.sh` + 12 per-crate gates (`test-crates.sh`) | **PASS on skip hygiene** (`SILENT-SKIP SITES: 0`; `SKIP COUNT: 0` on 12/12 crates; 10/12 `rc=0`, `acos-art` `rc=101` on runner-limited `spike_net_boundary`, `acos-services` `rc=101` on Step 1's `mproc_pms`) | `02-hygiene/` |
| 3 | `tools/run-matrix.sh --no-build --profile release` | **PASS — VALID** (`total_runs 43` [1 pos + 42 neg], `false_pass 0`, `unclassified 0`, `negative_correct 42/42`, `infra_timeout 0`, 5/5 gates `true`) | `03-matrix/` |
| 4 | 20 Gaten rows + `gaten-word-lookup`, fresh dir per row | **PASS** (`total 21  ok 21  bad 0`, `fresh-check` `rows 21  findings 0`) | `04-gaten/` |
| 5 | Conformance kit (`module_kit` 22/22 + `modules_doc` 2/2) + 48 positive module proofs (incl. 10 new v0.4.1) + `metro-reload` 5/5 + 9 new v0.4.1 negatives + `metro-reload-full` best-effort | **PASS** (kit 24/24; 47/48 positives first pass + `sysui1-shade` 5/5 on rerun [retryable `NO_FRESH_FRAME` launch flake]; `metro-reload` 5/5, 0 orphan `node`; 9/9 new negatives FAIL, `false_pass 0`; `metro-reload-full` best-effort 3/3) | `05-modules/` |
| 6 | Stock emulator comparison (`emu_gate.py` incl. `install` phase + live `n1-warm-tree`) | **PASS** (`12` metrics compared, **0 worse** for ACOS; 0 `qemu-system` processes left; peak RAM 33.2 %) | `06-emubench/` |
| 7 | Interleaved A/B `v0.4.0` (`8e020cee3`) vs `v0.4.1 RC` (`36859df75`), 3 reps `--exclusive` | **FAIL (idle CPU)** — **0 lattice regressions > 5 %** (all 4 faster by −0.4 % to −2.0 %), all strict latency & memory within-noise; **2 old-gate regressions**: `cpu_idle_n5_pct` `+18.9 %` (`2.10 → 2.49 %`) and `cpu_idle_n10_pct` `+28.6 %` (`3.59 → 4.61 %`), reproducible 3/3 reps (`ab-compare.py` `rc=1`) | `07-ab/` |
| 8 | Installed-prefix smoke (`tools/release/install.sh` into private temp prefix, run from empty `/tmp` cwd with `env -u ACOS_HOME`) | **PASS** (`doctor` `ok: true` with `pasta`, `gles`, `install_webview`; `share/doc/acos/pasta/{COPYING,BSD-3-Clause.txt,SOURCE.md}` & `bin/acos-webview` present and in `MANIFEST.txt`; `acos verify v2-art-counter` PASS; `mcp_smoke.py` 18/18 PASS) | `08-install/` |
| 9 | Shared WebView broker upgrade check (`v0.4.0` idle broker on canonical `broker.sock` → RC `webview-probe` 5×) | **PASS** (`webview-probe` **5/5 PASS** in 1–2 s each; `v0.4.0` broker exited cleanly on `BUILD_MISMATCH`; RC broker took over canonical `broker.sock` with no fallback socket; 0 lingering brokers after stop) | `09-wvbroker/` |

---

## Step 0 — Fixtures & `acos doctor` — PASS

`00-fixtures/`:

- `tools/fetch-camerax-deps.sh` → `RC=0` (37 artifacts fetched and sha256-verified into `.cache/maven-camerax`; `--verify-only` also `RC=0`, `00-fixtures/00-fetch-camerax.log`).
- Seeded large prebuilt/legacy APKs (`flutter-*`, `rn-hello*`, `hostile-*`, `native-hostile*`, `widget-gallery.apk`, `acos-counter.apk`) via `cp -al`, then built all workspace APKs cleanly in this worktree:
  - `tools/build-apps.sh` (incl. `camerax-probe.apk`) → `RC=0` (`00-fixtures/01-build-apps.log`)
  - `tools/build-dogfood-apps.sh` → `RC=0` (`00-fixtures/02-build-dogfood.log`)
  - `tools/build-negative-apps.sh` → `RC=0` (`00-fixtures/03-build-negative.log`)
  - `tools/build-webview-provider.sh` → `RC=0` (`00-fixtures/04-build-webview-provider.log`)
  - `tools/build-rn-hello.sh debug` → `RC=0` (`rn-debug.apk`, `00-fixtures/05-build-rn-debug.log`)
  - Total: **73 top-level APKs** under `fixtures/build/`.
- `acos doctor` (`00-fixtures/06-doctor.log`, `00-fixtures/06-doctor.json`): **healthy (0 failure(s))**, **22/22 `ok`**, including:
  - `[ok] pasta 2026_10_02.cba3570`
  - `[ok] metro node v24.19.0`
  - `[ok] gles angle-swiftshader`

## Step 1 — Release build + workspace test suite — **FAIL** (1 genuine test/product failure)

`01-tests/`:

- `cargo build --release --workspace --offline` → `CARGO_RC=0`, 68 s (`01-tests/01-build-release.log`).
- `cargo test --workspace --release --no-fail-fast` (with `unset TMPDIR`), split per crate group (`01-tests/run-tests.sh`, `01-tests/driver.log`):

| log | crates | passed | failed | ignored |
|---|---|---:|---:|---:|
| `02-test-groupA.log` | 19 non-ART crates | 803 | 1 | 1 |
| `03-test-groupB.log` | `acos-vm`, `acos-runtime`, `acos-sandbox` | 352 | 1 | 10 |
| `04-test-acos-verify.log` | `acos-verify` | 171 | 0 | 0 |
| `05-test-acos-art.log` | `acos-art` | 260 | 4 | 1 |
| `06-test-acos-cli.log` | `acos-cli` | 315 | 1 | 15 |
| `07-test-acos-services.log` | `acos-services` | 772 | 1 | 2 |
| `08-test-acos-mcp.log` | `acos-mcp` | 169 | 0 | 0 |
| `09-test-acos-jni.log` | `acos-jni` | 71 | 0 | 0 |
| `10-test-acos-webview.log` | `acos-webview` | 51 | 8 | 0 |
| **total** | **29 crates** | **2964** | **16** | **29** |

Every single one of the 16 failures was re-run alone and classified:

| # | failed test(s) | count | alone rerun | classification | evidence |
|---|---|---:|---|---|---|
| 1 | **`acos-services::mproc_pms::query_intent_activities_matches_every_installed_package`** | 1 | **FAIL** (`panicked at crates/acos-services/tests/mproc_pms.rs:219: the generic reply is a placeholder for the materialiser`) | **GENUINE PRODUCT/TEST FAILURE (NO-SHIP BLOCKER)** — `0f930c5af` (`LAUNCHER2`) upgraded `PmsService::call("queryIntentActivities")` from returning `Parcelable::Null` to returning real `ParceledListSlice<ResolveInfo>` without updating `mproc_pms.rs:219` | `01-tests/17-failure-services-mproc-pms-alone.log` |
| 2 | `acos-art::spike_net_boundary::guest_cannot_change_the_lan_drop_rules` | 1 | fails (`bwrap: setting up uid map: Operation not permitted`) | **known runner limitation** (`NoNewPrivs: 1` / `Seccomp: 2` under this sandbox; lead re-runs in normal shell) | `01-tests/14-failure-spike-net-alone.log` |
| 3 | `acos-cli::sandbox_s8::hostile_live_run_is_gated_and_recorded_separately` | 1 | **PASS 7/7** with `ACOS_S8_LIVE_HOSTILE=1` | **documented opt-in** (`tools/test-prereq-optouts.txt`) | `01-tests/15-failure-cli-s8-optin-alone.log` |
| 4 | `acos-webview` heavy Chromium integration tests | 8 | **PASS 59/59** crate total with `ACOS_WEBVIEW_TEST=1` (restored tracked `results/handoff/{WVB,WVFAIR}/` JSONs) | **documented opt-in** (`tools/test-prereq-optouts.txt`) | `01-tests/16-failure-webview-optin-alone.log` |
| 5 | `acos-art::cleanup` (3 tests) | 3 | **PASS 9/9** with clean `ACOS_ART_WARM_DATA` excluding pre-existing `< 1800s`-old dead run dirs | **shared-cache env artifact** (saw 7 recent dead run dirs copied from main checkout's `.cache/art/data/runs`) | `01-tests/13-failure-art-cleanup-alone.log` |
| 6 | `acos-protocol` (1) & `acos-vm` (1) fixture tests | 2 | **PASS 1/1** each after seeding `widget-gallery.apk` | **missing legacy fixture** (seeded via `cp -al`) | `01-tests/11-failure-protocol-alone.log`, `12-failure-vm-alone.log` |

**Genuine product/test failures: 1 (`mproc_pms`).**

## Step 2 — Silent-skip audit + 12 per-crate gates — PASS on skip hygiene

`02-hygiene/`:

- `tools/audit-silent-skips.sh` → `SILENT-SKIP SITES: 0`, `AUDIT_RC=0` (`02-hygiene/01-audit-silent-skips.log`).
- All 12 per-crate gates via `tools/art-runtime/test-crates.sh` (`--release --no-fail-fast`, `unset TMPDIR`): **`SKIP COUNT: 0` on 12/12 crates** (`02-hygiene/02-per-crate-summary.log`, `02-hygiene/gate-<crate>.log`):
  - 10/12 exited `rc=0` (`acos-cli`, `acos-dex`, `acos-framework-natives`, `acos-jni`, `acos-mcp`, `acos-protocol`, `acos-render`, `acos-runtime`, `acos-vm`, `acos-webview`).
  - `acos-art` exited `rc=101` solely on the known runner limitation `spike_net_boundary::guest_cannot_change_the_lan_drop_rules` (`SKIP COUNT: 0`).
  - `acos-services` exited `rc=101` solely on Step 1's `mproc_pms::query_intent_activities_matches_every_installed_package` (`SKIP COUNT: 0`).

## Step 3 — Matrix (`tools/run-matrix.sh --no-build --profile release`) — PASS (VALID)

`03-matrix/run-matrix.log`, `03-matrix/matrix.json`, `03-matrix/matrix.tsv`:

```json
{
  "classes": {"assertion": 40, "pass": 1, "step_timeout": 2},
  "errors": 0, "failed": 42, "false_fail": 0, "false_pass": 0,
  "gates": {
    "false_pass_zero": true,
    "harness_timeout_within_2pct": true,
    "negative_all_correct": true,
    "no_infra_timeout": true,
    "unclassified_zero": true
  },
  "harness_or_timeout": 0, "infra_timeout": 0,
  "negative_correct": 42, "negative_expect_error_mismatch": [],
  "negative_runs": 42, "passed": 1, "skipped_needs_env": [],
  "step_timeout": 2, "total_runs": 43, "unclassified": 0,
  "verdict_eligible": true
}
```

Comparison vs `v0.4.0` (RELGATE4):

| Metric | v0.4.0 (RELGATE4) | v0.4.1 RC (RELGATE5) | Change |
|---|---:|---:|---|
| Total rows | 34 (1 pos + 33 neg) | **43** (1 pos + 42 neg) | +9 new v0.4.1 negatives |
| `verdict_eligible` | `true` | **`true`** | VALID |
| `false_pass` | 0 | **0** | PASS |
| `unclassified` | 0 | **0** | PASS |
| `negative_correct` | 33 / 33 | **42 / 42** | all negatives match |
| `infra_timeout` | 0 | **0** | PASS |
| Gates passed | 5 / 5 | **5 / 5** | all gates `true` |

## Step 4 — 20 Gaten rows + `gaten-word-lookup` — PASS

`04-gaten/` (`05-gaten-table.md`, `05-gaten-classify.log`, `05-gaten-fresh-check.log`, `table.json`):

- All **20 Gaten rows** (10 positive `pass` + 10 negative `fail`/`ASSERTION_FAILED`) plus **`scenarios/gaten-word-lookup.yaml`** run into fresh artifact directories: **`total 21  ok 21  bad 0`**.
- Fresh-dir integrity (`fresh-check.py`, run before pruning raw `frames/`/`tree/`/`art-frames/`/`launcher.stderr.log`): **`rows 21  findings 0`**.

## Step 5 — Module conformance kit + module proofs (incl. v0.4.1 new) — PASS

`05-modules/`:

- **Conformance kit & docs** (`05-modules/01-conformance-kit.log`): **`KIT_RC=0`** — `module_kit` **22/22** (covering all **28** registered modules, incl. new `hw.camera`, `multiwindow`, `system.launcher`, `system.settings`) and `modules_doc` **2/2**.
- **48 positive module proof scenarios** (`05-modules/scenarios-list.txt`, `05-modules/driver.log`, `05-modules/rows.jsonl`): 38 v0.4.0 proofs + 10 new v0.4.1 proofs:
  - All 10 new v0.4.1 proofs passed on first run: `camera-probe` (16/16, 6 s), `camera-off` (11/11, 4 s), `camera-capture` (14/14, 4 s), `camera-scripted` (17/17, 3 s), `camera-jpeg` (8/8, 2 s), `camera-preview` (11/11, 2 s), `camerax-probe` (21/21, 6 s), `multiwindow-split` (17/17, 1 s), `launcher-home` (21/21, 4 s), `settings-intents` (25/25, 11 s).
  - `webview-probe` (20/20, 1 s) and `webview-fair` (14/14, 12 s) passed on first run.
  - 47/48 passed on first run; 1 initial retryable `NO_FRESH_FRAME` launch flake on `sysui1-shade` passed **5/5** on immediate rerun (`05-modules/sysui1-shade-rerun5.log`, 1–2 s each).
- **`metro-reload` (Fast Refresh)**: **5/5 PASS** (12/12 steps, 5–6 s each, 0 orphan `node`/Metro processes).
- **9 new v0.4.1 negative scenarios**: **9/9 FAIL as expected (`FALSE_PASS=0`)** (`camera-capture-wrong-checksum`, `camera-jpeg-wrong-size`, `camera-preview-wrong-color`, `camera-scripted-wrong-checksum`, `camerax-probe-wrong`, `launcher-home-wrong`, `metro-reload-wrong-text`, `multiwindow-split-wrong`, `settings-intents-wrong`).
- **`metro-reload-full` (best-effort)**: **3/3 PASS** (12/12 steps, 6 s each).

## Step 6 — Stock emulator comparison (`emu_gate.py`) — PASS (0 worse)

`06-emubench/`:

- **ACOS RC side** (`06-emubench/acos-rc-gate.json`): Step 7's 3-rep `v0.4.1 RC` bundle (`07-ab/v041.json`) merged with a fresh RC `install` phase (`06-emubench/acos-install-phase.json`, `ok=True`):
  - `m9_apk_ready_ms`: **10.0 ms** p50 (n=5, samples `[0.0, 10.0, 10.0, 10.0, 10.0]`).
  - `m9_apk_ready_cold_ms`: **3700.0 ms** (n=1, cold scratch).
- **Emulator side** (`06-emubench/emubench-relgate5.json`, 35 rows): fresh live emulator run of `n1-warm-tree` via `tools/art-runtime/emubench/emu_bench.py` under `run-exclusive.sh --exclusive -- guarded.sh --mem 24G --time 540` (`06-emubench/01-n1-warm-tree.log`, `06-emubench/phase-n1-warm-tree.json`: `install_ms` `2092.73 ms`, `warm_launch_ms` p50 `366.24 ms`, `warm_launch_tree_ms` p50 `5050.35 ms`, peak host RAM **33.2 %** / `20,550 MiB`), merged with baseline phases `n1-cold`, `n1-warm`, `n2`, `n4` from `results/handoff/EMUBENCH/raw/`.
- **Emulator shutdown verified**: `stop emulator-5554 (1.5s, qemu left=None)`, `PASS no qemu-system process is running`.
- **Gate verdict** (`06-emubench/gate.{md,json,txt}`; also verified against `results/bench/emubench-2026-11-18.json` in `gate-vs-20261118.*`):
  **`GATE_RC=0` — `12 metric(s) compared, 0 worse for ACOS, 0 worse beyond the 5 % noise floor`**:

| Metric | ACOS (RC) | Emulator | Unit | Better | Delta | Verdict |
|---|---:|---:|---|---|---:|---|
| Cold boot -> first app frame | 4120 | 13632 | ms | ACOS | −69.8 % | better |
| Warm launch -> readable settled UI tree | 288.1 | 5050 | ms | ACOS | −94.3 % | better |
| Tap -> settled UI result (counter) | 950.3 | 1931 | ms | ACOS | −50.8 % | better |
| Tap -> settled UI result (form) | 930.2 | 1946 | ms | ACOS | −52.2 % | better |
| UI tree read with text | 41.4 | 1928 | ms | ACOS | −97.9 % | better |
| Fresh PNG screenshot | 11.7 | 527.1 | ms | ACOS | −97.8 % | better |
| Install -> APK ready to launch | 10.0 | 1359 | ms | ACOS | −99.3 % | better |
| Reset -> first correct screen | 331.3 | 530.8 | ms | ACOS | −37.6 % | better |
| Instance memory, PSS | 239.3 | 3578.0 | MiB | ACOS | −93.3 % | better |
| Instance memory, anon+shmem | 59.4 | 3531.6 | MiB | ACOS | −98.3 % | better |
| Idle CPU | 0.5 | 13.1 | % of one core | ACOS | −95.8 % | better |
| Verified tasks / GiB-minute | 62.84 | 2.54 | tasks/GiB-min | ACOS | −96.0 % | better |
| Warm launch -> launcher-frame change (frame only, not gated) | 288.1 | 286.1 | ms | — | — | REFERENCE (not gated) |

## Step 7 — Interleaved A/B `v0.4.0` vs `v0.4.1 RC` — **FAIL** (idle CPU regression; lattice & memory PASS)

`07-ab/` (`arms.txt`, `driver.log`, `run-{v040,v041}/rep-{1,2,3}/{latency,cold,memory}.json`, `v040.{json,md}`, `v041.{json,md}`, `ab-compare.{txt,json}`):

| arm | source | binary sha256 |
|---|---|---|
| A | tag `v0.4.0` (`8e020cee3`), `git archive` → `.cache/rg5-v040/src`, own target dir | `3af2b0022f17b37c…` |
| B | `v0.4.1 RC` (`origin/modules` `36859df75`), `.cache/rg5-target` | `236db1a5407f632a…` |

Method (identical to RELGATE4): interleaved per rep — A `latency,cold`, A `memory`, B `latency,cold`, B `memory`; `--reps 20 --fast-reps 40 --p11-reps 3 --animations on`; one `--exclusive` lock acquisition per group behind `tools/guarded.sh`. All 12 groups completed `ok=True`, `rc=0`. Every latency and memory phase passed the host gate (`loadavg1` `0.33–1.11` for latency/memory; `cold` carried the same residual tail `4.17–9.99` on both arms).

**ADR-0028 Lattice metrics — 0 regressions > 5 % (all slightly faster):**

| metric | v0.4.0 trimmed | v0.4.1 RC trimmed | delta | delta % | first slot A → B | verdict |
|---|---:|---:|---:|---:|---:|---|
| M4 warm launch (fast) | 148.84 | 145.86 | −2.98 ms | −2.0 % | 34 % → 47 % | **within-noise** (`SLOT-SHIFT +12 pp`, faster) |
| M6 reset (fast) | 182.49 | 180.89 | −1.60 ms | −0.9 % | 0 % → 2 % | **within-noise** |
| M8 tap counter (fast) | 25.04 | 24.79 | −0.25 ms | −1.0 % | 49 % → 50 % | **within-noise** |
| M8 tap form (fast) | 28.64 | 28.53 | −0.12 ms | −0.4 % | 100 % → 100 % | **within-noise** |

**Strict latency & memory — 0 regressions (all within-noise):**

- Strict latency: M3 cold `4096.39 → 4116.50 ms` (+0.5 %), M4 strict `288.49 → 288.12 ms` (−0.1 %), M5 PNG `11.81 → 11.69 ms` (−1.0 %), M6 strict `328.16 → 332.07 ms` (+1.2 %), M7 strict `41.54 → 41.48 ms` (−0.1 %), M8 counter strict `945.92 → 950.02 ms` (+0.4 %), M8 form strict `927.44 → 930.32 ms` (+0.3 %), teardown `21.16 → 20.52 ms` (−3.0 %).
- Memory: `m1_idle_mem_mib` `58.20 → 59.44 MiB` (+2.1 %), `m1_idle_pss_mib` `237.98 → 239.28 MiB` (+0.5 %), `m2_per_session_anon_shmem_n1/n5/n10` `+2.2 % / +0.5 % / −0.2 %`, `m2_per_session_pss_n1/n5/n10` `+0.7 % / +0.6 % / −0.2 %`.
- Active CPU & idle wakeups: `cpu_active_tap_strict_pct` `44.29 → 41.27` (−6.8 %); `wakeups_idle_n1/n5/n10` `1.20→1.20` (0.0 %), `2.08→2.06` (−0.8 %), `3.11→3.12` (+0.1 %).

**Idle CPU — REGRESSION (`ab-compare.py` `rc=1`):**

| metric | v0.4.0 p50 (per-rep) | v0.4.1 RC p50 (per-rep) | delta | delta % | threshold | verdict |
|---|---:|---:|---:|---:|---:|---|
| `cpu_idle_n1_pct` | 0.50 (`[0.498, 0.498, 0.515]`) | 0.55 (`[0.548, 0.548, 0.631]`) | +0.05 | +10.0 % | ±31 % / 0.20 | within-noise |
| **`cpu_idle_n5_pct`** | **2.10** (`[1.916, 2.128, 2.095]`) | **2.49** (`[2.377, 2.491, 2.556]`) | **+0.40** | **+18.9 %** | ±10.00 % / 0.20 | **REGRESSION** |
| **`cpu_idle_n10_pct`** | **3.59** (`[3.588, 3.845, 3.535]`) | **4.61** (`[4.476, 4.613, 4.791]`) | **+1.03** | **+28.6 %** | ±10.00 % / 0.20 | **REGRESSION** |

Note: `v0.4.0` reproduced its RELGATE4 baseline (`2.10 %` at N=5, `3.58 %` at N=10) to within `0.01 pp`, and every `v0.4.1 RC` rep sits strictly above every `v0.4.0` rep, growing linearly at `~+0.10 %` of one core per idle session while voluntary wakeup rate is unchanged (`wakeups_idle_n10_per_s` `3.11 → 3.12`).

## Step 8 — Installed-prefix smoke — PASS

Private prefix `$WT/.cache/rg5-prefix` (`~/.local` never touched), installed via `tools/release/install.sh --prefix .cache/rg5-prefix --from-target .cache/rg5-target` (`08-install/01-install.log`, `INSTALL_RC=0`) and tested from an empty `/tmp` working directory with `env -u ACOS_HOME`:

| check | result | evidence |
|---|---|---|
| `acos --version` | `acos 0.4.0` (expected; version not bumped yet) | `08-install/02-version-doctor.log` |
| `acos doctor --json` | **PASS** — `ok: true`, 0 failures (28 `ok` + 2 expected checkout-only `warn`: `metro` without `ACOS_METRO_NODE_MODULES`, `apk_append` fixture-only); `pasta`, `gles`, `install_webview` all `ok` | `08-install/02-doctor.json`, `02-version-doctor.log` |
| `bin/acos-webview` & `com.android.webview.apk` | **PASS** — both present and listed in `MANIFEST.txt` | `08-install/02-version-doctor.log` |
| `pasta` binary + licence & source docs | **PASS** — `libexec/pasta` (1,548,232 B, sha256 `314c37e8…edf60`) + `share/doc/acos/pasta/{COPYING,BSD-3-Clause.txt,SOURCE.md}` all present and listed in `MANIFEST.txt` | `08-install/02-version-doctor.log` |
| `tools/fetch-camerax-deps.sh` | **Not shipped** in installed prefix (checkout-side fixture-build script for `fixtures/apps/camerax-probe`, not a runtime asset; matches `docs/INSTALL.md`) | `08-install/02-version-doctor.log` |
| `acos verify v2-art-counter.yaml --engine art --json` | **PASS** — `status: pass`, `ok: true`, `fidelity: art-reference`, 3246 ms | `08-install/03-prefix-verify-counter.json` |
| `tools/release/mcp_smoke.py --mode counter` | **PASS** — **18/18** checks `ok: true`, `{"smoke": "ok"}`, private ART daemon torn down with `survivors: []` | `08-install/04-mcp-smoke.jsonl`, `04-mcp-smoke.err` |

Private prefix deleted immediately after Step 8.

## Step 9 — Shared WebView broker upgrade check (ADR-0031 / WVBROKER1) — PASS

`09-wvbroker/` (`01-upgrade-check.log`, `v040-broker.log`, `rc-broker.log`, `r{1..5}.{json,err}`, `r{1..5}/`):

- Started `v0.4.0` (`8e020cee3`) `acos-webview --shared` (PID `529232`, sha256 `a2568df6…`) on canonical `/tmp/acos-wv-1000/broker.sock` (with `XDG_RUNTIME_DIR` unset, leaving unrelated PID `132225` on `/run/user/1000/acos-wv/broker.sock` untouched).
- Ran RC `scenarios/webview-probe.yaml` **5 times in a row** (`r1..r5`):
  - **5/5 PASS** (`r1`: 2 s, `r2`: 1 s, `r3`: 2 s, `r4`: 1 s, `r5`: 1 s).
  - On `r1`, `v0.4.0` broker PID `529232` logged `BUILD_MISMATCH ... (0 session(s) attached; exiting, nothing is left to serve)` and exited cleanly before `r1` completed.
  - RC broker PID `529317` (sha256 `d8a0ed65…`) took over canonical `/tmp/acos-wv-1000/broker.sock` with **no** build-private fallback socket (`broker-*.sock` absent).
- Cleanly stopped our Step 9 broker PID `529317` (verified via `/proc/529317/exe`); verified `remaining_our_brokers=0` and unrelated PID `132225` untouched.

## Housekeeping

- Drafted user-facing release notes in `results/release/v0.4.1-NOTES-DRAFT.md`.
- Deleted temporary build/runtime/fixture directories before final commit:
  `.cache/rg5-target`, `.cache/rg5-v040`, `.cache/rg5-prefix`, `.cache/rg5b`,
  `.cache/rg5-ab.pids`, `.cache/rt*`, `.cache/wvrun`, `.cache/avd`,
  `.cache/emu-run`, `fixtures/build`.
- Total size of `results/release/v0.4.1-gate/relgate5/`: **17 MB** (< 20 MB limit).
