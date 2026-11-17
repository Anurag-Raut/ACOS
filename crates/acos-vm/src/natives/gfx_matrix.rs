//! `android.graphics.Matrix` natives over `acos-render`'s [`Matrix3x3`].
//!
//! One native `Matrix` per `jlong` in a generation-checked handle table; a
//! stale handle is a structured error. `nGetValues`/`nSetValues` expose the
//! same row-major 9-float layout Android uses, so Java-level round-trips and
//! `mapPoints` agree with the rasteriser.

use std::sync::{Mutex, MutexGuard, OnceLock};

use acos_render::geom::{PointF, RectF};
use acos_render::matrix::{Matrix3x3, ScaleToFit};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::natives::gfx_handles::{Handle, HandleTable};
use crate::vm::Vm;

static MATRICES: OnceLock<Mutex<HandleTable<Matrix3x3>>> = OnceLock::new();

fn matrices() -> MutexGuard<'static, HandleTable<Matrix3x3>> {
    let m = MATRICES.get_or_init(|| Mutex::new(HandleTable::new()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn err(e: crate::natives::gfx_handles::HandleError) -> VmError {
    VmError::Internal(format!("Matrix: {e}"))
}

/// Read the 9 row-major values for a matrix handle (used by `Path.nTransform`).
pub fn matrix_values(handle: i64) -> VmResult<[f32; 9]> {
    matrices()
        .get(Handle::from_jlong(handle))
        .map(|m| m.get_values())
        .map_err(err)
}

/// Overwrite a matrix handle's value (used by `Canvas.nGetMatrix` /
/// `RenderNode.nGetTransformMatrix`).
pub fn matrix_store(handle: i64, m: Matrix3x3) -> VmResult<()> {
    *matrices()
        .get_mut(Handle::from_jlong(handle))
        .map_err(err)? = m;
    Ok(())
}

/// Build a [`Matrix3x3`] from 9 row-major values.
pub fn matrix_from_values(v: [f32; 9]) -> Matrix3x3 {
    let mut m = Matrix3x3::identity();
    m.set_values(v);
    m
}

fn with_matrix<R>(handle: i64, f: impl FnOnce(&mut Matrix3x3) -> R) -> VmResult<R> {
    let mut g = matrices();
    let m = g.get_mut(Handle::from_jlong(handle)).map_err(err)?;
    Ok(f(m))
}

fn read_matrix<R>(handle: i64, f: impl FnOnce(&Matrix3x3) -> R) -> VmResult<R> {
    let g = matrices();
    let m = g.get(Handle::from_jlong(handle)).map_err(err)?;
    Ok(f(m))
}

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let m = "Landroid/graphics/Matrix;";
    vm.register_native(m, "nCreate", "(J)J", matrix_create);
    vm.register_native(m, "nGetNativeFinalizer", "()J", zero_i64);
    vm.register_native(m, "nGetValues", "(J[F)V", matrix_get_values);
    vm.register_native(m, "nSetValues", "(J[F)V", matrix_set_values);
    vm.register_native(m, "nReset", "(J)V", matrix_reset);
    vm.register_native(m, "nIsIdentity", "(J)Z", matrix_is_identity);
    vm.register_native(m, "nIsAffine", "(J)Z", matrix_is_affine);
    vm.register_native(m, "nRectStaysRect", "(J)Z", matrix_rect_stays_rect);
    vm.register_native(m, "nSet", "(JJ)V", matrix_set);
    vm.register_native(m, "nSetConcat", "(JJJ)V", matrix_set_concat);
    vm.register_native(m, "nPreConcat", "(JJ)V", matrix_pre_concat);
    vm.register_native(m, "nPostConcat", "(JJ)V", matrix_post_concat);
    vm.register_native(m, "nSetTranslate", "(JFF)V", matrix_set_translate);
    vm.register_native(m, "nPreTranslate", "(JFF)V", matrix_pre_translate);
    vm.register_native(m, "nPostTranslate", "(JFF)V", matrix_post_translate);
    vm.register_native(m, "nSetScale", "(JFF)V", matrix_set_scale);
    vm.register_native(m, "nSetScale", "(JFFFF)V", matrix_set_scale_pivot);
    vm.register_native(m, "nPreScale", "(JFF)V", matrix_pre_scale);
    vm.register_native(m, "nPreScale", "(JFFFF)V", matrix_pre_scale_pivot);
    vm.register_native(m, "nPostScale", "(JFF)V", matrix_post_scale);
    vm.register_native(m, "nPostScale", "(JFFFF)V", matrix_post_scale_pivot);
    vm.register_native(m, "nSetRotate", "(JF)V", matrix_set_rotate);
    vm.register_native(m, "nSetRotate", "(JFFF)V", matrix_set_rotate_pivot);
    vm.register_native(m, "nPreRotate", "(JF)V", matrix_pre_rotate);
    vm.register_native(m, "nPreRotate", "(JFFF)V", matrix_pre_rotate_pivot);
    vm.register_native(m, "nPostRotate", "(JF)V", matrix_post_rotate);
    vm.register_native(m, "nPostRotate", "(JFFF)V", matrix_post_rotate_pivot);
    vm.register_native(m, "nSetSkew", "(JFF)V", matrix_set_skew);
    vm.register_native(m, "nSetSkew", "(JFFFF)V", matrix_set_skew_pivot);
    vm.register_native(m, "nPreSkew", "(JFF)V", matrix_pre_skew);
    vm.register_native(m, "nPreSkew", "(JFFFF)V", matrix_pre_skew_pivot);
    vm.register_native(m, "nPostSkew", "(JFF)V", matrix_post_skew);
    vm.register_native(m, "nPostSkew", "(JFFFF)V", matrix_post_skew_pivot);
    vm.register_native(m, "nSetSinCos", "(JFF)V", matrix_set_sin_cos);
    vm.register_native(m, "nSetSinCos", "(JFFFF)V", matrix_set_sin_cos_pivot);
    vm.register_native(m, "nInvert", "(JJ)Z", matrix_invert);
    vm.register_native(m, "nMapPoints", "(J[FI[FIIZ)V", matrix_map_points);
    vm.register_native(m, "nMapRadius", "(JF)F", matrix_map_radius);
    vm.register_native(
        m,
        "nMapRect",
        "(JLandroid/graphics/RectF;Landroid/graphics/RectF;)Z",
        matrix_map_rect,
    );
    vm.register_native(m, "nEquals", "(JJ)Z", matrix_equals);
    vm.register_native(
        m,
        "nSetRectToRect",
        "(JLandroid/graphics/RectF;Landroid/graphics/RectF;I)Z",
        matrix_rect_to_rect,
    );
    vm.register_native(m, "nSetPolyToPoly", "(J[FI[FII)Z", matrix_poly_to_poly);
    Ok(())
}

fn zero_i64(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn matrix_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = vm.arg_i64(ctx, 0);
    let m = if src == 0 {
        Matrix3x3::identity()
    } else {
        read_matrix(src, |m| *m)?
    };
    let h = matrices().insert(m).to_jlong();
    vm.ret_i64(ctx, h);
    Ok(())
}

fn matrix_get_values(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let values = read_matrix(vm.arg_i64(ctx, 0), |m| m.get_values())?;
    let arr = vm.arg_ref(ctx, 1);
    write_floats(vm, arr, &values)?;
    Ok(())
}

fn matrix_set_values(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arr = vm.arg_ref(ctx, 1);
    let v = read_floats(vm, arr, 9)?;
    let mut values = [0.0f32; 9];
    for (i, x) in v.iter().take(9).enumerate() {
        values[i] = *x;
    }
    with_matrix(vm.arg_i64(ctx, 0), |m| m.set_values(values))?;
    Ok(())
}

fn matrix_reset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    with_matrix(vm.arg_i64(ctx, 0), |m| m.reset())?;
    Ok(())
}

fn matrix_is_identity(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, read_matrix(vm.arg_i64(ctx, 0), |m| m.is_identity())?);
    Ok(())
}

fn matrix_is_affine(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, read_matrix(vm.arg_i64(ctx, 0), |m| m.is_affine())?);
    Ok(())
}

fn matrix_rect_stays_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(
        ctx,
        read_matrix(vm.arg_i64(ctx, 0), |m| m.rect_stays_rect())?,
    );
    Ok(())
}

fn matrix_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = read_matrix(vm.arg_i64(ctx, 1), |m| *m)?;
    with_matrix(vm.arg_i64(ctx, 0), |m| m.set(&src))?;
    Ok(())
}

fn matrix_set_concat(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = read_matrix(vm.arg_i64(ctx, 1), |m| *m)?;
    let b = read_matrix(vm.arg_i64(ctx, 2), |m| *m)?;
    with_matrix(vm.arg_i64(ctx, 0), |m| m.set_concat(&a, &b))?;
    Ok(())
}

fn matrix_pre_concat(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let other = read_matrix(vm.arg_i64(ctx, 1), |m| *m)?;
    with_matrix(vm.arg_i64(ctx, 0), |m| m.pre_concat(&other))?;
    Ok(())
}

fn matrix_post_concat(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let other = read_matrix(vm.arg_i64(ctx, 1), |m| *m)?;
    with_matrix(vm.arg_i64(ctx, 0), |m| m.post_concat(&other))?;
    Ok(())
}

fn matrix_set_translate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (dx, dy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_matrix(vm.arg_i64(ctx, 0), |m| m.set_translate(dx, dy))?;
    Ok(())
}

fn matrix_pre_translate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (dx, dy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_matrix(vm.arg_i64(ctx, 0), |m| m.pre_translate(dx, dy))?;
    Ok(())
}

fn matrix_post_translate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (dx, dy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_matrix(vm.arg_i64(ctx, 0), |m| m.post_translate(dx, dy))?;
    Ok(())
}

fn matrix_set_scale(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (sx, sy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_matrix(vm.arg_i64(ctx, 0), |m| m.set_scale(sx, sy))?;
    Ok(())
}

fn matrix_set_scale_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (sx, sy, px, py) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    with_matrix(vm.arg_i64(ctx, 0), |m| m.set_scale_pivot(sx, sy, px, py))?;
    Ok(())
}

fn matrix_pre_scale(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (sx, sy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_matrix(vm.arg_i64(ctx, 0), |m| m.pre_scale(sx, sy))?;
    Ok(())
}

fn matrix_pre_scale_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (sx, sy, px, py) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let mut tmp = Matrix3x3::identity();
    tmp.set_scale_pivot(sx, sy, px, py);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.pre_concat(&tmp))?;
    Ok(())
}

fn matrix_post_scale(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (sx, sy) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_matrix(vm.arg_i64(ctx, 0), |m| m.post_scale(sx, sy))?;
    Ok(())
}

fn matrix_post_scale_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (sx, sy, px, py) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let mut tmp = Matrix3x3::identity();
    tmp.set_scale_pivot(sx, sy, px, py);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.post_concat(&tmp))?;
    Ok(())
}

fn matrix_set_rotate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let d = vm.arg_f32(ctx, 1);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.set_rotate(d))?;
    Ok(())
}

fn matrix_set_rotate_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (d, px, py) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2), vm.arg_f32(ctx, 3));
    with_matrix(vm.arg_i64(ctx, 0), |m| m.set_rotate_pivot(d, px, py))?;
    Ok(())
}

fn matrix_pre_rotate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let d = vm.arg_f32(ctx, 1);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.pre_rotate(d))?;
    Ok(())
}

fn matrix_pre_rotate_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (d, px, py) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2), vm.arg_f32(ctx, 3));
    let mut tmp = Matrix3x3::identity();
    tmp.set_rotate_pivot(d, px, py);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.pre_concat(&tmp))?;
    Ok(())
}

fn matrix_post_rotate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let d = vm.arg_f32(ctx, 1);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.post_rotate(d))?;
    Ok(())
}

fn matrix_post_rotate_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (d, px, py) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2), vm.arg_f32(ctx, 3));
    let mut tmp = Matrix3x3::identity();
    tmp.set_rotate_pivot(d, px, py);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.post_concat(&tmp))?;
    Ok(())
}

fn matrix_set_skew(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (kx, ky) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_matrix(vm.arg_i64(ctx, 0), |m| m.set_skew(kx, ky))?;
    Ok(())
}

fn matrix_set_skew_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (kx, ky, px, py) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    with_matrix(vm.arg_i64(ctx, 0), |m| m.set_skew_pivot(kx, ky, px, py))?;
    Ok(())
}

fn matrix_pre_skew(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (kx, ky) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    let mut tmp = Matrix3x3::identity();
    tmp.set_skew(kx, ky);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.pre_concat(&tmp))?;
    Ok(())
}

fn matrix_pre_skew_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (kx, ky, px, py) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let mut tmp = Matrix3x3::identity();
    tmp.set_skew_pivot(kx, ky, px, py);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.pre_concat(&tmp))?;
    Ok(())
}

fn matrix_post_skew(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (kx, ky) = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    let mut tmp = Matrix3x3::identity();
    tmp.set_skew(kx, ky);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.post_concat(&tmp))?;
    Ok(())
}

fn matrix_post_skew_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (kx, ky, px, py) = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    let mut tmp = Matrix3x3::identity();
    tmp.set_skew_pivot(kx, ky, px, py);
    with_matrix(vm.arg_i64(ctx, 0), |m| m.post_concat(&tmp))?;
    Ok(())
}

fn sin_cos_matrix(sc: f32, ss: f32) -> Matrix3x3 {
    matrix_from_values([sc, -ss, 0.0, ss, sc, 0.0, 0.0, 0.0, 1.0])
}

fn matrix_set_sin_cos(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let m = sin_cos_matrix(vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    with_matrix(vm.arg_i64(ctx, 0), |dst| dst.set(&m))?;
    Ok(())
}

fn matrix_set_sin_cos_pivot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let sc = vm.arg_f32(ctx, 1);
    let ss = vm.arg_f32(ctx, 2);
    let (px, py) = (vm.arg_f32(ctx, 3), vm.arg_f32(ctx, 4));
    let mut m = sin_cos_matrix(sc, ss);
    // Translate by pivot: T(-p) * R * T(p)
    let pre = Matrix3x3::translate_matrix(-px, -py);
    let post = Matrix3x3::translate_matrix(px, py);
    m.set_concat(&post, &m.mul(&pre));
    with_matrix(vm.arg_i64(ctx, 0), |dst| dst.set(&m))?;
    Ok(())
}

fn matrix_invert(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = read_matrix(vm.arg_i64(ctx, 0), |m| *m)?;
    match src.inverted() {
        Some(inv) => {
            with_matrix(vm.arg_i64(ctx, 1), |dst| dst.set(&inv))?;
            vm.ret_bool(ctx, true);
        }
        None => vm.ret_bool(ctx, false),
    }
    Ok(())
}

fn matrix_map_points(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src_h = vm.arg_ref(ctx, 1);
    let src_index = vm.arg_i32(ctx, 2).max(0) as usize;
    let dst_h = vm.arg_ref(ctx, 3);
    let dst_index = vm.arg_i32(ctx, 4).max(0) as usize;
    let count = vm.arg_i32(ctx, 5).max(0) as usize;
    let src = read_floats(vm, src_h, 0)?;
    let mut pts: Vec<PointF> = (0..count)
        .map(|i| {
            let base = src_index + i * 2;
            PointF::new(
                src.get(base).copied().unwrap_or(0.0),
                src.get(base + 1).copied().unwrap_or(0.0),
            )
        })
        .collect();
    read_matrix(vm.arg_i64(ctx, 0), |m| m.map_points(&mut pts))?;
    let mut dst = read_floats(vm, dst_h, 0)?;
    for (i, p) in pts.iter().enumerate() {
        let base = dst_index + i * 2;
        if base + 1 < dst.len() {
            dst[base] = p.x;
            dst[base + 1] = p.y;
        }
    }
    write_floats(vm, dst_h, &dst)?;
    Ok(())
}

fn matrix_map_radius(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let r = vm.arg_f32(ctx, 1);
    vm.ret_f32(ctx, read_matrix(vm.arg_i64(ctx, 0), |m| m.map_radius(r))?);
    Ok(())
}

fn matrix_map_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src_h = vm.arg_ref(ctx, 1);
    let dst_h = vm.arg_ref(ctx, 2);
    let r = read_rectf(vm, src_h);
    let mapped = read_matrix(vm.arg_i64(ctx, 0), |m| m.map_rect(&r))?;
    match mapped {
        Some(mapped) => {
            write_rectf(vm, dst_h, mapped)?;
            vm.ret_bool(ctx, true);
        }
        None => vm.ret_bool(ctx, false),
    }
    Ok(())
}

fn matrix_equals(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = read_matrix(vm.arg_i64(ctx, 0), |m| *m)?;
    let b = read_matrix(vm.arg_i64(ctx, 1), |m| *m)?;
    vm.ret_bool(ctx, a == b);
    Ok(())
}

fn matrix_rect_to_rect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = read_rectf(vm, vm.arg_ref(ctx, 1));
    let dst = read_rectf(vm, vm.arg_ref(ctx, 2));
    let mode = vm.arg_i32(ctx, 3);
    let fit = ScaleToFit::from_ordinal(mode).ok_or_else(|| {
        VmError::unsupported_api(
            format!("Matrix.ScaleToFit ordinal {mode}"),
            "android.graphics.Matrix",
        )
    })?;
    let ok = with_matrix(vm.arg_i64(ctx, 0), |m| m.set_rect_to_rect(&src, &dst, fit))?;
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn matrix_poly_to_poly(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = read_floats(vm, vm.arg_ref(ctx, 1), 0)?;
    let src_index = vm.arg_i32(ctx, 2).max(0) as usize;
    let dst = read_floats(vm, vm.arg_ref(ctx, 3), 0)?;
    let dst_index = vm.arg_i32(ctx, 4).max(0) as usize;
    let count = vm.arg_i32(ctx, 5).max(0) as usize;
    let collect = |a: &[f32], base: usize| -> Vec<PointF> {
        (0..count)
            .map(|i| {
                let b = base + i * 2;
                PointF::new(
                    a.get(b).copied().unwrap_or(0.0),
                    a.get(b + 1).copied().unwrap_or(0.0),
                )
            })
            .collect()
    };
    let sp = collect(&src, src_index);
    let dp = collect(&dst, dst_index);
    let ok = with_matrix(vm.arg_i64(ctx, 0), |m| m.set_poly_to_poly(&sp, &dp))?;
    vm.ret_bool(ctx, ok);
    Ok(())
}

// ---------------------------------------------------------------------------
// Array / RectF helpers
// ---------------------------------------------------------------------------

fn read_floats(vm: &Vm, h: u32, _min: usize) -> VmResult<Vec<f32>> {
    use crate::heap::{ArrayData, Object};
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Float(v) => Ok(v.clone()),
            _ => Err(VmError::Internal("expected float[]".to_string())),
        },
        _ => Err(VmError::Internal("expected float[]".to_string())),
    }
}

fn write_floats(vm: &mut Vm, h: u32, src: &[f32]) -> VmResult<()> {
    use crate::heap::{ArrayData, Object};
    if h == 0 {
        return Ok(());
    }
    let Some(Object::Array(a)) = vm.heap.get_mut(h) else {
        return Err(VmError::Internal("expected float[]".to_string()));
    };
    if let ArrayData::Float(v) = &mut a.data {
        for (slot, s) in v.iter_mut().zip(src.iter()) {
            *slot = *s;
        }
    }
    Ok(())
}

fn read_rectf(vm: &Vm, h: u32) -> RectF {
    let get = |name: &str| {
        vm.get_instance_field_by_name(h, name)
            .map(f32::from_bits)
            .unwrap_or(0.0)
    };
    RectF::new(get("left"), get("top"), get("right"), get("bottom"))
}

fn write_rectf(vm: &mut Vm, h: u32, r: RectF) -> VmResult<()> {
    for (name, v) in [
        ("left", r.left),
        ("top", r.top),
        ("right", r.right),
        ("bottom", r.bottom),
    ] {
        vm.set_instance_field_by_name(h, name, v.to_bits());
    }
    Ok(())
}
