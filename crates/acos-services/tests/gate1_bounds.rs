//! Sandbox certification §5 gate 1 (W44 re-audit): guest-controlled counts and
//! sizes must be bounded by the bytes actually left in the parcel before any
//! allocation. Each case sends a hostile `i32::MAX`-class length in a tiny
//! parcel and must get a structured error quickly, not an allocation abort.

use acos_parcel::Parcel;
use acos_services::cursor::CursorWindowView;
use acos_services::parcelable;

#[test]
fn a_hostile_val_map_count_is_refused_without_allocating() {
    let mut p = Parcel::new();
    p.write_int32(parcelable::val::MAP);
    // API 34: the `VAL_MAP` body is length-prefixed (`Parcel.isLengthPrefixed`,
    // android-14.0.0_r75 `base/core/java/android/os/Parcel.java:4908-4926`), so
    // the word after the tag is the byte count, then `writeMap`'s size.
    p.write_int32(4); // declared body: the size word only
    p.write_int32(i32::MAX); // entry count, nothing follows
    p.set_data_position(0);
    // Before the fix this reserved i32::MAX * sizeof((DynamicValue, DynamicValue))
    // bytes and aborted the host; now the reserve is capped by the parcel and
    // the first missing entry is a structured truncation error.
    assert!(parcelable::read_dynamic(&mut p).is_err());
}

#[test]
fn a_val_map_length_beyond_the_parcel_is_refused() {
    let mut p = Parcel::new();
    p.write_int32(parcelable::val::MAP);
    p.write_int32(i32::MAX); // declared body size, nothing follows
    p.set_data_position(0);
    let err = parcelable::read_dynamic(&mut p).expect_err("must refuse");
    assert!(format!("{err}").contains("exceeds"), "{err}");
}

#[test]
fn a_val_map_that_fits_still_decodes() {
    let mut p = Parcel::new();
    p.write_int32(parcelable::val::MAP);
    // Size (4) + two `VAL_INTEGER` entries (4 + 4 bytes each) = 20 bytes.
    p.write_int32(20);
    p.write_int32(1);
    p.write_int32(parcelable::val::INTEGER);
    p.write_int32(7);
    p.write_int32(parcelable::val::INTEGER);
    p.write_int32(9);
    p.set_data_position(0);
    let v = parcelable::read_dynamic(&mut p).expect("a one-entry map decodes");
    assert!(format!("{v:?}").contains('7'));
    assert_eq!(p.data_avail(), 0, "the length word and the body agree");
}

#[test]
fn a_cursor_window_larger_than_the_parcel_is_refused() {
    let mut p = Parcel::new();
    p.write_int32(0); // mStartPos
    p.write_string8("hostile").unwrap();
    p.write_uint32(0); // rows
    p.write_uint32(0); // columns
    p.write_uint32(u32::MAX); // window size: far beyond the parcel
    p.write_int32(0); // inline, not ashmem
    p.set_data_position(0);
    let err = CursorWindowView::read_from(&mut p).expect_err("must refuse");
    assert!(format!("{err}").contains("exceeds"), "{err}");
}
