//! `java.lang.Enum` builtins.

use acos_dex::access;

use crate::class::NativeCtx;
use crate::corelib::{lang, Vm};
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, Object, NULL};

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/lang/Enum;",
        access::PUBLIC | access::ABSTRACT,
        Some("Ljava/lang/Object;"),
        &["Ljava/lang/Comparable;", "Ljava/io/Serializable;"],
    )?;
    vm.add_instance_field(id, "name", "Ljava/lang/String;", access::PRIVATE | access::FINAL);
    vm.add_instance_field(id, "ordinal", "I", access::PRIVATE | access::FINAL);
    let p = access::PUBLIC;
    vm.add_native(id, "<init>", "(Ljava/lang/String;I)V", p, enum_init);
    vm.add_native(id, "name", "()Ljava/lang/String;", p, enum_name);
    vm.add_native(id, "ordinal", "()I", p, enum_ordinal);
    vm.add_native(id, "toString", "()Ljava/lang/String;", p, enum_name);
    vm.add_native(
        id,
        "compareTo",
        "(Ljava/lang/Enum;)I",
        p,
        enum_compare_to,
    );
    vm.add_native(
        id,
        "compareTo",
        "(Ljava/lang/Object;)I",
        p,
        enum_compare_to_obj,
    );
    vm.add_native(id, "equals", "(Ljava/lang/Object;)Z", p, enum_equals);
    vm.add_native(id, "hashCode", "()I", p, enum_hash);
    vm.add_native(
        id,
        "getDeclaringClass",
        "()Ljava/lang/Class;",
        p,
        enum_declaring_class,
    );
    vm.add_native(
        id,
        "valueOf",
        "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;",
        access::PUBLIC | access::STATIC,
        enum_value_of,
    );
    vm.link_class(id)?;
    Ok(())
}

fn enum_field(vm: &Vm, this: crate::heap::Handle, name: &str) -> crate::heap::Handle {
    let class = match vm.class_of_object(this) {
        Some(c) => c,
        None => return NULL,
    };
    let mut c = Some(class);
    while let Some(cur) = c {
        if let Some(f) = vm.classes[cur as usize]
            .declared_instance_fields
            .iter()
            .find(|f| f.name == name)
        {
            if let Some(Object::Instance { fields, .. }) = vm.heap.get(this) {
                return fields[f.offset as usize];
            }
        }
        c = vm.classes[cur as usize].super_class;
    }
    NULL
}

fn enum_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let name = vm.arg_ref(ctx, 0);
    let ordinal = vm.arg_i32(ctx, 1);
    lang::set_instance_field(vm, this, "name", name);
    lang::set_instance_field(vm, this, "ordinal", ordinal as u32);
    Ok(())
}

fn enum_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_ref(ctx, enum_field(vm, this, "name"));
    Ok(())
}

fn enum_ordinal(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_i32(ctx, enum_field(vm, this, "ordinal") as i32);
    Ok(())
}

fn enum_compare_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    let a = enum_field(vm, this, "ordinal") as i32;
    let b = enum_field(vm, other, "ordinal") as i32;
    vm.ret_i32(ctx, a - b);
    Ok(())
}

fn enum_compare_to_obj(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    enum_compare_to(vm, ctx)
}

fn enum_equals(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    vm.ret_bool(ctx, this == other);
    Ok(())
}

fn enum_hash(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_i32(ctx, this as i32);
    Ok(())
}

fn enum_declaring_class(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let class = vm
        .class_of_object(this)
        .ok_or_else(|| VmError::Internal("enum without class".into()))?;
    let mut declaring = class;
    if let Some(sup) = vm.classes[class as usize].super_class {
        if vm.classes[sup as usize].access & access::ENUM != 0 {
            declaring = sup;
        }
    }
    let h = vm.class_object(declaring)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn enum_value_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let class_obj = vm.arg_ref(ctx, 0);
    let name = vm.arg_ref(ctx, 1);
    if class_obj == NULL || name == NULL {
        return Err(vm.throw_npe("Enum.valueOf"));
    }
    let cid = match vm.heap.get(class_obj) {
        Some(Object::Class(c)) => *c,
        _ => return Err(vm.throw_new("Ljava/lang/IllegalArgumentException;", Some("not a class"))),
    };
    // Find $VALUES in the enum class.
    let values = vm.classes[cid as usize]
        .declared_static_fields
        .iter()
        .find(|f| f.name == "$VALUES")
        .map(|f| (f.offset as usize, f.is_ref));
    let values = match values {
        Some((off, true)) => vm.classes[cid as usize].statics[off],
        _ => NULL,
    };
    let target = vm.string_to_rust(name).unwrap_or_default();
    if values != NULL {
        if let Some(Object::Array(a)) = vm.heap.get(values) {
            let len = a.len();
            for i in 0..len {
                let e = match vm.heap.get(values) {
                    Some(Object::Array(a)) => match &a.data {
                        ArrayData::Ref(v) => v[i],
                        _ => NULL,
                    },
                    _ => NULL,
                };
                let en = enum_field(vm, e, "name");
                if vm.string_to_rust(en).as_deref() == Some(target.as_str()) {
                    vm.ret_ref(ctx, e);
                    return Ok(());
                }
            }
        }
    }
    let msg = format!("No enum constant {}.{}", vm.class_desc_of_id(cid), target);
    Err(vm.throw_new("Ljava/lang/IllegalArgumentException;", Some(&msg)))
}
