# REL3P2 — parcelable oracle coverage and the `android.net` codecs

Status: **DONE**. The parcelable oracle now covers the image's whole boot
classpath and inlines the writer's private helpers; both `android.net` codecs it
flagged are rewritten from the image's own `writeToParcel`, the values ACOS
serves today are proven byte-for-byte against the real framework
(`crates/acos-runtime/tests/parcelable_java.rs`), an ART fixture reads both
replies through the framework's own readers, and `tools/run-matrix.sh` produces
real verdicts again. Branch `rel3p2`, branched from `rel3p` (`e1c54990`).

This document supersedes §0 and §5 of `results/handoff/REL3P/AUDIT.md`.

## 0. The oracle, and the two holes that let this class hide

`crates/acos-services/src/parcelable.rs` describes every parcelable ACOS writes
as a `ParcelableSpec`: the `FieldOp` list is `writeToParcel`'s field order. The
test oracle is the image's own bytecode: `Scanner` linearises the
`Parcel.write*` calls of `Landroid/…;->writeToParcel`.

REL3P fixed two holes (strict string-array ops, distinct `String8Array`). REL3P2
closes the two that were left:

| hole | consequence | fix |
|---|---|---|
| the scan took **one** directory (`dex/framework`) | every spec whose class lives in another boot jar (`android.net.*` in `framework-connectivity`, the apex jars) was skipped as "class not found" and stayed `Verification::Full` without ever being checked | `Scanner::write_to_parcel_boot` loads **every** boot-classpath DEX directory of the image: the union of `BOOTCLASSPATH`, `SYSTEMSERVERCLASSPATH` and the other jars the image ships under `dex/` (`boot_classpath_dex_dirs`, 83 dirs / 89 dex files). The test scans that set, not a directory. |
| the scan could not see **private helpers** | `NetworkCapabilities.writeParcelableArraySet` and `LinkProperties.writeAddresses`/`writeAddress` wrote fields the oracle never saw; `LinkProperties` "verified" 17 calls when the writer emits 32, and the specs modelled the helper bytes as unrelated ops | `record_invoke` now inlines any method on the boot classpath whose name starts with `write` and whose first parameter is `Landroid/os/Parcel;`. Recursion is bounded by a **stack**, not a "seen" set, because one helper is legitimately called several times in one writer (`writeAddresses` ×3, `writeAddress` ×4). |

Two field ops were added for the shapes helpers emit that no existing op
described, and one for `writeList`:

| op | wire form | first user |
|---|---|---|
| `FieldOp::Counted(&[FieldOp])` | `writeInt` count, then one field sequence per element; the collapsed loop body matches when present, the count alone when the list is empty/`null` | `NetworkCapabilities.mUids` (count + `writeParcelable`), `LinkProperties.mLinkAddresses`/`mDnses`/`mValidatedPrivateDnses`/`mPcscfs`/`mRoutes` |
| `FieldOp::Address` | `writeByteArray`, then the writer's IPv6 arm (`writeBoolean` + `writeInt`) which the static scan sees even though it is dead for an IPv4/`null` address | `LinkProperties.mDhcpServerAddress` |
| `FieldOp::ValueList` | `writeList`: `writeInt` count (`-1` null) then `writeValue` per element | `LinkProperties.mStackedLinks` |

## 1. `android.net.NetworkCapabilities` — `Verification::Full`, oracle 20/20

Spec table = the image's `writeToParcel` in `dex/framework-connectivity`
(`classes.dex`), call for call.

| # | AOSP field | image call(s) | ACOS op | match |
|---|---|---|---|---|
| 1 | `mNetworkCapabilities & ALL_VALID_CAPABILITIES` | writeLong | Long | yes |
| 2 | `mForbiddenNetworkCapabilities` | writeLong | Long | yes |
| 3 | `mTransportTypes & ALL_VALID_TRANSPORTS` | writeLong | Long | yes |
| 4 | `mLinkUpBandwidthKbps` | writeInt | Int | yes |
| 5 | `mLinkDownBandwidthKbps` | writeInt | Int | yes |
| 6 | `mNetworkSpecifier` | writeParcelable | **Named** (was `Str16`) | **fixed** |
| 7 | `mTransportInfo` | writeParcelable | **Named** (was `Str16`) | **fixed** |
| 8 | `mSignalStrength` | writeInt | Int | yes |
| 9 | `mUids` (ArraySet) | writeInt + writeParcelable | **Counted([Named(UidRange)])** (was `Int`) | **fixed** |
| 10 | `mAllowedUids` | writeIntArray | IntArray | yes |
| 11 | `mSSID` | writeString | Str16 | yes |
| 12 | `mPrivateDnsBroken` | writeBoolean | Bool | yes |
| 13 | administrator uids | writeIntArray | IntArray | yes |
| 14 | `mOwnerUid` | writeInt | Int | yes |
| 15 | `mRequestorUid` | writeInt | Int | yes |
| 16 | `mRequestorPackageName` | writeString | Str16 | yes |
| 17 | `mSubIds` | writeIntArray | IntArray | yes |
| 18 | `mUnderlyingNetworks` | writeTypedList | **TypedList(Network)** (was `Int`) | **fixed** |
| 19 | `mEnterpriseId & ALL_VALID_ENTERPRISE_IDS` | writeInt | Int | yes |

Before REL3P2 the extended oracle reports `matched 5/18, full=false`
(`results/handoff/REL3P/parcelable-audit.txt`, second section); after, 20/20.

## 2. `android.net.LinkProperties` — `Verification::Full`, oracle 32/32

| # | AOSP field | image call(s) | ACOS op | match |
|---|---|---|---|---|
| 1 | interface name | writeString | Str16 | yes |
| 2 | `mLinkAddresses` | writeInt + writeParcelable | **Counted([Named(LinkAddress)])** (was `Int`) | **fixed** |
| 3 | `mDnses` (`writeAddresses`) | writeInt + writeByteArray + writeBoolean + writeInt | **Counted([ByteArray, Bool, Int])** (was `Int`) | **fixed** |
| 4 | `mValidatedPrivateDnses` | same | **same** (was `Int`) | **fixed** |
| 5 | `mUsePrivateDns` | writeBoolean | Bool | yes |
| 6 | `mPrivateDnsServerName` | writeString | Str16 | yes |
| 7 | `mPcscfs` | same as 3 | **Counted([ByteArray, Bool, Int])** (was `Int`) | **fixed** |
| 8 | `mDomains` | writeString | Str16 | yes |
| 9 | `mDhcpServerAddress` (`writeAddress`) | writeByteArray (+ dead IPv6 arm) | **Address** (was `ByteArray`) | **fixed** |
| 10 | `mMtu` | writeInt | Int | yes |
| 11 | `mTcpBufferSizes` | writeString | Str16 | yes |
| 12 | `mRoutes` | writeInt + writeParcelable | **Counted([Named(RouteInfo)])** (was `Int`) | **fixed** |
| 13 | `mHttpProxy` | writeByte + writeParcelable | **ByteCond([Named(ProxyInfo)])** (was `Byte`) | **fixed** |
| 14 | `mNat64Prefix` | writeParcelable | **Named(IpPrefix)** (was `Str16`) | **fixed** |
| 15 | `mStackedLinks` | writeList | **ValueList** (was `Int`) | **fixed** |
| 16 | `mWakeOnLanSupported` | writeBoolean | Bool | yes |
| 17 | `mCaptivePortalApiUrl` | writeParcelable | **Named** (was `Str16`) | **fixed** |
| 18 | `mCaptivePortalData` | writeParcelable | **Named** (was `Str16`) | **fixed** |

Before REL3P2: `matched 2/17, full=false`; after: 32/32 (the 17-call figure was
the helper-blind scan).

### 2.1 Value plumbing (`crates/acos-services/src/connectivity.rs`)

The values are the same offline values as before (wifi transport, not validated,
`wlan0`/MTU 1500, no addresses, routes, proxy or NAT64). Only their encoding
changed to the ops above, so the byte stream is unchanged and a future non-null
value (real-network spike) encodes in the framework's order instead of
desynchronising:

* `mNetworkSpecifier`/`mTransportInfo`, NAT64, captive portal:
  `Value::NamedParcelable(Parcelable::Null)` (the `writeParcelable(null)` form,
  byte-identical to the old bare null string).
* `mUids`, underlying networks, every address/route list: `Value::List(None)`
  for `null` / `Value::List(Some(vec![]))` for the empty list; `writeTypedList`
  and `writeList` null is `-1`.
* `mHttpProxy`: `Value::Cond { present: false }` (the `writeByte(0)` marker).
* `mDhcpServerAddress`: `Value::ByteArray(None)`.

## 3. Every other spec against the whole boot classpath

Generated table: `results/handoff/REL3P2/parcelable-audit-table.txt`
(89 spec rows, one per spec). Full dump with the per-class ACOS/IMAGE call
lists: `results/handoff/REL3P2/parcelable-audit-boot.txt`. Reproduce:

```
cargo run -p acos-services --example parcelable_audit      # whole boot classpath
cargo run -p acos-services --example parcelable_audit -- <dir> [dir ...]
```

| verification | rows | result | justified? |
|---|---|---|---|
| `Full` | 53 | **all `match`** (every field and every real call consumed) | yes |
| `Prefix` | 6 | all have a real prefix: `DisplayInfo` 24/52, `InputDevice` 21/28, `AudioAttributes` 5/10 (documented image-specific gaps), `InstallSourceInfo` 6/6, `NotificationChannel` 32/32, `NotificationChannelGroup` 10/10 (curated conditional encodings) | yes — the remaining calls are documented gaps pinned by their own tests |
| `Curated` | 30 | no `Full` claim; small ACOS-owned values or dynamically encoded ones (`Uri`, `Bundle`, `PersistableBundle`, `ContentValues`, `ClientTransaction` members, …) | yes — `Curated` is not a pass claim |
| no Java writer | 7 | `KeyCharacterMap`, `Bundle`, `IBinder`, `PersistableBundle`, `DisplayCutout`, `ApplicationErrorReport$ParcelableCrashInfo` (native/other writer), `AccessibilityManager$ClientInfo` (absent from every shipped dex) | yes — nothing on the classpath to verify against |

No `Full` row fails and no row is unaccounted. Classes that the old
single-directory oracle never saw are now checked: e.g. `android.net.*`,
`android.app.NotificationChannel` (`framework.jar` has them all, but the test
also covers the apex/system jars the image ships).

## 4. Evidence that the replies are framework-exact

| check | evidence |
|---|---|
| real Java `writeToParcel` bytes == ACOS bytes for both production builders, and the real `CREATOR.createFromParcel` reads ACOS's bytes with `enforceNoDataAvail` + re-emits them identically | `crates/acos-runtime/tests/parcelable_java.rs` → `results/handoff/REL3P2/parcelable-java.log` |
| an app on the real framework reads both replies without an exception and sees the offline values | ART fixture `fixtures/apps/pms-parcel` (`getActiveNetwork` → `getNetworkCapabilities`/`getLinkProperties`), scenario `scenarios/rel3p-pms-parcel.yaml` → `results/handoff/REL3P2/rel3p2-pms-parcel.json`, tree `results/handoff/REL3P2/art/rel3p2-pms-parcel/tree/2.json` |
| the encoded value decodes back to the same value through its own spec (writing the right bytes but reading the wrong shape fails) | `crates/acos-services/tests/net1_connectivity.rs::net_capabilities_and_link_properties_round_trip_through_their_specs` |
| the image's call sequences are pinned (20 and 32 calls), and a `dex/framework`-only scan must still fail to find the class | `crates/acos-services/tests/net1_connectivity.rs::the_connectivity_specs_match_the_boot_images_write_to_parcel` |
| the boot-wide oracle is what the test runs | `crates/acos-services/tests/parcelable.rs::codec_field_orders_match_the_framework_dex` (asserts > 10 dirs, prints `verified 82 specs against 83 boot DEX dirs (53 full)`) |

## 5. Harness: `tools/run-matrix.sh` produced `class=harness` for every row

Root cause: the driver created the per-row artifacts directory and wrote its own
`stdout.json`/`stderr.txt` into it **before** launching
`acos verify --artifacts <that dir>`. `claim_run_dir` (v0.2.1 rule: one run owns
one artifacts directory) then refused the non-empty directory with
`INVALID_ARGUMENT … incomplete earlier run`, so every row was
`verdict=error class=harness`.

Fix (does not weaken the rule): the harness does not create the run directory at
all. Its logs are staged in `$ARTIFACTS/.driver/` and moved into the run
directory **after** verify has finished. When verify fails before writing
anything, the logs still land beside the row.

Regression test: `tools/tests/test-run-matrix-ownership.py` extracts the driver
the script actually embeds and runs it against a stub `acos` that enforces the
same ownership rule (non-empty `--artifacts` → `INVALID_ARGUMENT`). Before the
fix: both rows `verdict=error, class=harness`; after: both rows `pass` and
`stdout.json` beside the evidence.

Real run (`tools/run-matrix.sh --scenario scenarios/v2-art-counter.yaml
--scenario scenarios/negative/counter-wrong-count.yaml --engine art --profile
release --no-build`, release binaries built from this HEAD):

```
| run                            | variant  | expected | verdict | false_pass | error class |
| 01-v2-art-counter-r1           | good     | pass     | pass    | False      | -           |
| 02-negative-counter-wrong-count-r1 | negative | fail  | fail    | False      | assertion   |
summary: errors 0, false_pass 0, unclassified 0, harness_or_timeout 0
```

Evidence: `results/handoff/REL3P2/matrix/matrix.json` / `.tsv` / `run.log`, the
per-row envelopes in `matrix/rows/`; each row's verify directory was created by
`acos verify` itself (its `run.json`/`verdict.json` are there) and the
harness's `stdout.json`/`stderr.txt` were moved in after the run.
