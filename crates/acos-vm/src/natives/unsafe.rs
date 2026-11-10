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
        vm.register_native(c, "allocateMemory", "(J)J", allocate_memory);
        vm.register_native(c, "freeMemory", "(J)V", free_memory);
        vm.register_native(c, "reallocateMemory", "(JJ)J", reallocate_memory);
        vm.register_native(c, "setMemory", "(JJB)V", set_memory_addr);
        vm.register_native(c, "setMemory", "(Ljava/lang/Object;JJB)V", set_memory_obj);
        vm.register_native(c, "copyMemory", "(JJJ)V", copy_memory_addr);
        vm.register_native(c, "copyMemory", "(Ljava/lang/Object;JLjava/lang/Object;JJ)V", copy_memory_obj);
        // `jdk.internal.misc.Unsafe.copyMemory(Object,long,Object,long,long)`
        // funnels through this private native.
        vm.register_native(c, "copyMemory0", "(Ljava/lang/Object;JLjava/lang/Object;JJ)V", copy_memory_obj);
        vm.register_native(c, "copyMemoryFromPrimitiveArray", "(Ljava/lang/Object;JJJ)V", copy_from_array);
        vm.register_native(c, "copyMemoryToPrimitiveArray", "(JLjava/lang/Object;JJ)V", copy_to_array);
        // Raw-address (long) get/put family used by `java.nio.Bits` and
        // `DirectByteBuffer`. Byte order is native little-endian.
        vm.register_native(c, "getByte", "(J)B", get_byte_addr);
        vm.register_native(c, "putByte", "(JB)V", put_byte_addr);
        vm.register_native(c, "getShort", "(J)S", get_short_addr);
        vm.register_native(c, "putShort", "(JS)V", put_short_addr);
        vm.register_native(c, "getChar", "(J)C", get_char_addr);
        vm.register_native(c, "putChar", "(JC)V", put_char_addr);
        vm.register_native(c, "getInt", "(J)I", get_int_addr);
        vm.register_native(c, "putInt", "(JI)V", put_int_addr);
        vm.register_native(c, "getLong", "(J)J", get_long_addr);
        vm.register_native(c, "putLong", "(JJ)V", put_long_addr);
        vm.register_native(c, "getFloat", "(J)F", get_float_addr);
        vm.register_native(c, "putFloat", "(JF)V", put_float_addr);
        vm.register_native(c, "getDouble", "(J)D", get_double_addr);
        vm.register_native(c, "putDouble", "(JD)V", put_double_addr);
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
        vm.register_native(c, "getReferenceVolatile", "(Ljava/lang/Object;J)Ljava/lang/Object;", get_object);
        vm.register_native(c, "getReferenceAcquire", "(Ljava/lang/Object;J)Ljava/lang/Object;", get_object);
        vm.register_native(c, "getReferenceOpaque", "(Ljava/lang/Object;J)Ljava/lang/Object;", get_object);
        vm.register_native(c, "putReference", "(Ljava/lang/Object;JLjava/lang/Object;)V", put_object);
        vm.register_native(c, "putObject", "(Ljava/lang/Object;JLjava/lang/Object;)V", put_object);
        vm.register_native(c, "putObjectVolatile", "(Ljava/lang/Object;JLjava/lang/Object;)V", put_object);
        vm.register_native(c, "putReferenceVolatile", "(Ljava/lang/Object;JLjava/lang/Object;)V", put_object);
        vm.register_native(c, "putReferenceRelease", "(Ljava/lang/Object;JLjava/lang/Object;)V", put_object);
        vm.register_native(c, "putReferenceOpaque", "(Ljava/lang/Object;JLjava/lang/Object;)V", put_object);
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

/// Decode an **array** offset into a 4-byte element index.
///
/// Only valid for arrays: the array data base is [`ARRAY_BASE`]. Instance
/// offsets are decoded with [`instance_slot`] because an instance field offset
/// can also be `>= ARRAY_BASE` (slot 2 and beyond), and the old
/// `offset >= ARRAY_BASE` heuristic then read/wrote the wrong slot. That broke
/// `ConcurrentHashMap`'s `sizeCtl` CAS (`initTable` spun forever) during the
/// framework boot.
fn array_slot(offset: i64) -> usize {
    ((offset - ARRAY_BASE as i64) / 4).max(0) as usize
}

/// Decode an **instance** field offset into a 4-byte slot index.
fn instance_slot(offset: i64) -> usize {
    ((offset - INSTANCE_BASE as i64) / 4).max(0) as usize
}

fn get_slot(vm: &Vm, obj: crate::heap::Handle, offset: i64) -> u32 {
    match vm.heap.get(obj) {
        Some(Object::Instance { fields, .. }) => {
            fields.get(instance_slot(offset)).copied().unwrap_or(0)
        }
        Some(Object::Array(a)) => array_slot_u32(a, offset),
        Some(Object::String(u)) => {
            let i = instance_slot(offset);
            u.get(i).copied().unwrap_or(0) as u32
        }
        _ => 0,
    }
}

fn set_slot(vm: &mut Vm, obj: crate::heap::Handle, offset: i64, v: u32) {
    match vm.heap.get_mut(obj) {
        Some(Object::Instance { fields, .. }) => {
            let i = instance_slot(offset);
            if i < fields.len() {
                fields[i] = v;
            }
        }
        Some(Object::Array(a)) => set_array_slot_u32(a, offset, v),
        _ => {}
    }
}

fn array_slot_u32(a: &ArrayObj, offset: i64) -> u32 {
    let i = array_slot(offset);
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
    let i = array_slot(offset);
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
    let v = obj_read(vm, obj, off, 4)? as i32;
    vm.ret_i32(ctx, v);
    Ok(())
}

fn put_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_i32(ctx, 2);
    obj_write(vm, obj, off, 4, v as u32 as u64)
}

fn get_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = obj_read(vm, obj, off, 8)? as i64;
    vm.ret_i64(ctx, v);
    Ok(())
}

fn put_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_i64(ctx, 2) as u64;
    obj_write(vm, obj, off, 8, v)
}

fn get_byte(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = obj_read(vm, obj, off, 1)? as i8;
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn put_byte(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_i32(ctx, 2);
    obj_write(vm, obj, off, 1, (v as i8) as i32 as u32 as u64)
}

fn get_boolean(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = obj_read(vm, obj, off, 1)? != 0;
    vm.ret_bool(ctx, v);
    Ok(())
}

fn put_boolean(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_bool(ctx, 2);
    obj_write(vm, obj, off, 1, v as u64)
}

fn get_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = obj_read(vm, obj, off, 2)? as u16;
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn put_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_i32(ctx, 2);
    obj_write(vm, obj, off, 2, (v as u16) as u64)
}

fn get_short(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = obj_read(vm, obj, off, 2)? as i16;
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn put_short(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_i32(ctx, 2);
    obj_write(vm, obj, off, 2, (v as i16) as i32 as u32 as u64)
}

fn get_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = obj_read(vm, obj, off, 4)? as u32;
    vm.ret_f32(ctx, f32::from_bits(v));
    Ok(())
}

fn put_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_f32(ctx, 2);
    obj_write(vm, obj, off, 4, v.to_bits() as u64)
}

fn get_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = obj_read(vm, obj, off, 8)?;
    vm.ret_f64(ctx, f64::from_bits(v));
    Ok(())
}

fn put_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_f64(ctx, 2).to_bits();
    obj_write(vm, obj, off, 8, v)
}

fn get_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = obj_read(vm, obj, off, 4)? as u32;
    vm.ret_ref(ctx, v);
    Ok(())
}

fn put_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let v = vm.arg_ref(ctx, 2);
    obj_write(vm, obj, off, 4, v as u64)
}

fn cas_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let expect = vm.arg_i32(ctx, 2);
    let update = vm.arg_i32(ctx, 3);
    let old = obj_read(vm, obj, off, 4)? as i32;
    let ok = old == expect;
    if ok {
        obj_write(vm, obj, off, 4, update as u32 as u64)?;
    }
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn cas_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let expect = vm.arg_i64(ctx, 2);
    let update = vm.arg_i64(ctx, 3);
    let old = obj_read(vm, obj, off, 8)? as i64;
    let ok = old == expect;
    if ok {
        obj_write(vm, obj, off, 8, update as u64)?;
    }
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn cas_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let expect = vm.arg_ref(ctx, 2);
    let update = vm.arg_ref(ctx, 3);
    let old = obj_read(vm, obj, off, 4)? as u32;
    let ok = old == expect;
    if ok {
        obj_write(vm, obj, off, 4, update as u64)?;
    }
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn get_and_add_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let delta = vm.arg_i32(ctx, 2);
    let old = obj_read(vm, obj, off, 4)? as i32;
    obj_write(vm, obj, off, 4, old.wrapping_add(delta) as u32 as u64)?;
    vm.ret_i32(ctx, old);
    Ok(())
}

fn get_and_add_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let delta = vm.arg_i64(ctx, 2);
    let old = obj_read(vm, obj, off, 8)? as i64;
    obj_write(vm, obj, off, 8, old.wrapping_add(delta) as u64)?;
    vm.ret_i64(ctx, old);
    Ok(())
}

fn get_and_set_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let nv = vm.arg_i32(ctx, 2);
    let old = obj_read(vm, obj, off, 4)? as i32;
    obj_write(vm, obj, off, 4, nv as u32 as u64)?;
    vm.ret_i32(ctx, old);
    Ok(())
}

fn get_and_set_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let nv = vm.arg_i64(ctx, 2) as u64;
    let old = obj_read(vm, obj, off, 8)? as i64;
    obj_write(vm, obj, off, 8, nv)?;
    vm.ret_i64(ctx, old);
    Ok(())
}

fn get_and_set_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let nv = vm.arg_ref(ctx, 2);
    let old = obj_read(vm, obj, off, 4)? as u32;
    obj_write(vm, obj, off, 4, nv as u64)?;
    vm.ret_ref(ctx, old);
    Ok(())
}

// ---------------------------------------------------------------------------
// Object (array / instance) byte access
// ---------------------------------------------------------------------------

/// Byte offset within a Java array's backing store for an `Unsafe` array
/// offset (`arrayBaseOffset + index * arrayIndexScale`).
fn array_byte_offset(offset: i64) -> usize {
    (offset - ARRAY_BASE as i64).max(0) as usize
}

fn is_array(vm: &Vm, obj: crate::heap::Handle) -> bool {
    matches!(vm.heap.get(obj), Some(Object::Array(_)))
}

/// Read `n` (1/2/4/8) little-endian bytes from an array element or an
/// instance field slot. Array accesses are bounds-checked and raise a Java
/// `ArrayIndexOutOfBoundsException` on overflow.
fn obj_read(vm: &mut Vm, obj: crate::heap::Handle, off: i64, n: usize) -> VmResult<u64> {
    if is_array(vm, obj) {
        let bytes = super::native_mem::array_read_bytes(vm, obj, array_byte_offset(off), n)?;
        let mut v = 0u64;
        for (i, b) in bytes.iter().enumerate() {
            v |= (*b as u64) << (8 * i);
        }
        Ok(v)
    } else if n == 8 {
        let lo = get_slot(vm, obj, off) as u64;
        let hi = get_slot(vm, obj, off + 4) as u64;
        Ok(lo | (hi << 32))
    } else {
        Ok(get_slot(vm, obj, off) as u64)
    }
}

fn obj_write(vm: &mut Vm, obj: crate::heap::Handle, off: i64, n: usize, v: u64) -> VmResult<()> {
    if is_array(vm, obj) {
        let bytes: Vec<u8> = (0..n).map(|i| (v >> (8 * i)) as u8).collect();
        super::native_mem::array_write_bytes(vm, obj, array_byte_offset(off), &bytes)
    } else if n == 8 {
        set_slot(vm, obj, off, v as u32);
        set_slot(vm, obj, off + 4, (v >> 32) as u32);
        Ok(())
    } else {
        set_slot(vm, obj, off, v as u32);
        Ok(())
    }
}

/// Read `len` bytes from an array/native object (used by `setMemory`/`copyMemory`).
fn obj_read_bytes(vm: &mut Vm, obj: crate::heap::Handle, off: i64, len: usize) -> VmResult<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if is_array(vm, obj) {
        return super::native_mem::array_read_bytes(vm, obj, array_byte_offset(off), len);
    }
    let mut out = Vec::with_capacity(len);
    let mut i = 0usize;
    while i < len {
        let chunk = (len - i).min(4);
        out.extend_from_slice(&get_slot(vm, obj, off + i as i64).to_le_bytes()[..chunk]);
        i += chunk;
    }
    Ok(out)
}

/// Write `bytes` to an array/native object (used by `setMemory`/`copyMemory`).
fn obj_write_bytes(vm: &mut Vm, obj: crate::heap::Handle, off: i64, bytes: &[u8]) -> VmResult<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    if is_array(vm, obj) {
        return super::native_mem::array_write_bytes(vm, obj, array_byte_offset(off), bytes);
    }
    let mut i = 0usize;
    while i < bytes.len() {
        let chunk = (bytes.len() - i).min(4);
        let mut w = [0u8; 4];
        w[..chunk].copy_from_slice(&bytes[i..i + chunk]);
        set_slot(vm, obj, off + i as i64, u32::from_le_bytes(w));
        i += chunk;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Raw-address (`long`) access and memory management
// ---------------------------------------------------------------------------

fn len_of(vm: &mut Vm, n: i64) -> VmResult<usize> {
    if n < 0 {
        return Ok(0);
    }
    let n = n as usize;
    let cap = super::native_mem::cap_bytes();
    if n > cap {
        return Err(super::native_mem::MemError::CapExceeded.throw(vm));
    }
    Ok(n)
}

fn allocate_memory(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let n = len_of(vm, vm.arg_i64(ctx, 0))?;
    let addr = super::native_mem::alloc(n, false).map_err(|e| e.throw(vm))?;
    vm.ret_i64(ctx, addr);
    Ok(())
}

fn free_memory(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    super::native_mem::free(vm.arg_i64(ctx, 0));
    Ok(())
}

fn reallocate_memory(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let n = len_of(vm, vm.arg_i64(ctx, 1))?;
    let new = super::native_mem::realloc(addr, n).map_err(|e| e.throw(vm))?;
    vm.ret_i64(ctx, new);
    Ok(())
}

fn set_memory_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let n = len_of(vm, vm.arg_i64(ctx, 1))?;
    let v = vm.arg_i32(ctx, 2) as u8;
    let bytes = vec![v; n];
    super::native_mem::write_bytes_at(vm, addr, &bytes)
}

fn set_memory_obj(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let off = vm.arg_i64(ctx, 1);
    let n = len_of(vm, vm.arg_i64(ctx, 2))?;
    let v = vm.arg_i32(ctx, 3) as u8;
    let bytes = vec![v; n];
    obj_write_bytes(vm, obj, off, &bytes)
}

fn copy_memory_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = vm.arg_i64(ctx, 0);
    let dst = vm.arg_i64(ctx, 1);
    let n = len_of(vm, vm.arg_i64(ctx, 2))?;
    let bytes = super::native_mem::read_bytes_at(vm, src, n)?;
    super::native_mem::write_bytes_at(vm, dst, &bytes)
}

fn copy_memory_obj(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src_obj = vm.arg_ref(ctx, 0);
    let src_off = vm.arg_i64(ctx, 1);
    let dst_obj = vm.arg_ref(ctx, 2);
    let dst_off = vm.arg_i64(ctx, 3);
    let n = len_of(vm, vm.arg_i64(ctx, 4))?;
    let bytes = if src_obj == NULL {
        super::native_mem::read_bytes_at(vm, src_off, n)?
    } else {
        obj_read_bytes(vm, src_obj, src_off, n)?
    };
    if dst_obj == NULL {
        super::native_mem::write_bytes_at(vm, dst_off, &bytes)
    } else {
        obj_write_bytes(vm, dst_obj, dst_off, &bytes)
    }
}

fn copy_from_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = vm.arg_ref(ctx, 0);
    let src_off = vm.arg_i64(ctx, 1);
    let dst = vm.arg_i64(ctx, 2);
    let n = len_of(vm, vm.arg_i64(ctx, 3))?;
    let bytes = obj_read_bytes(vm, src, src_off, n)?;
    super::native_mem::write_bytes_at(vm, dst, &bytes)
}

fn copy_to_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = vm.arg_i64(ctx, 0);
    let dst = vm.arg_ref(ctx, 1);
    let dst_off = vm.arg_i64(ctx, 2);
    let n = len_of(vm, vm.arg_i64(ctx, 3))?;
    let bytes = super::native_mem::read_bytes_at(vm, src, n)?;
    obj_write_bytes(vm, dst, dst_off, &bytes)
}

fn get_byte_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = super::native_mem::read_i8(vm, vm.arg_i64(ctx, 0))?;
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn put_byte_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_i32(ctx, 1) as i8;
    super::native_mem::write_i8(vm, addr, v)
}

fn get_short_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = super::native_mem::read_i16(vm, vm.arg_i64(ctx, 0))?;
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn put_short_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_i32(ctx, 1) as i16;
    super::native_mem::write_i16(vm, addr, v)
}

fn get_char_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = super::native_mem::read_i16(vm, vm.arg_i64(ctx, 0))?;
    vm.ret_i32(ctx, v as u16 as i32);
    Ok(())
}

fn put_char_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_i32(ctx, 1) as u16 as i16;
    super::native_mem::write_i16(vm, addr, v)
}

fn get_int_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = super::native_mem::read_i32(vm, vm.arg_i64(ctx, 0))?;
    vm.ret_i32(ctx, v);
    Ok(())
}

fn put_int_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_i32(ctx, 1);
    super::native_mem::write_i32(vm, addr, v)
}

fn get_long_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = super::native_mem::read_i64(vm, vm.arg_i64(ctx, 0))?;
    vm.ret_i64(ctx, v);
    Ok(())
}

fn put_long_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_i64(ctx, 1);
    super::native_mem::write_i64(vm, addr, v)
}

fn get_float_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = super::native_mem::read_f32(vm, vm.arg_i64(ctx, 0))?;
    vm.ret_f32(ctx, v);
    Ok(())
}

fn put_float_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_f32(ctx, 1);
    super::native_mem::write_f32(vm, addr, v)
}

fn get_double_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = super::native_mem::read_f64(vm, vm.arg_i64(ctx, 0))?;
    vm.ret_f64(ctx, v);
    Ok(())
}

fn put_double_addr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_f64(ctx, 1);
    super::native_mem::write_f64(vm, addr, v)
}
