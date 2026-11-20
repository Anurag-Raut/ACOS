# RELGATE4 — v0.4.0 release gate (evidence only)

Worktree `/home/Qui/.t3/worktrees/acos/t3-f4bcc4d4-5c49b0ef4f83b639`, branch
`relgate4` created from `origin/modules` HEAD **`afad66858`** with
`relnotes2h` (`ba8703acc`, docs-only draft refresh) merged at `548926461`.
**No product code, tests, or scenarios were changed by this pass**; this
directory is evidence only. Version was not bumped, no tag was created,
`~/.local` was never touched, `~/Projects/gaten` was never modified, and the
physical phone was not touched.

**Verdict: NO-SHIP v0.4.0 — Step 8 FAIL: `tools/release/install.sh` ships the
GPL-2.0-or-later `libexec/pasta` binary into the installed prefix without a
licence file or source note (Steps 0–7 all PASS; fix is a separate small
installer task).**

## Environment (per `BRIEF_COMMON_0316.md`)

- `source tools/agent-env.sh` → `.cache/env.sh`; `ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts`;
  `.cache/{art,gaten,boot/android-34,webview,bionic-arm64,qemu-user,flutter-1}`
  and `libexec/pasta` symlinked to the main checkout's assets.
- `ACOS_BOOT_DIR`, `XDG_RUNTIME_DIR`, `ACOS_HOME` unset throughout (and
  `env -u ACOS_HOME` for the installed-prefix smoke).
- Private `ACOS_RUNTIME_DIR` + `ACOS_SESSION_SOCKET` per step under
  `$WT/.cache/rt*`.
- Cargo target dirs `.cache/rg4-target` (RC) and `.cache/rg4-v030/target`
  (v0.3.0 arm), both deleted at the end.
- Every ART and emulator run went through `tools/art-runtime/run-exclusive.sh`
  + `tools/guarded.sh`; all timing acquisitions (Steps 6 and 7) took
  `--exclusive`.
- Host: AMD Ryzen 9 9950X, 60 GiB RAM (`61,908 MiB`), kernel `7.2.11-arch1-1`.
- Peak host RAM observed: **30.6 %** (`18,971 / 61,908 MiB`, during Step 6
  live emulator `n1-warm-tree`; ACOS A/B peak was **26.0 %**). Disk remained
  **82–83 %** (`161–163 GiB` free, under the 86 % pause threshold).

## Summary

| Step | Gate | Result | Evidence |
|---:|---|---|---|
| 0 | Fixtures + dogfood + negative + WebView provider + `acos doctor` | **PASS** | `00-fixtures/` |
| 1 | `cargo build --release --workspace --offline` + `cargo test --workspace --release --no-fail-fast` | **PASS** (0 product failures; 1 known runner limitation, 17 runner-`TMPDIR` `SUN_LEN`, 9 documented opt-ins) | `01-tests/` |
| 2 | `tools/audit-silent-skips.sh` + 12 per-crate gates (`test-crates.sh`) | **PASS** (`SILENT-SKIP SITES: 0`; `SKIP COUNT: 0` on 12/12 crates) | `02-hygiene/` |
| 3 | `tools/run-matrix.sh --no-build --profile release` | **PASS — VALID** (`false_pass 0`, `unclassified 0`, `negative_correct 33/33`, `infra_timeout 0`, 5/5 gates `true`) | `03-matrix/` |
| 4 | 20 Gaten rows + `gaten-word-lookup` (Back & Close), fresh dir per row | **PASS** (`21/21` as declared, `fresh-check` `findings 0`) | `04-gaten/` |
| 5 | Conformance kit (`module_kit` + `modules_doc`) + 38 module proof scenarios (incl. `net-https-proof` & `modfill-wifi-network-real`) | **PASS** (kit 22/22; scenarios 37/38 first pass + `location-probe` 5/5 on rerun — known intermittent launch flake) | `05-modules/` |
| 6 | Stock emulator comparison (`emu_gate.py` incl. `install` phase + live `n1-warm-tree`) | **PASS** (`12` metrics compared, **0 worse** for ACOS; 0 `qemu-system` processes left) | `06-emubench/` |
| 7 | Interleaved A/B v0.3.0 (`02421e17b`) vs RC (`afad66858`), 3 reps `--exclusive` | **PASS** (**0 lattice regressions > 5 %**; M4 fast trimmed +1.6 % with −21 pp slot shift that trips old non-lattice p50 at +9.0 %; CPU & memory all within-noise) | `07-ab/` |
| 8 | Installed-prefix smoke (`tools/release/install.sh` into private temp prefix) | **FAIL** (runtime checks all PASS, but `libexec/pasta` ships with **no licence file and no source note** in the prefix) | `08-install/` |

---

## Step 0 — Fixtures & `acos doctor` — PASS

`00-fixtures/`:

- Seeded large prebuilt assets (`flutter`, `rn`, `rn-hermes`, `hostile`,
  `dexprogs`, `jni`) from the main checkout's `fixtures/build/` via `cp -al`,
  then built all workspace APKs cleanly in this worktree:
  - `tools/build-apps.sh` → `RC=0` (`00-fixtures/00-build-apps.log`)
  - `tools/build-dogfood-apps.sh` → `RC=0` (`00-fixtures/00-build-dogfood.log`)
  - `tools/build-negative-apps.sh` → `RC=0` (`00-fixtures/00-build-negative.log`)
  - `tools/build-webview-provider.sh` → `RC=0` (`00-fixtures/00-build-webview-provider.log`)
  - Total: **67 APKs** under `fixtures/build/`.
- `acos doctor` (`00-fixtures/01-doctor.log`): **healthy (0 failure(s))**,
  `[ok] pasta 2026_10_02.cba3570 (<WT>/libexec/pasta [runtime_root])`
  (sha256 `314c37e878bd6ffe43200735800d5efd4859d81d46b0450358a103c36c0edf60`),
  `[ok] gles angle-swiftshader`.

## Step 1 — Release build + workspace test suite — PASS (0 product failures)

`01-tests/`:

- `cargo build --release --workspace --offline` → `CARGO_RC=0`, 67 s
  (`01-tests/01-build-release.log`).
- `cargo test --workspace --release --no-fail-fast`, split per crate group so no
  ART-lock acquisition ran long (`01-tests/run-tests.sh`, `01-tests/driver.log`):

| log | crates | passed | failed | ignored |
|---|---|---:|---:|---:|
| `02-test-groupA.log` | 19 non-ART crates | 800 | 0 | 1 |
| `03-test-groupB.log` | `acos-vm`, `acos-runtime`, `acos-sandbox` | 353 | 0 | 10 |
| `04-test-acos-verify.log` | `acos-verify` | 163 | 0 | 0 |
| `05-test-acos-art.log` | `acos-art` | 261 | 1 | 1 |
| `06-test-acos-cli.log` | `acos-cli` | 306 | 8 | 15 |
| `07-test-acos-services.log` | `acos-services` | 741 | 0 | 2 |
| `08-test-acos-mcp.log` | `acos-mcp` | 159 | 10 | 0 |
| `09-test-acos-jni.log` | `acos-jni` | 71 | 0 | 0 |
| `10-test-acos-webview.log` | `acos-webview` | 40 | 8 | 0 |
| **total** | **29 crates** | **2894** | **27** | **29** |

(`acos-jni` passed **71/71**; v0.3.0's `flutter_hello` fixture gap is resolved.)

Every single one of the 27 failures was re-run alone and classified
(`01-tests/11-failures-summary.log`, `11-failure-*.log` .. `16-failure-*.log`):

| # | failed test(s) | count | alone rerun | classification | evidence |
|---|---|---:|---|---|---|
| 1 | `acos-art::spike_net_boundary::guest_cannot_change_the_lan_drop_rules` | 1 | fails (`bwrap: setting up uid map: Operation not permitted`) | **known runner limitation** (`NoNewPrivs: 1` / `Seccomp: 2` under this runner; not a product failure) | `01-tests/11-failure-spike-net-alone.log` |
| 2 | `acos-cli` `engine::zygote_host::tests` (3 failed in full run) | 3 | **PASS 8/8** with `env -u TMPDIR` | **runner `TMPDIR` `SUN_LEN` env**: runner exports 62-byte `TMPDIR`, overflowing Linux 108-byte `sun_path` in `std::env::temp_dir()` | `01-tests/12-failure-cli-zygote-alone.log` |
| 3 | `acos-cli` `rel3f_front_doors` (4 failed in full run) | 4 | **PASS 9/9** with `env -u TMPDIR` | **runner `TMPDIR` `SUN_LEN` env** (same cause) | `01-tests/13-failure-cli-rel3f-alone.log` |
| 4 | `acos-mcp` `fb5_hygiene` (8), `serveleak2_ownership` (1), `serveleak_shutdown` (1) | 10 | **PASS 10/10** (`8+1+1`) with `env -u TMPDIR` | **runner `TMPDIR` `SUN_LEN` env** (same cause) | `01-tests/14-failure-mcp-sunlen-alone.log` |
| 5 | `acos-cli::sandbox_s8::hostile_live_run_is_gated_and_recorded_separately` | 1 | **PASS 1/1** with `ACOS_S8_LIVE_HOSTILE=1` | **documented opt-in** (`tools/test-prereq-optouts.txt`) | `01-tests/15-failure-cli-s8-optin-alone.log` |
| 6 | `acos-webview` heavy Chromium integration tests (8 tests across `engine_gate`, `fair_pulls`, `gate_engine`, `wv8_viewport`, etc.) | 8 | **PASS 48/48** crate total with `ACOS_WEBVIEW_TEST=1` | **documented opt-in** (`tools/test-prereq-optouts.txt`) | `01-tests/16-failure-webview-optin-alone.log` |

**Product failures: 0.** (Running `acos-webview`'s `engine_gate`/`fair_pulls`
rewrote two tracked handoff JSON files under `results/handoff/{WVB,WVFAIR}/`;
both were restored cleanly via `git checkout --`.)

## Step 2 — Silent-skip audit + 12 per-crate gates — PASS

`02-hygiene/`:

- `tools/audit-silent-skips.sh` → `SILENT-SKIP SITES: 0`, `RC=0`
  (`02-hygiene/01-audit-silent-skips.log`).
- All 12 per-crate gates via `tools/art-runtime/test-crates.sh`
  (`--release --no-fail-fast`, `env -u TMPDIR`): **`SKIP COUNT: 0` on 12/12
  crates** (`02-hygiene/02-per-crate-summary.log`, `02-hygiene/gate-<crate>.log`):
  - 11/12 crates exited `rc=0` (`acos-cli`, `acos-dex`, `acos-framework-natives`,
    `acos-jni`, `acos-mcp`, `acos-protocol`, `acos-render`, `acos-runtime`,
    `acos-services`, `acos-vm`, `acos-webview`).
  - `acos-art` exited `rc=101` solely on the known runner limitation
    `spike_net_boundary::guest_cannot_change_the_lan_drop_rules` (`SKIP COUNT: 0`).

## Step 3 — Matrix (`tools/run-matrix.sh --no-build --profile release`) — PASS (VALID)

`03-matrix/run-matrix.log`, `03-matrix/matrix.json`, `03-matrix/matrix.tsv` (no
`.invalid.json` written):

```json
{
  "classes": {"assertion": 31, "pass": 1, "step_timeout": 2},
  "errors": 0, "failed": 33, "false_fail": 0, "false_pass": 0,
  "gates": {
    "false_pass_zero": true,
    "harness_timeout_within_2pct": true,
    "negative_all_correct": true,
    "no_infra_timeout": true,
    "unclassified_zero": true
  },
  "harness_or_timeout": 0, "infra_timeout": 0,
  "negative_correct": 33, "negative_expect_error_mismatch": [],
  "negative_runs": 33, "passed": 1, "skipped_needs_env": [],
  "step_timeout": 2, "total_runs": 34, "unclassified": 0,
  "verdict_eligible": true
}
```

Comparison vs v0.3.0 (RELGATE3):

| Metric | v0.3.0 (RELGATE3) | v0.4.0 RC (RELGATE4) | Change |
|---|---:|---:|---|
| Total rows | 33 (1 pos + 32 neg) | **34** (1 pos + 33 neg) | +1 new negative (`rot1-rotate-wrong.yaml`) |
| `verdict_eligible` | `true` | **`true`** | VALID |
| `false_pass` | 0 | **0** | PASS |
| `unclassified` | 0 | **0** | PASS |
| `negative_correct` | 29 / 32 | **33 / 33** | **improved** (+4; all negatives match) |
| `infra_timeout` (`harness_or_timeout`) | 3 | **0** | **improved** (`multi-wrong-after-back` → `assertion`; `gaten-content-window-impossible` & `offscreen-pager-wait` → expected `step_timeout`) |
| Gates passed | 2 / 4 (`false_pass_zero`, `unclassified_zero`) | **5 / 5** | all gates `true` |

## Step 4 — 20 Gaten rows + `gaten-word-lookup` (Back & Close) — PASS

`04-gaten/` (`05-gaten-table.md`, `05-gaten-classify.log`,
`05-gaten-fresh-check.log`, `table.json`):

- All **20 Gaten rows** (10 positive `pass` + 10 negative `fail`/`ASSERTION_FAILED`)
  plus **`scenarios/gaten-word-lookup.yaml`** (tests both Back and Close paths,
  64/64 steps, 36 s) run into fresh artifact directories:
  **`total 21  ok 21  bad 0`**.
- Fresh-dir integrity (`fresh-check.py`): **`rows 21  findings 0`**.
- Per-row raw `frames/`, `tree/`, `art-frames/`, and `launcher.stderr.log` were
  pruned after `fresh-check.py` (keeping all `run.json`/`engine.json`/`repro.json`/`verdict.json`
  and `steps/`).

## Step 5 — Module conformance kit + 38 module proof scenarios — PASS (1 known intermittent launch flake)

`05-modules/`:

- **Conformance kit & docs**:
  `cargo test --release --offline -p acos-services --test module_kit --test modules_doc`
  (`05-modules/01-conformance-kit.log`): **`KIT_RC=0`** — `module_kit` **20/20**
  (covering all 24 registered modules: manifest, attach/detach twice, baseline,
  honest report, negative, N=20 density) and `modules_doc` **2/2**.
- **38 module proof scenarios** (`05-modules/scenarios-list.txt`,
  `05-modules/driver.log`, `05-modules/rows.jsonl`, `05-modules/verdicts/`):
  every scenario named in `results/handoff/CONFKIT/README.md` and
  `results/handoff/MODFILL/README.md`, plus `net-https-proof`,
  `modfill-wifi-network-real`, `net-check`, `net-check-offline`, and
  `gles-software`:
  - **37/38 passed on first run**, including `gaten-bubble-overlay` (28/28),
    `webview-probe` (20/20), `webview-fair` (14/14), `net-https-proof` (12/12,
    real HTTPS to `https://example.com`), `net-check` (10/10),
    `net-network-real` (9/9), `net-network-offline` (14/14), `modfill-wifi`
    (44/44), `modfill-media` (32/32), `modfill-wifi-network-real` (14/14, real
    network + scripted Wi-Fi), `modfill-clipboard` (22/22), and
    `modfill-autofill` (12/12).
  - **1 initial failure — classified as known pre-existing intermittent launch
    flake**: `location-probe` failed on its first run with retryable
    `NO_FRESH_FRAME` / `POST_RESUME_NOT_READY`
    (`draw_skip=traversal:view_not_visible`, previously recorded in
    `results/handoff/MODFILL/README.md` §5; `05-modules/location-probe.json`,
    `location-probe.err`). Immediate 5× rerun passed **5/5** in 1–2 s each
    (`05-modules/location-probe-rerun5.log`, `location-probe-r{1..5}.json`).

## Step 6 — Stock emulator comparison (`emu_gate.py`) — PASS (0 worse)

`06-emubench/`:

- **ACOS RC side** (`06-emubench/acos-rc-gate.json`): Step 7's 3-rep v0.4.0 RC
  bundle (`07-ab/v040.json`) merged with a fresh RC `install` phase
  (`06-emubench/acos-install-phase.json`, `ok=True`):
  - `m9_apk_ready_ms`: **0.0 ms** p50 (n=5, all 5 samples `0.0 ms`).
  - `m9_apk_ready_cold_ms`: **3690.0 ms** (n=1, cold scratch).
- **Emulator side** (`06-emubench/emubench-relgate4.json`, 35 rows): fresh live
  emulator run of `n1-warm-tree` via `tools/art-runtime/emubench/emu_bench.py`
  under `run-exclusive.sh --exclusive -- guarded.sh --mem 24G --time 540`
  (`06-emubench/01-n1-warm-tree.log`, `06-emubench/phase-n1-warm-tree.json`:
  `install_ms` p50 `1072.10 ms`, `warm_launch_ms` p50 `346.85 ms`,
  `warm_launch_tree_ms` p50 `4955.68 ms`, peak host RAM **30.6 %**), merged
  with the baseline emulator phases `n1-cold`, `n1-warm`, `n2`, `n4` from
  `results/handoff/EMUBENCH/raw/`.
- **Emulator shutdown verified**: `stop emulator-5554 (2.0s, qemu left=None)`,
  `PASS no qemu-system process is running`, independently re-verified via
  `ps -eo pid,args | awk '/qemu-system/ && !/awk/'` (0 processes).
- **Gate verdict** (`06-emubench/gate.md`, `06-emubench/gate.json`,
  `06-emubench/gate.txt`; also verified against committed
  `results/bench/emubench-2026-11-18.json` in `gate-vs-20261118.*`):
  **`GATE_RC=0` — `12 metric(s) compared, 0 worse for ACOS, 0 worse beyond the 5 % noise floor`**:

| Metric | ACOS (RC) | Emulator | Unit | Better | Delta | Verdict |
|---|---:|---:|---|---|---:|---|
| Cold boot -> first app frame | 4039 | 13632 | ms | ACOS | −70.4 % | better |
| Warm launch -> readable settled UI tree | 287.0 | 4956 | ms | ACOS | −94.2 % | better |
| Tap -> settled UI result (counter) | 951.0 | 1931 | ms | ACOS | −50.8 % | better |
| Tap -> settled UI result (form) | 923.6 | 1946 | ms | ACOS | −52.5 % | better |
| UI tree read with text | 41.6 | 1928 | ms | ACOS | −97.8 % | better |
| Fresh PNG screenshot | 11.7 | 527.1 | ms | ACOS | −97.8 % | better |
| Install -> APK ready to launch | 0.0 | 1359 | ms | ACOS | −100.0 % | better |
| Reset -> first correct screen | 331.9 | 530.8 | ms | ACOS | −37.5 % | better |
| Instance memory, PSS | 240.9 | 3578.0 | MiB | ACOS | −93.3 % | better |
| Instance memory, anon+shmem | 59.3 | 3531.6 | MiB | ACOS | −98.3 % | better |
| Idle CPU | 0.6 | 13.1 | % of one core | ACOS | −95.7 % | better |
| Verified tasks / GiB-minute | 62.84 | 2.54 | tasks/GiB-min | ACOS | −96.0 % | better |
| Warm launch -> launcher-frame change (frame only, not gated) | 287.0 | 286.1 | ms | — | — | REFERENCE (not gated) |

## Step 7 — Interleaved A/B v0.3.0 vs v0.4.0 RC — PASS (0 lattice regressions > 5 %)

`07-ab/` (`arms.txt`, `driver.log`, `run-{v030,v040}/rep-{1,2,3}/{latency,cold,memory}.json`,
`v030.{json,md}`, `v040.{json,md}`, `ab-compare.{txt,json}`, `table.md`):

| arm | source | binary sha256 |
|---|---|---|
| A | tag `v0.3.0` (`02421e17b`), `git archive` → `.cache/rg4-v030/src`, own target dir | `567aee7a86cb7d95…` |
| B | v0.4.0 RC (`origin/modules` `afad66858` + `relnotes2h`), `.cache/rg4-target` | `0a39e118d8113448…` |

Method (identical to RELGATE3): interleaved per rep — A latency+cold, A memory,
B latency+cold, B memory; `--reps 20 --fast-reps 40 --animations on`; one
`--exclusive` lock acquisition per group behind `tools/guarded.sh`. All 12
groups completed `ok=True`, `rc=0`. Every latency and memory phase passed the
host gate (`loadavg1` `0.38–1.09`); the `cold` phase immediately after `latency`
carried the same residual tail (`4.63–8.56`) on both arms (matching RELGATE3).

**ADR-0028 Lattice metrics — 0 regressions > 5 %:**

| metric | v0.3.0 trimmed | v0.4.0 RC trimmed | delta | delta % | first slot A → B | per-rep trimmed (v0.3.0 → v0.4.0) | verdict |
|---|---:|---:|---:|---:|---:|---|---|
| M4 warm launch (fast) | 143.11 | 145.40 | +2.30 ms | +1.6 % | 59 % → 38 % | 145.10/143.26/141.34 → 144.37/148.24/144.35 | **within-noise** (`SLOT-SHIFT −21 pp`) |
| M6 reset (fast) | 184.93 | 187.70 | +2.77 ms | +1.5 % | 2 % → 1 % | 186.31/186.81/181.68 → 189.75/188.47/184.83 | **within-noise** |
| M8 tap counter (fast) | 24.81 | 24.85 | +0.04 ms | +0.1 % | 50 % → 50 % | 24.76/24.88/24.80 → 24.86/24.90/24.78 | **within-noise** |
| M8 tap form (fast) | 29.79 | 29.65 | −0.14 ms | −0.5 % | 100 % → 100 % | 30.07/29.68/29.61 → 29.70/29.86/29.41 | **within-noise** |

**Explicit note on old non-lattice p50 gate:** because `M4 warm launch (fast)`
first-slot share shifted across 50 % (`59 % → 38 %`, `SLOT-SHIFT −21 pp`,
likely from the ~2 ms additional module/GL setup pushing borderline launches
across one 16.67 ms vsync boundary), the old non-lattice median-of-p50s row
jumped by one frame slot: **137.30 → 149.62 ms (+12.31 ms, +9.0 %, old gate
`REGRESSION`)**. Under the ADR-0028 lattice gate, pooled 5 %-trimmed mean is
**+2.30 ms (+1.6 %, within-noise)**.

**Strict latency, CPU, and memory — 0 regressions (all within-noise):**

- Strict latency: M3 cold `4020.62 → 4039.76 ms` (+0.5 %), M4 strict
  `287.58 → 287.01 ms` (−0.2 %), M5 PNG `11.75 → 11.70 ms` (−0.4 %), M6 strict
  `331.35 → 332.06 ms` (+0.2 %), M7 strict `41.45 → 41.55 ms` (+0.2 %), M8
  counter strict `951.02 → 948.94 ms` (−0.2 %), M8 form strict
  `923.47 → 923.07 ms` (−0.0 %).
- CPU: `cpu_active_tap_strict_pct` `44.13 → 43.84` (−0.7 %); `cpu_idle_n1/n5/n10`
  −2.9 % / −3.1 % / +3.3 %.
- Memory: `m1_idle_mem_mib` `58.01 → 59.33 MiB` (+1.32 MiB, +2.3 %);
  `m2_per_session_anon_shmem_n1/n5/n10` `+1.22 / +0.55 / +0.34 MiB` (+2.2 % /
  +1.0 % / +0.6 %); `m2_per_session_pss_n1/n5/n10` `+3.67 / +0.89 / +2.08 MiB`
  (+1.7 % / +1.2 % / +3.8 % — all inside their thresholds).

## Step 8 — Installed-prefix smoke — **FAIL** (pasta licence + source note missing)

Private prefix `$WT/.cache/rg4-prefix` (`~/.local` never touched), installed
with `tools/release/install.sh --prefix .cache/rg4-prefix --from-target .cache/rg4-target`
(`08-install/06-install.log`: `INSTALL_RC=0`, 1.5 GB, 3,185 files,
`com.android.webview.apk` staged, `angle-swiftshader` staged, `libexec/pasta`
installed).

| check | result | evidence |
|---|---|---|
| `acos --version` | `acos 0.3.0` (expected; version not bumped yet) | `08-install/07-version-doctor.log` |
| `acos doctor --json` | **PASS** — `ok: true`, 30/30 checks ok, `layout: installed`, `[ok] pasta 2026_10_02.cba3570 [acos_home]`, `[ok] gles angle-swiftshader` | `08-install/07-version-doctor.log` |
| `acos verify scenarios/v2-art-counter.yaml --engine art --json` | **PASS** — `pass`, `fidelity: art-reference`, exit 0 | `08-install/08-prefix-verify-counter.jsonl`, `08-prefix-verify-counter/` |
| MCP `initialize` + `tools/list` | **PASS** — `{"name":"acos-mcp","version":"0.3.0"}`, protocol `2025-03-26`, **22 tools**, capability probe 8 commands | `08-install/09-mcp-roundtrip.jsonl`, `09-mcp-roundtrip.log` |
| `pasta` binary present & pinned | **PASS** — `share/acos/0.3.0/libexec/pasta`, 1,548,232 bytes, `2026_10_02.cba3570`, sha256 `314c37e8…edf60` | `08-install/07-version-doctor.log` |
| **`pasta` licence + source note in installed prefix** | **FAIL (product/packaging defect)** — `tools/release/install.sh` copies `libexec/pasta` (GPL-2.0-or-later) with **no licence text file** (e.g. `GPL-2.0-or-later.txt` / `LICENSES/`) and **no source note** (e.g. `passt-build.txt` / upstream URL + tag + commit + GPL §3 written offer) anywhere in the installed prefix; `MANIFEST.txt` lists only `libexec/pasta` with no licence or source metadata | `08-install/07-version-doctor.log`, `tools/release/install.sh` lines 244–272, 311–320 |

Secondary observations recorded for the installer fix task (not gate-blocking on
their own):

- `tools/release/install.sh` line 270 logs `(49 bytes)` for `pasta` because
  `stat -c '%s'` does not dereference symlinks when `<repo>/libexec/pasta` is a
  symlink (though `install -m 0755` copies the full 1,548,232-byte binary).
- `BINS=(acos acos-art acos-mcp acos-netd)` in `tools/release/install.sh` does
  not install `acos-webview` alongside `com.android.webview.apk`.

## Housekeeping

- Updated `results/release/v0.4.0-NOTES-DRAFT.md` "Release gate results" with
  the full per-step table, numbers, log paths, and one-line `NO-SHIP` verdict.
- Deleted temporary build/runtime/fixture directories before final commit:
  `.cache/rg4-target`, `.cache/rg4-v030`, `.cache/rg4-prefix`, `.cache/rg4b`,
  `.cache/rg4-*`, `.cache/rt*`, `.cache/wvrun`, `.cache/avd`, `fixtures/build`.
- Total size of `results/release/v0.4.0-gate/relgate4/`: ~12 MB (< 15 MB limit).
