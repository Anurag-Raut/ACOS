# PERF4 diagnosis — where the 8.7 ms went (measured, not guessed)

All numbers below come from `p4probe.py` / `p4trace.py` in this directory, run
inside a **shared** `run-exclusive.sh --task PERF4` slot (no `--exclusive`), so
they are indicative: the paired deltas within one run are what matter, not the
absolute level (host load moves both arms together).

Arms:

| arm | source | `acos` sha256 |
|---|---|---|
| `v022` | tag `v0.2.2` (`git archive` → `.cache/p4/v022-src`, own target dir) | see `03-ab/arms.txt` |
| `rel030` | `rel-030` HEAD before this branch (`5a912d4b`) | — |
| `e4` | `rel-030` at the PERF3 merge (`e4fb2424`), own target dir | — |
| `fix` | this branch (`perf4`) | see `03-ab/arms.txt` |

Each arm was built in place (`strings` check: the `v022` binary names
`.cache/p4/v022-src`, the `rel030`/`fix` binaries name this worktree and no
`v022-src` path), and `acos-art` was built into the same target dir so the
client finds its engine.

## 1. The RELGATE2 numbers are the *bound C1 store*, not the ack

From the exclusive gate run (`results/release/v0.3.0-gate/timing/02-ab/run-*/`,
n = 120 samples/arm, `samples.<metric>.extra`), M8 form fast:

| field | v0.2.2 | v0.3.0 RC | delta |
|---|---:|---:|---:|
| wall | 32.42 | 41.09 | +8.67 |
| `ack_ms` (guest's own publish→FINISHED) | 5 | 5 | 0 |
| `first_change_ms` (publish→changed probe sample) | 15.79 | 14.77 | −1.0 |
| `frame_ms` (publish→the **bound** C1 store) | 20.16 | 29.23 | **+9.07** |

M8 counter fast: `frame_ms` 23.14 → 27.46 (+4.3), `first_change_ms` 16.68 →
20.19 (+3.5). So the tap reply's own shaping, the ack and the probe are flat; the
reply waits for a *later* C1 store.

## 2. `frame_ms` was quantized by the host's poll, and the poll hid a 9 ms cost

`p4trace.py` records the host trace (`ACOS_ART_INPUT_TRACE`) plus the reply's own
`timing.input` split. One form-fast tap, publish-relative (µs from the
`tap publish_begin` line):

| event | v0.2.2 | rel-030 (as shipped) | fix |
|---|---:|---:|---:|
| `acks_returned total_ms` | 5 | 14 | 5 |
| first store seen by the settle loop | 20 | 15 | 15 |
| `probe rx` (changed tree) | 16 | 16 | 16 |
| bound store | 20 | 29 | 15 |

The 15 ms / 10 ms `thread::sleep` in the two settle loops meant the host could
only *observe* a store on its next tick: the same binary resolves `frame_ms` 16
or 29 depending on phase (`tr-fix3` vs `tr-fix2` runs of the same build; the
`rel030` run below). That is why M8 form fast is one tight band per arm instead
of a lattice.

The 9 ms that put rel-030 on the wrong side of the tick is **not** the settle,
the ack or the probe. Instrumenting the action path (`tap-split-rel030.txt`):

```
PERF4 stamp removal_mark_us=0
tap publish_begin at (157,616) ack_wait_ms=2500
PERF4 stamp entry_fnv_us=8989 none=true        <-- REL3H's pre-publish composite
tap acks_returned total_ms=14 seqs=[3, 4] acks=[(3, true, Some(5077)), ...]
```

`entry_fnv` (REL3H's fallback pre-action pixels) ran `capture_latest()` — a full
composite of every visible layer's buffer, 1080x2400 copied and fnv1a-hashed —
**before every tap whose caller had no capture of its own**, i.e. every tap after
a `pixels:false` reply. The bench's M8 form loop (`reset` → `tap` → `observe`)
is exactly that shape, so it paid 8.7–9.2 ms per tap (`tap-split-rel030-stamp-trace.log`: 9101, 9007, 8989, 9141, 9045, 9203 µs;
the same build's in-tap stamps in `tap-split-rel030.txt`). The value is only read by the
unhandled-input branch.

The 5 ms poll (guest acks land at ~5.07 ms, the loop drains at 0/5/10 ms) is a
second, smaller quantizer on the same path (`tap-split-v022.txt`: `publish_us=7..12 total_us=5070..5080`; rel-030's
`tap-split-rel030.txt`: `publish_us=10..13 total_us=5092..5111`, i.e. the tap
itself was never the cost — the composite ran before it).

## 3. The fast rule asked for the *next* frame

With the ack back at 5 ms, the shipped rule (`a C1 store observed after the
sample`) still bound the *following* store, because a draw's own buffer is stored
before the probe sample that describes it is parsed: rel-030 `frame_ms` 29–31
even with an eager loop (`tr-fix2`: `commit_gen=3 draw_seq=2 own_draw=false`).
The sample already carries the two counters that make the precise statement
possible (`generation` = C1 store count at parse, `draw_seq` = the guest's
`OnDraw` counter): when every draw has been stored and the store arrived after
that draw's own clock, the tree and the bound frame are one UI state − the
property the observation-order rule approximates. `tr-fix3`:
`commit_gen=2 draw_seq=2 own_draw=true → frame_ms=14..16`.

## 4. Per-arm result (interleaved, shared slot)

`diag-form-3arm.json`, 50 taps/arm, `diag-form-merged.json` /
`diag-counter-merged.json`, 90 taps/arm on the merged branch tree:

| metric (trimmed) | v0.2.2 | e4 (PERF3 merge) | rel-030 shipped | fix |
|---|---:|---:|---:|---:|
| M8 form fast wall | 32.52 | 34.54 | 34.57 / 41.09 (gate) | **29.01** |
| M8 form fast `frame_ms` | 20.17 | 20.59 | 21.0 / 29.2 (gate) | **14.95** |
| M8 counter fast wall | 30.76 | — | — | **24.90** |

`e4` is where the shared-slot data stops separating the arms (34.54 vs 34.57):
the poll quantization the two builds share dominates the +2 ms of real ack-path
difference, which is why the gate numbers flip by a whole step. Fixing the
quantizer (3) is what makes the remaining difference visible *and* small.

## 5. Reproduce

```bash
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
tools/art-runtime/run-exclusive.sh --task PERF4 -- \
  tools/guarded.sh --mem 8G --time 900 -- \
  python3 results/handoff/PERF4/p4probe.py \
    --arm v022=$PWD/.cache/p4/v022-target/release/acos \
    --arm fix=$PWD/.cache/p4-target/release/acos \
    --metric form --reps 30 --rounds 3 --scratch $PWD/.cache/p4/scratch \
    --out results/handoff/PERF4/diag-form-merged.json
```

`p4trace.py --taps N` does the same for a handful of taps and leaves the host
trace + the reply split in `--out`; `diagnosis/tap-split.txt` is one tap per arm
extracted from those traces, `tap-split-{v022,rel030}.txt` are the in-tap
`publish_us/total_us` stamps, and `tap-split-rel030-stamp-trace.log` is the
instrumented rel-030 trace that names the 9 ms composite.
