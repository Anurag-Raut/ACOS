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
    vm.register_native(c, "properties", "()[Ljava/lang/String;", runtime_properties);
    vm.register_native(c, "getBaseApkOptimizationInfo", "()Ldalvik/system/DexFile$OptimizationInfo;", ret_null);
    vm.register_native(c, "newUnpaddedArray", "(Ljava/lang/Class;I)Ljava/lang/Object;", new_array_of);
    vm.register_native(c, "newNonMovableArray", "(Ljava/lang/Class;I)Ljava/lang/Object;", new_array_of);
    vm.register_native(c, "addressOf", "(Ljava/lang/Object;)J", address_of);
    atomic_integer(vm);
    atomic_boolean(vm);
    Ok(())
}

/// `java.util.concurrent.atomic.AtomicBoolean`.
///
/// Like `AtomicInteger`, the real `<clinit>` builds a `VarHandle` (and
/// `compareAndSet` is a signature-polymorphic `VarHandle` call, opcode
/// `invoke-polymorphic`). The VM implements the `int value` storage directly
/// instead; `CloseGuard`/`ImageDecoder.close` otherwise abort on the
/// unimplemented opcode.
fn atomic_boolean(vm: &mut Vm) {
    let c = "Ljava/util/concurrent/atomic/AtomicBoolean;";
    vm.register_native(c, "<clinit>", "()V", ai_noop);
    vm.register_native(c, "get", "()Z", ab_get);
    vm.register_native(c, "set", "(Z)V", ab_set);
    vm.register_native(c, "lazySet", "(Z)V", ab_set);
    vm.register_native(c, "getAndSet", "(Z)Z", ab_get_and_set);
    vm.register_native(c, "compareAndSet", "(ZZ)Z", ab_cas);
    vm.register_native(c, "weakCompareAndSet", "(ZZ)Z", ab_cas);
    vm.register_native(c, "weakCompareAndSetPlain", "(ZZ)Z", ab_cas);
    vm.register_native(c, "weakCompareAndSetVolatile", "(ZZ)Z", ab_cas);
    vm.register_native(c, "weakCompareAndSetAcquire", "(ZZ)Z", ab_cas);
    vm.register_native(c, "weakCompareAndSetRelease", "(ZZ)Z", ab_cas);
    vm.register_native(c, "getPlain", "()Z", ab_get);
    vm.register_native(c, "setPlain", "(Z)V", ab_set);
    vm.register_native(c, "getOpaque", "()Z", ab_get);
    vm.register_native(c, "setOpaque", "(Z)V", ab_set);
    vm.register_native(c, "getAcquire", "()Z", ab_get);
    vm.register_native(c, "setRelease", "(Z)V", ab_set);
}

fn ab_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_bool(ctx, ai_value(vm, this) != 0);
    Ok(())
}

fn ab_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_bool(ctx, 0) as i32;
    vm.set_instance_field_by_name(this, "value", v as u32);
    Ok(())
}

fn ab_get_and_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let nv = vm.arg_bool(ctx, 0);
    let old = ai_value(vm, this) != 0;
    vm.set_instance_field_by_name(this, "value", nv as i32 as u32);
    vm.ret_bool(ctx, old);
    Ok(())
}

fn ab_cas(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let expect = vm.arg_bool(ctx, 0);
    let update = vm.arg_bool(ctx, 1);
    let old = ai_value(vm, this) != 0;
    let ok = old == expect;
    if ok {
        vm.set_instance_field_by_name(this, "value", update as i32 as u32);
    }
    vm.ret_bool(ctx, ok);
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

    atomic_long(vm);
}

/// `java.util.concurrent.atomic.AtomicLong`.
///
/// Same trick as `AtomicInteger`: the real `<clinit>` builds a `VarHandle`, so
/// ACOS implements the class's `long` storage directly. Needed because
/// `MemoryIntArray`/`CloseGuard` pulls `AtomicLong` in through `UUID`.
fn atomic_long(vm: &mut Vm) {
    let c = "Ljava/util/concurrent/atomic/AtomicLong;";
    vm.register_native(c, "<clinit>", "()V", ai_noop);
    vm.register_native(c, "VMSupportsCS8", "()Z", vm_supports_cs8);
    vm.register_native(c, "get", "()J", al_get);
    vm.register_native(c, "set", "(J)V", al_set);
    vm.register_native(c, "lazySet", "(J)V", al_set);
    vm.register_native(c, "getAndSet", "(J)J", al_get_and_set);
    vm.register_native(c, "compareAndSet", "(JJ)Z", al_cas);
    vm.register_native(c, "weakCompareAndSet", "(JJ)Z", al_cas);
    vm.register_native(c, "weakCompareAndSetPlain", "(JJ)Z", al_cas);
    vm.register_native(c, "weakCompareAndSetVolatile", "(JJ)Z", al_cas);
    vm.register_native(c, "weakCompareAndSetAcquire", "(JJ)Z", al_cas);
    vm.register_native(c, "weakCompareAndSetRelease", "(JJ)Z", al_cas);
    vm.register_native(c, "getAndAdd", "(J)J", al_get_and_add);
    vm.register_native(c, "addAndGet", "(J)J", al_add_and_get);
    vm.register_native(c, "getAndIncrement", "()J", al_get_and_inc);
    vm.register_native(c, "getAndDecrement", "()J", al_get_and_dec);
    vm.register_native(c, "incrementAndGet", "()J", al_inc_and_get);
    vm.register_native(c, "decrementAndGet", "()J", al_dec_and_get);
    vm.register_native(c, "getPlain", "()J", al_get);
    vm.register_native(c, "setPlain", "(J)V", al_set);
    vm.register_native(c, "getOpaque", "()J", al_get);
    vm.register_native(c, "setOpaque", "(J)V", al_set);
    vm.register_native(c, "getAcquire", "()J", al_get);
    vm.register_native(c, "setRelease", "(J)V", al_set);
}

fn vm_supports_cs8(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, true);
    Ok(())
}

fn al_value(vm: &mut Vm, this: crate::heap::Handle) -> i64 {
    match vm.get_field(this, "value") {
        Ok(crate::javabridge::JavaValue::Long(v)) => v,
        _ => 0,
    }
}

fn al_store(vm: &mut Vm, this: crate::heap::Handle, v: i64) {
    let _ = vm.set_field(this, "value", crate::javabridge::JavaValue::Long(v));
}

fn al_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = al_value(vm, this);
    vm.ret_i64(ctx, v);
    Ok(())
}

fn al_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_i64(ctx, 0);
    al_store(vm, this, v);
    Ok(())
}

fn al_get_and_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let nv = vm.arg_i64(ctx, 0);
    let old = al_value(vm, this);
    al_store(vm, this, nv);
    vm.ret_i64(ctx, old);
    Ok(())
}

fn al_cas(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let expect = vm.arg_i64(ctx, 0);
    let update = vm.arg_i64(ctx, 1);
    let old = al_value(vm, this);
    let ok = old == expect;
    if ok {
        al_store(vm, this, update);
    }
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn al_get_and_add(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let delta = vm.arg_i64(ctx, 0);
    let old = al_value(vm, this);
    al_store(vm, this, old.wrapping_add(delta));
    vm.ret_i64(ctx, old);
    Ok(())
}

fn al_add_and_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let delta = vm.arg_i64(ctx, 0);
    let nv = al_value(vm, this).wrapping_add(delta);
    al_store(vm, this, nv);
    vm.ret_i64(ctx, nv);
    Ok(())
}

fn al_get_and_inc(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let old = al_value(vm, this);
    al_store(vm, this, old.wrapping_add(1));
    vm.ret_i64(ctx, old);
    Ok(())
}

fn al_get_and_dec(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let old = al_value(vm, this);
    al_store(vm, this, old.wrapping_sub(1));
    vm.ret_i64(ctx, old);
    Ok(())
}

fn al_inc_and_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let nv = al_value(vm, this).wrapping_add(1);
    al_store(vm, this, nv);
    vm.ret_i64(ctx, nv);
    Ok(())
}

fn al_dec_and_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let nv = al_value(vm, this).wrapping_sub(1);
    al_store(vm, this, nv);
    vm.ret_i64(ctx, nv);
    Ok(())
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

/// `VMRuntime.properties()` — the `-Dkey=value` assignments the embedded
/// runtime hands to `java.lang.System.<clinit>`.
///
/// ART's `AndroidRuntime::start` sets `user.dir=/` (the zygote chdirs to `/`),
/// which `jdk.internal.util.StaticProperty` and `java.io.UnixFileSystem`
/// require to be present (they throw `InternalError` otherwise). It also
/// passes the linker library search path as `java.library.path`; API 34's
/// `LoadedApk.createOrUpdateClassLoaderLocked` reads it unconditionally
/// (`System.getProperty("java.library.path").contains("/vendor/lib")`), so a
/// `null` value is a fatal NPE during `handleBindApplication`. The other Java
/// properties (`java.home`, `user.home`, `user.name`, `java.io.tmpdir`,
/// `os.*`, ICU versions) are derived by
/// `System.initUnchangeableSystemProperties` from `Libcore.os`, `ICU` and the
/// hardcoded table, so they are not repeated here.
fn runtime_properties(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // A non-null library search path that does not contain `/vendor/lib`, the
    // same shape a real app process gets from the linker namespace. ACOS loads
    // native libraries through the extracted boot cache, not this path.
    let entries = ["user.dir=/", "java.library.path=/system/lib64:/system/lib"];
    let mut refs = Vec::with_capacity(entries.len());
    // Root each string: `vm.alloc` below may trigger a (stress) GC and the
    // handles in `refs` are not roots yet.
    for e in entries {
        let h = vm.new_string(e)?;
        vm.native_roots.push(h);
        refs.push(h);
    }
    let h = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/String;".to_string(),
        data: ArrayData::Ref(refs),
    }))?;
    vm.ret_ref(ctx, h);
    for _ in entries {
        vm.native_roots.pop();
    }
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
    // A live synthetic address that aliases the array's backing store. Direct
    // byte buffers are `newNonMovableArray(byte.class, n)` plus `addressOf`,
    // then read/written through `libcore.io.Memory`; the alias keeps those
    // accesses pointed at the real heap array.
    vm.ret_i64(ctx, super::native_mem::array_address(obj));
    Ok(())
}

// Keep these referenced for future array converters.
const _: fn() = || {
    let _ = ArrayData::Int(Vec::new());
};
