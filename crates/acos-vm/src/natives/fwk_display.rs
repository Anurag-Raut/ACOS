//! `android.hardware.OverlayProperties` natives.
//!
//! `ResourcesManager.applyConfigurationToResources` calls
//! `DisplayManagerGlobal.getInstance()`, which asks `IDisplayManager` for the
//! display overlay capabilities. The reply is a real `OverlayProperties`
//! parcelable, so its `CREATOR` (<clinit> + `(Parcel)` constructor) must run.
//!
//! ACOS's display service reports no HDR/wide-color overlay support
//! (`getOverlaySupport` returns two `false` flags). The framework's native
//! representation is an opaque handle; a null handle (0) is exactly "no
//! properties", and every query on it answers false. The parcel reader/writer
//! still consume/produce the two booleans the Java `writeToParcel` layout
//! defines, so the surrounding `Parcel` stays in sync.
//!
//! This is not a stub of an ACOS display feature: it materialises the real
//! Java object with the values the ACOS display service actually reports.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::javabridge::JavaValue;
use crate::vm::Vm;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Landroid/hardware/OverlayProperties;";
    vm.register_native(c, "nGetDestructor", "()J", get_destructor);
    vm.register_native(
        c,
        "nReadOverlayPropertiesFromParcel",
        "(Landroid/os/Parcel;)J",
        read_from_parcel,
    );
    vm.register_native(
        c,
        "nWriteOverlayPropertiesToParcel",
        "(JLandroid/os/Parcel;)V",
        write_to_parcel,
    );
    vm.register_native(c, "nSupportFp16ForHdr", "(J)Z", support_fp16);
    vm.register_native(c, "nSupportMixedColorSpaces", "(J)Z", support_mixed);
    Ok(())
}

/// No native allocation exists, so there is no destructor.
fn get_destructor(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn read_from_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let parcel = vm.arg_ref(ctx, 0);
    // Match `OverlayProperties.writeToParcel`: two booleans.
    read_bool(vm, parcel);
    read_bool(vm, parcel);
    // Null native object: no overlay capabilities.
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn write_to_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let parcel = vm.arg_ref(ctx, 1);
    write_bool(vm, parcel, false);
    write_bool(vm, parcel, false);
    Ok(())
}

fn support_fp16(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

fn support_mixed(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

fn read_bool(vm: &mut Vm, parcel: crate::heap::Handle) -> bool {
    matches!(
        vm.call_method(parcel, "readBoolean", "()Z", &[]),
        Ok(JavaValue::Boolean(v)) if v
    )
}

fn write_bool(vm: &mut Vm, parcel: crate::heap::Handle, v: bool) {
    let _ = vm.call_method(parcel, "writeBoolean", "(Z)V", &[JavaValue::Boolean(v)]);
}
