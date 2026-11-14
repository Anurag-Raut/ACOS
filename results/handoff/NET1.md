# NET1 handoff

**Branch** `compat-net1-connectivity` (base `main` `3e11d9c`). Worktree
`/home/Qui/.paseo/worktrees/3oxa6xao/s3ab`.

**One line:** RN NetInfo's `requestNetwork`/`registerDefaultNetworkCallback`
path is implemented against AOSP's `NetworkCallback` protocol; Joplin launches
3/3 to `activity_resumed` + a published frame with no SIGABRT. Gates, crate
suites and the 2×42 corpus are **not** run yet.

## Files

| path | what |
|---|---|
| `crates/acos-services/src/connectivity.rs` | **new**: the whole `IConnectivityManager` state machine, the emulated default network, `NetworkCapabilities`/`NetworkRequest`/`LinkProperties`/`NetworkInfo` builders, `Message` encoder, AOSP callback sequence |
| `crates/acos-services/src/parcelable.rs` | + `ParcelableSpec`s for `android.net.NetworkCapabilities`, `android.net.NetworkRequest`, `android.net.LinkProperties`, `android.net.NetworkInfo`; + `write_parcelable_bundle` (framework-exact `VAL_PARCELABLE`, no length word) |
| `crates/acos-services/src/declared.rs` | the old `ConnectivityService`/`ConnectivityState` removed (one implementation only) |
| `crates/acos-services/src/registry.rs`, `src/lib.rs` | wire the new service |
| `crates/acos-services/tests/net1_connectivity.rs` | **new**: 9 handler tests through the real dispatcher |
| `results/compat/NET1.md` | full report, AOSP citations, evidence |
| `results/compat/NET1/run{1,2,3}/` | the three Joplin runs' `report.json`, `01-launch.json`, `session-exit.json`, `unsupported.jsonl` |
| `EXPERIMENTS.md` | NET1 entry |

## Behaviour that is easy to break

1. `requestNetwork` with `TRACK_DEFAULT`/`TRACK_SYSTEM_DEFAULT` **must ignore the
   caller's capabilities** and answer the default network's. RN sends `null`
   there; the first cut echoed the (null) request and panicked in the bundle
   writer.
2. Callback messages carry the *same* `NetworkRequest` bytes as the reply —
   `ConnectivityManager.sCallbacks` is keyed by `NetworkRequest.equals`.
3. A callback bundle must be written with `write_parcelable_bundle`, not
   `write_bundle` (see the `VAL_PARCELABLE` length-word note in NET1.md §2).
4. `NET_CAPABILITY_VALIDATED` stays unset (no reachability claim). Changing that
   is an owner decision.

## Next actions (in order)

1. Run the gate battery + Gaten flows/negatives on this tree (copy
   `.cache/wt-int/.cache/int/run_suite.sh` into `.cache/net1/`, retarget
   binaries/outputs to `.cache/s3ab-target/release` and `.cache/net1/suite`).
2. Crate suites: `acos-services`, `acos-cli`, `acos-verify`, and `acos-art` with
   the documented flake recipe.
3. Persistent guest logd in `tools/compat/k1-run.py` → grep RN NetInfo
   `isConnected` for one Joplin launch (closes acceptance 2b).
4. 2×42-app corpus, interleaved by app, this branch vs `main`; per-app rung
   diff; watch for apps that now block on a socket because a network exists.
