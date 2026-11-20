//! `String8[]` vs `String16[]` on the wire.
//!
//! AOSP has two Java methods that write a `String[]`:
//!
//! * `Parcel.writeStringArray` (`Parcel.java:1867`) → `writeString16Array`:
//!   every element is a `String16` (length in UTF-16 **units**, 2 bytes each).
//! * `Parcel.writeString8Array` (`Parcel.java:1881`):
//!   `writeInt(N)` then `writeString8` per element (length in UTF-8 **bytes**,
//!   1 byte per ASCII character).
//!
//! The `android.content.pm` parcelables use the **String8** form
//! (`PackageInfo.java`: `splitNames`, `requestedPermissions`;
//! `ApplicationInfo.java`: every `split*`/`resourceDirs`/`sharedLibraryFiles`
//! array; `ComponentInfo.java`: `attributionTags`). Encoding those with
//! `writeString16Array` appends two bytes of string data per character the
//! framework reader does not expect, so it reads the following fields from the
//! middle of the string payload — the release defect this file pins down:
//! `getPackageInfo(pkg, 0x100024c0)` replied with `signatures.length =
//! 7 209 071` and died with `OutOfMemoryError: Failed to allocate a 7676737936
//! byte allocation at Parcel.createString8Array`.

use acos_parcel::Parcel;

/// The exact bytes of `writeString8Array(new String[]{"ab", null, "ü"})`.
///
/// ```text
/// 03 00 00 00              count = 3
/// 02 00 00 00 61 62 00 00  "ab"   (len in bytes, NUL, pad to 4)
/// ff ff ff ff              null element
/// 02 00 00 00 c3 bc 00 00  "ü"    (2 UTF-8 bytes, not 1 UTF-16 unit)
/// ```
#[test]
fn string8_array_bytes_match_parcel_writeString8Array() {
    let mut p = Parcel::new();
    p.write_string8_array(Some(&[Some("ab"), None, Some("ü")]))
        .expect("write String8[]");
    assert_eq!(
        p.marshall(),
        vec![
            3, 0, 0, 0, // count
            2, 0, 0, 0, b'a', b'b', 0, 0, // "ab"
            0xff, 0xff, 0xff, 0xff, // null
            2, 0, 0, 0, 0xc3, 0xbc, 0, 0, // "ü"
        ]
    );
}

/// The same array in `writeStringArray` (String16) form, for the contrast: the
/// payload is twice as wide and the length word counts code units.
#[test]
fn string16_array_bytes_match_parcel_writeStringArray() {
    let mut p = Parcel::new();
    p.write_string16_array_nullable(Some(&[Some("ab"), None, Some("ü")]))
        .expect("write String16[]");
    assert_eq!(
        p.marshall(),
        vec![
            3, 0, 0, 0, // count
            2, 0, 0, 0, b'a', 0, b'b', 0, 0, 0, 0, 0, // "ab"
            0xff, 0xff, 0xff, 0xff, // null
            1, 0, 0, 0, 0xfc, 0x00, 0, 0, // "ü" is one UTF-16 unit
        ]
    );
}

/// The two encodings must not be interchangeable: same input, different bytes.
#[test]
fn the_two_array_encodings_differ() {
    let items: [Option<&str>; 2] = [Some("requested.permission"), Some("x")];
    let mut eight = Parcel::new();
    eight.write_string8_array(Some(&items)).expect("String8[]");
    let mut sixteen = Parcel::new();
    sixteen
        .write_string16_array_nullable(Some(&items))
        .expect("String16[]");
    assert_ne!(eight.marshall(), sixteen.marshall());
    assert!(
        sixteen.marshall().len() > eight.marshall().len(),
        "String16[] is wider per character of string data"
    );
}

#[test]
fn string8_array_round_trips() {
    let items: [Option<&str>; 4] = [Some(""), None, Some("ab"), Some("ü☃")];
    let mut p = Parcel::new();
    p.write_string8_array(Some(&items)).expect("write");
    p.set_data_position(0);
    let got = p.read_string8_array().expect("read");
    assert_eq!(
        got,
        Some(vec![
            Some(String::new()),
            None,
            Some("ab".to_owned()),
            Some("ü☃".to_owned())
        ])
    );
    p.enforce_no_data_avail().expect("no trailing bytes");
}

#[test]
fn string8_array_null_array_is_minus_one() {
    let mut p = Parcel::new();
    p.write_string8_array(None).expect("write null array");
    assert_eq!(p.marshall(), vec![0xff, 0xff, 0xff, 0xff]);
    p.set_data_position(0);
    assert_eq!(p.read_string8_array().expect("read"), None);
}

/// An empty array is `0`, not `-1` (the framework distinguishes them).
#[test]
fn string8_array_empty_array_is_zero() {
    let mut p = Parcel::new();
    p.write_string8_array(Some(&[])).expect("write empty array");
    assert_eq!(p.marshall(), vec![0, 0, 0, 0]);
    p.set_data_position(0);
    assert_eq!(p.read_string8_array().expect("read"), Some(Vec::new()));
}

/// A hostile count must be rejected before the backing vector is allocated,
/// the same guard `read_string16_array` has.
#[test]
fn string8_array_count_is_bounded_before_allocation() {
    let mut p = Parcel::new();
    p.write_int32(i32::MAX);
    p.set_data_position(0);
    let err = p.read_string8_array().expect_err("must reject");
    assert!(
        matches!(err, acos_parcel::ParcelError::BadLength { .. }),
        "expected BadLength, got {err:?}"
    );
}

/// A non-ASCII element must be written as UTF-8 bytes, not UTF-16, and its
/// length word is the byte count (`writeString8` semantics).
#[test]
fn string8_array_element_length_is_utf8_bytes() {
    let mut p = Parcel::new();
    p.write_string8_array(Some(&[Some("☃")])).expect("write");
    // count 1, then int32 3 (three UTF-8 bytes) + 3 bytes + NUL (4 bytes,
    // already 4-byte aligned).
    assert_eq!(
        p.marshall(),
        vec![1, 0, 0, 0, 3, 0, 0, 0, 0xe2, 0x98, 0x83, 0]
    );
}
