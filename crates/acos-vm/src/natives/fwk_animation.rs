//! `android.animation.PropertyValuesHolder` native surface (API 34).
//!
//! `PropertyValuesHolder` resolves a property setter to a `jmethodID` through
//! `nGet*Method` and later invokes it through `nCall*Method`, bypassing
//! `java.lang.reflect.Method`. ART implements this in
//! `frameworks/base/core/jni/android_animation_PropertyValuesHolder.cpp`:
//! `nGetFloatMethod` builds the fixed `(F)V` signature, `nGetIntMethod` `(I)V`,
//! and the `nGetMultiple*Method` variants build `(T…T)V` with `argCount`
//! parameters (`T` = `F`/`I`). A lookup that fails raises
//! `java.lang.NoSuchMethodError`, which the framework catches and answers by
//! falling back to reflection.
//!
//! ACOS has no raw `ArtMethod*` to hand out, so `nGet*Method` returns an opaque
//! `jlong` carrying the VM `MethodId` behind [`METHOD_HANDLE_TAG`]; the
//! `nCall*Method` natives decode it and invoke the real DEX method through
//! `interp::call_method`, exactly as `java.lang.reflect.Method.invoke` does.
//! The handle is only ever stored in `PropertyValuesHolder.mJniSetter`
//! (`long`) and passed straight back, so no other code observes its encoding.
//!
//! The native list is the API-34 boot DEX: `PropertyValuesHolder` declares
//! exactly the twelve methods bound here. (Legacy methods such as
//! `nSetFloatValues`/`nInitializeIntProperty` no longer exist in Android 14.)

use crate::class::{MethodId, NativeCtx};
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, Handle};
use crate::vm::{InvokeKind, Vm};

/// Class that declares the natives.
const PVH: &str = "Landroid/animation/PropertyValuesHolder;";

/// High bit set on every handle we return so a raw `MethodId` (including 0)
/// can never be mistaken for a real `jmethodID`, and a forged/stale value is
/// rejected instead of invoking an unrelated method.
const METHOD_HANDLE_TAG: u64 = 1 << 32;

fn encode_method(mid: MethodId) -> i64 {
    (METHOD_HANDLE_TAG | mid as u64) as i64
}

fn decode_method(handle: i64) -> Option<MethodId> {
    let h = handle as u64;
    if h & METHOD_HANDLE_TAG == 0 {
        return None;
    }
    Some((h & 0xffff_ffff) as MethodId)
}

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    vm.register_native(PVH, "nGetFloatMethod", "(Ljava/lang/Class;Ljava/lang/String;)J", n_get_float_method);
    vm.register_native(PVH, "nGetIntMethod", "(Ljava/lang/Class;Ljava/lang/String;)J", n_get_int_method);
    vm.register_native(
        PVH,
        "nGetMultipleFloatMethod",
        "(Ljava/lang/Class;Ljava/lang/String;I)J",
        n_get_multiple_float_method,
    );
    vm.register_native(
        PVH,
        "nGetMultipleIntMethod",
        "(Ljava/lang/Class;Ljava/lang/String;I)J",
        n_get_multiple_int_method,
    );

    vm.register_native(PVH, "nCallFloatMethod", "(Ljava/lang/Object;JF)V", n_call_float_method);
    vm.register_native(PVH, "nCallIntMethod", "(Ljava/lang/Object;JI)V", n_call_int_method);
    vm.register_native(PVH, "nCallTwoFloatMethod", "(Ljava/lang/Object;JFF)V", n_call_two_float_method);
    vm.register_native(PVH, "nCallTwoIntMethod", "(Ljava/lang/Object;JII)V", n_call_two_int_method);
    vm.register_native(
        PVH,
        "nCallFourFloatMethod",
        "(Ljava/lang/Object;JFFFF)V",
        n_call_four_float_method,
    );
    vm.register_native(
        PVH,
        "nCallFourIntMethod",
        "(Ljava/lang/Object;JIIII)V",
        n_call_four_int_method,
    );
    vm.register_native(
        PVH,
        "nCallMultipleFloatMethod",
        "(Ljava/lang/Object;J[F)V",
        n_call_multiple_float_method,
    );
    vm.register_native(
        PVH,
        "nCallMultipleIntMethod",
        "(Ljava/lang/Object;J[I)V",
        n_call_multiple_int_method,
    );
    Ok(())
}

/// ART's `getMethod` builds a fixed-arity `(T…T)V` signature from `argCount`.
fn fixed_signature(arg_count: i32, ty: char) -> String {
    let n = arg_count.max(0) as usize;
    let mut sig = String::with_capacity(n + 3);
    sig.push('(');
    for _ in 0..n {
        sig.push(ty);
    }
    sig.push_str(")V");
    sig
}

/// Shared body of the four `nGet*Method` natives.
///
/// `arg_count` is 1 for the scalar variants and the framework-supplied
/// parameter count for the multiple variants.
fn get_method(vm: &mut Vm, ctx: NativeCtx, arg_count: i32, ty: char) -> VmResult<()> {
    let class_obj = vm.arg_ref(ctx, 0);
    let name_obj = vm.arg_ref(ctx, 1);
    let name = match vm.string_to_rust(name_obj) {
        Some(n) => n,
        None => {
            return Err(vm.throw_new(
                "Ljava/lang/NullPointerException;",
                Some("PropertyValuesHolder: method name is null"),
            ))
        }
    };
    let Some(cid) = vm.class_id_of_class_object(class_obj) else {
        return Err(vm.throw_new(
            "Ljava/lang/NullPointerException;",
            Some("PropertyValuesHolder: targetClass is not a java.lang.Class"),
        ));
    };
    let sig = fixed_signature(arg_count, ty);
    match vm.find_method(cid, &name, &sig, InvokeKind::Virtual) {
        Ok(mid) => {
            vm.ret_i64(ctx, encode_method(mid));
            Ok(())
        }
        Err(VmError::UnsupportedApi { .. }) => {
            let class_desc = vm.class_desc(cid).to_string();
            let dotted = crate::vm::descriptor_to_dotted(&class_desc);
            let msg = format!("{dotted}.{name}{sig}");
            Err(vm.throw_new("Ljava/lang/NoSuchMethodError;", Some(&msg)))
        }
        Err(e) => Err(e),
    }
}

fn n_get_float_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    get_method(vm, ctx, 1, 'F')
}

fn n_get_int_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    get_method(vm, ctx, 1, 'I')
}

fn n_get_multiple_float_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arg_count = vm.arg_i32(ctx, 2);
    get_method(vm, ctx, arg_count, 'F')
}

fn n_get_multiple_int_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arg_count = vm.arg_i32(ctx, 2);
    get_method(vm, ctx, arg_count, 'I')
}

/// Resolve `(target, handle)` and run the setter with the given arguments.
fn call_method(vm: &mut Vm, ctx: NativeCtx, args: &[u64]) -> VmResult<()> {
    let target: Handle = vm.arg_ref(ctx, 0);
    let handle = vm.arg_i64(ctx, 1);
    let Some(mid) = decode_method(handle) else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalStateException;",
            Some("PropertyValuesHolder: invalid JNI setter handle"),
        ));
    };
    crate::interp::call_method(vm, mid, target, args)?;
    Ok(())
}

fn n_call_float_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f32(ctx, 2);
    call_method(vm, ctx, &[v.to_bits() as u64])
}

fn n_call_int_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 2);
    call_method(vm, ctx, &[v as u32 as u64])
}

fn n_call_two_float_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_f32(ctx, 2);
    let b = vm.arg_f32(ctx, 3);
    call_method(vm, ctx, &[a.to_bits() as u64, b.to_bits() as u64])
}

fn n_call_two_int_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_i32(ctx, 2);
    let b = vm.arg_i32(ctx, 3);
    call_method(vm, ctx, &[a as u32 as u64, b as u32 as u64])
}

fn n_call_four_float_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_f32(ctx, 2);
    let b = vm.arg_f32(ctx, 3);
    let c = vm.arg_f32(ctx, 4);
    let d = vm.arg_f32(ctx, 5);
    call_method(
        vm,
        ctx,
        &[
            a.to_bits() as u64,
            b.to_bits() as u64,
            c.to_bits() as u64,
            d.to_bits() as u64,
        ],
    )
}

fn n_call_four_int_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_i32(ctx, 2);
    let b = vm.arg_i32(ctx, 3);
    let c = vm.arg_i32(ctx, 4);
    let d = vm.arg_i32(ctx, 5);
    call_method(
        vm,
        ctx,
        &[
            a as u32 as u64,
            b as u32 as u64,
            c as u32 as u64,
            d as u32 as u64,
        ],
    )
}

fn n_call_multiple_float_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arr = vm.arg_ref(ctx, 2);
    let values = match vm.heap.get(arr) {
        Some(crate::heap::Object::Array(a)) => match &a.data {
            ArrayData::Float(v) => v.clone(),
            _ => return Err(vm.throw_new("Ljava/lang/IllegalArgumentException;", Some("expected float[]"))),
        },
        _ => return Err(vm.throw_new("Ljava/lang/NullPointerException;", Some("values is null"))),
    };
    let args: Vec<u64> = values.iter().map(|f| f.to_bits() as u64).collect();
    call_method(vm, ctx, &args)
}

fn n_call_multiple_int_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arr = vm.arg_ref(ctx, 2);
    let values = match vm.heap.get(arr) {
        Some(crate::heap::Object::Array(a)) => match &a.data {
            ArrayData::Int(v) => v.clone(),
            _ => return Err(vm.throw_new("Ljava/lang/IllegalArgumentException;", Some("expected int[]"))),
        },
        _ => return Err(vm.throw_new("Ljava/lang/NullPointerException;", Some("values is null"))),
    };
    let args: Vec<u64> = values.iter().map(|&v| v as u32 as u64).collect();
    call_method(vm, ctx, &args)
}
