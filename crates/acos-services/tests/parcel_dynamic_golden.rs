//! Golden byte layouts for `parcelable::write_dynamic` / `read_dynamic`, one per
//! `Parcel.VAL_*` type the module writes (HYG1).
//!
//! The reference is the **API-34 framework** that runs in the guest:
//! android-14.0.0_r75 `base/core/java/android/os/Parcel.java` (sparse checkout in
//! `.cache/aosp`, see `docs/AOSP_REFERENCE.md`), and the shipped
//! `.cache/boot/android-34/jars/framework.jar` carries the same
//! `isLengthPrefixed` method. The rules that matter here:
//!
//! * `writeValue` writes the type tag, then — **only for the types
//!   `isLengthPrefixed` names** — a byte count that is the size of everything
//!   after it and is backpatched at the end (`Parcel.java:2458-2481`);
//!   `readValue` reads that count and compares it to what the body consumed
//!   (`Parcel.java:4529-4546`).
//! * `isLengthPrefixed` (`Parcel.java:4908-4926`) is: `VAL_MAP`,
//!   `VAL_PARCELABLE`, `VAL_LIST`, `VAL_SPARSEARRAY`, `VAL_PARCELABLEARRAY`,
//!   `VAL_OBJECTARRAY`, `VAL_SERIALIZABLE` — **nothing else**. In particular
//!   `VAL_STRING`, `VAL_INTEGER`, `VAL_LONG`, `VAL_FLOAT`, `VAL_DOUBLE`,
//!   `VAL_BOOLEAN`, `VAL_BYTE`, `VAL_BYTEARRAY`, `VAL_INTARRAY`,
//!   `VAL_STRINGARRAY`, `VAL_IBINDER` and `VAL_BUNDLE` have **no** length word
//!   (`VAL_BUNDLE` carries its own `BaseBundle` length instead).
//! * `VAL_MAP`'s body is `writeMap` (`Parcel.java:2586`): a size, then
//!   alternating key/value `writeValue` payloads.
//! * `VAL_PARCELABLE`'s body is the class name string16 followed by
//!   `writeToParcel` (`Parcel.java:2594-2595`, `writeParcelable` at 2688). The
//!   count covers the class name too, because it starts *after* the length word.
//!
//! A round-trip test alone cannot catch a wrong layout (both sides agree with
//! each other); these tests pin the bytes.

use acos_parcel::Parcel;
use acos_services::parcelable::{self, val};
use acos_services::value::{DynamicValue as D, Parcelable, Value};

/// The written payload as little-endian 32-bit words (every `writeValue` body
/// is 4-byte aligned, so a word view is the byte layout).
fn layout(value: &D) -> Vec<i32> {
    let mut p = Parcel::new();
    parcelable::write_dynamic(&mut p, value).expect("write_dynamic");
    p.set_data_position(0);
    let mut words = Vec::new();
    while p.data_avail() > 0 {
        words.push(p.read_int32().expect("word-aligned payload"));
    }
    words
}

/// The UTF-16 char words of `s` (no length word, no terminator).
fn utf16_words(s: &str) -> Vec<i32> {
    let units: Vec<u16> = s.encode_utf16().collect();
    units
        .chunks(2)
        .map(|c| i32::from(c[0]) | i32::from(*c.get(1).unwrap_or(&0)) << 16)
        .collect()
}

fn round_trip(value: &D) {
    let mut p = Parcel::new();
    parcelable::write_dynamic(&mut p, value).expect("write_dynamic");
    p.set_data_position(0);
    let back = parcelable::read_dynamic(&mut p).expect("read_dynamic");
    assert_eq!(
        p.data_avail(),
        0,
        "the decoder must consume the whole payload"
    );
    assert_eq!(format!("{back:?}"), format!("{value:?}"), "round trip");
}

// ---------------------------------------------------------------------------
// types with NO length word
// ---------------------------------------------------------------------------

#[test]
fn val_null_is_one_tag_word() {
    assert_eq!(layout(&D::Null), vec![val::NULL]);
}

#[test]
fn val_string_is_tag_then_string16() {
    assert_eq!(
        layout(&D::String(Some("ab".into()))),
        vec![val::STRING, 2, 0x0062_0061, 0]
    );
    // A null String is the `-1` string16 length, not a null value.
    assert_eq!(layout(&D::String(None)), vec![val::STRING, -1]);
}

#[test]
fn val_integer_is_tag_then_int32() {
    assert_eq!(layout(&D::Int(7)), vec![val::INTEGER, 7]);
}

#[test]
fn val_long_is_tag_then_little_endian_int64() {
    assert_eq!(
        layout(&D::Long(0x0102_0304_0506_0708)),
        vec![val::LONG, 0x0506_0708, 0x0102_0304]
    );
}

#[test]
fn val_float_and_double_are_tag_then_ieee_bits() {
    assert_eq!(layout(&D::Float(1.0)), vec![val::FLOAT, 0x3f80_0000]);
    assert_eq!(layout(&D::Double(1.0)), vec![val::DOUBLE, 0, 0x3ff0_0000]);
}

#[test]
fn val_boolean_and_byte_are_tag_then_int32() {
    // `writeValue` writes `writeInt(v ? 1 : 0)` and `writeInt((Byte) v)`.
    assert_eq!(layout(&D::Bool(true)), vec![val::BOOLEAN, 1]);
    assert_eq!(layout(&D::Bool(false)), vec![val::BOOLEAN, 0]);
    assert_eq!(layout(&D::Byte(-2)), vec![val::BYTE, -2]);
}

#[test]
fn val_bytearray_is_tag_then_length_and_padded_bytes() {
    assert_eq!(
        layout(&D::ByteArray(Some(vec![0xde, 0xad, 0xbe, 0xef]))),
        vec![val::BYTEARRAY, 4, 0xefbe_addeu32 as i32]
    );
    assert_eq!(layout(&D::ByteArray(None)), vec![val::BYTEARRAY, -1]);
}

#[test]
fn val_intarray_is_tag_then_count_and_words() {
    assert_eq!(
        layout(&D::IntArray(Some(vec![1, 2]))),
        vec![val::INTARRAY, 2, 1, 2]
    );
    assert_eq!(layout(&D::IntArray(None)), vec![val::INTARRAY, -1]);
}

#[test]
fn val_stringarray_is_tag_then_count_and_string16s() {
    assert_eq!(
        layout(&D::StringArray(Some(vec![
            Some("a".into()),
            Some("bb".into())
        ]))),
        vec![val::STRINGARRAY, 2, 1, 0x61, 2, 0x0062_0062, 0]
    );
    assert_eq!(layout(&D::StringArray(None)), vec![val::STRINGARRAY, -1]);
}

#[test]
fn val_ibinder_is_tag_then_the_flat_binder_object() {
    // A null strong binder is still a 24-byte `flat_binder_object`
    // (`BINDER_TYPE_BINDER`, `STRONG_BINDER_FLAGS`, id 0, cookie 0); the
    // stability record is a wire-form detail and is not written by default.
    assert_eq!(
        layout(&D::Binder(None)),
        vec![val::IBINDER, 0x7362_2a85, 0x17f, 0, 0, 0, 0]
    );
}

// ---------------------------------------------------------------------------
// types WITH a length word (isLengthPrefixed, Parcel.java:4908-4926)
// ---------------------------------------------------------------------------

#[test]
fn val_map_is_tag_length_size_then_pairs() {
    // Empty: the length covers the size word only.
    assert_eq!(layout(&D::Map(Vec::new())), vec![val::MAP, 4, 0]);
    // One `Int(7) -> String("ab")` entry: size (1 word) + key (2) + value (4)
    // = 7 words = 28 bytes, all under the backpatched length.
    assert_eq!(
        layout(&D::Map(vec![(D::Int(7), D::String(Some("ab".into())))])),
        vec![val::MAP, 28, 1, 1, 7, 0, 2, 0x0062_0061, 0]
    );
}

#[test]
fn val_parcelable_is_tag_length_then_class_name_and_payload() {
    // A null Parcelable still carries the length word and a null string16.
    assert_eq!(
        layout(&D::Parcelable(Parcelable::Null)),
        vec![val::PARCELABLE, 4, -1]
    );

    // `android.graphics.Point(3, 4)`: class string16 is 4 + 48 = 52 bytes
    // (22 units, NUL, padding), plus 8 bytes of fields = 60.
    let point = D::Parcelable(Parcelable::ordered(
        "android.graphics.Point",
        vec![Value::Int(3), Value::Int(4)],
    ));
    let mut want = vec![val::PARCELABLE, 60, 22];
    want.extend(utf16_words("android.graphics.Point"));
    want.push(0); // NUL + padding to four bytes
    want.extend([3, 4]);
    assert_eq!(want.len(), 17, "tag + length + 13 class words + 2 fields");
    assert_eq!(layout(&point), want);
    round_trip(&point);
}

// ---------------------------------------------------------------------------
// round trips (every type the module writes)
// ---------------------------------------------------------------------------

#[test]
fn every_written_type_round_trips_through_the_same_bytes() {
    for value in [
        D::Null,
        D::String(Some("hello".into())),
        D::String(None),
        D::Int(-9),
        D::Long(-1),
        D::Float(2.5),
        D::Double(-0.25),
        D::Bool(true),
        D::Byte(7),
        D::ByteArray(Some(vec![1, 2, 3, 4, 5])),
        D::IntArray(Some(vec![-1, 0, 1])),
        D::StringArray(Some(vec![Some("a".into()), Some("bb".into())])),
        D::Binder(None),
        D::Map(Vec::new()),
        D::Map(vec![
            (
                D::String(Some("k".into())),
                D::Map(vec![(D::Int(1), D::Bool(false))]),
            ),
            (D::Int(2), D::IntArray(Some(vec![3, 4]))),
        ]),
    ] {
        round_trip(&value);
        // The bytes the writer produced are exactly what the reader expects:
        // a second pass over the same layout must decode identically.
        let words = layout(&value);
        let mut p = Parcel::new();
        for word in &words {
            p.write_int32(*word);
        }
        p.set_data_position(0);
        parcelable::read_dynamic(&mut p).expect("the golden words decode");
        assert_eq!(p.data_avail(), 0);
    }
}

// ---------------------------------------------------------------------------
// teeth: the old (length-less) map layout and corrupt lengths are refused
// ---------------------------------------------------------------------------

#[test]
fn a_val_map_without_the_api34_length_word_is_refused() {
    // The pre-HYG1 layout: tag, size, pairs — the size word would be read as
    // the byte count (28) and the body then desynchronises.
    let mut p = Parcel::new();
    p.write_int32(val::MAP);
    p.write_int32(1);
    p.write_int32(val::INTEGER);
    p.write_int32(7);
    p.write_int32(val::STRING);
    p.write_string16("ab").unwrap();
    p.set_data_position(0);
    let err = parcelable::read_dynamic(&mut p).expect_err("the legacy layout must be refused");
    assert!(format!("{err}").contains("VAL_2"), "{err}");
}

#[test]
fn a_body_that_disagrees_with_its_length_word_is_refused() {
    for value in [
        D::Map(vec![(D::Int(1), D::Int(2))]),
        D::Parcelable(Parcelable::ordered(
            "android.graphics.Point",
            vec![Value::Int(3), Value::Int(4)],
        )),
    ] {
        let mut p = Parcel::new();
        parcelable::write_dynamic(&mut p, &value).expect("write_dynamic");
        // The length word is the second word of every length-prefixed body.
        p.set_data_position(4);
        let declared = p.read_int32().unwrap();
        p.set_data_position(4);
        p.write_int32(declared - 4);
        p.set_data_position(0);
        let err = parcelable::read_dynamic(&mut p).expect_err("a short length must be refused");
        assert!(format!("{err}").contains("declared"), "{err}");
    }
}

#[test]
fn a_length_beyond_the_parcel_is_refused_before_allocating() {
    let mut p = Parcel::new();
    p.write_int32(val::MAP);
    p.write_int32(i32::MAX);
    p.set_data_position(0);
    assert!(parcelable::read_dynamic(&mut p).is_err());

    // A negative count is malformed, not "no bytes".
    let mut p = Parcel::new();
    p.write_int32(val::PARCELABLE);
    p.write_int32(-4);
    p.set_data_position(0);
    assert!(parcelable::read_dynamic(&mut p).is_err());
}
