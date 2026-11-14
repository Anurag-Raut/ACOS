# NET1 — `IConnectivityManager` network callbacks (RN NetInfo / Joplin)

**Branch:** `compat-net1-connectivity` (base `main` = `3e11d9c`).
**Status: PARTIAL (verified)** — the implementation and unit tests are done and
Joplin now launches with a frame. The NET1B window closed the three verification
gaps: the four crate suites are green (§7.1), all 8 positive gates plus the 5
Gaten flows pass and all 3 negative gates plus the 5 Gaten negatives fail with
their designed codes (§7.2, §7.3), and the guest logd is now persisted by
`tools/compat/k1-run.py` (§7.4). The 2×42-app corpus is **round 1 only** in this
window (§7.5): the host ART lock was contended and 42 apps × 2 arms does not fit
in 60 minutes.

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
* **Parcel shapes, DEX-verified.** The four new specs are also scanned out of
  the **image's own** `writeToParcel`
  (`test the_connectivity_specs_match_the_boot_images_write_to_parcel`, which
  reads `.cache/boot/android-34/dex/framework-connectivity` — the classes are in
  the `framework-connectivity` APEX, so `tests/parcelable.rs`'s
  `dex/framework` scan skips them):
  * `android.net.NetworkRequest` — **4/4 full match** (image order + arity).
  * `android.net.NetworkInfo` — **11/11 full match**.
  * `NetworkCapabilities` (18 direct `Parcel` calls) and `LinkProperties` (17)
    call private helpers (`writeParcelableArraySet`, `writeAddresses`,
    `writeAddress`) the scanner cannot see into; the specs model those as their
    inlined wire form, so the test **pins** the image's observed call sequence
    (it fails if the image's order or arity changes). The bytes are identical:
    the helper writes are exactly the `-1`/`0` counts the specs emit.
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

### Unit tests (acceptance 1) — PASS (all four crates in §7.1)

```
cargo test -p acos-services --test net1_connectivity   → 10 passed, 0 failed
cargo test -p acos-services                            → all suites green once the
  boot-DEX symlink exists; `tests/parcelable.rs` needs `.cache/boot/android-34`
  (it was missing from this worktree at first and recreated).
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
| `the_connectivity_specs_match_the_boot_images_write_to_parcel` | the image's DEX `writeToParcel` for all four classes (see §2) |

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

## 5. Remaining (ranked, smallest first) — see §7 for what NET1B closed

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

## 7. NET1B verification window

Everything below was measured on this branch at `e5eeba3` with binaries built
from it into `.cache/s3ab-target/release` (`cargo build --release --offline`,
12 s incremental, no source drift). Every ART-touching command ran under
`tools/art-runtime/run-exclusive.sh --task NET1B -- tools/guarded.sh …`.

### 7.1 Crate suites (acceptance 1) — PASS

`cargo test --release --offline`, one binary at a time, `--test-threads=1`,
logs in `.cache/net1b/tests/` (runner: `.cache/net1b/run_tests.sh`).

| crate | test binaries | passed | failed | ignored |
|---|---:|---:|---:|---:|
| `acos-services` | 41 | **312** | 0 | 0 |
| `acos-verify` | 3 | **66** | 0 | 0 |
| `acos-art` | 14 + 1 alone | **150** | 0 | 0 |
| `acos-cli` | 15 | **60** | 0 | 5 |

* `acos-art` uses the documented flake recipe: the batch run skips
  `refuses_to_run_when_no_cgroup_can_be_created`, then it runs alone. Running it
  alone is **not sufficient**: `RunCgroup::prepare` reads a per-boot capability
  stamp (`cgroup-capability.json` under `ACOS_ART_DATA`, H15) and the stamp from
  the batch run makes the fake-`systemd-run` probe unreachable, so the test fails
  with `RunCgroup { cg: "" }`. The honest invocation points `ACOS_ART_DATA` at a
  cold directory so the probe path actually executes:
  `env ACOS_ART_DATA=.cache/net1b/tests/cold-cg cargo test -p acos-art --test
  sandbox_s5 -- --exact refuses_to_run_when_no_cgroup_can_be_created` → 1 passed
  (`.cache/net1b/tests/acos-art-cgroup-coldcache.log`). No test was skipped,
  weakened or `#[ignore]`d.
* The 5 ignored tests are pre-existing in the tree, not this track:
  `gl1_kept_session` 4/4 ignored, `w50_serve_art` 1 ignored.
* `0 passed / 0 failed` binaries in the logs are the doctest and empty-binary
  stubs cargo emits; the per-crate totals above are the sum over all binaries.

### 7.2 Gates (acceptance 2a) — 8/8 green, 3/3 red as designed

`acos verify scenarios/<gate>.yaml --engine art --json`, JSON in
`.cache/net1b/suite/`, runner `.cache/net1b/run_suite.sh gates` (copied from the
int reference runner, paths retargeted):

| gate | rc | status | code |
|---|---:|---|---|
| `v2-art-counter` | 0 | pass | — |
| `w22-counter-reset` | 0 | pass | — |
| `w54-dialog-pixels` | 0 | pass | — |
| `w42-back-frame` | 0 | pass | — |
| `w15-dialog-menu` | 0 | pass | — |
| `form-validation` | 0 | pass | — |
| `suite/T03-render-good` | 0 | pass | — |
| `v1-offscreen-pager` | 0 | pass | — |
| `suite/T03-render-wrong` | 1 | fail | **`ASSERTION_FAILED`** (`region differs from …/T03-hello-views-top.png (568029 differing bytes)`) |
| `negative/offscreen-pager-visible` | 1 | fail | **`ELEMENT_OFFSCREEN`** |
| `p11-progress-timeout` | 1 | timeout | **`TIMEOUT`** |

**0 false passes:** every positive gate passed *and* every negative gate failed
with its own designed code, so no assertion was satisfied by accident. The
known main-side red `w7-insets-report` (track W7B) is outside this battery.

The runner's `verdict()` reads `result.verdict.steps`; per scenario it shows the
scenario really executed (steps / assertions actually evaluated / the failing
step):

| scenario | steps | assertions | first failing step |
|---|---:|---:|---|
| `v2-art-counter` | 11 | 8 | — |
| `w22-counter-reset` | 9 | 5 | — |
| `w54-dialog-pixels` | 14 | 9 | — |
| `w42-back-frame` | 17 | 12 | — |
| `w15-dialog-menu` | 3 | 2 | — |
| `form-validation` | 28 | 21 | — |
| `suite/T03-render-good` | 5 | 3 | — |
| `v1-offscreen-pager` | 9 | 5 | — |
| `suite/T03-render-wrong` | 3 | 1 | `assert_region` → `ASSERTION_FAILED` |
| `negative/offscreen-pager-visible` | 3 | 1 | `assert_visible` → `ELEMENT_OFFSCREEN` |
| `p11-progress-timeout` | 3 | 0 | `tap` → `TIMEOUT` |

### 7.3 Gaten flows and negatives (acceptance 2b) — 5/5 and 5/5

`acos verify scenarios/gaten-*.yaml --apk <gaten apk> --engine art --json`

| flow | rc | status |
|---|---:|---|
| `gaten-home` | 0 | pass |
| `gaten-tabs` | 0 | pass |
| `gaten-settings` | 0 | pass |
| `gaten-search` | 0 | pass |
| `gaten-lesson` | 0 | pass |
| `negative/gaten-home-wrong` | 1 | fail `ASSERTION_FAILED` |
| `negative/gaten-tabs-wrong` | 1 | fail `ASSERTION_FAILED` |
| `negative/gaten-settings-wrong` | 1 | fail `ASSERTION_FAILED` |
| `negative/gaten-search-wrong` | 1 | fail `ASSERTION_FAILED` |
| `negative/gaten-lesson-wrong` | 1 | fail `ASSERTION_FAILED` |

`gaten-home` timing out was the pre-existing red in K4 (`results/compat/K4.md`
§5.1); it passes here, 5/5 flows in one acquisition (09:40:04→09:45:50 UTC,
~67 s per flow). Each flow evaluated its assertions (`home` 14 steps/9
assertions, `tabs` 22/13, `settings` 33/17, `search` 22/11, `lesson` 28/13) and
each negative failed on `assert_text` with `ASSERTION_FAILED` (`home` 8 steps,
`tabs` 8, `settings` 13, `search` 17, `lesson` 11 — i.e. they failed at the
first, deliberately wrong assertion rather than not running).

### 7.4 NetInfo state evidence (acceptance 3) — logd persisted, JS state not logged by Joplin

`tools/compat/k1-run.py` now reads `acos_logs` before destroying the guest and
writes `06-logs.json` (last `--logs-limit` records, default 3000; `--no-logs`
disables). `report["logs"]` carries `total`, `kept`, the tag census and the
engine's own `note`. This is what used to be thrown away with the guest, and it
is the only place an app's JS-level state can be observed.

One Joplin launch on this branch (`.cache/net1b/joplin/run-095315`, evidence
copied to `results/compat/NET1/netinfo-run/`): first frame, full ladder, 266
logd records, `ReactNativeJS` ×105 of them. What the log says:

```
I ReactNativeJS Running "main"
I ReactNativeJS '…', 'Starting application net.cozic.joplin-mobile v3.7.8 (prod)'
I ReactNativeJS '…', 'Client ID: 266d4d2d40e648529f427f2f59d83c0f'
I ReactNativeJS '…', 'Performance: buildStartupTasks/go: initial route: End at 0.51s'
```

i.e. the JS bundle evaluated past module init — the exact point where `main`
aborted (`Exception in HostObject::get for prop 'RNCNetInfo'`) — and the app
went on to open its database and route to its first screen.

**What it does not show, honestly:** Joplin's release bundle logs no
`NetInfo`/`isConnected` line, and none of the 266 records mentions `netinfo`,
`isInternetReachable` or a connectivity callback. The NetInfo *values* are
therefore still not quoted from the app's own console. Whole-log census
(`results/compat/NET1/netinfo-run/logd.txt`, grep for
`netinfo|connect|online|offline|reach|network|sync`): 5 hits, none from NetInfo —
`nativeloader` ×2 (library preload), `SystemServiceRegistry E No service
published for: wifi` (the WifiManager wall, unchanged by this track), and
`SQLitePlugin` write errors on the fresh app-data dir (`no such table: version`,
`no such module: spellfix1` — first-run schema, the app recovers and keeps
running). No `JniException`, no SIGABRT, `unsupported.jsonl` empty.

The honest chain for the state is therefore: the parcel shapes are asserted
against the image's own readers (§2, §4), the callback protocol is asserted
through the real dispatcher (`net1_connectivity`, 10 tests), and the app-side
proof is that the receiver ran and the app continued — with
`isConnected=true` / `isInternetReachable=false` the declared, decidable
outcome (§3). Quoting the JS value itself needs either a NetInfo probe app that
logs it, or an ACOS-side trace of the `IConnectivityManager` transactions the
module receives; both are small, neither fit in this window.

One limitation of the snapshot: `acos_logs` needs a live session, so it answers
`SESSION_NOT_FOUND` after a guest that died during launch (e.g.
`com.nextcloud.client` in §7.5, where the guest exits with status 1 before any
frame). The logd of a failed launch is therefore still lost; capturing it needs
the snapshot taken inside the engine's failure path (or the session artifacts
to keep the logd sink).

### 7.5 42-app corpus (acceptance 4) — round 1 complete, round 2 in flight

The K1 corpus (42 real apps, `.cache/corpus/k1`) through the same harness the
K-tracks use (`tools/compat/run-k1.sh`'s `k1-run.py`), **both arms in the same
acquisition per app** (order: branch then main), one `acos serve --engine art`
daemon per app per arm, run dirs `.cache/net1b/corpus/r{1,2}/<arm>/<pkg>`
(`.cache/net1b/corpus.sh`). Main arm = the read-only detached checkout
`.cache/wt-abmain` (`3e11d9c`, binary `release/acos` built 14:09), run but never
built from here.

Rung key: `inspect, process_start, oncreate, first_frame, tree, tap` —
`Y` reached, `.` not; `@xms` is the launch reply wall time (first frame).

Totals (r1, 42 apps):

| rung | main | **branch** | Δ |
|---|---:|---:|---:|
| inspect | 42 | 42 | 0 |
| process_start | 41 | **42** | **+1** |
| onCreate | 41 | **42** | **+1** |
| first frame | 15 | **16** | **+1** |
| tree | 15 | **16** | **+1** |
| tap | 9 | 9 | 0 |

| app | r1-branch | r1-main |
|---|---|---|
| `chat.fluffy.fluffychat` | YYY... | YYY... |
| `com.aurora.store` | YYY... | YYY... |
| `com.beemdevelopment.aegis` | YYYYYY@499ms | YYYYYY@497ms |
| `com.dot.gallery` | YYY... | YYY... |
| `com.fsck.k9` | YYY... | YYY... |
| `com.github.gotify` | YYY... | YYY... |
| `com.github.muellerma.coffee` | YYYYYY@420ms | YYYYYY@398ms |
| `com.ichi2.anki` | YYY... | YYY... |
| `com.ismartcoding.plain` | YYY... | YYY... |
| `com.keylesspalace.tusky` | YYY... | YYY... |
| `com.libreweatherrn` | YYY... | YYY... |
| `com.money.manager.ex` | YYYYYY@469ms | YYYYYY@513ms |
| `com.nextcloud.client` | YYY... | Y..... |
| `com.nononsenseapps.feeder` | YYYYY.@1256ms | YYYYY.@1244ms |
| `com.perflyst.twire` | YYYYYY@1998ms | YYYYYY@1991ms |
| `com.sadellie.unitto` | YYYYY.@937ms | YYYYY.@962ms |
| `com.shatteredpixel.shatteredpixeldungeon` | YYY... | YYY... |
| `com.tailscale.ipn` | YYY... | YYY... |
| `com.termux` | YYY... | YYY... |
| `com.unciv.app` | YYY... | YYY... |
| `de.danoeh.antennapod` | YYYYYY@1260ms | YYYYYY@1261ms |
| `de.tutao.calendar` | YYY... | YYY... |
| `de.westnordost.streetcomplete` | YYYYY.@1430ms | YYYYY.@1461ms |
| `io.anuke.mindustry` | YYY... | YYY... |
| `io.element.android.x` | YYY... | YYY... |
| `io.github.muntashirakon.AppManager` | YYYYY.@1523ms | YYYYY.@1516ms |
| `it.niedermann.owncloud.notes` | YYY... | YYY... |
| `me.zhanghai.android.files` | YYY... | YYY... |
| `net.cozic.joplin` | YYYYY.@1650ms | YYY... |
| `net.gsantner.markor` | YYYYYY@684ms | YYYYYY@688ms |
| `net.mullvad.mullvadvpn` | YYYYY.@1126ms | YYYYY.@1072ms |
| `org.billthefarmer.editor` | YYYYYY@389ms | YYYYYY@313ms |
| `org.breezyweather` | YYY... | YYY... |
| `org.fdroid.basic` | YYY... | YYY... |
| `org.fdroid.fdroid` | YYY... | YYY... |
| `org.jellyfin.mobile` | YYYYYY@551ms | YYYYYY@571ms |
| `org.kde.kdeconnect_tp` | YYYYYY@629ms | YYYYYY@627ms |
| `org.quantumbadger.redreader` | YYY... | YYY... |
| `org.secuso.privacyfriendlynotes` | YYYYY.@590ms | YYYYY.@586ms |
| `org.supertuxkart.stk` | YYY... | YYY... |
| `org.videolan.vlc` | YYY... | YYY... |
| `org.wikipedia` | YYY... | YYY... |

**Every changed app, explained (2 of 42; nothing moved down a rung):**

1. **`net.cozic.joplin`** — `first_frame`+`tree`: main `.` → branch `Y`
   (1650 ms). On `main` the launch dies with `JniException … RNCNetInfo` →
   `IConnectivityManager code 42`; on this branch the JS bundle evaluates, the
   framework callback path runs and the app publishes a frame. This is the wall
   the track exists to remove (`§1`).
2. **`com.nextcloud.client`** — `process_start`+`oncreate`: main `.` → branch
   `Y`. Not a false pass and not a regression: **both arms still exit 1 without a
   frame**, at different walls.
   * main: `Unable to instantiate application com.owncloud.android.MainApp …:
     java.lang.UnsupportedOperationException: unsupported Android API:
     android.net.IConnectivityManager code 42 (requestNetwork)` — the *same*
     code the Joplin wall used; nextcloud's `MainApp` registers a default network
     callback while it is being instantiated.
   * branch: `Unable to create application com.owncloud.android.MainApp:
     java.lang.NullPointerException: Attempt to invoke interface method
     'boolean android.database.Cursor.moveToFirst()' on a null object reference`
     — a SQLite/cursor wall of its own, reached only because the connectivity
     wall is gone. Next smallest action: track that `Cursor` wall (it is a
     `android.database` surface, not connectivity).

No app that reached a rung on `main` lost it on this branch, and no app's launch
time moved outside noise (the largest shift among green apps is
`net.mullvad.mullvadvpn` 1072→1126 ms, +5 %; the rest are within ±3 %). In
particular no app that now sees a non-null `getActiveNetwork` blocks on a socket:
the 26 apps that still stop at `onCreate` stop for the same non-connectivity
reasons as on `main`.

**Round 2** (`ROUND=2 BATCH=6 TIME=800 bash .cache/net1b/corpus.sh`) was started
at 10:05:01 UTC and reached batch `[7-12]` (6 of 42 apps, both arms) before the
window closed; it was then stopped so it would not sit in the shared ART queue
behind this report. Restart it with the same command — the driver overwrites the
per-app dirs, so a rerun is safe — and `python3 .cache/net1b/rung.py --rounds
1,2 --markdown` reproduces the table with both rounds. Round 1 already shows the
whole diff, so round 2 is the determinism check, not new information — and the
nextcloud delta is the one that most needs it (a marker race in a crashing guest,
not a rung change). The 6 r2 apps that did run (`chat.fluffy.fluffychat`,
`com.aurora.store`, `com.beemdevelopment.aegis`, `com.dot.gallery`,
`com.fsck.k9`, `com.github.gotify`) all reproduce their r1 rungs on both arms.

**Host cost of the corpus.** Max per-run guest+daemon peak RSS 734.6 MiB
(sampled by `k1-run.py` over the serve process tree), 97 runs, host RAM 14–15 %
of 60 GiB throughout, `/` 77 % → 81 % (`.cache/net1b` 937 MiB including the
`r2` partial).
