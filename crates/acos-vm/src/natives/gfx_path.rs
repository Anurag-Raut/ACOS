//! `android.graphics.Path` and `android.graphics.Region` natives.
//!
//! Paths and regions are stored in generation-checked handle tables
//! ([`super::gfx_handles`]) and every operation is delegated to
//! `acos-render`'s [`Path`]/[`Region`], so bounds/booleans agree with the
//! rasteriser. Boolean `Path.op` is genuinely unsupported by the renderer
//! (tiny-skia has no path boolean ops): it raises a structured
//! `UNSUPPORTED_JAVA_API` rather than returning a wrong path.

use std::sync::{Mutex, MutexGuard, OnceLock};

use acos_render::geom::{Rect, RectF};
use acos_render::path::{FillType, Path, PathOp};
use acos_render::region::{Region, RegionOp};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::natives::gfx_handles::{Handle, HandleTable};
use crate::vm::Vm;

static PATHS: OnceLock<Mutex<HandleTable<Path>>> = OnceLock::new();
static REGIONS: OnceLock<Mutex<HandleTable<Region>>> = OnceLock::new();

fn paths() -> MutexGuard<'static, HandleTable<Path>> {
    let m = PATHS.get_or_init(|| Mutex::new(HandleTable::new()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn regions() -> MutexGuard<'static, HandleTable<Region>> {
    let m = REGIONS.get_or_init(|| Mutex::new(HandleTable::new()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn path_err(e: crate::natives::gfx_handles::HandleError) -> VmError {
    VmError::Internal(format!("Path: {e}"))
}

fn region_err(e: crate::natives::gfx_handles::HandleError) -> VmError {
    VmError::Internal(format!("Region: {e}"))
}

/// Replace the contents of an existing path handle (used by `Paint.getTextPath`).
pub fn replace_path(handle: i64, path: Path) -> VmResult<()> {
    let mut g = paths();
    *g.get_mut(Handle::from_jlong(handle)).map_err(path_err)? = path;
    Ok(())
}

/// Clone the `Path` behind a handle (used by `gfx_canvas` to freeze a path into
/// a recorded display op, so later mutation cannot change the op).
pub fn path_clone(handle: i64) -> VmResult<Path> {
    let g = paths();
    Ok(g.get(Handle::from_jlong(handle)).map_err(path_err)?.clone())
}

fn with_path<R>(handle: i64, f: impl FnOnce(&mut Path) -> R) -> VmResult<R> {
    let mut g = paths();
    let p = g.get_mut(Handle::from_jlong(handle)).map_err(path_err)?;
    Ok(f(p))
}

fn read_path<R>(handle: i64, f: impl FnOnce(&Path) -> R) -> VmResult<R> {
    let g = paths();
    let p = g.get(Handle::from_jlong(handle)).map_err(path_err)?;
    Ok(f(p))
}

fn read_region<R>(handle: i64, f: impl FnOnce(&Region) -> R) -> VmResult<R> {
    let g = regions();
    let r = g.get(Handle::from_jlong(handle)).map_err(region_err)?;
    Ok(f(r))
}

fn new_path(vm: &mut Vm, ctx: NativeCtx, path: Path) -> VmResult<()> {
    let h = paths().insert(path).to_jlong();
    vm.ret_i64(ctx, h);
    Ok(())
}

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let p = "Landroid/graphics/Path;";
    vm.register_native(p, "nInit", "()J", path_init);
    vm.register_native(p, "nInit", "(J)J", path_init_copy);
    vm.register_native(p, "nGetFinalizer", "()J", zero_i64);
    vm.register_native(p, "nReset", "(J)V", path_reset);
    vm.register_native(p, "nRewind", "(J)V", path_reset);
    vm.register_native(p, "nSet", "(JJ)V", path_set);
    vm.register_native(p, "nGetFillType", "(J)I", path_get_fill);
    vm.register_native(p, "nSetFillType", "(JI)V", path_set_fill);
    vm.register_native(p, "nIsEmpty", "(J)Z", path_is_empty);
    vm.register_native(p, "nIsRect", "(JLandroid/graphics/RectF;)Z", path_is_rect);
    vm.register_native(p, "nComputeBounds", "(JLandroid/graphics/RectF;)V", path_compute_bounds);
    vm.register_native(p, "nMoveTo", "(JFF)V", path_move_to);
    vm.register_native(p, "nRMoveTo", "(JFF)V", path_rmove_to);
    vm.register_native(p, "nLineTo", "(JFF)V", path_line_to);
    vm.register_native(p, "nRLineTo", "(JFF)V", path_rline_to);
    vm.register_native(p, "nQuadTo", "(JFFFF)V", path_quad_to);
    vm.register_native(p, "nRQuadTo", "(JFFFF)V", path_rquad_to);
    vm.register_native(p, "nConicTo", "(JFFFFF)V", path_conic_to);
    vm.register_native(p, "nRConicTo", "(JFFFFF)V", path_rconic_to);
    vm.register_native(p, "nCubicTo", "(JFFFFFF)V", path_cubic_to);
    vm.register_native(p, "nRCubicTo", "(JFFFFFF)V", path_rcubic_to);
    vm.register_native(p, "nArcTo", "(JFFFFFFZ)V", path_arc_to);
    vm.register_native(p, "nClose", "(J)V", path_close);
    vm.register_native(p, "nAddRect", "(JFFFFI)V", path_add_rect);
    vm.register_native(p, "nAddOval", "(JFFFFI)V", path_add_oval);
    vm.register_native(p, "nAddCircle", "(JFFFI)V", path_add_circle);
    vm.register_native(p, "nAddArc", "(JFFFFFF)V", path_add_arc);
    vm.register_native(p, "nAddRoundRect", "(JFFFFFFI)V", path_add_round_rect);
    vm.register_native(p, "nAddRoundRect", "(JFFFF[FI)V", path_add_round_rect_radii);
    vm.register_native(p, "nAddPath", "(JJ)V", path_add_path);
    vm.register_native(p, "nAddPath", "(JJFF)V", path_add_path_offset);
    vm.register_native(p, "nAddPath", "(JJJ)V", path_add_path_matrix);
    vm.register_native(p, "nOffset", "(JFF)V", path_offset);
    vm.register_native(p, "nSetLastPoint", "(JFF)V", path_set_last_point);
    vm.register_native(p, "nTransform", "(JJ)V", path_transform);
    vm.register_native(p, "nTransform", "(JJJ)V", path_transform_to);
    vm.register_native(p, "nOp", "(JJIJ)Z", path_op);
    vm.register_native(p, "nApproximate", "(JF)[F", path_approximate);
    vm.register_native(p, "nGetGenerationID", "(J)I", path_generation);
    vm.register_native(p, "nIncReserve", "(JI)V", noop);
    vm.register_native(p, "nIsConvex", "(J)Z", path_is_convex);
    vm.register_native(p, "nInterpolate", "(JJFJ)Z", unsupported_interpolate);
    vm.register_native(p, "nIsInterpolatable", "(JJ)Z", unsupported_interpolate_bool);

    let r = "Landroid/graphics/Region;";
    vm.register_native(r, "nativeConstructor", "()J", region_init);
    vm.register_native(r, "nativeDestructor", "(J)V", region_free);
    vm.register_native(r, "nativeSetRect", "(JIIII)Z", region_set_rect);
    vm.register_native(r, "nativeSetRegion", "(JJ)V", region_set_region);
    vm.register_native(r, "nativeSetPath", "(JJJ)Z", region_set_path);
    vm.register_native(r, "nativeGetBounds", "(JLandroid/graphics/Rect;)Z", region_get_bounds);
    vm.register_native(r, "nativeOp", "(JIIIII)Z", region_op_rect);
    vm.register_native(r, "nativeOp", "(JLandroid/graphics/Rect;JI)Z", region_op_rect_dst);
    vm.register_native(r, "nativeOp", "(JJJI)Z", region_op_region);
    vm.register_native(r, "nativeGetBoundaryPath", "(JJ)Z", region_boundary);
    vm.register_native(r, "nativeEquals", "(JJ)Z", region_equals);
    vm.register_native(r, "nativeToString", "(J)Ljava/lang/String;", region_to_string);
    vm.register_native(r, "nativeWriteToParcel", "(JLandroid/os/Parcel;)Z", unsupported_parcel);
    vm.register_native(r, "nativeCreateFromParcel", "(Landroid/os/Parcel;)J", unsupported_parcel_from);
    Ok(())
}

fn zero_i64(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Path
// ---------------------------------------------------------------------------

fn path_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    new_path(vm, ctx, Path::new())
}

fn path_init_copy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = vm.arg_i64(ctx, 0);
    let copy = read_path(src, |p| p.clone())?;
    new_path(vm, ctx, copy)
}

fn path_reset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    with_path(vm.arg_i64(ctx, 0), |p| p.reset())?;
    Ok(())
}

fn path_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = read_path(vm.arg_i64(ctx, 1), |p| p.clone())?;
    with_path(vm.arg_i64(ctx, 0), |p| p.set(&src))?;
    Ok(())
}

fn path_get_fill(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ft = read_path(vm.arg_i64(ctx, 0), |p| p.fill_type())?;
    vm.ret_i32(ctx, ft as i32);
    Ok(())
}

fn path_set_fill(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let o = vm.arg_i32(ctx, 1);
    let ft = FillType::from_ordinal(o).ok_or_else(|| {
        VmError::unsupported_api(format!("Path.FillType ordinal {o}"), "android.graphics.Path")
    })?;
    with_path(vm.arg_i64(ctx, 0), |p| p.set_fill_type(ft))?;
    Ok(())
}

fn path_is_empty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, read_path(vm.arg_i64(ctx, 0), |p| p.is_empty())?);
    Ok(())
}

fn path_is_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let out = vm.arg_ref(ctx, 1);
    let (is_rect, bounds) = read_path(h, |p| (p.is_rect(), p.compute_bounds()))?;
    if let Some(b) = bounds {
        if out != 0 {
            crate::natives::gfx_text::write_rect(vm, out, b)?;
        }
    }
    vm.ret_bool(ctx, is_rect);
    Ok(())
}

fn path_compute_bounds(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let out = vm.arg_ref(ctx, 1);
    let b = read_path(vm.arg_i64(ctx, 0), |p| p.compute_bounds())?;
    let b = b.unwrap_or_else(|| RectF::new(0.0, 0.0, 0.0, 0.0));
    crate::natives::gfx_text::write_rect(vm, out, b)?;
    Ok(())
}

fn path_move_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (x, y) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_path(vm.arg_i64(ctx, 0), |p| p.move_to(x, y))?;
    Ok(())
}

fn path_line_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (x, y) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_path(vm.arg_i64(ctx, 0), |p| p.line_to(x, y))?;
    Ok(())
}

fn path_rmove_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (dx, dy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_path(vm.arg_i64(ctx, 0), |p| {
        let (x, y) = p.last_point().map(|q| (q.x, q.y)).unwrap_or((0.0, 0.0));
        p.move_to(x + dx, y + dy);
    })?;
    Ok(())
}

fn path_rline_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (dx, dy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_path(vm.arg_i64(ctx, 0), |p| {
        let (x, y) = p.last_point().map(|q| (q.x, q.y)).unwrap_or((0.0, 0.0));
        p.line_to(x + dx, y + dy);
    })?;
    Ok(())
}

fn path_quad_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (x1, y1, x2, y2) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    with_path(vm.arg_i64(ctx, 0), |p| p.quad_to(x1, y1, x2, y2))?;
    Ok(())
}

fn path_rquad_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (dx1, dy1, dx2, dy2) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    with_path(vm.arg_i64(ctx, 0), |p| {
        let (x, y) = p.last_point().map(|q| (q.x, q.y)).unwrap_or((0.0, 0.0));
        p.quad_to(x + dx1, y + dy1, x + dx2, y + dy2);
    })?;
    Ok(())
}

fn path_conic_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (x1, y1, x2, y2, w) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
        vm.arg_f32(ctx, 5),
    );
    with_path(vm.arg_i64(ctx, 0), |p| p.conic_to(x1, y1, x2, y2, w))?;
    Ok(())
}

fn path_rconic_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (dx1, dy1, dx2, dy2, w) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
        vm.arg_f32(ctx, 5),
    );
    with_path(vm.arg_i64(ctx, 0), |p| {
        let (x, y) = p.last_point().map(|q| (q.x, q.y)).unwrap_or((0.0, 0.0));
        p.conic_to(x + dx1, y + dy1, x + dx2, y + dy2, w);
    })?;
    Ok(())
}

fn path_cubic_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (x1, y1, x2, y2, x3, y3) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
        vm.arg_f32(ctx, 5),
        vm.arg_f32(ctx, 6),
    );
    with_path(vm.arg_i64(ctx, 0), |p| p.cubic_to(x1, y1, x2, y2, x3, y3))?;
    Ok(())
}

fn path_rcubic_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (dx1, dy1, dx2, dy2, dx3, dy3) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
        vm.arg_f32(ctx, 5),
        vm.arg_f32(ctx, 6),
    );
    with_path(vm.arg_i64(ctx, 0), |p| {
        let (x, y) = p.last_point().map(|q| (q.x, q.y)).unwrap_or((0.0, 0.0));
        p.cubic_to(x + dx1, y + dy1, x + dx2, y + dy2, x + dx3, y + dy3);
    })?;
    Ok(())
}

fn path_arc_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (l, t, rr, b) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let (start, sweep) = (vm.arg_f32(ctx, 5), vm.arg_f32(ctx, 6));
    let force = vm.arg_bool(ctx, 7);
    with_path(vm.arg_i64(ctx, 0), |p| {
        p.arc_to(RectF::new(l, t, rr, b), start, sweep, force)
    })?;
    Ok(())
}

fn path_close(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    with_path(vm.arg_i64(ctx, 0), |p| p.close())?;
    Ok(())
}

fn path_add_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (l, t, rr, b) = rect_args(vm, ctx, 1);
    let dir = vm.arg_i32(ctx, 5);
    with_path(vm.arg_i64(ctx, 0), |p| p.add_rect(l, t, rr, b, dir != 0))?;
    Ok(())
}

fn path_add_oval(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (l, t, rr, b) = rect_args(vm, ctx, 1);
    let dir = vm.arg_i32(ctx, 5);
    with_path(vm.arg_i64(ctx, 0), |p| p.add_oval(l, t, rr, b, dir != 0))?;
    Ok(())
}

fn path_add_circle(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (cx, cy, r) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2), vm.arg_f32(ctx, 3));
    let dir = vm.arg_i32(ctx, 4);
    with_path(vm.arg_i64(ctx, 0), |p| p.add_circle(cx, cy, r, dir != 0))?;
    Ok(())
}

fn path_add_arc(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (l, t, rr, b) = rect_args(vm, ctx, 1);
    let (start, sweep) = (vm.arg_f32(ctx, 5), vm.arg_f32(ctx, 6));
    with_path(vm.arg_i64(ctx, 0), |p| {
        p.arc_to(RectF::new(l, t, rr, b), start, sweep, true)
    })?;
    Ok(())
}

fn path_add_round_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (l, t, rr, b) = rect_args(vm, ctx, 1);
    let (rx, ry) = (vm.arg_f32(ctx, 5), vm.arg_f32(ctx, 6));
    let dir = vm.arg_i32(ctx, 7);
    with_path(vm.arg_i64(ctx, 0), |p| {
        p.add_round_rect(&RectF::new(l, t, rr, b), rx, ry, dir != 0)
    })?;
    Ok(())
}

fn path_add_round_rect_radii(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (l, t, rr, b) = rect_args(vm, ctx, 1);
    let arr = vm.arg_ref(ctx, 5);
    let dir = vm.arg_i32(ctx, 6);
    let radii = read_float_array(vm, arr)?;
    // Android accepts 8 radii (4 corners x2); use the first corner when given.
    let (rx, ry) = match radii.len() {
        0 => (0.0, 0.0),
        1 => (radii[0], radii[0]),
        _ => (radii[0], radii[1]),
    };
    with_path(vm.arg_i64(ctx, 0), |p| {
        p.add_round_rect(&RectF::new(l, t, rr, b), rx, ry, dir != 0)
    })?;
    Ok(())
}

fn path_add_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let other = read_path(vm.arg_i64(ctx, 1), |p| p.clone())?;
    with_path(vm.arg_i64(ctx, 0), |p| p.add_path(&other))?;
    Ok(())
}

fn path_add_path_offset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (dx, dy) = (vm.arg_f32(ctx, 2), vm.arg_f32(ctx, 3));
    let mut other = read_path(vm.arg_i64(ctx, 1), |p| p.clone())?;
    let m = acos_render::matrix::Matrix3x3::translate_matrix(dx, dy);
    other.transform(&m);
    with_path(vm.arg_i64(ctx, 0), |p| p.add_path(&other))?;
    Ok(())
}

fn path_add_path_matrix(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mh = vm.arg_i64(ctx, 2);
    let m = crate::natives::gfx_matrix::matrix_values(mh)?;
    let mut other = read_path(vm.arg_i64(ctx, 1), |p| p.clone())?;
    other.transform(&crate::natives::gfx_matrix::matrix_from_values(m));
    with_path(vm.arg_i64(ctx, 0), |p| p.add_path(&other))?;
    Ok(())
}

fn path_offset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (dx, dy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    let m = acos_render::matrix::Matrix3x3::translate_matrix(dx, dy);
    with_path(vm.arg_i64(ctx, 0), |p| p.transform(&m))?;
    Ok(())
}

fn path_set_last_point(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `Path.setLastPoint` rewrites the last verb's target. ACOS's immutable
    // verb list has no in-place edit; record it as unsupported rather than
    // silently dropping the change.
    let _ = (vm.arg_i64(ctx, 0), vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    Err(VmError::unsupported_api(
        "Path.nSetLastPoint",
        "android.graphics.Path",
    ))
}

fn path_transform(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let m = crate::natives::gfx_matrix::matrix_values(vm.arg_i64(ctx, 1))?;
    let m = crate::natives::gfx_matrix::matrix_from_values(m);
    with_path(vm.arg_i64(ctx, 0), |p| p.transform(&m))?;
    Ok(())
}

fn path_transform_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let m = crate::natives::gfx_matrix::matrix_values(vm.arg_i64(ctx, 2))?;
    let m = crate::natives::gfx_matrix::matrix_from_values(m);
    let mut src = read_path(vm.arg_i64(ctx, 1), |p| p.clone())?;
    src.transform(&m);
    with_path(vm.arg_i64(ctx, 0), |p| p.set(&src))?;
    Ok(())
}

fn path_op(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let op = PathOp::from_ordinal(vm.arg_i32(ctx, 2)).ok_or_else(|| {
        VmError::unsupported_api("Path.Op ordinal", "android.graphics.Path")
    })?;
    let _ = op;
    Err(VmError::unsupported_api(
        "Path.nOp (boolean path ops)",
        "android.graphics.Path",
    ))
}

fn path_approximate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let acceptable = vm.arg_f32(ctx, 1).max(0.0);
    let pts = read_path(vm.arg_i64(ctx, 0), |p| p.flatten(acceptable))?;
    let mut flat: Vec<f32> = Vec::new();
    for contour in &pts {
        for p in contour {
            flat.push(p.x);
            flat.push(p.y);
        }
    }
    let arr = vm.alloc(crate::heap::Object::Array(crate::heap::ArrayObj {
        elem_desc: "F".to_string(),
        data: crate::heap::ArrayData::Float(flat),
    }))?;
    vm.ret_ref(ctx, arr);
    Ok(())
}

fn path_generation(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // ACOS paths have no mutation counter; return a stable 0 (Android's value
    // for a never-mutated path). Documented: callers using it only as a cache
    // key will see "unchanged", which is safe (no stale cache) because ACOS
    // recomputes bounds on demand.
    read_path(vm.arg_i64(ctx, 0), |_| ())?;
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn path_is_convex(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // A single rectangle is convex; anything else is reported as non-convex.
    // Conservative `false` only costs a slower draw path, never a wrong pixel.
    vm.ret_bool(ctx, read_path(vm.arg_i64(ctx, 0), |p| p.is_rect())?);
    Ok(())
}

fn unsupported_interpolate(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(VmError::unsupported_api(
        "Path.nInterpolate (path morphing)",
        "android.graphics.Path",
    ))
}

fn unsupported_interpolate_bool(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(VmError::unsupported_api(
        "Path.nIsInterpolatable",
        "android.graphics.Path",
    ))
}

// ---------------------------------------------------------------------------
// Region
// ---------------------------------------------------------------------------

fn region_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = regions().insert(Region::new()).to_jlong();
    vm.ret_i64(ctx, h);
    Ok(())
}

fn region_free(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = regions().remove(Handle::from_jlong(vm.arg_i64(ctx, 0)));
    Ok(())
}

fn region_set_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (l, t, r, b) = rect_int_args(vm, ctx, 1);
    let h = vm.arg_i64(ctx, 0);
    let mut g = regions();
    *g.get_mut(Handle::from_jlong(h)).map_err(region_err)? =
        Region::from_rect(Rect::new(l, t, r, b));
    vm.ret_bool(ctx, true);
    Ok(())
}

fn region_set_region(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let other = read_region(vm.arg_i64(ctx, 1), |r| r.clone())?;
    let h = vm.arg_i64(ctx, 0);
    let mut g = regions();
    *g.get_mut(Handle::from_jlong(h)).map_err(region_err)? = other;
    Ok(())
}

fn region_set_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = read_path(vm.arg_i64(ctx, 1), |p| p.clone())?;
    let clip = if vm.arg_i64(ctx, 2) == 0 {
        None
    } else {
        Some(read_region(vm.arg_i64(ctx, 2), |r| r.clone())?)
    };
    let mut region = Region::new();
    let ok = region.set_path(&path, clip.as_ref()).is_ok();
    let h = vm.arg_i64(ctx, 0);
    let mut g = regions();
    *g.get_mut(Handle::from_jlong(h)).map_err(region_err)? = region;
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn region_get_bounds(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let out = vm.arg_ref(ctx, 1);
    let b = read_region(vm.arg_i64(ctx, 0), |r| r.bounds())?;
    if let Some(b) = b {
        vm.set_instance_field_by_name(out, "left", b.left as u32);
        vm.set_instance_field_by_name(out, "top", b.top as u32);
        vm.set_instance_field_by_name(out, "right", b.right as u32);
        vm.set_instance_field_by_name(out, "bottom", b.bottom as u32);
    }
    vm.ret_bool(ctx, b.is_some());
    Ok(())
}

fn region_compute_op(region: &Region, other: &Region, op: RegionOp) -> VmResult<bool> {
    let mut out = region.clone();
    out.op(other, op).map_err(|e| {
        VmError::unsupported_api(format!("Region.op: {e}"), "android.graphics.Region")
    })?;
    Ok(!out.is_empty())
}

fn region_op_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (l, t, r, b) = rect_int_args(vm, ctx, 1);
    let op = region_op(vm.arg_i32(ctx, 5))?;
    let src = read_region(vm.arg_i64(ctx, 0), |r| r.clone())?;
    let other = Region::from_rect(Rect::new(l, t, r, b));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        region_compute_op(&src, &other, op)
    }))
    .map_err(|_| VmError::Internal("Region.op panicked".into()))??;
    vm.ret_bool(ctx, result);
    Ok(())
}

fn region_op_rect_dst(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rect_h = vm.arg_ref(ctx, 1);
    let dst = vm.arg_i64(ctx, 2);
    let op = region_op(vm.arg_i32(ctx, 3))?;
    let (l, t, r, b) = read_rect_int(vm, rect_h);
    let src = read_region(vm.arg_i64(ctx, 0), |r| r.clone())?;
    let other = Region::from_rect(Rect::new(l, t, r, b));
    let mut out = src;
    out.op(&other, op)
        .map_err(|e| VmError::unsupported_api(format!("Region.op: {e}"), "android.graphics.Region"))?;
    let nonempty = !out.is_empty();
    let mut g = regions();
    *g.get_mut(Handle::from_jlong(dst)).map_err(region_err)? = out;
    vm.ret_bool(ctx, nonempty);
    Ok(())
}

fn region_op_region(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let other = read_region(vm.arg_i64(ctx, 1), |r| r.clone())?;
    let dst = vm.arg_i64(ctx, 2);
    let op = region_op(vm.arg_i32(ctx, 3))?;
    let src = read_region(vm.arg_i64(ctx, 0), |r| r.clone())?;
    let mut out = src;
    out.op(&other, op)
        .map_err(|e| VmError::unsupported_api(format!("Region.op: {e}"), "android.graphics.Region"))?;
    let nonempty = !out.is_empty();
    let mut g = regions();
    *g.get_mut(Handle::from_jlong(dst)).map_err(region_err)? = out;
    vm.ret_bool(ctx, nonempty);
    Ok(())
}

fn region_boundary(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path_h = vm.arg_i64(ctx, 1);
    let boundary = read_region(vm.arg_i64(ctx, 0), |r| r.boundary_path())?;
    let nonempty = !boundary.is_empty();
    *paths().get_mut(Handle::from_jlong(path_h)).map_err(path_err)? = boundary;
    vm.ret_bool(ctx, nonempty);
    Ok(())
}

fn region_equals(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = read_region(vm.arg_i64(ctx, 0), |r| r.clone())?;
    let b = read_region(vm.arg_i64(ctx, 1), |r| r.clone())?;
    vm.ret_bool(ctx, a == b);
    Ok(())
}

fn region_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = read_region(vm.arg_i64(ctx, 0), |r| match r.bounds() {
        Some(b) => format!("Region({},{},{},{})", b.left, b.top, b.right, b.bottom),
        None => "Region()".to_string(),
    })?;
    let h = vm.new_string(&s)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn unsupported_parcel(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(VmError::unsupported_api(
        "Region.nativeWriteToParcel",
        "android.graphics.Region",
    ))
}

fn unsupported_parcel_from(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(VmError::unsupported_api(
        "Region.nativeCreateFromParcel",
        "android.graphics.Region",
    ))
}

// ---------------------------------------------------------------------------
// Arg helpers
// ---------------------------------------------------------------------------

fn rect_args(vm: &Vm, ctx: NativeCtx, base: usize) -> (f32, f32, f32, f32) {
    (
        vm.arg_f32(ctx, base),
        vm.arg_f32(ctx, base + 1),
        vm.arg_f32(ctx, base + 2),
        vm.arg_f32(ctx, base + 3),
    )
}

fn rect_int_args(vm: &Vm, ctx: NativeCtx, base: usize) -> (i32, i32, i32, i32) {
    (
        vm.arg_i32(ctx, base),
        vm.arg_i32(ctx, base + 1),
        vm.arg_i32(ctx, base + 2),
        vm.arg_i32(ctx, base + 3),
    )
}

fn read_rect_int(vm: &Vm, h: u32) -> (i32, i32, i32, i32) {
    let get = |name: &str| vm.get_instance_field_by_name(h, name).unwrap_or(0) as i32;
    (get("left"), get("top"), get("right"), get("bottom"))
}

fn read_float_array(vm: &Vm, h: u32) -> VmResult<Vec<f32>> {
    use crate::heap::{ArrayData, Object};
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Float(v) => Ok(v.clone()),
            _ => Err(VmError::Internal("expected float[]".to_string())),
        },
        _ => Err(VmError::Internal("expected float[]".to_string())),
    }
}

fn region_op(o: i32) -> VmResult<RegionOp> {
    RegionOp::from_ordinal(o).ok_or_else(|| {
        VmError::unsupported_api(format!("Region.Op ordinal {o}"), "android.graphics.Region")
    })
}
