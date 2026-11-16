# LAYER1 single-window latency A/B — bench owner runbook

The driver is `tools/bench/layer0-single-window-ab.sh`. It was **not run** by
the implementer: timed A/Bs are batched by the bench owner.

## Arms

| arm | build | binary |
|---|---|---|
| A (base) | `main` / `layer0` commit `6b00f943` | `<checkout>/target/release/acos` |
| B (stack) | `layer1` HEAD | `<layer1-worktree>/.cache/layer1-target/release/acos` |

Both must be `--release` builds of the commit their `--*-test-dir` names; the
driver refuses identical bytes and records both sha256s. Build each with the
track's `CARGO_TARGET_DIR` (the ART binary `acos-art` must sit next to `acos`,
or point `ACOS_ART_BIN` at it).

## Command

```bash
tools/bench/layer0-single-window-ab.sh \
  --a-bin /path/to/main/target/release/acos \
  --b-bin /path/to/layer1/.cache/layer1-target/release/acos \
  --a-test-dir /path/to/main \
  --b-test-dir /path/to/layer1 \
  --reps 3 --latency-reps 20 --fast-reps 40 \
  --task LAYER1-AB
```

It runs `acos-bench.sh --phases latency,cold` per arm with the BENCHFIX bundle
(3 reps, 20 latency reps, 40 fast samples), then `ab-compare.py` (old gate +
lattice view) and the C1 one-layer composite micro-check
(`cargo test --release -p acos-services --test w35_composite
a_single_full_window_layer_passes_through_with_its_own_stride`). Output:
`results/bench/layer0-ab-<UTC>/{a,b}.json`, `ab.txt`/`ab.json`, `microcheck-*.txt`,
`report.json`.

## Pass rule

* every `latency`/`cold` metric inside the documented gate (max(category floor,
  2× rep spread) capped at 40 %, plus the absolute floor) and the lattice
  verdict not `REGRESSION` on M4/M6/M8 fast;
* the one-layer composite micro-check green on both arms (the stack must not
  put a two-layer path under a single-window app).

`acos-bench.sh` takes the exclusive lock per group; run it on an otherwise idle
host and delete the per-arm scratch after the report.
