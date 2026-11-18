# EMUBENCH — stock Android emulator baseline (evidence)

Owner direction 2026-11-18 §2: ACOS must beat the normal Android emulator on
every tracked metric, so the emulator is a tracked baseline for the release gate.
ADR-0015's one-emulator rule is relaxed **for this benchmark only**: N=1, 2 and 4
concurrent instances, each ≤2048 MiB guest RAM, only while the host stays under
the RAM ceiling, always shut down.

| Item | Value |
|---|---|
| Branch / commit | `emubench` (from `modules` 473859c2) — see the driver log header |
| Driver | `tools/art-runtime/emubench/run-emubench.sh` |
| Harness | `tools/art-runtime/emubench/emu_bench.py` (`--self-test`: 10 checks, 0 failed) |
| Gate | `tools/art-runtime/emubench/emu_gate.py` |
| Merged raw record | `results/bench/emubench-2026-11-18.json` |
| Report | `BENCHMARKS.md` §13 "Stock emulator baseline" |

## Layout

```
results/handoff/EMUBENCH/
  README.md                 this file
  scripts/                  copies of the harness, driver and gate at run time
  raw/phase-<phase>.json    per-phase documents (every sample, host_log, failures)
  raw/phase-<phase>.log     per-phase driver output
  logs/driver-<stamp>.log   the driver log: lock acquisitions, free -m per step,
                            postflight "no qemu left" verification
  gate/                     emu_gate.py output (table.md + gate.json)
```

## What was run

Four phases, each its own exclusive lock acquisition (`run-exclusive.sh
--exclusive` wrapping `tools/guarded.sh --mem 24G`):

| Phase | Instances | Rows |
|---|---|---|
| `n1-cold` | 1 | cold boot → `sys.boot_completed`, cold launch → first app frame, install |
| `n1-warm` | 1 | warm launch, tap→settled (counter, form), UI tree, screenshot, reset, PSS/RSS/anon+shmem + guest memory, idle CPU, warm boot from a snapshot |
| `n2` | 2 | boot, per-instance memory, CPU, verified counter tasks / GiB-minute |
| `n4` | 4 | same at N=4 |

Every phase ≥5 reps per row (cold 5 boots, warm 5, tap/tree/shot 10, reset 5,
memory 5 samples, CPU 3 windows, snapshot 5, N phases 5 reps × 3 tasks per
instance). Median and p90 are in the merged JSON (`rows.<metric>.p50/p90`) and in
`BENCHMARKS.md` §13.4.

## Acceptance checks

| Criterion | Check |
|---|---|
| No emulator left running | driver postflight prints `PASS no qemu-system process is running`; independently `ps -eo pid,args \| awk '/qemu-system/ && !/awk/'` is empty |
| Host RAM never above 90 % | every `free -m` line in `raw/phase-*.json` `host_log[]` (max `used_pct` is in `logs/`), and the guard aborts a start that would cross 85 % |
| ≤2048 MiB guest RAM | `config.guest_mb` in every phase document; `cmd-*.txt` |
| Same apps / screen as ACOS | `counter.apk` + `form-validation.apk`, 1080×2400 @ 420 dpi |
| 0 false passes | a task counts only when both live tree checks pass; failures are listed per rep in `reps[].failures` |

## Notes

* The emulator's screencap route is the measurement resolution: ~200 ms per frame
  for the launch rows, ~2 s per `uiautomator dump` for the tap and tree rows. Those
  resolutions are stated with the numbers; nothing is interpolated.
* A frame captured in one emulator boot differs from the same screen in another
  boot by 1–3 units per channel on every pixel (colour transform), so the first
  frame is detected as "app region differs from the idle launcher frame", and the
  correctness oracle is the live UI tree. See BENCHMARKS.md §13.3.
* ACOS has no install step (it ingests the APK instead of installing it into a
  system image), so the install row has no ACOS counterpart and is reported as
  NOT_MEASURED on the ACOS side — never as a win.
