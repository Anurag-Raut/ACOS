//! Framework graphics configuration natives accepted before rendering exists.
//!
//! `ActivityThread.handleBindApplication` calls
//! `android.graphics.Compatibility.setTargetSdkVersion`, whose only action is
//! `Canvas.setCompatibilityVersion` → `Canvas.nSetCompatibilityVersion(int)`.
//! It records the API level the native canvas must scale for. ACOS has no
//! rasteriser yet (`acos-render` is Rust software 2D, ADR-0011), so there is no
//! canvas state to configure; the value is recorded so a future renderer can
//! read it back, and no app API observes it.
//!
//! This is a declared divergence. Every drawing/measurement native stays a
//! structured `UNSUPPORTED_JAVA_API`; only this fire-and-forget configuration
//! setter is accepted, so the lifecycle can proceed without pretending a frame
//! was drawn.

use std::sync::atomic::{AtomicI32, Ordering};

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::vm::Vm;

/// The API level recorded by `Canvas.setCompatibilityVersion` (0 until set).
static COMPATIBILITY_VERSION: AtomicI32 = AtomicI32::new(0);

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    vm.register_native(
        "Landroid/graphics/Canvas;",
        "nSetCompatibilityVersion",
        "(I)V",
        canvas_set_compatibility_version,
    );
    // `android.graphics.Typeface` static support. The system font map itself is
    // built by real Java (`FontListParser.parse`) from `/system/etc/fonts.xml`;
    // these three only touch the native font cache, which ACOS does not have
    // (ADR-0011 is a Rust software rasteriser with no native Skia/Minikin).
    //
    // * `nativeGetReleaseFunc` returns the free-function pointer for native
    //   typeface objects. There are none, so `0` ("no finalizer") is correct.
    // * `preloadFontFile`/`nativeRegisterLocaleList` warm the native font cache;
    //   with no native font backend there is nothing to warm. No app API reads
    //   them back, so they are declared no-ops like the `DdmServer` discard.
    let t = "Landroid/graphics/Typeface;";
    vm.register_native(t, "nativeGetReleaseFunc", "()J", typeface_no_release_func);
    vm.register_native(t, "preloadFontFile", "(Ljava/lang/String;)V", typeface_preload_font);
    vm.register_native(
        t,
        "nativeRegisterLocaleList",
        "(Ljava/lang/String;)V",
        typeface_register_locale_list,
    );
    Ok(())
}

/// No native typeface objects exist, so there is no free function to return.
fn typeface_no_release_func(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

/// No native font cache to preload into.
fn typeface_preload_font(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

/// No native font fallback cache to register locales with.
fn typeface_register_locale_list(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

/// The compatibility API level last set by the framework (0 = unset).
#[must_use]
pub fn compatibility_version() -> i32 {
    COMPATIBILITY_VERSION.load(Ordering::Relaxed)
}

fn canvas_set_compatibility_version(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    COMPATIBILITY_VERSION.store(vm.arg_i32(ctx, 0), Ordering::Relaxed);
    Ok(())
}
