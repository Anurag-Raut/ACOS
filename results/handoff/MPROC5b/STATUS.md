# MPROC5b status (branch `mproc5b`, base `mproc5` @ `e6da5e1d` + `mproc6`)

**Step 1 (cross-process top-of-task on real ART) is NOT green yet: two root
causes were found and fixed, and the run that finally delivered the items
exposed a third, unrelated-looking window-session failure. Step 2 (the driver
death half) is implemented and host-tested. Steps 3 (idle kill) and 4 (the
scenario) are not started.**

| step | state |
|---|---|
| 1 per-process top-of-task on ART | **2 root causes fixed; still FAIL** — run2/run3 crash the newly-started process on `IWindowSession.relayout` (`unknown session`), see §"Run 2/3" |
| 2 driver death half | **done + host driver test passes** (`evidence-driver-test.txt`) |
| 3 idle kill | not started (design unchanged, `MPROC2/STATUS.md` §4) |
| 4 `scenarios/mproc3-two-process.yaml` | not written (a scenario for 1+3 that cannot pass would be a fake spec) |

## Step 1 — what run0/run1 measured, and the two causes it exposed

Baseline `evidence/run0-before` (mproc5 as committed): `proc-b-window-on-top`
FAIL, `proc-a-paused-or-stopped-under-b` FAIL, everything else in the fatal set
PASS. Nothing in the session ever queued a `PauseActivityItem`.

1. **The AIDL boolean is not "resumed".** The image's
   `IActivityClientController.activityResumed(IBinder token, boolean
   handleSplashScreenExit)` was read as `resumed = args[1]`. Every real resume
   therefore stored `resumed = false` and `top_of_task` never ran. The callback
   *is* the resume; the handler now always calls `activity_resumed(token, true)`.
   (Dex evidence: `dexdump -d` of the image's `IActivityClientController`
   `activityResumed` descriptor; the AIDL table already carries
   `(Landroid/os/IBinder;Z)V`.)
2. **The adopted record could never leave `Uncreated`.** A guest-minted activity
   token (W38) is first seen in `activityResumed`; the record was created in
   `Uncreated` and no transition was applied, so the next honest report —
   `activityStopped` from a finishing activity — failed with `illegal
   transition Stop from Uncreated` (measured for proc-b's `finish` in run1).
   `activity_resumed` now establishes `Create → Start → Resume` for an adopted
   record (idempotent for a repeated report). New test
   `an_adopted_token_resumes_through_create_start_and_resume`.
3. **The `ClientTransaction` wire shape** (found statically, then merged with
   MPROC6's one encoder): the image's `writeToParcel` writes
   `writeBoolean(mActivityToken != null)` + binder only when present,
   `writeParcelable(mLifecycleStateRequest)`, then
   `writeBoolean(mActivityCallbacks != null)` + list only when present. MPROC5
   wrote `Bool(false)` then a binder, and `Bool(false)` then `List(None)`
   (`-1`), i.e. a null token plus 4 stray bytes. `FieldOp::BoolCond` +
   `ClientTransaction` spec now encode the image's presence markers exactly;
   `the_lifecycle_transaction_parcels_exactly_like_the_image` decodes with the
   guest's reader order and asserts **0 trailing bytes**. The merge with MPROC6
   (`29f696e5`) replaced the item/transaction constructors with the single
   `client_transaction.rs` encoder; `lifecycle_parcelable` delegates to it.

## Run 2/3 — the items are delivered, and the new process crashes

`evidence/run2` (real ART, merged tree): B spawns, binds and resumes; then
process 1 dies with

```
E/AndroidRuntime: LOOPER_THREW
java.lang.IllegalArgumentException: internal service error:
  android.view.IWindowSession: unknown session 2
  at android.view.IWindowSession$Stub$Proxy.relayout
```

and `acos-binder-peer: UNSUPPORTED_ANDROID_API … IWindowSession::relayout …
unknown session 2` (code 5). `WmState::relayout` returns `UnknownSession` when
the window record's `session` differs from the caller's, so the second process's
window record and its `IWindowSession` disagree. This is the *next* root cause,
not a pass and not the launch-encoder path: before MPROC5b no `Pause`/`Stop`
ever reached the guest, so this ordering (B on top while A is still windowed)
was never exercised on ART.

Both `run2` and `run3` reproduce it deterministically (3 `unknown session`
hits in `run3`), so it is not a flake. **Root cause located in the window
layer**, not in the launch encoder: the host registers exactly **one**
`android.view.IWindowSession` node (`registry.rs` → `WmsState::host_session`)
and `WindowSessionService` serves every process through it, while
`WmsState.session` holds only the **last** `openSession` id
(`crates/acos-services/src/wms.rs::open_session`). So proc-a's window is added
under session 1, proc-b's `openSession` overwrites `session = 2`, and proc-a's
next `relayout` is validated against session 2 → `WmError::UnknownSession(2)`
(`wms.rs::relayout`: `record.session != session`). One process never noticed
because the single value was always its own.

Fixing it means the session must be resolved **per caller**: either one host
`IWindowSession` node per session (the `openSession` reply must carry the
session's own binder, and the host object registry must be able to serve N
of them), or the caller's process tag must reach `ServiceObject::raw_transact`
and select the session from it. `ServiceObject`/`ServiceHost::transact` today
carry **no caller identity** (`host.rs`), so both options change the
binder/service-layer seam — flagged for the orchestrator, not worked around
here (the alternative, keying the session by window, would silently accept a
relayout from the wrong process).

## Step 2 — the driver death half (done)

`crates/acos-binder-peer/native/binder_driver.c`:

- `BC_REQUEST_DEATH_NOTIFICATION`/`BC_CLEAR_DEATH_NOTIFICATION` now parse the
  packed `binder_handle_cookie` and keep `(handle, cookie)` registrations
  (previously the 12 bytes were skipped as "peer liveness not modelled").
- `F_DEATH` (already emitted by the peer since MPROC3, `target` = peer node)
  queues one `BR_DEAD_BINDER` per registration for every local handle of that
  node, carrying **that exact cookie**; a looper thread delivers them
  (`BR_DEAD_BINDER` is now decodable by `br_cmd_len`, so `flush_pend` never
  splits it). The registration is one-shot; `BC_DEAD_BINDER_DONE` is consumed.
- A dead-owner refusal (`F_RSP` with the new `protocol::RSP_DEAD` flag, set by
  `forward_foreign` for a dead owner and by `release_foreign_of` for requests
  still waiting on it) reaches the caller as `BR_DEAD_REPLY`
  (`DeadObjectException`), not `BR_FAILED_REPLY`.
- `native/driver_test.c` (new) compiles the driver's portable core on the host
  (`E3G_HOST_TEST`, which leaves out the bionic/JNI substrate) and drives the
  three shapes above against a socketpair peer. `evidence-driver-test.txt`:
  `driver_test: PASS (0 failure(s))`. The NDK build still succeeds
  (`libacos_binder_driver.so` built in the acceptance run).

## Tests

`cargo test -p acos-services -p acos-binder-peer`: **56 binaries, 412 tests
passed, 0 failed** (`evidence-tests.txt`). `cargo test -p acos-art --no-run`
green after fixing the two MPROC5 test-only compile errors
(`drain_stdout_lines` tag argument, `ProbeState` literal fields).

## Next action (smallest, in order)

1. Reproduce `run3`; if the `unknown session` crash is consistent, fix it at
   the WMS/`IWindowSession` session identity for a second process (the record's
   session must be the session the `openSession` reply actually handed that
   process), then rerun this driver. This is the only thing between step 1 and
   green: the `Pause`/`Stop` items are now encoded and delivered.
2. Step 3 (idle kill) per `MPROC2/STATUS.md` §4, then the
   `scenarios/mproc3-two-process.yaml` (`engine: art`) for 1+3.
