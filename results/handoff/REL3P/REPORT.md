# REL3P — ACOS parcelable writers vs the AOSP wire format (FINAL REPORT)

Track: **REL3P** (ACOS v0.3.0, "parcel String8 defect"). Branch `rel3p`,
branched from `7507f9d1`. Date 2026-11-16.

## 1. Status: DONE

The measured defect is fixed at the root (a missing field op + an oracle that
accepted the wrong one), the pm family is audited field by field, every change
has a test that fails before it, and the app-visible check (a real ART session
calling `getPackageInfo(own package, 0x100024c0)`) passes with no exception.

Two `android.net` codecs (`NetworkCapabilities`, `LinkProperties`) were found to
be wrong the same way and are **not** fixed here; they are reported as a gap with
evidence (§4.2), not as passing.

## 2. Commits

| sha | line |
|---|---|
| `ffa115bf` | REL3P: String8 arrays in the parcelable writers (getPackageInfo 0x100024c0 desync) |
| `99316ab4` | REL3P: parse the APK v2/v3 signers sequence, so GET_SIGNATURES has certs |
| `16a727ae` | REL3P: pms-parcel ART fixture, scenario, and the parcelable audit |

Branch HEAD after the final commit (`results/handoff/REL3P/REPORT.md`):
see `git log -1 --oneline`.

## 3. Acceptance criteria

| # | criterion | verdict | evidence |
|---|---|---|---|
| 1 | a real `String8Array` field op in the ACOS parcel writer (plus the other missing primitive: a nullable-element String16 array) | **PASS** | `crates/acos-parcel/src/parcel.rs` `write_string8_array`/`read_string8_array`/`write_string16_array_nullable`; 8 byte-level tests in `crates/acos-parcel/tests/string8_array.rs` |
| 2 | every parcelable ACOS writes audited against the image's `writeToParcel`, one row per parcelable | **PASS** | `results/handoff/REL3P/AUDIT.md` §3–§5; generated dump `results/handoff/REL3P/parcelable-audit.txt` (178 spec rows, 56 oracle-full) |
| 3 | every mismatch in the pm family fixed | **PASS** | 16 `writeString8Array` fields + `ProviderInfoList` + the `TypedArray` presence marker; `AUDIT.md` §3 names each one |
| 4 | a regression test per parcelable that round-trips through the **real** framework reader | **PASS** | `crates/acos-runtime/tests/parcelable_java.rs` (`ParcelableCompareMain.packageInfoArrays()`: real `writeToParcel` bytes == Rust bytes, real `PackageInfo.CREATOR.createFromParcel` on the Rust bytes, signatures/permissions/splitNames checked, byte-identical re-emission) |
| 5 | byte-layout tests derived from the AOSP field order, incl. the `0x100024c0` PackageInfo case | **PASS** | `crates/acos-services/tests/parcelable.rs::package_info_reply_reads_back_in_the_framework_order`, `::application_info_split_arrays_are_string8`, `::typed_array_elements_round_trip_with_their_presence_marker`, `::codec_field_orders_match_the_framework_dex` (strict) |
| 6 | each new test fails before the fix | **PASS** | measured: `codec_field_orders…` fails with `StringArray` for `writeString8Array`; `package_info_reply…` fails at `requestedPermissions`; `typed_array_elements…` fails with the marker skipped; `parcelable_java` fails with "PackageInfo real-Java bytes differ"; `fixture_apk::signed_fixture…` fails with `APK_SIGNING_MALFORMED: … truncated signatures`; the ART fixture failed with the `OutOfMemoryError` (SPIKE-WV) and then with `sigs=0` |
| 7 | crate tests for `acos-parcel` / `acos-services` via `tools/guarded.sh` | **PASS** | `cargo test -p acos-parcel -p acos-apk -p acos-services --no-fail-fast` → **444 passed, 0 failed** (under `run-exclusive.sh --task REL3P -- guarded.sh --mem 12G`) |
| 8 | an ART run of a fixture calling `getPackageInfo(own, GET_SIGNATURES\|GET_META_DATA\|GET_PERMISSIONS)` showing signature count / requestedPermissions on screen without exceptions | **PASS** | `results/handoff/REL3P/gates/rel3p-pms-parcel.json` (`ok:true status:pass engine:art`); tree text `report sigs=1 perms=2 splits=-1 meta=N pkg=dev.acos.fixtures.pmsparcel` in `results/handoff/REL3P/art/run/tree/`; logs `AcosPmsParcel: getPackageInfo flags=0x100024c0 sigs=1 perms=2 …` then `OK` |
| 9 | the gates still pass (counter / gaten launch gates) | **PASS** | `results/handoff/REL3P/gates/v2-art-counter.json` (`pass`, engine art, 11 steps) and `results/handoff/REL3P/gates/gaten-home.json` (`pass`, engine art, 14 steps) |

### 3.1 What changed, in one paragraph

`Parcel` gained the real `String8[]` encoding (`int32 count`, then `writeString8`
per element) and a nullable-element String16 variant. `FieldOp` gained
`String8Array`, and the three string-array ops now each match exactly one
`Parcel.write*` name, so the DEX oracle can no longer accept a String16 array
where the framework writes String8. Sixteen `android.content.pm` fields switched
to it (`PackageInfo.splitNames`/`requestedPermissions`, seven
`ApplicationInfo` arrays, `ComponentInfo.attributionTags` in `ComponentInfo`,
`ActivityInfo`, `ServiceInfo` and `ProviderInfo`, three
`InstrumentationInfo` arrays); `ProviderInfoList` was corrected to
`writeTypedList(ProviderInfo)`. Two adjacent encoder/decoder defects that the fix
exposed were fixed in the same class: the `TypedArray` **reader** never consumed
the per-element `writeTypedObject` presence marker the writer emits, and the APK
signing-block parser read the signers sequence length as a signer length, so
`GET_SIGNATURES` always returned an empty `Signature[]`.

## 4. What remains

### 4.1 Smallest next action

Nothing blocks REL3P. The orchestrator's merge order is free.

### 4.2 Known gaps (not this track's defect, must not be reported as fixed)

1. **`android.net.NetworkCapabilities` (5/18) and `android.net.LinkProperties`
   (2/17)** are declared `Verification::Full` but their field lists do not match
   the image (`AUDIT.md` §5, `parcelable-audit.txt` second section). A
   `getNetworkCapabilities()` / `getLinkProperties()` reply is therefore
   misaligned in the same way the PackageInfo reply was. Smallest next action:
   rewrite both specs from
   `.cache/boot/android-34/dex/framework-connectivity` (and the
   `packages/modules/Connectivity` sources), add the `Named`/`List` value
   plumbing in `connectivity.rs`, and let the audit example be the oracle.
2. **The DEX oracle only scans `dex/framework`.** That is why (1) hid. Smallest
   next action: make `Scanner`/`scripts` take the boot classpath roots
   (`dex/*`) so every spec is checked against the dex that actually contains its
   class. (The evidence file already runs both directories by hand.)
3. **`run-matrix.sh` cannot run at all on this tree** (unrelated to REL3P): it
   `mkdir`s the per-row artifacts directory and then `acos verify --artifacts
   <dir>` refuses an existing directory with `INVALID_ARGUMENT ("already holds
   an incomplete earlier run")` — both rows came back `verdict=error
   class=harness` (`results/handoff/REL3P/art/matrix-counter-gaten.json`). The
   gates were therefore run with the same command the matrix wraps
   (`acos verify <scenario> --engine art --json`), as FB1 did. Smallest next
   action: let `acos verify` accept an empty artifacts directory (or have
   `run-matrix.sh` not create it).
4. `SigningInfo` is only ever written as an absent conditional marker; a present
   `SigningInfo` needs `SigningDetails`' `writeArraySet` + two typed arrays
   (Curated gap, never non-null today).

## 5. Decisions for the orchestrator

1. **The APK signing-block fix (`99316ab4`) is a second, independent release
   defect**: `GET_SIGNATURES` and `WebViewFactory.verifyPackageInfo` need the
   APK's certificate, and every APK `tools/build-apps.sh` produces silently
   produced zero certificates. It is small, tested against `apksigner`'s own
   digest, and belongs in v0.3.0; merge it with the parcel fix or separately, but
   please note it in the release notes (a `PackageInfo.signatures` reply changes
   from `[]` to one 791-byte DER).
2. **Two `android.net` codecs are in the same defect class and are still wrong**
   (§4.2.1). If v0.3.0's "trustworthy replies" promise covers them, they need
   their own ticket; the connectivity module is v0.4.0 material in the plan, but
   `ConnectivityManager` replies are served today.
3. The oracle coverage gap (§4.2.2) should be closed before the next
   parcelable-heavy track, otherwise the next wrong field list hides the same
   way.

## 6. Host

* Peak RAM: guard ceilings 12 GiB (builds) / 10 GiB (ART runs); no single run
  above that. Guest peak RSS for the fixture session 15.3 MiB
  (`gates/rel3p-pms-parcel.json` `metrics.peak_rss_bytes`); `free -g` showed
  ≥ 50 GiB available before and after, never above 90 % used.
* Disk: `/` was at **86 % (133 GiB free)** when the work started (above the 85 %
  rule — flagged here as required); after the build it was 80 % (182 GiB free)
  because other tracks pruned meanwhile. My scratch: `.cache/rel3p-target`
  8.9 GiB, `.cache/rel3p` < 1 MiB, `results/handoff/REL3P` 112 KiB, `out/`
  (untracked verify artifacts) 0.4 MiB. Nothing was written outside this
  worktree except the shared `.cache/boot`, `.cache/art`, `.cache/gaten`
  symlinks the brief prescribes and the read-only AOSP cache.
* All ART runs went through `tools/art-runtime/run-exclusive.sh --task REL3P --
  tools/guarded.sh`; no process was left behind (each invocation exited with its
  guard).
