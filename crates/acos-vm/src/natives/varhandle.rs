//! `java.lang.invoke.VarHandle` / `MethodHandle` signature-polymorphic access.
//!
//! Android's real core library builds `VarHandle` objects in guest bytecode:
//! `MethodHandles.lookup().findVarHandle` / `findStaticVarHandle` return a
//! `java.lang.invoke.FieldVarHandle` / `StaticFieldVarHandle`, and
//! `MethodHandles.arrayElementVarHandle` returns an `ArrayElementVarHandle`.
//! Their accessors (`get`, `set`, `compareAndSet`, ...) are declared `native
//! final varargs` with the generic `([Ljava/lang/Object;)…` signature and are
//! invoked through the DEX `invoke-polymorphic` opcode (0xfa/0xfb), whose
//! `index2` operand carries the call-site prototype with the *real* operand
//! types.
//!
//! This module resolves the field/array descriptor already stored in the real
//! `VarHandle` instance (ART stores the `ArtField` offset in `artField:J`, and
//! the array/declaring class in the `VarHandle` coordinate fields) and performs
//! the access directly on the ACOS heap. The VM runs green threads on one host
//! thread, so an atomic access mode is trivially atomic; the acquire/release/
//! opaque/volatile variants all reduce to the same single-threaded operation.
//!
//! Anything the VM cannot represent (an unknown access mode, an arithmetic
//! access on a reference, ...) fails with a structured `UNSUPPORTED_BYTECODE`
//! naming the `VarHandle` class and field, never a silent fake.

use crate::class::{ClassId, FieldSign, TypeKind};
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, Object, NULL};
use crate::vm::Vm;

/// Object header / instance-field slot encoding used by `natives::reflect`:
/// a field's `artField` word is `8 + slot*4` (two slots for wide fields).
const FIELD_BASE: i32 = 8;

/// A resolved storage target behind a `VarHandle`.
pub(crate) enum VhTarget {
    Instance {
        class: ClassId,
        slot: usize,
        desc: String,
    },
    Static {
        class: ClassId,
        slot: usize,
        desc: String,
    },
    Array {
        elem_desc: String,
    },
}

impl VhTarget {
    pub(crate) fn desc(&self) -> &str {
        match self {
            VhTarget::Instance { desc, .. } | VhTarget::Static { desc, .. } => desc,
            VhTarget::Array { elem_desc } => elem_desc,
        }
    }

    /// Human-readable target for error messages.
    fn label(&self, vm: &Vm) -> String {
        match self {
            VhTarget::Instance { class, slot, desc } => {
                format!("{} field #{} ({desc})", vm.class_desc(*class), slot)
            }
            VhTarget::Static { class, slot, desc } => {
                format!("static {} field #{} ({desc})", vm.class_desc(*class), slot)
            }
            VhTarget::Array { elem_desc } => format!("array element ({elem_desc})"),
        }
    }
}

/// An access mode, normalized from the `VarHandle` method name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VhMode {
    Get,
    Set,
    GetAndSet,
    CompareAndSet,
    CompareAndExchange,
    GetAndAdd,
    GetAndBitwiseAnd,
    GetAndBitwiseOr,
    GetAndBitwiseXor,
}

/// The result of an access. `Value` is the raw register encoding: a heap handle
/// for a reference target, the little-endian bits for a primitive, already
/// narrowed/sign-extended to the target descriptor.
pub(crate) enum VhOut {
    Void,
    Value(u64),
    Bool(bool),
}

/// Classify a `VarHandle` method name into an access mode, ignoring the
/// memory-ordering suffix (`Acquire`/`Release`/`Opaque`/`Volatile`/`Plain`).
pub(crate) fn mode_of(name: &str) -> Option<VhMode> {
    let mut base = name;
    for suffix in ["Acquire", "Release", "Opaque", "Volatile", "Plain"] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped;
            break;
        }
    }
    Some(match base {
        "get" => VhMode::Get,
        "set" => VhMode::Set,
        "getAndSet" => VhMode::GetAndSet,
        "compareAndSet" | "weakCompareAndSet" => VhMode::CompareAndSet,
        "compareAndExchange" => VhMode::CompareAndExchange,
        "getAndAdd" => VhMode::GetAndAdd,
        "getAndBitwiseAnd" => VhMode::GetAndBitwiseAnd,
        "getAndBitwiseOr" => VhMode::GetAndBitwiseOr,
        "getAndBitwiseXor" => VhMode::GetAndBitwiseXor,
        _ => return None,
    })
}

/// Resolve the storage behind a live `VarHandle` instance.
///
/// Returns `None` when `vh` is not one of the core-library VarHandle shapes
/// (the caller then reports a structured unsupported error).
pub(crate) fn resolve_target(vm: &Vm, vh: u32) -> Option<VhTarget> {
    if vh == NULL {
        return None;
    }
    let cid = vm.class_of_object(vh)?;
    let cdesc = vm.class_desc(cid).to_string();

    // `varType` is the accessed value type (component type for arrays).
    let var_type = vm.get_instance_field_by_name(vh, "varType")?;
    let var_cid = vm.class_id_of_class_object(var_type)?;
    let var_desc = vm.class_desc(var_cid).to_string();

    if cdesc == "Ljava/lang/invoke/StaticFieldVarHandle;" {
        // `StaticFieldVarHandle` extends `FieldVarHandle`, so it also carries
        // the `artField` offset; the declaring class is its own field because
        // the base `VarHandle(Class, boolean)` constructor leaves the
        // coordinate types null.
        let dc = vm.get_instance_field_by_name(vh, "declaringClass")?;
        let class = vm.class_id_of_class_object(dc)?;
        let art = vm.get_instance_field_by_name(vh, "artField")? as i32;
        let slot = field_slot(art)?;
        return Some(VhTarget::Static {
            class,
            slot,
            desc: var_desc,
        });
    }
    if cdesc == "Ljava/lang/invoke/FieldVarHandle;" {
        let dc = vm.get_instance_field_by_name(vh, "coordinateType0")?;
        let class = vm.class_id_of_class_object(dc)?;
        let art = vm.get_instance_field_by_name(vh, "artField")? as i32;
        let slot = field_slot(art)?;
        return Some(VhTarget::Instance {
            class,
            slot,
            desc: var_desc,
        });
    }
    if cdesc == "Ljava/lang/invoke/ArrayElementVarHandle;" {
        return Some(VhTarget::Array {
            elem_desc: var_desc,
        });
    }
    None
}

fn field_slot(art_field: i32) -> Option<usize> {
    let slot = (art_field - FIELD_BASE) / 4;
    if slot < 0 {
        None
    } else {
        Some(slot as usize)
    }
}

#[inline]
fn wide(desc: &str) -> bool {
    matches!(desc.as_bytes().first(), Some(b'J') | Some(b'D'))
}

#[inline]
fn is_ref(desc: &str) -> bool {
    TypeKind::from_descriptor(desc).is_ref()
}

/// Narrow an incoming register value to the target's storage width.
#[inline]
fn narrow(desc: &str, bits: u64) -> u64 {
    match desc.as_bytes().first().copied() {
        Some(b'Z') => bits & 1,
        Some(b'B') => bits & 0xff,
        Some(b'C') | Some(b'S') => bits & 0xffff,
        Some(b'I') | Some(b'F') => bits & 0xffff_ffff,
        _ => bits,
    }
}

/// Decode a stored value into the register representation (sign-extend the
/// sub-word integer kinds, keep float/double/int/long bits).
#[inline]
fn decode(desc: &str, bits: u64) -> u64 {
    match FieldSign::of_desc(desc) {
        FieldSign::Byte => (bits as u8 as i8 as i64) as u64,
        FieldSign::Short => (bits as u16 as i16 as i64) as u64,
        FieldSign::Char => (bits as u16) as u64,
        FieldSign::Bool => bits & 1,
        FieldSign::None => bits,
    }
}

fn array_data_error(vm: &mut Vm, index: i32) -> VmError {
    vm.throw_new(
        "Ljava/lang/ArrayIndexOutOfBoundsException;",
        Some(&format!("index {index}")),
    )
}

fn read_raw(vm: &mut Vm, target: &VhTarget, obj: u32, index: i32) -> VmResult<u64> {
    match target {
        VhTarget::Instance { slot, desc, .. } => {
            if obj == NULL {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            }
            let (lo, hi) = match vm.heap.get(obj) {
                Some(Object::Instance { fields, .. }) => (
                    fields.get(*slot).copied().unwrap_or(0),
                    fields.get(*slot + 1).copied().unwrap_or(0),
                ),
                _ => (0, 0),
            };
            let bits = if wide(desc) {
                (lo as u64) | ((hi as u64) << 32)
            } else {
                lo as u64
            };
            Ok(decode(desc, bits))
        }
        VhTarget::Static { class, slot, desc } => {
            let st = &vm.classes[*class as usize].statics;
            let lo = st.get(*slot).copied().unwrap_or(0);
            let hi = st.get(*slot + 1).copied().unwrap_or(0);
            let bits = if wide(desc) {
                (lo as u64) | ((hi as u64) << 32)
            } else {
                lo as u64
            };
            Ok(decode(desc, bits))
        }
        VhTarget::Array { elem_desc } => {
            if obj == NULL {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            }
            let Some(Object::Array(a)) = vm.heap.get(obj) else {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            };
            let i = index as usize;
            let n = a.data.len();
            if index < 0 || i >= n {
                return Err(array_data_error(vm, index));
            }
            let bits = match &a.data {
                ArrayData::Bool(v) => v[i] as u64,
                ArrayData::Byte(v) => v[i] as u8 as u64,
                ArrayData::Char(v) => v[i] as u64,
                ArrayData::Short(v) => v[i] as u16 as u64,
                ArrayData::Int(v) => v[i] as u32 as u64,
                ArrayData::Long(v) => v[i] as u64,
                ArrayData::Float(v) => v[i].to_bits() as u64,
                ArrayData::Double(v) => v[i].to_bits(),
                ArrayData::Ref(v) => v[i] as u64,
            };
            let _ = elem_desc;
            Ok(decode(elem_desc, bits))
        }
    }
}

fn write_raw(vm: &mut Vm, target: &VhTarget, obj: u32, index: i32, bits: u64) -> VmResult<()> {
    let bits = narrow(target.desc(), bits);
    match target {
        VhTarget::Instance { slot, desc, .. } => {
            if obj == NULL {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            }
            let wide = wide(desc);
            let Some(Object::Instance { fields, .. }) = vm.heap.get_mut(obj) else {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            };
            if *slot >= fields.len() {
                return Err(VmError::Internal(
                    "VarHandle field slot out of range".into(),
                ));
            }
            fields[*slot] = bits as u32;
            if wide {
                if *slot + 1 >= fields.len() {
                    return Err(VmError::Internal("VarHandle long slot out of range".into()));
                }
                fields[*slot + 1] = (bits >> 32) as u32;
            }
            Ok(())
        }
        VhTarget::Static { class, slot, desc } => {
            let wide = wide(desc);
            let st = &mut vm.classes[*class as usize].statics;
            if *slot >= st.len() {
                return Err(VmError::Internal(
                    "VarHandle static slot out of range".into(),
                ));
            }
            st[*slot] = bits as u32;
            if wide {
                if *slot + 1 >= st.len() {
                    return Err(VmError::Internal(
                        "VarHandle static long slot out of range".into(),
                    ));
                }
                st[*slot + 1] = (bits >> 32) as u32;
            }
            Ok(())
        }
        VhTarget::Array { elem_desc } => {
            if obj == NULL {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            }
            let Some(Object::Array(a)) = vm.heap.get_mut(obj) else {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            };
            let i = index as usize;
            let n = a.data.len();
            if index < 0 || i >= n {
                return Err(array_data_error(vm, index));
            }
            match &mut a.data {
                ArrayData::Bool(v) => v[i] = (bits & 1) != 0,
                ArrayData::Byte(v) => v[i] = bits as u8 as i8,
                ArrayData::Char(v) => v[i] = bits as u16,
                ArrayData::Short(v) => v[i] = bits as u16 as i16,
                ArrayData::Int(v) => v[i] = bits as u32 as i32,
                ArrayData::Long(v) => v[i] = bits as i64,
                ArrayData::Float(v) => v[i] = f32::from_bits(bits as u32),
                ArrayData::Double(v) => v[i] = f64::from_bits(bits),
                ArrayData::Ref(v) => v[i] = bits as u32,
            }
            let _ = elem_desc;
            Ok(())
        }
    }
}

/// Perform one access mode on a resolved target.
///
/// `obj` is the first coordinate (receiver object for instance fields, array
/// for array element handles) and `index` the second (array index). `operand`
/// is the value operand and `expected` the compare value, both in register
/// encoding (handle word or primitive bits).
pub(crate) fn access(
    vm: &mut Vm,
    target: &VhTarget,
    mode: VhMode,
    obj: u32,
    index: i32,
    operand: u64,
    expected: u64,
) -> VmResult<VhOut> {
    let desc = target.desc().to_string();
    let ref_target = is_ref(&desc);
    let numeric_add = matches!(mode, VhMode::GetAndAdd);
    let bitwise = matches!(
        mode,
        VhMode::GetAndBitwiseAnd | VhMode::GetAndBitwiseOr | VhMode::GetAndBitwiseXor
    );

    if numeric_add && (ref_target || !matches!(desc.as_bytes()[0], b'I' | b'J')) {
        return Err(unsupported_mode(vm, target, mode));
    }
    if bitwise && (ref_target || !matches!(desc.as_bytes()[0], b'I' | b'J')) {
        return Err(unsupported_mode(vm, target, mode));
    }

    let old = match mode {
        VhMode::Get => return Ok(VhOut::Value(read_raw(vm, target, obj, index)?)),
        VhMode::Set => {
            write_raw(vm, target, obj, index, operand)?;
            return Ok(VhOut::Void);
        }
        _ => read_raw(vm, target, obj, index)?,
    };

    match mode {
        VhMode::GetAndSet => {
            write_raw(vm, target, obj, index, operand)?;
            Ok(VhOut::Value(old))
        }
        VhMode::CompareAndSet => {
            let ok = old == narrow(&desc, expected);
            if ok {
                write_raw(vm, target, obj, index, operand)?;
            }
            Ok(VhOut::Bool(ok))
        }
        VhMode::CompareAndExchange => {
            if old == narrow(&desc, expected) {
                write_raw(vm, target, obj, index, operand)?;
            }
            Ok(VhOut::Value(old))
        }
        VhMode::GetAndAdd => {
            let sum = old.wrapping_add(narrow(&desc, operand));
            write_raw(vm, target, obj, index, sum)?;
            Ok(VhOut::Value(old))
        }
        VhMode::GetAndBitwiseAnd => {
            write_raw(vm, target, obj, index, old & narrow(&desc, operand))?;
            Ok(VhOut::Value(old))
        }
        VhMode::GetAndBitwiseOr => {
            write_raw(vm, target, obj, index, old | narrow(&desc, operand))?;
            Ok(VhOut::Value(old))
        }
        VhMode::GetAndBitwiseXor => {
            write_raw(vm, target, obj, index, old ^ narrow(&desc, operand))?;
            Ok(VhOut::Value(old))
        }
        VhMode::Get | VhMode::Set => unreachable!(),
    }
}

fn unsupported_mode(vm: &Vm, target: &VhTarget, mode: VhMode) -> VmError {
    VmError::unsupported_bytecode(
        format!("invoke-polymorphic VarHandle {mode:?}"),
        format!("VarHandle target {}", target.label(vm)),
    )
}

/// The number of coordinate arguments a resolved target consumes.
pub(crate) fn coordinate_count(target: &VhTarget) -> usize {
    match target {
        VhTarget::Instance { .. } => 1,
        VhTarget::Static { .. } => 0,
        VhTarget::Array { .. } => 2,
    }
}

/// Box a primitive value as the boxed wrapper class (the inverse of the
/// interpreter's `unbox_primitive`), used when a signature-polymorphic call
/// site returns `Object` for a primitive `VarHandle` access.
pub(crate) fn box_value(vm: &mut Vm, kind: char, raw: u64) -> VmResult<u32> {
    super::fwk_annotations::box_primitive(vm, kind, raw)
}

/// The kind of `VarHandle` behind a receiver, for error reporting.
pub(crate) fn varhandle_class(vm: &Vm, vh: u32) -> String {
    vm.class_of_object(vh)
        .map(|c| vm.class_desc(c).to_string())
        .unwrap_or_else(|| "<not a VarHandle>".to_string())
}
