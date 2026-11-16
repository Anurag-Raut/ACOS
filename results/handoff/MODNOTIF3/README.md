# MODNOTIF3 — the `Notification` parcel codec (PARTIAL: item 1 of 4)

Branch `modnotif3` from `modnotif2` (`2240731b`). This commit lands **step 1** of the four
the brief ordered, because steps 2-4 depend on it: a `notify()` that cannot be *decoded* is
the fake progress MODNOTIF2 §4 refused to ship. Steps 2-4 are named below with their exact
next action; nothing was half-built and no assertion was weakened.

| Item | State |
|---|---|
| 1. `android.app.Notification`, `Notification$Action`, `Icon`, `PendingIntent` codecs + tests | **DONE** |
| 2. `getIntentSender` sender records + a dynamic `IIntentSender.send` node | NOT BUILT — see §3 |
| 3. `notifications` module (channels, notify/cancel, gating, control ops) | NOT BUILT — depends on 1+2 |
| 4. `notif-perm-probe` fixture + `services_control` scenario step + ART run | NOT BUILT |

## 1. What the codec is

`crates/acos-services/src/parcelable.rs`:

* `android.app.Notification` — the **whole** `writeToParcelImpl`
  (`Notification.java:3193-3360`) plus the trailing `allPendingIntents` count, in field order.
  `Verification::Full`: the scanner reports `63/63` real `Parcel.write*` calls consumed by the
  45 spec fields (`codec-verification.txt`). The five intent fields are
  `Cond(&[Nested("android.app.PendingIntent")])` because a `PendingIntent` writes only a
  strong binder, and `Cond(&[Nested("android.widget.RemoteViews")])` carries the four
  `RemoteViews` fields (a *present* `RemoteViews` is a structured `UnsupportedParcelable`, not
  a mis-read).
* `android.app.Notification$Action` — `Full`, `11/11` calls: optional `Icon`, title
  (`TextUtils`), optional `actionIntent`, extras bundle, typed `RemoteInput[]`, four flags.
* `android.app.PendingIntent` — `Full`, one strong binder (`PendingIntent.java:1500`).
* `android.graphics.drawable.Icon` — `Curated` because `Icon.writeToParcel` **switches on
  `mType`** (a linear field list cannot express it): `read_icon`/`write_icon` model the
  resource (`setSmallIcon(resId)` becomes this branch), URI and data branches; a bitmap icon
  is `UnsupportedParcelable("android.graphics.Bitmap")`.

Two general fixes were needed to make the field order real, both root-cause:

1. **The DEX scanner did not follow `writeToParcelImpl`.** AOSP's convention for a parcelable
   with marshaling hooks is `writeToParcel` → `writeToParcelImpl`; the scanner recorded only
   `writeArraySet` for `Notification` (1 call). `record_invoke` now inlines
   `writeToParcelImpl` on the same class, and maps `Parcel.writeArraySet` to its leading
   `writeInt` count (the same rule `writeTypedArray` already used).
2. **`read_field(TypedArray)` did not consume the per-element presence marker** that
   `write_field` has always written and that `Parcel.writeTypedArray` really writes
   (`writeTypedObject`: `writeInt(0|1)` + payload). Empty arrays hid it; the notification's
   non-empty `actions` exposed it. Both sides now agree (`Parcelable::Null` for a `0` marker).

Documented value gap (not an op gap): `allPendingIntents` — the `ArraySet<PendingIntent>` that
`Notification.writeToParcel` writes **last** — is written as the null encoding (`-1`). The
framework reader stores it and nothing on the system side dereferences it; the set is a
marshaling scratch list of the intents that were already written field-by-field above it.

## 2. Evidence

| File | Contents |
|---|---|
| `codec-verification.txt` | `codec_field_orders_match_the_framework_dex --nocapture`: `Notification Full 63/45 real=63`, `$Action Full 11/9 real=11`, `PendingIntent Full 1/1`, `Icon Curated real=4` |
| `codec-tests.txt` | the parcelable test binary's list |
| `full-suite.txt` | `cargo test -p acos-services`: **47/47 test binaries ok, 0 failed** (the shared `read_field` change does not disturb any existing codec) |

Round-trip coverage: `every_spec_round_trips_through_a_parcel` now has samples for
`android.app.Notification` (resource small icon, content PendingIntent binder, `android.title`
/ `android.text` extras, one action, `mChannelId`) and `Notification$Action`; `Icon` has its own
`icon_payload_round_trips`.

## 3. What is left, and the exact next action

1. **Real-framework differential (unfinished part of item 1).** `tests/parcelable_java.rs` has
   no `notification` case yet. The blocker is constructible: a `Notification` with
   `mSmallIcon`, `extras`, `actions` and `mChannelId` can be built by reflection in
   `ParcelableCompareMain` (public no-arg `Notification()`, `set()` helper already there);
   `contentIntent` must stay `null` unless a `PendingIntent` is manufactured with a
   `Proxy`-backed `IIntentSender` (the `PendingIntent(IIntentSender)` constructor is private).
   Do the no-`PendingIntent` case first — it still compares all 45 fields byte-for-byte — then
   the proxy case if the tap arm needs it.
2. **`getIntentSender` (item 2).** `crates/acos-aidl/src/generated/tables.rs:2071/2091` already
   carries codes 62/63; `acos-services` has no handler, and `acos-runtime`'s `ams_java` does not
   own them, so the request reaches the host. Next action: a
   `PendingIntentRecord { kind, request_code, flags, intents: Vec<IntentFacts>, extras, package }`
   list on `AmsState` + an `IActivityManager` handler that registers a
   `host.register_dynamic(...)` node per sender id and answers `binder(Some("android.content.IIntentSender"))`.
   The node's `send` handler is `transact(sender, send)` → the stored intents; `cancel`/flag
   semantics per `PendingIntent.java` (`FLAG_UPDATE_CURRENT` replaces the extras of an existing
   record, `FLAG_IMMUTABLE` refuses a fill-in).
3. **`notifications` module (item 3)** per MODNOTIF2 §4, now unblocked for decoding.
4. **Fixture + scenario + ART run (item 4).** Note for the orchestrator: the `tap` arm asserts
   "the target activity opens". On the ART path a system-initiated launch is a
   `ClientTransaction` pushed **from the host into the guest**; `acos-binder-peer` currently
   delivers exactly one such transaction per session (the initial launch,
   `peer.rs:86-95`, `take_launch_delivery`), pushed by the guest before the loop. A `tap`-driven
   second launch therefore needs the peer to *originate* a framework-marshalled
   `LaunchActivityItem` — the same system→app delivery class as the activity-result work the
   orchestrator moved to `mproc1`. Until then `tap` can only record the fired sender, and the
   fixture should assert that rather than a window that does not open (the honest-arm question
   is the orchestrator's call).

## 4. Peak resources

RAM peak observed: 13 GiB used / 60 GiB (host), disk `/` 82% before and after (target under
`.cache/modnotif3-target`, deleted at the end). No ART/gate run was needed for this commit.
