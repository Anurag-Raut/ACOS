//! Further `handleLaunchActivity` / `handleBindApplication` natives found by
//! driving the real M2 chain.
//!
//! These are genuine framework call sites on the path from
//! `ApplicationThread.bindApplication` to `MainActivity.onCreate`. Nothing here
//! fakes a lifecycle callback.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::heap::NULL;
use crate::vm::Vm;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    vm.register_native(
        "Landroid/net/NetworkUtils;",
        "getBoundNetworkHandleForProcess",
        "()J",
        get_bound_network_handle_for_process,
    );
    vm.register_native(
        "Ljava/lang/reflect/Field;",
        "getAnnotationNative",
        "(Ljava/lang/Class;)Ljava/lang/annotation/Annotation;",
        field_get_annotation_native,
    );
    Ok(())
}

/// `NetworkUtils.getBoundNetworkHandleForProcess()`.
///
/// `handleBindApplication` asks for the process default proxy
/// (`ConnectivityManager.getDefaultProxy` -> `getBoundNetworkForProcess` ->
/// `getProcessDefaultNetwork` -> this native). On a device the native returns
/// the handle of the network the process was bound to, or `0` when nothing was
/// bound with `bindProcessToNetwork`. ACOS has no network stack and never binds
/// a process, so `0` is the real state, which makes `getProcessDefaultNetwork`
/// return `null` exactly like an unbound device process. It is not a faked
/// "online" answer.
fn get_bound_network_handle_for_process(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

/// `Field.getAnnotationNative(Class<? extends Annotation>)`.
///
/// `PhoneWindow.<init>` -> `Settings.Global.<clinit>` ->
/// `Settings.getPublicSettingsForClass` asks each field for its annotations.
/// ACOS does not parse annotation metadata (the same declared divergence as
/// `natives::reflect`'s `Class.getDeclaredAnnotation`, which returns `null`), so
/// the honest empty answer is `null` — no annotation is reported as present.
/// This does not fabricate an annotation value.
fn field_get_annotation_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_ref(ctx, NULL);
    Ok(())
}
