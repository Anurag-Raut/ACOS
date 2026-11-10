//! `jdk.internal.misc.Unsafe` / `sun.misc.Unsafe` (M1 subset).
//!
//! The console tier only needs the array-geometry constants and enough raw
//! access for `FileDescriptor`/`VarHandle` bootstrap. Offsets are model-defined:
//! instance slots are 4 bytes starting at byte 8, and array data starts at
//! byte 16 with the natural element scale.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::heap::{ArrayData, ArrayObj, Object, NULL};
use crate::vm::Vm;

const ARRAY_BASE: i32 = 16;
const INSTANCE_BASE: i32 = 8;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    for c in ["Ljdk/internal/misc/Unsafe;", "Lsun/misc/Unsafe;"] {
        vm.register_native(c, "arrayBaseOffset", "(Ljava/lang/Class;)I", array_base_offset);
        vm.register_native(c, "arrayIndexScale", "(Ljava/lang/Class;)I", array_index_scale);
        vm.register_native(c, "addressSize", "()I", address_size);
        vm.register_native(c, "pageSize", "()I", page_size);
        vm.register_native(c, "allocateInstance", "(Ljava/lang/Class;)Ljava/lang/Object;", allocate_instance);
        vm.register_native(c, "getInt", "(Ljava/lang/Object;J)I", get_int);
        vm.register_native(c, "putInt", "(Ljava/lang/Object;JI)V", put_int);
        vm.register_native(c, "getIntVolatile", "(Ljava/lang/Object;J)I", get_int);
        vm.register_native(c, "putIntVolatile", "(Ljava/lang/Object;JI)V", put_int);
        vm.register_native(c, "getIntAcquire", "(Ljava/lang/Object;J)I", get_int);
        vm.register_native(c, "putIntRelease", "(Ljava/lang/Object;JI)V", put_int);
        vm.register_native(c, "getIntOpaque", "(Ljava/lang/Object;J)I", get_int);
        vm.register_native(c, "putIntOpaque", "(Ljava/lang/Object;JI)V", put_int);
        vm.register_native(c, "getLong", "(Ljava/lang/Object;J)J", get_long);
        vm.register_native(c, "putLong", "(Ljava/lang/Object;JJ)V", put_long);
        vm.register_native(c, "getLongVolatile", "(Ljava/lang/Object;J)J", get_long);
        vm.register_native(c, "putLongVolatile", "(Ljava/lang/Object;JJ)V", put_long);
        vm.register_native(c, "getByte", "(Ljava/lang/Object;J)B", get_byte);
        vm.register_native(c, "putByte", "(Ljava/lang/Object;JB)V", put_byte);
        vm.register_native(c, "getBoolean", "(Ljava/lang/Object;J)Z", get_boolean);
        vm.register_native(c, "putBoolean", "(Ljava/lang/Object;JZ)V", put_boolean);
        vm.register_native(c, "getChar", "(Ljava/lang/Object;J)C", get_char);
        vm.register_native(c, "putChar", "(Ljava/lang/Object;JC)V", put_char);
        vm.register_native(c, "getShort", "(Ljava/lang/Object;J)S", get_short);
        vm.register_native(c, "putShort", "(Ljava/lang/Object;JS)V", put_short);
        vm.register_native(c, "getFloat", "(Ljava/lang/Object;J)F", get_float);
        vm.register_native(c, "putFloat", "(Ljava/lang/Object;JF)V", put_float);
        vm.register_native(c, "getDouble", "(Ljava/lang/Object;J)D", get_double);
        vm.register_native(c, "putDouble", "(Ljava/lang/Object;JD)V", put_double);
        vm.register_native(c, "getReference", "(Ljava/lang/Object;J)Ljava/lang/Object;", get_object);
        vm.register_native(c, "getObject", "(Ljava/lang/Object;J)Ljava/lang/Object;", get_object);
        vm.register_native(c, "getObjectVolatile", "(Ljava/lang/Object;J)Ljava/lang/Object;", get_object);
        vm.register_native(c, "putReference", "(Ljava/lang/Object;JLjava/lang/Object;)V", put_object);
        vm.register_native(c, "putObject", "(Ljava/lang/Object;JLjava/lang/Object;)V", put_object);
        vm.register_native(c, "putObjectVolatile", "(Ljava/lang/Object;JLjava/lang/Object;)V", put_object);
        vm.register_native(c, "compareAndSetInt", "(Ljava/lang/Object;JII)Z", cas_int);
        vm.register_native(c, "compareAndSetLong", "(Ljava/lang/Object;JJJ)Z", cas_long);
        vm.register_native(c, "compareAndSetReference", "(Ljava/lang/Object;JLjava/lang/Object;Ljava/lang/Object;)Z", cas_object);
        vm.register_native(c, "compareAndSetObject", "(Ljava/lang/Object;JLjava/lang/Object;Ljava/lang/Object;)Z", cas_object);
        vm.register_native(c, "getAndAddInt", "(Ljava/lang/Object;JI)I", get_and_add_int);
        vm.register_native(c, "getAndAddLong", "(Ljava/lang/Object;JJ)J", get_and_add_long);
        vm.register_native(c, "getAndSetInt", "(Ljava/lang/Object;JI)I", get_and_set_int);
        vm.register_native(c, "getAndSetLong", "(Ljava/lang/Object;JJ)J", get_and_set_long);
        vm.register_native(c, "getAndSetReference", "(Ljava/lang/Object;JLjava/lang/Object;)Ljava/lang/Object;", get_and_set_object);
        vm.register_native(c, "loadFence", "()V", noop);
        vm.register_native(c, "storeFence", "()V", noop);
        vm.register_native(c, "fullFence", "()V", noop);
        vm.register_native(c, "park", "(ZJ)V", noop);
        vm.register_native(c, "unpark", "(Ljava/lang/Object;)V", noop);
        vm.register_native(c, "objectFieldOffset", "(Ljava/lang/reflect/Field;)J", object_field_offset);
    }
    Ok(())
}

fn noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn array_base_offset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, ARRAY_BASE);
    Ok(())
}

fn array_index_scale(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let class_obj = vm.arg_ref(ctx, 0);
    let scale = vm
        .class_id_of_class_object(class_obj)
        .map(|c| vm.class_desc(c).to_string())
        .map(|d| match d.as_bytes().get(1).copied() {
            Some(b'Z') | Some(b'B') => 1,
            Some(b'C') | Some(b'S') => 2,
            Some(b'I') | Some(b'F') => 4,
            Some(b'J') | Some(b'D') => 8,
            _ => 4,
        })
        .unwrap_or(4);
    vm.ret_i32(ctx, scale);
    Ok(())
}

fn address_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, 8);
    Ok(())
}

fn page_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, 4096);
    Ok(())
}

fn allocate_instance(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let class_obj = vm.arg_ref(ctx, 0);
    let cid = vm.class_id_of_class_object(class_obj);
    match cid {
        Some(c) => {
            let h = vm.alloc_instance(c)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn object_field_offset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let field = vm.arg_ref(ctx, 0);
    let off = vm
        .get_instance_field_by_name(field, "offset")
        .map(|v| v as i32)
        .unwrap_or(INSTANCE_BASE);
    vm.ret_i64(ctx, off as i64);
    Ok(())
}

/// Decode an unsafe offset into `(is_array, index_slot)`.
fn offset_slots(offset: i64) -> usize {
    if offset >= ARRAY_BASE as i64 {
        ((offset - ARRAY_BASE as i64) / 4) as usize
    } else {
        ((offset - INSTANCE_BASE as i64) / 4).max(0) as usize
    }
}

fn get_slot(vm: &Vm, obj: crate::heap::Handle, offset: i64) -> u32 {
    match vm.heap.get(obj) {
        Some(Object::Instance { fields, .. }) => {
            fields.get(offset_slots(offset)).copied().unwrap_or(0)
        }
        Some(Object::Array(a)) => array_slot_u32(a, offset),
        Some(Object::String(u)) => {
            let i = offset_slots(offset);
            u.get(i).copied().unwrap_or(0) as u32
        }
        _ => 0,
    }
}

fn set_slot(vm: &mut Vm, obj: crate::heap::Handle, offset: i64, v: u32) {
    match vm.heap.get_mut(obj) {
        Some(Object::Instance { fields, .. }) => {
            let i = offset_slots(offset);
            if i < fields.len() {
                fields[i] = v;
            }
        }
        Some(Object::Array(a)) => set_array_slot_u32(a, offset, v),
        _ => {}
    }
}

fn array_slot_u32(a: &ArrayObj, offset: i64) -> u32 {
    let i = offset_slots(offset);
    match &a.data {
        ArrayData::Int(v) => v.get(i).copied().unwrap_or(0) as u32,
        ArrayData::Long(v) => v.get(i / 2).map(|x| *x as u32).unwrap_or(0),
        ArrayData::Float(v) => v.get(i).copied().unwrap_or(0.0).to_bits(),
        ArrayData::Double(v) => v.get(i / 2).map(|x| x.to_bits() as u32).unwrap_or(0),
        ArrayData::Char(v) => v.get(i / 2).copied().unwrap_or(0) as u32,
        ArrayData::Short(v) => v.get(i / 2).copied().unwrap_or(0) as u32,
        ArrayData::Byte(v) => v.get(i / 4).copied().unwrap_or(0) as u32,
        ArrayData::Bool(v) => *v.get(i / 4).unwrap_or(&false) as u32,
        ArrayData::Ref(v) => v.get(i / 2).copied().unwrap_or(0),
    }
}

fn set_array_slot_u32(a: &mut ArrayObj, offset: i64, v: u32) {
    let i = offset_slots(offset);
    match &mut a.data {
        ArrayData::Int(x) => {
            if i < x.len() {
                x[i] = v as i32;
            }
        }
        ArrayData::Long(x) => {
            if i / 2 < x.len() {
                x[i / 2] = v as i64;
            }
        }
        ArrayData::Float(x) => {
            if i < x.len() {
                x[i] = f32::from_bits(v);
            }
        }
        ArrayData::Double(x) => {
            if i / 2 < x.len() {
                x[i / 2] = f64::from_bits(v as u64);
            }
        }
        ArrayData::Char(x) => {
            if i / 2 < x.len() {
                x[i / 2] = v as u16;
            }
        }
        ArrayData::Short(x) => {
            if i / 2 < x.len() {
                x[i / 2] = v as i16;
            }
        }
        ArrayData::Byte(x) => {
            if i / 4 < x.len() {
                x[i / 4] = v as i8;
            }
        }
        ArrayData::Bool(x) => {
            if i / 4 < x.len() {
                x[i / 4] = v != 0;
            }
        }
        ArrayData::Ref(x) => {
            if i / 2 < x.len() {
                x[i / 2] = v;
            }
        }
    }
}

fn get_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = get_slot(vm, obj, off);
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn put_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_i32(ctx, 2);
    set_slot(vm, obj, off, v as u32);
    Ok(())
}

fn get_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let lo = get_slot(vm, obj, off) as u64;
    let hi = get_slot(vm, obj, off + 4) as u64;
    vm.ret_i64(ctx, (lo | (hi << 32)) as i64);
    Ok(())
}

fn put_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_i64(ctx, 2) as u64;
    set_slot(vm, obj, off, v as u32);
    set_slot(vm, obj, off + 4, (v >> 32) as u32);
    Ok(())
}

fn get_byte(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = get_slot(vm, vm.arg_ref(ctx, 0), vm.arg_i64(ctx, 1));
    vm.ret_i32(ctx, (v as i8) as i32);
    Ok(())
}

fn put_byte(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_i32(ctx, 2);
    set_slot(vm, obj, off, (v as i8) as i32 as u32);
    Ok(())
}

fn get_boolean(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = get_slot(vm, vm.arg_ref(ctx, 0), vm.arg_i64(ctx, 1));
    vm.ret_bool(ctx, v != 0);
    Ok(())
}

fn put_boolean(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_bool(ctx, 2);
    set_slot(vm, obj, off, v as u32);
    Ok(())
}

fn get_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = get_slot(vm, vm.arg_ref(ctx, 0), vm.arg_i64(ctx, 1));
    vm.ret_i32(ctx, (v as u16) as i32);
    Ok(())
}

fn put_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_i32(ctx, 2);
    set_slot(vm, obj, off, (v as u16) as u32);
    Ok(())
}

fn get_short(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = get_slot(vm, vm.arg_ref(ctx, 0), vm.arg_i64(ctx, 1));
    vm.ret_i32(ctx, (v as i16) as i32);
    Ok(())
}

fn put_short(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_i32(ctx, 2);
    set_slot(vm, obj, off, (v as i16) as i32 as u32);
    Ok(())
}

fn get_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = get_slot(vm, vm.arg_ref(ctx, 0), vm.arg_i64(ctx, 1));
    vm.ret_f32(ctx, f32::from_bits(v));
    Ok(())
}

fn put_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_f32(ctx, 2);
    set_slot(vm, obj, off, v.to_bits());
    Ok(())
}

fn get_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let lo = get_slot(vm, obj, off) as u64;
    let hi = get_slot(vm, obj, off + 4) as u64;
    vm.ret_f64(ctx, f64::from_bits(lo | (hi << 32)));
    Ok(())
}

fn put_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_f64(ctx, 2).to_bits();
    set_slot(vm, obj, off, v as u32);
    set_slot(vm, obj, off + 4, (v >> 32) as u32);
    Ok(())
}

fn get_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = get_slot(vm, vm.arg_ref(ctx, 0), vm.arg_i64(ctx, 1));
    vm.ret_ref(ctx, v);
    Ok(())
}

fn put_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_ref(ctx, 2);
    set_slot(vm, obj, off, v);
    Ok(())
}

fn cas_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let expect = vm.arg_i32(ctx, 2);
    let update = vm.arg_i32(ctx, 3);
    let old = get_slot(vm, obj, off) as i32;
    let ok = old == expect;
    if ok {
        set_slot(vm, obj, off, update as u32);
    }
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn cas_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let expect = vm.arg_i64(ctx, 2);
    let update = vm.arg_i64(ctx, 3);
    let lo = get_slot(vm, obj, off) as u64;
    let hi = get_slot(vm, obj, off + 4) as u64;
    let old = (lo | (hi << 32)) as i64;
    let ok = old == expect;
    if ok {
        let u = update as u64;
        set_slot(vm, obj, off, u as u32);
        set_slot(vm, obj, off + 4, (u >> 32) as u32);
    }
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn cas_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let expect = vm.arg_ref(ctx, 2);
    let update = vm.arg_ref(ctx, 3);
    let old = get_slot(vm, obj, off);
    let ok = old == expect;
    if ok {
        set_slot(vm, obj, off, update);
    }
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn get_and_add_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let delta = vm.arg_i32(ctx, 2);
    let old = get_slot(vm, obj, off) as i32;
    set_slot(vm, obj, off, old.wrapping_add(delta) as u32);
    vm.ret_i32(ctx, old);
    Ok(())
}

fn get_and_add_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let delta = vm.arg_i64(ctx, 2);
    let lo = get_slot(vm, obj, off) as u64;
    let hi = get_slot(vm, obj, off + 4) as u64;
    let old = (lo | (hi << 32)) as i64;
    let nv = old.wrapping_add(delta) as u64;
    set_slot(vm, obj, off, nv as u32);
    set_slot(vm, obj, off + 4, (nv >> 32) as u32);
    vm.ret_i64(ctx, old);
    Ok(())
}

fn get_and_set_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let nv = vm.arg_i32(ctx, 2);
    let old = get_slot(vm, obj, off) as i32;
    set_slot(vm, obj, off, nv as u32);
    vm.ret_i32(ctx, old);
    Ok(())
}

fn get_and_set_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let nv = vm.arg_i64(ctx, 2) as u64;
    let lo = get_slot(vm, obj, off) as u64;
    let hi = get_slot(vm, obj, off + 4) as u64;
    let old = (lo | (hi << 32)) as i64;
    set_slot(vm, obj, off, nv as u32);
    set_slot(vm, obj, off + 4, (nv >> 32) as u32);
    vm.ret_i64(ctx, old);
    Ok(())
}

fn get_and_set_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let nv = vm.arg_ref(ctx, 2);
    let old = get_slot(vm, obj, off);
    set_slot(vm, obj, off, nv);
    vm.ret_ref(ctx, old);
    Ok(())
}
