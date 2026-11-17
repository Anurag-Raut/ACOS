//! Golden vectors for real measured AIDL calls.
//!
//! The byte arrays below were hand-computed from the Android 14 **system**
//! `Parcel` layout (see `REPORT.md` for the disassembly evidence):
//!
//! * token prefix: `int32 0x80000000` (`PENALTY_GATHER`),
//!   `int32 -1` (work source), `int32 0x53595354` (**system** header `"SYST"`),
//!   `String16 descriptor`;
//! * `String16` = `int32 units` + UTF-16LE + `0x0000` + pad;
//! * a proxy binder is a 24-byte `BINDER_TYPE_HANDLE` flat object
//!   (`type=0x73682a85`, `flags=0x17f`, `id`, `cookie=0`).
//!
//! Each vector is `encode -> hard-coded bytes -> decode -> re-encode`.

use acos_parcel::binder::{BinderId, BinderKind};
use acos_parcel::parcel::Parcel;
use acos_parcel::{ParcelError, SYSTEM_INTERFACE_TOKEN_HEADER, VNDK_INTERFACE_TOKEN_HEADER};

/// `IDisplayManager.getDisplayInfo(int displayId)` with `displayId = 0`.
/// Measured at launch (`binder_calls_launch.tsv` order 3926).
#[rustfmt::skip]
const GET_DISPLAY_INFO: &[u8] = &[
    0x00, 0x00, 0x00, 0x80, 0xff, 0xff, 0xff, 0xff, 0x54, 0x53, 0x59, 0x53,
    0x28, 0x00, 0x00, 0x00, 0x61, 0x00, 0x6e, 0x00, 0x64, 0x00, 0x72, 0x00,
    0x6f, 0x00, 0x69, 0x00, 0x64, 0x00, 0x2e, 0x00, 0x68, 0x00, 0x61, 0x00,
    0x72, 0x00, 0x64, 0x00, 0x77, 0x00, 0x61, 0x00, 0x72, 0x00, 0x65, 0x00,
    0x2e, 0x00, 0x64, 0x00, 0x69, 0x00, 0x73, 0x00, 0x70, 0x00, 0x6c, 0x00,
    0x61, 0x00, 0x79, 0x00, 0x2e, 0x00, 0x49, 0x00, 0x44, 0x00, 0x69, 0x00,
    0x73, 0x00, 0x70, 0x00, 0x6c, 0x00, 0x61, 0x00, 0x79, 0x00, 0x4d, 0x00,
    0x61, 0x00, 0x6e, 0x00, 0x61, 0x00, 0x67, 0x00, 0x65, 0x00, 0x72, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// `IActivityClientController.activityResumed(IBinder token, boolean finished)`
/// with `token = 0x2a`, `finished = true`. Measured (order 105437).
#[rustfmt::skip]
const ACTIVITY_RESUMED: &[u8] = &[
    0x00, 0x00, 0x00, 0x80, 0xff, 0xff, 0xff, 0xff, 0x54, 0x53, 0x59, 0x53,
    0x25, 0x00, 0x00, 0x00, 0x61, 0x00, 0x6e, 0x00, 0x64, 0x00, 0x72, 0x00,
    0x6f, 0x00, 0x69, 0x00, 0x64, 0x00, 0x2e, 0x00, 0x61, 0x00, 0x70, 0x00,
    0x70, 0x00, 0x2e, 0x00, 0x49, 0x00, 0x41, 0x00, 0x63, 0x00, 0x74, 0x00,
    0x69, 0x00, 0x76, 0x00, 0x69, 0x00, 0x74, 0x00, 0x79, 0x00, 0x43, 0x00,
    0x6c, 0x00, 0x69, 0x00, 0x65, 0x00, 0x6e, 0x00, 0x74, 0x00, 0x43, 0x00,
    0x6f, 0x00, 0x6e, 0x00, 0x74, 0x00, 0x72, 0x00, 0x6f, 0x00, 0x6c, 0x00,
    0x6c, 0x00, 0x65, 0x00, 0x72, 0x00, 0x00, 0x00, 0x85, 0x2a, 0x68, 0x73,
    0x7f, 0x01, 0x00, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
];

/// `IWindowSession.relayout(...)` first arguments: interface token, the
/// `IWindow` strong binder, then the `LayoutParams` non-null marker. Measured
/// (order 120199).
#[rustfmt::skip]
const RELAYOUT_PREFIX: &[u8] = &[
    0x00, 0x00, 0x00, 0x80, 0xff, 0xff, 0xff, 0xff, 0x54, 0x53, 0x59, 0x53,
    0x1b, 0x00, 0x00, 0x00, 0x61, 0x00, 0x6e, 0x00, 0x64, 0x00, 0x72, 0x00,
    0x6f, 0x00, 0x69, 0x00, 0x64, 0x00, 0x2e, 0x00, 0x76, 0x00, 0x69, 0x00,
    0x65, 0x00, 0x77, 0x00, 0x2e, 0x00, 0x49, 0x00, 0x57, 0x00, 0x69, 0x00,
    0x6e, 0x00, 0x64, 0x00, 0x6f, 0x00, 0x77, 0x00, 0x53, 0x00, 0x65, 0x00,
    0x73, 0x00, 0x73, 0x00, 0x69, 0x00, 0x6f, 0x00, 0x6e, 0x00, 0x00, 0x00,
    0x85, 0x2a, 0x68, 0x73, 0x7f, 0x01, 0x00, 0x00, 0x2a, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00,
];

#[test]
fn golden_get_display_info_encode_decode_reencode() {
    let desc = "android.hardware.display.IDisplayManager";

    // encode
    let mut p = Parcel::new();
    p.write_interface_token(desc).unwrap();
    p.write_int32(0);
    assert_eq!(p.marshall(), GET_DISPLAY_INFO);

    // decode
    let mut d = Parcel::new();
    d.unmarshall(GET_DISPLAY_INFO);
    d.enforce_interface(desc).unwrap();
    assert_eq!(d.read_int32().unwrap(), 0);
    d.enforce_no_data_avail().unwrap();

    // re-encode from decoded values is byte-identical
    let mut r = Parcel::new();
    r.write_interface_token(desc).unwrap();
    r.write_int32(0);
    assert_eq!(r.marshall(), GET_DISPLAY_INFO);
}

#[test]
fn golden_activity_resumed_encode_decode_reencode() {
    let desc = "android.app.IActivityClientController";
    let token = BinderId(0x2a);

    let mut p = Parcel::new();
    p.write_interface_token(desc).unwrap();
    p.write_strong_binder_kind(token, BinderKind::Proxy);
    p.write_bool(true);
    assert_eq!(p.marshall(), ACTIVITY_RESUMED);

    let mut d = Parcel::new();
    d.unmarshall(ACTIVITY_RESUMED);
    d.enforce_interface(desc).unwrap();
    assert_eq!(d.read_strong_binder().unwrap(), Some(token));
    assert!(d.read_bool().unwrap());
    d.enforce_no_data_avail().unwrap();

    let mut r = Parcel::new();
    r.write_interface_token(desc).unwrap();
    r.write_strong_binder_kind(token, BinderKind::Proxy);
    r.write_bool(true);
    assert_eq!(r.marshall(), ACTIVITY_RESUMED);
}

#[test]
fn golden_relayout_prefix_encode_decode_reencode() {
    let desc = "android.view.IWindowSession";
    let window = BinderId(0x2a);

    let mut p = Parcel::new();
    p.write_interface_token(desc).unwrap();
    p.write_strong_binder_kind(window, BinderKind::Proxy);
    p.write_parcelable_marker(true); // LayoutParams != null
    assert_eq!(p.marshall(), RELAYOUT_PREFIX);

    let mut d = Parcel::new();
    d.unmarshall(RELAYOUT_PREFIX);
    d.enforce_interface(desc).unwrap();
    assert_eq!(d.read_strong_binder().unwrap(), Some(window));
    assert!(d.read_parcelable_marker().unwrap());
    d.enforce_no_data_avail().unwrap();

    let mut r = Parcel::new();
    r.write_interface_token(desc).unwrap();
    r.write_strong_binder_kind(window, BinderKind::Proxy);
    r.write_parcelable_marker(true);
    assert_eq!(r.marshall(), RELAYOUT_PREFIX);
}

#[test]
fn interface_token_prefix_bytes_and_word_order() {
    // The default header is the SYSTEM libbinder value the framework links to.
    let mut p = Parcel::new();
    assert_eq!(p.interface_token_header(), SYSTEM_INTERFACE_TOKEN_HEADER);
    p.write_interface_token_full(0, -1, "android.app.IActivityManager")
        .unwrap();
    let b = p.marshall();
    // Word 1: strict-mode policy | PENALTY_GATHER (1 << 31).
    assert_eq!(&b[0..4], &0x8000_0000u32.to_le_bytes());
    // Word 2: work source (-1 when not propagating).
    assert_eq!(&b[4..8], &(-1i32).to_le_bytes());
    // Word 3: the magic, and its on-wire bytes.
    assert_eq!(&b[8..12], &SYSTEM_INTERFACE_TOKEN_HEADER.to_le_bytes());
    assert_eq!(&b[8..12], &[0x54, 0x53, 0x59, 0x53]); // "SYST"
                                                      // Then String16(descriptor).
    p.set_data_position(12);
    assert_eq!(
        p.read_string16().unwrap().as_deref(),
        Some("android.app.IActivityManager")
    );
    p.enforce_no_data_avail().unwrap();

    // The VNDK value is an explicit, named alternative.
    let mut v = Parcel::new();
    assert_eq!(VNDK_INTERFACE_TOKEN_HEADER, 0x564e_4452);
    v.set_interface_token_header(VNDK_INTERFACE_TOKEN_HEADER);
    v.write_interface_token_full(0, -1, "android.app.IActivityManager")
        .unwrap();
    let vb = v.marshall();
    assert_eq!(&vb[8..12], &VNDK_INTERFACE_TOKEN_HEADER.to_le_bytes());
    assert_eq!(&vb[8..12], &[0x52, 0x44, 0x4e, 0x56]); // "RDNV" on the wire
                                                       // enforce accepts the VNDK header only while the parcel expects VNDK.
    v.set_data_position(0);
    v.enforce_interface("android.app.IActivityManager").unwrap();

    let mut sys = Parcel::new();
    sys.unmarshall(&vb);
    sys.set_data_position(0);
    let err = sys
        .enforce_interface("android.app.IActivityManager")
        .unwrap_err();
    assert!(matches!(
        err,
        ParcelError::BadInterfaceTokenHeader {
            found: VNDK_INTERFACE_TOKEN_HEADER,
            expected: SYSTEM_INTERFACE_TOKEN_HEADER,
        }
    ));
}

#[test]
fn data_position_after_every_op_matches_table() {
    let desc = "android.hardware.display.IDisplayManager";
    let mut p = Parcel::new();
    assert_eq!(p.data_position(), 0);
    p.write_interface_token(desc).unwrap();
    // 12-byte prefix + 4 + 40*2 + 2 (NUL) padded to 88.
    assert_eq!(p.data_position(), 100);
    assert_eq!(p.data_size(), 100);
    p.write_int32(0);
    assert_eq!(p.data_position(), 104);
    assert_eq!(p.data_size(), 104);
    assert_eq!(p.data_avail(), 0);

    p.set_data_position(0);
    assert_eq!(p.data_avail(), 104);
    p.set_data_position(100);
    assert_eq!(p.data_avail(), 4);
}

fn enc_string8(s: Option<&str>) -> Vec<u8> {
    let mut p = Parcel::new();
    p.write_string8_opt(s).unwrap();
    p.marshall()
}

fn enc_string16(s: Option<&str>) -> Vec<u8> {
    let mut p = Parcel::new();
    p.write_string16_opt(s).unwrap();
    p.marshall()
}

#[test]
fn string8_exact_bytes_ascii_nonbmp_empty_null() {
    assert_eq!(enc_string8(Some("A")), [1, 0, 0, 0, b'A', 0, 0, 0]);
    // Empty writes len 0 plus the NUL, padded to 8 bytes total.
    assert_eq!(enc_string8(Some("")), [0, 0, 0, 0, 0, 0, 0, 0]);
    // Non-BMP is standard 4-byte UTF-8 (U+1F600 = F0 9F 98 80).
    assert_eq!(
        enc_string8(Some("\u{1F600}")),
        [4, 0, 0, 0, 0xf0, 0x9f, 0x98, 0x80, 0, 0, 0, 0]
    );
    assert_eq!(enc_string8(None), [0xff, 0xff, 0xff, 0xff]);

    // Decode round-trips all four.
    for value in [Some("A"), Some(""), Some("\u{1F600}"), None] {
        let mut p = Parcel::new();
        p.unmarshall(&enc_string8(value));
        assert_eq!(p.read_string8().unwrap().as_deref(), value);
    }
}

#[test]
fn string16_exact_bytes_ascii_nonbmp_empty_null() {
    assert_eq!(enc_string16(Some("A")), [1, 0, 0, 0, 0x41, 0, 0, 0]);
    assert_eq!(enc_string16(Some("")), [0, 0, 0, 0, 0, 0, 0, 0]);
    // U+1F600 is the surrogate pair D83D DE00.
    assert_eq!(
        enc_string16(Some("\u{1F600}")),
        [2, 0, 0, 0, 0x3d, 0xd8, 0x00, 0xde, 0, 0, 0, 0]
    );
    assert_eq!(enc_string16(None), [0xff, 0xff, 0xff, 0xff]);

    for value in [Some("A"), Some(""), Some("\u{1F600}"), None] {
        let mut p = Parcel::new();
        p.unmarshall(&enc_string16(value));
        assert_eq!(p.read_string16().unwrap().as_deref(), value);
    }
}

#[test]
fn append_from_bytes_and_object_offsets() {
    let mut a = Parcel::new();
    a.write_int32(0x11);
    a.write_string16("hi").unwrap();
    a.write_strong_binder_kind(BinderId(7), BinderKind::Proxy);
    a.write_int32(0x22);
    // int32(4) + String16("hi") = 4 + (4 + 4 + 2 -> 12) = 16, so the binder is
    // at offset 16.
    assert_eq!(a.objects().to_vec(), vec![16]);

    let mut b = Parcel::new();
    b.write_int32(0xaa);
    b.write_int32(0xbb);
    b.append_from(&a, 0, a.data_size()).unwrap();

    let mut expected = vec![0xaa, 0, 0, 0, 0xbb, 0, 0, 0];
    expected.extend_from_slice(&a.marshall());
    assert_eq!(b.marshall(), expected);
    // Object offset rebased from 16 to 8 + 16 = 24.
    assert_eq!(b.objects().to_vec(), vec![24]);

    // A sub-range that does not include the binder copies no objects.
    let mut c = Parcel::new();
    c.append_from(&a, 0, 16).unwrap();
    assert!(c.objects().is_empty());
    assert_eq!(c.marshall(), a.marshall()[..16].to_vec());

    // A sub-range that is exactly the binder copies and preserves its id.
    let mut d = Parcel::new();
    d.append_from(&a, 16, 24).unwrap();
    assert_eq!(d.objects().to_vec(), vec![0]);
    d.set_data_position(0);
    assert_eq!(d.read_strong_binder().unwrap(), Some(BinderId(7)));
}

#[test]
fn enforce_interface_token_mismatch_and_bad_header() {
    let mut p = Parcel::new();
    p.write_interface_token("android.app.IActivityManager")
        .unwrap();
    p.set_data_position(0);
    let err = p
        .enforce_interface("android.view.IWindowManager")
        .unwrap_err();
    assert_eq!(err.code(), "PARCEL_TOKEN_MISMATCH");
    assert_eq!(err.status(), -1);

    // A VNDK header is a distinct, structured error for an ACOS (system) parcel.
    let mut bad = Parcel::new();
    bad.write_int32(0x8000_0000u32 as i32);
    bad.write_int32(-1);
    bad.write_int32(VNDK_INTERFACE_TOKEN_HEADER as i32);
    bad.write_string16("android.app.IActivityManager").unwrap();
    bad.set_data_position(0);
    let err = bad
        .enforce_interface("android.app.IActivityManager")
        .unwrap_err();
    assert_eq!(err.code(), "PARCEL_BAD_TOKEN_HEADER");
    assert!(matches!(
        err,
        ParcelError::BadInterfaceTokenHeader {
            found: VNDK_INTERFACE_TOKEN_HEADER,
            expected: SYSTEM_INTERFACE_TOKEN_HEADER,
        }
    ));
}
