//! `android.graphics.RenderNode` natives (T-32 display-list side).
//!
//! A `RenderNode` is a property bag plus a recorded display list. Every
//! property that `acos-render`'s [`RenderNode`] models is stored there; the
//! handful of API-34 properties the renderer does not consume (clip bounds,
//! camera distance, reveal clip, force-dark, static/animation matrix, render
//! effect, usage hint) are kept in a side table so their get/set round-trips
//! exactly, without pretending the renderer honours them.
//!
//! `nCreate` allocates the node in the shared [`super::gfx_state`] context;
//! the node's display list is filled by [`super::gfx_canvas`] and rasterised by
//! [`super::gfx_hwui`]. `beginRecording`/`endRecording` are ordinary Java
//! methods that call `RecordingCanvas.nCreateDisplayListCanvas` /
//! `nFinishRecording`, both implemented in `gfx_canvas.rs`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use acos_render::handle::Handle as RHandle;
use acos_render::matrix::Matrix3x3;
use acos_render::paint::{RenderEffectSpec, TileMode};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::natives::gfx_handles::{Handle, HandleTable};
use crate::natives::gfx_paint;
use crate::natives::gfx_state::{gj, render_err, rh, stale, with_ctx};
use crate::vm::Vm;

/// API-34 `RenderNode` properties not modelled by `acos-render::RenderNode`.
#[derive(Clone, Debug)]
struct NodeExtra {
    clip_bounds: Option<(i32, i32, i32, i32)>,
    allow_force_dark: bool,
    camera_distance: f32,
    reveal_clip: Option<(bool, f32, f32, f32)>,
    static_matrix: Option<RHandle>,
    animation_matrix: Option<RHandle>,
    render_effect: Option<i64>,
    usage_hint: i32,
    unique_id: u64,
    texture_view: bool,
    projection_receiver: bool,
    project_backwards: bool,
    animators: Vec<i64>,
    stretch: Option<(f32, f32, f32, f32)>,
}

impl Default for NodeExtra {
    fn default() -> Self {
        NodeExtra {
            clip_bounds: None,
            allow_force_dark: true,
            camera_distance: 1280.0,
            reveal_clip: None,
            static_matrix: None,
            animation_matrix: None,
            render_effect: None,
            usage_hint: 0,
            unique_id: 0,
            texture_view: false,
            projection_receiver: false,
            project_backwards: false,
            animators: Vec::new(),
            stretch: None,
        }
    }
}

thread_local! {
    static EXTRA: RefCell<HashMap<u64, NodeExtra>> = RefCell::new(HashMap::new());
    static NEXT_ID: RefCell<u64> = const { RefCell::new(1) };
}

/// `android.graphics.RenderEffect` descriptions, keyed by the `jlong` the
/// framework holds in `RenderEffect.mNativeRenderEffect`.
static EFFECTS: OnceLock<Mutex<HandleTable<RenderEffectSpec>>> = OnceLock::new();

fn effects() -> &'static Mutex<HandleTable<RenderEffectSpec>> {
    EFFECTS.get_or_init(|| Mutex::new(HandleTable::new()))
}

fn effect_spec(handle: i64) -> VmResult<RenderEffectSpec> {
    effects()
        .lock()
        .map_err(|_| VmError::Internal("RenderEffect table poisoned".to_string()))?
        .get(Handle::from_jlong(handle))
        .cloned()
        .map_err(|e| VmError::Internal(format!("RenderEffect: {e}")))
}

fn node(vm: &Vm, ctx: NativeCtx) -> VmResult<RHandle> {
    let j = vm.arg_i64(ctx, 0);
    if with_ctx(|c| c.render_nodes.contains(rh(j))) {
        Ok(rh(j))
    } else {
        Err(stale("RenderNode", j))
    }
}

fn extra<R>(h: RHandle, f: impl FnOnce(&mut NodeExtra) -> R) -> R {
    EXTRA.with(|e| f(e.borrow_mut().entry(h.0).or_default()))
}

fn extra_ref<R>(h: RHandle, f: impl FnOnce(&NodeExtra) -> R) -> Option<R> {
    EXTRA.with(|e| e.borrow().get(&h.0).map(f))
}

/// Register every `RenderNode` native whose semantics ACOS can honour.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    const N: &str = "Landroid/graphics/RenderNode;";
    // Lifecycle / identity.
    vm.register_native(N, "nCreate", "(Ljava/lang/String;)J", n_create);
    vm.register_native(N, "nGetNativeFinalizer", "()J", n_finalizer);
    vm.register_native(N, "nGetUniqueId", "(J)J", n_unique_id);
    vm.register_native(N, "nIsValid", "(J)Z", n_is_valid);
    vm.register_native(N, "nGetAllocatedSize", "(J)I", n_alloc_size);
    vm.register_native(N, "nGetUsageSize", "(J)I", n_alloc_size);
    vm.register_native(N, "nDiscardDisplayList", "(J)V", n_discard);
    vm.register_native(N, "nOutput", "(J)V", n_noop);

    // Bounds.
    vm.register_native(N, "nSetLeftTopRightBottom", "(JIIII)Z", n_set_ltrb);
    vm.register_native(N, "nGetLeft", "(J)I", n_get_left);
    vm.register_native(N, "nGetTop", "(J)I", n_get_top);
    vm.register_native(N, "nGetRight", "(J)I", n_get_right);
    vm.register_native(N, "nGetBottom", "(J)I", n_get_bottom);
    vm.register_native(N, "nGetWidth", "(J)I", n_get_width);
    vm.register_native(N, "nGetHeight", "(J)I", n_get_height);
    vm.register_native(N, "nSetLeft", "(JI)Z", n_set_left);
    vm.register_native(N, "nSetTop", "(JI)Z", n_set_top);
    vm.register_native(N, "nSetRight", "(JI)Z", n_set_right);
    vm.register_native(N, "nSetBottom", "(JI)Z", n_set_bottom);
    vm.register_native(N, "nOffsetLeftAndRight", "(JI)Z", n_offset_x);
    vm.register_native(N, "nOffsetTopAndBottom", "(JI)Z", n_offset_y);

    // Alpha / transforms.
    vm.register_native(N, "nSetAlpha", "(JF)Z", n_set_alpha);
    vm.register_native(N, "nGetAlpha", "(J)F", n_get_alpha);
    vm.register_native(N, "nSetTranslationX", "(JF)Z", n_set_tx);
    vm.register_native(N, "nGetTranslationX", "(J)F", n_get_tx);
    vm.register_native(N, "nSetTranslationY", "(JF)Z", n_set_ty);
    vm.register_native(N, "nGetTranslationY", "(J)F", n_get_ty);
    vm.register_native(N, "nSetTranslationZ", "(JF)Z", n_set_tz);
    vm.register_native(N, "nGetTranslationZ", "(J)F", n_get_tz);
    vm.register_native(N, "nSetScaleX", "(JF)Z", n_set_sx);
    vm.register_native(N, "nGetScaleX", "(J)F", n_get_sx);
    vm.register_native(N, "nSetScaleY", "(JF)Z", n_set_sy);
    vm.register_native(N, "nGetScaleY", "(J)F", n_get_sy);
    vm.register_native(N, "nSetRotation", "(JF)Z", n_set_rot);
    vm.register_native(N, "nGetRotation", "(J)F", n_get_rot);
    vm.register_native(N, "nSetRotationX", "(JF)Z", n_set_rot_x);
    vm.register_native(N, "nGetRotationX", "(J)F", n_get_rot_x);
    vm.register_native(N, "nSetRotationY", "(JF)Z", n_set_rot_y);
    vm.register_native(N, "nGetRotationY", "(J)F", n_get_rot_y);
    vm.register_native(N, "nSetPivotX", "(JF)Z", n_set_pivot_x);
    vm.register_native(N, "nGetPivotX", "(J)F", n_get_pivot_x);
    vm.register_native(N, "nSetPivotY", "(JF)Z", n_set_pivot_y);
    vm.register_native(N, "nGetPivotY", "(J)F", n_get_pivot_y);
    vm.register_native(N, "nIsPivotExplicitlySet", "(J)Z", n_pivot_explicit);
    vm.register_native(N, "nResetPivot", "(J)Z", n_reset_pivot);
    vm.register_native(N, "nSetCameraDistance", "(JF)Z", n_set_camera);
    vm.register_native(N, "nGetCameraDistance", "(J)F", n_get_camera);

    // Clip / outline.
    vm.register_native(N, "nSetClipToBounds", "(JZ)Z", n_set_clip_bounds_flag);
    vm.register_native(N, "nGetClipToBounds", "(J)Z", n_get_clip_bounds_flag);
    vm.register_native(N, "nSetClipToOutline", "(JZ)Z", n_set_clip_outline);
    vm.register_native(N, "nGetClipToOutline", "(J)Z", n_get_clip_outline);
    vm.register_native(N, "nSetClipBounds", "(JIIII)Z", n_set_clip_bounds);
    vm.register_native(N, "nSetClipBoundsEmpty", "(J)Z", n_clip_bounds_empty);
    vm.register_native(N, "nSetOutlineRoundRect", "(JIIIIFF)Z", n_set_outline_rr);
    vm.register_native(N, "nSetOutlinePath", "(JJF)Z", n_set_outline_path);
    vm.register_native(N, "nSetOutlineEmpty", "(J)Z", n_outline_none);
    vm.register_native(N, "nSetOutlineNone", "(J)Z", n_outline_none);
    vm.register_native(N, "nSetRevealClip", "(JZFFF)Z", n_set_reveal);
    // Stretch effect (API 34). ACOS records the coefficients so the get/set
    // round-trips exactly, but the software rasteriser does not apply the
    // GPU stretch shader (same declared divergence as the other unmodelled
    // RenderNode properties).
    vm.register_native(N, "nStretch", "(JFFFF)Z", n_set_stretch);
    vm.register_native(N, "nClearStretch", "(J)Z", n_clear_stretch);

    // Elevation / shadows.
    vm.register_native(N, "nSetElevation", "(JF)Z", n_set_elevation);
    vm.register_native(N, "nGetElevation", "(J)F", n_get_elevation);
    vm.register_native(N, "nSetAmbientShadowColor", "(JI)Z", n_set_ambient);
    vm.register_native(N, "nGetAmbientShadowColor", "(J)I", n_get_ambient);
    vm.register_native(N, "nSetSpotShadowColor", "(JI)Z", n_set_spot);
    vm.register_native(N, "nGetSpotShadowColor", "(J)I", n_get_spot);
    vm.register_native(N, "nHasShadow", "(J)Z", n_has_shadow);

    // Layer / rendering flags.
    vm.register_native(N, "nSetLayerType", "(JI)Z", n_set_layer_type);
    vm.register_native(N, "nGetLayerType", "(J)I", n_get_layer_type);
    vm.register_native(N, "nSetLayerPaint", "(JJ)Z", n_set_layer_paint);
    vm.register_native(N, "nSetHasOverlappingRendering", "(JZ)Z", n_set_overlapping);
    vm.register_native(N, "nHasOverlappingRendering", "(J)Z", n_get_overlapping);
    vm.register_native(N, "nSetAllowForceDark", "(JZ)Z", n_set_force_dark);
    vm.register_native(N, "nGetAllowForceDark", "(J)Z", n_get_force_dark);
    vm.register_native(N, "nSetUsageHint", "(JI)V", n_set_usage);
    vm.register_native(N, "nSetIsTextureView", "(J)V", n_set_texture_view);
    vm.register_native(N, "nSetProjectionReceiver", "(JZ)Z", n_set_proj_receiver);
    vm.register_native(N, "nSetProjectBackwards", "(JZ)Z", n_set_project_backwards);
    vm.register_native(N, "nSetRenderEffect", "(JJ)Z", n_set_render_effect);

    // RenderEffect factories (W78-A).
    const RE: &str = "Landroid/graphics/RenderEffect;";
    vm.register_native(RE, "nativeCreateBlurEffect", "(FFJI)J", re_blur);
    vm.register_native(RE, "nativeCreateColorFilterEffect", "(JJ)J", re_color_filter);
    vm.register_native(RE, "nativeGetFinalizer", "()J", re_finalizer);

    // Matrices.
    vm.register_native(N, "nHasIdentityMatrix", "(J)Z", n_identity);
    vm.register_native(N, "nGetTransformMatrix", "(JJ)V", n_get_transform);
    vm.register_native(N, "nGetInverseTransformMatrix", "(JJ)V", n_get_inverse);
    vm.register_native(N, "nGetAnimationMatrix", "(JJ)Z", n_get_anim_matrix);
    vm.register_native(N, "nSetStaticMatrix", "(JJ)Z", n_set_static_matrix);
    vm.register_native(N, "nSetAnimationMatrix", "(JJ)Z", n_set_anim_matrix);

    // Animator bookkeeping (ACOS has no native animators; keep the ops honest).
    vm.register_native(N, "nAddAnimator", "(JJ)V", n_add_animator);
    vm.register_native(N, "nEndAllAnimators", "(J)V", n_end_animators);
    vm.register_native(N, "nForceEndAnimators", "(J)V", n_end_animators);
    vm.register_native(N, "nRequestPositionUpdates", "(JLjava/lang/ref/WeakReference;)V", n_noop);
    Ok(())
}

fn n_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = {
        let h = vm.arg_ref(ctx, 0);
        if h == 0 {
            String::new()
        } else {
            vm.string_to_rust(h).unwrap_or_default()
        }
    };
    let r = with_ctx(|c| c.render_node_create(&name));
    EXTRA.with(|e| {
        let id = {
            NEXT_ID.with(|n| {
                let mut n = n.borrow_mut();
                let id = *n;
                *n += 1;
                id
            })
        };
        let ex = NodeExtra {
            unique_id: id,
            ..NodeExtra::default()
        };
        e.borrow_mut().insert(r.0, ex);
    });
    vm.ret_i64(ctx, gj(r));
    Ok(())
}

fn n_finalizer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // No native finalizer: nodes are owned by the render context, not malloc.
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn n_unique_id(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let id = extra_ref(h, |e| e.unique_id).unwrap_or(0);
    vm.ret_i64(ctx, id as i64);
    Ok(())
}

fn n_is_valid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // API-34 `RenderNode.hasDisplayList()` and `RenderNode.isValid()` both
    // bottom out in `nIsValid`. In HWUI it is true only once a display list
    // has actually been recorded: `ThreadedRenderer.updateRootDisplayList`
    // records the root only when `!mRootNode.hasDisplayList()`, and
    // `View.updateDisplayListIfDirty` re-records when `!renderNode.isValid()`.
    // Returning true for a fresh, never-recorded node silently made the root
    // display list empty (every frame black). A stale handle is `false`, not an
    // error, because callers use the boolean as a predicate.
    let j = vm.arg_i64(ctx, 0);
    let valid = with_ctx(|c| {
        c.render_nodes
            .get(rh(j))
            .is_some_and(|n| !n.display_list.is_empty())
    });
    vm.ret_bool(ctx, valid);
    Ok(())
}

fn n_alloc_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let size = with_ctx(|c| {
        c.render_nodes
            .get(h)
            .map(|n| (n.display_list.len() * 32 + 256) as i32)
            .unwrap_or(0)
    });
    vm.ret_i32(ctx, size);
    Ok(())
}

fn n_discard(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    with_ctx(|c| c.render_node_clear_display_list(h).map_err(|e| render_err("RenderNode.discardDisplayList", e)))?;
    Ok(())
}

fn n_noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

macro_rules! get_i32 {
    ($f:ident, $field:ident) => {
        fn $f(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let h = node(vm, ctx)?;
            let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.$field)).unwrap_or(0);
            vm.ret_i32(ctx, v);
            Ok(())
        }
    };
}
macro_rules! get_f32 {
    ($f:ident, $field:ident) => {
        fn $f(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let h = node(vm, ctx)?;
            let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.$field)).unwrap_or(0.0);
            vm.ret_f32(ctx, v);
            Ok(())
        }
    };
}
macro_rules! set_i32_zone {
    ($f:ident, $field:ident) => {
        fn $f(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let h = node(vm, ctx)?;
            let v = vm.arg_i32(ctx, 1);
            with_ctx(|c| {
                if let Some(n) = c.render_nodes.get_mut(h) {
                    n.$field = v;
                    n.version = n.version.wrapping_add(1).max(1);
                }
            });
            vm.ret_bool(ctx, true);
            Ok(())
        }
    };
}

get_i32!(n_get_left, left);
get_i32!(n_get_top, top);
get_i32!(n_get_right, right);
get_i32!(n_get_bottom, bottom);
get_f32!(n_get_alpha, alpha);
get_f32!(n_get_tx, translation_x);
get_f32!(n_get_ty, translation_y);
get_f32!(n_get_tz, translation_z);
get_f32!(n_get_sx, scale_x);
get_f32!(n_get_sy, scale_y);
get_f32!(n_get_rot, rotation);
get_f32!(n_get_rot_x, rotation_x);
get_f32!(n_get_rot_y, rotation_y);
get_f32!(n_get_pivot_x, pivot_x);
get_f32!(n_get_pivot_y, pivot_y);
get_f32!(n_get_elevation, elevation);

// `width`/`height` are methods, not fields: separate bodies.
fn n_get_width(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.width())).unwrap_or(0);
    vm.ret_i32(ctx, v);
    Ok(())
}
fn n_get_height(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.height())).unwrap_or(0);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn n_set_ltrb(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let (l, t, r, b) = (
        vm.arg_i32(ctx, 1),
        vm.arg_i32(ctx, 2),
        vm.arg_i32(ctx, 3),
        vm.arg_i32(ctx, 4),
    );
    with_ctx(|c| {
        c.render_node_set_bounds(h, l, t, r, b)
            .map_err(|e| render_err("RenderNode.setLeftTopRightBottom", e))
    })?;
    vm.ret_bool(ctx, true);
    Ok(())
}

set_i32_zone!(n_set_left, left);
set_i32_zone!(n_set_top, top);
set_i32_zone!(n_set_right, right);
set_i32_zone!(n_set_bottom, bottom);

fn n_offset_x(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let d = vm.arg_i32(ctx, 1);
    mut_node(h, |n| {
        n.left += d;
        n.right += d;
    });
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_offset_y(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let d = vm.arg_i32(ctx, 1);
    mut_node(h, |n| {
        n.top += d;
        n.bottom += d;
    });
    vm.ret_bool(ctx, true);
    Ok(())
}

fn mut_node(h: RHandle, f: impl FnOnce(&mut acos_render::RenderNode)) {
    with_ctx(|c| {
        if let Some(n) = c.render_nodes.get_mut(h) {
            f(n);
            n.version = n.version.wrapping_add(1).max(1);
        }
    });
}

macro_rules! set_f32 {
    ($f:ident, $field:ident) => {
        fn $f(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let h = node(vm, ctx)?;
            let v = vm.arg_f32(ctx, 1);
            mut_node(h, |n| n.$field = v);
            vm.ret_bool(ctx, true);
            Ok(())
        }
    };
}
set_f32!(n_set_tx, translation_x);
set_f32!(n_set_ty, translation_y);
set_f32!(n_set_tz, translation_z);
set_f32!(n_set_sx, scale_x);
set_f32!(n_set_sy, scale_y);
set_f32!(n_set_rot, rotation);
set_f32!(n_set_rot_x, rotation_x);
set_f32!(n_set_rot_y, rotation_y);

fn n_set_alpha(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_f32(ctx, 1);
    with_ctx(|c| c.render_node_set_alpha(h, v).map_err(|e| render_err("RenderNode.setAlpha", e)))?;
    vm.ret_bool(ctx, true);
    Ok(())
}

fn n_set_pivot_x(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let x = vm.arg_f32(ctx, 1);
    let y = with_ctx(|c| c.render_nodes.get(h).map(|n| n.pivot_y)).unwrap_or(0.0);
    with_ctx(|c| c.render_node_set_pivot(h, x, y).map_err(|e| render_err("RenderNode.setPivotX", e)))?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_set_pivot_y(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let y = vm.arg_f32(ctx, 1);
    let x = with_ctx(|c| c.render_nodes.get(h).map(|n| n.pivot_x)).unwrap_or(0.0);
    with_ctx(|c| c.render_node_set_pivot(h, x, y).map_err(|e| render_err("RenderNode.setPivotY", e)))?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_pivot_explicit(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.pivot_explicit)).unwrap_or(false);
    vm.ret_bool(ctx, v);
    Ok(())
}
fn n_reset_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    mut_node(h, |n| n.pivot_explicit = false);
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_set_camera(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_f32(ctx, 1);
    extra(h, |e| e.camera_distance = v);
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_get_camera(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    vm.ret_f32(ctx, extra_ref(h, |e| e.camera_distance).unwrap_or(0.0));
    Ok(())
}

fn n_set_clip_bounds_flag(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_bool(ctx, 1);
    with_ctx(|c| c.render_node_set_clip_to_bounds(h, v).map_err(|e| render_err("RenderNode.setClipToBounds", e)))?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_get_clip_bounds_flag(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.clip_to_bounds)).unwrap_or(false);
    vm.ret_bool(ctx, v);
    Ok(())
}
fn n_set_clip_outline(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_bool(ctx, 1);
    with_ctx(|c| c.render_node_set_clip_to_outline(h, v).map_err(|e| render_err("RenderNode.setClipToOutline", e)))?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_get_clip_outline(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.clip_to_outline)).unwrap_or(false);
    vm.ret_bool(ctx, v);
    Ok(())
}
fn n_set_clip_bounds(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = (vm.arg_i32(ctx, 1), vm.arg_i32(ctx, 2), vm.arg_i32(ctx, 3), vm.arg_i32(ctx, 4));
    extra(h, |e| e.clip_bounds = Some(v));
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_clip_bounds_empty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    extra(h, |e| e.clip_bounds = None);
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_set_outline_rr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let (l, t, r, b, rx, ry) = (
        vm.arg_i32(ctx, 1),
        vm.arg_i32(ctx, 2),
        vm.arg_i32(ctx, 3),
        vm.arg_i32(ctx, 4),
        vm.arg_f32(ctx, 5),
        vm.arg_f32(ctx, 6),
    );
    with_ctx(|c| {
        c.render_node_set_outline_round_rect(h, l, t, r, b, rx, ry)
            .map_err(|e| render_err("RenderNode.setOutlineRoundRect", e))
    })?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_set_outline_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let path = vm.arg_i64(ctx, 1);
    let alpha = vm.arg_f32(ctx, 2);
    with_ctx(|c| {
        c.render_node_set_outline_path(h, rh(path), alpha)
            .map_err(|e| render_err("RenderNode.setOutlinePath", e))
    })?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_outline_none(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    with_ctx(|c| c.render_node_set_outline_none(h).map_err(|e| render_err("RenderNode.setOutlineNone", e)))?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_set_stretch(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let s = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    extra(h, |e| e.stretch = Some(s));
    vm.ret_bool(ctx, true);
    Ok(())
}

fn n_clear_stretch(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    extra(h, |e| e.stretch = None);
    vm.ret_bool(ctx, true);
    Ok(())
}

fn n_set_reveal(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let on = vm.arg_bool(ctx, 1);
    let v = (vm.arg_f32(ctx, 2), vm.arg_f32(ctx, 3), vm.arg_f32(ctx, 4));
    extra(h, |e| e.reveal_clip = if on { Some((true, v.0, v.1, v.2)) } else { None });
    vm.ret_bool(ctx, true);
    Ok(())
}

fn n_set_elevation(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_f32(ctx, 1);
    with_ctx(|c| c.render_node_set_elevation(h, v).map_err(|e| render_err("RenderNode.setElevation", e)))?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_set_ambient(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_i32(ctx, 1) as u32;
    mut_node(h, |n| n.ambient_shadow_color = v);
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_get_ambient(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.ambient_shadow_color as i32)).unwrap_or(0);
    vm.ret_i32(ctx, v);
    Ok(())
}
fn n_set_spot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_i32(ctx, 1) as u32;
    mut_node(h, |n| n.spot_shadow_color = v);
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_get_spot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.spot_shadow_color as i32)).unwrap_or(0);
    vm.ret_i32(ctx, v);
    Ok(())
}
fn n_has_shadow(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.elevation > 0.0)).unwrap_or(false);
    vm.ret_bool(ctx, v);
    Ok(())
}

fn n_set_layer_type(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_i32(ctx, 1);
    with_ctx(|c| c.render_node_set_layer_type(h, v).map_err(|e| render_err("RenderNode.setLayerType", e)))?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_get_layer_type(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.layer_type as i32)).unwrap_or(0);
    vm.ret_i32(ctx, v);
    Ok(())
}
fn n_set_layer_paint(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let p = vm.arg_i64(ctx, 1);
    let handle = if p == 0 { None } else { Some(rh(p)) };
    with_ctx(|c| c.render_node_set_layer_paint(h, handle).map_err(|e| render_err("RenderNode.setLayerPaint", e)))?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_set_overlapping(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_bool(ctx, 1);
    with_ctx(|c| c.render_node_set_overlapping(h, v).map_err(|e| render_err("RenderNode.setHasOverlappingRendering", e)))?;
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_get_overlapping(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.has_overlapping_rendering)).unwrap_or(true);
    vm.ret_bool(ctx, v);
    Ok(())
}
fn n_set_force_dark(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_bool(ctx, 1);
    extra(h, |e| e.allow_force_dark = v);
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_get_force_dark(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    vm.ret_bool(ctx, extra_ref(h, |e| e.allow_force_dark).unwrap_or(true));
    Ok(())
}
fn n_set_usage(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_i32(ctx, 1);
    extra(h, |e| e.usage_hint = v);
    Ok(())
}
fn n_set_texture_view(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    extra(h, |e| e.texture_view = true);
    Ok(())
}
fn n_set_proj_receiver(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_bool(ctx, 1);
    extra(h, |e| e.projection_receiver = v);
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_set_project_backwards(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_bool(ctx, 1);
    extra(h, |e| e.project_backwards = v);
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_set_render_effect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = vm.arg_i64(ctx, 1);
    let spec = if v == 0 {
        None
    } else {
        Some(effect_spec(v)?)
    };
    extra(h, |e| e.render_effect = if v == 0 { None } else { Some(v) });
    with_ctx(|c| {
        if let Some(n) = c.render_nodes.get_mut(h) {
            n.render_effect = spec;
        }
    });
    vm.ret_bool(ctx, true);
    Ok(())
}

fn re_finalizer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn store_effect(vm: &mut Vm, ctx: NativeCtx, spec: RenderEffectSpec) -> VmResult<()> {
    let h = effects()
        .lock()
        .map_err(|_| VmError::Internal("RenderEffect table poisoned".to_string()))?
        .insert(spec)
        .to_jlong();
    vm.ret_i64(ctx, h);
    Ok(())
}

fn re_blur(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let sigma_x = vm.arg_f32(ctx, 0);
    let sigma_y = vm.arg_f32(ctx, 1);
    let input = vm.arg_i64(ctx, 2);
    if input != 0 {
        return Err(VmError::unsupported_api(
            "RenderEffect blur chains",
            "input RenderEffect",
        ));
    }
    let edge = TileMode::from_ordinal(vm.arg_i32(ctx, 3)).unwrap_or(TileMode::Clamp);
    store_effect(
        vm,
        ctx,
        RenderEffectSpec::Blur {
            sigma_x,
            sigma_y,
            edge,
        },
    )
}

fn re_color_filter(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let filter = vm.arg_i64(ctx, 0);
    let input = vm.arg_i64(ctx, 1);
    if input != 0 {
        return Err(VmError::unsupported_api(
            "RenderEffect colour-filter chains",
            "input RenderEffect",
        ));
    }
    let cf = gfx_paint::filter_spec(filter)
        .ok_or_else(|| VmError::Internal("RenderEffect colour filter unresolved".to_string()))?;
    store_effect(vm, ctx, RenderEffectSpec::ColorFilter(cf))
}

fn n_identity(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let v = with_ctx(|c| c.render_nodes.get(h).map(|n| n.has_identity_matrix())).unwrap_or(true);
    vm.ret_bool(ctx, v);
    Ok(())
}
fn write_matrix_arg(vm: &Vm, ctx: NativeCtx, m: Matrix3x3) -> VmResult<()> {
    crate::natives::gfx_matrix::matrix_store(vm.arg_i64(ctx, 1), m)
}

fn n_get_transform(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    // Java-visible transform excludes the layout position (`left`/`top`):
    // `View.transformFromViewToWindowSpace` adds `mLeft`/`mTop` itself (W179-A).
    let m = with_ctx(|c| c.render_nodes.get(h).map(|n| n.transform_matrix()));
    if let Some(m) = m {
        write_matrix_arg(vm, ctx, m)?;
    }
    Ok(())
}
fn n_get_inverse(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let m = with_ctx(|c| c.render_nodes.get(h).map(|n| n.transform_matrix()))
        .and_then(|m| m.inverted());
    if let Some(m) = m {
        write_matrix_arg(vm, ctx, m)?;
    }
    Ok(())
}
fn n_get_anim_matrix(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let src = extra_ref(h, |e| e.animation_matrix).flatten();
    if let Some(src) = src {
        let vals = crate::natives::gfx_matrix::matrix_values(gj(src))?;
        let m = crate::natives::gfx_matrix::matrix_from_values(vals);
        write_matrix_arg(vm, ctx, m)?;
        vm.ret_bool(ctx, true);
    } else {
        vm.ret_bool(ctx, false);
    }
    Ok(())
}
fn n_set_static_matrix(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let m = vm.arg_i64(ctx, 1);
    extra(h, |e| e.static_matrix = if m == 0 { None } else { Some(rh(m)) });
    vm.ret_bool(ctx, true);
    Ok(())
}
fn n_set_anim_matrix(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let m = vm.arg_i64(ctx, 1);
    extra(h, |e| e.animation_matrix = if m == 0 { None } else { Some(rh(m)) });
    vm.ret_bool(ctx, true);
    Ok(())
}

fn n_add_animator(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    let a = vm.arg_i64(ctx, 1);
    extra(h, |e| e.animators.push(a));
    Ok(())
}
fn n_end_animators(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = node(vm, ctx)?;
    extra(h, |e| e.animators.clear());
    Ok(())
}
