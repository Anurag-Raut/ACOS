# NET1 — `IConnectivityManager` network callbacks (RN NetInfo / Joplin)

**Branch:** `compat-net1-connectivity` (base `main` = `3e11d9c`).
**Status: PARTIAL** — the implementation and unit tests are done and Joplin now
launches 3/3 with a frame. The gate battery, the crate suites and the 2×42-app
corpus were **not** run in this 60-minute window (see "Remaining").

## 1. The wall

On `main`, every React Native app that registers `@react-native-community/netinfo`
aborted at launch:

```
JniException … JavascriptException: [runtime not ready]: Error: Exception in
HostObject::get for prop 'RNCNetInfo'(…) java.lang.UnsupportedOperationException:
unsupported Android API: android.net.IConnectivityManager code 42 (requestNetwork)
```

`ConnectivityService` (added on main in `a98e839`, T-38/T-39) answered only the
query transactions and refused every registration, because ACOS had no network
model. This track supplies the model: **one declared default network**, and
AOSP's `NetworkCallback` protocol on it.

## 2. Transaction and wire verification (not guessed)

* **Codes.** `packages/modules/Connectivity/framework/src/android/net/
  IConnectivityManager.aidl` at `android-14.0.0_r75` lists `requestNetwork` as the
  42nd method and `listenForNetwork` as the 45th; the boot image's decoded
  `acos-aidl` table (`crates/acos-aidl/src/generated/tables.rs`) agrees for every
  code used here (42 request, 45 listen, 47 release, 1/2/3/4/5/6/8/9/10/12/13/14/16/17/20/32/55/63/73/78 queries).
  The AIDL argument lists and the decoded `WireOp` lists are identical.
* **Parcel shapes** were transcribed from the same tag and cross-checked against
  the API-34 image's reader (`NetworkCapabilities.CREATOR`,
  `NetworkRequest.CREATOR`, `LinkProperties.CREATOR`, `NetworkInfo.CREATOR`,
  `Message.readFromParcel`, `Messenger.writeMessengerOrNullToParcel`):
  * `NetworkCapabilities` — 3 longs, 5 ints, 2 nullable parcelables, the uid
    `ArraySet` count (`-1`), 3 never-null int arrays, SSID, boolean, owner /
    requestor / package, the underlying-network typed-list count, enterprise id
    (`FieldOp::Str16` with a null value is byte-identical to `writeParcelable(null)`).
  * `NetworkRequest` — the capabilities as a **bare payload** (`FieldOp::Nested`),
    `legacyType`, `requestId`, `type.name()`.
  * `LinkProperties` — interface name, three address lists, private-DNS facts,
    dhcp byte array, MTU, TCP buffers, routes, proxy byte marker, NAT64, stacked
    links, wake-on-LAN, two captive-portal parcelables.
  * `android.os.Message` — `what, arg1, arg2, obj-presence, when, Bundle,
    replyTo (one `0` word for null), sendingUid, workSourceUid`. Delivered as
    `android.os.IMessenger` code **1** (`oneway interface IMessenger { void
    send(in Message msg); }`), presence marker `1`, then the message body.
* **A bug found in shared code (not fixed here, reported):**
  `parcelable::write_dynamic`/`read_dynamic` write/read a **length word after
  `VAL_PARCELABLE`**. API 34's `Parcel.writeValue` is
  `writeParcelable(v, 0)` — class name then payload, no length. ACOS was
  self-consistent but not framework-compatible for a `Bundle` whose value is a
  parcelable. `write_parcelable_bundle` (new) writes the framework form and is
  the one used for callback messages; `write_dynamic` was left untouched so no
  measured path changes behaviour inside this track.

## 3. What was implemented

`crates/acos-services/src/connectivity.rs` (new) — `ConnectivityState` /
`ConnectivityService`:

* **Registration.** `requestNetwork` (42) and `listenForNetwork` (45) allocate a
  request id, store the record and answer a real `NetworkRequest`:
  `NetworkCapabilities` + `legacyType` + `requestId` + `Type.name()`.
  `Type.ordinal()` is decoded through the enum, so `TRACK_DEFAULT` (2),
  `LISTEN` (1), `REQUEST` (3), `BACKGROUND_REQUEST` (4), `TRACK_SYSTEM_DEFAULT`
  (5), `LISTEN_FOR_BEST` (6) are all accepted and echoed faithfully.
  **`TRACK_DEFAULT`/`TRACK_SYSTEM_DEFAULT` ignore the caller's capabilities and
  use the default network's** — exactly AOSP
  (`ConnectivityService.requestNetwork`: "the passed networkCapabilities is
  unused and will be replaced"), which is why `registerDefaultNetworkCallback`
  sends `null` and why the first implementation panicked before this rule was
  added. A `null` request for any other type is a structured `BadRequest`
  (AOSP throws `IllegalArgumentException`).
* **Release.** `releaseNetworkRequest` (47) removes the registration; a second
  release of an unknown id is a structured `BadRequest` (AOSP
  `IllegalArgumentException`), never a silent success.
* **Callbacks.** AOSP order to the app's `Messenger`: `CALLBACK_AVAILABLE` (2,
  `arg1` = blocked status 0) → `CALLBACK_CAP_CHANGED` (6) →
  `CALLBACK_IP_CHANGED` (7) → `CALLBACK_BLK_CHANGED` (11). Bundle keys are the
  class simple names AOSP uses (`NetworkRequest`, `Network`,
  `NetworkCapabilities`, `LinkProperties`, `LocalNetworkInfo`=null), sorted in
  `ArrayMap` order. A request without a messenger (the system default request)
  queues nothing, matching `if (nri.mMessenger == null) return;`.
* **Queries.** `getActiveNetwork`, `getActiveNetworkForUid`,
  `getActiveNetworkInfo(ForUid)`, `getNetworkInfo`, `getNetworkInfoForUid`,
  `getNetworkForType`, `getAllNetworks`, `getAllNetworkInfo`,
  `getNetworkCapabilities(ForPackage)`, `getDefaultNetworkCapabilitiesForUser`,
  `getActiveLinkProperties`, `getLinkProperties(ForType)`, `isNetworkSupported`,
  `getDefaultRequest`, `getRestoreDefaultNetworkDelay`,
  `getCaptivePortalServerUrl`, plus the previous scalar/void answers. Unknown →
  `null`/`false`, exactly as AOSP answers for a network it does not know.
* **`getNetworkInfoForUid` (code 6)** was added after the first real Joplin run:
  the callbacks arrived, NetInfo's `updateAndSend` ran, and the next wall was
  this legacy query. That is the whole reason Joplin's `onCreate` completed.

### The emulated default network (declared, not discovered)

The guest runs `--unshare-net`: no interface, no sockets. ACOS therefore models
one network — id `100`, legacy type `TYPE_WIFI`, transport `WIFI`, interface
name `wlan0`, MTU 1500 — with capabilities `INTERNET`, `NOT_METERED`,
`NOT_RESTRICTED`, `TRUSTED`, `NOT_VPN`, `NOT_ROAMING`, `FOREGROUND`,
`NOT_CONGESTED`, `NOT_SUSPENDED`, `NOT_VCN_MANAGED`.

**`NET_CAPABILITY_VALIDATED` is deliberately not set**: AOSP sets it only after
a captive-portal probe reached the internet, and ACOS cannot probe. Apps
therefore see `isConnected` true and `isInternetReachable` false — the honest
pair for a sandbox with no sockets. `LinkProperties` carries **no** addresses
(there is no interface to describe); every list is an explicit `0` count rather
than a fabricated address.

## 4. Verification

### Unit tests (acceptance 1) — PASS

```
cargo test -p acos-services --test net1_connectivity   → 9 passed, 0 failed
```

Through the real `ServiceHost` dispatcher (table lookup → argument decode →
handler → reply encode → queued deliveries):

| test | asserts |
|---|---|
| `request_network_answers_a_network_request_and_queues_the_aosp_callbacks` | reply is a `NetworkRequest` typed object with the app's `requestId`, `legacyType`, `TRACK_DEFAULT`; 4 `IMessenger` deliveries with codes/what 2,6,7,11, `arg1` 0, and the AOSP bundle key sets per callback |
| `the_same_network_request_is_echoed_in_every_callback_message` | the embedded `NetworkRequest` equals the reply field-for-field (the app's `sCallbacks` key) |
| `register_default_network_callback_sends_null_capabilities` | `TRACK_DEFAULT` + `null` caps (the RN path) answers the default capabilities and 4 callbacks |
| `a_request_without_a_messenger_queues_no_callbacks` | `TRACK_SYSTEM_DEFAULT`, no messenger → no deliveries |
| `listen_for_network_is_a_listen_request` | `LISTEN` type echoed, 4 callbacks |
| `release_network_request_drops_the_registration_and_unknown_ids_fail` | first release ok, second is `BadRequest` |
| `network_queries_describe_the_emulated_default_network` | `Network` 100, one entry in `getAllNetworks`, caps bits (INTERNET set, VALIDATED unset, WIFI transport), `LinkProperties`, `NetworkInfo` `CONNECTED`, `getNetworkInfoForUid` |
| `pending_intent_registrations_stay_a_structured_refusal` | `UnsupportedAndroidApi`, never a silent success |
| `state_counters_record_each_registration_and_release` | state machine counters |

### Joplin (acceptance 2) — launch PASS, NetInfo JS state NOT CAPTURED

`net.cozic.joplin` (3.7.8, `sha256 79acd248…b2782`), real ART, 3 consecutive
launches on this branch (`.cache/net1/runs-r{1,2,3}`, copies in
`results/compat/NET1/run{1,2,3}/`):

| run | process_start | application_on_create | first_frame | tree | screenshot | tap | SIGABRT |
|---|---|---|---|---|---|---|---|
| 1 | ✓ | ✓ | ✓ 1633.5 ms | ✓ | ✓ | ✗ | none |
| 2 | ✓ | ✓ | ✓ 1630.9 ms | ✓ | ✓ | ✗ | none |
| 3 | ✓ | ✓ | ✓ 1821.2 ms | ✓ | ✓ | ✗ | none |

`session-exit.json` for all three: `activity_resumed: true`,
`application_created: true`, `guest_crash: null`, `unsupported.jsonl` **empty**
(no structured refusals during the run). The tree is Joplin's live UI
(`02-tree.json`, `max_nodes 2000`) and `screen.png` is the published C1 frame.

Callback delivery is proven structurally by the unit tests and by the
intermediate run `.cache/net1/runs/net.cozic.joplin` (kept), whose
`session-exit.json` shows the app's own receiver running our messages:

```
NetworkCallbackConnectivityReceiver.updateAndSend(…:108)
NetworkCallbackConnectivityReceiver.lambda$asyncUpdateAndSend$0(…:155)
```
— i.e. `CALLBACK_AVAILABLE` was parsed by the real framework, the app's
callback thread woke, and only the *next* transaction (`getNetworkInfoForUid`)
was still refused (fixed since; that run is the pre-fix evidence).

**Not captured:** the JS-level `state.isConnected` readout. `k1-run.py` does not
persist the guest logd, so there is no NetInfo log line to quote.
`tap` is false for this app in the corpus at every rung on main and here, so it
is not a regression (it needs a text target the runner does not have).

## 5. Remaining (ranked, smallest first)

1. **Capture NetInfo's JS state.** Persist the guest logd (or add a one-off RN
   console tee) and grep `RNCNetInfo`/`isConnected` for a launch — ~10 minutes
   of harness work, then the acceptance-2 second half is closed.
2. **Gate battery + Gaten flows/negatives.** `tools/art-runtime/run_suite.sh`
   was not copied/run here; the connectivity change touches every launch, so the
   full battery (`v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`,
   `w42-back-frame`, `w15-dialog-menu`, `form-validation`,
   `suite/T03-render-good`, `v1-offscreen-pager` green; the three negative gates
   red with their expected codes) is the first thing to run on this tree.
3. **Crate suites.** `acos-services`, `acos-cli`, `acos-art` (with the documented
   `--test-threads=1 --skip refuses_to_run_when_no_cgroup_can_be_created` then
   that test alone), `acos-verify`.
4. **42-app corpus, 2 interleaved rounds** vs `main`, per-app rung diff, to check
   that answering `getActiveNetwork`/`getNetworkCapabilities` non-null does not
   move any app *down* a rung (an app that now believes it is online may block
   on a socket it cannot open; that is the one plausible regression class).
5. Decide the `VALIDATED` policy (see §6).

## 6. For the orchestrator

* **`VALIDATED` policy.** Declaring `INTERNET` without `VALIDATED` is the honest
  pair, but it means RN's `isInternetReachable` is false. If the corpus shows
  apps gating work on reachability, the choice is between (a) keeping it
  unset (honest, some apps show "offline"), or (b) declaring a network that is
  `VALIDATED` — which asserts reachability ACOS cannot verify, so it needs an
  explicit owner decision, not an agent one.
* **`write_dynamic`/`read_dynamic` `VAL_PARCELABLE` length word** is wrong for
  cross-boundary bundles (see §2). It is unused by this track and was left
  alone; it should be fixed with its own test because `Intent` extras containing
  parcelables would mis-parse.
