# MERGEMPROC — mproc7's process model on modules' module framework (v0.4.0)

Branch `mergemproc`, cut from `modules` `a97d71b1`, merged with `mproc7`
`d4c282ed`, then with the orchestrator's `modules` `f00396b2` (WV8).

```
mergemproc HEAD = modules(a97d71b1) + mproc7(d4c282ed) + modules(f00396b2) + MPROC8 + driver fixes
```

Everything below was verified on real ART in this worktree
(`ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` private, every run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`).

## 1. The merge, and the one implementation chosen per seam

`mproc7` is a process model: per-process driver connections with a process tag,
host-built `ClientTransaction`s, lazy spawn, `startActivityForResult`,
`singleTop`/`NewIntentItem`, idle kill. `modules` is a **module framework**: one
control path (`ModuleRegistry`/`Module`), the BINDERREF2 reference ledger on the
host, and a deadline-driven idle turn. Where the two overlapped, one
implementation was kept:

| seam | kept | why |
|---|---|---|
| `F_HELLO` handshake | **both facts in one frame**: `seq` = wire version, `code` = process tag, `data` = name | the two branches used the same frame for different things; one handshake carries both |
| wire constants | `F_HELLO` 5, **`F_REF` 6** (modules), **`F_DEATH` 7** (mproc7, renumbered), `PROTOCOL_VERSION` 3 | one meaning per frame number |
| serve loop | **`acos_binder_peer::serve`** (mproc7's N-connection loop) + a control pipe and the module deadline in the same `poll` | the launcher's old per-session loop is gone; `serve_with_control` is the only loop |
| launch API | **`ArtSession::start_with_apks_and_session`** (one funnel: `start_inner`) with module specs, session id, extra APKs, idle kill | every constructor delegates |
| reference ledger | **the host's `retain_binder`/`release_binder`** (BINDERREF2), routed per connection: a `JAVA_TAG` id names its process, so an `F_REF` release goes to the connection whose driver owns the node | a delivery and its release can no longer cross processes |
| process teardown | peer `disconnect_process` + `release_process_refs(tag)` | a dead process's nodes are released, and its bound-service callers get `onServiceDisconnected` |
| PMS | **one factory over `pms_shared`**, one `PermissionStore`; `check_permission_for(package, …)` is the single policy (runtime table for the package it describes, else that package's manifest grants) | `IPackageManager.checkPermission`, the ATMS path and the `permissions` module read one table |
| `IntentFacts` | `mime_type` (mproc7/fill_in naming), `identifier`, `flags` | one field name across the codec, fill-in and the capture target |
| IWindowSession | mproc7's **per-`openSession` factory** + modules' accessibility window-stack sink attached to every session | stock `WindowManagerService` creates one `Session` per call |
| services | one `ServiceHost`; the module registry owns its table | live attach/detach is a request to the thread that owns the table |

Files whose two versions were resolved by keeping both sides' additions: the
protocol constants, `acos-parcel`'s `unmarshall_with_objects` (a thin wrapper
over modules' `register_object`), the launcher's option struct/argv (module
specs + extra APKs + idle kill), `annotate`-style test argument lists, and the
driver test (see §4).

## 2. MPROC8 — a cross-process `bindService`, host-side (AOSP `ActiveServices`)

New module `crates/acos-services/src/service_bind.rs` (host-side `ActiveServices`
+ tests) and the hooks in `host.rs`:

1. `IActivityManager.bindService` resolves the component through the manifest
   table (the same resolver a launch uses). The host mints the service **token**
   (a real host node, `ServiceToken`) and records the caller's
   `IServiceConnection`.
2. The target package's process is brought up **lazily**: if it is attached, the
   host schedules `IApplicationThread.scheduleCreateService` (code 3) then
   `scheduleBindService` (code 11) to that process's own app thread; if not, it
   records a spawn request exactly like an activity launch, and the calls are
   delivered when the process reports `attachApplication`.
   Payloads are the framework's own shapes (`ServiceInfo.writeToParcel`,
   `Intent.writeToParcel`, `CompatibilityInfo`).
3. The app's `ActivityThread` runs the real `Service.onCreate`/`onBind` and
   reports the binder with `IActivityManager.publishService(token, intent,
   binder)`.
4. The host answers every waiting caller with
   `IServiceConnection.connected(ComponentName, IBinder, false)`. The service
   binder lives in B's process, so the peer exports it through the **MPROC3
   foreign-node path**: A receives a real `IBinder` whose transactions the peer
   forwards to B and relays back.
5. `unbindService`: the last connection schedules `scheduleUnbindService` +
   `scheduleStopService`; a dead service process reaches its callers as
   `connected(name, null, true)` (`LoadedApk` → `onServiceDisconnected`).

The caller's `IServiceConnection` is stored on the host's reference table (the
BINDERREF2 ledger) and released on unbind / death / teardown, so the `F_REF`
model covers service bindings too.

The host performs this only for a session with **real app processes**
(`ServiceHost::set_host_side_services(true)`, set by the ART launcher). The
in-process Rust VM keeps its own bind path (`AmsState::pending_binds`), so there
is exactly one implementation per engine.

Driver rows that were `OPEN_3C` since MPROC5c now pass:
`aidl-round-trip-through-foreign-node` and `transaction-ran-in-proc-b`
(`drive-mproc8f/`: `FAILURES_3B []`, `OPEN_3C []`).

## 3. Three ART regressions the merged tree exposed (each fixed at its cause)

1. **`scheduleBindService` before `scheduleCreateService`** (MPROC8): two reader
   threads popped one delivery each and posted the two handler messages in the
   opposite order, so `Service.onBind` ran for a service that did not exist.
   Fix: the incoming queue has **one owning thread** (the kernel's per-node
   single async transaction); it keeps ownership until the queue is empty.
2. **Two broadcasts in one turn, only the second delivered** (tail-bluetooth):
   `IPCThreadState` reads commands into a small fixed parcel, so two callback
   deliveries in one write leave the first one's tail unread. Fix: the owner
   hands libbinder **one transaction per read**.
3. **`F_REF` release killed the callback it was about to deliver**
   (location-geocoder): the module releases a one-shot callback in the same turn
   as its delivery, and the driver dropped the node at read time. Fix: a release
   for a node with a queued delivery is **deferred** and retried when the queue
   no longer names it — the kernel's `binder_dec_node` runs on the node's own
   thread after its queued work.

Plus one wire regression: the guest's `DisplayInfo` Mode carried a rounded
`60.0` instead of the framework's `REFRESH_RATE_HZ` (`60.000004`), which
`a11y-probe` asserts and `dispatchGesture` samples against.

## 4. Driver test split

`native/driver_test.c` (modules) and `native/driver_test.c` (mproc7) were two
different programs. They are now two programs: `driver_test.c` (nodes, `F_REF`,
the shared transaction heap — 18 checks) and `driver_death_test.c`
(`F_DEATH`/`BR_DEAD_BINDER`, dead `F_RSP` → `BR_DEAD_REPLY`). Both compile the
same `binder_driver.c` and both run under `cargo test -p acos-binder-peer`
(`tests/driver.rs`, two tests).

## 5. Acceptance evidence (all in this directory)

| criterion | result | evidence |
|---|---|---|
| `cargo check --workspace --all-targets` | **PASS** | `check-workspace.log` |
| `cargo test -p acos-services -p acos-binder-peer -p acos-wm -p acos-protocol -p acos-mcp` | **PASS** (113 `test result: ok`, 0 failures) | `tests-nonart.log` |
| `cargo test -p acos-art -p acos-cli` (ART lock) | **PASS** (34 `test result: ok`, 0 failures; incl. `launch_hang` 5/5) | `tests-art-cli.log` |
| `cargo test -p acos-mcp --test e2e` | **PASS** (uses `.cache/mergemproc-target/debug/acos`) | this file §5 |
| driver tests | **PASS**: `RESULT failures=0`, `driver_test: PASS` | `driver/driver_test.log`, `driver/driver_death_test.log` |
| `scenarios/mproc3-two-process.yaml` on ART (incl. `idle_kill_seconds: 2`) | **PASS 16/16**, verdict pass | `scenario-mproc3/verify.json` |
| `tools/mproc4-two-process-drive.py` on ART | **PASS**: `FAILURES_3B []`, `OPEN_3C []` | `drive-mproc8d/`, `drive-mproc8e/`, `drive-mproc8f/` (`launcher.stderr.log`, `session-exit.json`) |
| module scenarios: sensors (scripted/off), a11y-probe, notif (granted/denied), location x3, tail-* x5, net-check-offline, net-network-offline | **PASS**, each `verdict=pass` | `scenarios/<name>/verify.json` |
| `scenarios/webview-probe.yaml` on the merged tree | **PASS 19/19** | `scenario-webview2/verify.json` |
| `verify::annotate` stale test call | fixed (14 args in both `engine/mod.rs` sites) | `crates/acos-cli/src/engine/mod.rs` |
| `wait` documented as time-only | `docs/AGENT_GUIDE.md` §steps, `WaitStep` doc | — |

The three regressions above are each shown red-then-green by re-running the
scenario that exposed them: `scenarios/` (red, before the driver fix),
`scenarios-fix2/` (green), and `scenario-webview/` vs `scenario-webview2/`.

## 6. Environment-only facts (not passes, not regressions)

* **Real-network arms need `pasta`.** `scenarios/net-check.yaml` with
  `ACOS_SPIKE_NET=1`, `net-network-real.yaml` and `net-network-off.yaml` require
  the pinned `pasta` binary (`ACOS_SPIKE_NET_PASTA`, NET6 §6) which is **not
  present on this host** (`find / -name pasta -type f` → none). Without the flag
  `net-check` shows the honest `SecurityException (missing INTERNET
  permission?)` — libcore's mapping of the sandbox's `EACCES` — which is the
  documented flag-off behaviour, not a fake success. `net-check-offline` and
  `net-network-offline` (which exercise the offline path) pass.
* **`webview-probe` step 10 (`text=="tapped"`)** failed once and passed on the
  re-run of the same binary and scenario (`scenario-webview/` →
  `ELEMENT_NOT_FOUND` after 12 s; `scenario-webview2/` → 19/19 pass). The tap →
  page-JS round trip is timing sensitive under a loaded host; the flake is
  recorded rather than hidden.
* The long worktree path shortened the unix-socket budget of `live_view`'s
  tests; they pass with the documented `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`.
* `out/` is tracked in this repository and `acos verify` rewrites it; it was
  restored with `git checkout -- out` before the final commit.

## 7. Reproduce

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv
export CARGO_TARGET_DIR=$PWD/.cache/mergemproc-target
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
tools/guarded.sh --mem 12G --time 900 -- cargo build -p acos-cli -p acos-art

tools/art-runtime/run-exclusive.sh --task MERGEMPROC -- \
  tools/guarded.sh --mem 12G --time 1100 -- \
  env -u ACOS_HOME .cache/mergemproc-target/debug/acos verify \
  scenarios/mproc3-two-process.yaml --engine art --json

tools/art-runtime/run-exclusive.sh --task MERGEMPROC -- \
  tools/guarded.sh --mem 12G --time 780 -- \
  python3 tools/mproc4-two-process-drive.py \
  .cache/mergemproc-target/debug/acos-art \
  fixtures/build/apps/proc-a.apk fixtures/build/apps/proc-b.apk <out-dir>

# the module corpus (15 runnable scenarios; the 3 real-network arms need pasta)
tools/art-runtime/run-exclusive.sh --task MERGEMPROC -- \
  tools/guarded.sh --mem 14G --time 2900 -- \
  results/handoff/MERGEMPROC/run-module-scenarios.sh "$PWD" <out-dir>

# webview (shared engine cache; the broker binary must be on PATH)
PATH=$PWD/.cache/mergemproc-target/debug:$PATH \
ACOS_WEBVIEW_CACHE=/home/Qui/Projects/acos/.cache/webview \
  tools/art-runtime/run-exclusive.sh --task MERGEMPROC -- \
  tools/guarded.sh --mem 16G --time 1400 -- \
  env -u ACOS_HOME .cache/mergemproc-target/debug/acos verify \
  scenarios/webview-probe.yaml --engine art --json
```
