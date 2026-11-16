# MODLOC2 — `modloc` into `modules`: one module-control path, a deadline-driven peer

Branch `modloc2`, created from `modules` @ `cfed3eea` (MODTAIL2's single module
control path), `modloc` @ `b29fe364` merged in, then `modules` merged back twice
(`575c19d7` = MODTAIL3 + MODA11Y5, and `9eb1b8ee` = MODNOTIF5 notifications/
permissions/PendingIntent senders) so the branch fast-forwards. Implementation
only; the orchestrator reviews and merges.

Worktree `t3-9e894753`, private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`, every
ART-lock run through `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`,
builds with `CARGO_PROFILE_DEV_DEBUG=0 … CARGO_INCREMENTAL=0` into
`.cache/modloc2-target`.

## 1. The merge: MODTAIL2's control path wins, location is ported onto it

15 conflicted files. The rule for every one: **MODTAIL2's single module-control
path stays, and modloc's duplicate seam is deleted**; modloc's *content* (the
`location` module, the off node, the stale-handle answer, the kit row, the
fixture, the how-to) is kept and ported.

| # | file | both sides wanted | resolution (intent kept) |
|---|---|---|---|
| 1 | `crates/acos-cli/src/engine/mod.rs` | MODTAIL2: `SessionBackend::service_op(request)`, `Capability`/field `services`, `Backend::service_ctl(ServiceCtlRequest)`. modloc: trait `service_ctl(module, op, args)`, `service_module` capability, `EnvironmentChange::ServiceCtl` arm | MODTAIL2's `service_op` only; the trait `service_ctl` is deleted; `services` is the one capability (`service_module` field/key/`to_protocol` mapping deleted); the `configure()` `EnvironmentChange::ServiceCtl` arm is deleted (the step reaches `Backend::service_ctl`, which forwards `service_op`). **Kept from modloc** (independent of the control path): `module_rows_handle`, `EngineBackend::module_rows`, so a verdict cites the session's *final* module rows after teardown. |
| 2 | `engine/art.rs` | `fn service_op` (MODTAIL2) vs `fn service_ctl` + `module_rows_handle` (modloc) | `service_op` (forwards to `ArtSession::service_op`), `module_rows_handle` kept; `capabilities()` loses modloc's `service_module: true` (the `services: true` row already says it). |
| 3 | `engine/serve.rs` | error text for an unknown `acos_services` op | MODTAIL2's text: “use list, status, attach or detach (a module control op is `acos_service_ctl {module, op, args}`)”. The route already accepts `op: ctl` internally for the `acos_service_ctl` path. |
| 4 | `engine/fake.rs`, `engine/rust_vm.rs`, `verify/native.rs`, `verify/reference.rs` | `services: false` (MODTAIL2) vs `service_module: false` (modloc) | `services: false` in all four. |
| 5 | `crates/acos-verify/src/executor.rs` (auto-merged into a duplicate match arm) | modloc's `EnvironmentChange::ServiceCtl` capability-gated arm; MODTAIL2's `service_ctl_step(backend, ctl)` | only `service_ctl_step`; `Capability::ServiceModule`, `Capabilities::service_modules`, `EnvironmentChange::ServiceCtl` and its `kind()` arm are gone. `Capability::Services` (already on `modules`) is the gate, and `ServiceCtlRequest` keeps `attach`/`detach`/ctl. |
| 6 | `crates/acos-protocol/src/scenario.rs` | `ServiceCtlStep {module, op, args}` (modloc) vs `{module, op, profile, config, args}` (MODTAIL2) | MODTAIL2's superset; modloc's early short validation arm (duplicate `StepRef::ServiceCtl` → unreachable pattern) deleted in favour of MODTAIL2's full validator (attach/detach vs control op, profile/config only on `attach`, the “module is not attached by this run” refusal — which is what makes the live attach/detach scenario legal). |
| 7 | `crates/acos-protocol/schema/scenario.json` | both | MODTAIL2's (regenerated; `UPDATE_SCHEMA=1 cargo test -p acos-protocol` leaves it byte-identical). |
| 8 | `crates/acos-services/src/module.rs` (registry list) | `telephony/bluetooth/biometric` + `tail_stubs` (MODTAIL2) vs `location` (modloc) | both: `core, sensors, vibrator, location, telephony, bluetooth, biometric` + `tail_stubs::modules()`. |
| 9 | `crates/acos-services/src/module.rs` (conformance kit, honest-report check) | MODTAIL2: the name must answer exactly what it answered *before* the attach; modloc: accept a tombstone naming the owner or a structured `UNSUPPORTED_ANDROID_API` | MODTAIL2's before/after comparison. It is the stronger check and it holds with `off_node` because `install_off_nodes` runs from `install_tombstones`, i.e. it is part of the kit's **baseline**: the off node is what `location` answered before the attach too. |
| 10 | `crates/acos-services/src/module_kit.rs` | kit rows | both: `net.telephony`, `hw.bluetooth`+`hw.biometric`, **and** `location`. `detaching_core_removes_its_registrations` keeps modloc's accepted shape: only the off module's documented disabled node (`location`) may outlive `core`, and it must answer `UNSUPPORTED_ANDROID_API`. |
| 11 | `crates/acos-binder-peer/src/peer.rs` | MODTAIL2: `pub fn take_delivery_frames()` (`take_deliveries`, flushed after a live control op). modloc: `pub fn tick()` (`take_timed_deliveries`, drained on the idle turn) | both, with distinct jobs: `take_delivery_frames` for the post-control flush, `tick` for the idle turn. Neither is folded into the other (`take_deliveries` must not take a dynamic object's lock). `tick` is now deadline-driven (section 2). |
| 12 | `crates/acos-art/src/launch.rs` | `"ctl"` branch: MODTAIL2 `{id, ctl_op, args}` → `modules.control(host, id, ctl_op, args)` replying `{id, op, value, attached, modules}`; modloc `modules.control(id, ctl_op, args)` replying `{module, ctl_op, result}` | MODTAIL2's, because that is the shape `Module::control(&mut ServiceHost, …)` needs (the module gets the session host so a control can deliver) and the shape `summarize_reply`/`acos_service_ctl` read. |
| 13 | `docs/AGENT_GUIDE.md` | MODTAIL2's “Driving a scripted module mid-run (`service_ctl`)” + sensors how-to; modloc's location how-to | both: MODTAIL2's section stays verbatim, followed by a new `### Location (\`location\`)` section adapted from modloc's (the live spelling is now `acos_service_ctl {module, op, args}` for a control op and `acos_services {op: attach\|detach}` for the lifecycle, not `acos_services {op:"ctl"}`). |
| 14 | `tools/build-apps.sh` | `tail-probe` (MODTAIL2) vs `location-probe` (modloc) | both fixtures are built. |
| 15 | `crates/acos-services/src/modules/mod.rs` | module list | `accessibility, biometric, bluetooth, location, notifications, permissions, sensors, tail_stubs, telephony, vibrator`. |

**Second merge (`modules` @ `9eb1b8ee`, MODNOTIF5):** two conflicts, both from
MODNOTIF5's `ModuleManifest.replaces` (a module may only take a `core` name over
when its manifest declares it):

| file | resolution |
|---|---|
| `crates/acos-services/src/module.rs` (`ModuleRegistry::add`) | MODNOTIF5's rule (an explicit `replaces` declaration, with the “declare it in `replaces` to take it over” message). modloc's blanket “core may always be taken over” is deleted. |
| `crates/acos-services/src/modules/mod.rs` | both lists (accessibility/notifications/permissions + location). |

`location`'s manifest now declares `replaces: &[SERVICE_NAME]`, which is the same
intent modloc had (the name is always published on a device, the module takes it
over while attached) expressed in the newer, stricter mechanism; `location`
still installs `off_node` so the name keeps answering the disabled-device node
while the module is off. `cargo check --workspace --all-targets` and the full
`acos-services` suite are green after both merges.

**Porting location onto the one path** (no conflict, but required): `location.rs`
now implements `Module::control(&mut self, host: &mut ServiceHost, op, args)` and
its manifest declares `process_initializers: &[]` / `features: &[]`;
`tests/location_module.rs` passes the session host to every `control` call (and
still asserts the identical wire behaviour: 8/8 pass).

Orchestrator-accepted decisions from MODLOC §5, all kept:

* `Module::off_node` + `ModuleRegistry::install_off_nodes`: `location` answers a
  phone's honest disabled-device node while the module is off (the name is always
  published on a real device).
* the stale-handle structured answer: a guest holding the detached module's
  binder gets `SERVICE_MODULE_UNAVAILABLE` naming the module and the attach call.
* both sides' tests and fixtures.

## 2. The idle turn is deadline-driven (the 20 ms poll is gone)

MODLOC's peer API is kept (`Peer::tick` / `ServiceHost::take_timed_deliveries`),
but the **fixed 20 ms socket poll is replaced by a deadline wakeup**:

* `ServiceObject::next_delivery_deadline()` — the next instant this object's own
  schedule has a delivery ready (`None` = no timed feed) — and
  `ServiceObject::advance_timed(now)`, which queues what is due.
  `ServiceHost::take_timed_deliveries()` calls `advance_timed` first, then drains,
  so the API MODLOC introduced keeps its meaning.
* `ServiceHost::next_timed_deadline()` aggregates; `Peer::next_timed_deadline()`
  also reports *now* when a delivery is already queued.
* `acos_binder_peer::wait_for_idle(fd, stop_fd, deadline)` is the shared wait:
  `poll(2)` on the guest socket + the launcher's stop pipe for exactly the
  remaining time (rounded **up**, so a wakeup never lands a millisecond early and
  re-sleeps), or unbounded when no module has a deadline.
* The launcher's `serve` loop uses it; the stop pipe (`PeerWakeup`) makes
  `stop` immediate instead of “within one poll interval”, and the socket receive
  timeout is only a mid-frame stall bound (1 s).
* **`location`'s `play_track` is a schedule, not a thread.** `Track.next_due`
  is published through `next_delivery_deadline` and advanced point by point by
  `advance_timed` (one interval per point, missed points delivered in order, the
  track ends itself when a non-looping track is done). The module keeps no
  thread, so a detach has nothing to join, and the delivery instant is the
  deadline the peer slept until rather than the next poll.

Net effect: an idle session with no timed module has **zero** wakeups (the old
loop had 50/s), and a scheduled fix lands within 5 ms of its deadline (the old
loop bounded it at 20 ms).

**Two things the first ART runs caught** (both fixed, both in this branch):

1. **A live module request had no wake.** With the idle turn asleep until a
   deadline, `acos_services {op: attach|detach}` / `acos_service_ctl` waited for a
   guest frame that never came: the first run answered
   `SERVICE_CONTROL_TIMEOUT … did not answer the module request within 10 s`.
   `ServiceControl::submit` now writes to the same wake pipe `stop` uses, and
   `IdleWake::Stop` is `IdleWake::Wake` (the `stop` flag is the decision; a queued
   request is served at the top of the next turn).
2. **A control op's own deliveries were left in the module's queue.** The
   post-control flush used the plain transaction drain (`take_delivery_frames` →
   `host.take_deliveries`), which cannot see a delivery a module parked in its own
   object (`ServiceObject::drain_raw_deliveries`). `location-probe` regressed to
   `wait_for last=37.4220 … timed out`; MODLOC's 20 ms tick used to hide it. The
   launcher now flushes a control op with `Peer::tick()` (the timed drain) — the
   control op has returned, so no module lock is held.

Tests (`crates/acos-binder-peer/tests/peer.rs`, 18 pass):

```
an_idle_peer_with_no_timed_module_has_no_tick_wakeups
  -> next_timed_deadline() = None; 2 s of the peer loop = ONE unbounded poll and 0 tick calls
a_scheduled_fix_fires_within_five_ms_of_its_deadline
  -> play_track point arrives as an F_TXN frame targeted at the registered listener,
     lateness <= 5 ms, and next_due moves by exactly one interval (40 ms)
```

Fail-before: both need `next_timed_deadline` / `wait_for_idle` / `advance_timed`,
which do not exist on the tree they replace (MODLOC's loop ticked every 20 ms via
`SO_RCVTIMEO`), so they do not compile there — the behaviour they pin (0 wakeups,
≤5 ms) was impossible on that loop.

## 3. Live attach/detach as a scenario step

MODTAIL2's `ServiceCtlStep` is already the superset (`attach` with
`profile`/`config`, `detach`, every control op with `args`), so **no new step was
needed**; the detach proof moves from MODLOC's MCP driver (`live_location.py`)
into a scenario: `results/handoff/MODLOC2/location-live-detach.yaml` (12/12 on
ART).

```
services: [{id: location, profile: scripted, config: {permissions: fine}}]
launch -> state=ready providers=fused,gps,network,passive
service_ctl {module: location, op: set_location, args: {...37.4220/-122.0841...}}
tap check -> last=37.4220,-122.0841
service_ctl {module: location, op: detach}         # the lifecycle word, in-band
tap check -> state=unavailable, last=none, fix=none
```

The fixture's “Check again” button re-runs the whole probe through the framework
client it already holds, which is what makes the detach observable on screen.

**A live *attach* is not observable by this app, and that is an honest limit, not
a bug.** `SystemServiceRegistry` keeps one `LocationManager` per process and that
object caches the `ILocationManager` binder it resolved at construction, so after
a replace-on-attach the guest still talks to the old handle and gets the
structured “module is not attached” answer (measured: the first version of this
scenario, `location-live-attach.yaml`, timed out at `state=ready`). Detach is
observable precisely because the cached handle must stop working. An attach is
seen by resolving the service again — the launch path, which is what
`location-probe.yaml` covers.

## 4. Verification

| check | result | evidence |
|---|---|---|
| `cargo check --workspace --all-targets --offline` | **PASS** (0 errors) | this branch, after both merges |
| `cargo test -p acos-services` (52 suites) | **PASS**, 0 failed | `results/handoff/MODLOC2/services-tests.log` (`location_module` 8/8, `module_kit` 13/13) |
| `cargo test -p acos-binder-peer` | **PASS** 18 (+ the two new) | same log tree / `peer` suite |
| `cargo test -p acos-verify -p acos-protocol` | **PASS** (92 + 28 + lib units) | — |
| `cargo test -p acos-mcp` | **PASS** *with `ACOS_RUNTIME_DIR` unset* (141 tests) | see the environment note below |

Environment note (not from this work): `acos-mcp`'s
`private_dir_is_per_process_and_under_the_runtime_dir` and
`art_default::default_session_tools_use_the_private_art_daemon` pin the private
socket path to `sun_path`'s 100-byte budget **and** to the default runtime dir
(`$ACOS_RUNTIME_DIR/acos`). This worktree's path makes the socket 101 bytes when
`ACOS_RUNTIME_DIR=$PWD/.cache/…`, so the mcp suites must run with
`ACOS_RUNTIME_DIR` unset (then 100 % green). `cargo test -p acos-services` was
run with the private runtime dir, as the brief requires.

### ART on the merged tree (`--engine art`, correctness runs under the shared lock)

| scenario | verdict | evidence |
|---|---|---|
| `results/handoff/MODLOC2/location-probe.yaml` | **pass 12/12** | `v-location-probe/` |
| `results/handoff/MODLOC2/location-geocoder.yaml` | **pass 3/3** | `v-location-geocoder/` |
| `results/handoff/MODLOC2/location-live-detach.yaml` | **pass 12/12** | `v-location-live-detach/` |
| `scenarios/sensors-scripted.yaml` | **pass 16/16** | `v-sensors-scripted/` |
| `results/handoff/MODTAIL/tail-telephony.yaml` | **pass 14/14** | `v-tail-telephony/` |

The detach, from the app's own logcat (`v-location-live-detach/logs/logcat.txt`):

```
AcosLocationProbe: current location Location[gps 37.422000,-122.084100 hAcc=4.0 ...]
AcosLocationProbe: unavailable: java.lang.UnsupportedOperationException: "binder 0x64" is
  provided by service module "location", which is not attached — attach it:
  acos_launch {"services": [{"id": "location", "profile": "scripted"}]}
```

and the verdict's `modules[]` row for `location` is the **final** state
(`profile: off`, `reason: module is off; attach it: …`) even though the run
attached it at launch — the verdict-cites-the-final-rows path MODLOC added.

## 5. What is NOT done / what the orchestrator may want next

* **`requestLocationUpdates(PendingIntent)` still answers `UNSUPPORTED_ANDROID_API`**
  (MODLOC's honest limit). MODNOTIF5 now provides `getIntentSender` +
  `IIntentSender.send` + `ServiceHost::pending_intent_fire`, so the module could
  deliver the pending-intent form instead of refusing it. Not done here (it needs
  the sender round-trip in the location node plus a fixture and a scenario);
  smallest next action: in `location.rs`'s `REGISTER_LOCATION_PENDING_INTENT` arm,
  keep the `IIntentSender` from the request and `pending_intent_fire` a
  `LocationManagerService` intent on each fix, then extend the scenario.
* `/proc/self/...`-based limits, the forward geocoder path and the 2 MiB/10 ms
  budget checks are the same open MODLOC items (unchanged).
* Artifact dirs under `v-*/` hold `verdict.json`, `run.json`, `engine.json`,
  `steps/*.json` and `logs/logcat.txt`; the PNGs/trees are left out to keep the
  handoff small (they are reproduced by re-running the scenario).
