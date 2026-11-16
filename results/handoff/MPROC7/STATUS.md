# MPROC7 status (branch `mproc7`, base `mproc5c` @ `a3ff535b`)

**Step 0 is done: `acos-art --test launch_hang::ordinary_launch_to_application_created_stays_fast`
was red on the mproc chain for a real reason, not the environment.** The launch
delivery facts (`bind_bytes`, `delivered`, `txn_*`) were stored in the
per-connection `LaunchState` (MPROC2) and wiped by `disconnect_process`. The
launcher classifies the run *after* the guest ended (`PeerSummary`), so a guest
that quits the looper right after `bindApplication` (the `until=application-created`
watcher does) made the launcher report `application_created=false` /
`GUEST_EXITED` even though the bind had been delivered.

On `rel-030` the same facts lived directly on `Peer` and were never cleared
(`git show rel-030:crates/acos-binder-peer/src/peer.rs | grep launch_delivered`),
so that branch cannot show this failure. The fix keeps the facts across the
disconnect; a reconnecting tag gets a fresh `ProcessConn` from
`register_process`, so nothing stale is replayed.

| step | state |
|---|---|
| 0 launch_hang red? | **green again** — `results/handoff/MPROC7/launch-hang-mproc7-after.log` (`test result: ok. 1 passed`), regression test `acos-binder-peer::mproc2_process_tags::launch_delivery_facts_survive_the_guest_connection` |
| 1 launch into a running process | in progress |
| 2 startActivityForResult | not started |
| 3 singleTop/newIntent | not started |
| 4 Bundle VAL_* + Stop-after-Destroy | not started |
| 5 orchestrator decisions | not started |

Commits: see `git log --oneline mproc5c..mproc7`.
