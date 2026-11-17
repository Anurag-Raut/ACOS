# MPROC6 status (branch `mproc6`, base `mproc4` @ `5efe9869`, merged `mproc5` @ `e6da5e1d`)

**The host builds launch transactions.** One encoder in
`crates/acos-services/src/client_transaction.rs` writes
`android.app.servertransaction.ClientTransaction` + every item
(`LaunchActivityItem`, `Resume`/`Pause`/`Stop`/`DestroyActivityItem`,
`ActivityResultItem`, `NewIntentItem`) from the **image's** framework dex, checked
byte-for-byte against real Java in both directions. MPROC5's lifecycle encoder now
calls the same code (no second implementation).

| item | where |
|---|---|
| `ClientTransaction`, `LaunchActivityItem`, lifecycle + callback items | `crates/acos-services/src/client_transaction.rs` |
| item specs (dex-verified) | `crates/acos-services/src/parcelable.rs` |
| narrow ATMS call site | `AtmsState::launch_delivery` / `AtmsState::encode_launch` |
| Rust tests | `crates/acos-services/tests/client_transaction.rs` |
| real-Java differential | `crates/acos-services/tests/client_transaction_java.rs` + `tests/java/ClientTransactionCompareMain.java` (+ 16 `@hide` stubs) |
| MPROC5 lifecycle delivery now on this encoder | `crates/acos-services/src/atms.rs` (`ClientTransactionItem::to_parcelable`, `ClientTransaction::lifecycle_parcelable`) |

## The wire shape (image dex, `.cache/boot/android-34/dex/framework`)

`ClientTransaction.writeToParcel` — `dexdump -d` of `classes.dex`, method
`Landroid/app/servertransaction/ClientTransaction;->writeToParcel`:

```text
0x0000 iget-object v0, mActivityToken
0x0009 writeBoolean(v0 != null)                          # writeActivityToken
0x000c if (v0 != null) writeStrongBinder(mActivityToken)
0x0015 writeParcelable(mLifecycleStateRequest, flags)
0x0018 iget-object v3, mActivityCallbacks
0x001e writeBoolean(v3 != null)                          # writeActivityCallbacks
0x0021 if (v3 != null) writeParcelableList(mActivityCallbacks, flags)
```

The image's class has **no `mIsGlobal` and no `mIsReady` field** (fields:
`mActivityCallbacks`, `mActivityToken`, `mClient`, `mLifecycleStateRequest`).
The spec was corrected to two `FieldOp::BoolCond` presence groups (a new `BoolCond`
op: a `writeBoolean` marker, wire-identical to `Cond`'s `writeInt`, but a distinct
dex call so the image scan stays strict).

**MPROC5's open question — empty list or null for `mActivityCallbacks`:** *neither*.
`ClientTransaction.obtain` leaves `mActivityCallbacks` null and only `addCallback`
allocates it, so a **lifecycle-only** transaction writes the presence `false` and
then **no list at all** (MPROC5's `Value::List(None)` wrote four extra `-1` bytes
after the flag). A transaction with callbacks writes `true` + the list. Both are
pinned by the differential cases `client_transaction` and
`client_transaction_lifecycle_only` and by the corrected spec's dex scan.

Same class of finding in the launch item: `mPendingResults`/`mPendingNewIntents`
are **null** (`writeTypedList` → `-1`) when nothing is pending, not empty lists.

### `LaunchActivityItem`: 21 fields, not 23

The image's `writeToParcel` ends at `mTaskFragmentToken`. `initialCallerInfoAccessToken`,
`activityWindowInfo` and `sceneTransitionInfo` exist in the AOSP source cache but
**not in this image** (`LC_ALL=C grep -c initialCallerInfoAccessToken classes.dex`
→ 0; the same for `activityWindowInfo`, `sceneTransitionInfo`), so the host writes
21 fields. `CompatibilityInfo` is not an item field on this build either — it
travels in `bindApplication`, so no second copy is invented here.

`ResumeActivityItem` on this image has `obtain(IZZ)` and `obtain(ZZ)` (no
`obtain(IZZZ)`); `obtain(IZZ)` sets `mUpdateProcState = true` (bytecode read), so
the host's resume writes `(procState=2, update=true, isForward=false,
fakeFocus=false)`.

The launch `Configuration` is **exactly** the one `bind_app::build_config`
produces (locale `en-US`, display density/dp, `fontScale` 1.0, portrait, every
other field the framework's `new Configuration()` default — `userSetLocale` false,
`screenLayout = LAYOUTDIR_LTR` from `setLocales`, empty window configuration,
`rotation = -1`, `fontWeightAdjustment = i32::MAX`). It must stay byte-identical to
`bindApplication`'s config or the guest sees a spurious
`onConfigurationChanged`; carrying the display bounds is a change for *both*
paths (flagged below, not done here).

## Narrow ATMS integration (no routing)

```rust
pub fn AtmsState::launch_delivery(&self, token: u64, app: &pms::AppInfo,
                                  activity: &pms::ActivityInfo,
                                  activity_token: BinderId) -> Result<LaunchDelivery>
```

`LaunchDelivery::Ready { process_tag, method: "scheduleTransaction", parcel }`,
`::NoPendingLaunch`, `::ProcessNotRunning`. It finds the `PendingLaunch` recorded
for `token`, the **attached** process record whose package owns the activity, and
returns the marshalled `ClientTransaction{LaunchActivityItem + ResumeActivityItem}`.
Routing the delivery and the lifecycle that follows are MPROC5's / the
orchestrator's; no existing call site was changed.

## Tests

`cargo test -p acos-services`: **50 test binaries, 0 failures** (evidence:
`evidence-tests.txt`), including:

* `tests/client_transaction.rs` (8): every item decodes/re-encodes to the same
  bytes, extras (+ a parcelable extra) survive, a present-only extra is a
  structured refusal, `launch_delivery` targets the running process and carries
  the extras, `NoPendingLaunch`/`ProcessNotRunning`, the process
  `Configuration`'s values, bundle `ArrayMap` order.
* `tests/client_transaction_java.rs` (1, real Java): for `intent`,
  `configuration`, `activity_info`, `result_info`, `referrer_intent`,
  `launch_activity_item`, `launch_activity_item_with_result`,
  `resume`/`pause`/`stop`/`destroy_activity_item`, `activity_result_item`,
  `new_intent_item`, `client_transaction`, `client_transaction_lifecycle_only`:
  Java `writeToParcel` bytes == Rust bytes, and the real
  `CREATOR.createFromParcel` reads the Rust bytes back (field checks incl. the
  string/int/bool/**parcelable** extras and a pending `ResultInfo`), re-emitting
  them identically.
* `tests/parcelable.rs`: the new item specs (`DestroyActivityItem`,
  `ActivityResultItem`, `NewIntentItem`, `ReferrerIntent`) plus the corrected
  `ClientTransaction`/`ActivityRelaunchItem` are re-verified against the image dex
  (`Full`; `ReferrerIntent` is `Curated` because the linear scanner records only
  its trailing `writeString`, so the differential pins its order).
* `tests/mproc5_top_of_task.rs` (4) updated to the corrected `ClientTransaction`
  shape and still green.

## Follow-ups (orchestrator)

1. **`acos-art` tests do not compile on `mproc5`** (`git merge mproc5`):
   `launch.rs:7725 drain_stdout_lines` takes 5 arguments but 4 are passed, and
   `launch.rs:7737 ProbeState` is missing `by_process`/`top_process`. Not
   introduced here (`cargo check -p acos-runtime --all-targets` is clean); the
   mproc5 author needs to finish those test edits.
2. Routing `launch_delivery` (MPROC5/orchestrator): the `activity_token` binder
   passed in must be the record's published token — the same binder the guest
   returns in `activityResumed`.
3. Both `bind_app::build_config` and this encoder carry an empty
   `WindowConfiguration`; giving both the display bounds is one change for a
   later step (it changes what the app sees at bind time).
