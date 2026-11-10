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
use acos_render::geom::{NinePatchChunk, Rect, RectF};
use acos_render::handle::Handle as RHandle;
use acos_render::matrix::Matrix3x3;
use acos_render::render_node::{DisplayOp, RenderNode};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, Object};
use crate::natives::gfx_handles::{Handle, HandleTable};
use crate::natives::gfx_state::{render_err, rh, snapshot_paint, snapshot_path, stale, with_ctx};
use crate::vm::Vm;

/// Nine-patch chunks parsed by `NinePatch.validateNinePatchChunk`, keyed by the
/// `jlong` the framework holds in `NinePatch.mNativeChunk`.
static NINE_PATCHES: std::sync::OnceLock<std::sync::Mutex<HandleTable<NinePatchChunk>>> =
    std::sync::OnceLock::new();

fn nine_patches() -> &'static std::sync::Mutex<HandleTable<NinePatchChunk>> {
    NINE_PATCHES.get_or_init(|| std::sync::Mutex::new(HandleTable::new()))
}

/// A recording canvas staged against a `RenderNode`.
struct RecCanvas {
    node: RHandle,
    width: i32,
    height: i32,
    ops: Vec<DisplayOp>,
    /// Saved `(matrix, clip)` pairs, parallel to the `Save` ops.
    stack: Vec<(Matrix3x3, RectF)>,
    /// Op indices of `SaveLayerAlpha` ops recorded by `nSaveUnclippedLayer`,
    /// patched with the restore paint's alpha before the layer is popped.
    unclipped: Vec<usize>,
    matrix: Matrix3x3,
    clip: RectF,
    /// `Some` for a raster `Canvas`: every draw op is flushed immediately into
    /// this bitmap. `None` is the recording `RecordingCanvas` path.
    raster: Option<RasterTarget>,
}

/// A raster `Canvas` target. The display list accumulates on `node` and is
/// replayed over the bitmap's current contents after each draw op.
struct RasterTarget {
    bitmap: i64,
    /// Snapshot of the bitmap's pixels when it was bound; every flush replays
    /// the whole display list over this base so ops are never double-applied.
    base: Vec<u32>,
}

impl RecCanvas {
    fn new(node: RHandle, width: i32, height: i32) -> Self {
        RecCanvas {
            node,
            width,
            height,
            ops: Vec::new(),
            stack: Vec::new(),
            unclipped: Vec::new(),
            matrix: Matrix3x3::identity(),
            clip: RectF::new(0.0, 0.0, width.max(0) as f32, height.max(0) as f32),
            raster: None,
        }
    }

    fn new_raster(node: RHandle, width: i32, height: i32, bitmap: i64, base: Vec<u32>) -> Self {
        let mut c = RecCanvas::new(node, width, height);
        c.raster = Some(RasterTarget { bitmap, base });
        c
    }
}

thread_local! {
    static REC: RefCell<HandleTable<RecCanvas>> = RefCell::new(HandleTable::new());
}

fn rec<R>(h: i64, f: impl FnOnce(&mut RecCanvas) -> R) -> Option<R> {
    REC.with(|t| t.borrow_mut().get_mut(Handle::from_jlong(h)).ok().map(f))
}

fn push(canvas: i64, op: DisplayOp) -> VmResult<()> {
    let raster = rec(canvas, |c| {
        c.ops.push(op);
        c.raster.is_some()
    })
    .ok_or_else(|| stale("Canvas", canvas))?;
    if raster {
        flush_raster(canvas)?;
    }
    Ok(())
}

/// Push a display op onto a canvas from another gfx native module (the
/// `VectorDrawable` tree emits Save/Concat/ClipPath/DrawPath at `nDraw` time).
pub(crate) fn push_op(canvas: i64, op: DisplayOp) -> VmResult<()> {
    push(canvas, op)
}

/// Whether a canvas handle resolves. Used by `gfx_vector::nDraw` so an invalid
/// canvas is a zero-pixel no-op instead of a panic.
pub(crate) fn canvas_exists(canvas: i64) -> bool {
    rec(canvas, |_| ()).is_some()
}

/// Replay a raster canvas's full display list over its bitmap.
fn flush_raster(canvas: i64) -> VmResult<()> {
    let Some((bitmap, node, ops, base)) = rec(canvas, |c| {
        c.raster
            .as_ref()
            .map(|r| (r.bitmap, c.node, c.ops.clone(), r.base.clone()))
    })
    .flatten()
    else {
        return Ok(());
    };
    if bitmap == 0 {
        return Err(VmError::unsupported_api(
            "Canvas draw before Canvas.setBitmap (raster canvas has no bitmap)",
            "android.graphics.Canvas",
        ));
    }
    with_ctx(|c| -> VmResult<()> {
        if !c.bitmaps.contains(rh(bitmap)) {
            return Err(stale("Bitmap", bitmap));
        }
        if let Some(n) = c.render_nodes.get_mut(node) {
            n.set_display_list(ops);
        }
        c.bitmap_render_node_over(rh(bitmap), node, &base)
            .map_err(|e| render_err("Canvas raster draw", e))?;
        Ok(())
    })
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
    // `Canvas()` / `new Canvas(bitmap)` wrap a raster (bitmap-backed) target.
    // The argument is the ACOS bitmap handle (`Bitmap.getNativeInstance`), or 0
    // for a `Canvas()` that will receive `setBitmap` later. Raster draw ops are
    // replayed over the bitmap's current pixels after each op.
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
    vm.register_native(C, "nSetBitmap", "(JJ)V", c_set_bitmap);
    vm.register_native(C, "nSaveUnclippedLayer", "(JIIII)I", c_save_unclipped_layer);
    vm.register_native(C, "nRestoreUnclippedLayer", "(JIJ)V", c_restore_unclipped_layer);

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
        vm.register_native(class, "nDrawTextRun", "(JLjava/lang/String;IIIIFFZJ)V", d_text_run);
        vm.register_native(class, "nDrawTextRun", "(J[CIIIIFFZJJ)V", d_text_run);
        vm.register_native(class, "nDrawBitmap", "(JJFFJIII)V", d_bitmap_xy);
        vm.register_native(class, "nDrawBitmap", "(JJFFFFFFFFJII)V", d_bitmap_rects);
        vm.register_native(class, "nDrawNinePatch", "(JJJFFFFJII)V", d_nine_patch);
    }
    // --- Nine-patch chunk validation (W78-A) ---
    let np = "Landroid/graphics/NinePatch;";
    vm.register_native(np, "validateNinePatchChunk", "([B)J", np_validate);
    vm.register_native(np, "isNinePatchChunk", "([B)Z", np_is_valid);
    vm.register_native(np, "nativeFinalize", "(J)V", noop);
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
    if std::env::var_os("ACOS_RENDER_VIEWS_VERBOSE").is_some() {
        let text_ops = ops
            .iter()
            .filter(|o| matches!(o, DisplayOp::DrawText(..)))
            .count();
        eprintln!(
            "RecordingCanvas.finishRecording ops={} text={}",
            ops.len(),
            text_ops
        );
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

/// `Canvas.nInitRaster(J)J`: create a raster canvas. The argument is the ACOS
/// bitmap handle for `new Canvas(bitmap)`, or 0 for `Canvas()` (followed by
/// `nSetBitmap`). A render node backs the accumulating display list.
fn c_init_raster(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let bitmap = vm.arg_i64(ctx, 0);
    // `Canvas()` passes 0; `Surface`'s CompatibleCanvas may pass an opaque
    // native pointer. Only bind when it resolves to a real bitmap, otherwise
    // leave the canvas unbound so a later draw fails with a structured error.
    let (w, h, base) = with_ctx(|c| {
        c.bitmaps
            .get(rh(bitmap))
            .map(|b| (b.width() as i32, b.height() as i32, b.pixels().to_vec()))
    })
    .unwrap_or((1, 1, Vec::new()));
    let bitmap = if w == 1 && h == 1 && base.is_empty() && bitmap != 0 {
        // Unknown/non-bitmap pointer: keep it unbound rather than mis-binding.
        0
    } else {
        bitmap
    };
    let node = with_ctx(|c| c.render_nodes.insert(RenderNode::new("raster-canvas")));
    with_ctx(|c| {
        if let Some(n) = c.render_nodes.get_mut(node) {
            n.left = 0;
            n.top = 0;
            n.right = w;
            n.bottom = h;
        }
    });
    let hh = rec_handle(RecCanvas::new_raster(node, w, h, bitmap, base));
    vm.ret_i64(ctx, hh);
    Ok(())
}

fn rec_handle(c: RecCanvas) -> i64 {
    REC.with(|t| Handle::to_jlong(t.borrow_mut().insert(c)))
}

/// `Canvas.nSetBitmap(J bitmap)`: bind a raster canvas to a bitmap (or unbind
/// with 0), matching `Canvas.setBitmap`.
fn c_set_bitmap(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let canvas = canvas_handle(vm, ctx)?;
    let bitmap = vm.arg_i64(ctx, 1);
    if bitmap != 0 && !with_ctx(|c| c.bitmaps.contains(rh(bitmap))) {
        return Err(stale("Bitmap", bitmap));
    }
    let node = rec(canvas, |c| {
        let Some(r) = &mut c.raster else {
            return None;
        };
        r.bitmap = bitmap;
        r.base = if bitmap == 0 {
            Vec::new()
        } else {
            with_ctx(|x| x.bitmaps.get(rh(bitmap)).map(|b| b.pixels().to_vec()))
                .unwrap_or_default()
        };
        // `setBitmap` re-initialises the canvas state and clears the list.
        c.ops.clear();
        c.stack.clear();
        c.unclipped.clear();
        c.matrix = Matrix3x3::identity();
        c.clip = RectF::new(0.0, 0.0, c.width.max(0) as f32, c.height.max(0) as f32);
        if bitmap != 0 {
            if let Some((bw, bh)) = with_ctx(|x| {
                x.bitmaps.get(rh(bitmap)).map(|b| (b.width(), b.height()))
            }) {
                c.width = bw as i32;
                c.height = bh as i32;
                c.clip = RectF::new(0.0, 0.0, bw as f32, bh as f32);
            }
        }
        Some((c.width, c.height, c.node))
    })
    .ok_or_else(|| stale("Canvas", canvas))?;
    if let Some((w, h, node)) = node {
        with_ctx(|c| {
            if let Some(n) = c.render_nodes.get_mut(node) {
                n.left = 0;
                n.top = 0;
                n.right = w;
                n.bottom = h;
            }
        });
    }
    Ok(())
}

/// `Canvas.nSaveUnclippedLayer(int l, int t, int r, int b)`: save a layer that
/// ignores the current clip. Implemented as a `Save`, a `clipRect(Replace,
/// bounds)`, and a `SaveLayerAlpha`, so subsequent drawing sees a bare `bounds`
/// clip and is composited as a group on restore.
fn c_save_unclipped_layer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let bounds = RectF::new(
        vm.arg_i32(ctx, 1) as f32,
        vm.arg_i32(ctx, 2) as f32,
        vm.arg_i32(ctx, 3) as f32,
        vm.arg_i32(ctx, 4) as f32,
    );
    let n = rec(h, |c| {
        c.stack.push((c.matrix, c.clip));
        c.ops.push(DisplayOp::Save);
        // `bounds` is in the current (local) coordinate space; the tracked clip
        // is kept in device/root coordinates, so map it through the matrix.
        c.clip = c
            .matrix
            .map_rect(&bounds)
            .unwrap_or_else(|| RectF::new(0.0, 0.0, 0.0, 0.0));
        c.ops.push(DisplayOp::ClipRect(bounds, ClipOp::Replace, false));
        c.ops.push(DisplayOp::SaveLayerAlpha(Some(bounds), 255));
        c.unclipped.push(c.ops.len() - 1);
        c.stack.len() as i32 + 1
    })
    .ok_or_else(|| stale("Canvas", h))?;
    vm.ret_i32(ctx, n);
    Ok(())
}

/// `Canvas.nRestoreUnclippedLayer(int saveCount, Paint paint)`: composite the
/// unclipped layer using `paint`'s alpha, then restore the pre-save state.
fn c_restore_unclipped_layer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let paint = vm.arg_i64(ctx, 2);
    let alpha = if paint == 0 {
        255u8
    } else {
        crate::natives::gfx_paint::paint_rec(paint)
            .map(|p| (p.alpha & 0xff) as u8)
            .unwrap_or(255)
    };
    rec(h, |c| {
        // Patch the recorded layer's alpha; the restore paint is only known
        // when the layer is popped.
        if let Some(idx) = c.unclipped.pop() {
            if let Some(DisplayOp::SaveLayerAlpha(_, a)) = c.ops.get_mut(idx) {
                *a = alpha;
            }
        }
        // Pop the unclipped-layer state, then the outer save.
        if let Some((m, clip)) = c.stack.pop() {
            c.matrix = m;
            c.clip = clip;
            c.ops.push(DisplayOp::Restore);
        }
        if let Some((m, clip)) = c.stack.pop() {
            c.matrix = m;
            c.clip = clip;
            c.ops.push(DisplayOp::Restore);
        }
    })
    .ok_or_else(|| stale("Canvas", h))?;
    Ok(())
}

fn c_save_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let n = rec(h, |c| c.stack.len() as i32 + 1).unwrap_or(1);
    vm.ret_i32(ctx, n);
    Ok(())
}

fn c_save(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    // `Canvas.save()` returns the value `restoreToCount` takes to undo it:
    // Skia returns the *post-save* depth (before it was `len`, now `len+1`).
    // Returning `len+1` here made `View.draw`'s `restoreToCount(saveCount)`
    // no-op, so the child's clip leaked into every later op and the frame was
    // clipped to the first child's bounds (canvas-shapes SSIM 0.02).
    let n = rec(h, |c| {
        c.stack.push((c.matrix, c.clip));
        c.ops.push(DisplayOp::Save);
        c.stack.len() as i32
    })
    .unwrap_or(0);
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
    rec(h, |c| {
        c.ops.push(DisplayOp::ClipRect(r, op, false));
        // The argument is in the current (local) coordinate space, but the
        // tracked clip is kept in device/root coordinates so successive
        // intersects compose correctly under a translated/concatenated canvas.
        // (Not mapping it made `getClipBounds` return a local-sized rect at a
        // parent-space offset — e.g. `[42,42,9,18]` — so `Layout.draw` saw an
        // empty clip and every `TextView`/`Button` label vanished.)
        let device = c
            .matrix
            .map_rect(&r)
            .unwrap_or_else(|| RectF::new(0.0, 0.0, 0.0, 0.0));
        // Only Intersect and Replace stay rectangular; Difference and
        // ReverseDifference are left to the rasteriser's mask.
        c.clip = match op {
            ClipOp::Replace => device,
            ClipOp::Intersect => intersect(c.clip, device),
            ClipOp::Difference | ClipOp::ReverseDifference => c.clip,
        };
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
    // The tracked clip is in device/root coordinates; `getClipBounds` must
    // report it in the canvas's current local coordinate space.
    let clip = rec(h, |c| {
        c.matrix
            .inverted()
            .and_then(|inv| inv.map_rect(&c.clip))
            .unwrap_or(c.clip)
    })
    .unwrap_or_default();
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
    let (clip, matrix) = rec(h, |c| (c.clip, c.matrix)).unwrap_or_default();
    let device = matrix
        .map_rect(&r)
        .unwrap_or_else(|| RectF::new(0.0, 0.0, 0.0, 0.0));
    vm.ret_bool(ctx, !overlaps(clip, device));
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
        c.stack.len() as i32
    })
    .unwrap_or(0);
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
        c.stack.len() as i32
    })
    .unwrap_or(0);
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
    // The op is recorded in local coordinates and transformed on replay, so
    // report the tracked device clip back in this canvas's local space.
    let clip = rec(h, |c| {
        c.matrix
            .inverted()
            .and_then(|inv| inv.map_rect(&c.clip))
            .unwrap_or(c.clip)
    })
    .unwrap_or_default();
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

/// `Canvas.nDrawTextRun` (both overloads). `Layout`/`TextLine` — every
/// `TextView` and `Button` label — reach the canvas through this entry point,
/// not `drawText`, so without it those labels never reach the display list.
/// The overloads differ only in the text carrier (`String` vs `char[]`) and an
/// extra trailing `precomputedText` handle on the `char[]` form; the geometry
/// args (`start`,`end`,`context*`,`x`,`y`,`isRtl`) and the paint are identical.
fn d_text_run(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = canvas_handle(vm, ctx)?;
    let (start, end) = (vm.arg_i32(ctx, 2), vm.arg_i32(ctx, 3));
    let carrier = vm.arg_ref(ctx, 1);
    let all: Vec<u16> = if let Some(s) = vm.string_to_rust(carrier) {
        s.encode_utf16().collect()
    } else {
        match vm.heap.get(carrier) {
            Some(crate::heap::Object::Array(a)) => match &a.data {
                crate::heap::ArrayData::Char(v) => v.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    };
    let a = start.max(0) as usize;
    let b = end.max(0) as usize;
    let text = if a <= b && b <= all.len() { all[a..b].to_vec() } else { all };
    let (x, y) = (vm.arg_f32(ctx, 6), vm.arg_f32(ctx, 7));
    let p = snap(vm.arg_i64(ctx, 9))?;
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
    // `nDrawNinePatch(long canvas, long bitmap, long ninePatch,
    //   float left, float top, float right, float bottom, long paint,
    //   int dstGamma, int srcGamma)` — the rect is args 3..=6 and the paint is
    // arg 7.
    let h = canvas_handle(vm, ctx)?;
    let bitmap = vm.arg_i64(ctx, 1);
    if bitmap_bounds(bitmap).is_none() {
        return Err(stale("Bitmap", bitmap));
    }
    let chunk = nine_patch_chunk(vm.arg_i64(ctx, 2))?;
    let dst = RectF::new(
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
        vm.arg_f32(ctx, 5),
        vm.arg_f32(ctx, 6),
    );
    let p = snap(vm.arg_i64(ctx, 7))?;
    let dst_i = Rect::new(
        dst.left as i32,
        dst.top as i32,
        dst.right as i32,
        dst.bottom as i32,
    );
    push(h, DisplayOp::DrawNinePatch(rh(bitmap), chunk, dst_i, p, true))
}

fn nine_patch_chunk(handle: i64) -> VmResult<NinePatchChunk> {
    if handle == 0 {
        return Ok(NinePatchChunk::default());
    }
    nine_patches()
        .lock()
        .map_err(|_| VmError::Internal("NinePatch table poisoned".to_string()))?
        .get(Handle::from_jlong(handle))
        .cloned()
        .map_err(|e| VmError::Internal(format!("NinePatch chunk: {e}")))
}

fn read_byte_array(vm: &Vm, h: u32) -> Option<Vec<u8>> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Byte(v) => Some(v.iter().map(|&b| b as u8).collect()),
            _ => None,
        },
        _ => None,
    }
}

fn np_validate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_ref(ctx, 0);
    let bytes = read_byte_array(vm, h).ok_or_else(|| {
        VmError::Internal("NinePatch.validateNinePatchChunk: expected byte[]".to_string())
    })?;
    let chunk = NinePatchChunk::parse(&bytes).ok_or_else(|| {
        VmError::Internal("NinePatch.validateNinePatchChunk: invalid npTc chunk".to_string())
    })?;
    let handle = nine_patches()
        .lock()
        .map_err(|_| VmError::Internal("NinePatch table poisoned".to_string()))?
        .insert(chunk)
        .to_jlong();
    vm.ret_i64(ctx, handle);
    Ok(())
}

fn np_is_valid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_ref(ctx, 0);
    let valid = read_byte_array(vm, h)
        .map(|b| NinePatchChunk::parse(&b).is_some())
        .unwrap_or(false);
    vm.ret_bool(ctx, valid);
    Ok(())
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
