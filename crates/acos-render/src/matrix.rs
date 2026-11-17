//! A 3×3 affine/perspective matrix with Android `android.graphics.Matrix`
//! semantics.
//!
//! Android exposes the nine floats in row-major order:
//! `[MSCALE_X, MSKEW_X, MTRANS_X, MSKEW_Y, MSCALE_Y, MTRANS_Y, MPERSP_0,
//! MPERSP_1, MPERSP_2]` and maps points as `[x y 1] · M`. We keep the same
//! layout and the same `preConcat` (`M = M·other`) / `postConcat`
//! (`M = other·M`) convention, so `Matrix.nPreTranslate`/`nPostScale`/… map
//! one-to-one onto these methods.

use crate::geom::{PointF, RectF};
use tiny_skia::Transform;

/// Identifiers for Android's `Matrix.ScaleToFit`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ScaleToFit {
    Fill,
    Start,
    Center,
    End,
}

impl ScaleToFit {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => ScaleToFit::Fill,
            1 => ScaleToFit::Start,
            2 => ScaleToFit::Center,
            3 => ScaleToFit::End,
            _ => return None,
        })
    }
}

/// Android-compatible 3×3 matrix.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Matrix3x3 {
    pub m: [f32; 9],
}

impl Default for Matrix3x3 {
    fn default() -> Self {
        Self::identity()
    }
}

impl Matrix3x3 {
    #[inline]
    pub const fn identity() -> Self {
        Matrix3x3 {
            m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }
    }

    #[inline]
    pub fn reset(&mut self) {
        *self = Self::identity();
    }

    #[inline]
    pub fn is_identity(&self) -> bool {
        self.m == [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
    }

    pub fn is_affine(&self) -> bool {
        self.m[6] == 0.0 && self.m[7] == 0.0 && self.m[8] == 1.0
    }

    pub fn get_values(&self) -> [f32; 9] {
        self.m
    }

    pub fn set_values(&mut self, v: [f32; 9]) {
        self.m = v;
    }

    /// Standard matrix product `self · other`.
    pub fn mul(&self, other: &Matrix3x3) -> Matrix3x3 {
        let a = &self.m;
        let b = &other.m;
        let mut r = [0.0f32; 9];
        for row in 0..3 {
            for col in 0..3 {
                let mut sum = 0.0;
                for k in 0..3 {
                    sum += a[row * 3 + k] * b[k * 3 + col];
                }
                r[row * 3 + col] = sum;
            }
        }
        Matrix3x3 { m: r }
    }

    /// `this = this · other` (Android `preConcat`).
    pub fn pre_concat(&mut self, other: &Matrix3x3) {
        *self = self.mul(other);
    }

    /// `this = other · this` (Android `postConcat`).
    pub fn post_concat(&mut self, other: &Matrix3x3) {
        *self = other.mul(self);
    }

    pub fn set_concat(&mut self, a: &Matrix3x3, b: &Matrix3x3) {
        *self = a.mul(b);
    }

    pub fn set(&mut self, other: &Matrix3x3) {
        self.m = other.m;
    }

    pub fn translate_matrix(dx: f32, dy: f32) -> Self {
        let mut m = Self::identity();
        m.m[2] = dx;
        m.m[5] = dy;
        m
    }

    pub fn scale_matrix(sx: f32, sy: f32) -> Self {
        let mut m = Self::identity();
        m.m[0] = sx;
        m.m[4] = sy;
        m
    }

    #[inline]
    pub fn set_translate(&mut self, dx: f32, dy: f32) {
        *self = Self::translate_matrix(dx, dy);
    }

    #[inline]
    pub fn pre_translate(&mut self, dx: f32, dy: f32) {
        self.pre_concat(&Self::translate_matrix(dx, dy));
    }

    #[inline]
    pub fn post_translate(&mut self, dx: f32, dy: f32) {
        self.post_concat(&Self::translate_matrix(dx, dy));
    }

    #[inline]
    pub fn set_scale(&mut self, sx: f32, sy: f32) {
        *self = Self::scale_matrix(sx, sy);
    }

    #[inline]
    pub fn pre_scale(&mut self, sx: f32, sy: f32) {
        self.pre_concat(&Self::scale_matrix(sx, sy));
    }

    #[inline]
    pub fn post_scale(&mut self, sx: f32, sy: f32) {
        self.post_concat(&Self::scale_matrix(sx, sy));
    }

    pub fn set_scale_pivot(&mut self, sx: f32, sy: f32, px: f32, py: f32) {
        // T(px,py) · S · T(-px,-py)
        let mut m = Self::translate_matrix(px, py);
        m.pre_concat(&Self::scale_matrix(sx, sy));
        m.pre_concat(&Self::translate_matrix(-px, -py));
        *self = m;
    }

    pub fn rotate_matrix(degrees: f32) -> Self {
        let r = degrees.to_radians();
        let (s, c) = r.sin_cos();
        let mut m = Self::identity();
        m.m[0] = c;
        m.m[1] = -s;
        m.m[3] = s;
        m.m[4] = c;
        m
    }

    #[inline]
    pub fn set_rotate(&mut self, degrees: f32) {
        *self = Self::rotate_matrix(degrees);
    }

    pub fn set_rotate_pivot(&mut self, degrees: f32, px: f32, py: f32) {
        let mut m = Self::translate_matrix(px, py);
        m.pre_concat(&Self::rotate_matrix(degrees));
        m.pre_concat(&Self::translate_matrix(-px, -py));
        *self = m;
    }

    #[inline]
    pub fn pre_rotate(&mut self, degrees: f32) {
        self.pre_concat(&Self::rotate_matrix(degrees));
    }

    #[inline]
    pub fn post_rotate(&mut self, degrees: f32) {
        self.post_concat(&Self::rotate_matrix(degrees));
    }

    pub fn skew_matrix(kx: f32, ky: f32) -> Self {
        let mut m = Self::identity();
        m.m[1] = kx;
        m.m[3] = ky;
        m
    }

    #[inline]
    pub fn set_skew(&mut self, kx: f32, ky: f32) {
        *self = Self::skew_matrix(kx, ky);
    }

    pub fn set_skew_pivot(&mut self, kx: f32, ky: f32, px: f32, py: f32) {
        let mut m = Self::translate_matrix(px, py);
        m.pre_concat(&Self::skew_matrix(kx, ky));
        m.pre_concat(&Self::translate_matrix(-px, -py));
        *self = m;
    }

    /// Determinant of the 3×3 matrix.
    pub fn determinant(&self) -> f32 {
        let m = &self.m;
        m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6])
    }

    /// Invert in place. Returns `false` if singular.
    pub fn invert(&mut self) -> bool {
        let inv = match self.inverted() {
            Some(v) => v,
            None => return false,
        };
        *self = inv;
        true
    }

    pub fn inverted(&self) -> Option<Matrix3x3> {
        let m = &self.m;
        let det = self.determinant();
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let inv_det = 1.0 / det;
        let mut r = [0.0f32; 9];
        // Cofactor / adjugate.
        r[0] = (m[4] * m[8] - m[5] * m[7]) * inv_det;
        r[1] = (m[2] * m[7] - m[1] * m[8]) * inv_det;
        r[2] = (m[1] * m[5] - m[2] * m[4]) * inv_det;
        r[3] = (m[5] * m[6] - m[3] * m[8]) * inv_det;
        r[4] = (m[0] * m[8] - m[2] * m[6]) * inv_det;
        r[5] = (m[2] * m[3] - m[0] * m[5]) * inv_det;
        r[6] = (m[3] * m[7] - m[4] * m[6]) * inv_det;
        r[7] = (m[1] * m[6] - m[0] * m[7]) * inv_det;
        r[8] = (m[0] * m[4] - m[1] * m[3]) * inv_det;
        Some(Matrix3x3 { m: r })
    }

    /// Map a point with perspective divide.
    pub fn map_point(&self, p: PointF) -> PointF {
        let m = &self.m;
        let x = m[0] * p.x + m[1] * p.y + m[2];
        let y = m[3] * p.x + m[4] * p.y + m[5];
        let w = m[6] * p.x + m[7] * p.y + m[8];
        if w != 0.0 && w != 1.0 {
            PointF::new(x / w, y / w)
        } else {
            PointF::new(x, y)
        }
    }

    /// Map an array of points in place, applying perspective divide.
    pub fn map_points(&self, pts: &mut [PointF]) {
        for p in pts.iter_mut() {
            *p = self.map_point(*p);
        }
    }

    /// Map a rectangle (transformed bounds). Returns `None` for empty input.
    pub fn map_rect(&self, r: &RectF) -> Option<RectF> {
        if r.is_empty() {
            return None;
        }
        let corners = [
            PointF::new(r.left, r.top),
            PointF::new(r.right, r.top),
            PointF::new(r.left, r.bottom),
            PointF::new(r.right, r.bottom),
        ];
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for c in corners {
            let p = self.map_point(c);
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
        Some(RectF::new(min_x, min_y, max_x, max_y))
    }

    /// Android `mapRadius`: average of the mapped x-radius and y-radius.
    pub fn map_radius(&self, radius: f32) -> f32 {
        let x = self.map_point(PointF::new(radius, 0.0));
        let y = self.map_point(PointF::new(0.0, radius));
        let lx = (x.x * x.x + x.y * x.y).sqrt();
        let ly = (y.x * y.x + y.y * y.y).sqrt();
        (lx + ly) * 0.5
    }

    /// Derive the affine `Transform` used by tiny-skia. Returns `None` when
    /// the matrix has a non-trivial perspective row (Android accepts it, but
    /// tiny-skia's rasteriser is affine-only; callers label such draws
    /// approximate).
    pub fn to_affine(&self) -> Option<Transform> {
        if !self.is_affine() {
            return None;
        }
        let m = &self.m;
        // tiny-skia: x' = sx*x + kx*y + tx ; y' = ky*x + sy*y + ty
        Some(Transform::from_row(m[0], m[3], m[1], m[4], m[2], m[5]))
    }

    /// `true` when mapping does not require perspective division.
    pub fn rect_stays_rect(&self) -> bool {
        // Non-zero only in the diagonal / translation.
        self.m[1] == 0.0 && self.m[3] == 0.0 && self.is_affine()
    }

    /// Android `setRectToRect`.
    pub fn set_rect_to_rect(&mut self, src: &RectF, dst: &RectF, mode: ScaleToFit) -> bool {
        if src.is_empty() || dst.is_empty() {
            return false;
        }
        let sx = dst.width() / src.width();
        let sy = dst.height() / src.height();
        if !sx.is_finite() || !sy.is_finite() {
            return false;
        }
        let (use_sx, use_sy, tx, ty) = if mode == ScaleToFit::Fill {
            (sx, sy, dst.left - src.left * sx, dst.top - src.top * sy)
        } else {
            // START / CENTER / END preserve aspect ratio with a uniform scale.
            let s = sx.min(sy);
            let w = src.width() * s;
            let h = src.height() * s;
            let mut tx = dst.left - src.left * s;
            let mut ty = dst.top - src.top * s;
            match mode {
                ScaleToFit::Center => {
                    tx += (dst.width() - w) * 0.5;
                    ty += (dst.height() - h) * 0.5;
                }
                ScaleToFit::End => {
                    tx += dst.width() - w;
                    ty += dst.height() - h;
                }
                _ => {}
            }
            (s, s, tx, ty)
        };
        let mut m = Self::identity();
        m.m[0] = use_sx;
        m.m[4] = use_sy;
        m.m[2] = tx;
        m.m[5] = ty;
        *self = m;
        true
    }

    /// Android `setPolyToPoly`. `src` and `dst` must be the same length
    /// (0..=4). Returns `false` when the solution is degenerate.
    pub fn set_poly_to_poly(&mut self, src: &[PointF], dst: &[PointF]) -> bool {
        let n = src.len();
        if n != dst.len() || n > 4 {
            return false;
        }
        match n {
            0 => {
                *self = Self::identity();
                true
            }
            1 => {
                self.set_translate(dst[0].x - src[0].x, dst[0].y - src[0].y);
                true
            }
            2 => {
                // Similarity (scale + rotate + translate) between two segments.
                let (s0, s1) = (src[0], src[1]);
                let (d0, d1) = (dst[0], dst[1]);
                let sdx = s1.x - s0.x;
                let sdy = s1.y - s0.y;
                let ddx = d1.x - d0.x;
                let ddy = d1.y - d0.y;
                let slen = (sdx * sdx + sdy * sdy).sqrt();
                let dlen = (ddx * ddx + ddy * ddy).sqrt();
                if slen == 0.0 {
                    return false;
                }
                let scale = dlen / slen;
                let cos = (sdx * ddx + sdy * ddy) / (slen * dlen).max(1e-20);
                let sin = (sdx * ddy - sdy * ddx) / (slen * dlen).max(1e-20);
                let mut m = Self::identity();
                m.m[0] = scale * cos;
                m.m[1] = -scale * sin;
                m.m[3] = scale * sin;
                m.m[4] = scale * cos;
                m.m[2] = d0.x - (m.m[0] * s0.x + m.m[1] * s0.y);
                m.m[5] = d0.y - (m.m[3] * s0.x + m.m[4] * s0.y);
                *self = m;
                true
            }
            3 => {
                // Solve the affine system for the six unknowns.
                let mut a = [[0.0f32; 6]; 6];
                let mut b = [0.0f32; 6];
                for i in 0..3 {
                    let (x, y) = (src[i].x, src[i].y);
                    a[i * 2] = [x, y, 1.0, 0.0, 0.0, 0.0];
                    b[i * 2] = dst[i].x;
                    a[i * 2 + 1] = [0.0, 0.0, 0.0, x, y, 1.0];
                    b[i * 2 + 1] = dst[i].y;
                }
                match solve6(a, b) {
                    Some(sol) => {
                        let mut m = Self::identity();
                        m.m[0] = sol[0];
                        m.m[1] = sol[1];
                        m.m[2] = sol[2];
                        m.m[3] = sol[3];
                        m.m[4] = sol[4];
                        m.m[5] = sol[5];
                        *self = m;
                        true
                    }
                    None => false,
                }
            }
            4 => {
                // Homography: solve for the 8 unknowns h0..h7.
                let mut a = [[0.0f32; 8]; 8];
                let mut b = [0.0f32; 8];
                for i in 0..4 {
                    let (x, y) = (src[i].x, src[i].y);
                    let (u, v) = (dst[i].x, dst[i].y);
                    a[i * 2] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y];
                    b[i * 2] = u;
                    a[i * 2 + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y];
                    b[i * 2 + 1] = v;
                }
                match solve8(a, b) {
                    Some(sol) => {
                        self.m = [
                            sol[0], sol[1], sol[2], sol[3], sol[4], sol[5], sol[6], sol[7], 1.0,
                        ];
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }
}

fn solve6(mut a: [[f32; 6]; 6], mut b: [f32; 6]) -> Option<[f32; 6]> {
    gaussian6(&mut a, &mut b).then_some(b)
}

fn gaussian6(a: &mut [[f32; 6]; 6], b: &mut [f32; 6]) -> bool {
    for col in 0..6 {
        let mut pivot = col;
        for row in col + 1..6 {
            if a[row][col].abs() > a[pivot][col].abs() {
                pivot = row;
            }
        }
        if a[pivot][col].abs() < 1e-9 {
            return false;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        let p = a[col][col];
        for k in col..6 {
            a[col][k] /= p;
        }
        b[col] /= p;
        for row in 0..6 {
            if row != col {
                let f = a[row][col];
                if f != 0.0 {
                    for k in col..6 {
                        a[row][k] -= f * a[col][k];
                    }
                    b[row] -= f * b[col];
                }
            }
        }
    }
    true
}

fn solve8(mut a: [[f32; 8]; 8], mut b: [f32; 8]) -> Option<[f32; 8]> {
    for col in 0..8 {
        let mut pivot = col;
        for row in col + 1..8 {
            if a[row][col].abs() > a[pivot][col].abs() {
                pivot = row;
            }
        }
        if a[pivot][col].abs() < 1e-9 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        let p = a[col][col];
        for k in col..8 {
            a[col][k] /= p;
        }
        b[col] /= p;
        for row in 0..8 {
            if row != col {
                let f = a[row][col];
                if f != 0.0 {
                    for k in col..8 {
                        a[row][k] -= f * a[col][k];
                    }
                    b[row] -= f * b[col];
                }
            }
        }
    }
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn identity_is_identity() {
        assert!(Matrix3x3::identity().is_identity());
        assert!(Matrix3x3::identity().is_affine());
    }

    #[test]
    fn pre_translate_then_map() {
        // Start with a 2x scale; pre-translate by (10, 20) means the
        // translation happens *before* the existing transform, i.e. the point
        // is moved then scaled.
        let mut m = Matrix3x3::scale_matrix(2.0, 2.0);
        m.pre_translate(10.0, 20.0);
        let p = m.map_point(PointF::new(1.0, 1.0));
        assert!(approx(p.x, 22.0), "{p:?}");
        assert!(approx(p.y, 42.0), "{p:?}");
    }

    #[test]
    fn post_translate_then_map() {
        let mut m = Matrix3x3::scale_matrix(2.0, 2.0);
        m.post_translate(10.0, 20.0);
        let p = m.map_point(PointF::new(1.0, 1.0));
        assert!(approx(p.x, 12.0), "{p:?}");
        assert!(approx(p.y, 22.0), "{p:?}");
    }

    #[test]
    fn rotate_90() {
        let m = Matrix3x3::rotate_matrix(90.0);
        let p = m.map_point(PointF::new(1.0, 0.0));
        assert!(approx(p.x, 0.0));
        assert!(approx(p.y, 1.0));
    }

    #[test]
    fn invert_round_trip() {
        let mut m = Matrix3x3::identity();
        m.post_scale(3.0, 4.0);
        m.post_rotate(30.0);
        m.post_translate(5.0, -2.0);
        let inv = m.inverted().unwrap();
        let p = PointF::new(7.0, 9.0);
        let q = inv.map_point(m.map_point(p));
        assert!(approx(q.x, p.x), "{q:?}");
        assert!(approx(q.y, p.y), "{q:?}");
    }

    #[test]
    fn rect_to_rect_center() {
        let src = RectF::new(0.0, 0.0, 100.0, 50.0);
        let dst = RectF::new(0.0, 0.0, 400.0, 400.0);
        let mut m = Matrix3x3::identity();
        assert!(m.set_rect_to_rect(&src, &dst, ScaleToFit::Center));
        let p = m.map_point(PointF::new(0.0, 0.0));
        // scale = 4, width = 400, height = 200 => centered vertically
        assert!(approx(p.x, 0.0), "{p:?}");
        assert!(approx(p.y, 100.0), "{p:?}");
    }

    #[test]
    fn poly_to_poly_affine() {
        let src = [
            PointF::new(0.0, 0.0),
            PointF::new(1.0, 0.0),
            PointF::new(0.0, 1.0),
        ];
        let dst = [
            PointF::new(10.0, 20.0),
            PointF::new(12.0, 20.0),
            PointF::new(10.0, 23.0),
        ];
        let mut m = Matrix3x3::identity();
        assert!(m.set_poly_to_poly(&src, &dst));
        let p = m.map_point(PointF::new(1.0, 1.0));
        assert!(approx(p.x, 12.0), "{p:?}");
        assert!(approx(p.y, 23.0), "{p:?}");
    }

    #[test]
    fn affine_rejects_perspective() {
        let mut m = Matrix3x3::identity();
        m.m[7] = 0.5;
        assert!(m.to_affine().is_none());
    }
}
