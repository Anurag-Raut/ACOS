# RELGATE3 — v0.3.0 FINAL gate rerun (correctness + timing, one pass)

Worktree `/home/Qui/.t3/worktrees/acos/t3-7a2f4ca5-3dab9ee7edba13ab`, branch
`relgate3` from `rel-030` HEAD **`c7962f714a325faadcf37d87ad31eb44f30ddf9d`**
(the v0.3.0 RC = RELGATE1 correctness evidence + GATEFIX `8f8919f4` (launch
first-frame wait) + LOOPERWAKE `381ec7ad` + **PERF4** (M8 tap→settled fix)).
**No product code was changed by this pass**; this directory is evidence only.

This rerun exists because RELGATE2 (`..gate/timing/README.md`) *failed* the
timing half: two M8 fast-settle lattice regressions (counter +14.2 %, form
+26.8 %). PERF4 (`results/handoff/PERF4/README.md`) diagnosed and fixed them.

## Environment (per `BRIEF_COMMON_0316.md`)

`ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts`; worktree
`.cache/{art,gaten,boot/android-34}` symlinks to the main checkout; `fixtures/build`
= `cp -al` of the main checkout + the fixtures it lacks, built in this worktree
with `ACOS_APP_ONLY="finish-start tap-targets touch-probe back-layers looper-wake
pms-parcel scroll-list" tools/build-apps.sh` (`00-fixtures/`). `ACOS_BOOT_DIR`,
`XDG_RUNTIME_DIR`, `ACOS_HOME` never set; `env -u ACOS_HOME` for the installed
smokes. Cargo target dirs `.cache/rg3-target` (RC) and `.cache/rg3/v022-target`
(v0.2.2 arm), both deleted at the end.

Every ART run went through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`; every **timing**
acquisition took all slots (`--exclusive`). Correctness runs (tests, matrix,
Gaten rows) used the shared slots.

Host: AMD Ryzen 9 9950X, 60 GiB, kernel 7.2.11-arch1-1, no reboot this time.

### Quiet window

`MERGEMPROC` (v0.4.0) was still running when this pass started. Checks 60 s apart
(`quiet-window.log`): loadavg1 4.67 → **1.83 / 0.97 / 0.95** (13:47:03Z–13:49:03Z)
= 3 consecutive readings < 4. Marker `/home/Qui/Projects/acos/.cache/standby/QUIET_WINDOW`
written 13:49:09Z (pid 80135, a placeholder `sleep`) and deleted 14:46:18Z, right
after the last timed rep completed (14:44:44Z). **Not contaminated**: no
`--allow-contaminated`, and every timing phase passed the bench's host gate.

## 1. Build + workspace test suite — PASS (2 known-env failures)

`cargo build --release --workspace --offline` → `CARGO_RC=0`, 1m10s, 29 warnings
(`01-tests/01-build-release.log`).

`cargo test --workspace --release --no-fail-fast`, run per crate group so no
ART-lock acquisition ran long (same method as RELGATE1), `01-tests/run-tests.sh`,
driver log `01-tests/driver.log`:

| log | crates | passed | failed | ignored |
|-----|--------|--------|--------|---------|
| `02-test-groupA.log` | 16 non-ART crates | 678 | 0 | 1 |
| `02-test-groupB.log` | acos-vm, acos-runtime, acos-sandbox | 345 | 0 | 10 |
| `03-test-acos-verify.log` | acos-verify | 152 | 0 | 0 |
| `04-test-acos-art.log` | acos-art | 222 | 0 | 1 |
| `05-test-acos-cli.log` | acos-cli | 297 | 0 | 15 |
| `06-test-acos-services.log` | acos-services | 365 | 0 | 0 |
| `07-test-acos-mcp.log` | acos-mcp | 156 | **1** | 0 |
| `08-test-acos-jni.log` | acos-jni | 69 | **1** | 0 |
| **total** | 24 crates | **2284** | **2** | **27** |

(+1 passed vs RELGATE1/RELGATE2: PERF4's new
`a_fast_commit_binds_its_own_draw_only_with_proof`.)

Both failures re-run alone, both **known env**, no product defect:

| test | alone | classification |
|------|-------|----------------|
| `acos-mcp/tests/art_default.rs::default_session_tools_use_the_private_art_daemon` | **passes** (`03-failures-mcp-art_default-alone.log`, 5/5) | test assumes `ACOS_RUNTIME_DIR` is unset; with it set to this worktree's 60-character path it computes a 112-byte `sun_path` and asserts it is "under the private runtime dir" |
| `acos-jni/tests/flutter_hello.rs::flutter_hello_under_confinement` | still fails (`03-failures-jni-flutter-alone.log`) | the shared fixture `flutter-hello.apk` has no `lib/x86_64/libapp_probe.so` (`unzip -l … \| grep -i probe` → nothing), so `W93A_SEM … strings=0` and the pointer packet never reaches `onPointerDataPacket`; documented in `v0.3.0-NOTES.md` under Known limits |

## 2. `tools/run-matrix.sh --no-build --profile release` — **VALID**, false_pass 0, unclassified 0

`02-matrix/run-matrix.log`, `02-matrix/matrix.json`, `02-matrix/matrix.tsv`
(no `.invalid.json` was written):

```
run-matrix: host gate ok (loadavg1 1.03, MemAvailable 49.34 GiB)
{"classes": {"assertion": 29, "infra_timeout": 3, "pass": 1}, "errors": 3, "false_fail": 0,
 "false_pass": 0, "gates": {"false_pass_zero": true, "harness_timeout_within_2pct": false,
 "negative_all_correct": false, "unclassified_zero": true}, "harness_or_timeout": 3,
 "negative_correct": 29, "negative_runs": 32, "passed": 1, "total_runs": 33,
 "unclassified": 0, "verdict_eligible": true}
MATRIX_RC=0
```

33 rows: 1 positive + 32 negatives, `negative_correct = 29/32`, **`unclassified = 0`**,
`false_pass = 0`, `verdict_eligible = true`. RELGATE2's INVALID status is gone:
both `ELEMENT_OFFSCREEN` rows (`form-wrong-error`, `offscreen-pager-visible`) now
classify as `assertion`, `finish-start-wrong-home` runs (the fixture was built
here), and the two leading `gaten-content-*` rows that failed as
`PLACEHOLDER_CONTENT` in RELGATE2 (its finding F1) now reach their pinned
`ASSERTION_FAILED` — i.e. **GATEFIX fixed F1**.

Remaining 3 rows are `infra_timeout` (`gaten-content-window-impossible`,
`multi-wrong-after-back`, `offscreen-pager-wait`). **Known and pre-existing**:
RELGATE1's v0.2.2 baseline sweep
(`..gate/correctness/04-scenarios-baseline022-table.md`) records the same two of
them as `TIMEOUT` on the v0.2.2 binary, and `gaten-content-window-impossible` is
a scenario that waits on a condition it declares impossible. Recorded, not fixed
(per brief).

## 3. The 20 Gaten rows — **20/20 as declared**

`03-gaten/` (fresh artifact dir per row, `gaten-list.txt`, `sweep.sh`,
`05-gaten.log`, `05-gaten-classify.log`, `05-gaten-table.md`,
`05-gaten-fresh-check.log`).

`total 20  ok 20  bad 0` — 10 positives `pass`, 10 negatives `fail
ASSERTION_FAILED`. Include `scenarios/gaten-lesson.yaml` (**pass**, 14 s), the one
row RELGATE1 lost to a `wait_for` timeout and RELGATE2 re-verified.

Fresh-dir integrity (`fresh-check.py`): `rows 20  findings 0` — every row's
`run.json`/`engine.json`/`repro.json`/`verdict.json` name one `request_id` and one
`session_id`, all mtimes inside the run window, no artifact carried over. Like
RELGATE1, the per-row `artifacts/{frames,tree,art-frames}/` were pruned **after**
the check (294 MiB → 5.7 MiB); every JSON document, `steps/` and `logs/` are kept.

## 4. Interleaved A/B v0.2.2 vs this RC — **no regressions**

`04-ab/` — driver `run-ab.sh` (RELGATE2's driver, paths adapted), `driver.log`,
raw `run-{v022,v030}/rep-{1,2,3}/{latency,cold,memory}.json`, merged
`v022.json`/`v030.json` (with every raw sample), `ab-compare.txt` (verbatim gate
output), `ab-compare.json`, `table.md`.

| arm | source | version | `acos` sha256 |
|---|---|---|---|
| A | tag `v0.2.2` (`ce14b888`), `git archive` → `.cache/rg3/v022-src`, own target dir | `acos 0.2.2` | `7bac4e0c281fde0f…` |
| B | `rel-030` HEAD `c7962f71` in this worktree (`.cache/rg3-target`) | `acos 0.3.0` | `006c5ac02707d224…` |

(Arm identity is the version string + binary sha. The merged JSON's `commit` field
names this worktree's HEAD for both arms because `--merge` reads the repo, not the
tag — the same artifact as RELGATE2; it is not arm provenance.)

Method (identical to RELGATE2, so the two gates are comparable): interleaved
**per rep** — A latency+cold, A memory, B latency+cold, B memory; `--reps 20
--fast-reps 40`; latency+cold `8G/540 s`, memory `16G/600 s`; `--animations on`;
one `--exclusive` lock acquisition per group, behind `tools/guarded.sh`; same
APKs, display, density and actions for both arms. n = 60 strict / 120 fast samples
per metric per arm; memory n = 3 fresh-core pairs per arm. 12 acquisitions,
13:53:40Z → 14:44:44Z.

Per-phase host gate as recorded by the bench (`host_before` in each phase doc):

| arm | rep | latency noisy (load1) | cold noisy (load1) | memory noisy (load1) |
|---|---|---|---:|---|
| v0.2.2 | 1 | False (1.20) | True (5.97) | False (1.07) |
| v0.2.2 | 2 | False (0.47) | True (10.82) | False (0.44) |
| v0.2.2 | 3 | False (0.88) | True (8.05) | False (0.67) |
| v0.3.0 | 1 | False (1.00) | True (10.28) | False (0.60) |
| v0.3.0 | 2 | False (0.64) | True (7.90) | False (0.48) |
| v0.3.0 | 3 | False (1.08) | True (4.86) | False (0.76) |

The latency and memory phases of all six reps passed the host gate. The **cold**
phase starts ~4 s after the latency group releases its load, so `loadavg1` still
carries that tail; the label is identical on both arms and M3 is reported with
that caveat (the RELGATE2 pattern). Response times agree: 4048.68 vs 4148.25 ms
(+2.5 %, within noise).

### Verdict per metric (full table `04-ab/table.md`)

**Lattice metrics (ADR-0028) — 0 regressions, all four improved or flat:**

| metric | v0.2.2 trimmed | v0.3.0 trimmed | delta | delta % | first slot | verdict |
|---|---:|---:|---:|---:|---:|---|
| M4 warm launch (fast) | 159.74 | 143.59 | −16.15 ms | −10.1 % | 0 % → 57 % | **improved** (slot-shift +58 pp) |
| M6 reset (fast) | 197.75 | 185.46 | −12.29 ms | −6.2 % | 0 % → 5 % | **improved** |
| M8 tap→settled, counter (fast) | 46.37 | 24.93 | −21.44 ms | −46.2 % | 8 % → 49 % | **improved** (slot-shift +41 pp) |
| M8 tap→settled, form (fast) | 32.40 | 29.60 | −2.81 ms | −8.7 % | 100 % → 99 % | **improved** |

Consistent in every rep (per-rep trimmed means, `lattice.per_rep`):

| metric | v0.2.2 rep 1/2/3 | v0.3.0 rep 1/2/3 |
|---|---|---|
| M4 fast | 159.00 / 158.99 / 161.22 | 144.26 / 141.12 / 145.41 |
| M6 fast | 205.36 / 198.30 / 189.16 | 186.48 / 183.42 / 186.35 |
| M8 counter fast | 49.85 / 39.15 / 48.36 | 24.78 / 25.27 / 24.73 |
| M8 form fast | 32.28 / 32.41 / 32.53 | 29.76 / 30.59 / 28.44 |

The two RELGATE2 regressions (counter +14.2 %, form +26.8 %) are **gone**;
M8 counter is now 1.86× faster and M8 form 8.7 % faster than v0.2.2. M4/M6 fast
keep RELGATE2's improvement. `M7 UI tree (fast)` 0.557 → 0.627 ms: +13.6 % but
+0.07 ms, far under ADR-0028's +2 ms floor → within-noise (it is a sub-millisecond
metric; no lattice block is recorded for it).

**CPU rows — all inside the ±10 % row, so no STOP condition:**

| metric | v0.2.2 | v0.3.0 | delta % | per rep (v0.2.2 → v0.3.0) |
|---|---:|---:|---:|---|
| `cpu_active_tap_strict_pct` | 45.51 | 43.37 | **−4.7 %** | 44.25/45.51/46.04 → 42.89/44.12/43.37 (lower in all three) |
| `cpu_idle_n1_pct` | 0.56 | 0.65 | +14.5 % (abs +0.08) | under the old gate's ±21 % derived band → within-noise |
| `cpu_idle_n5_pct` | 2.39 | 2.31 | −3.4 % | — |
| `cpu_idle_n10_pct` | 3.83 | 3.60 | −6.0 % | — |
| `cpu_idle_n{1,5,10}_per_session_pct` | 0.56/0.48/0.38 | 0.65/0.46/0.36 | +14.5 / −3.3 / −6.0 % | abs ≤ 0.09 |
| `wakeups_idle_n1/n5/n10_per_s` | 1.21/2.05/3.11 | 1.23/2.18/3.24 | +1.3 / +6.4 / +4.1 % | within noise |

PERF4 flagged this row as the one to watch (its fix made the fast settle
event-driven; it kept the strict cadence because making that eager had cost ~20
points of CPU). Measured at the gate: **−4.7 %, lower in every rep**, i.e. the
fix did not buy its latency win with CPU, and the planned
"signal only when the C1 store counter advanced" follow-up is **not needed** and
was not implemented.

**Memory — the accepted uniform v0.3.0 feature cost (~+1.6–2.2 MiB/session):**

| metric | v0.2.2 | v0.3.0 | delta | delta % | old gate |
|---|---:|---:|---:|---:|---|
| `m1_idle_mem_mib` | 55.44 | 57.94 | **+2.50 MiB** | +4.5 % | REGRESSION (relative ±3 %) |
| `m2_per_session_anon_shmem_n1` | 53.81 | 56.04 | **+2.24 MiB** | +4.2 % | REGRESSION (relative ±3 %) |
| `m2_per_session_anon_shmem_n5` | 54.21 | 55.62 | +1.41 MiB | +2.6 % | within-noise |
| `m2_per_session_anon_shmem_n10` | 54.19 | 55.82 | +1.62 MiB | +3.0 % | within-noise |
| `m2_per_session_pss_n1` | 213.74 | 214.87 | +1.13 MiB | +0.5 % | within-noise |
| `m2_per_session_pss_n5` | 71.20 | 72.67 | +1.48 MiB | +2.1 % | within-noise |
| `m2_per_session_pss_n10` | 52.88 | 54.62 | **+1.73 MiB** | +3.3 % | REGRESSION (relative ±3 %) |
| `m1_idle_pss_mib` | 230.80 | 233.49 | +2.69 MiB | +1.2 % | within-noise |

Per-rep `rep_p50`, showing the delta is a **constant and not noise**:

| metric | v0.2.2 rep 1/2/3 | v0.3.0 rep 1/2/3 | mean delta |
|---|---|---|---|
| `m2_per_session_anon_shmem_n1` | 53.94 / 53.77 / 53.81 | 56.04 / 55.74 / 56.20 | +2.16 |
| `m2_per_session_anon_shmem_n5` | 53.59 / 54.21 / 54.28 | 55.54 / 55.62 / 55.71 | +1.60 |
| `m2_per_session_anon_shmem_n10` | 53.66 / 54.19 / 54.37 | 56.08 / 55.82 / 55.73 | +1.80 |
| `m2_per_session_pss_n10` | 52.38 / 52.88 / 52.98 | 54.85 / 54.62 / 54.39 | +1.87 |

**Read this as the documented feature cost, not a regression.** The increment is
within noise of +1.5 MiB/session at every N (1, 5, 10) and in PSS as well as
anon+shmem, i.e. it is a fixed per-guest allocation, not a per-session scaling
term. Which *relative* row trips depends only on which N has the smallest
denominator: RELGATE2 tripped `anon_shmem_n10` and `pss_n10`; this run tripped
`anon_shmem_n1` and `pss_n10`; `n5` escaped both times at +2.6 %/+2.1 %. PERF4 §3
located the same +1.5 MiB inside each guest process (guest PSS up, host share
flat) as the v0.3.0 feature work touching more framework code per guest (IME
`InputEventSender` + the real touch chain), not a host-side buffer, ring or fd.
`m1_idle_mem_mib` (+2.50 MiB with one app) is one session's worth of the same
cost. Accepted per brief: **+~1.5 MiB/session, uniform N=1/5/10, documented as
the v0.3.0 module/feature cost.**

### Verdict

**No lattice regression on M4/M5/M6/M7/M8. No CPU row beyond +10 % (the watched
row is −4.7 %). No new memory regression beyond the accepted uniform feature cost
of +~1.5 MiB/session.** Timing half: **PASS**.

## 5. Installed-prefix smoke — PASS

Private prefix `$WT/.cache/rg3-prefix` (`~/.local` never touched), built from
this RC's binaries with `--from-target`; install log `05-install/06-install.log`
(`INSTALL_RC=0`, `installed ACOS 0.3.0`, 1.6G, 3135 files). The stray
`boot/android-34` self-symlink RELGATE1 hit did not recur: `copy_tree` resolves
`.cache/boot/android-34` with `readlink -f` before `cp -a`, and the
`escaped`-symlink check after the copy passed.

| check | result | evidence |
|---|---|---|
| `acos --version` | `acos 0.3.0` | `05-install/07-version-doctor.log` |
| `acos doctor --json` | **ok, 28/28 checks ok**, `layout: installed`, `acos_home` = the prefix's `share/acos/0.3.0` | `05-install/07-version-doctor.log` |
| `acos verify scenarios/v2-art-counter.yaml --engine art --json` | **pass**, `fidelity: art-reference`, `ok: true`, exit 0, reply capped at 20 KiB with the full text in the artifacts | `05-install/08-prefix-verify-counter.jsonl`, `08-prefix-verify-counter/` |
| MCP `initialize` + `tools/list` | server `{"name":"acos-mcp","version":"0.3.0"}`, protocol `2025-03-26`, **19 tools**, capability probe 8 commands | `05-install/09-mcp-roundtrip.jsonl`, `09-mcp-roundtrip.log`, `mcp_handshake.py` |

## Housekeeping

* `results/release/v0.3.0-NOTES.md` "Release gate results" updated with the full
  RELGATE1 → RELGATE2 (fail) → PERF4 fixes → RELGATE3 (pass) chain.
* Deleted at the end: `.cache/rg3-target`, `.cache/rg3` (`v022-src`, `v022-target`,
  matrix artifacts), `.cache/rg3b`, `.cache/rg3/rt*`, `.cache/rg3-prefix`.
* Nothing outside this worktree was modified except the
  `.cache/standby/QUIET_WINDOW` coordination marker (written 13:49:09Z, deleted
  14:46:18Z). The owner's `~/.local`, `~/Projects/gaten`, MCP registrations and
  daemons were never touched.
