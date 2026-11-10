//! The Android 14 `Parcel` exception header.
//!
//! Every AIDL reply starts with an `int`: `0` (`writeNoException`) or one of the
//! `Parcel.EX_*` constants written by `Parcel.writeException`. The constants are
//! the ones the framework's own `Parcel.readException` switch understands; they
//! are read out of `android.os.Parcel` static fields in the Android 14 boot DEX
//! (see `examples/scan_parcelables.rs` and the `parcel_exception_constants`
//! test in `parcelable::scan`). They are reproduced here so the dispatcher can
//! emit them without a VM.

use acos_parcel::{Parcel, ParcelError};

/// `Parcel.writeNoException()`.
pub const NO_EXCEPTION: i32 = 0;
/// `Parcel.EX_SECURITY`.
pub const EX_SECURITY: i32 = -1;
/// `Parcel.EX_BAD_PARCELABLE`.
pub const EX_BAD_PARCELABLE: i32 = -2;
/// `Parcel.EX_ILLEGAL_ARGUMENT`.
pub const EX_ILLEGAL_ARGUMENT: i32 = -3;
/// `Parcel.EX_NULL_POINTER`.
pub const EX_NULL_POINTER: i32 = -4;
/// `Parcel.EX_ILLEGAL_STATE`.
pub const EX_ILLEGAL_STATE: i32 = -5;
/// `Parcel.EX_NETWORK_MAIN_THREAD`.
pub const EX_NETWORK_MAIN_THREAD: i32 = -6;
/// `Parcel.EX_UNSUPPORTED_OPERATION`.
pub const EX_UNSUPPORTED_OPERATION: i32 = -7;
/// `Parcel.EX_SERVICE_SPECIFIC`.
pub const EX_SERVICE_SPECIFIC: i32 = -8;
/// `Parcel.EX_PARCELABLE`.
pub const EX_PARCELABLE: i32 = -9;

/// Every exception code the framework's `readException` understands.
pub const ALL_EXCEPTION_CODES: [i32; 10] = [
    NO_EXCEPTION,
    EX_SECURITY,
    EX_BAD_PARCELABLE,
    EX_ILLEGAL_ARGUMENT,
    EX_NULL_POINTER,
    EX_ILLEGAL_STATE,
    EX_NETWORK_MAIN_THREAD,
    EX_UNSUPPORTED_OPERATION,
    EX_SERVICE_SPECIFIC,
    EX_PARCELABLE,
];

/// The Java exception class name a code maps to (for evidence/logging).
#[must_use]
pub fn exception_class(code: i32) -> &'static str {
    match code {
        NO_EXCEPTION => "none",
        EX_SECURITY => "java.lang.SecurityException",
        EX_BAD_PARCELABLE => "android.os.BadParcelableException",
        EX_ILLEGAL_ARGUMENT => "java.lang.IllegalArgumentException",
        EX_NULL_POINTER => "java.lang.NullPointerException",
        EX_ILLEGAL_STATE => "java.lang.IllegalStateException",
        EX_NETWORK_MAIN_THREAD => "android.os.NetworkOnMainThreadException",
        EX_UNSUPPORTED_OPERATION => "java.lang.UnsupportedOperationException",
        EX_SERVICE_SPECIFIC => "android.os.ServiceSpecificException",
        EX_PARCELABLE => "android.os.ParcelableException",
        _ => "java.lang.RuntimeException",
    }
}

/// Write a successful AIDL reply header.
pub fn write_no_exception(reply: &mut Parcel) {
    reply.write_int32(NO_EXCEPTION);
}

/// Write an AIDL exception header.
///
/// For `EX_SERVICE_SPECIFIC` the framework's `Parcel.readException` reads a
/// `String16` message followed by an `int` error code (see
/// `Parcel.writeException`/`readException` asymmetry in AOSP; ACOS mirrors it
/// exactly), so those extra fields are emitted here.
pub fn write_exception(reply: &mut Parcel, code: i32, message: &str) -> Result<(), ParcelError> {
    reply.write_int32(code);
    if code == EX_SERVICE_SPECIFIC {
        reply.write_string16(message)?;
        reply.write_int32(0);
    }
    Ok(())
}

/// Read an AIDL reply header, returning the exception code.
pub fn read_exception(reply: &mut Parcel) -> Result<i32, ParcelError> {
    reply.read_int32()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable() {
        assert_eq!(ALL_EXCEPTION_CODES.len(), 10);
        assert_eq!(exception_class(EX_UNSUPPORTED_OPERATION), "java.lang.UnsupportedOperationException");
        assert_eq!(exception_class(NO_EXCEPTION), "none");
    }

    #[test]
    fn service_specific_round_trips_message() {
        let mut p = Parcel::new();
        write_exception(&mut p, EX_SERVICE_SPECIFIC, "nope").unwrap();
        p.set_data_position(0);
        assert_eq!(read_exception(&mut p).unwrap(), EX_SERVICE_SPECIFIC);
        assert_eq!(p.read_string16().unwrap().as_deref(), Some("nope"));
        assert_eq!(p.read_int32().unwrap(), 0);
    }

    #[test]
    fn every_framework_exception_code_round_trips() {
        for code in ALL_EXCEPTION_CODES {
            let mut p = Parcel::new();
            write_exception(&mut p, code, "msg").unwrap();
            p.set_data_position(0);
            assert_eq!(read_exception(&mut p).unwrap(), code);
            if code == EX_SERVICE_SPECIFIC {
                assert_eq!(p.read_string16().unwrap().as_deref(), Some("msg"));
                assert_eq!(p.read_int32().unwrap(), 0);
            }
        }
    }

    #[test]
    fn no_exception_writes_zero() {
        let mut p = Parcel::new();
        write_no_exception(&mut p);
        assert_eq!(p.marshall(), 0i32.to_le_bytes().to_vec());
    }
}
