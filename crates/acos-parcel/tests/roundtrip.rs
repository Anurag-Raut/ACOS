//! Round-trip, descriptor, and `ParcelTable` tests.

use std::os::fd::RawFd;
use std::sync::Arc;

use acos_parcel::binder::{BinderId, BinderKind};
use acos_parcel::fd::ParcelFd;
use acos_parcel::parcel::{Parcel, ParcelTable};

fn read_fd_content(parcel: &Parcel, raw: RawFd) -> Vec<u8> {
    let fd = parcel.borrowed_file_descriptor(raw).unwrap();
    let mut buf = vec![0u8; 128];
    let n = rustix::io::pread(fd, &mut buf, 0).unwrap();
    buf.truncate(n);
    buf
}

#[test]
fn binder_round_trip_identity() {
    let mut p = Parcel::new();
    p.write_strong_binder(BinderId(0xdead_beef));
    p.write_strong_binder_kind(BinderId(1), BinderKind::Local);
    p.write_strong_binder_opt(None);

    assert_eq!(p.objects().to_vec(), vec![0, 24, 48]);

    p.set_data_position(0);
    assert_eq!(p.read_strong_binder().unwrap(), Some(BinderId(0xdead_beef)));
    assert_eq!(p.read_strong_binder().unwrap(), Some(BinderId(1)));
    assert_eq!(p.read_strong_binder().unwrap(), None);
    p.enforce_no_data_avail().unwrap();
}

#[test]
fn binder_null_then_binder_then_null_is_stable() {
    for id in [BinderId(7), BinderId::NULL, BinderId(7)] {
        let mut p = Parcel::new();
        p.write_strong_binder(id);
        p.set_data_position(0);
        let got = p.read_strong_binder().unwrap();
        assert_eq!(got, if id.is_null() { None } else { Some(id) });
    }
}

#[test]
fn reading_a_non_binder_object_is_an_error() {
    let memfd = ParcelFd::memfd("not-a-binder").unwrap();
    let mut p = Parcel::new();
    p.write_file_descriptor(memfd.as_fd()).unwrap();
    p.set_data_position(0);
    let err = p.read_strong_binder().unwrap_err();
    assert_eq!(err.code(), "PARCEL_NOT_A_BINDER");
}

#[test]
fn fd_round_trip_preserves_content_across_recycle() {
    let memfd = ParcelFd::memfd("parcel-fd").unwrap();
    rustix::io::pwrite(memfd.as_fd(), b"payload-bytes", 0).unwrap();

    let mut p = Parcel::new();
    p.write_file_descriptor(memfd.as_fd()).unwrap();
    assert!(p.has_file_descriptors());
    assert!(p.has_file_descriptors_in_range(0, p.data_size()));
    assert!(!p.has_file_descriptors_in_range(24, 24));

    p.set_data_position(0);
    let raw = p.read_file_descriptor().unwrap().unwrap();
    assert_eq!(read_fd_content(&p, raw), b"payload-bytes");

    // The original descriptor is untouched (the parcel owns a dup).
    let mut buf = [0u8; 32];
    let n = rustix::io::pread(memfd.as_fd(), &mut buf, 0).unwrap();
    assert_eq!(&buf[..n], b"payload-bytes");

    p.recycle();
    assert!(p.owned_file_descriptors().is_empty());
    assert!(!p.has_file_descriptors());
}

#[test]
fn append_from_duplicates_file_descriptors() {
    let memfd = ParcelFd::memfd("append-fd").unwrap();
    rustix::io::pwrite(memfd.as_fd(), b"abc", 0).unwrap();

    let mut a = Parcel::new();
    a.write_file_descriptor(memfd.as_fd()).unwrap();

    let mut b = Parcel::new();
    b.append_from(&a, 0, a.data_size()).unwrap();
    assert_eq!(b.objects().to_vec(), vec![0]);
    assert_eq!(b.owned_file_descriptors().len(), 1);
    assert_eq!(a.owned_file_descriptors().len(), 1);

    let raw_a = a.owned_file_descriptors()[0];
    let raw_b = b.owned_file_descriptors()[0];
    assert_ne!(raw_a, raw_b, "appendFrom must dup the descriptor");
    assert_eq!(read_fd_content(&b, raw_b), b"abc");
}

#[test]
fn allow_fds_gates_writes() {
    let memfd = ParcelFd::memfd("gated").unwrap();
    let mut p = Parcel::new();
    assert!(p.allow_fds());
    let old = p.push_allow_fds(false);
    assert!(old);
    assert!(!p.allow_fds());
    let err = p.write_file_descriptor(memfd.as_fd()).unwrap_err();
    assert_eq!(err.code(), "PARCEL_FDS_NOT_ALLOWED");
    p.restore_allow_fds(true);
    assert!(p.write_file_descriptor(memfd.as_fd()).is_ok());
    assert!(!p.is_for_rpc());
}

#[test]
fn scalar_and_array_round_trips() {
    let mut p = Parcel::new();
    p.write_int32(-7);
    p.write_uint32(0xFFFF_FFFF);
    p.write_int64(i64::MIN);
    p.write_float(1.5);
    p.write_double(-2.25);
    p.write_bool(true);
    p.write_byte(-3);
    p.write_char(0x4E2D);
    p.write_string8("utf-8 ☃").unwrap();
    p.write_string8_opt(None).unwrap();
    p.write_string16("utf-16 ☃").unwrap();
    p.write_byte_array(Some(&[1, 2, 3])).unwrap();
    p.write_byte_array(None).unwrap();
    p.write_int32_array(Some(&[1, -2, 3])).unwrap();
    p.write_int64_array(Some(&[1, -2])).unwrap();
    p.write_float_array(Some(&[1.0, 2.0])).unwrap();
    p.write_double_array(Some(&[3.0])).unwrap();
    p.write_string16_array(Some(&["a", "b"])).unwrap();
    p.write_blob(b"blobby").unwrap();
    p.write_parcelable_marker(true);

    p.set_data_position(0);
    assert_eq!(p.read_int32().unwrap(), -7);
    assert_eq!(p.read_uint32().unwrap(), 0xFFFF_FFFF);
    assert_eq!(p.read_int64().unwrap(), i64::MIN);
    assert_eq!(p.read_float().unwrap(), 1.5);
    assert_eq!(p.read_double().unwrap(), -2.25);
    assert!(p.read_bool().unwrap());
    assert_eq!(p.read_byte().unwrap(), -3);
    assert_eq!(p.read_char().unwrap(), 0x4E2D);
    assert_eq!(p.read_string8().unwrap().as_deref(), Some("utf-8 ☃"));
    assert_eq!(p.read_string8().unwrap(), None);
    assert_eq!(p.read_string16().unwrap().as_deref(), Some("utf-16 ☃"));
    assert_eq!(p.read_byte_array().unwrap(), Some(vec![1, 2, 3]));
    assert_eq!(p.read_byte_array().unwrap(), None);
    assert_eq!(p.read_int32_array().unwrap(), Some(vec![1, -2, 3]));
    assert_eq!(p.read_int64_array().unwrap(), Some(vec![1, -2]));
    assert_eq!(p.read_float_array().unwrap(), Some(vec![1.0, 2.0]));
    assert_eq!(p.read_double_array().unwrap(), Some(vec![3.0]));
    assert_eq!(
        p.read_string16_array().unwrap(),
        Some(vec![Some("a".to_owned()), Some("b".to_owned())])
    );
    assert_eq!(p.read_blob(6).unwrap(), Some(b"blobby".to_vec()));
    assert!(p.read_parcelable_marker().unwrap());
    p.enforce_no_data_avail().unwrap();
}

#[test]
fn lenient_readers_match_art_zero_values() {
    let mut p = Parcel::new();
    p.write_int32(5);
    p.set_data_position(0);
    assert_eq!(p.read_int32_lenient(), 5);
    // Short buffer: strict errors, lenient returns the ART zero value.
    assert_eq!(p.read_int32_lenient(), 0);
    assert_eq!(p.read_float_lenient(), 0.0);
    assert_eq!(p.read_double_lenient(), 0.0);
    assert_eq!(p.read_int64_lenient(), 0);
    assert!(!p.read_bool_lenient());

    let mut short = Parcel::new();
    short.unmarshall(&[0xff, 0xff, 0xff]);
    assert!(short.read_int32().is_err());
    assert_eq!(short.read_int32_lenient(), 0);
}

#[test]
fn marshal_unmarshal_and_mark_for_binder() {
    let mut p = Parcel::new();
    p.write_int32(0x1234);
    p.mark_for_binder(BinderId(9));
    p.mark_for_binder(BinderId::NULL);
    let bytes = p.marshall();
    assert_eq!(bytes, vec![0x34, 0x12, 0, 0]);
    assert_eq!(p.marked_binders().collect::<Vec<_>>(), vec![BinderId(9)]);

    let mut q = Parcel::new();
    q.unmarshall(&bytes);
    assert_eq!(q.data_size(), 4);
    assert_eq!(q.read_int32().unwrap(), 0x1234);
}

#[test]
fn string_decode_errors_do_not_panic() {
    // Invalid UTF-8 in a String8.
    let mut p = Parcel::new();
    p.write_int32(2);
    p.write_int32(0);
    p.unmarshall(&[2, 0, 0, 0, 0xff, 0xfe, 0x00, 0x00]);
    assert_eq!(p.read_string8().unwrap_err().code(), "PARCEL_BAD_STRING");

    // Unpaired surrogate in a String16.
    let mut p = Parcel::new();
    p.unmarshall(&[1, 0, 0, 0, 0x00, 0xd8, 0, 0]);
    assert_eq!(p.read_string16().unwrap_err().code(), "PARCEL_BAD_STRING");
}

#[test]
fn parcel_table_lifecycle_and_generations() {
    let table = ParcelTable::new();
    assert!(table.is_empty());

    let h = table.create();
    assert!(table.contains(h));
    table.with_parcel(h, |p| p.write_int32(42)).unwrap();
    let v = table
        .with_parcel_shared(h, |p| p.marshall())
        .unwrap();
    assert_eq!(v, vec![42, 0, 0, 0]);

    table.free(h).unwrap();
    assert!(!table.contains(h));
    assert!(table.with_parcel(h, |_| ()).is_err());
    assert_eq!(
        table.free(h).unwrap_err().code(),
        "PARCEL_STALE_HANDLE"
    );

    // The slot is reused with a bumped generation: the old handle stays dead.
    let h2 = table.create();
    assert_ne!(h, h2);
    assert!(table.contains(h2));
    assert!(table.with_parcel(h, |_| ()).is_err());
    table.free(h2).unwrap();
}

#[test]
fn parcel_table_is_thread_safe() {
    let table = Arc::new(ParcelTable::new());
    let mut handles = Vec::new();
    for t in 0..4 {
        let table = Arc::clone(&table);
        handles.push(std::thread::spawn(move || {
            for i in 0..64i32 {
                let h = table.create();
                table.with_parcel(h, |p| p.write_int32(t * 1000 + i)).unwrap();
                let v = table.with_parcel_shared(h, |p| p.marshall()).unwrap();
                assert_eq!(v, (t * 1000 + i).to_le_bytes().to_vec());
                table.free(h).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert!(table.is_empty());
}
