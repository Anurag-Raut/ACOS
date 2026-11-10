//! Regression test for results/audit/HONESTY_AUDIT.md finding #3.
//!
//! `Parcel::read_string16_array` must reject a count that cannot fit in the
//! remaining payload as `BadLength` *before* allocating the backing vector.

use acos_parcel::{Parcel, ParcelError};

#[test]
fn string_array_count_is_bounded_before_allocation() {
    let mut p = Parcel::new();
    // A count that cannot possibly fit in the remaining (empty) payload.
    p.write_int32(1_000_000);
    p.set_data_position(0);
    let err = p.read_string16_array().expect_err("must reject");
    assert!(
        matches!(err, ParcelError::BadLength { .. }),
        "expected BadLength before allocating the vector, got {err:?}"
    );
}
