# ACOS VM results — units, methodology, and honest labelling

This directory holds machine-readable artifacts produced by the ACOS Rust DEX
interpreter (`crates/acos-vm`).

| File | Produced by | Meaning |
|---|---|---|
| `dexprogs-normal.json` | `tools/diff-dexprogs.sh` | Per-program PASS/FAIL/UNSUPPORTED for the 20-program corpus in default mode, with the ACOS metrics block. |
| `dexprogs-gc-stress.json` | `tools/diff-dexprogs.sh --gc-stress` | Same corpus with `ACOS_GC_STRESS=1` (a stop-the-world collection before every allocation). |
| `opcode-coverage.json` | `cargo test -p acos-vm` (`tests/dexprogs.rs`) | Which defined Dalvik opcodes the corpus actually executed, which are implemented but unexercised, and which are unsupported. |
| `bench-<timestamp>.json` | `tools/bench-vm.sh` | Micro-benchmark timings/RSS. |

## Fidelity labels

- The expected program outputs in `fixtures/expected/` are **`jvm-reference`**
  values captured from the host OpenJDK 17. A pass is a match against that weak
  oracle, **not** Android/ART verification.
- `acos run-dex` reports `backend = "acos-native"` and
  `fidelity = "native-runtime"`: the Rust interpreter really executed the DEX.

## Benchmarks — units and methodology

`tools/bench-vm.sh` measures:

- **wall time**: monotonic clock around process `start -> exit`, in **milliseconds**;
  p50 is the median, p95 is the nearest-rank 95th percentile over
  `ACOS_BENCH_REPS` (default **20**) samples after 2 warmup runs.
- **peak RSS**: `os.wait4` `rusage.ru_maxrss` on Linux, reported in **KiB**.
- **instructions/second**: for ACOS only, `metrics.instructions` from one
  `--json` run divided by the p50 wall time.

### The comparison target is the HOST JVM

The baselines are the **host OpenJDK 17 JVM** (default JIT), `java -Xint`, and
`java -Xshare:auto`. They are convenient and reproducible on this machine.

They are **not** Android ART. No number here is an Android-verified or
device-verified number, and none should be quoted as one. In particular,
`java -Xint` is not ART's interpreter, and the JVM's JIT is not ART's JIT.

There is no special-casing of the benchmark programs: the same interpreter and
core-library paths that run `fixtures/dexprogs` run these benchmarks.
