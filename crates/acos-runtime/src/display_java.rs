//! Materialise the real framework `DisplayInfo` / `OverlayProperties` objects
//! for the `IDisplayManager` replies.
//!
//! # Why materialise instead of hand-roll the wire format
//!
//! API 34's `DisplayInfo.writeToParcel` writes 49 fields in an order that does
//! not resemble the small prefix the ACOS codec used to guess: `layerStack`,
//! `flags`, `type`, `displayId`, `displayGroupId` come *before* `name`, and the
//! object also carries a `Display.Mode[]` (length + each `Mode.writeToParcel`),
//! a `DisplayCutout` written through
//! `DisplayCutout$ParcelableWrapper.writeCutoutToParcel`, a
//! `SparseArray`, `long`s and `float`s. A hand-written table desynchronised the
//! `Parcel` and the framework reported `RuntimeException: bad array lengths`
//! while reading `getDisplayInfo`.
//!
//! The invariant in `research/service-layer/REPORT.md` §3.6 is: *the only code
//! that writes a parcelable's bytes is that parcelable's own
//! `writeToParcel`*. This module follows it: it builds the real Java object with
//! the fields the ACOS display service serves, then calls the framework's
//! `writeToParcel` into a real `Parcel` and copies the marshalled bytes into the
//! transport reply. `DisplayInfo.CREATOR` then reads exactly what
//! `writeToParcel` wrote — proved by
//! `crates/acos-runtime/tests/display.rs` through the real Java `CREATOR`.
//!
//! `OverlayProperties`'s native serialisation is the ACOS native in
//! `acos-vm/src/natives/fwk_display.rs`; the object is still constructed and
//! serialised by the real framework bytecode, never by a codec here.

use acos_parcel::Parcel;
use acos_services::display::DisplayInfo;
use acos_vm::heap::Handle;
use acos_vm::javabridge::JavaValue;
use acos_vm::Vm;

use crate::pms_java;

type JResult<T> = Result<T, String>;

/// True when this module owns the reply for `method`.
#[must_use]
pub fn owns_method(method: &str) -> bool {
    matches!(method, "getDisplayInfo" | "getOverlaySupport")
}

fn err(e: impl std::fmt::Display) -> String {
    format!("display_java: {e}")
}

/// Rewrite the reply for an `IDisplayManager` method that returns a real Java
/// parcelable. The caller has already dispatched the handler, so the typed
/// description is in the cache.
///
/// Returns `Ok(false)` when this module does not own the method (the generic
/// reply is left untouched).
pub fn rewrite_reply(vm: &mut Vm, method: &str, reply: &mut Parcel) -> JResult<bool> {
    if !owns_method(method) {
        return Ok(false);
    }
    // Reset the generic (prefix) reply; the Java object writes its own bytes.
    reply.reset();
    acos_services::exception::write_no_exception(reply);

    match method {
        "getDisplayInfo" => match acos_services::display::cached_display_info() {
            None => pms_java::typed_object(vm, None, reply)?,
            Some(info) => {
                let h = display_info(vm, &info)?;
                pms_java::typed_object(vm, Some(h), reply)?;
            }
        },
        "getOverlaySupport" => {
            let h = overlay_properties(vm)?;
            pms_java::typed_object(vm, Some(h), reply)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn set_int(vm: &mut Vm, obj: Handle, name: &str, value: i32) -> JResult<()> {
    vm.set_field(obj, name, JavaValue::Int(value)).map_err(err)
}

fn set_float(vm: &mut Vm, obj: Handle, name: &str, value: f32) -> JResult<()> {
    vm.set_field(obj, name, JavaValue::Float(value)).map_err(err)
}

fn set_long(vm: &mut Vm, obj: Handle, name: &str, value: i64) -> JResult<()> {
    vm.set_field(obj, name, JavaValue::Long(value)).map_err(err)
}

fn set_bool(vm: &mut Vm, obj: Handle, name: &str, value: bool) -> JResult<()> {
    vm.set_field(obj, name, JavaValue::Boolean(value)).map_err(err)
}

fn set_str(vm: &mut Vm, obj: Handle, name: &str, value: &str) -> JResult<()> {
    let h = vm.new_string_rooted(value).map_err(err)?;
    vm.set_field(obj, name, JavaValue::Object(h)).map_err(err)
}

/// `DisplayCutout.NO_CUTOUT` after running the class's `<clinit>`.
///
/// The no-arg `DisplayInfo()` leaves `displayCutout` null, which writes the
/// same `-1` marker as `NO_CUTOUT`; materialising the real constant keeps the
/// reference geometry (`NO_CUTOUT` = "no cutout present") explicit.
fn no_cutout(vm: &mut Vm) -> JResult<Handle> {
    let cid = vm.load_class("Landroid/view/DisplayCutout;").map_err(err)?;
    vm.initialize_class(cid).map_err(err)?;
    match vm
        .get_static_field("android.view.DisplayCutout", "NO_CUTOUT")
        .map_err(err)?
    {
        JavaValue::Object(h) => Ok(h),
        other => Err(format!(
            "display_java: DisplayCutout.NO_CUTOUT is {}, not an object",
            other.kind()
        )),
    }
}

/// Materialise a real `android.view.DisplayInfo` with the served fields.
///
/// Unset fields keep the no-arg constructor's defaults (`supportedColorModes`
/// `{COLOR_MODE_SRGB}`, empty `userDisabledHdrTypes`, empty
/// `thermalRefreshRateThrottling`, `hdrSdrRatio = NaN`, null `address` /
/// `deviceProductInfo` / `displayShape` / `hdrCapabilities` / `roundedCorners`).
fn display_info(vm: &mut Vm, info: &DisplayInfo) -> JResult<Handle> {
    let h = vm
        .new_instance_ctor("android.view.DisplayInfo", "()V", &[])
        .map_err(err)?;

    set_int(vm, h, "layerStack", 0)?;
    set_int(vm, h, "flags", info.flags)?;
    set_int(vm, h, "type", info.display_type)?;
    set_int(vm, h, "displayId", info.display_id)?;
    set_int(vm, h, "displayGroupId", 0)?;
    set_str(vm, h, "name", &info.name)?;
    set_int(vm, h, "appWidth", info.app_width)?;
    set_int(vm, h, "appHeight", info.app_height)?;
    // The nominal bounds equal the app bounds on a fixed virtual display.
    set_int(vm, h, "smallestNominalAppWidth", info.app_width)?;
    set_int(vm, h, "smallestNominalAppHeight", info.app_height)?;
    set_int(vm, h, "largestNominalAppWidth", info.app_width)?;
    set_int(vm, h, "largestNominalAppHeight", info.app_height)?;
    set_int(vm, h, "logicalWidth", info.logical_width)?;
    set_int(vm, h, "logicalHeight", info.logical_height)?;
    let cutout = no_cutout(vm)?;
    vm.set_field(h, "displayCutout", JavaValue::Object(cutout))
        .map_err(err)?;
    set_int(vm, h, "rotation", info.rotation)?;
    set_int(vm, h, "modeId", info.mode_id)?;
    set_float(vm, h, "renderFrameRate", info.refresh_rate)?;
    set_int(vm, h, "defaultModeId", info.mode_id)?;

    // supportedModes = [Mode(modeId, width, height, refreshRate)].
    let mode = vm
        .new_instance_ctor(
            "android.view.Display$Mode",
            "(IIIF)V",
            &[
                JavaValue::Int(info.mode_id),
                JavaValue::Int(info.logical_width),
                JavaValue::Int(info.logical_height),
                JavaValue::Float(info.refresh_rate),
            ],
        )
        .map_err(err)?;
    let modes = vm
        .new_array_empty("android.view.Display$Mode", 1)
        .map_err(err)?;
    vm.set_array_element(modes, 0, JavaValue::Object(mode))
        .map_err(err)?;
    vm.set_field(h, "supportedModes", JavaValue::Object(modes))
        .map_err(err)?;

    set_int(vm, h, "colorMode", info.color_mode)?;
    let colors = vm.new_array_empty("I", 1).map_err(err)?;
    vm.set_array_element(colors, 0, JavaValue::Int(info.color_mode))
        .map_err(err)?;
    vm.set_field(h, "supportedColorModes", JavaValue::Object(colors))
        .map_err(err)?;

    set_bool(vm, h, "minimalPostProcessingSupported", false)?;
    set_int(vm, h, "logicalDensityDpi", info.density_dpi)?;
    set_float(vm, h, "physicalXDpi", info.density_dpi as f32)?;
    set_float(vm, h, "physicalYDpi", info.density_dpi as f32)?;
    set_long(vm, h, "appVsyncOffsetNanos", 0)?;
    set_long(vm, h, "presentationDeadlineNanos", 0)?;
    set_int(vm, h, "state", info.state)?;
    set_int(vm, h, "committedState", info.state)?;
    set_int(vm, h, "ownerUid", crate::FIXTURE_UID as i32)?;
    set_int(vm, h, "removeMode", 0)?;
    set_float(vm, h, "refreshRateOverride", 0.0)?;
    set_float(vm, h, "brightnessMinimum", 0.0)?;
    set_float(vm, h, "brightnessMaximum", 1.0)?;
    set_float(vm, h, "brightnessDefault", 0.5)?;
    set_int(vm, h, "installOrientation", info.install_orientation)?;
    Ok(h)
}

/// Materialise a real `android.hardware.OverlayProperties`.
///
/// `mNativeObject = 1` is the ACOS "no overlay capabilities" handle: the ACOS
/// native writer serialises the two capability flags, and every query method
/// (`supportFp16ForHdr`, `supportMixedColorSpaces`) answers `false`.
fn overlay_properties(vm: &mut Vm) -> JResult<Handle> {
    let cid = vm
        .load_class("Landroid/hardware/OverlayProperties;")
        .map_err(err)?;
    vm.initialize_class(cid).map_err(err)?;
    vm.new_instance_ctor(
        "android.hardware.OverlayProperties",
        "(J)V",
        &[JavaValue::Long(1)],
    )
    .map_err(err)
}
