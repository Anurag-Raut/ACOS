//! `java.lang.reflect.Proxy` support.
//!
//! `Proxy.generateProxy(String, Class[], ClassLoader, Method[], Class[][])` is
//! native in ART and synthesises a class that implements the requested
//! interfaces. ACOS builds the same class in the VM: it extends the real
//! `java.lang.reflect.Proxy` (so `Proxy.isProxyClass`/`getInvocationHandler`
//! work), implements every interface method and the three `Object` methods, and
//! dispatches each invocation to the invocation handler stored in `Proxy.h`.
//!
//! This is what `libcore.reflect.AnnotationFactory.createAnnotation` needs, and
//! it also makes hand-written `Proxy.newProxyInstance` use real.

use std::collections::HashSet;

use crate::class::{ClassId, MethodId, NativeCtx, TypeKind};
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::{InvokeKind, Vm};

use super::fwk_annotations::{
    box_primitive, class_array_descs, get_long_field, make_method, unbox_primitive,
};

const PROXY: &str = "Ljava/lang/reflect/Proxy;";
const HANDLER_SIG: &str = "(Ljava/lang/Object;Ljava/lang/reflect/Method;[Ljava/lang/Object;)Ljava/lang/Object;";

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    vm.register_native(
        PROXY,
        "generateProxy",
        "(Ljava/lang/String;[Ljava/lang/Class;Ljava/lang/ClassLoader;[Ljava/lang/reflect/Method;[[Ljava/lang/Class;)Ljava/lang/Class;",
        generate_proxy,
    );
    // Override the `fwk_reflect` stub, which rejects non-empty constructor
    // arguments; proxies are constructed with their `InvocationHandler`.
    vm.register_native(
        "Ljava/lang/reflect/Constructor;",
        "newInstance0",
        "([Ljava/lang/Object;)Ljava/lang/Object;",
        ctor_new_instance0,
    );
    Ok(())
}

fn ref_array(vm: &mut Vm, elem_desc: &str, elems: Vec<Handle>) -> VmResult<Handle> {
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: elem_desc.to_string(),
        data: ArrayData::Ref(elems),
    }))
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

/// `Proxy.generateProxy(...)`.
fn generate_proxy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    let iface_handles = array_refs(vm, vm.arg_ref(ctx, 1));
    let mut ifaces = Vec::new();
    for h in iface_handles {
        if let Some(cid) = vm.class_id_of_class_object(h) {
            ifaces.push(cid);
        }
    }
    let desc = format!("L{};", name.replace('.', "/"));
    if let Ok(existing) = vm.load_class(&desc) {
        let obj = vm.class_object(existing)?;
        vm.ret_ref(ctx, obj);
        return Ok(());
    }

    let proxy_sup = vm.load_class(PROXY)?;
    let id = vm.add_class(crate::class::ClassSpec {
        descriptor: desc.clone(),
        access: acos_dex::access::PUBLIC | acos_dex::access::FINAL,
        super_class: Some(proxy_sup),
        interfaces: ifaces,
        is_interface: false,
        is_builtin: true,
    });

    // `Proxy.<init>(InvocationHandler)`.
    vm.add_native(
        id,
        "<init>",
        "(Ljava/lang/reflect/InvocationHandler;)V",
        acos_dex::access::PUBLIC | acos_dex::access::CONSTRUCTOR,
        proxy_init,
    );

    // Every interface method, plus the `Object` contract methods.
    let mut methods = Vec::new();
    let mut seen = HashSet::new();
    for &iface in &vm.classes[id as usize].interfaces {
        collect_interface_methods(vm, iface, &mut methods, &mut seen);
    }
    for (name, sig) in [
        ("toString", "()Ljava/lang/String;"),
        ("hashCode", "()I"),
        ("equals", "(Ljava/lang/Object;)Z"),
    ] {
        let key = (name.to_string(), sig.to_string());
        if seen.insert(key) {
            methods.push((name.to_string(), sig.to_string()));
        }
    }
    for (name, sig) in methods {
        vm.add_native(id, &name, &sig, acos_dex::access::PUBLIC, proxy_dispatch);
    }

    vm.link_class(id)?;
    let obj = vm.class_object(id)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn collect_interface_methods(
    vm: &Vm,
    iface: ClassId,
    out: &mut Vec<(String, String)>,
    seen: &mut HashSet<(String, String)>,
) {
    for &mid in &vm.classes[iface as usize].declared_methods {
        let m = &vm.methods[mid as usize];
        if m.name == "<clinit>" || m.name == "<init>" {
            continue;
        }
        if m.access & acos_dex::access::STATIC != 0 {
            continue;
        }
        let key = (m.name.clone(), m.sig.clone());
        if seen.insert(key) {
            out.push((m.name.clone(), m.sig.clone()));
        }
    }
    for &sup in &vm.classes[iface as usize].interfaces {
        collect_interface_methods(vm, sup, out, seen);
    }
}

/// `Proxy.<init>(InvocationHandler)` — store the handler in `Proxy.h`.
fn proxy_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let handler = vm.arg_ref(ctx, 0);
    vm.set_instance_field_by_name(this, "h", handler);
    Ok(())
}

/// Dispatch one generated proxy method to the invocation handler.
fn proxy_dispatch(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let mid = vm.ctx_method_id(ctx);
    let (name, sig, params, ret_desc, ret_kind) = {
        let m = &vm.methods[mid as usize];
        (
            m.name.clone(),
            m.sig.clone(),
            m.params.clone(),
            vm.type_desc(m.ret).to_string(),
            vm.type_kind(m.ret),
        )
    };
    let Some(proxy_cid) = vm.class_of_object(this) else {
        return Err(vm.throw_new("Ljava/lang/InternalError;", Some("proxy receiver")));
    };
    let (decl_cid, target_mid) = find_proxy_target(vm, proxy_cid, &name, &sig)?;
    let method_h = make_method(vm, decl_cid, target_mid)?;
    vm.native_roots.push(method_h);
    let args_arr = build_args(vm, ctx, &params)?;
    vm.native_roots.push(args_arr);

    let handler = vm.get_instance_field_by_name(this, "h").unwrap_or(NULL);
    if handler == NULL {
        return Err(vm.throw_new(
            "Ljava/lang/NullPointerException;",
            Some("proxy invocation handler"),
        ));
    }
    let hcid = vm
        .class_of_object(handler)
        .ok_or_else(|| vm.throw_new("Ljava/lang/InternalError;", Some("handler class")))?;
    let invoke = vm.find_method(hcid, "invoke", HANDLER_SIG, InvokeKind::Interface)?;
    let result =
        crate::interp::call_method(vm, invoke, handler, &[this as u64, method_h as u64, args_arr as u64]);
    vm.native_roots.truncate(vm.native_roots.len().saturating_sub(2));

    let r = result? as u32;
    if ret_kind == TypeKind::Void {
        vm.ret_ref(ctx, NULL);
    } else if ret_kind.is_ref() {
        vm.ret_ref(ctx, r);
    } else {
        let c = ret_desc.chars().next().unwrap_or('I');
        let raw = unbox_primitive(vm, c, r)?;
        if ret_kind == TypeKind::Long || ret_kind == TypeKind::Double {
            vm.ret_i64(ctx, raw as i64);
        } else {
            vm.ret_i32(ctx, raw as u32 as i32);
        }
    }
    Ok(())
}

/// Find the interface (or `Object`) method a proxy method corresponds to.
fn find_proxy_target(
    vm: &mut Vm,
    proxy_cid: ClassId,
    name: &str,
    sig: &str,
) -> VmResult<(ClassId, MethodId)> {
    let ifaces = vm.classes[proxy_cid as usize].interfaces.clone();
    for iface in ifaces {
        if let Some(v) = find_interface_method(vm, iface, name, sig) {
            return Ok(v);
        }
    }
    let object = vm.load_class("Ljava/lang/Object;")?;
    let mid = vm.find_method(object, name, sig, InvokeKind::Virtual)?;
    Ok((object, mid))
}

fn find_interface_method(
    vm: &Vm,
    iface: ClassId,
    name: &str,
    sig: &str,
) -> Option<(ClassId, MethodId)> {
    for &mid in &vm.classes[iface as usize].declared_methods {
        let m = &vm.methods[mid as usize];
        if m.name == name && m.sig == sig {
            return Some((iface, mid));
        }
    }
    for &sup in &vm.classes[iface as usize].interfaces {
        if let Some(v) = find_interface_method(vm, sup, name, sig) {
            return Some(v);
        }
    }
    None
}

/// Build the `Object[]` argument array for the handler from the frame regs.
fn build_args(vm: &mut Vm, ctx: NativeCtx, params: &[crate::class::TypeId]) -> VmResult<Handle> {
    let base = vm.native_roots.len();
    let mut elems = Vec::new();
    for (i, &tid) in params.iter().enumerate() {
        let kind = vm.type_kind(tid);
        let desc = vm.type_desc(tid).to_string();
        let h = if kind.is_ref() {
            vm.arg_ref(ctx, i)
        } else {
            let c = desc.chars().next().unwrap_or('I');
            let raw = if kind == TypeKind::Long || kind == TypeKind::Double {
                vm.arg_u64(ctx, i)
            } else {
                vm.arg_u32(ctx, i) as u64
            };
            box_primitive(vm, c, raw)?
        };
        vm.native_roots.push(h);
        elems.push(h);
    }
    let arr = ref_array(vm, "Ljava/lang/Object;", elems)?;
    vm.native_roots.truncate(base);
    Ok(arr)
}

/// `Constructor.newInstance0(Object[])` (overrides the empty-args-only stub).
fn ctor_new_instance0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let args_arr = vm.arg_ref(ctx, 0);
    let dc = vm.get_instance_field_by_name(this, "declaringClass").unwrap_or(NULL);
    let Some(cid) = vm.class_id_of_class_object(dc) else {
        return Err(vm.throw_new("Ljava/lang/InstantiationException;", None));
    };
    let mid = get_long_field(vm, this, "artMethod").unwrap_or(0) as MethodId;
    if mid == 0 || mid as usize >= vm.methods.len() {
        return Err(vm.throw_new("Ljava/lang/InstantiationException;", None));
    }
    let params = vm.methods[mid as usize].params.clone();
    let args = array_refs(vm, args_arr);
    if args.len() != params.len() {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("wrong number of arguments"),
        ));
    }
    let mut raw = Vec::with_capacity(params.len());
    for (i, &tid) in params.iter().enumerate() {
        let kind = vm.type_kind(tid);
        let desc = vm.type_desc(tid).to_string();
        if kind.is_ref() {
            raw.push(args[i] as u64);
        } else {
            let c = desc.chars().next().unwrap_or('I');
            raw.push(unbox_primitive(vm, c, args[i])?);
        }
    }
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let result = crate::interp::call_method(vm, mid, obj, &raw);
    vm.native_roots.pop();
    result?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

// Keep the DEX class-descriptor helper reachable for the annotation module.
#[allow(dead_code)]
fn _class_descs(vm: &Vm, arr: Handle) -> Vec<String> {
    class_array_descs(vm, arr)
}

#[allow(dead_code)]
fn _unsupported() -> VmError {
    VmError::unsupported_api("proxy", "acos_vm::natives::fwk_proxy")
}
