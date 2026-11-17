# RELGATE2 — v0.3.0 release gate, TIMING half

Worktree `/home/Qui/.t3/worktrees/acos/t3-c216e10c-9ee2c3262751508a`, branch
`relgate2` from `rel-030` (RC **`169bfa5e`**, version 0.3.0: RELGATE1 correctness
evidence + the gaten-lesson two-Back fix). No code was changed; this directory is
evidence only. Parent record: `results/release/v0.3.0-gate/correctness/SUMMARY.md`
(RELGATE1, correctness half).

Environment (per `BRIEF_COMMON_0316.md`):
`ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts`; worktree
`.cache/{art,gaten,boot/android-34}` symlinks; `fixtures/build` = `cp -al` of the
main checkout; `ACOS_BOOT_DIR`/`XDG_RUNTIME_DIR`/`ACOS_HOME` never set. Target
dir `.cache/rg2-target` (RC) and `.cache/rg2/v022-target` (v0.2.2 arm), both
deleted at the end. Every ART run went through
`/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh` wrapping
`tools/guarded.sh`; every timing acquisition took **all** slots (`--exclusive`).
Nothing outside this worktree was modified (the only file written outside it was
the `.cache/standby/QUIET_WINDOW` coordination marker, released at the end).

Host: AMD Ryzen 9 9950X, 60 GiB, kernel 7.2.11-arch1-1. The host **rebooted at
10:36Z** (no clean shutdown) in the middle of the first A/B attempt; per the
orchestrator every timing result here is from the **post-reboot** run only (the
pre-reboot attempt is kept under `02-ab/preboot/` as provenance and is *not*
used).

Quiet window: 3 consecutive checks 60 s apart with `loadavg1 < 4` before the
timing runs (0.84 / 0.64 / 0.61 at claim, 10:48:55Z–11:39:48Z), marker
`/home/Qui/Projects/acos/.cache/standby/QUIET_WINDOW` (pid 40092), deleted after
the last timed rep.

## 1. The one failing RELGATE1 Gaten row — **PASS**

`scenarios/gaten-lesson.yaml` re-run with RELGATE1's exact command into a fresh
artifact dir (`00-gaten-lesson/`, command and detail in its `README.md`):
`status: pass`, `ok: true`, `fidelity: art-reference`, 29 steps, 20 s wall.
The `wait_for` that timed out in RELGATE1 settles well inside its 30 s deadline.

## 2. `tools/run-matrix.sh --no-build --profile release` — false_pass 0, matrix gate INVALID

| run | when | host gate line | result |
|---|---|---|---|
| `01-matrix/` | 09:44Z (pre-reboot) | `run-matrix: host gate ok (loadavg1 1.08, MemAvailable 46.0 GiB)` | 33 rows, `false_pass=0`, INVALID (3 unclassified) |
| `01-matrix-postreboot/` | 05:57Z next boot | `run-matrix: host gate ok (loadavg1 0.59, MemAvailable 53.45 GiB)` | 33 rows, `false_pass=0`, INVALID (3 unclassified) |

Both runs: 1 positive + 32 negatives, release binary
`sha256 9f7b8c5d5dbe6d0d…`, `verdict_eligible=true`.
Post-reboot summary (`matrix.json.invalid.json`, `run-matrix.log`):

```
{"classes": {"assertion": 24, "harness": 2, "infra_timeout": 3, "pass": 1, "unknown": 3},
 "false_pass": 0, "false_fail": 0, "negative_correct": 26, "negative_runs": 32,
 "unclassified": 3, "harness_or_timeout": 5, "total_runs": 33,
 "gates": {"false_pass_zero": true, "harness_timeout_within_2pct": false,
           "negative_all_correct": false, "unclassified_zero": false}}
```

The mandatory gate (`false_pass == 0`) holds. The run is nevertheless **INVALID**
because of three rows that cannot be classified; none of them is a false pass:

| row | expected | got | why |
|---|---|---|---|
| `negative/finish-start-wrong-home` | fail | `APK_IO` (class unknown) | `fixtures/build/apps/finish-start.apk` does not exist in the main checkout's fixture build (RELGATE1 built it in its own worktree) — environment gap of the default matrix set |
| `negative/form-wrong-error`, `negative/offscreen-pager-visible` | fail | fail, `ELEMENT_OFFSCREEN` | both are **correct negatives**; `benchlib`'s classifier has no class for `ELEMENT_OFFSCREEN`, so the harness counts them "unclassified" |
| `negative/gaten-content-{assert-screenshot,selector-bounds}` | fail `ASSERTION_FAILED/content_settle_*` | `PLACEHOLDER_CONTENT` (class harness) | the `launch` step (which carries `settle: content`) never mounted the app: see finding F1 |
| `negative/gaten-content-window-impossible`, `negative/multi-wrong-after-back`, `negative/offscreen-pager-wait` | non-pass | `TIMEOUT` (class infra_timeout) | identical timeouts on the **v0.2.2** binary in RELGATE1 (`04-scenarios-baseline022.log`) — pre-existing |

## 3. Interleaved A/B, v0.2.2 (tag) vs v0.3.0 RC — **2 lattice regressions**

### Arms and method

| arm | source | version | `acos` sha256 |
|---|---|---|---|
| A | tag `v0.2.2` (ce14b888), `git archive` → `.cache/rg2/v022-src`, own target dir | `acos 0.2.2` | `5bc0904a6646b6988d911ba80c50a6a72278cb438f82252e1bb9d609ff3edd8c` |
| B | `rel-030` HEAD `169bfa5e` in this worktree (`.cache/rg2-target`) | `acos 0.3.0` | `9f7b8c5d5dbe6d0dc0f5ae3b23e745bb01dc735808a8e83ec8385166210820b4` |

Driver `02-ab/run-ab.sh` (interleaved **per rep**: A latency+cold, A memory, B
latency+cold, B memory — 12 lock acquisitions of `--exclusive` +
`tools/guarded.sh`), 3 reps, the phase-group spec and sample counts of
`tools/bench/acos-bench.sh`'s full profile: latency+cold `8G/540 s`, memory
`16G/600 s`, `--reps 20 --fast-reps 40` (ADR-0028: 20 strict samples, 40
fast-settle samples per rep per metric), `--animations on`, same APKs, screen,
density and actions for both arms. Merged per arm by `acos_bench.py --merge`
(raw `samples` and per-rep `lattice` blocks kept), compared by
`tools/bench/ab-compare.py` (`ab-compare.txt`, `ab-compare.json`,
`table.md`). n = 60 strict / 120 fast samples per metric per arm; memory n = 3
fresh-core pairs per arm.

Per-phase host gate as recorded by the bench (`host_before`):

| arm | rep | latency ok/noisy (load1) | wall s | cold ok/noisy (load1) | wall s | memory ok/noisy (load1) | wall s |
|---|---|---|---:|---|---:|---|---:|
| v0.2.2 | 1 | True/False (1.31) | 199.9 | True/True (8.60) | 86.1 | True/False (0.54) | 222.6 |
| v0.2.2 | 2 | True/False (1.55) | 197.5 | True/True (10.35) | 87.1 | True/False (1.66) | 222.2 |
| v0.2.2 | 3 | True/False (1.66) | 197.7 | True/True (9.07) | 86.9 | True/False (1.55) | 222.3 |
| v0.3.0 | 1 | True/False (1.44) | 197.4 | True/True (6.17) | 90.5 | True/False (1.56) | 222.4 |
| v0.3.0 | 2 | True/False (1.45) | 194.6 | True/True (6.05) | 89.3 | True/False (1.64) | 222.1 |
| v0.3.0 | 3 | True/False (1.41) | 194.9 | True/True (5.68) | 90.5 | True/False (1.52) | 222.3 |

The latency and memory phases of every rep passed the bench's host gate
(`loadavg1 ≤ 4`, `MemAvailable ≥ 30 GiB`). The **cold** phase starts ~4 s after
the latency group releases its load, so `loadavg1` still carries that tail and
the harness labels all six cold phases `noisy`; the label is identical on both
arms and M3 is therefore reported with that caveat (it is also consistent per
rep: 4009/4083/4082 vs 4232/4186/4233 ms).

### Result per metric

Full table: `02-ab/table.md` (generated from `ab-compare.json`); verbatim gate
output: `02-ab/ab-compare.txt`.

**Lattice gate (ADR-0028) regressions — 2:**

| metric | v0.2.2 (trimmed) | v0.3.0 RC (trimmed) | delta | delta % | first slot |
|---|---:|---:|---:|---:|---|
| M8 tap → settled, counter (**fast**) | 35.73 ms | 40.82 ms | **+5.08 ms** | **+14.2 %** | 27 % → 18 % |
| M8 tap → settled, form (**fast**) | 32.42 ms | 41.09 ms | **+8.68 ms** | **+26.8 %** | 0 % → 4 % |

Both are also called regressions by the old median-of-p50s gate (+45.7 % / +26.8 %),
and both are consistent across all three reps:

| metric | v0.2.2 per-rep trimmed | v0.3.0 RC per-rep trimmed |
|---|---|---|
| M8 counter fast | 40.36 / 33.99 / 32.88 | 49.07 / 38.39 / 34.47 |
| M8 form fast | 32.32 / 32.48 / 32.44 | 41.06 / 41.06 / 40.80 |

Sample distributions (`samples` in `v0{22,30}.json`):

* **M8 form fast** — v0.2.2 n=120, p10/p50/p90 = 31.9/32.4/33.0, min 31.5,
  max 34.7 (100 % of samples in one band); RC n=120, p10/p50/p90 = 40.4/41.1/41.8,
  min 25.3, max 42.7 (95–100 % in one band). The two arms are cleanly separated;
  this is a real ~+8.7 ms (≈half a 16.7 ms frame) shift of the form-submit
  tap→settle path, not a lattice flip of a middle sample.
* **M8 counter fast** — bimodal on both arms (bands at ≈16 / ≈32 / ≈48 ms):
  v0.2.2 slot shares 0:27 % 1:29 % 2:42 %, RC 0:18 % 1:20 % 2:57 %. The RC
  lands one slot later more often; the per-rep spread overlaps at the good end
  (RC rep 3 34.47 vs v0.2.2 rep 2 33.99), so this is the weaker of the two
  regressions — but the pooled trimmed mean and the old gate agree.

**Improved / flat / within-noise (not regressions):**

* M4 warm launch **fast** 162.82 → 143.39 ms (**−11.9 %**, first slot 0 % → 54 %,
  flagged slot-shift +54 pp) and M6 reset **fast** 186.55 → 168.06 ms
  (**−9.9 %**, first slot 0 % → 29 %).
* M4 strict −5.4 % (improved under the old gate), M6 strict −5.2 % (within
  noise), M7 strict flat (+0.2 %), M7 fast 0.44 → 0.64 ms (within noise),
  M8 strict flat (+0.0 %/+0.2 %), M5 −1.2 %, tap ack 5.00 ms, teardown flat.
* CPU (active/idle at N=1/5/10 and per session) and idle wakeups: all within
  noise.
* Memory: M1 55.46 → 57.05 MiB anon+shmem (+2.9 %) and PSS +0.4 %; M2 N=1/N=5
  within noise. **Two non-fast memory rows trip the old gate's ±3 % threshold at
  N=10**: `m2_per_session_anon_shmem_n10` 53.98 → 55.75 MiB (**+3.3 %**,
  +1.77 MiB/session) and `m2_per_session_pss_n10` 52.40 → 54.40 MiB
  (**+3.8 %**, +2.00 MiB/session). They are small but above the derived
  threshold, and they are recorded here as regressions even though ADR-0028's
  lattice rule only governs the fast-settle metrics.

### Verdict

Per ADR-0028 the release gate **fails on timing**: three metrics regress —
M8 tap→settled counter (fast, +14.2 %), M8 tap→settled form (fast, +26.8 %) and,
on the old gate, two N=10 per-session memory rows (+3.3 % anon+shmem, +3.8 %
Pss). Everything else is parity or better (M4 launch fast −11.9 %, M6 reset fast
−9.9 %, all strict latencies flat or better, CPU flat). Per the brief I stopped
after recording: nothing was changed, no fix was attempted.

Unattributed; the smallest next action is a profile of one action reply between
v0.2.2 and the RC (the v0.3.0 reply adds `input.handled`/`input.handled_evidence`/
`input.target` with its tree-geometry hit-test to every action, which is on this
path), plus a look at why N=10 per-session memory grew ~2 MiB/session while
N=1/N=5 did not.

## Findings

**F1 — a `launch` step that carries `settle: content` can end in
`PLACEHOLDER_CONTENT` on the RC (reproducible, fail-closed).**
`negative/gaten-content-assert-screenshot` and `negative/gaten-content-selector-bounds`
fail at step 0 instead of their pinned `ASSERTION_FAILED/content_settle_*`:
`launch reached the window skeleton and nothing about the frame moved … 8 frame(s)
over 8 traversal(s)/0 ms … reason: placeholder-content`. Both reproduce on the
post-reboot run (`01-matrix-postreboot/`), both are *errors*, never passes, and
the expectation pin correctly refuses them (class `harness`, "a negative that
fails for another reason proves nothing"). RELGATE1 recorded both as passing at
`f162b2d9` with the same scenario (`04-scenarios-table.md`, launch
`bound: content-stable, elapsed_ms 353`), so the outcome depends on how much wall
time the 8 traversals take: on a busy host the app's first frame arrives inside
the sampling window; on this quiet host the 8 traversals cost ~0 ms and the
placeholder verdict is reached before the app can draw. The third sibling
(`gaten-content-welcome-wrong`) failed the same way pre-reboot and passed
post-reboot, i.e. it is the same race. Smallest next action: a launch
content-settle must not conclude "placeholder" from samples taken within a few
ms of the window mount (owner decision — product code, not touched here).

**F2 — `benchlib`'s error classifier has no class for `ELEMENT_OFFSCREEN`**, so
two correct negatives that fail with that code are counted `unclassified` and
make `run-matrix.sh` INVALID even though `false_pass=0`. Harness-only.

**F3 — the default matrix set references a fixture that is not in the main
checkout's `fixtures/build`** (`finish-start.apk`), so that row is `APK_IO`.
RELGATE1 built it in its own worktree; a release gate should build the matrix set
from scratch or skip-and-declare it.

## Housekeeping

* Raw per-phase documents kept: `02-ab/run-{v022,v030}/rep-{1,2,3}/{latency,cold,memory}.json`
  + `bundle.json` (~1 MB total), merged `v022.json`/`v030.json` with every raw
  sample. Matrix artifacts (frames/trees) were pruned; `matrix.json(.invalid.json)`
  + `run-matrix.log` + per-row JSON in `.driver/`-free form are kept.
* Deleted at the end: `.cache/rg2-target`, `.cache/rg2`(v022-src, v022-target,
  matrix artifacts), `.cache/rg2b`, `.cache/rg2/rt`.
* Nothing outside this worktree was modified. The owner's `~/.local`,
  `~/Projects/gaten` and MCP/daemon were never touched.
