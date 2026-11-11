//! Framework-tier reflection natives that the console tier stubbed out.
//!
//! The M1 console programs only need `Class.getName`, primitive classes and
//! `newInstance`, so `natives::reflect` returns an **empty** array for
//! `Class.getDeclaredFields`. The framework boot path does not accept that:
//! `jdk.internal.misc.Unsafe.objectFieldOffset(Class, String)` (real DEX code)
//! scans `getDeclaredFields()` for a field by name and throws
//! `java.lang.InternalError` when it cannot find it. `ConcurrentHashMap.<clinit>`
//! is the first caller on the `ActivityThread` path, so this module supplies the
//! real declared-field arrays.
//!
//! The `Field` mirror layout is the one `natives::reflect` already reads back
//! (`declaringClass`, `type`, `accessFlags`, `artFieldIndex`, `offset`), so the
//! existing `Field.getNameInternal`/`get`/`set` natives keep working unchanged.
//! The offset encoding (`FIELD_BASE + slot*4`) is duplicated here to keep this
//! file independent of `reflect.rs` internals; if one changes, both must.

use crate::class::{ClassId, FieldDef, MethodId, NativeCtx};
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::{InvokeKind, Vm};

/// Base of the ACOS object header, mirrored from `natives::reflect::FIELD_BASE`.
const FIELD_BASE: i32 = 8;

/// `java.lang.reflect.Constructor` descriptor.
const CONSTRUCTOR: &str = "Ljava/lang/reflect/Constructor;";

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Ljava/lang/Class;";
    vm.register_native(
        c,
        "getDeclaredFields",
        "()[Ljava/lang/reflect/Field;",
        cl_get_declared_fields,
    );
    vm.register_native(
        c,
        "getDeclaredFieldsUnchecked",
        "(Z)[Ljava/lang/reflect/Field;",
        cl_get_declared_fields_unchecked,
    );
    vm.register_native(
        c,
        "getPublicDeclaredFields",
        "()[Ljava/lang/reflect/Field;",
        cl_get_public_declared_fields,
    );
    // Constructor reflection for the `java.security` provider instantiation
    // path (`sun.security.jca.GetInstance` → `Provider$Service.newInstance` →
    // `Class.getConstructor(...).newInstance(...)`). `natives::reflect`
    // returns `null` for the internal lookup; these supply the real mirror.
    vm.register_native(
        c,
        "getDeclaredConstructorInternal",
        "([Ljava/lang/Class;)Ljava/lang/reflect/Constructor;",
        cl_get_declared_constructor,
    );
    vm.register_native(
        CONSTRUCTOR,
        "newInstance0",
        "([Ljava/lang/Object;)Ljava/lang/Object;",
        ctor_new_instance0,
    );
    Ok(())
}

fn cl_get_declared_fields(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    declared_fields(vm, ctx, false)
}

fn cl_get_declared_fields_unchecked(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let public_only = vm.arg_bool(ctx, 0);
    declared_fields(vm, ctx, public_only)
}

fn cl_get_public_declared_fields(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    declared_fields(vm, ctx, true)
}

/// Build the `Field[]` for the class mirror in `ctx.this`.
///
/// Order matches Android's `Class.getDeclaredFields`: instance fields first,
/// then static fields, each in declaration order. `public_only` filters on the
/// `PUBLIC` access flag exactly like ART's `getDeclaredFieldsUnchecked`.
fn declared_fields(vm: &mut Vm, ctx: NativeCtx, public_only: bool) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let Some(cid) = vm.class_id_of_class_object(this) else {
        // A primitive/array mirror has no DEX field table.
        let arr = empty_field_array(vm)?;
        vm.ret_ref(ctx, arr);
        return Ok(());
    };
    let (instance, statics) = {
        let c = &vm.classes[cid as usize];
        (
            c.declared_instance_fields.clone(),
            c.declared_static_fields.clone(),
        )
    };

    let mut roots: Vec<Handle> = Vec::new();
    let mut fields: Vec<Handle> = Vec::new();
    for (idx, def) in instance.iter().enumerate() {
        if public_only && def.access & acos_dex::access::PUBLIC == 0 {
            continue;
        }
        let h = make_field(vm, cid, def, idx)?;
        vm.native_roots.push(h);
        roots.push(h);
        fields.push(h);
    }
    for (idx, def) in statics.iter().enumerate() {
        if public_only && def.access & acos_dex::access::PUBLIC == 0 {
            continue;
        }
        let h = make_field(vm, cid, def, idx)?;
        vm.native_roots.push(h);
        roots.push(h);
        fields.push(h);
    }

    let arr = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/reflect/Field;".to_string(),
        data: ArrayData::Ref(fields),
    }))?;
    for _ in roots {
        vm.native_roots.pop();
    }
    vm.ret_ref(ctx, arr);
    Ok(())
}

fn empty_field_array(vm: &mut Vm) -> VmResult<Handle> {
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/reflect/Field;".to_string(),
        data: ArrayData::Ref(Vec::new()),
    }))
}

/// Materialise one `java.lang.reflect.Field` exactly like `natives::reflect`.
pub(crate) fn make_field(vm: &mut Vm, cid: ClassId, def: &FieldDef, idx: usize) -> VmResult<Handle> {
    let field_class = vm.load_class("Ljava/lang/reflect/Field;")?;
    let obj = vm.alloc_instance(field_class)?;
    vm.native_roots.push(obj);

    let dc = vm.class_object(cid)?;
    vm.native_roots.push(dc);
    let type_cid = vm.load_class(&def.desc)?;
    let ty = vm.class_object(type_cid)?;
    vm.native_roots.push(ty);

    let offset = FIELD_BASE + def.offset as i32 * 4;
    vm.set_instance_field_by_name(obj, "declaringClass", dc);
    vm.set_instance_field_by_name(obj, "type", ty);
    vm.set_instance_field_by_name(obj, "accessFlags", def.access);
    vm.set_instance_field_by_name(obj, "artFieldIndex", idx as u32);
    vm.set_instance_field_by_name(obj, "offset", offset as u32);

    vm.native_roots.pop();
    vm.native_roots.pop();
    vm.native_roots.pop();
    Ok(obj)
}

// --- constructor reflection -------------------------------------------------

/// Build the `(params...)V` descriptor for the `Class[]` argument of
/// `Class.getDeclaredConstructorInternal`.
fn param_signature(vm: &Vm, arr: Handle) -> Option<String> {
    let elems = match vm.heap.get(arr) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Ref(v) => v.clone(),
            _ => return None,
        },
        _ => return None,
    };
    let mut sig = String::from("(");
    for h in elems {
        let cid = vm.class_id_of_class_object(h)?;
        sig.push_str(&vm.class_desc_of_id(cid));
    }
    sig.push_str(")V");
    Some(sig)
}

/// Write a `jlong` instance field.
fn set_long_field(vm: &mut Vm, obj: Handle, name: &str, value: i64) {
    if let Some(class) = vm.class_of_object(obj) {
        let mut c = Some(class);
        while let Some(cur) = c {
            let off = vm.classes[cur as usize]
                .declared_instance_fields
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.offset as usize);
            if let Some(off) = off {
                if let Some(Object::Instance { fields, .. }) = vm.heap.get_mut(obj) {
                    if off + 1 < fields.len() {
                        let v = value as u64;
                        fields[off] = v as u32;
                        fields[off + 1] = (v >> 32) as u32;
                    }
                }
                return;
            }
            c = vm.classes[cur as usize].super_class;
        }
    }
}

/// Read a `jlong` instance field.
fn get_long_field(vm: &Vm, obj: Handle, name: &str) -> Option<i64> {
    let class = vm.class_of_object(obj)?;
    let mut c = Some(class);
    while let Some(cur) = c {
        let off = vm.classes[cur as usize]
            .declared_instance_fields
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.offset as usize);
        if let Some(off) = off {
            if let Some(Object::Instance { fields, .. }) = vm.heap.get(obj) {
                let lo = *fields.get(off)? as u64;
                let hi = *fields.get(off + 1)? as u64;
                return Some((lo | (hi << 32)) as i64);
            }
            return None;
        }
        c = vm.classes[cur as usize].super_class;
    }
    None
}

/// Materialise a `java.lang.reflect.Constructor` mirror for `mid`.
fn make_constructor(vm: &mut Vm, cid: ClassId, mid: MethodId) -> VmResult<Handle> {
    let ctor_class = vm.load_class(CONSTRUCTOR)?;
    let obj = vm.alloc_instance(ctor_class)?;
    vm.native_roots.push(obj);
    let dc = vm.class_object(cid)?;
    vm.native_roots.push(dc);
    let access = vm.methods[mid as usize].access;
    vm.set_instance_field_by_name(obj, "declaringClass", dc);
    vm.set_instance_field_by_name(obj, "accessFlags", access);
    vm.set_instance_field_by_name(obj, "dexMethodIndex", mid);
    // `artMethod` carries the ACOS method id; `newInstance0` reads it back.
    set_long_field(vm, obj, "artMethod", mid as i64);
    vm.native_roots.pop();
    vm.native_roots.pop();
    Ok(obj)
}

/// `Class.getDeclaredConstructorInternal(Class[])`.
fn cl_get_declared_constructor(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let Some(cid) = vm.class_id_of_class_object(this) else {
        vm.ret_ref(ctx, NULL);
        return Ok(());
    };
    let arr = vm.arg_ref(ctx, 0);
    let Some(sig) = param_signature(vm, arr) else {
        vm.ret_ref(ctx, NULL);
        return Ok(());
    };
    match vm.find_method(cid, "<init>", &sig, InvokeKind::Direct) {
        Ok(mid) => {
            let obj = make_constructor(vm, cid, mid)?;
            vm.ret_ref(ctx, obj);
        }
        Err(_) => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

/// `Constructor.newInstance0(Object[])`.
///
/// ACOS supports the no-arg constructors the provider surface uses; a
/// non-empty argument list is a structured `UNSUPPORTED_JAVA_API` rather than
/// a silently-wrong invocation.
fn ctor_new_instance0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let args_arr = vm.arg_ref(ctx, 0);
    let arg_len = match vm.heap.get(args_arr) {
        Some(Object::Array(a)) => a.len(),
        _ => 0,
    };
    if arg_len != 0 {
        return Err(VmError::unsupported_api(
            "Constructor.newInstance0(non-empty args)",
            "acos_vm::natives::fwk_reflect",
        ));
    }
    let dc = vm.get_instance_field_by_name(this, "declaringClass").unwrap_or(NULL);
    let Some(cid) = vm.class_id_of_class_object(dc) else {
        return Err(vm.throw_new("Ljava/lang/InstantiationException;", None));
    };
    let mid = get_long_field(vm, this, "artMethod").unwrap_or(0) as MethodId;
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let result = crate::interp::call_method(vm, mid, obj, &[]);
    vm.native_roots.pop();
    result?;
    vm.ret_ref(ctx, obj);
    Ok(())
}
