//! `dalvik.system.VMRuntime`, `dalvik.system.VMStack` support and `Unsafe`
//! shims needed by core-library class initialisation.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::heap::{new_array_data, ArrayData, ArrayObj, Object, NULL};
use crate::vm::Vm;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Ldalvik/system/VMRuntime;";
    vm.register_native(c, "getNotifyNativeInterval", "()I", ret_notify_interval);
    vm.register_native(c, "stopHeapTaskProcessor", "()V", noop);
    vm.register_native(c, "startHeapTaskProcessor", "()V", noop);
    vm.register_native(c, "runHeapTasks", "()V", noop);
    vm.register_native(c, "trimHeap", "()V", noop);
    vm.register_native(c, "requestConcurrentGC", "()V", noop);
    vm.register_native(c, "requestHeapTrim", "()V", noop);
    vm.register_native(c, "resetJitCounters", "()V", noop);
    vm.register_native(c, "clampGrowthLimit", "()V", noop);
    vm.register_native(c, "clearGrowthLimit", "()V", noop);
    vm.register_native(c, "notifyStartupCompleted", "()V", noop);
    vm.register_native(c, "notifyNativeAllocationsInternal", "()V", noop);
    vm.register_native(c, "registerNativeAllocation", "(J)V", noop_long);
    vm.register_native(c, "registerNativeFree", "(J)V", noop_long);
    vm.register_native(c, "registerSensitiveThread", "()V", noop);
    vm.register_native(c, "setSystemDaemonThreadPriority", "()V", noop);
    vm.register_native(c, "setDedupeHiddenApiWarnings", "(Z)V", noop);
    vm.register_native(c, "setHiddenApiExemptions", "([Ljava/lang/String;)V", noop);
    vm.register_native(c, "setHiddenApiAccessLogSamplingRate", "(I)V", noop);
    vm.register_native(c, "setDisabledCompatChangesNative", "([J)V", noop);
    vm.register_native(c, "setProcessPackageName", "(Ljava/lang/String;)V", noop);
    vm.register_native(c, "setProcessDataDirectory", "(Ljava/lang/String;)V", noop);
    vm.register_native(c, "setTargetSdkVersionNative", "(I)V", noop);
    vm.register_native(c, "nativeSetTargetHeapUtilization", "(F)V", noop);
    vm.register_native(c, "registerAppInfo", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;I)V", noop);
    vm.register_native(c, "preloadDexCaches", "()V", noop);
    vm.register_native(c, "updateProcessState", "(I)V", noop);
    vm.register_native(c, "bootCompleted", "()V", noop);
    vm.register_native(c, "is64Bit", "()Z", ret_true);
    vm.register_native(c, "isCheckJniEnabled", "()Z", ret_false);
    vm.register_native(c, "isJavaDebuggable", "()Z", ret_false);
    vm.register_native(c, "isNativeDebuggable", "()Z", ret_false);
    vm.register_native(c, "isBootClassPathOnDisk", "(Ljava/lang/String;)Z", ret_true);
    vm.register_native(c, "isValidClassLoaderContext", "(Ljava/lang/String;)Z", ret_true);
    vm.register_native(c, "getTargetHeapUtilization", "()F", ret_half_f);
    vm.register_native(c, "getFinalizerTimeoutMs", "()J", ret_10000_l);
    vm.register_native(c, "getSdkVersionNative", "(I)I", ret_sdk);
    vm.register_native(c, "getCurrentInstructionSet", "()Ljava/lang/String;", ret_x86_64);
    vm.register_native(c, "vmInstructionSet", "()Ljava/lang/String;", ret_x86_64);
    vm.register_native(c, "vmVersion", "()Ljava/lang/String;", ret_null_str);
    vm.register_native(c, "vmLibrary", "()Ljava/lang/String;", ret_null_str);
    vm.register_native(c, "bootClassPath", "()Ljava/lang/String;", ret_empty_str);
    vm.register_native(c, "classPath", "()Ljava/lang/String;", ret_empty_str);
    vm.register_native(c, "properties", "()[Ljava/lang/String;", empty_string_array);
    vm.register_native(c, "getBaseApkOptimizationInfo", "()Ldalvik/system/DexFile$OptimizationInfo;", ret_null);
    vm.register_native(c, "newUnpaddedArray", "(Ljava/lang/Class;I)Ljava/lang/Object;", new_array_of);
    vm.register_native(c, "newNonMovableArray", "(Ljava/lang/Class;I)Ljava/lang/Object;", new_array_of);
    vm.register_native(c, "addressOf", "(Ljava/lang/Object;)J", address_of);
    atomic_integer(vm);
    Ok(())
}

/// `java.util.concurrent.atomic.AtomicInteger`.
///
/// Its real `<clinit>` builds a `VarHandle` through `MethodHandles.lookup()`,
/// which pulls in the whole reflection/VarHandle substrate. The VM implements
/// the class's storage directly instead (the field is a plain `int`).
fn atomic_integer(vm: &mut Vm) {
    let c = "Ljava/util/concurrent/atomic/AtomicInteger;";
    vm.register_native(c, "<clinit>", "()V", ai_noop);
    vm.register_native(c, "get", "()I", ai_get);
    vm.register_native(c, "set", "(I)V", ai_set);
    vm.register_native(c, "lazySet", "(I)V", ai_set);
    vm.register_native(c, "getAndSet", "(I)I", ai_get_and_set);
    vm.register_native(c, "compareAndSet", "(II)Z", ai_cas);
    vm.register_native(c, "weakCompareAndSet", "(II)Z", ai_cas);
    vm.register_native(c, "weakCompareAndSetPlain", "(II)Z", ai_cas);
    vm.register_native(c, "weakCompareAndSetVolatile", "(II)Z", ai_cas);
    vm.register_native(c, "weakCompareAndSetAcquire", "(II)Z", ai_cas);
    vm.register_native(c, "weakCompareAndSetRelease", "(II)Z", ai_cas);
    vm.register_native(c, "getAndAdd", "(I)I", ai_get_and_add);
    vm.register_native(c, "addAndGet", "(I)I", ai_add_and_get);
    vm.register_native(c, "getAndIncrement", "()I", ai_get_and_inc);
    vm.register_native(c, "getAndDecrement", "()I", ai_get_and_dec);
    vm.register_native(c, "incrementAndGet", "()I", ai_inc_and_get);
    vm.register_native(c, "decrementAndGet", "()I", ai_dec_and_get);
    vm.register_native(c, "getPlain", "()I", ai_get);
    vm.register_native(c, "setPlain", "(I)V", ai_set);
    vm.register_native(c, "getOpaque", "()I", ai_get);
    vm.register_native(c, "setOpaque", "(I)V", ai_set);
    vm.register_native(c, "getAcquire", "()I", ai_get);
    vm.register_native(c, "setRelease", "(I)V", ai_set);
}

fn ai_value(vm: &Vm, this: crate::heap::Handle) -> i32 {
    vm.get_instance_field_by_name(this, "value").unwrap_or(0) as i32
}

fn ai_noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn ai_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = ai_value(vm, this);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn ai_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_i32(ctx, 0);
    vm.set_instance_field_by_name(this, "value", v as u32);
    Ok(())
}

fn ai_get_and_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let nv = vm.arg_i32(ctx, 0);
    let old = ai_value(vm, this);
    vm.set_instance_field_by_name(this, "value", nv as u32);
    vm.ret_i32(ctx, old);
    Ok(())
}

fn ai_cas(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let expect = vm.arg_i32(ctx, 0);
    let update = vm.arg_i32(ctx, 1);
    let old = ai_value(vm, this);
    let ok = old == expect;
    if ok {
        vm.set_instance_field_by_name(this, "value", update as u32);
    }
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn ai_get_and_add(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let delta = vm.arg_i32(ctx, 0);
    let old = ai_value(vm, this);
    vm.set_instance_field_by_name(this, "value", old.wrapping_add(delta) as u32);
    vm.ret_i32(ctx, old);
    Ok(())
}

fn ai_add_and_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let delta = vm.arg_i32(ctx, 0);
    let nv = ai_value(vm, this).wrapping_add(delta);
    vm.set_instance_field_by_name(this, "value", nv as u32);
    vm.ret_i32(ctx, nv);
    Ok(())
}

fn ai_get_and_inc(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let old = ai_value(vm, this);
    vm.set_instance_field_by_name(this, "value", old.wrapping_add(1) as u32);
    vm.ret_i32(ctx, old);
    Ok(())
}

fn ai_get_and_dec(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let old = ai_value(vm, this);
    vm.set_instance_field_by_name(this, "value", old.wrapping_sub(1) as u32);
    vm.ret_i32(ctx, old);
    Ok(())
}

fn ai_inc_and_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let nv = ai_value(vm, this).wrapping_add(1);
    vm.set_instance_field_by_name(this, "value", nv as u32);
    vm.ret_i32(ctx, nv);
    Ok(())
}

fn ai_dec_and_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let nv = ai_value(vm, this).wrapping_sub(1);
    vm.set_instance_field_by_name(this, "value", nv as u32);
    vm.ret_i32(ctx, nv);
    Ok(())
}

fn noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn noop_long(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}


fn ret_notify_interval(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, 300);
    Ok(())
}

fn ret_true(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, true);
    Ok(())
}

fn ret_false(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

fn ret_half_f(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f32(ctx, 0.5);
    Ok(())
}

fn ret_10000_l(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 10_000);
    Ok(())
}

fn ret_sdk(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    vm.ret_i32(ctx, if v <= 0 { 34 } else { v });
    Ok(())
}

fn ret_x86_64(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.new_string("x86_64")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn ret_null_str(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.new_string("")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn ret_empty_str(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.new_string("")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn ret_null(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_ref(ctx, NULL);
    Ok(())
}

fn empty_string_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/String;".to_string(),
        data: ArrayData::Ref(Vec::new()),
    }))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn new_array_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let class_obj = vm.arg_ref(ctx, 0);
    let len = vm.arg_i32(ctx, 1).max(0) as usize;
    let desc = vm
        .class_id_of_class_object(class_obj)
        .map(|c| vm.class_desc(c).to_string())
        .unwrap_or_else(|| "Ljava/lang/Object;".to_string());
    let data = new_array_data(&desc, len)?;
    let h = vm.alloc(Object::Array(ArrayObj { elem_desc: desc, data }))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn address_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    vm.ret_i64(ctx, obj as i64);
    Ok(())
}

// Keep these referenced for future array converters.
const _: fn() = || {
    let _ = ArrayData::Int(Vec::new());
};
