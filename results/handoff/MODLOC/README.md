# MODLOC — the `location` service module (v0.4.0)

Branch `modloc` (created from `modules` = MOD0+MOD1), 2026-11-16. Implementation
only; the orchestrator reviews and merges.

## What landed

| Brief item | Where | State |
|---|---|---|
| `location` module, profiles **off / stub / scripted** (no `real`: a sandbox has no GPS) | `crates/acos-services/src/modules/location.rs`, manifest `profiles: [stub, scripted]` | DONE |
| Framework-exact `ILocationManager` for API 34: providers, `getLastKnownLocation`, `requestLocationUpdates` (listener form), `getCurrentLocation`, `removeUpdates`, provider status/enabled callbacks, location setting, fine/coarse permission checks | `location.rs` (`codes`, `LocalService::raw_transact`), AOSP evidence in the module doc comment | DONE (PendingIntent form refused, see limits) |
| Geocoder: scripted answers or the framework's no-geocoder path | `location.rs` (`getFromLocation` = 13, `getFromLocationName` = 14, `IGeocodeListener.onResults`) | DONE (ART proof: `location-geocoder.yaml`) |
| Control ops `set_location`, `play_track`, `set_provider_enabled`, `set_geocoder` (+ `set_location_enabled`, `set_permission`, `stop_track`, `delivered`, `reset`) | `location.rs::CONTROL_OPS`; live via `acos_services {op:"ctl"}` and scenario `service_ctl` | DONE |
| Hot attach, conformance kit green | `AttachMode::Hot`; `tests/module_kit.rs::the_location_module_passes_the_conformance_kit` | DONE |
| Every delivered fix recorded in the session so a verdict can cite it | `LocationState::delivered` → `ModuleReport.reason` → `verdict.json` `modules[].reason`; `ctl_op: "delivered"` returns the list | DONE |
| Real-app proof on ART | `fixtures/apps/location-probe/`, `results/handoff/MODLOC/*.yaml`, `live_location.py` | DONE |
| Generated capabilities/limits row | automatic from the registry (`generated-docs.txt`) | DONE |
| `docs/AGENT_GUIDE.md` how-to | `topic:scenarios` → "Feeding a scripted module (`service_ctl`)" | DONE |

## Framework changes this module needed (all small, all reusable)

1. **Timed delivery to an idle guest.** A module's feed runs on its own thread;
   the peer only drained `system -> app` deliveries after a guest transaction.
   `Peer::tick` now drains them on the peer loop's idle turn
   (`crates/acos-binder-peer/src/peer.rs`), and the ART launcher's peer loop polls
   its socket every 20 ms and writes whatever the tick produced
   (`crates/acos-art/src/launch.rs::serve`). `ServiceHost::take_timed_deliveries`
   is the drain that touches a module's own object; it is deliberately **not**
   folded into `take_deliveries` (a caller holding that object's lock would
   deadlock — `surface_client`'s cached-buffer test caught exactly that).
2. **A module may take over a `core` name.** `location` is a name a phone always
   publishes (core registers an offline node today). `ModuleRegistry::add` now
   lets a module claim a name `core` provides, `ServiceHost::register_dynamic_named`
   keeps **one node per descriptor**, and `Module::off_node` hands back the honest
   "disabled device" node that `ModuleRegistry::detach` reinstalls after the scope
   rollback (`install_off_nodes`, also called by `install_tombstones` so every
   launch path and the kit's baseline agree).
3. **Stale guest proxies after a detach.** A guest that cached the binder got the
   service-layer `unknown binder handle`. `ServiceHost` now remembers the handles
   a detach freed and answers `SERVICE_MODULE_UNAVAILABLE` naming the module and
   the attach call (proven by `live_location.py`).
4. **Scenario `service_ctl` step.** `acos-protocol` (`ServiceCtlStep`), a new
   `Capability::ServiceModule` (a backend may have environment knobs and no module
   registry), `EnvironmentChange::ServiceCtl`, the executor arm, and
   `acos_services {op:"ctl", id, ctl_op, args}` on the MCP/CLI/launcher routes.
5. **Verdict cites the session's final module rows.** `ArtSession` publishes its
   module rows into a shared handle that `verify` reads *after* teardown (which
   asks the launcher once more for `services list`), so the delivered-fix record
   in `modules[].reason` is the end state, not the launch snapshot.
6. **Conformance kit honesty rule.** A name that has a documented disabled node
   answers `UNSUPPORTED_ANDROID_API` after detach; the kit accepts that as honest
   (it still rejects any working answer), and `detaching_core_removes_its_registrations`
   now pins the one surviving name (`location`, the off module's node) instead of
   asserting "no names at all".

## AOSP evidence (image over r75 where they differ)

* Transaction codes are the image's `ILocationManager$Stub` constants, dumped from
  `.cache/boot/android-34/dex/framework/classes2.dex`
  (`TRANSACTION_geocoderIsPresent`=12, `getFromLocation`=13,
  `getFromLocationName`=14, `isProviderEnabledForUser`=47, `isLocationEnabledForUser`=48,
  `getAllProviders`=37, `registerLocationListener`=3, `getCurrentLocation`=2), and
  match the `android-14.0.0_r75` AIDL order.
* **The image's geocoder ABI is the pre-r75 one.** The r75 AIDL has
  `reverseGeocode`/`forwardGeocode` with a request object; the image's
  `Geocoder.getFromLocation:(DDILandroid/location/GeocodeListener;)V` still sends
  `GeocoderParams` + `IGeocodeListener` (`onResults(String, List<Address>)`,
  code 1). The module serves the **image** shape (the guest is what runs) and says
  so in the doc comment.
* AIDL parcelable arguments carry a `writeTypedObject` presence word; strings are
  String16 even when `@nullable` (`@nullable String attributionTag`);
  `LocationRequest.mWorkSource` is non-null and empty by default (16 bytes after
  its presence word) — all three caught by the first real ART runs, all three now
  covered by wire tests.
* Coarse permission: `LocationFudger.createCoarse` behaviour (snap to a 200 m
  grid, drop bearing/speed/altitude/extras, raise accuracy to ≥200 m). The
  framework's slowly-moving `SecureRandom` offset is omitted so a scripted run is
  reproducible; the coarse fix is never finer than the grid.

## Evidence

### Unit and wire tests (`final-tests.log`, `services-tests.log`)

```
cargo test -p acos-services                 -> 100 lib + 46 integration suites, 0 failed
cargo test -p acos-protocol                 -> 66 lib + schemas/scenarios, 0 failed
cargo test -p acos-verify --lib             -> 91 passed
cargo test -p acos-mcp --lib --test guide --test docs -> 83 + 13 + 1 passed
cargo test -p acos-services --test location_module -> 8 passed (wire layouts)
cargo test -p acos-services --test module_kit      -> 11 passed (location kit row)
```

`tests/location_module.rs` builds every request the way the AIDL proxy does and
reads every reply the way the stub does: the `Location` parcel, a real
(non-null, empty) `WorkSource`, `LastLocationRequest`, the coarse/permission
paths, `getAllProviders`, the geocoder request/answer pair, `play_track` delivery
at real timing, the provider-toggle callback, and the post-detach structured
error for a stale handle.

### Real ART (guarded, shared lock)

```
# set_location + a 3-point track, asserted on screen
acos verify results/handoff/MODLOC/location-probe.yaml --engine art --json
  -> verify-location-probe.json   status=pass
     verdict.json modules[location] = scripted, "5 fix(es) delivered to the app
     (last gps lat 37.425 lng -122.087)"
     artifacts/logs/logcat.txt: logcat lines 88-93 show set_location, then the
     three track points ~250 ms apart (real timing)

# scripted geocoder answers
acos verify results/handoff/MODLOC/location-geocoder.yaml --engine art --json
  -> verify-location-geocoder.json status=pass
     logcat: "geocoder present=true", "reverse geocode results=1
     first=1600 Amphitheatre Parkway"

# live attach -> ctl -> detach -> the app's next call is the honest disabled path
python3 results/handoff/MODLOC/live_location.py   -> live-location.json  pass=true
  after_set_location: last=37.4220,-122.0841 accuracy=4.0, fix=37.4220,-122.0841 provider=gps
  after_detach:       state=unavailable: UnsupportedOperationException: "binder 0x4c" is
                      provided by service module "location", which is not attached —
                      attach it: acos_launch {"services":[{"id":"location","profile":"scripted"}]}
                      last=none  fix=none
```

### Generated docs

`generated-docs.txt`: the `acos_help` limits appendix row
``| `location` | `off`, `stub`, `scripted` | hot | ≤2 MiB (estimated) | `location` |``
and the `acos_capabilities` catalog row with all nine control ops (rendered live
by the `acos-mcp` server).

## Fail-before / pass-after

* Every test in `tests/location_module.rs`, the `location` row of
  `tests/module_kit.rs`, `ServiceCtlStep`/`service_ctl` and
  `Capability::ServiceModule` fail on `modules` because none of them exist there
  (`git grep` on `modules` is empty for `location`, `ServiceCtlStep`,
  `take_timed_deliveries`, `off_node`).
* The deadlock the peer tick would have caused was caught by
  `surface_client::a_client_cached_buffer_reference_stores_the_frame_it_names`
  (it hung for >60 s with the drain inside `take_deliveries` and passes now).

## Honest limits

* **`requestLocationUpdates(PendingIntent)` is refused, not delivered.** The guest
  cannot create a `PendingIntent` in this build (ATMS has no `getIntentSender`),
  and delivering one needs `IIntentSender.send` with a marshalled `Intent` (no
  host writer exists). The call answers a structured `UNSUPPORTED_ANDROID_API`
  naming that gap; `unregisterLocationPendingIntent` is accepted and clears.
* **No scenario step for a live module attach/detach yet.** `service_ctl` drives
  control ops only; the detach proof is `live_location.py` over MCP. A
  `services: {op: detach}` step is the next small step if the orchestrator wants
  that in YAML.
* **Coarse fudging omits the framework's random offset** (documented above): the
  grid snap is exact, the offset is the part that is not reproduced.
* **Scripted fixes are not marked `isMock()` by default** (a device reports GNSS
  fixes as real; the verdict's `profile: scripted` row is the honest label).
  `set_location {mock: true}` sets the flag when a test needs it.
* **`getCurrentLocation` answers null `ICancellationSignal`** and keeps the
  request pending until a fix arrives; a cancelled request still resolves once.
* **Geocoder `by_name` (forward) is implemented but not exercised on ART** (the
  probe app only reverse-geocodes); the wire test covers it.
* **RAM/startup budget (2 MiB / 10 ms) is an estimate** and the N=20 density check
  was not run: both are batched by the bench owner.
