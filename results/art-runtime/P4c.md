# P4c — the `IContentProvider` binder node is *served*: `Activity.onCreate`/`onResume` run through the real `ClientTransaction`

**Status: the P4b wall is gone.** The `settings` provider binder the holder names is now
reachable from the guest (`android.content.IContentProvider`, the exact node
`ProviderRegistry`/`ServiceHost` hands out), `PhoneWindow.<init>` gets its
`Settings.Global` value, and the counter fixture's **`AcosCounter: onCreate / onStart /
onResume`** lines appear in logd — i.e. `Activity.attach` → `onCreate` → `onStart` →
`onResume` all ran inside the real `ActivityThread`, driven by the real
`ClientTransaction{LaunchActivityItem, ResumeActivityItem}` delivered by the peer.
Nothing committed.

The root cause was **not** a missing provider handler (Q2's `call`/`query` were already
there): it was a **file-descriptor ownership bug in the peer** that turned the provider's
`call` reply into a reply that could never be sent, so the guest's main thread blocked in
`BinderProxy.transactNative` forever. §2 has the byte-level story. The next wall is now
named exactly (§5): `IWindowManager.openSession` (code 4) still raises
`UNSUPPORTED_ANDROID_API`, so `ViewRootImpl.<init>` throws in `handleResumeActivity`
**after** `performResumeActivity` — the activity really is resumed, the window is not.

```bash
# the counter reaches Activity resumed (onCreate/onStart/onResume), then names the next wall
tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art launch \
    --apk fixtures/build/apps/counter.apk --until activity-resumed \
    --out results/art-runtime/p4c/activity
# outcome=crash code=GUEST_EXIT_AFTER_RESUME wall=121..152ms
# application_created=true activity_resumed=true txn=Some(2108)/delivered=true calls=51

# the frame is untouched (P3c renderer)
tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art render-app \
    --apk fixtures/build/apps/counter.apk --out results/art-runtime/p4c/counter
# md5 9ec17f055843a2b9d18078ec595e9300 (== P3c), ssim=0.99351 tree=12/12 structural=8/8
```

## 0. Acceptance

| requirement | result |
|---|---|
| the holder's provider binder is a node the peer serves (`android.content.IContentProvider`) | **PASS** — `getContentProvider("settings")` (code 30) is answered by `AtmsService`/`ProviderRegistry`; the holder's `provider` is `ServiceHost`'s registered `android.content.IContentProvider` binder, encoded as peer node `0x200000024`; the caller's `IContentProvider.call` (code 21) reaches that node and is answered from `SettingsState` (§1, §3) |
| incoming transactions are answered through `ServiceHost`/`ProviderRegistry` (`call`, `query`, `getType`, …) | **PASS for `call`**; `query` is the same `ServiceHost::transact` route (Q2 answers it with a real `BulkCursorDescriptor` + `CursorWindow`) — the launch does not issue one; `getType`/`insert`/`update`/`delete` remain structured `UNSUPPORTED_ANDROID_API` (Q2 §5), never a hang |
| bulk-cursor reply for `query` | **not exercised by this launch** (no `ContentResolver.query` in `PhoneWindow.<init>`); the code path is Q2's `cursor.rs`, unchanged and tested there |
| `Activity.onCreate` **and** `onResume` of the counter fixture run | **PASS** — `AcosCounter: onCreate`, `onStart`, `onResume` in logd; `activity-resumed.marker` carries `lifecycle=onCreate,onStart,onResume resumed=true called=true window=PhoneWindow@…` (§1) |
| reached through the real `ActivityThread`/`ClientTransaction` path | **PASS** — paired looper trace `>>>>> … H … 110` / `<<<<< Finished` (BIND_APPLICATION) and `>>>>> … H … 159` (EXECUTE_TRANSACTION); `ResumeActivityItem.execute` ran; no framework class was replaced |
| frame through the existing P3c renderer, md5 `9ec17f…`, SSIM ≥ 0.99 | **PASS** — md5 identical to P3c, SSIM **0.99351**, decor band 0.99762, tree **12/12**, structural **8/8** |
| `cargo test -p acos-art -p acos-binder-peer` | **PASS** — 39 + 6 (`acos-art`), 3 + 13 (`acos-binder-peer`), 0 failed; one new regression test (§4) |
| every ART run under `tools/guarded.sh --mem 4G --time 300`, one ART process at a time, scratch under `.cache/`, no sudo, no commit | **PASS** |

`crates/acos-services/src/parcelable.rs` (D1's concurrent edit) **compiled throughout this
task's window**; the last `cargo test -p acos-art -p acos-binder-peer` ran against it and is
green. No `acos-services` file was touched by P4c (not even a line).

## 1. What the run now does (real path, in order)

```text
ACOSLAUNCH TRACE >>>>> Dispatching to Handler (android.app.ActivityThread$H) … : 110
ACOSLAUNCH TRACE <<<<< Finished to Handler (android.app.ActivityThread$H) …
ACOSLAUNCH MARK application-created package=dev.acos.fixtures.counter
ACOSLAUNCH MARK transaction-marshalled items=launch+resume activity=dev.acos.fixtures.counter.MainActivity bytes=2108
ACOSLAUNCH MARK schedule-transaction-pushed bytes=2108 token=local-binder
ACOSLAUNCH TRACE >>>>> Dispatching to Handler (android.app.ActivityThread$H) … : 159
ACOSLAUNCH MARK activity-resumed activity=dev.acos.fixtures.counter.MainActivity window=com.android.internal.policy.PhoneWindow@eacd690 recorded_after=looper-threw
```

`activity-resumed.marker`, verbatim:

```
package=dev.acos.fixtures.counter
activity=dev.acos.fixtures.counter.MainActivity
lifecycle=onCreate,onStart,onResume
resumed=true
called=true
stopped=false
window=com.android.internal.policy.PhoneWindow@eacd690
post_resume_failure=java.lang.UnsupportedOperationException: unsupported Android API: android.view.IWindowSession code 4 (openSession)
```

The lifecycle lines are the fixture's own code (`dev.acos.fixtures.counter.MainActivity`
logs `AcosCounter: onCreate/onStart/onResume`); the marker fields are read off the real
`Activity`/`ActivityClientRecord` by reflection, not written by the fixture.

### 1.1 The peer now *pushes* the resume out of the raw state (not a race)

`handleResumeActivity` performs `performResumeActivity` (→ `Activity.onResume`, `mResumed =
true`) **before** `WindowManagerGlobal.addView`, so a throw in the window path leaves a
genuinely resumed `Activity` behind. In the first runs of this task the watcher thread
simply lost that race (the callback throws ~1 ms after `onResume`) and the run was classed
`NO_ACTIVITY_RESUMED` even though the activity had resumed. Two changes keep the evidence
honest instead of timing-dependent:

* `LaunchMain`: `Looper.loop()` is wrapped; on an uncaught throw the guest reads
  `resumedActivity(mActivityThread)` and, if a record is `mResumed`, writes the same
  `activity-resumed.marker` the watcher would (plus `post_resume_failure=`), prints the
  stack, and exits 1 with `{"outcome":"crash","code":"LOOPER_THREW","activity_resumed":true,…}`.
  Both callers share one `writeResumeMarker()` — one source of truth.
* `acos-art launch`: a run with the marker present and a non-zero guest exit is
  `Crash`/`GUEST_EXIT_AFTER_RESUME` (was `NO_ACTIVITY_RESUMED`), i.e. "the milestone was
  reached, the *next* wall stopped the process". No fake `ok`.

## 2. The blocker, exactly: a reply the peer could not send (`EBADF`, not a hang)

P4b recorded "the holder's binder is a node nobody serves, so `ContentProviderProxy.call`
blocks". The transaction *was* served — the reply never left the host process. Evidence,
from a peer-side frame trace (`ACOS_PEER_DEBUG=1`, added in this task):

```text
peer-trace: in  ty=1 seq=55 code=30 target=0x200000005 data=192 offs=8   # getContentProvider
peer-trace: out ty=2 seq=55 code=0 data=1024                              # holder (2 binders)
peer-trace: in  ty=1 seq=56 code=21 target=0x200000024 data=364 offs=8   # IContentProvider.call
peer-trace: handle -> 1 frames
peer-trace: write_frame FAILED: Bad file descriptor (os error 9) (ty=2 seq=56 data=272 offs=8)
```

`IContentProvider.call` for a `Settings` `GET_*` returns
`Bundle{value, generation=MemoryIntArray}` (Q2 §2). The `MemoryIntArray` is a
`BINDER_TYPE_FD` flat object whose id is the **parcel's** duplicate of
`SettingsState`'s memfd (`MemfdGeneration`, a 12-byte `memfd_create` holding the three
namespace counters). `Peer::service_transaction` builds the reply `Parcel`, then
`frame_from_parcel` copied the raw fd number into `Frame::fds` — and the reply `Parcel` is
dropped on return from the transaction, closing that duplicate. By the time `serve` called
`write_frame`, the number was stale: `sendmsg` returned `EBADF`, `serve` returned `Err`, the
peer thread exited, and the guest's `transactNative` waited forever for the `BR_REPLY`.

That is the whole P4b "hang": a main thread blocked on a reply the transport could not
carry. Two smaller consequences, both fixed:

* `Frame` now owns its descriptors: `frame_from_parcel` dups each `BINDER_TYPE_FD` with
  `F_DUPFD_CLOEXEC`, and `close_frame_fds` releases them once `write_frame` has sent the
  frame (both serve loops: `acos-binder-peer/src/main.rs` and `acos-art/src/launch.rs`).
  A failure to dup is a structured `ART_FD_UNAVAILABLE` line, never a silent drop.
* `read_frame`'s received descriptors were leaked; they are the same `Frame::fds`, so they
  are closed by the same call. (One fd per fd-carrying transaction, either direction.)

A failed reply is now **always** visible (`write_frame FAILED: …` is not gated by the debug
env var): the whole class of "structured error vs. blocked main thread" turned on exactly
this line.

## 3. The provider path, end to end (what is whose)

| step | who answers | real or stand-in |
|---|---|---|
| `IActivityManager.getContentProvider("settings")` (code 30) | `acos-services::atms` → `ProviderRegistry` (Q2) | **real holder layout** (`ContentProviderHolder` 5 fields, `noReleaseNeeded=0`, `mLocal=1`, real `ContentProviderConnection` token) |
| holder's `provider` binder | `ServiceHost`'s `android.content.IContentProvider` node (`registry.rs` registers the descriptor) | **real node** — the same id `acos-runtime`'s `ams_java` (Q2b) routes to |
| `IContentProvider.call` (code 21), `GET_*`/`PUT_*`/`GET_config`/`LIST_config` | `acos-services::provider::ProviderService` → `SettingsState` | **real** — stock API-34 defaults; the reply bundle is written by the ACOS codec in the framework's `ContentProviderNative` shape |
| the `generation` `MemoryIntArray` | `SettingsState`'s `MemfdGeneration` | **real fd, ACOS counter**: a 12-byte memfd the guest `mmap`s; the *value* is ACOS's per-namespace counter (a device's is the SettingsProvider DB generation). Only observable if a `PUT` + a second read race, which this launch does not do |
| `Certificate`/`ProviderInfo` fields | registry entry | stand-in metadata (`com.android.providers.settings` / `SettingsProvider`), never read by `Settings$NameValueCache` |

The `Settings` read itself is the framework's: `Settings$NameValueCache.getStringForUser`
(`Settings.java:3331`) → `Settings.Global.getInt` → `PhoneWindow.<init>`.

## 4. What shipped (files, all uncommitted)

| path | change |
|---|---|
| `crates/acos-binder-peer/src/peer.rs` | `frame_from_parcel` duplicates every `BINDER_TYPE_FD` into `Frame::fds` (the fix, §2); new `record_error_at(descriptor, code, err)` used by the service-call error path, so a failing transaction's unsupported record names the descriptor instead of `<unknown>` (P4a §5, closed for this path) |
| `crates/acos-binder-peer/src/protocol.rs` | `close_frame_fds(&Frame)` |
| `crates/acos-binder-peer/src/lib.rs`, `src/main.rs` | export + call it after `write_frame` |
| `crates/acos-binder-peer/tests/peer.rs` | **new** `reply_file_descriptors_stay_open_until_the_frame_is_written` — a memfd written through `Parcel::write_file_descriptor`, `frame_from_parcel`, parcel dropped, `pread` on the frame's fd returns the provider's bytes; `close_frame_fds` releases it |
| `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` | `Looper.loop()` wrapped: on an uncaught throw, record the resume the framework reached (`recordResumeAfterLooperFailure`), print the stack + one-line JSON, exit 1; shared `writeResumeMarker` (§1.1) |
| `crates/acos-art/src/launch.rs` | `GUEST_EXIT_AFTER_RESUME` when the marker exists but the guest failed; `ACOS_PEER_DEBUG=1` frame trace (in/out sizes, fds) and an always-on `write_frame FAILED` line |
| `results/art-runtime/p4c/` | `activity/` (markers, `unsupported.jsonl`), `activity.log`, `counter/{frame.png,tree.json}`, `compare.json` |

`crates/acos-art/src/props.rs`, `crates/acos-services/**`, `crates/acos-runtime/**`,
`crates/acos-wm/**`: untouched.

## 5. The next blocker (exact): `IWindowManager.openSession` → no `IWindowSession` node

```text
java.lang.UnsupportedOperationException: unsupported Android API:
    android.view.IWindowSession code 4 (openSession)
  at android.os.Parcel.createExceptionOrNull(Parcel.java:3069)
  at android.os.Parcel.createException(Parcel.java:3041)
  at android.os.Parcel.readException(Parcel.java:3024)
  at android.os.Parcel.readException(Parcel.java:2966)
  at android.view.IWindowManager$Stub$Proxy.openSession(IWindowManager.java:3193)
  at android.view.WindowManagerGlobal.getWindowSession(WindowManagerGlobal.java:197)
  at android.view.ViewRootImpl.<init>(ViewRootImpl.java:997)
  at android.view.WindowManagerGlobal.addView(WindowManagerGlobal.java:397)
  at android.view.WindowManagerImpl.addView(WindowManagerImpl.java:150)
  at android.app.ActivityThread.handleResumeActivity(ActivityThread.java:5048)
  at android.app.servertransaction.ResumeActivityItem.execute(ResumeActivityItem.java:57)
  at android.app.servertransaction.ActivityTransactionItem.execute(ActivityTransactionItem.java:45)
  at android.app.servertransaction.TransactionExecutor.executeLifecycleState(TransactionExecutor.java:180)
  at android.app.servertransaction.TransactionExecutor.execute(TransactionExecutor.java:98)
  at android.app.ActivityThread$H.handleMessage(ActivityThread.java:2443)
  at android.os.Handler.dispatchMessage(Handler.java:106)
  at android.os.Looper.loopOnce(Looper.java:205)
  at android.os.Looper.loop(Looper.java:294)
```

The transaction is `android.view.IWindowManager` code **4** `openSession` (in
`service_calls`, the last entry of the run) and the peer answers it with
`UNSUPPORTED_ANDROID_API` (`results/art-runtime/p4c/activity/unsupported.jsonl`, third
record, now with the descriptor named). Cause, verified in the tree (not guessed):

* `acos-services/src/wms.rs:365` — `WmsService::call` matches only `useBLAST`,
  `getCurrentAnimatorScale`, `hasNavigationBar`, `isInTouchMode`, `setInTouchMode`;
  everything else, `openSession` included, goes to `Self::unsupported(method)`
  (`wms.rs:333`, which is why the message names `IWindowSession`).
* `WmsState::open_session()` **exists** (`wms.rs:143`) and `acos-wm` implements
  `addToDisplayAsUser`/`relayout`/`finishDrawing` — but the session binder it returns is an
  `acos-wm` id that the `ServiceHost` does not know: `registry.rs` lists
  `android.view.IWindowSession` in `NOT_OWNED` (`acos-wm`) and installs **no** node for it.
  So `openSession` cannot answer with a routable binder today, and if it did the following
  `IWindowSession.addToDisplayAsUser` would be the next structured error on the same wall.

That is one `acos-services`/`acos-wm` change (not P4c's crates): return the session binder
as a `ServiceHost` node (`register_dynamic("android.view.IWindowSession", …)` over the same
`WmsState`), add the `openSession` reply to `WmsService::call`, and give the peer's
`direct_dispatch` path an `IWindowSession` object. Then the P4b §5 order still holds:

1. **`addToDisplayAsUser`** (`ViewRootImpl.java:1476`) with the full arg/reply codec
   (`Insets`, `InsetsState`, `InsetsSourceControl`, `InputChannel`) — `acos-wm` has the state
   machine, the API-34 wire codec is what is missing.
2. **`Choreographer`/vsync**: `ViewRootImpl`'s constructor calls `Choreographer.getInstance()`
   *before* any traversal, which needs `DisplayEventReceiver` →
   `ComposerServiceAIDL::getComposerService()` → `ISurfaceComposer.createDisplayEventConnection`
   → `BitTube` (`native/libs/gui/DisplayEventReceiver.cpp:36-46`). `servicemanager.ready`
   is already served (P4b §6), so the Q3 *hang* cannot recur, but a missing `SurfaceFlinger`
   now takes the Q3 **SIGSEGV** path (`~NativeDisplayEventReceiver` → `getJNIEnv` on a null
   `JavaVM`, `si_addr=0x0`). This needs a peer node for the native name `SurfaceFlinger`
   returning a `BitTube` socketpair fd + a vsync writer, or a guest-side guard. It is the
   next hard wall after `openSession`, and it is *not* small.
3. `relayout` with a null `SurfaceControl` (rung 0 of P4b §5.2) is enough to let
   `performTraversals` finish without drawing; pixels from the app's own `Canvas` need the
   shmem `ANativeWindow` (rung 1).

## 6. Real vs stand-in (this task's slice)

| piece | verdict |
|---|---|
| `IContentProvider` binder in the holder | **real `ServiceHost` node**; transactions reach it through the driver → peer → `ProviderRegistry` |
| `IContentProvider.call` reply bytes | **real** ACOS codec in `ContentProviderNative`'s shape; values from `SettingsDefaults::api34()` |
| the reply's `MemoryIntArray` fd | **real memfd** created by the provider state and carried through `SCM_RIGHTS` (this task's fix); its counter values are ACOS's, not a device DB's |
| `Activity.onCreate`/`onStart`/`onResume` | **real framework**, real `Instrumentation`, real `Activity` from `counter.apk`; proved by the fixture's own log lines + `mResumed` |
| the `ClientTransaction` bytes | **real framework objects** (`ClientTransaction.writeToParcel`), as in P4b; activity token still `null` (`Parcel.marshall()` cannot carry binders — P4b §3) |
| `IWindowManager.openSession` (code 4) | **missing** → structured `UNSUPPORTED_ANDROID_API`; the activity resume is *before* it, the window is not |
| `IWindowSession`, `Surface(Control)`, `Choreographer`/vsync | **missing** (§5) |
| frame | from `render-app` (P3c chrome path), md5 unchanged |

## 7. Limits / not done

* `--until activity-resumed` now ends in `GUEST_EXIT_AFTER_RESUME` (exit 1), not `ok`:
  the milestone is reached, the process then dies in the window path. A `--until
  window-added` (or `--until traversed`) rung is the honest next target.
* No `query`/bulk-cursor transaction occurs in this launch (nothing to observe); Q2's
  cursor tests remain the evidence for that half.
* `getType`/`insert`/`update`/`delete`/`openFile` still structured-unsupported (Q2 §5).
* The `<unknown>` descriptor in unsupported records is fixed for the *service-call* error
  path only; `service_manager`/`add_service`/`launch_push` still re-peek their parcels (they
  are one-shot handshakes, not the diagnosis path).
* `Choreographer`/vsync was **not** implemented: the exact source line, the Q3 crash
  mechanism and the required peer node are recorded in §5 instead of a half-built
  `SurfaceFlinger` that would hide the next wall.

## 8. Reproduction

```bash
# 1. acceptance run (activity resumed; next wall named)
tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art launch \
    --apk fixtures/build/apps/counter.apk --until activity-resumed \
    --out results/art-runtime/p4c/activity
cat results/art-runtime/p4c/activity/activity-resumed.marker
cat results/art-runtime/p4c/activity/unsupported.jsonl         # 3 records, the third is openSession

# 2. peer frames (the evidence for §2)
ACOS_PEER_DEBUG=1 tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art launch \
    --apk fixtures/build/apps/counter.apk --until activity-resumed --out .cache/p4c/trace 2>&1 \
    | grep peer-trace | tail -20

# 3. frame (unchanged)
tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art render-app \
    --apk fixtures/build/apps/counter.apk --out results/art-runtime/p4c/counter
md5sum results/art-runtime/p4c/counter/frame.png results/art-runtime/p3c/counter/frame.png
python3 tools/art-render-app-compare.py --png results/art-runtime/p4c/counter/frame.png \
    --tree results/art-runtime/p4c/counter/tree.json --out results/art-runtime/p4c/compare.json

# 4. tests
tools/guarded.sh --mem 4G --time 900 -- cargo test -p acos-art -p acos-binder-peer
#   acos-art        39 + 6    acos-binder-peer 3 + 13    (0 failed)
```
