# PIFILLIN — `Intent.fillIn`, the intent-carrying fire path, and the location `PendingIntent` form

Branch `pifillin`, created from `modules` @ `46831a9e` (MODLOC3), 2026-11-16.
Implementation only; the orchestrator reviews and merges. Private
`ACOS_RUNTIME_DIR` + `ACOS_SESSION_SOCKET` (`.cache/pifillin-run`,
`.cache/pifillin-runtime`), every ART run through `tools/art-runtime/run-exclusive.sh`
+ `tools/guarded.sh`, builds with `CARGO_PROFILE_*_DEBUG=0 … CARGO_INCREMENTAL=0`
into `.cache/pifillin-target`.

## 1. `Intent.fillIn` — one implementation, byte-verified against real Java

`crates/acos-services/src/fill_in.rs` is the single implementation of AOSP
`Intent.fillIn` (`base/core/java/android/content/Intent.java:11530-11615`) over
the real `android.content.Intent` value the codec reads and writes:

| field | rule (AOSP, exactly) |
|---|---|
| action | `other.action != null && (self.action == null \|\| FILL_IN_ACTION)` |
| data **and** type | one field: `(other.data != null \|\| other.type != null) && ((self.data == null && self.type == null) \|\| FILL_IN_DATA)`; both slots are assigned (a type-only fill-in clears `mData`) |
| identifier | `other != null && (self == null \|\| FILL_IN_IDENTIFIER)` |
| categories | `other != null && (self == null \|\| FILL_IN_CATEGORIES)` (a count-0 list is null, as `Intent.readFromParcel` reads it) |
| package | `other != null && (self == null \|\| FILL_IN_PACKAGE)` **and** `self.selector == null` |
| selector | only with `FILL_IN_SELECTOR`, and only when `self.package == null`; the package rule runs first, so a fill-in carrying both defines the package and the selector is then blocked |
| clipData | `other != null && (self == null \|\| FILL_IN_CLIP_DATA)` |
| component | only with `FILL_IN_COMPONENT` |
| flags | **always** `self.flags \|= other.flags` |
| sourceBounds | `other != null && (self == null \|\| FILL_IN_SOURCE_BOUNDS)` |
| extras | key by key: the fill-in's keys are added and the stored value wins (`newb = new Bundle(other.mExtras); newb.putAll(mExtras)`) |
| contentUserHint | only when a Uri may have been copied: `self == USER_CURRENT(-2) && other != USER_CURRENT` |

`mExtendedFlags` is the one part not modelled: this image's
`Intent.writeToParcel` does not write it and the codec does not carry it (the
`mFlags` merge is modelled).

`IntentFacts` now carries every field the merge touches (`type_`, `identifier`,
`flags`, `selector`, `source_bounds`, `clip_data` presence,
`content_user_hint`) and `IntentFacts::fill_in` runs the **same** function
through `to_parcelable`/`from_parcelable`, so the reduced path cannot drift from
the byte-verified one. `IntentFacts::default()` is a real `new Intent()`
(`contentUserHint = -2`), and `to_parcelable` writes a **null** extras bundle
when there are no extras (AOSP only creates `mExtras` on the first `putExtra`,
and that null/not-null difference is what gates the `contentUserHint` rule).

### The differential (`parcelable_java`)

`crates/acos-runtime/tests/java/ParcelableCompareMain.java` builds the stored
intent `a` and the fill-in `b`, calls the **real** `a.fillIn(b, flags)` and
writes `a` with the real `writeToParcel`. `parcelable_java.rs` builds the same
two intents, runs `fill_in_value` and compares:

* the bytes are identical, and the Rust merge reports the same `FILL_IN_*`
  change mask, for **8 cases**: `fillin_empty` (action+type+identifier+
  categories+package+extras), `fillin_component_kept`, `fillin_component_allowed`,
  `fillin_overwritten`, `fillin_extras` (stored key wins), `fillin_data_type`
  (type-only replace), `fillin_selector`, `fillin_flags_bounds`
  (flags merge + sourceBounds);
* the reverse direction feeds the Rust bytes to the real `Intent.CREATOR` and
  re-emits them (`read.fillin_*=ok`, `roundtrip.fillin_*` byte-identical).

A non-null `Uri` is deliberately not among the cases: `parcelable` refuses that
body (the dex does not pin `Uri.writeToParcel`), so the data/type rule is
reached through `type` alone. Evidence: `runtime-tests.log`
(`test real_java_write_to_parcel_matches_the_rust_codec ... ok`).

## 2. The intent-carrying fire path

`IIntentSender.send(code, intent, resolvedType, whitelistToken,
finishedReceiver, requiredPermission, options)` now really applies the fill-in.
The image's `PendingIntentRecord.sendInner` was disassembled first
(`pending-intent-dex.txt` §1):

```
finalIntent = new Intent(key.requestIntent)            // or new Intent()
if ((key.flags & 0x04000000 /*FLAG_IMMUTABLE*/) == 0) { // skip the merge when set
    if (intent != null) finalIntent.fillIn(intent, key.flags);
    if ((changes & FILL_IN_DATA) != 0) resolvedType = key.requestResolvedType;
}
```

* `pending_intent::FillIn { None, Intent(&IntentFacts), Ignored }`: the sender
  decides **before** the body is decoded. A mutable sender's fill-in must decode
  (an unreadable body — a non-null `Uri`, a `ClipData` — is the structured
  `SERVICE_UNSUPPORTED_PARCELABLE`, never a send without it); an immutable
  sender's is ignored, so a body this codec cannot read must not fail a send the
  framework performs.
* `AmsState::send_pending_intent_with_fill_in` merges into a **copy** (the
  record is never mutated), applies the `FLAG_UPDATE_CURRENT` extras first,
  delivers the merged intent through the sender type's path
  (activity/activity-result → `start_activity`, broadcast → `broadcast`,
  service/foreground → `start_service`) and records the fill-in, the
  `FILL_IN_*` changes and `fill_in_ignored` as evidence
  (`PendingIntentRecord.sends[]`).
* `FireSender = Fn(binder, code, Option<IntentFacts>)`: `notifications` `tap`/
  `action` pass `None`, `location` passes the fix.

### Two framework facts this path needed (both were missing)

1. **`ActivityManager.INTENT_SENDER_*` were off by one.** The old constants had
   `ACTIVITY=1, BROADCAST=2, SERVICE=3, FOREGROUND=4`; AOSP
   (`ActivityManager.java:603-632`) and the image's
   `PendingIntent.getBroadcastAsUser` (`pending-intent-dex.txt` §3:
   `const/4 v4, #1`) say **BROADCAST=1, ACTIVITY=2, ACTIVITY_RESULT=3,
   SERVICE=4, FOREGROUND_SERVICE=5**. With the old values every `getBroadcast`
   sender was replayed as an activity start and every `getActivity` sender as a
   broadcast — invisible in the notification scenario because the tap only
   asserted the fired record's `start_result`. Fixed, pinned by
   `pending_intent::tests::the_sender_types_are_the_frameworks`, and noted in
   `docs/AOSP_REFERENCE.md`.
2. **A fired broadcast reached no dynamic receiver.** The guest's
   `registerReceiver` is served by the runtime's AMS state (`ams_java`) while a
   **system**-originated broadcast is delivered from the host-side AMS state, so
   the host matcher saw no receiver at all. `registerReceiver`/
   `unregisterReceiver` now **mirror** the registration into the host state
   (`ServiceHost::mirror_dynamic_receiver`), which is what
   `AtmsState::inject_broadcast` always documented; the guest's own
   `broadcastIntent` keeps using the runtime state, so nothing is delivered
   twice. `ServiceHost::take_deliveries`/`take_timed_deliveries` also drain the
   AMS's queued `IIntentReceiver.performReceive` deliveries, one delivery path
   shared with `inject_broadcast`.

Two more replies the `PendingIntent` client path needed, both from the image
dex: `isIntentSenderTargetedToPackage` (the image's rule: false only when a
stored intent carries **both** a package and a component) and
`getInfoForIntentSender`'s `ActivityManager$PendingIntentInfo`
(`PendingIntentInfo.writeToParcel = String16, Int, Bool, Int` — the old reply had
three fields and no codec, so the client failed to decode it).

## 3. The location `PendingIntent` form (code 5)

The registration is no longer refused. `LocationService` keeps the caller's
`IIntentSender` (`Sink::PendingIntent`) and queues a `PendingFire` per delivery;
the fire runs **after** the module's state lock is dropped, through the session's
`FireSender`:

* registration → `providerEnabled` (`deliverOnProviderEnabledChanged`'s fill-in),
* every fix → `putExtra("location", Location)` under
  `LocationManager.KEY_LOCATION_CHANGED` (`fix_fill_in`),
* provider toggle / location-setting change → `providerEnabled` again,
* `unregisterLocationPendingIntent` drops the registration.

ACOS delivers one fix per update, never a `LocationResult` batch, so the
`"locations"` array form is not produced (the key is declared and the gap named
in the module docs). `service_ctl set_location` now reports
`pending_intent_fires`.

### The `Location` codec (both directions, one writer)

The fill-in's `Location` extra travels as a `Parcel.writeValue` parcelable, so
`parcelable::read_payload`/`write_payload` gained a `Location` arm backed by the
`location` module's codec. While adding it, the mask constants were corrected
against the image's `Location.writeToParcel` (`pending-intent-dex.txt` §4): the
old writer set `1<<4` as a "mock" bit, but bit 4 is `HAS_VERTICAL_ACCURACY_MASK`
(a float payload), so a scripted `mock: true` fix desynchronised every reader.
There is no mock bit on the wire; `Fix::mock` stays a module-side record. The
listener/callback payload and the fill-in extra now share one writer
(`write_location` → `write_location_value`), and the reader refuses an unknown
mask bit instead of guessing its width.

## 4. ART proof (real ART, `acos verify --engine art`)

`scenarios/location-pending-intent.yaml` — **pass, 18/18 steps**,
`v-location-pending-intent/`. The fixture (`fixtures/apps/location-probe`) now
registers a **mutable** `PendingIntent.getBroadcast` for
`dev.acos.fixtures.locationprobe.FIX` with a real `BroadcastReceiver`, which
renders the `Location` it read from the extra (`pi=<lat>,<lng> provider=gps
count=N`). The app's logcat is the narrative:

```
registered a PendingIntent for gps updates
pending intent providerEnabled=true                    <- registration (fill-in)
pending intent fix #1 37.4220,-122.0841 provider=gps   <- set_location fill-in
provider disabled gps / pending intent providerEnabled=false
provider enabled gps  / pending intent providerEnabled=true
pending intent fix #2 37.5000,-122.2000 provider=gps   <- second fix, count=2
```

The scenario asserts `pi=37.4220,-122.0841 provider=gps count=1` after the first
fix, `count=1` after the `providerEnabled` toggle (no Location repaints the
line), and `pi=37.5000,-122.2000 ... count=2` after the second fix, plus the
`service_ctl` `expect: {pending_intent_fires: 1}` on each fire. It also asserts
the listener form receives the same fixes, so both registrations are live.

## 5. Verification

| check | result | evidence |
|---|---|---|
| `cargo test -p acos-services` (private runtime dir) | **PASS** — 59 test binaries, 0 failed | `services-tests.log` (exit 0) |
| `cargo test -p acos-runtime` (private runtime dir) | **PASS** — 32 test binaries, 0 failed, incl. the 8-case fill-in differential | `runtime-tests.log` (exit 0) |
| `cargo check -p acos-services -p acos-runtime --all-targets` | **PASS** (pre-existing warnings only) | — |
| `scenarios/location-pending-intent.yaml` (new) | **pass 18/18** | `v-location-pending-intent/verdict.json` |
| `scenarios/notif-perm-granted.yaml` | **pass 10/10** | `v-notif-perm-granted/` |
| `scenarios/notif-perm-denied.yaml` | **pass 5/5** | `v-notif-perm-denied/` |
| `results/handoff/MODLOC3/location-live-attach.yaml` | **pass 18/18** | `v-location-live-attach/` |
| `results/handoff/MODLOC2/location-probe.yaml` | **pass 12/12** | `v-location-probe/` |
| `results/handoff/MODLOC2/location-geocoder.yaml` | **pass 3/3** | `v-location-geocoder/` |
| `results/handoff/MODLOC2/location-live-detach.yaml` | **pass 12/12** | `v-location-live-detach/` |

Runs: `tools/art-runtime/run-exclusive.sh --task PIFILLIN -- tools/guarded.sh
--mem 14G --time 900 -- env -u ACOS_HOME
.cache/pifillin-target/release/acos verify <yaml> --engine art --artifacts …
--json` with `ACOS_FONT_DIR` set and the private `ACOS_RUNTIME_DIR`. Artifact
dirs hold `verdict.json`, `run.json`, `engine.json`, `events.jsonl`,
`steps/*.json` and `logs/logcat.txt`.

## 6. What is NOT done / open items

* `mExtendedFlags` in the fill-in merge (not written by this image's
  `Intent.writeToParcel`, not carried by the codec).
* `Intent` values with a non-null `data` **Uri** or `ClipData` body: the codec
  refuses them (the image's `Uri.writeToParcel` body is not pinned), so a fill-in
  carrying one is a structured refusal on a mutable sender and ignored on an
  immutable one. `clipData` is modelled as presence only.
* A `LocationResult` **batch** (`"locations"`) is not produced by the scripted
  feed (one fix per update).
* `TYPE_ACTIVITY_RESULT` senders start their activity but do not deliver a
  result to `resultTo` (not modelled by `start_activity`; no consumer yet).
* The notification module's `tap` now really starts the target activity
  (`TYPE_ACTIVITY`); a second system-originated `LaunchActivityItem` is still
  the MPROC delivery class, unchanged.
