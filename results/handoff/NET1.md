# NET1 handoff (NET1B checkpoint)

**Branch** `compat-net1-connectivity` (base `main` `3e11d9c`). Worktree
`/home/Qui/.paseo/worktrees/3oxa6xao/s3ab`. Stay on this branch; do not push.

**One line:** the connectivity implementation from NET1 is now *verified*: four
crate suites green, 8/8 positive gates and 5/5 Gaten flows pass with 3/3 + 5/5
negatives failing as designed, `k1-run.py` persists the guest logd, and corpus
round 1 (42 apps × 2 arms) shows **two apps changed, both upward, none down** —
Joplin gains `first_frame`+`tree`, Nextcloud gains `process_start`+`oncreate`
(both were blocked by `IConnectivityManager code 42` on main) and then dies in
its own SQLite cursor wall. Corpus round 2 was in flight at the window close.

**Resume here:** `ROUND=2 BATCH=6 TIME=800 bash .cache/net1b/corpus.sh` (the
script is resumable — it skips nothing but overwrites per-app dirs), then
`python3 .cache/net1b/rung.py --rounds 1,2 --markdown`. Expect only joplin and
nextcloud to differ; anything else is a finding.

## State of the tree

| path | what |
|---|---|
| `crates/acos-services/src/connectivity.rs` | the `IConnectivityManager` state machine + declared default network (NET1) |
| `crates/acos-services/src/parcelable.rs` | the four parcelable specs + `write_parcelable_bundle` |
| `crates/acos-services/tests/net1_connectivity.rs` | 10 handler tests through the real dispatcher |
| `tools/compat/k1-run.py` | **changed in NET1B**: persists the guest logd as `06-logs.json` (`--logs-limit`, `--no-logs`) |
| `results/compat/NET1/run{1,2,3}/` | the NET1 Joplin launches |
| `results/compat/NET1/netinfo-run/` | the NET1B Joplin launch + `logd.txt` (266 records) |
| `results/compat/NET1.md` §7 | the NET1B measurements |

## Reproducing the verification (all inside this worktree)

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
# tests (binaries must be built first: cargo test --release --no-run -p ...)
tools/art-runtime/run-exclusive.sh --task NET1B -- tools/guarded.sh --mem 10G \
  --time 560 -- .cache/net1b/run_tests.sh acos-services acos-verify
tools/art-runtime/run-exclusive.sh --task NET1B -- tools/guarded.sh --mem 10G \
  --time 900 -- .cache/net1b/run_tests.sh acos-art
# gates / Gaten
tools/art-runtime/run-exclusive.sh --task NET1B -- tools/guarded.sh --mem 10G \
  --time 560 -- .cache/net1b/run_suite.sh gates 1            # all 11
tools/art-runtime/run-exclusive.sh --task NET1B -- tools/guarded.sh --mem 10G \
  --time 560 -- .cache/net1b/run_suite.sh scenarios 1
tools/art-runtime/run-exclusive.sh --task NET1B -- tools/guarded.sh --mem 10G \
  --time 560 -- .cache/net1b/run_suite.sh negatives 1
# corpus (both arms, interleaved per app)
ROUND=1 BATCH=6 TIME=800 bash .cache/net1b/corpus.sh
python3 .cache/net1b/rung.py --rounds 1 --markdown
```

Scripts live in `.cache/net1b/` (gitignored): `run_tests.sh`, `run_suite.sh`,
`chain.sh <stage>...`, `corpus.sh`, `joplin.sh`, `rung.py`.

## Behaviour that is easy to break

1. `requestNetwork` with `TRACK_DEFAULT`/`TRACK_SYSTEM_DEFAULT` **must ignore the
   caller's capabilities** and answer the default network's.
2. Callback messages carry the *same* `NetworkRequest` bytes as the reply.
3. Callback bundles use `write_parcelable_bundle`, not `write_bundle`
   (`VAL_PARCELABLE` has no length word in API 34).
4. `NET_CAPABILITY_VALIDATED` stays unset (owner decision, not an agent one).
5. `acos-art`'s cgroup test needs a **cold** `ACOS_ART_DATA` to exercise its own
   probe path (see `results/compat/NET1.md` §7.1).

## Next actions (in order)

1. Finish the corpus: round 1 (`ROUND=1 BATCH=6 TIME=800 bash
   .cache/net1b/corpus.sh`), then round 2 (`ROUND=2 …`), then
   `python3 .cache/net1b/rung.py --markdown` for the per-app rung table and the
   diff. Watch for an app that moves *down* a rung now that
   `getActiveNetwork`/`getNetworkCapabilities` answer non-null.
2. Quote the JS NetInfo value: a NetInfo probe APK that logs
   `state.isConnected`/`isInternetReachable`, or an ACOS-side trace of the
   `IConnectivityManager` transactions `RNCNetInfo` receives.
3. Report `write_dynamic`/`read_dynamic`'s `VAL_PARCELABLE` length word (NET1 §2)
   as its own fix with a test — untouched here.
