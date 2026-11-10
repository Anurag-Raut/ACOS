//! Property tests: random op sequences round-trip, and arbitrary bytes never
//! make a reader panic.

use acos_parcel::binder::{BinderId, BinderKind};
use acos_parcel::parcel::Parcel;
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Bool(bool),
    Byte(i8),
    Str8(Option<String>),
    Str16(Option<String>),
    ByteArray(Option<Vec<u8>>),
    I32Array(Vec<i32>),
    I64Array(Vec<i64>),
    Binder(u64),
    Blob(Vec<u8>),
    Parcelable(bool),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        any::<i32>().prop_map(Op::I32),
        any::<i64>().prop_map(Op::I64),
        any::<f32>().prop_map(Op::F32),
        any::<f64>().prop_map(Op::F64),
        any::<bool>().prop_map(Op::Bool),
        any::<i8>().prop_map(Op::Byte),
        prop::option::of(".*").prop_map(Op::Str8),
        prop::option::of(".*").prop_map(Op::Str16),
        prop::option::of(prop::collection::vec(any::<u8>(), 0..8)).prop_map(Op::ByteArray),
        prop::collection::vec(any::<i32>(), 0..6).prop_map(Op::I32Array),
        prop::collection::vec(any::<i64>(), 0..6).prop_map(Op::I64Array),
        any::<u64>().prop_map(Op::Binder),
        prop::collection::vec(any::<u8>(), 0..8).prop_map(Op::Blob),
        any::<bool>().prop_map(Op::Parcelable),
    ]
}

fn apply(p: &mut Parcel, op: &Op) {
    match op {
        Op::I32(v) => p.write_int32(*v),
        Op::I64(v) => p.write_int64(*v),
        Op::F32(v) => p.write_float(*v),
        Op::F64(v) => p.write_double(*v),
        Op::Bool(v) => p.write_bool(*v),
        Op::Byte(v) => p.write_byte(*v),
        Op::Str8(v) => p.write_string8_opt(v.as_deref()).unwrap(),
        Op::Str16(v) => p.write_string16_opt(v.as_deref()).unwrap(),
        Op::ByteArray(v) => p.write_byte_array(v.as_deref()).unwrap(),
        Op::I32Array(v) => p.write_int32_array(Some(v)).unwrap(),
        Op::I64Array(v) => p.write_int64_array(Some(v)).unwrap(),
        Op::Binder(v) => p.write_strong_binder_kind(BinderId(*v), BinderKind::Proxy),
        Op::Blob(v) => p.write_blob(v).unwrap(),
        Op::Parcelable(v) => p.write_parcelable_marker(*v),
    }
}

fn check(p: &mut Parcel, op: &Op) {
    match op {
        Op::I32(v) => assert_eq!(p.read_int32().unwrap(), *v),
        Op::I64(v) => assert_eq!(p.read_int64().unwrap(), *v),
        Op::F32(v) => assert_eq!(p.read_float().unwrap().to_bits(), v.to_bits()),
        Op::F64(v) => assert_eq!(p.read_double().unwrap().to_bits(), v.to_bits()),
        Op::Bool(v) => assert_eq!(p.read_bool().unwrap(), *v),
        Op::Byte(v) => assert_eq!(p.read_byte().unwrap(), *v),
        Op::Str8(v) => assert_eq!(p.read_string8().unwrap().as_deref(), v.as_deref()),
        Op::Str16(v) => assert_eq!(p.read_string16().unwrap().as_deref(), v.as_deref()),
        Op::ByteArray(v) => assert_eq!(p.read_byte_array().unwrap(), *v),
        Op::I32Array(v) => assert_eq!(p.read_int32_array().unwrap(), Some(v.clone())),
        Op::I64Array(v) => assert_eq!(p.read_int64_array().unwrap(), Some(v.clone())),
        Op::Binder(v) => {
            let expected = if *v == 0 { None } else { Some(BinderId(*v)) };
            assert_eq!(p.read_strong_binder().unwrap(), expected);
        }
        Op::Blob(v) => assert_eq!(p.read_blob(v.len()).unwrap(), Some(v.clone())),
        Op::Parcelable(v) => assert_eq!(p.read_parcelable_marker().unwrap(), *v),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn write_then_read_back(ops in prop::collection::vec(op_strategy(), 0..32)) {
        let mut p = Parcel::new();
        for op in &ops {
            apply(&mut p, op);
        }
        let encoded = p.marshall();
        p.set_data_position(0);
        for op in &ops {
            check(&mut p, op);
        }
        prop_assert_eq!(p.data_avail(), 0);
        // Re-encoding from the same operations is deterministic.
        let mut q = Parcel::new();
        for op in &ops {
            apply(&mut q, op);
        }
        prop_assert_eq!(q.marshall(), encoded);
    }

    #[test]
    fn arbitrary_bytes_never_panic(
        data in prop::collection::vec(any::<u8>(), 0..256),
        pos in any::<usize>(),
    ) {
        let mut p = Parcel::new();
        p.unmarshall(&data);
        p.set_data_position(pos);
        // Every reader, in sequence, must return a value or an error.
        let _ = p.read_int32();
        let _ = p.read_uint32();
        let _ = p.read_int64();
        let _ = p.read_uint64();
        let _ = p.read_float();
        let _ = p.read_double();
        let _ = p.read_bool();
        let _ = p.read_byte();
        let _ = p.read_char();
        let _ = p.read_string8();
        let _ = p.read_string16();
        let _ = p.read_byte_array();
        let _ = p.read_int32_array();
        let _ = p.read_int64_array();
        let _ = p.read_float_array();
        let _ = p.read_double_array();
        let _ = p.read_string16_array();
        let _ = p.read_blob(data.len());
        let _ = p.read_blob(usize::MAX / 2);
        let _ = p.read_strong_binder();
        let _ = p.read_file_descriptor();
        let _ = p.read_parcelable_marker();
        let _ = p.data_avail();
        let _ = p.data_position();
        let _ = p.data_size();
        let _ = p.objects_count();
        let _ = p.has_file_descriptors();
        let _ = p.marshall();
    }

    #[test]
    fn arbitrary_bytes_into_every_offset(
        data in prop::collection::vec(any::<u8>(), 0..128),
        pos in 0usize..=512,
    ) {
        let mut p = Parcel::new();
        p.unmarshall(&data);
        p.set_data_position(pos);
        let _ = p.read_string8();
        let _ = p.read_string16();
        let _ = p.read_strong_binder();
        let _ = p.read_int32_array();
    }

    #[test]
    fn string_round_trips_arbitrary_unicode(s in "\\PC*") {
        let mut p = Parcel::new();
        p.write_string16(&s).unwrap();
        p.write_string8(&s).unwrap();
        p.set_data_position(0);
        let got16 = p.read_string16().unwrap();
        prop_assert_eq!(got16.as_deref(), Some(s.as_str()));
        let got8 = p.read_string8().unwrap();
        prop_assert_eq!(got8.as_deref(), Some(s.as_str()));
    }
}
