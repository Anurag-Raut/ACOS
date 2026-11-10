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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::heap::{ArrayData, Object};
use crate::vm::Vm;

/// Backing storage for one `android.graphics.Interpolator`.
///
/// The deprecated framework interpolator is a native object holding a matrix
/// of key-frame times (`mFrameCount`) by values (`mValueCount`). ACOS keeps the
/// same shape in Rust; `timeToValues` linearly interpolates between the two
/// bracketing key frames.
struct InterpState {
    value_count: usize,
    frame_count: usize,
    times: Vec<f32>,
    values: Vec<f32>,
    repeat: f32,
    mirror: bool,
}

thread_local! {
    static INTERPS: RefCell<HashMap<u64, InterpState>> = RefCell::new(HashMap::new());
    static NEXT_INTERP: Cell<u64> = const { Cell::new(1) };
}

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

    // The deprecated `android.graphics.Interpolator` still backs a few widget
    // animators. It owns a native key-frame table; register the full surface so
    // layouts that construct one do not stall on `UNSUPPORTED_JAVA_API`.
    let i = "Landroid/graphics/Interpolator;";
    vm.register_native(i, "nativeConstructor", "(II)J", ip_ctor);
    vm.register_native(i, "nativeDestructor", "(J)V", ip_dtor);
    vm.register_native(i, "nativeReset", "(JII)V", ip_reset);
    vm.register_native(i, "nativeSetKeyFrame", "(JII[F[F)V", ip_set_key_frame);
    vm.register_native(i, "nativeSetRepeatMirror", "(JFZ)V", ip_set_repeat_mirror);
    vm.register_native(i, "nativeTimeToValues", "(JI[F)I", ip_time_to_values);
    Ok(())
}

fn read_f32_array(vm: &Vm, h: u32) -> Vec<f32> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Float(v) => v.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn write_f32_array(vm: &mut Vm, h: u32, src: &[f32]) {
    if h == 0 {
        return;
    }
    if let Some(Object::Array(a)) = vm.heap.get_mut(h) {
        if let ArrayData::Float(v) = &mut a.data {
            for (i, value) in src.iter().enumerate() {
                if let Some(slot) = v.get_mut(i) {
                    *slot = *value;
                }
            }
        }
    }
}

fn ip_ctor(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let value_count = vm.arg_i32(ctx, 0).max(0) as usize;
    let frame_count = vm.arg_i32(ctx, 1).max(0) as usize;
    let handle = NEXT_INTERP.with(|n| {
        let v = n.get();
        n.set(v + 1);
        v
    });
    let state = InterpState {
        value_count,
        frame_count,
        times: vec![0.0; frame_count],
        values: vec![0.0; frame_count * value_count],
        repeat: 0.0,
        mirror: false,
    };
    INTERPS.with(|m| m.borrow_mut().insert(handle, state));
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn ip_dtor(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    INTERPS.with(|m| m.borrow_mut().remove(&handle));
    Ok(())
}

fn ip_reset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let value_index = vm.arg_i32(ctx, 1).max(0) as usize;
    let frame_index = vm.arg_i32(ctx, 2).max(0) as usize;
    INTERPS.with(|m| {
        if let Some(s) = m.borrow_mut().get_mut(&handle) {
            if frame_index < s.frame_count {
                s.times[frame_index] = 0.0;
                let base = frame_index * s.value_count + value_index;
                if base < s.values.len() {
                    s.values[base] = 0.0;
                }
            }
        }
    });
    Ok(())
}

fn ip_set_key_frame(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let frame_index = vm.arg_i32(ctx, 1).max(0) as usize;
    let msec = vm.arg_i32(ctx, 2);
    let values = read_f32_array(vm, vm.arg_ref(ctx, 3));
    INTERPS.with(|m| {
        if let Some(s) = m.borrow_mut().get_mut(&handle) {
            if frame_index < s.frame_count {
                s.times[frame_index] = msec as f32;
                let base = frame_index * s.value_count;
                for k in 0..s.value_count {
                    s.values[base + k] = values.get(k).copied().unwrap_or(0.0);
                }
            }
        }
    });
    Ok(())
}

fn ip_set_repeat_mirror(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let repeat = vm.arg_f32(ctx, 1);
    let mirror = vm.arg_bool(ctx, 2);
    INTERPS.with(|m| {
        if let Some(s) = m.borrow_mut().get_mut(&handle) {
            s.repeat = repeat;
            s.mirror = mirror;
        }
    });
    Ok(())
}

/// Interpolate the value vector at `msec`.
///
/// Returns the `Interpolator.Result` ordinal: `0` = `FREEZE_END`, `1` =
/// `FREEZE_START`, `2` = `NORMAL` (matches `Interpolator$Result` `$VALUES`
/// order); the `values` array is filled with the sampled vector in every case.
fn ip_time_to_values(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let msec = vm.arg_i32(ctx, 1) as f32;
    let out_handle = vm.arg_ref(ctx, 2);
    let Some((value_count, times, values)) = INTERPS.with(|m| {
        m.borrow().get(&handle).map(|s| {
            if s.value_count == 0 || s.frame_count == 0 {
                (0, Vec::new(), Vec::new())
            } else {
                (s.value_count, s.times.clone(), s.values.clone())
            }
        })
    }) else {
        vm.ret_i32(ctx, 0);
        return Ok(());
    };
    if value_count == 0 {
        vm.ret_i32(ctx, 0);
        return Ok(());
    }
    let frame_count = times.len();
    let mut out = vec![0.0f32; value_count];
    let result;
    if msec <= times[0] {
        result = 1; // FREEZE_START
        out.copy_from_slice(&values[0..value_count]);
    } else if msec >= times[frame_count - 1] {
        result = 0; // FREEZE_END
        let base = (frame_count - 1) * value_count;
        out.copy_from_slice(&values[base..base + value_count]);
    } else {
        result = 2; // NORMAL
        let mut i = 0;
        while i + 1 < frame_count && times[i + 1] < msec {
            i += 1;
        }
        let t0 = times[i];
        let t1 = times[i + 1];
        let f = if t1 > t0 { (msec - t0) / (t1 - t0) } else { 0.0 };
        for k in 0..value_count {
            let a = values[i * value_count + k];
            let b = values[(i + 1) * value_count + k];
            out[k] = a + (b - a) * f;
        }
    }
    write_f32_array(vm, out_handle, &out);
    vm.ret_i32(ctx, result);
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
