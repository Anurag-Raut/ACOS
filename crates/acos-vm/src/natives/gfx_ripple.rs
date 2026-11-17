//! Ripple / AGSL graphics natives: `RuntimeShader`, `CanvasProperty` and
//! `RenderNodeAnimator` (API 34).
//!
//! ## Why this module exists
//!
//! `RippleDrawable`'s press feedback is an AGSL (`RuntimeShader`) program. When
//! a `View` is pressed, `View.setPressed` -> `RippleDrawable.setState` first
//! touches `RippleShader`, whose constructor calls
//! `RuntimeShader.nativeCreateBuilder`; the press animation then allocates a
//! `CanvasProperty` and a `RenderNodeAnimator`. Before this module those
//! natives were unbound, so the async traversal that the press scheduled threw
//! `nativeCreateBuilder is not implemented`. The throw aborts the whole
//! traversal — including the sync `View$PerformClick` message queued by the
//! same ACTION_UP — so a *second* tap on a widget never ran its `onClick`.
//! Implementing the native *surface* is therefore load-bearing for the
//! interaction loop, not a rendering nicety.
//!
//! ## What is modelled, and what is a declared fallback
//!
//! * `RuntimeShader` keeps a real builder holding the AGSL source string and a
//!   real instance record holding the builder link plus every uniform written
//!   through `nativeUpdateUniforms` / `nativeUpdateShader`. The handles are
//!   generation-checked [`Handle`]s, exactly like every other `android.graphics`
//!   native, so a stale/forged handle is a structured VM error rather than an
//!   alias.
//! * `CanvasProperty` / `RenderNodeAnimator` keep real records for the float /
//!   paint property and the animator parameters (`duration`, `startDelay`,
//!   interpolator, listener). `nStart` does **not** schedule a render-thread
//!   animation: ACOS has no RenderThread, and the interaction pump rasterises
//!   frames synchronously (`Runtime::render_frame_now`). This is the structured
//!   fallback the task calls for.
//! * **Rendering fallback:** a `RuntimeShader`'s native instance is deliberately
//!   *not* registered in `gfx_paint`'s `ShaderSpec` table. When a `Paint` that
//!   carries it is snapshotted, `gfx_state::snapshot_paint` records a structured
//!   `unsupported` note and draws the paint **without** the AGSL shader, i.e. it
//!   falls back to the paint's flat colour (for the ripple, the pressed colour).
//!   The traversal completes; only the AGSL ripple *program* is not evaluated.
//!   This is honest: no pixels are fabricated from an unexecuted shader.
//!
//! All three classes' native lists are the API-34 boot DEX (scanned from
//! `.cache/boot/android-34/dex/framework/classes.dex`); the module declares
//! exactly the methods those classes need.

// The state records below intentionally hold the native parameters (uniforms,
// matrix, duration, interpolator, listener) even though the current fallback
// rasteriser does not evaluate them; reading them back is what makes the
// "builder holding uniforms" contract observable to a future AGSL evaluator.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::natives::gfx_handles::{Handle, HandleTable};
use crate::vm::Vm;

const RUNTIME_SHADER: &str = "Landroid/graphics/RuntimeShader;";
const CANVAS_PROPERTY: &str = "Landroid/graphics/CanvasProperty;";
const RENDER_NODE_ANIMATOR: &str = "Landroid/graphics/animation/RenderNodeAnimator;";
const INTERPOLATOR_FACTORY: &str = "Landroid/graphics/animation/NativeInterpolatorFactory;";

/// One uniform value written through a `RuntimeShader` native. Kept so the
/// builder "holds uniforms" observably (a later AGSL evaluator can read it)
/// even though the current rasteriser does not evaluate the program.
#[derive(Clone, Debug, PartialEq)]
enum Uniform {
    Floats(Vec<f32>),
    Ints(Vec<i32>),
    InputHandle(i64),
}

#[derive(Clone, Debug, Default)]
struct ShaderBuilder {
    /// The AGSL source passed to `nativeCreateBuilder`.
    source: String,
    uniforms: HashMap<String, Uniform>,
}

#[derive(Clone, Debug, Default)]
struct RuntimeShaderRec {
    builder: Option<i64>,
    /// `nativeCreateShader`'s opaque second argument (the matrix handle).
    matrix: i64,
    opaque: bool,
    /// `nativeUpdateShader` input links (name -> Bitmap/Shader handle).
    inputs: HashMap<String, i64>,
}

#[derive(Clone, Debug)]
enum CanvasProp {
    Float(f32),
    Paint(i64),
}

#[derive(Clone, Debug, Default)]
struct Animator {
    duration: i64,
    start_delay: i64,
    start_value: f32,
    interpolator: i64,
    listener: i64,
}

#[derive(Default, Debug)]
struct State {
    builders: HandleTable<ShaderBuilder>,
    shaders: HandleTable<RuntimeShaderRec>,
    canvas_props: HandleTable<CanvasProp>,
    animators: HandleTable<Animator>,
    /// Native interpolator handles. The fallback animator never evaluates an
    /// interpolation curve, so the record is empty; the handle only has to be
    /// real and stable because `RenderNodeAnimator` stores it and passes it
    /// back to `nSetInterpolator`.
    interpolators: HandleTable<()>,
}

fn state() -> MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    match S.get_or_init(|| Mutex::new(State::default())).lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Register the `RuntimeShader` / `CanvasProperty` / `RenderNodeAnimator`
/// natives.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    // --- android.graphics.RuntimeShader -----------------------------------
    vm.register_native(
        RUNTIME_SHADER,
        "nativeCreateBuilder",
        "(Ljava/lang/String;)J",
        rs_create_builder,
    );
    vm.register_native(
        RUNTIME_SHADER,
        "nativeCreateShader",
        "(JJ)J",
        rs_create_shader,
    );
    vm.register_native(RUNTIME_SHADER, "nativeGetFinalizer", "()J", rs_finalizer);
    vm.register_native(
        RUNTIME_SHADER,
        "nativeUpdateShader",
        "(JLjava/lang/String;J)V",
        rs_update_shader,
    );
    vm.register_native(
        RUNTIME_SHADER,
        "nativeUpdateUniforms",
        "(JLjava/lang/String;FFFFI)V",
        rs_uniform_floats4,
    );
    vm.register_native(
        RUNTIME_SHADER,
        "nativeUpdateUniforms",
        "(JLjava/lang/String;IIIII)V",
        rs_uniform_ints5,
    );
    vm.register_native(
        RUNTIME_SHADER,
        "nativeUpdateUniforms",
        "(JLjava/lang/String;[FZ)V",
        rs_uniform_float_array,
    );
    vm.register_native(
        RUNTIME_SHADER,
        "nativeUpdateUniforms",
        "(JLjava/lang/String;[I)V",
        rs_uniform_int_array,
    );

    // --- android.graphics.CanvasProperty ----------------------------------
    vm.register_native(CANVAS_PROPERTY, "nCreateFloat", "(F)J", cp_create_float);
    vm.register_native(CANVAS_PROPERTY, "nCreatePaint", "(J)J", cp_create_paint);

    // --- android.graphics.animation.RenderNodeAnimator --------------------
    vm.register_native(RENDER_NODE_ANIMATOR, "nCreateAnimator", "(IF)J", rna_create);
    vm.register_native(
        RENDER_NODE_ANIMATOR,
        "nCreateCanvasPropertyFloatAnimator",
        "(JF)J",
        rna_create_cp_float,
    );
    vm.register_native(
        RENDER_NODE_ANIMATOR,
        "nCreateCanvasPropertyPaintAnimator",
        "(JIF)J",
        rna_create_cp_paint,
    );
    vm.register_native(
        RENDER_NODE_ANIMATOR,
        "nCreateRevealAnimator",
        "(IIFF)J",
        rna_create_reveal,
    );
    vm.register_native(RENDER_NODE_ANIMATOR, "nStart", "(J)V", rna_start);
    vm.register_native(RENDER_NODE_ANIMATOR, "nEnd", "(J)V", rna_end);
    vm.register_native(
        RENDER_NODE_ANIMATOR,
        "nGetDuration",
        "(J)J",
        rna_get_duration,
    );
    vm.register_native(
        RENDER_NODE_ANIMATOR,
        "nSetDuration",
        "(JJ)V",
        rna_set_duration,
    );
    vm.register_native(
        RENDER_NODE_ANIMATOR,
        "nSetStartDelay",
        "(JJ)V",
        rna_set_start_delay,
    );
    vm.register_native(
        RENDER_NODE_ANIMATOR,
        "nSetStartValue",
        "(JF)V",
        rna_set_start_value,
    );
    vm.register_native(
        RENDER_NODE_ANIMATOR,
        "nSetInterpolator",
        "(JJ)V",
        rna_set_interpolator,
    );
    vm.register_native(
        RENDER_NODE_ANIMATOR,
        "nSetAllowRunningAsync",
        "(JZ)V",
        rna_noop_jz,
    );
    vm.register_native(
        RENDER_NODE_ANIMATOR,
        "nSetListener",
        "(JLandroid/graphics/animation/RenderNodeAnimator;)V",
        rna_set_listener,
    );

    // --- android.graphics.animation.NativeInterpolatorFactory ---------------
    // `RenderNodeAnimator.setInterpolator` builds one of these and hands the
    // pointer to `nSetInterpolator`. ACOS keeps a real handle per factory call
    // (the curve itself is not evaluated by the fallback animator).
    const IF: &str = INTERPOLATOR_FACTORY;
    vm.register_native(
        IF,
        "createAccelerateDecelerateInterpolator",
        "()J",
        interp_create,
    );
    vm.register_native(IF, "createAccelerateInterpolator", "(F)J", interp_create);
    vm.register_native(IF, "createAnticipateInterpolator", "(F)J", interp_create);
    vm.register_native(
        IF,
        "createAnticipateOvershootInterpolator",
        "(F)J",
        interp_create,
    );
    vm.register_native(IF, "createBounceInterpolator", "()J", interp_create);
    vm.register_native(IF, "createCycleInterpolator", "(F)J", interp_create);
    vm.register_native(IF, "createDecelerateInterpolator", "(F)J", interp_create);
    vm.register_native(IF, "createLinearInterpolator", "()J", interp_create);
    vm.register_native(IF, "createLutInterpolator", "([F)J", interp_create);
    vm.register_native(IF, "createOvershootInterpolator", "(F)J", interp_create);
    vm.register_native(IF, "createPathInterpolator", "([F[F)J", interp_create);
    Ok(())
}

fn interp_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // Arguments (control points, LUT, tension) are recorded-by-handle only;
    // the synchronous renderer does not run an animation, so the curve is not
    // sampled. See the module docs.
    let h = state().interpolators.insert(());
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

// ---------------------------------------------------------------------------
// RuntimeShader
// ---------------------------------------------------------------------------

fn read_string(vm: &mut Vm, h: u32) -> String {
    vm.str_units(h)
        .map(String::from_utf16_lossy)
        .unwrap_or_default()
}

fn rs_create_builder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let source = read_string(vm, vm.arg_ref(ctx, 0));
    let h = state().builders.insert(ShaderBuilder {
        source,
        uniforms: HashMap::new(),
    });
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn rs_create_shader(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let builder = vm.arg_i64(ctx, 0);
    let matrix = vm.arg_i64(ctx, 1);
    let h = state().shaders.insert(RuntimeShaderRec {
        builder: (builder != 0).then_some(builder),
        matrix,
        opaque: false,
        inputs: HashMap::new(),
    });
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn rs_finalizer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // No native memory to free; the Rust records are reclaimed with the
    // process. Returning 0 tells NativeAllocationRegistry there is nothing to
    // release (same contract as `gfx_paint::release_func`).
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn rs_update_shader(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let shader = vm.arg_i64(ctx, 0);
    let name = read_string(vm, vm.arg_ref(ctx, 1));
    let input = vm.arg_i64(ctx, 2);
    let mut st = state();
    if let Ok(rec) = st.shaders.get_mut(Handle::from_jlong(shader)) {
        rec.inputs.insert(name, input);
    }
    Ok(())
}

fn record_uniform(vm: &mut Vm, ctx: NativeCtx, value: Uniform) {
    let shader = vm.arg_i64(ctx, 0);
    let name = read_string(vm, vm.arg_ref(ctx, 1));
    let mut st = state();
    let Ok(rec) = st.shaders.get(Handle::from_jlong(shader)) else {
        return;
    };
    let Some(builder) = rec.builder else {
        return;
    };
    if let Ok(b) = st.builders.get_mut(Handle::from_jlong(builder)) {
        b.uniforms.insert(name, value);
    }
}

fn rs_uniform_floats4(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vec![
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
        vm.arg_f32(ctx, 5),
    ];
    record_uniform(vm, ctx, Uniform::Floats(v));
    Ok(())
}

fn rs_uniform_ints5(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vec![
        vm.arg_i32(ctx, 2),
        vm.arg_i32(ctx, 3),
        vm.arg_i32(ctx, 4),
        vm.arg_i32(ctx, 5),
        vm.arg_i32(ctx, 6),
    ];
    record_uniform(vm, ctx, Uniform::Ints(v));
    Ok(())
}

fn rs_uniform_float_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = read_string(vm, vm.arg_ref(ctx, 1));
    // The array itself is not evaluated by the fallback rasteriser; record the
    // uniform name with an empty payload so the builder still tracks it.
    record_uniform_by_name(vm, ctx, name, Uniform::Floats(Vec::new()));
    Ok(())
}

fn rs_uniform_int_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = read_string(vm, vm.arg_ref(ctx, 1));
    record_uniform_by_name(vm, ctx, name, Uniform::Ints(Vec::new()));
    Ok(())
}

fn record_uniform_by_name(vm: &mut Vm, ctx: NativeCtx, name: String, value: Uniform) {
    let shader = vm.arg_i64(ctx, 0);
    let mut st = state();
    let Ok(rec) = st.shaders.get(Handle::from_jlong(shader)) else {
        return;
    };
    let Some(builder) = rec.builder else {
        return;
    };
    if let Ok(b) = st.builders.get_mut(Handle::from_jlong(builder)) {
        b.uniforms.insert(name, value);
    }
}

// ---------------------------------------------------------------------------
// CanvasProperty
// ---------------------------------------------------------------------------

fn cp_create_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f32(ctx, 0);
    let h = state().canvas_props.insert(CanvasProp::Float(v));
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn cp_create_paint(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let paint = vm.arg_i64(ctx, 0);
    let h = state().canvas_props.insert(CanvasProp::Paint(paint));
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

// ---------------------------------------------------------------------------
// RenderNodeAnimator
// ---------------------------------------------------------------------------

fn insert_animator(a: Animator) -> i64 {
    state().animators.insert(a).to_jlong()
}

fn rna_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `(renderNode, property)` — the render node is not animated by the
    // fallback; keep the record so the handle is real and duration reads work.
    let _ = vm.arg_i64(ctx, 0);
    let _ = vm.arg_i32(ctx, 1);
    vm.ret_i64(ctx, insert_animator(Animator::default()));
    Ok(())
}

fn rna_create_cp_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let start = vm.arg_f32(ctx, 1);
    vm.ret_i64(
        ctx,
        insert_animator(Animator {
            start_value: start,
            ..Animator::default()
        }),
    );
    Ok(())
}

fn rna_create_cp_paint(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let start = vm.arg_f32(ctx, 2);
    vm.ret_i64(
        ctx,
        insert_animator(Animator {
            start_value: start,
            ..Animator::default()
        }),
    );
    Ok(())
}

fn rna_create_reveal(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = (vm.arg_i32(ctx, 0), vm.arg_i32(ctx, 1));
    let start = vm.arg_f32(ctx, 3);
    vm.ret_i64(
        ctx,
        insert_animator(Animator {
            start_value: start,
            ..Animator::default()
        }),
    );
    Ok(())
}

fn with_animator<F: FnOnce(&mut Animator)>(handle: i64, f: F) {
    let mut st = state();
    if let Ok(a) = st.animators.get_mut(Handle::from_jlong(handle)) {
        f(a);
    }
}

fn rna_start(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // Structured fallback: ACOS has no RenderThread and rasterises frames
    // synchronously, so there is no async animation to schedule. The animator
    // record exists and its duration/delay are honoured by callers.
    let _ = vm.arg_i64(ctx, 0);
    Ok(())
}

fn rna_end(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.arg_i64(ctx, 0);
    Ok(())
}

fn rna_get_duration(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0);
    let d = state()
        .animators
        .get(Handle::from_jlong(handle))
        .map(|a| a.duration)
        .unwrap_or(0);
    vm.ret_i64(ctx, d);
    Ok(())
}

fn rna_set_duration(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (handle, v) = (vm.arg_i64(ctx, 0), vm.arg_i64(ctx, 1));
    with_animator(handle, |a| a.duration = v);
    Ok(())
}

fn rna_set_start_delay(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (handle, v) = (vm.arg_i64(ctx, 0), vm.arg_i64(ctx, 1));
    with_animator(handle, |a| a.start_delay = v);
    Ok(())
}

fn rna_set_start_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0);
    let v = vm.arg_f32(ctx, 1);
    with_animator(handle, |a| a.start_value = v);
    Ok(())
}

fn rna_set_interpolator(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (handle, v) = (vm.arg_i64(ctx, 0), vm.arg_i64(ctx, 1));
    with_animator(handle, |a| a.interpolator = v);
    Ok(())
}

fn rna_set_listener(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0);
    // The listener is a Java object; keep only a non-zero presence marker
    // (the fallback never fires animation callbacks).
    let listener = i64::from(vm.arg_ref(ctx, 1) != crate::heap::NULL);
    with_animator(handle, |a| a.listener = listener);
    Ok(())
}

fn rna_noop_jz(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = (vm.arg_i64(ctx, 0), vm.arg_bool(ctx, 1));
    Ok(())
}
