# MODLOC3 — `location` on the core-publishes / module-fills pattern

Branch `modloc3`, created from `modules` @ `db0b858c` (MODLOC2). Implementation
only; the orchestrator reviews and merges. Private `ACOS_RUNTIME_DIR` +
`ACOS_SESSION_SOCKET` (`.cache/runtime-modloc3`, 80 chars deep on purpose), every
ART run through `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`, builds
with `CARGO_PROFILE_DEV_DEBUG=0 … CARGO_INCREMENTAL=0` into
`.cache/modloc3-target`.

## 1. The decision: `location` no longer replaces the name, it fills the node

MODLOC2 §5b measured an honest limit that contradicted the ADR: `location`
declared `replaces: ["location"]`, so an attach **replaced the binder**, and
`SystemServiceRegistry` keeps one `LocationManager` per process whose
`ILocationManager` proxy is resolved once — the guest kept talking to the freed
handle and a live attach was invisible.

ADR-0029 note (MODSENS) says what a caching client needs: `core` publishes the
binder node, the module fills it (`core_module::FILLED_BY_MODULE` +
`ServiceHost::dynamic_object_mut`), and a detach empties the node back to the
honest off answer. `location` now does exactly what `sensors`/`accessibility` do:

| piece | before (MODLOC2) | now (MODLOC3) |
|---|---|---|
| manifest | `replaces: &["location"]` | `replaces: &[]` |
| `core_module::FILLED_BY_MODULE` | `sensorservice`, `accessibility` | `… + location` (so `core`'s manifest does not claim the name) |
| `core`'s install (`registry.rs`) | `extras::LocationService` (a second implementation of the off answer) | `modules::location::install(host)` — the module's own **off node** |
| module `attach` | `host.register_dynamic_named(…)` with a new `LocationService` (frees the old binder) | `host.dynamic_object_mut(DESCRIPTOR)` + downcast + `LocationService::fill(state, stamp)` |
| module `detach` | drops its own state; the registry re-registers the off node (new binder) | `LocationService::empty()` **in place**; the same binder answers the disabled device |
| `Module::off_node` | `Some(crate::extras::LocationService)` | removed (like `sensors`); the hook stays documented with no shipped user |
| `extras::LocationService` | the core node | deleted — one implementation of the honest disabled answer |

What a cached proxy sees, in order:

* **off (launch, no module):** `isGeocodeAvailable` false, and
  `UNSUPPORTED_ANDROID_API` for every provider call, naming the module and the
  attach call — the disabled-device truth a phone with location off answers.
* **attach:** the *same* binder starts answering the scripted fixes.
* **detach:** the *same* binder answers the disabled-device truth again. No stale
  fix, no dead handle, no bare `unknown binder handle`.

Conformance kit: `the_location_module_passes_the_conformance_kit` PASS (its
honest-report check compares what the name answered before the attach with what
it answers after the detach — the same node object, so it is identical, and the
`detach-restores` baseline is unchanged because filling registers nothing).

`detaching_core_removes_its_registrations` was updated, and the new assertion is
**stronger**: the `location` node is `core`'s now, so a `core` detach leaves
`service_names() == []` (MODLOC2 allowed the off module's node to survive). The
unit test that held the old stale-handle shape
(`a_provider_toggle_notifies_its_listeners_and_a_detached_module_is_honest`) now
pins the new one: the binder is **identical** across attach and detach, and the
detached answer on that binder is the structured disabled answer.

## 2. ART proof: the app caches `LocationManager` while detached, then gets a live fix

`results/handoff/MODLOC3/location-live-attach.yaml` — **pass, 18/18 steps**,
`v-location-live-attach/` (verdict, run, engine, steps, logcat). The app is the
real `LocationManager` client (`dev.acos.fixtures.locationprobe`); every step goes
through the object it created before the attach:

| step | screen / reply |
|---|---|
| launch (no `services:`) | `state=unavailable: UnsupportedOperationException: … code 37 (location: the location module is not attached; the `location` name answers the disabled-device node (a phone with location off). Attach it: acos_launch {"services": [{"id": "location", "profile": "scripted"}]})`, `last=none`, `fix=none` |
| `service_ctl {op: attach, profile: scripted, config: {permissions: fine}}` | `attached: core, location` |
| tap "Check again" (same cached `LocationManager`) | `state=ready providers=fused,gps,network,passive enabled=true gps=true`, geocoder present=false |
| `service_ctl {op: set_location, 37.4220/-122.0841}` | reply `delivered: 2`, `stored: true`, module report `1 register(s), 2 fix(es) delivered` |
| the listener registered through the cached proxy | `fix=37.4220,-122.0841 provider=gps count=1`, `last=37.4220,-122.0841 accuracy=4.0` |
| `service_ctl {op: detach}` | `attached: core`, `location: off` |
| tap "Check again" | `state=unavailable …`, `last=none`, `fix=none` |

The app's own logcat (`v-location-live-attach/logs/logcat.txt`) is the narrative:

```
unavailable: … "location" is not attached … attach it: acos_launch {"services": [{"id": "location", …}]}
providers=[fused, gps, network, passive] enabled=true gps=true     <- after the live attach
registered for gps updates
fix #1 Location[gps 37.422000,-122.084100 hAcc=4.0 …]              <- through the pre-attach proxy
current location Location[gps 37.422000,-122.084100 hAcc=4.0 …]
unavailable: … "location" is not attached …                        <- after the live detach
```

## 3. `requestLocationUpdates(PendingIntent)` (code 5): blocked by PIFILLIN, reported

**Status: BLOCKED on an out-of-scope dependency, per this brief's instruction
("if delivering an extra through a fired sender needs the fill-in merge that is
out of scope (PIFILLIN), stop and report").** No delivery was faked.

The image dex is authoritative and says the fix travels as a **fill-in intent**:

* `services/classes2.dex`, class
  `Lcom/android/server/location/provider/LocationProviderManager$LocationPendingIntentTransport;`
* `deliverOnLocationChanged(LocationResult, IRemoteCallback)` builds
  `new Intent()`, `putExtra("location", result.getLastLocation())` (plus
  `"locations"` with a `Location[]` when the result is a batch) and calls
  `LocationPendingIntentTransport$PendingIntentSender.send(PendingIntent, Context,
  Intent, Runnable, Bundle)` — the `Intent` argument is `PendingIntent.send`'s
  **fill-in**.
* `deliverOnProviderEnabledChanged(String, boolean)` puts `"providerEnabled"` and
  calls the same sender path.
  Full disassembly: `results/handoff/MODLOC3/pending-intent-dex.txt`.

Two ACOS-side facts decide it:

1. `ServiceHost`'s fire path is `FireSender = Fn(binder, code)` — there is no
   parameter that could carry an `Intent`.
2. MODNOTIF5's decision (already merged) is that `IIntentSender.send` **refuses**
   a fill-in on a mutable sender (`Intent.fillIn` is not modelled — PIFILLIN) and
   **drops** it on an immutable one. A `PendingIntent` an app makes for
   `requestLocationUpdates` is exactly a mutable broadcast/activity sender, so the
   framework's own call would be refused on the guest side anyway.

Accepting the registration would therefore mean firing the stored intent with no
Location (a `PendingIntent` that looks delivered but carries nothing) or writing a
stale fix into the record (the record is replayed on every later fire; AOSP never
mutates it). Both are fake success, so the arm keeps its structured refusal and the
message now names the real gap instead of the obsolete "no IIntentSender.send"
reason:

```
UNSUPPORTED_ANDROID_API, interface=android.location.ILocationManager, code=5
location: registerLocationPendingIntent(gps) refused: the framework delivers a fix as the
**fill-in** intent of IIntentSender.send (LocationPendingIntentTransport puts the Location
under LocationManager.KEY_LOCATION_CHANGED), and merging a fill-in into the stored intent
needs Intent.fillIn, which this build refuses (PIFILLIN); use the LocationListener form
```

Probe: `crates/acos-services/tests/location_module.rs::the_pending_intent_form_is_refused_and_names_the_fill_in_it_needs`
asserts the code is the transaction the caller sent (5), that the message names
`KEY_LOCATION_CHANGED`, the fill-in merge and `PIFILLIN`, and the listener form.
Also fixed: the arm reported `code: 0` (a made-up interface code) instead of the
request's transaction.

Smallest next action for the orchestrator: decide whether to fund PIFILLIN
(`Intent.fillIn` extras merge + a `FireSender` that carries an intent). With it,
the arm becomes: keep the request's `IIntentSender`, and on every fix
`host.pending_intent_fire(binder, 0)` with a fill-in intent carrying
`KEY_LOCATION_CHANGED` (and `locations` for a batch, `providerEnabled` for the
toggle) — plus one scenario step per delivery.

## 4. Harness hygiene: `acos-mcp` tests own their runtime dir

MODLOC2 had to run `acos-mcp` with `ACOS_RUNTIME_DIR` unset: two tests pinned the
private socket path to `sun_path`'s 100-byte budget *and* to the caller's runtime
dir, so a deep worktree scratch path (101 bytes) failed them. Fixed at the cause,
no assertion weakened:

* `crates/acos-mcp/src/private_daemon.rs` — `private_runtime_dir()` is now
  `private_runtime_dir_in(base)`; the unit test
  `private_dir_is_per_process_and_under_the_runtime_dir` builds its own short base
  (`/run/user/<uid>` else the temp dir, the INTEG030 pattern) and asserts the same
  four properties against it (per-pid name, under the base, socket ≤ 100 bytes).
* `crates/acos-mcp/tests/art_default.rs` — `Server::spawn` now `env_remove`s
  `ACOS_RUNTIME_DIR` (alongside the engine/daemon vars it already dropped), so the
  short `XDG_RUNTIME_DIR` the test creates decides the private socket, and the
  assertions (`socket` under `<runtime>/acos`, per-server-pid name, ≤ 100 bytes)
  are untouched.

Both suites were run **with** `ACOS_RUNTIME_DIR=$PWD/.cache/runtime-modloc3`
(80 characters, socket path over the budget — the exact shape that used to fail)
and **without** it: green both ways.

## 5. Verification

| check | result | evidence |
|---|---|---|
| `cargo test -p acos-services` | **PASS** — 59 suites, 478 tests, 0 failed | `results/handoff/MODLOC3/services-tests.log` (`location_module` 9/9 incl. the new probe, `module_kit` 16/16) |
| `cargo test -p acos-binder-peer` | **PASS** — 18 + 3 | this branch (`peer` suite) |
| `cargo test -p acos-mcp` **with** a deep `ACOS_RUNTIME_DIR` (80 chars) | **PASS** — 141 tests | §4 |
| `cargo test -p acos-mcp` **without** `ACOS_RUNTIME_DIR` | **PASS** — 141 tests | §4 |
| `cargo check -p acos-services --all-targets` | **PASS** (only pre-existing warnings) | — |
| `location-live-attach.yaml` (new) | **pass 18/18**, `art-reference` | `v-location-live-attach/verdict.json` |
| `location-probe.yaml` (MODLOC2) | **pass 12/12** | `v-location-probe/` |
| `location-geocoder.yaml` (MODLOC2) | **pass 3/3** | `v-location-geocoder/` |
| `location-live-detach.yaml` (MODLOC2) | **pass 12/12** | `v-location-live-detach/` |
| `scenarios/sensors-scripted.yaml` (fill pattern neighbour) | **pass 16/16** | `v-sensors-scripted/` |

Runs were `tools/art-runtime/run-exclusive.sh --task MODLOC3 -- tools/guarded.sh
--mem 8G --time 600 -- .cache/modloc3-target/release/acos verify <yaml> --engine
art --artifacts …` with `ACOS_FONT_DIR` set. Artifact dirs hold `verdict.json`,
`run.json`, `engine.json`, `events.jsonl`, `steps/*.json` and `logs/logcat.txt`;
the PNG frames and UI trees are left out to keep the handoff small (re-running the
scenario reproduces them).

## 6. What is NOT done

* **PIFILLIN** (§3) — the only thing between the module and the PendingIntent
  form. Not a location-module issue: it is the `Intent.fillIn` merge and an
  intent-carrying fire path.
* The MODLOC open items are unchanged (forward geocoder, `/proc/self/…` limits,
  the 2 MiB/10 ms budget checks).
* `Module::off_node` / `ModuleRegistry::install_off_nodes` now have no shipped
  user. Kept because the conformance-kit baseline and `detach` call them and the
  trait documents the two shapes; removing them is a separate cleanup if the
  orchestrator wants fewer extension points.
