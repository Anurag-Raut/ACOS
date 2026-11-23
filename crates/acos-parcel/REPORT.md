# acos-parcel — implementation report

Status: T-01, T-02, T-03 implemented and tested. `#![forbid(unsafe_code)]`,
standalone of the VM, no dependency outside `rustix`/`thiserror` (`proptest` is
a dev-dependency).

## Public API (what the native layer maps onto)

### `Parcel` (`src/parcel.rs`)

Buffer/position:

| Method | Java native |
|---|---|
| `new()`, `with_capacity(n)` | `nativeCreate` |
| `data_position()` / `set_data_position(usize)` | `nativeDataPosition` / `nativeSetDataPosition` |
| `data_size()` / `set_data_size(usize)` | `nativeDataSize` |
| `data_avail()` | `nativeDataAvail` |
| `data_capacity()` | (pooling) |
| `marshall() -> Vec<u8>` / `unmarshall(&[u8])` | `nativeMarshall` / `nativeUnmarshall` |
| `append_from(&Parcel, offset, len)` | `nativeAppendFrom` |
| `recycle()` / `reset()` | `nativeFreeBuffer` / `Parcel.recycle` |
| `enforce_no_data_avail()` | `Parcel.enforceNoDataAvail` |

Scalars (each with a strict and a `*_lenient` twin):

`write_int32/uint32/int64/uint64/float/double/bool/byte/char` and
`read_*` / `read_*_lenient`.

Strings / arrays / blobs / markers:

`write_string8[_opt]`, `write_string16[_opt]`, `read_string8[_lenient]`,
`read_string16[_lenient]`, `write_byte_array`, `write_int32_array`,
`write_int64_array`, `write_float_array`, `write_double_array`,
`write_string16_array`, `write_blob`, `read_blob(len)`,
`write_parcelable_marker(is_non_null)`, `read_parcelable_marker()`.

Binders / fds:

`write_strong_binder(BinderId)`, `write_strong_binder_kind(BinderId, BinderKind)`,
`write_strong_binder_opt(Option<BinderId>)`, `read_strong_binder() -> Option<BinderId>`,
`write_file_descriptor(BorrowedFd)`, `write_dup_file_descriptor(BorrowedFd)`,
`read_file_descriptor() -> Option<RawFd>`, `has_file_descriptors()`,
`has_file_descriptors_in_range(offset, len)`, `push_allow_fds(bool) -> bool`,
`restore_allow_fds(bool)`, `allow_fds()`, `is_for_rpc() == false`,
`mark_for_binder(BinderId)`, `borrowed_file_descriptor(raw fd)`.

Interface token (`src/token.rs`):

`write_interface_token(desc)`, `write_interface_token_full(policy, work_source, desc)`,
`enforce_interface(desc)`, `peek_interface_token() -> Option<String>`,
`Parcel::thread_strict_mode_policy()` / `set_thread_strict_mode_policy`.

### `ParcelTable` (`src/parcel.rs`)

Slab of `Parcel`s addressed by a stable `u64` handle (index + generation):
`create()`, `create_with(Parcel)`, `free(handle)`, `with_parcel(handle, f)`,
`with_parcel_shared(handle, f)`, `contains(handle)`, `len()`, `is_empty()`.
Stale handles return `ParcelError::StaleHandle`; the table is `Mutex`-guarded
and safe to share across the VM dispatcher threads.

### `BinderRegistry` + identity (`src/registry.rs`)

`register_local/register_proxy(desc) -> BinderId`, `descriptor(id)`, `kind(id)`,
`contains`, `remove`, `is_binder_alive`, `ping_binder`, `add_service(name, id)`,
`get_service/check_service(name)`, `list_services()`, `link_to_death(id, recipient)`,
`unlink_to_death(id, recipient)`, `death_recipients(id)`.

Thread-local calls: `push_calling_identity(uid, pid)`, `pop_calling_identity()`,
`calling_identity/uid/pid()`, `clear_calling_identity() -> IdentityToken`,
`restore_calling_identity(token) -> Result<()>` (LIFO enforced),
`set_process_identity`, `with_cleared_identity`.

## Semantics — verified against the real Android 34 writers

The ground truth is the disassembly of **two** libbinder builds, plus the NDK's
`linux/android/binder.h`:

| build | path | build-id |
|---|---|---|
| **system** (what `/system/lib64/libandroid_runtime.so` links and what the emulator framework actually uses) | SDK API-34 google_apis x86_64 `/system/lib64/libbinder.so` | `404ed6308cc41e47e1246a91ff68e31e` |
| **VNDK/vendor** | legacy ACVM cache `~/.cache/acvm/w2/artapex/apex/com.android.vndk.current/lib64/libbinder.so` (outside this repo; read-only use) | `390786171b247b62d573f168e77a716a` |

The system library was extracted read-only with
`crates/acos-parcel/tools/extract-system-libbinder.sh` (same unprivileged
GPT → `dd` → `lpunpack` → `debugfs` method as `tools/fetch/extract-bootclasspath.sh`,
output under `.cache/parcel-evidence/`, multi-GB intermediates deleted on exit).

### System-build addresses (authoritative for ACOS)

| Function | Address | What it proves |
|---|---|---|
| `Parcel::writeString8(char const*, size_t)` | `0x81590` | null → `movl $0xffffffff`; else `int32 len` + `writeInplace(len+1)` + `memcpy` + `movw $0x0`. No `(len<<1)|1` header. |
| `Parcel::readString8Inplace` | `0x84260` | `cmp $0x7ffffffe` cap, `and $~3` padding. |
| `Parcel::writeString16(char16_t const*, size_t)` | `0x89df0` | null → `movl $0xffffffff`; else `int32 len` + `2*len` bytes + `0x0000`. |
| `Parcel::readString16Inplace` | `0x6f330` | `cmp $0x7ffffffe` / `cmp $0x3ffffffe` caps, `add $0x2` NUL, `and $~3`. |
| `Parcel::writeInterfaceToken` | `0x8adb0` | `0x8b684` `or $0x80000000` (policy), `0x8b70c` work source, `0x8b746` `movl $0x53595354`, then `String16`. |
| `Parcel::enforceInterface` | `0x82390` | `0x82759` `cmp $0x53595354,%r12d`. |

### VNDK-build addresses (explicit alternative)

| Function | Address | What it proves |
|---|---|---|
| `Parcel::writeInterfaceToken` | `0x792d0` | `0x79350` writes `0x564e4452` (`writeString8` `0x7fc90`, `readString8Inplace` `0x81550`, `writeString16` `0x79380`, `readString16Inplace` `0x79880` all show the same layouts). |
| `Parcel::enforceInterface` | `0x79680` | `0x79771` `cmp $0x564e4452`. |

The binder/fd/`appendFrom`/blob/parcelable/marker semantics were verified by
disassembling the VNDK build (addresses in the first table of the previous
revision and in the crate comments); the system build was independently verified
for the interface token and for the `String8`/`String16` layouts above. Both
builds are the same AOSP `Parcel.cpp` modulo the `__ANDROID_VNDK__` header.

**Interface-token header selection.** `Parcel.cpp` uses `#ifdef __ANDROID_VNDK__`
to choose between `B_PACK_CHARS('S','Y','S','T')` and
`B_PACK_CHARS('V','N','D','R')`. The system build writes/checks `0x53595354`
(`"SYST"`); the VNDK build writes/checks `0x564e4452` (`"VNDR"`). ACOS talks to
framework services, so the **default is the system value**; the VNDK value is
exported as [`crate::token::VNDK_INTERFACE_TOKEN_HEADER`] and can be selected per
parcel with `Parcel::set_interface_token_header`.

Type tags from the NDK header (`B_PACK_CHARS(c1,c2,c3,0x85)`):
`BINDER_TYPE_BINDER=0x73622a85`, `WEAK_BINDER=0x77622a85`,
`HANDLE=0x73682a85`, `WEAK_HANDLE=0x77682a85`, `FD=0x66642a85`.

### Corrections to the research note

* **The interface-token header is build-dependent.** The research note's
  `0x53595354` (`"SYST"`) is correct for the **system** build (the one ACOS
  must match), while the first implementation wrongly generalised the **VNDK**
  value `0x564e4452`. Both are now named constants; the system value is the
  default.
* **String8 has no compact `(len<<1)|1` header in Android 34.** It is a plain
  `int32 len` + bytes + NUL, exactly like String16 but with byte units; verified
  in both builds.
* **`writeString16` stores `len` (not `len+1`)** then writes `len` units plus a
  separate 2-byte NUL; the total padded span is `align4(2*len+2)`; verified in
  both builds.
* The strict-mode word is OR-ed with **`0x80000000`**
  (`STRICT_MODE_PENALTY_GATHER = 1 << 31`, `android/os/StrictMode.java:338`),
  not `0x40000000`; verified in both builds.

## Uncertainties / deliberate decisions

These are the points where the binary did not pin the behaviour and a documented
choice was made; they are flagged in code comments too.

1. **Padding bytes.** libbinder's `writeInplace` leaves the 1–3 pad bytes after
   a string/array/blob *uninitialised*. ACOS zeroes them. Readers skip padding,
   so this cannot change decoded values, and it makes encode/decode/re-encode
   byte-identical and reproducible. If a differential test ever compares raw
   buffers against a captured one, padding must be masked.
2. **`marshall()` is bytes-only**, per T-01. Android's C++ `marshall()` also
   serialises the object-offset table; ACOS binder ids are process-local, so the
   table is not serialised and `unmarshall` resets the object/fd tables. This is
   the one deviation from the C++ function and is intentional.
3. **Flat object union.** The 24-byte layout matches `flat_binder_object`
   (`u32 type`, `u32 flags`, `u64 id`, `u64 cookie`), but the union stores the
   ACOS `BinderId` (or the raw fd for `BINDER_TYPE_FD`) instead of a kernel
   `binder_uintptr_t`/`handle`. Offsets and alignment therefore match Android;
   the union payload does not need to.
4. **`write_string8` of a null Java `String`.** The Android 14 JNI path is not
   in the VNDK library, so the exact null path could not be disassembled. T-01
   and the AIDL contract both specify `int32 -1` for null; ACOS does that via
   `write_string8_opt(None)` and `write_string16_opt(None)`.
5. **String8 encoding of supplementary characters.** ACOS writes standard
   4-byte UTF-8 (Rust `str`). `String8::setTo(String16)` in libbinder does the
   same, but the JNI `nativeWriteString8` path (not available to disassemble
   here) could historically route through modified UTF-8. If a future
   differential test shows CESU-8 for non-BMP, only `write_string8` needs a
   conversion shim.
6. **`append_from` growth.** libbinder does `mDataSize += len` even when the
   write position is not at the end. ACOS copies at the current position and
   grows the data size to `max(old_size, pos + len)`, which is identical for the
   append-at-end usage the framework actually performs.
7. **`recycle`/`reset` close owned fds.** libbinder returns the buffer to a
   pool; ACOS clears it and drops the `OwnedFd` list, which closes the duped
   descriptors. Observable through `owned_file_descriptors().is_empty()`.
8. **`read_string8`/`read_string16` reject invalid UTF-8/unpaired surrogates.**
   libbinder's `String8` is arbitrary bytes and `String16` is arbitrary `u16`;
   ACOS returns `PARCEL_BAD_STRING` rather than panicking. The fuzz tests feed
   arbitrary bytes to confirm no panic.
9. **`push_allow_fds`/`restore_allow_fds` are a save/restore pair** (matching
   the C++ API), not a stack. The caller must keep the returned previous value.
10. **`mark_for_binder` is bookkeeping only**, matching the non-RPC no-op in the
    binary.

## Unsafe inventory

**None.** The crate is `#![forbid(unsafe_code)]` at the root. Descriptor
creation/duplication uses `rustix::fs::memfd_create` and `rustix::io::dup`,
both of which return `std::os::fd::OwnedFd` and close on drop, so no manual
`close(2)` or `from_raw_fd` is needed. `Parcel::borrowed_file_descriptor` is
safe because the borrow is tied to the parcel that owns the descriptor.

## Tests

`cargo test -p acos-parcel` runs 37 tests:

* `tests/golden.rs` (9): the three measured AIDL vectors
  (`IDisplayManager.getDisplayInfo`, `IActivityClientController.activityResumed`,
  `IWindowSession.relayout` prefix) encode → hard-coded bytes → decode →
  re-encode; the **system** interface-token prefix bytes and word order
  (`PENALTY_GATHER` / work source / `0x53595354` / `String16`), including an
  explicit VNDK-header round-trip; the `dataPosition` table; exact
  `String8`/`String16` bytes for ASCII/non-BMP/empty/null; `appendFrom` bytes
  and rebased object offsets; token mismatch and bad-header errors.
* `tests/roundtrip.rs` (12): binder null/local/proxy identity, fd content across
  `recycle`, `appendFrom` fd duplication, `allowFds`, all scalar/array/blob
  markers, ART-lenient readers, `ParcelTable` generations and a 4-thread table
  stress.
* `tests/properties.rs` (4 proptest cases): random op sequences write→read equal
  and re-encode deterministically; arbitrary bytes into every reader never
  panic; arbitrary Unicode String8/String16 round-trips.
* 12 unit tests inside the modules (error codes, flat-object tags/bytes, fd
  semantics, registry service table, death recipients, LIFO identity, two-thread
  thread-locality).

Benchmarks are in `BENCH.md` (`cargo bench -p acos-parcel`). The evidence used
for the token header is regenerable with
`crates/acos-parcel/tools/extract-system-libbinder.sh` and recorded in
`.cache/parcel-evidence/EVIDENCE.md` (gitignored).
