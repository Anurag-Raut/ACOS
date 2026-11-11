# W5-08 — `ArrayMap.append` RuntimeException during Bundle decode

**Status:** fixed (real codec ordering bug, not a benign log).
**Where:** `crates/acos-services/src/parcelable.rs` (`write_bundle`).
**Date:** 2026-11-11.

## Symptom

Launching `hello-views.apk` and reading `acos logs` showed six W-level records
from tag `ArrayMap`, each carrying a ~1.6–3.0 KB `java.lang.RuntimeException`
stack trace:

```
W ArrayMap: New hash -2059721656 is before end of array hash 111972721 at index 1
java.lang.RuntimeException: here
    at java.lang.Throwable.fillInStackTrace(Unknown Source:819)
    at android.util.ArrayMap.append(Unknown Source:653)
    at android.os.Parcel.readArrayMap(Unknown Source:5339)
    at android.os.BaseBundle.initializeFromParcelLocked(Unknown Source:476)
    at android.os.BaseBundle.unparcel(Unknown Source:320)
    at android.os.BaseBundle.getString(Unknown Source:1415)
    at android.provider.Settings$NameValueCache.getStringForUser(Unknown Source:3335)
    at android.provider.Settings$Global.getInt(Unknown Source:17036)
    at com.android.internal.policy.PhoneWindow.<init>(Unknown Source:361)
    ...
```

This is the **real AOSP framework code** running on the ACOS VM. It is not an
ACOS exception and it is not fatal, but it means the framework built an
`ArrayMap` whose `mHashes` are not sorted.

## Root cause

`android.os.Parcel.readArrayMap` (Android 14) reconstructs a bundle's
`ArrayMap` by calling `ArrayMap.append(key, value)` **in wire order**:

```
android.os.Parcel.readArrayMap -> android.util.ArrayMap.append
```

`ArrayMap.append` is only valid for entries whose `String.hashCode()` is
non-decreasing; it logs the `New hash ... is before end of array hash ...`
warning when the appended hash is smaller than the last one. Android's own
writer (`Parcel.writeArrayMapInternal` / `BaseBundle.writeToParcelInner`) emits
entries in the map's internal order, which is **ascending `hashCode`**.

ACOS's `write_bundle` emitted entries in **insertion order**
(`Bundle { entries: Vec<(String, Value)> }`). Lookups still worked in the common
case, but every settings/provider bundle whose insertion order differs from hash
order decoded into an unsorted `ArrayMap`, producing the warning (and, in
principle, unreliable binary-search lookups in the framework).

So: the parcel *bytes* were complete and readable; the **entry order** was
wrong vs. real Android.

## Fix

`write_bundle` now sorts the bundle entries by the key's Java `String.hashCode`
(ascending `i32`) before serializing, matching `ArrayMap`'s internal order.
`java_string_hash` mirrors `org.acos`/corelib `str_hash_code` and AOSP:
`h = h*31 + utf16_unit`.

The in-memory `Bundle` still preserves insertion order for app-facing lookups;
only the serialized form changes.

## Evidence

Same worktree/HEAD, `ACOS_NO_BOOT_CACHE=1`, one-shot
`acos logs --apk fixtures/build/apps/hello-views.apk --json`:

| | before fix | after fix |
|---|---:|---:|
| log records | 75 | 63 |
| `ArrayMap` warning records | **6** | **0** |
| `--tag ArrayMap` result | 6 records | `count: 0` |

The 12-record drop is exactly the six 1.6 KB warnings plus their six
`Throwable.fillInStackTrace` continuations that the runtime attributes to the
same record. No other records change.

## Tests

`crates/acos-services` round-trip tests are green; the bundle layout
(`[length][BNDL magic][size][entries]`) is unchanged, only the entry order.
