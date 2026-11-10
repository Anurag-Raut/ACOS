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

use crate::class::{ClassId, FieldDef, NativeCtx};
use crate::error::VmResult;
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::Vm;

/// Base of the ACOS object header, mirrored from `natives::reflect::FIELD_BASE`.
const FIELD_BASE: i32 = 8;

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
fn make_field(vm: &mut Vm, cid: ClassId, def: &FieldDef, idx: usize) -> VmResult<Handle> {
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
