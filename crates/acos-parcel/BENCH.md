# acos-parcel micro-benchmarks

Regenerate with:

```sh
cargo bench -p acos-parcel
```

The launch trace called `nativeReadInt` alone ~1,835 times and ~27 `Parcel`
natives total, so per-operation cost is on the hot path for both the outgoing
AIDL proxies and the incoming `onTransact` handlers.

## Environment

| | |
|---|---|
| CPU | AMD Ryzen 9 9950X 16-Core |
| Toolchain | rustc 1.90.0, cargo 1.90.0 |
| Profile | `--release` (`debug = 1`), `taskset -c 4` |
| Harness | `benches/parcel_bench.rs`, 401 samples x 2,000 iterations/sample, p50/p95 over per-sample averages |

## Results

| Benchmark | p50 (ns) | p95 (ns) | mean (ns) |
|---|---:|---:|---:|
| writeInt32 | 1.14 | 1.17 | 1.18 |
| readInt32 | 1.11 | 1.11 | 1.11 |
| writeString16 (40 units) | 35.29 | 40.77 | 36.82 |
| readString16 (40 units) | 49.99 | 50.99 | 50.09 |
| 100-call AIDL replay (per call) | 65.02 | 69.74 | 66.42 |

100-call replay total: p50 6,502 ns, p95 6,974 ns.

## What the replay is

`synthetic_call` reproduces the write side of one measured AIDL transaction:
`writeInterfaceToken(40-char descriptor)` + a strong binder + two `int32`
arguments + a 12-unit `String16` + `marshall()`. That is the shape of the
`IDisplayManager.getDisplayInfo`, `IActivityClientController.activityResumed`
and `IWindowSession.relayout` calls in `binder_calls_launch.tsv`.

## Notes

* `writeString16`/`readString16` are dominated by the unavoidable
  allocation/validation of a UTF-16 string. The writer writes straight into the
  parcel buffer (no scratch `Vec`), which keeps it ~35 ns for a 40-unit
  descriptor; the reader's `String::from_utf16` allocation puts it at ~50 ns.
* `readInt32` remains ~1 ns even though the trace hits it 1,835 times at
  launch: a full launch's worth of scalar reads is < 2 us.
* The 100-call replay is ~6.5 us, i.e. well under a frame budget; the cost is
  string encoding, not the binder/object bookkeeping.
* Numbers were stable across runs; the p95 for the replay occasionally shows a
  scheduler artefact (~7.5 us total) when a sample is preempted. Run with
  `taskset -c <cpu>` for the cleanest numbers.
