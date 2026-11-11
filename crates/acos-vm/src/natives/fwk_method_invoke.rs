//! `java.lang.reflect.Method.invoke` (declared `native` in the API-34 boot
//! DEX) and the `Executable` accessors the animation/reflection paths need.
//!
//! ART implements `Method.invoke` in `java_lang_reflect_Method.cc`: it checks
//! the receiver against `static`/instance binding, unboxes the `Object[]`
//! arguments according to the method's parameter types and dispatches through
//! `InvokeWithArgArray`. ACOS runs the real DEX body of the target method
//! through `interp::call_method`, with the same unbox/box contract
//! `fwk_proxy`'s proxy dispatch and `fwk_reflect`'s constructor already use.
//!
//! The `Method` mirror layout is the one `fwk_annotations`/`fwk_reflect`
//! build: `artMethod` carries the ACOS `MethodId` behind
//! [`super::fwk_annotations::mirror_method_id`].

use crate::class::{NativeCtx, TypeKind};
use crate::error::VmResult;
use crate::heap::{ArrayData, Handle, Object, NULL};
use crate::vm::Vm;

use super::fwk_annotations::{box_primitive, mirror_method_id, unbox_primitive};

const METHOD: &str = "Ljava/lang/reflect/Method;";

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    vm.register_native(
        METHOD,
        "invoke",
        "(Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/Object;",
        method_invoke,
    );
    Ok(())
}

fn array_refs(vm: &Vm, arr: Handle) -> Vec<Handle> {
    match vm.heap.get(arr) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Ref(v) => v.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn method_invoke(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm
        .ctx_this(ctx)
        .ok_or_else(|| vm.throw_new("Ljava/lang/NullPointerException;", None))?;
    let obj = vm.arg_ref(ctx, 0);
    let args_arr = vm.arg_ref(ctx, 1);

    let Some(mid) = mirror_method_id(vm, this) else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("Method.artMethod is not a valid method"),
        ));
    };
    let (params, ret, is_static, name, sig) = {
        let m = &vm.methods[mid as usize];
        (
            m.params.clone(),
            m.ret,
            m.is_static(),
            m.name.clone(),
            m.sig.clone(),
        )
    };
    let args = array_refs(vm, args_arr);
    if args.len() != params.len() {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("wrong number of arguments"),
        ));
    }

    let receiver: Handle = if is_static {
        NULL
    } else {
        if obj == NULL {
            return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
        }
        obj
    };

    // `Method.invoke` must use virtual dispatch: `Class.getMethods()` returns
    // the *declaring* method, so an inherited abstract declaration (e.g.
    // `NativeTimingSpec.createTimer`) would otherwise be invoked directly and
    // throw `AbstractMethodError` instead of reaching the concrete override on
    // the receiver's class (W157-A). Private/static methods are not in the
    // virtual map, so `dispatch` falls back to the declaring method.
    let effective = if is_static {
        mid
    } else {
        vm.dispatch(receiver, &name, &sig).unwrap_or(mid)
    };

    let mut raw = Vec::with_capacity(params.len());
    for (i, &tid) in params.iter().enumerate() {
        let kind = vm.type_kind(tid);
        if kind.is_ref() {
            raw.push(args[i] as u64);
        } else {
            let desc = vm.type_desc(tid).to_string();
            let c = desc.chars().next().unwrap_or('I');
            raw.push(unbox_primitive(vm, c, args[i])?);
        }
    }

    let result = crate::interp::call_method(vm, effective, receiver, &raw)?;

    let rk = vm.type_kind(ret);
    if rk == TypeKind::Void {
        vm.ret_ref(ctx, NULL);
    } else if rk.is_ref() {
        vm.ret_ref(ctx, result as u32);
    } else {
        let desc = vm.type_desc(ret).to_string();
        let c = desc.chars().next().unwrap_or('I');
        let h = box_primitive(vm, c, result)?;
        vm.ret_ref(ctx, h);
    }
    Ok(())
}
