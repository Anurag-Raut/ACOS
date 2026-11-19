# MODFILL — clipboard + autofill proofs, `net.wifi`, `media.session`

Branch `modfillc` (continuation of `modfillb`, which died on a rate limit after
`ab401c966`). Everything below is measured **on this tree**; the evidence is in
`evidence/<scenario>/` (`verdict.json` = the verdict and every step) and the
command is in §6.

## 1. What this track changed

| # | change | kind |
|---|---|---|
| 1 | `core` clipboard serves `ClipData` calls raw on ART (`serves_raw`, `read_clip_shape`) — `modfill-clipboard` (from `ab401c966`, re-run here) | core fix |
| 2 | `net.wifi` module (`modules/wifi.rs`): `off`/`stub`/`scripted`, hot | new module |
| 3 | `media.session` module (`modules/media_session.rs`): `off`/`stub`/`scripted`, hot | new module |
| 4 | **Root cause fixed:** `PmsService::set_package_from_apk_as` rebuilt the package table with a fresh `FeatureTable`, so every feature a module turns on at attach (`android.hardware.wifi`, `.telephony`, `.bluetooth`, …) was **withdrawn when the session installed the launched APK** (modules attach first). `hasSystemFeature` disagreed with the attached set and `WifiManager` (factory gated on `FEATURE_WIFI`) was `null` although `net.wifi` was attached. The table now survives the replacement. Test: `wifi_module::a_module_feature_survives_the_launched_apk_install` — fails without the one-line fix, passes with it | core fix |
| 5 | `ServiceHost::{set_connectivity_state, connectivity_state}`: the one handle to the default-network state **whichever** service is answering (`core`'s, or the `network` module's while it replaces it; the module hands `core`'s back on detach). A module that moves the default network (`net.wifi`) uses it instead of reaching around the service | host seam |
| 6 | `ServiceObject::drain_broadcasts`: a node asks the host to originate a broadcast after its reply (`startScan` → `SCAN_RESULTS_AVAILABLE`) | host seam |
| 7 | Parcelable codec rows, all `Verification::Full` against the **image's** framework dex: `WifiInfo`, `ScanResult`, `WifiSsid`, `SupplicantState`, `DhcpInfo`, `MediaMetadata`, `PlaybackState`, `PlaybackState$CustomAction`; `Parcel.writeValue` `VAL_CHARSEQUENCE` reader | wire formats |
| 8 | Fixtures `wifi-probe`, `media-probe`; scenarios `modfill-wifi`, `modfill-media`; `docs/MODULES.md` regenerated | proof |

Wire-format findings (image dex vs the AOSP source cache — the image won):

* `WifiInfo.writeToParcel` of the image ends at `mApTidToLinkMappingNegotiationSupported`; the cache's AOSP-`main` source also writes `writeList(mVendorData)`. Not written here.
* `SupplicantState.writeToParcel` is `writeString(name())`, not an ordinal (the first draft wrote an int; the dex scan test caught it).
* `ScanResult`'s three optional lists compile to `writeInt(0)` arms that duplicate the tail of the method (D8); the test pins the first pass in full and proves the rest is exactly that copy.
* `Message.writeToParcel` uses one uid word (NET6, already recorded in `connectivity.rs`).

## 2. Truth table

| item | implemented | profiles / attach | kit | proof (this tree) | evidence |
|---|---|---|---|---|---|
| clipboard (`core`) | yes | — | (core) | `modfill-clipboard` **pass 22/22** — copy, primary-clip-changed listener fires (n=1,2,3), paste, describe, clear | `evidence/modfill-clipboard/` |
| autofill (`core`) | **disabled by design** — no autofill provider, and the service says so | — | (core) | `modfill-autofill` **pass 12/12** — see expectations below | `evidence/modfill-autofill/` |
| `net.wifi` | yes | `off`,`stub`,`scripted`; hot | **PASS** (`module_kit`) | `modfill-wifi` **pass 44/44** | `evidence/modfill-wifi/` |
| `media.session` | yes | `off`,`stub`,`scripted`; hot | **PASS** (`module_kit`) | `modfill-media` **pass 32/32** | `evidence/modfill-media/` |

### Autofill — honest expectations

The platform has no autofill provider, so the proof asserts the honest chain,
not a fill: `FEATURE_AUTOFILL` absent → `isEnabled()` is `false` **without a
stall** (`took_ms` asserted to be at most 4 digits, i.e. not the framework's 15000 ms give-up: the client answered itself), `hasEnabledAutofillServices()`
is `false` (a real `IAutoFillManager.isServiceEnabled` round trip), `requestAutofill()`
returns and the field keeps the app's own text (a filled field would be a fabricated
success). The service-side wire shape (`IResultReceiver.send` answers) is pinned
byte-for-byte by `tests/autofill_wire.rs`, because the app cannot reach it while
the feature is off. A real fill needs an autofill *service app*; that is a separate
module and is **not** claimed.

### `net.wifi` — what the app sees (all values from the real `WifiManager`)

| step in `modfill-wifi` | observed |
|---|---|
| `getConnectionInfo` (config SSID `Office`, RSSI −48) | `ssid="Office" bssid=aa:bb:cc:dd:ee:01 rssi=-48 freq=2437 speed=72 netid=0 supp=COMPLETED level=4` |
| `registerDefaultNetworkCallback` | `onAvailable`, capabilities `wifi=true validated=false` |
| `setWifiEnabled(false)` (target 34) | `accepted=false enabled=true` (AOSP answer for an unprivileged app) |
| `getDhcpInfo` without the `network` module | all zeros — **no fake connectivity** |
| control `rssi -73` | `RSSI_CHANGED newRssi=-73`; `getConnectionInfo` → `rssi=-73 level=2` |
| `startScan` | `true`; `SCAN_RESULTS_AVAILABLE` and `ScanResultsCallback.onScanResultsAvailable` |
| `getScanResults` | the two scripted `ScanResult`s (SSID, BSSID, level, frequency, capabilities) |
| control `scan {results}` | list replaced, callbacks + broadcast again |
| control `disconnect` | `onLost`; `NETWORK_STATE_CHANGED` DISCONNECTED; `getActiveNetwork()` null; `WifiInfo` = `<unknown ssid>`, −127, `DISCONNECTED` |
| control `connect {ssid: Cafe}` | `onAvailable`; `NETWORK_STATE_CHANGED` CONNECTED with `bssid`/`WifiInfo` extras |
| control `enable {on:false}` | `WIFI_STATE_CHANGED` 0 (previous 3) → 1 (previous 0), `onLost`, `startScan()` → `false` |

Consistency with the `network` module (`tests/wifi_network_gate.rs`, one process because
`network::` is process-global): `network`'s `offline` keeps the default network down
across a Wi-Fi `connect`; while the relay is `real`, `disconnect`/`enable{on:false}` are
refused with a structured error naming `acos_net offline`; with the relay real the
`DhcpInfo`/`WifiInfo` address are the session's own. **Not run on ART** — the host has no
`pasta` (`net-network-real` is the same blocker, see CONFKIT).

### `media.session` — what the app and the agent see

| step in `modfill-media` | observed |
|---|---|
| `new MediaSession` + `setActive` + `MediaController(token)` | `tag=acos-music pkg=dev.acos.fixtures.mediaprobe` (real `ISession.getController` binder) |
| `setMetadata` + `setPlaybackState` | the controller callback runs (`CTLCB state=PLAYING`, `CTLCB title=…`); `controller.getPlaybackState()/getMetadata()` return the **app's own parcel bytes** (`actions=310 … duration=337000`) |
| control `sessions` | the agent reads package, tag, active, playback state, metadata, delivered commands |
| control `pause` / `next` / `seek` / `previous` / `custom_action` / `play` | each reaches the app's `MediaSession.Callback` with `getCurrentControllerInfo().getPackageName() == "dev.acos.agent"`; the app republishes and the controller callback follows |
| app's own `MediaController.getTransportControls().pause()` | reaches the same callback, caller = the app's package (pid/uid reported `-1`, not invented) |
| `MediaSessionManager.getActiveSessions` | `UnsupportedOperationException` — **recorded as a gap, not hidden** |

## 3. Conformance kit

`module_kit::every_registered_module_passes_the_conformance_kit` runs over
`ModuleRegistry::standard().ids()`; `net.wifi` and `media.session` are in it (the
registry now has 24 modules) and both pass all six checks (manifest, attach,
detach-restores, honest-report, negative, budget). `module_kit::
every_registered_module_is_a_verdict_row_and_honours_its_attach_mode` and
`modules_doc::docs_modules_md_matches_the_registry` pass with the regenerated
`docs/MODULES.md`. `media.session` provides two names (`media_session`, `media_router`);
the tombstone rows return when it detaches (`detach_returns_the_tombstone…`).

## 4. Not implemented / limits (honest)

| item | state |
|---|---|
| Wi-Fi `real` profile | none — no radio. `connect(WifiNetworkSpecifier)`, configured networks, suggestions, hotspot, P2P, RTT, passpoint: structured `UNSUPPORTED_ANDROID_API`. `NetworkCapabilities.getTransportInfo()` (a `WifiInfo` on the default network) is `null`: `core`'s capabilities carry no transport info |
| Wi-Fi caller identity | every caller gets the scripted view; `scan_access: false` redacts SSID/BSSID like `WifiInfo.makeCopy` but is per session, not per app |
| `network` + `wifi` pre-existing mismatch | `network` publishes `android.hardware.wifi` while the `wifi` name is `null` unless `net.wifi` is attached; `WifiManager` is then null (the tombstone row says "ACOS reports no Wi-Fi feature", which is only true when `network` is off). Not changed here — it is `network`'s manifest |
| media: `getSessions`, `addSessionsListener`, media-key dispatch, queue, volume, remote sessions, `media_router` | structured `UNSUPPORTED_ANDROID_API` |
| media: controller-originated command identity | package from the call, pid/uid `-1` (the host does not hand a `ServiceObject` the calling identity) |
| media: undecodable metadata (album-art `Bitmap`) | accepted, reported `unreadable`; a controller's `getMetadata()` then answers a structured error, never a made-up value |
| `net-check` / `net-network-real` / `net-network-offline` | need `pasta` + `ACOS_SPIKE_NET`; fail on this host exactly as before (`HTTP 200` expected, `SecurityException` seen) — unrelated to this change |

## 5. Regression runs (existing module scenarios, same binary, not committed)

`tail-vibrator`, `tail-telephony`, `tail-bluetooth`, `tail-biometric`, `tail-detached`,
`location-probe` (one `POST_RESUME_NOT_READY` infra flake, **pass** on rerun),
`location-geocoder`, `location-live-attach`, `sensors-scripted`, `sensors-off`,
`net-network-off` — all **pass**. The `FeatureTable` fix changes what
`hasSystemFeature` answers for module features in a launched session; every one of
those scenarios still passes with it.

## 6. Commands

```
source tools/agent-env.sh                        # CARGO_TARGET_DIR=$PWD/.cache/modfill-target
tools/build-apps.sh && tools/build-dogfood-apps.sh
cargo test --release --offline -p acos-services  # 89 suites, 0 failed
tools/art-runtime/run-exclusive.sh --task MODFILL -- tools/guarded.sh --mem 14G --time 1500 -- \
    cargo test --release --offline -p acos-art   # 0 failed (1 pre-existing ignore)
tools/art-runtime/run-exclusive.sh --task MODFILL -- tools/guarded.sh --mem 8G --time 580 -- \
    results/handoff/MODFILL/run-batch.sh modfill-clipboard modfill-autofill modfill-wifi modfill-media
cargo run -p acos-services --example gen-modules-docs > docs/MODULES.md
```

Evidence is trimmed to < 10 MB (frames dropped; verdict, steps, logcat, events, tree kept).
