//! `android.graphics.Canvas` / `BaseCanvas` / `BaseRecordingCanvas` /
//! `RecordingCanvas` natives — the recording side of the display list.
//!
//! In API 34 the `Canvas.draw*` methods are `final` in `BaseCanvas` and
//! `BaseRecordingCanvas` and bottom out in `nDraw*` natives. A `RecordingCanvas`
//! wraps a `RenderNode`: `RecordingCanvas.nCreateDisplayListCanvas(node,w,h)`
//! binds the canvas to the node, the `nDraw*` calls append to a staging
//! [`DisplayOp`] list, and `nFinishRecording(canvas,node)` publishes the list
//! onto the node.
//!
//! **Record-time state capture (advisor risk 2).** A `DisplayOp` stores paint
//! by handle, and the renderer resolves it at replay time; if the framework
//! mutated the `Paint` after recording, the frame would change. Every draw op
//! therefore snapshots its paint ([`gfx_state::snapshot_paint`]) into a frozen
//! clone at record time, so a later `Paint.setColor` cannot alter an already
//! recorded op. `Path` handles are cloned in the same way by `gfx_path.rs`.
//!
//! Raster (`new Canvas(bitmap)`) canvases are not on the recording path and
//! are deliberately left unregistered → a structured `UNSUPPORTED_JAVA_API`,
//! never a silently empty display list.

use std::cell::RefCell;

use acos_render::canvas::ClipOp;
use acos_render::geom::{Rect, RectF};
use acos_render::handle::Handle as RHandle;
use acos_render::matrix::Matrix3x3;
use acos_render::render_node::DisplayOp;

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::natives::gfx_handles::{Handle, HandleTable};
use crate::natives::gfx_state::{rh, snapshot_paint, snapshot_path, stale, with_ctx};
use crate::vm::Vm;

/// A recording canvas staged against a `RenderNode`.
struct RecCanvas {
    node: RHandle,
    width: i32,
    height: i32,
    ops: Vec<DisplayOp>,
    /// Saved `(matrix, clip)` pairs, parallel to the `Save` ops.
    stack: Vec<(Matrix3x3, RectF)>,
    matrix: Matrix3x3,
    clip: RectF,
}

impl RecCanvas {
    fn new(node: RHandle, width: i32, height: i32) -> Self {
        RecCanvas {
            node,
            width,
            height,
            ops: Vec::new(),
            stack: Vec::new(),
            matrix: Matrix3x3::identity(),
            clip: RectF::new(0.0, 0.0, width.max(0) as f32, height.max(0) as f32),
        }
    }
}

thread_local! {
    static REC: RefCell<HandleTable<RecCanvas>> = RefCell::new(HandleTable::new());
    /// Opaque handles for raster `Canvas` objects. They exist only so
    /// `android.view.Surface`'s `CompatibleCanvas` can be constructed; no draw
    /// op resolves here, so raster drawing stays unsupported.
    static RASTER: RefCell<HandleTable<()>> = RefCell::new(HandleTable::new());
}

fn rec<R>(h: i64, f: impl FnOnce(&mut RecCanvas) -> R) -> Option<R> {
    REC.with(|t| t.borrow_mut().get_mut(Handle::from_jlong(h)).ok().map(f))
}

fn push(canvas: i64, op: DisplayOp) -> VmResult<()> {
    rec(canvas, |c| c.ops.push(op)).ok_or_else(|| stale("RecordingCanvas", canvas))?;
    Ok(())
}

fn snap(paint: i64) -> VmResult<RHandle> {
    if paint == 0 {
        Ok(RHandle(0))
    } else {
        Ok(rh(snapshot_paint(paint)?))
    }
}

fn snap_opt(paint: i64) -> VmResult<Option<RHandle>> {
    if paint == 0 {
        Ok(None)
    } else {
        Ok(Some(rh(snapshot_paint(paint)?)))
    }
}

/// Read the pending draw canvas handle and return it, erroring if stale.
fn canvas_handle(vm: &Vm, ctx: NativeCtx) -> VmResult<i64> {
    let h = vm.arg_i64(ctx, 0);
    if rec(h, |_| ()).is_some() {
        Ok(h)
    } else {
        Err(stale("Canvas", h))
    }
}

/// Register the recording-canvas natives.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    const C: &str = "Landroid/graphics/Canvas;";
    const B: &str = "Landroid/graphics/BaseCanvas;";
    const R: &str = "Landroid/graphics/BaseRecordingCanvas;";
    const RC: &str = "Landroid/graphics/RecordingCanvas;";

    // --- RecordingCanvas lifecycle (both class spellings are declared) ---
    vm.register_native(RC, "nCreateDisplayListCanvas", "(JII)J", rc_create);
    vm.register_native(RC, "nResetDisplayListCanvas", "(JJII)V", rc_reset);
    vm.register_native(RC, "nFinishRecording", "(JJ)V", rc_finish);
    vm.register_native(RC, "nEnableZ", "(JZ)V", noop);
    vm.register_native(RC, "nGetMaximumTextureWidth", "()I", ret_4096);
    vm.register_native(RC, "nGetMaximumTextureHeight", "()I", ret_4096);
    vm.register_native(RC, "nDrawRenderNode", "(JJ)V", rc_draw_render_node);

    // --- Canvas state ---
    vm.register_native(C, "nGetNativeFinalizer", "()J", ret_zero_long);
    // `Canvas()` calls `nInitRaster` to wrap a raster target. ACOS supports the
    // recording path only, but `android.view.Surface` constructs a
    // `CompatibleCanvas` in its constructor, so the compatibility handle must
    // exist. Returning the null raster handle lets `Surface`/`Surface.copyFrom`
    // work; every raster draw op still resolves through the recording table and
    // fails as stale, so nothing is silently rendered. W22-B edit (T-29).
    vm.register_native(C, "nInitRaster", "(J)J", c_init_raster);
    vm.register_native(C, "nFreeCaches", "()V", noop);
    vm.register_native(C, "nFreeTextLayoutCaches", "()V", noop);
    vm.register_native(C, "nGetSaveCount", "(J)I", c_save_count);
    vm.register_native(C, "nSave", "(JI)I", c_save);
    vm.register_native(C, "nRestore", "(J)Z", c_restore);
    vm.register_native(C, "nRestoreToCount", "(JI)V", c_restore_to_count);
    vm.register_native(C, "nTranslate", "(JFF)V", c_translate);
    vm.register_native(C, "nScale", "(JFF)V", c_scale);
    vm.register_native(C, "nRotate", "(JF)V", c_rotate);
    vm.register_native(C, "nSkew", "(JFF)V", c_skew);
    vm.register_native(C, "nConcat", "(JJ)V", c_concat);
    vm.register_native(C, "nSetMatrix", "(JJ)V", c_set_matrix);
    vm.register_native(C, "nGetMatrix", "(JJ)V", c_get_matrix);
    vm.register_native(C, "nClipRect", "(JFFFFI)Z", c_clip_rect);
    vm.register_native(C, "nClipPath", "(JJI)Z", c_clip_path);
    vm.register_native(C, "nGetClipBounds", "(JLandroid/graphics/Rect;)Z", c_get_clip_bounds);
    vm.register_native(C, "nQuickReject", "(JFFFF)Z", c_quick_reject_rect);
    vm.register_native(C, "nQuickReject", "(JJ)Z", c_quick_reject_rect);
    vm.register_native(C, "nGetWidth", "(J)I", c_width);
    vm.register_native(C, "nGetHeight", "(J)I", c_height);
    vm.register_native(C, "nIsOpaque", "(J)Z", ret_false);
    vm.register_native(C, "nSaveLayer", "(JFFFFJ)I", c_save_layer);
    vm.register_native(C, "nSaveLayerAlpha", "(JFFFFI)I", c_save_layer_alpha);
    vm.register_native(C, "nSetDrawFilter", "(JJ)V", noop);
    vm.register_native(C, "nSetBitmap", "(JJ)V", unsupported_bitmap_canvas);
    vm.register_native(C, "nSaveUnclippedLayer", "(JIIII)I", unsupported_unclipped);
    vm.register_native(C, "nRestoreUnclippedLayer", "(JIJ)V", unsupported_unclipped);

    // --- Draw ops (declared on both BaseCanvas and BaseRecordingCanvas) ---
    for class in [B, R] {
        vm.register_native(class, "nDrawRect", "(JFFFFJ)V", d_rect);
        vm.register_native(class, "nDrawRoundRect", "(JFFFFFFJ)V", d_round_rect);
        vm.register_native(class, "nDrawCircle", "(JFFFJ)V", d_circle);
        vm.register_native(class, "nDrawOval", "(JFFFFJ)V", d_oval);
        vm.register_native(class, "nDrawArc", "(JFFFFFFZJ)V", d_arc);
        vm.register_native(class, "nDrawLine", "(JFFFFJ)V", d_line);
        vm.register_native(class, "nDrawPoint", "(JFFJ)V", d_point);
        vm.register_native(class, "nDrawPath", "(JJJ)V", d_path);
        vm.register_native(class, "nDrawColor", "(JII)V", d_color);
        vm.register_native(class, "nDrawColor", "(JJJI)V", d_color_long);
        vm.register_native(class, "nDrawPaint", "(JJ)V", d_paint);
        vm.register_native(class, "nDrawText", "(JLjava/lang/String;IIFFIJ)V", d_text_string);
        vm.register_native(class, "nDrawText", "(J[CIIFFIJ)V", d_text_chars);
        vm.register_native(class, "nDrawBitmap", "(JJFFJIII)V", d_bitmap_xy);
        vm.register_native(class, "nDrawBitmap", "(JJFFFFFFFFJII)V", d_bitmap_rects);
        vm.register_native(class, "nDrawNinePatch", "(JJJFFFFJII)V", d_nine_patch);
    }
    // `BaseCanvas` declares the legacy round-rect without the extra radius pair.
    vm.register_native(B, "nDrawRoundRect", "(JFFFFFFJ)V", d_round_rect);
    Ok(())
}

// --- lifecycle --------------------------------------------------------------

fn rc_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = rh(vm.arg_i64(ctx, 0));
    if !with_ctx(|c| c.render_nodes.contains(node)) {
        return Err(stale("RenderNode", vm.arg_i64(ctx, 0)));
    }
    let w = vm.arg_i32(ctx, 1);
    let h = vm.arg_i32(ctx, 2);
    let h = REC.with(|t| gj_handle(t.borrow_mut().insert(RecCanvas::new(node, w, h))));
    vm.ret_i64(ctx, h);
    Ok(())
}

fn gj_handle(h: Handle) -> i64 {
    Handle::to_jlong(h)
}

fn rc_reset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let canvas = vm.arg_i64(ctx, 0);
    let node = rh(vm.arg_i64(ctx, 1));
    let w = vm.arg_i32(ctx, 2);
    let h = vm.arg_i32(ctx, 3);
    rec(canvas, |c| {
        *c = RecCanvas::new(node, w, h);
    })
    .ok_or_else(|| stale("RecordingCanvas", canvas))?;
    Ok(())
}

fn rc_finish(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let canvas = vm.arg_i64(ctx, 0);
    let node = rh(vm.arg_i64(ctx, 1));
    let (canvas_node, ops) = rec(canvas, |c| (c.node, std::mem::take(&mut c.ops)))
        .ok_or_else(|| stale("RecordingCanvas", canvas))?;
    if canvas_node != node {
        return Err(VmError::Internal(format!(
            "RecordingCanvas.finishRecording node mismatch: canvas bound to 0x{:016x}, finishing 0x{:016x}",
            canvas_node.0, node.0
        )));
    }
    // Publish the staged list atomically onto the node.
    with_ctx(|c| {
        if let Some(n) = c.render_nodes.get_mut(node) {
            n.set_display_list(ops);
        }
    });
    Ok(())
}

fn rc_draw_render_node(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let canvas = canvas_handle(vm, ctx)?;
    let child = vm.arg_i64(ctx, 1);
    let child = rh(child);
    if !with_ctx(|c| c.render_nodes.contains(child)) {
        return Err(stale("RenderNode", vm.arg_i64(ctx, 1)));
    }
    // Keep parent/child linkage so `surface_render_node` traverses it.
    push(canvas, DisplayOp::DrawRenderNode(child))?;
    Ok(())
}

// --- state ------------------------------------------------------------------

fn ret_zero_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}
fn ret_4096(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, 4096);
    Ok(())
}
fn ret_false(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}
fn noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

/// `Canvas.nInitRaster(J)J`: return a non-null compatibility handle for the
/// raster canvas. ACOS has no raster drawing path, so the handle is an empty
/// staged canvas: constructing it (which `android.view.Surface` always does)
/// succeeds, but every draw op still fails as a stale/empty canvas rather than
/// silently dropping pixels.
fn c_init_raster(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = RASTER.with(|t| t.borrow_mut().insert(()));
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}
fn unsupported_bitmap_canvas(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(VmError::unsupported_api(
        "Canvas.nSetBitmap (raster canvas)",
        "android.graphics.Canvas",
    ))
}
fn unsupported_unclipped(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(VmError::unsupported_api(
        "Canvas unclipped layer",
        "android.graphics.Canvas",
    ))
}

fn c_save_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let n = rec(h, |c| c.stack.len() as i32 + 1).unwrap_or(1);
    vm.ret_i32(ctx, n);
    Ok(())
}

fn c_save(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let n = rec(h, |c| {
        c.stack.push((c.matrix, c.clip));
        c.ops.push(DisplayOp::Save);
        c.stack.len() as i32 + 1
    })
    .unwrap_or(1);
    vm.ret_i32(ctx, n);
    Ok(())
}

fn c_restore(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let changed = rec(h, |c| {
        if let Some((m, clip)) = c.stack.pop() {
            c.matrix = m;
            c.clip = clip;
            c.ops.push(DisplayOp::Restore);
            true
        } else {
            false
        }
    })
    .unwrap_or(false);
    vm.ret_bool(ctx, changed);
    Ok(())
}

fn c_restore_to_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let count = vm.arg_i32(ctx, 1);
    rec(h, |c| {
        while c.stack.len() as i32 + 1 > count.max(1) {
            if let Some((m, clip)) = c.stack.pop() {
                c.matrix = m;
                c.clip = clip;
                c.ops.push(DisplayOp::Restore);
            } else {
                break;
            }
        }
    });
    Ok(())
}

fn c_translate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let (dx, dy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    rec(h, |c| {
        c.matrix.pre_translate(dx, dy);
        c.ops.push(DisplayOp::Concat(Matrix3x3::translate_matrix(dx, dy)));
    });
    Ok(())
}

fn c_scale(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let (sx, sy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    rec(h, |c| {
        c.matrix.pre_scale(sx, sy);
        c.ops.push(DisplayOp::Concat(Matrix3x3::scale_matrix(sx, sy)));
    });
    Ok(())
}

fn c_rotate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let deg = vm.arg_f32(ctx, 1);
    rec(h, |c| {
        c.matrix.pre_rotate(deg);
        c.ops.push(DisplayOp::Concat(Matrix3x3::rotate_matrix(deg)));
    });
    Ok(())
}

fn c_skew(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let (kx, ky) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    rec(h, |c| {
        c.matrix.pre_concat(&Matrix3x3::skew_matrix(kx, ky));
        c.ops.push(DisplayOp::Concat(Matrix3x3::skew_matrix(kx, ky)));
    });
    Ok(())
}

fn read_matrix(vm: &Vm, ctx: NativeCtx, n: usize) -> VmResult<Matrix3x3> {
    let j = vm.arg_i64(ctx, n);
    let v = crate::natives::gfx_matrix::matrix_values(j)?;
    Ok(crate::natives::gfx_matrix::matrix_from_values(v))
}

fn c_concat(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let val = read_matrix(vm, ctx, 1)?;
    rec(h, |c| {
        c.matrix.pre_concat(&val);
        c.ops.push(DisplayOp::Concat(val));
    });
    Ok(())
}

fn c_set_matrix(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let val = read_matrix(vm, ctx, 1)?;
    rec(h, |c| {
        c.matrix = val;
        c.ops.push(DisplayOp::SetMatrix(val));
    });
    Ok(())
}

fn c_get_matrix(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let m = rec(h, |c| c.matrix).unwrap_or_else(Matrix3x3::identity);
    crate::natives::gfx_matrix::matrix_store(vm.arg_i64(ctx, 1), m)?;
    Ok(())
}

fn c_clip_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let r = RectF::new(
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let op = ClipOp::from_ordinal(vm.arg_i32(ctx, 5)).unwrap_or(ClipOp::Intersect);
    if op == ClipOp::ReverseDifference {
        return Err(VmError::unsupported_api(
            "Canvas.clipRect ReverseDifference",
            "android.graphics.Canvas",
        ));
    }
    rec(h, |c| {
        c.ops.push(DisplayOp::ClipRect(r, op, false));
        c.clip = intersect(c.clip, r);
    });
    vm.ret_bool(ctx, true);
    Ok(())
}

fn c_clip_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let path = rh(snapshot_path(vm.arg_i64(ctx, 1))?);
    let op = ClipOp::from_ordinal(vm.arg_i32(ctx, 2)).unwrap_or(ClipOp::Intersect);
    rec(h, |c| c.ops.push(DisplayOp::ClipPath(path, op, true)));
    vm.ret_bool(ctx, true);
    Ok(())
}

fn c_get_clip_bounds(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let clip = rec(h, |c| c.clip).unwrap_or_default();
    let obj = vm.arg_ref(ctx, 1);
    vm.set_instance_field_by_name(obj, "left", clip.left.floor() as i32 as u32);
    vm.set_instance_field_by_name(obj, "top", clip.top.floor() as i32 as u32);
    vm.set_instance_field_by_name(obj, "right", clip.right.ceil() as i32 as u32);
    vm.set_instance_field_by_name(obj, "bottom", clip.bottom.ceil() as i32 as u32);
    vm.ret_bool(ctx, true);
    Ok(())
}

fn c_quick_reject_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    // Overload 1 is `(JFFFF)`, overload 2 is `(JJ)` (a Rect handle). Both need
    // the same bounds; read the four floats for the first and the fields for
    // the second.
    let r = if cur_sig(vm, ctx) == "(JFFFF)Z" {
        RectF::new(
            vm.arg_f32(ctx, 1),
            vm.arg_f32(ctx, 2),
            vm.arg_f32(ctx, 3),
            vm.arg_f32(ctx, 4),
        )
    } else {
        let obj = vm.arg_ref(ctx, 1);
        let l = vm.get_instance_field_by_name(obj, "left").unwrap_or(0) as i32;
        let t = vm.get_instance_field_by_name(obj, "top").unwrap_or(0) as i32;
        let rr = vm.get_instance_field_by_name(obj, "right").unwrap_or(0) as i32;
        let b = vm.get_instance_field_by_name(obj, "bottom").unwrap_or(0) as i32;
        RectF::new(l as f32, t as f32, rr as f32, b as f32)
    };
    let clip = rec(h, |c| c.clip).unwrap_or_default();
    vm.ret_bool(ctx, !overlaps(clip, r));
    Ok(())
}

fn c_width(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    vm.ret_i32(ctx, rec(h, |c| c.width).unwrap_or(0));
    Ok(())
}

fn c_height(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    vm.ret_i32(ctx, rec(h, |c| c.height).unwrap_or(0));
    Ok(())
}

fn c_save_layer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let bounds = RectF::new(
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let paint = vm.arg_i64(ctx, 5);
    let alpha = if paint == 0 {
        255u8
    } else {
        crate::natives::gfx_paint::paint_rec(paint)
            .map(|p| (p.alpha & 0xff) as u8)
            .unwrap_or(255)
    };
    let n = rec(h, |c| {
        c.stack.push((c.matrix, c.clip));
        c.ops.push(DisplayOp::SaveLayerAlpha(Some(bounds), alpha));
        c.stack.len() as i32 + 1
    })
    .unwrap_or(1);
    vm.ret_i32(ctx, n);
    Ok(())
}

fn c_save_layer_alpha(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let bounds = RectF::new(
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let alpha = vm.arg_i32(ctx, 5) as u8;
    let n = rec(h, |c| {
        c.stack.push((c.matrix, c.clip));
        c.ops.push(DisplayOp::SaveLayerAlpha(Some(bounds), alpha));
        c.stack.len() as i32 + 1
    })
    .unwrap_or(1);
    vm.ret_i32(ctx, n);
    Ok(())
}

// --- draw ops ---------------------------------------------------------------

fn d_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let r = RectF::new(
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let p = snap(vm.arg_i64(ctx, 5))?;
    push(h, DisplayOp::DrawRect(r, p))
}

fn d_round_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let r = RectF::new(
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let (rx, ry) = (vm.arg_f32(ctx, 5), vm.arg_f32(ctx, 6));
    let p = snap(vm.arg_i64(ctx, 7))?;
    push(h, DisplayOp::DrawRoundRect(r, rx, ry, p))
}

fn d_circle(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let (cx, cy, rad) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2), vm.arg_f32(ctx, 3));
    let p = snap(vm.arg_i64(ctx, 4))?;
    push(h, DisplayOp::DrawCircle(cx, cy, rad, p))
}

fn d_oval(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let r = RectF::new(
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let p = snap(vm.arg_i64(ctx, 5))?;
    push(h, DisplayOp::DrawOval(r, p))
}

fn d_arc(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let r = RectF::new(
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let (start, sweep) = (vm.arg_f32(ctx, 5), vm.arg_f32(ctx, 6));
    let use_center = vm.arg_bool(ctx, 7);
    let p = snap(vm.arg_i64(ctx, 8))?;
    push(h, DisplayOp::DrawArc(r, start, sweep, use_center, p))
}

fn d_line(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let (x0, y0, x1, y1) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let p = snap(vm.arg_i64(ctx, 5))?;
    push(h, DisplayOp::DrawLine(x0, y0, x1, y1, p))
}

fn d_point(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let (x, y) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    let p = snap(vm.arg_i64(ctx, 3))?;
    push(h, DisplayOp::DrawPoint(x, y, p))
}

fn d_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let path = rh(snapshot_path(vm.arg_i64(ctx, 1))?);
    let p = snap(vm.arg_i64(ctx, 2))?;
    push(h, DisplayOp::DrawPath(path, p))
}

fn d_color(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let color = vm.arg_i32(ctx, 1) as u32;
    push(h, DisplayOp::DrawColor(color))
}

fn d_color_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let color = vm.arg_i64(ctx, 1) as u32;
    push(h, DisplayOp::DrawColor(color))
}

fn d_paint(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let p = snap(vm.arg_i64(ctx, 1))?;
    let clip = rec(h, |c| c.clip).unwrap_or_default();
    push(h, DisplayOp::DrawRect(clip, p))
}

fn cur_sig(vm: &Vm, ctx: NativeCtx) -> String {
    let m = vm.ctx_method_id(ctx) as usize;
    vm.methods[m].sig.clone()
}

fn d_text_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let (start, end) = (vm.arg_i32(ctx, 2), vm.arg_i32(ctx, 3));
    let obj = vm.arg_ref(ctx, 1);
    let all: Vec<u16> = vm
        .string_to_rust(obj)
        .map(|s| s.encode_utf16().collect())
        .unwrap_or_default();
    let a = start.max(0) as usize;
    let b = end.max(0) as usize;
    let text = if a <= b && b <= all.len() { all[a..b].to_vec() } else { all };
    let (x, y) = (vm.arg_f32(ctx, 4), vm.arg_f32(ctx, 5));
    let p = snap(vm.arg_i64(ctx, 7))?;
    push(h, DisplayOp::DrawText(text, x, y, p))
}

fn d_text_chars(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let (start, end) = (vm.arg_i32(ctx, 2), vm.arg_i32(ctx, 3));
    let arr = vm.arg_ref(ctx, 1);
    let all = match vm.heap.get(arr) {
        Some(crate::heap::Object::Array(a)) => match &a.data {
            crate::heap::ArrayData::Char(v) => v.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let a = start.max(0) as usize;
    let b = end.max(0) as usize;
    let text = if a <= b && b <= all.len() { all[a..b].to_vec() } else { all };
    let (x, y) = (vm.arg_f32(ctx, 4), vm.arg_f32(ctx, 5));
    let p = snap(vm.arg_i64(ctx, 7))?;
    push(h, DisplayOp::DrawText(text, x, y, p))
}

fn bitmap_bounds(bitmap: i64) -> Option<(u32, u32)> {
    with_ctx(|c| c.bitmaps.get(rh(bitmap)).map(|b| (b.width(), b.height())))
}

fn d_bitmap_xy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let bitmap = vm.arg_i64(ctx, 1);
    let Some((w, ht)) = bitmap_bounds(bitmap) else {
        return Err(stale("Bitmap", bitmap));
    };
    let (left, top) = (vm.arg_f32(ctx, 2), vm.arg_f32(ctx, 3));
    let p = snap_opt(vm.arg_i64(ctx, 4))?;
    let dst = RectF::new(left, top, left + w as f32, top + ht as f32);
    push(h, DisplayOp::DrawBitmap(rh(bitmap), None, dst, p, true))
}

fn d_bitmap_rects(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let bitmap = vm.arg_i64(ctx, 1);
    if !with_ctx(|c| c.bitmaps.contains(rh(bitmap))) {
        return Err(stale("Bitmap", bitmap));
    }
    let src = RectF::new(
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
        vm.arg_f32(ctx, 5),
    );
    let dst = RectF::new(
        vm.arg_f32(ctx, 6),
        vm.arg_f32(ctx, 7),
        vm.arg_f32(ctx, 8),
        vm.arg_f32(ctx, 9),
    );
    let p = snap_opt(vm.arg_i64(ctx, 10))?;
    push(h, DisplayOp::DrawBitmap(rh(bitmap), Some(src), dst, p, true))
}

fn d_nine_patch(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let bitmap = vm.arg_i64(ctx, 1);
    let Some((w, ht)) = bitmap_bounds(bitmap) else {
        return Err(stale("Bitmap", bitmap));
    };
    let dst = RectF::new(
        vm.arg_f32(ctx, 4),
        vm.arg_f32(ctx, 5),
        vm.arg_f32(ctx, 6),
        vm.arg_f32(ctx, 7),
    );
    let p = snap(vm.arg_i64(ctx, 8))?;
    let src = Rect::new(0, 0, w as i32, ht as i32);
    let dst_i = Rect::new(
        dst.left as i32,
        dst.top as i32,
        dst.right as i32,
        dst.bottom as i32,
    );
    push(h, DisplayOp::DrawNinePatch(rh(bitmap), src, dst_i, p, true))
}

fn intersect(a: RectF, b: RectF) -> RectF {
    RectF::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}

fn overlaps(a: RectF, b: RectF) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}
