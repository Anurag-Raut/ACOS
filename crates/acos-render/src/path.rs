//! `android.graphics.Path` — a verb list that converts to tiny-skia on demand.
//!
//! Android's `Path` is mutable (`moveTo`/`lineTo`/… append), so we keep our own
//! verb list and build a `tiny_skia::Path` only when rasterising. Boolean path
//! ops (`Path.op`) are *not* provided by tiny-skia; they are reported as
//! `RENDER_UNSUPPORTED_OP` rather than silently skipped.

use crate::geom::{PointF, RectF};
use crate::matrix::Matrix3x3;
use crate::RenderError;
use tiny_skia::{FillRule, PathBuilder};

/// Android `Path.FillType`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum FillType {
    #[default]
    Winding = 0,
    EvenOdd = 1,
    InverseWinding = 2,
    InverseEvenOdd = 3,
}

impl FillType {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => FillType::Winding,
            1 => FillType::EvenOdd,
            2 => FillType::InverseWinding,
            3 => FillType::InverseEvenOdd,
            _ => return None,
        })
    }

    pub fn is_inverse(self) -> bool {
        matches!(self, FillType::InverseWinding | FillType::InverseEvenOdd)
    }

    /// Map to the tiny-skia fill rule (inverse types use the non-inverse rule
    /// plus a mask inversion at raster time).
    pub fn fill_rule(self) -> FillRule {
        match self {
            FillType::Winding | FillType::InverseWinding => FillRule::Winding,
            FillType::EvenOdd | FillType::InverseEvenOdd => FillRule::EvenOdd,
        }
    }
}

/// A path verb.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Verb {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    QuadTo(f32, f32, f32, f32),
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

/// `android.graphics.Path`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Path {
    verbs: Vec<Verb>,
    fill_type: FillType,
}

impl Path {
    pub fn new() -> Self {
        Path {
            verbs: Vec::new(),
            fill_type: FillType::Winding,
        }
    }

    /// Android `Path.reset()`.
    pub fn reset(&mut self) {
        self.verbs.clear();
        self.fill_type = FillType::Winding;
    }

    /// Android `Path.rewind()` — keeps the fill type, drops geometry.
    pub fn rewind(&mut self) {
        self.verbs.clear();
    }

    pub fn set(&mut self, other: &Path) {
        self.verbs = other.verbs.clone();
        self.fill_type = other.fill_type;
    }

    pub fn fill_type(&self) -> FillType {
        self.fill_type
    }

    pub fn set_fill_type(&mut self, ft: FillType) {
        self.fill_type = ft;
    }

    pub fn is_empty(&self) -> bool {
        self.verbs.is_empty()
    }

    pub fn verbs(&self) -> &[Verb] {
        &self.verbs
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.verbs.push(Verb::MoveTo(x, y));
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        self.verbs.push(Verb::LineTo(x, y));
    }

    pub fn quad_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32) {
        self.verbs.push(Verb::QuadTo(x1, y1, x2, y2));
    }

    pub fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) {
        self.verbs.push(Verb::CubicTo(x1, y1, x2, y2, x3, y3));
    }

    /// `Path.conicTo`. A rational quadratic is approximated by a cubic, which
    /// is exact for `weight == 1` and a very close approximation otherwise.
    pub fn conic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, weight: f32) {
        let last = self.last_point().unwrap_or(PointF::new(x1, y1));
        let w = if weight.is_finite() { weight.max(0.0) } else { 1.0 };
        if (w - 1.0).abs() < 1e-6 {
            self.quad_to(x1, y1, x2, y2);
            return;
        }
        let c = 2.0 * w / (3.0 * (1.0 + w));
        let c1x = last.x + c * (x1 - last.x);
        let c1y = last.y + c * (y1 - last.y);
        let c2x = x2 + c * (x1 - x2);
        let c2y = y2 + c * (y1 - y2);
        self.cubic_to(c1x, c1y, c2x, c2y, x2, y2);
    }

    pub fn close(&mut self) {
        self.verbs.push(Verb::Close);
    }

    pub fn last_point(&self) -> Option<PointF> {
        if let Some(v) = self.verbs.last() {
            match *v {
                Verb::MoveTo(x, y) | Verb::LineTo(x, y) => return Some(PointF::new(x, y)),
                Verb::QuadTo(_, _, x, y) => return Some(PointF::new(x, y)),
                Verb::CubicTo(_, _, _, _, x, y) => return Some(PointF::new(x, y)),
                Verb::Close => return None,
            }
        }
        None
    }

    /// Android `Path.addRect`.
    pub fn add_rect(&mut self, l: f32, t: f32, r: f32, b: f32, direction_ccw: bool) {
        if direction_ccw {
            self.move_to(l, t);
            self.line_to(l, b);
            self.line_to(r, b);
            self.line_to(r, t);
        } else {
            self.move_to(l, t);
            self.line_to(r, t);
            self.line_to(r, b);
            self.line_to(l, b);
        }
        self.close();
    }

    /// Android `Path.addOval`.
    pub fn add_oval(&mut self, l: f32, t: f32, r: f32, b: f32, direction_ccw: bool) {
        let sweep = if direction_ccw { -360.0 } else { 360.0 };
        let oval = RectF::new(l, t, r, b);
        self.arc_to(oval, 0.0, sweep, true);
        self.close();
    }

    pub fn add_circle(&mut self, cx: f32, cy: f32, radius: f32, direction_ccw: bool) {
        self.add_oval(cx - radius, cy - radius, cx + radius, cy + radius, direction_ccw);
    }

    /// Android `Path.addRoundRect` (uniform or elliptical radii).
    pub fn add_round_rect(&mut self, r: &RectF, rx: f32, ry: f32, direction_ccw: bool) {
        let rx = rx.min(r.width() * 0.5).max(0.0);
        let ry = ry.min(r.height() * 0.5).max(0.0);
        let (l, t, rr, b) = (r.left, r.top, r.right, r.bottom);
        if rx <= 0.0 || ry <= 0.0 {
            self.add_rect(l, t, rr, b, direction_ccw);
            return;
        }
        // Cubic approximation of a 90° elliptical corner (kappa).
        let k = 0.552_284_8;
        let (kxc, kyc) = (rx * k, ry * k);
        if !direction_ccw {
            self.move_to(l + rx, t);
            self.line_to(rr - rx, t);
            self.cubic_to(rr - rx + kxc, t, rr, t + ry - kyc, rr, t + ry);
            self.line_to(rr, b - ry);
            self.cubic_to(rr, b - ry + kyc, rr - rx + kxc, b, rr - rx, b);
            self.line_to(l + rx, b);
            self.cubic_to(l + rx - kxc, b, l, b - ry + kyc, l, b - ry);
            self.line_to(l, t + ry);
            self.cubic_to(l, t + ry - kyc, l + rx - kxc, t, l + rx, t);
        } else {
            self.move_to(l + rx, t);
            self.line_to(l, t + ry);
            self.cubic_to(l, t + ry - kyc, l + rx - kxc, t, l + rx, t);
            self.line_to(rr - rx, t);
            self.cubic_to(rr - rx + kxc, t, rr, t + ry - kyc, rr, t + ry);
            self.line_to(rr, b - ry);
            self.cubic_to(rr, b - ry + kyc, rr - rx + kxc, b, rr - rx, b);
            self.line_to(l + rx, b);
            self.cubic_to(l + rx - kxc, b, l, b - ry + kyc, l, b - ry);
        }
        self.close();
    }

    pub fn add_path(&mut self, other: &Path) {
        self.verbs.extend_from_slice(&other.verbs);
    }

    /// Android `Path.arcTo(oval, startAngle, sweepAngle, forceMoveTo)`.
    pub fn arc_to(&mut self, oval: RectF, start_angle: f32, sweep_angle: f32, force_move_to: bool) {
        if sweep_angle == 0.0 || oval.is_empty() {
            if force_move_to {
                self.move_to(oval.left, oval.top);
            } else {
                self.line_to(oval.left, oval.top);
            }
            return;
        }
        let cx = oval.center_x();
        let cy = oval.center_y();
        let rx = oval.width() * 0.5;
        let ry = oval.height() * 0.5;
        // Android measures angles in degrees, 0 = 3 o'clock, positive = clockwise
        // in screen coordinates (y down).
        let a0 = start_angle.to_radians();
        let a1 = (start_angle + sweep_angle).to_radians();
        let start = PointF::new(cx + rx * a0.cos(), cy + ry * a0.sin());
        if force_move_to {
            self.move_to(start.x, start.y);
        } else {
            self.line_to(start.x, start.y);
        }
        let segments = (sweep_angle.abs() / 90.0).ceil().max(1.0) as usize;
        let delta = (a1 - a0) / segments as f32;
        let k = 4.0 / 3.0 * (delta / 4.0).tan();
        let mut theta = a0;
        for _ in 0..segments {
            let next = theta + delta;
            let p0 = PointF::new(cx + rx * theta.cos(), cy + ry * theta.sin());
            let p1 = PointF::new(cx + rx * next.cos(), cy + ry * next.sin());
            let d0 = PointF::new(-rx * theta.sin(), ry * theta.cos());
            let d1 = PointF::new(-rx * next.sin(), ry * next.cos());
            let c1 = PointF::new(p0.x + k * d0.x, p0.y + k * d0.y);
            let c2 = PointF::new(p1.x - k * d1.x, p1.y - k * d1.y);
            self.cubic_to(c1.x, c1.y, c2.x, c2.y, p1.x, p1.y);
            theta = next;
        }
    }

    /// Android `Path.transform(matrix)`.
    pub fn transform(&mut self, m: &Matrix3x3) {
        for v in self.verbs.iter_mut() {
            match v {
                Verb::MoveTo(x, y) | Verb::LineTo(x, y) => {
                    let p = m.map_point(PointF::new(*x, *y));
                    *x = p.x;
                    *y = p.y;
                }
                Verb::QuadTo(x1, y1, x2, y2) => {
                    let a = m.map_point(PointF::new(*x1, *y1));
                    let b = m.map_point(PointF::new(*x2, *y2));
                    *x1 = a.x;
                    *y1 = a.y;
                    *x2 = b.x;
                    *y2 = b.y;
                }
                Verb::CubicTo(x1, y1, x2, y2, x3, y3) => {
                    let a = m.map_point(PointF::new(*x1, *y1));
                    let b = m.map_point(PointF::new(*x2, *y2));
                    let c = m.map_point(PointF::new(*x3, *y3));
                    *x1 = a.x;
                    *y1 = a.y;
                    *x2 = b.x;
                    *y2 = b.y;
                    *x3 = c.x;
                    *y3 = c.y;
                }
                Verb::Close => {}
            }
        }
    }

    /// Android `Path.computeBounds(rect, exact=false)` — the control-point
    /// bounds, which is what the framework relies on for layout.
    pub fn compute_bounds(&self) -> Option<RectF> {
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut any = false;
        let mut push = |x: f32, y: f32| {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        };
        for v in &self.verbs {
            match *v {
                Verb::MoveTo(x, y) | Verb::LineTo(x, y) => push(x, y),
                Verb::QuadTo(x1, y1, x2, y2) => {
                    push(x1, y1);
                    push(x2, y2);
                }
                Verb::CubicTo(x1, y1, x2, y2, x3, y3) => {
                    push(x1, y1);
                    push(x2, y2);
                    push(x3, y3);
                }
                Verb::Close => {}
            }
            any = true;
        }
        if any && min_x <= max_x {
            Some(RectF::new(min_x, min_y, max_x, max_y))
        } else {
            None
        }
    }

    /// `true` when the path traces an axis-aligned rectangle.
    pub fn is_rect(&self) -> bool {
        let pts: Vec<PointF> = self
            .verbs
            .iter()
            .filter_map(|v| match *v {
                Verb::MoveTo(x, y) | Verb::LineTo(x, y) => Some(PointF::new(x, y)),
                _ => None,
            })
            .collect();
        if pts.len() != 4 {
            return false;
        }
        let Some(b) = self.compute_bounds() else {
            return false;
        };
        for p in &pts {
            let on_x = (p.x - b.left).abs() < 1e-4 || (p.x - b.right).abs() < 1e-4;
            let on_y = (p.y - b.top).abs() < 1e-4 || (p.y - b.bottom).abs() < 1e-4;
            if !(on_x && on_y) {
                return false;
            }
        }
        true
    }

    pub fn from_rect(r: RectF) -> Path {
        let mut p = Path::new();
        p.add_rect(r.left, r.top, r.right, r.bottom, false);
        p
    }

    pub fn from_round_rect(r: RectF, rx: f32, ry: f32) -> Path {
        let mut p = Path::new();
        p.add_round_rect(&r, rx, ry, false);
        p
    }

    /// Build the tiny-skia path for rasterisation.
    pub fn build_tiny_skia(&self) -> Option<tiny_skia::Path> {
        if self.verbs.is_empty() {
            return None;
        }
        let mut b = PathBuilder::new();
        for v in &self.verbs {
            match *v {
                Verb::MoveTo(x, y) => b.move_to(x, y),
                Verb::LineTo(x, y) => b.line_to(x, y),
                Verb::QuadTo(x1, y1, x2, y2) => b.quad_to(x1, y1, x2, y2),
                Verb::CubicTo(x1, y1, x2, y2, x3, y3) => b.cubic_to(x1, y1, x2, y2, x3, y3),
                Verb::Close => b.close(),
            }
        }
        b.finish()
    }

    /// Android `Path.setPath`-style transform returning a new path.
    pub fn transformed(&self, m: &Matrix3x3) -> Path {
        let mut p = self.clone();
        p.transform(m);
        p
    }

    /// Flatten to polylines (tolerance in device units) for `PathMeasure`.
    pub fn flatten(&self, tolerance: f32) -> Vec<Vec<PointF>> {
        let mut out: Vec<Vec<PointF>> = Vec::new();
        let mut current: Vec<PointF> = Vec::new();
        let mut cursor = PointF::default();
        let mut start = PointF::default();
        let tol = tolerance.max(0.05);
        for v in &self.verbs {
            match *v {
                Verb::MoveTo(x, y) => {
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                    cursor = PointF::new(x, y);
                    start = cursor;
                    current.push(cursor);
                }
                Verb::LineTo(x, y) => {
                    cursor = PointF::new(x, y);
                    current.push(cursor);
                }
                Verb::QuadTo(x1, y1, x2, y2) => {
                    flatten_quad(cursor, PointF::new(x1, y1), PointF::new(x2, y2), tol, &mut current);
                    cursor = PointF::new(x2, y2);
                }
                Verb::CubicTo(x1, y1, x2, y2, x3, y3) => {
                    flatten_cubic(
                        cursor,
                        PointF::new(x1, y1),
                        PointF::new(x2, y2),
                        PointF::new(x3, y3),
                        tol,
                        &mut current,
                    );
                    cursor = PointF::new(x3, y3);
                }
                Verb::Close => {
                    if !current.is_empty() {
                        current.push(start);
                        out.push(std::mem::take(&mut current));
                    }
                    cursor = start;
                }
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
        out
    }
}

fn flatten_quad(p0: PointF, p1: PointF, p2: PointF, tol: f32, out: &mut Vec<PointF>) {
    // Flatness test for quadratic Bézier.
    let dx = p0.x - 2.0 * p1.x + p2.x;
    let dy = p0.y - 2.0 * p1.y + p2.y;
    if (dx * dx + dy * dy).sqrt() <= tol {
        out.push(p2);
        return;
    }
    let m0 = mid(p0, p1);
    let m1 = mid(p1, p2);
    let m = mid(m0, m1);
    flatten_quad(p0, m0, m, tol, out);
    flatten_quad(m, m1, p2, tol, out);
}

fn flatten_cubic(p0: PointF, p1: PointF, p2: PointF, p3: PointF, tol: f32, out: &mut Vec<PointF>) {
    let d1 = (p0.x - 2.0 * p1.x + p2.x).abs() + (p0.y - 2.0 * p1.y + p2.y).abs();
    let d2 = (p1.x - 2.0 * p2.x + p3.x).abs() + (p1.y - 2.0 * p2.y + p3.y).abs();
    if (d1 + d2) * (d1 + d2) <= 16.0 * tol * tol {
        out.push(p3);
        return;
    }
    let m0 = mid(p0, p1);
    let m1 = mid(p1, p2);
    let m2 = mid(p2, p3);
    let a = mid(m0, m1);
    let b = mid(m1, m2);
    let m = mid(a, b);
    flatten_cubic(p0, m0, a, m, tol, out);
    flatten_cubic(m, b, m2, p3, tol, out);
}

#[inline]
fn mid(a: PointF, b: PointF) -> PointF {
    PointF::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
}

/// `android.graphics.PathMeasure` implemented over the flattened path.
pub struct PathMeasure {
    contours: Vec<Vec<f32>>, // cumulative lengths
    points: Vec<Vec<PointF>>,
    total: f32,
    forced_closed: bool,
}

impl PathMeasure {
    pub fn new(path: &Path, force_closed: bool) -> Self {
        let mut points = path.flatten(0.2);
        if force_closed {
            for c in points.iter_mut() {
                if c.len() > 1 && c.first() != c.last() {
                    let first = c[0];
                    c.push(first);
                }
            }
        }
        let mut contours = Vec::with_capacity(points.len());
        let mut total = 0.0;
        for c in &points {
            let mut lens = Vec::with_capacity(c.len());
            let mut acc = 0.0f32;
            lens.push(0.0);
            for w in c.windows(2) {
                acc += dist(w[0], w[1]);
                lens.push(acc);
            }
            total += acc;
            contours.push(lens);
        }
        PathMeasure {
            contours,
            points,
            total,
            forced_closed: force_closed,
        }
    }

    pub fn total_length(&self) -> f32 {
        self.total
    }

    /// `getPosTan(distance, pos, tan)` — position and unit tangent.
    pub fn get_pos_tan(&self, distance: f32) -> Option<(PointF, PointF)> {
        let mut remaining = distance;
        for (c, lens) in self.points.iter().zip(&self.contours) {
            let len = *lens.last().unwrap_or(&0.0);
            if remaining > len {
                remaining -= len;
                continue;
            }
            return pos_tan_on(c, lens, remaining);
        }
        None
    }

    /// `getSegment(start, stop, dst, startWithMoveTo)`.
    pub fn get_segment(&self, start: f32, stop: f32, start_with_move_to: bool) -> Path {
        let mut out = Path::new();
        let mut seg_start = start;
        let mut seg_stop = stop;
        let reversed = start > stop;
        if reversed {
            std::mem::swap(&mut seg_start, &mut seg_stop);
        }
        let mut base = 0.0;
        let mut first = true;
        for (c, lens) in self.points.iter().zip(&self.contours) {
            let len = *lens.last().unwrap_or(&0.0);
            let c_start = base;
            let c_end = base + len;
            if seg_stop < c_start || seg_start > c_end {
                base = c_end;
                if base > seg_stop {
                    break;
                }
                continue;
            }
            let local_start = (seg_start - c_start).max(0.0);
            let local_stop = (seg_stop - c_start).min(len);
            let pts = segment_points(c, lens, local_start, local_stop);
            let mut pts = pts;
            if reversed {
                pts.reverse();
            }
            if pts.len() >= 2 {
                if first && start_with_move_to {
                    out.move_to(pts[0].x, pts[0].y);
                } else if first {
                    out.line_to(pts[0].x, pts[0].y);
                }
                for p in &pts[1..] {
                    out.line_to(p.x, p.y);
                }
                first = false;
            }
            base = c_end;
            if base > seg_stop {
                break;
            }
        }
        out
    }

    pub fn is_closed(&self) -> bool {
        self.forced_closed
    }
}

fn pos_tan_on(c: &[PointF], lens: &[f32], d: f32) -> Option<(PointF, PointF)> {
    if c.len() < 2 {
        return None;
    }
    for i in 0..c.len() - 1 {
        let l0 = lens[i];
        let l1 = lens[i + 1];
        if d >= l0 && d <= l1 {
            let seg = l1 - l0;
            let t = if seg > 0.0 { (d - l0) / seg } else { 0.0 };
            let a = c[i];
            let b = c[i + 1];
            let pos = PointF::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
            let dx = b.x - a.x;
            let dy = b.y - a.y;
            let len = (dx * dx + dy * dy).sqrt().max(1e-6);
            return Some((pos, PointF::new(dx / len, dy / len)));
        }
    }
    None
}

fn segment_points(c: &[PointF], lens: &[f32], start: f32, stop: f32) -> Vec<PointF> {
    let mut out = Vec::new();
    if let Some((p, _)) = pos_tan_on(c, lens, start) {
        out.push(p);
    }
    for i in 0..c.len() {
        if lens[i] > start && lens[i] < stop {
            out.push(c[i]);
        }
    }
    if let Some((p, _)) = pos_tan_on(c, lens, stop) {
        out.push(p);
    }
    out
}

#[inline]
fn dist(a: PointF, b: PointF) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    (dx * dx + dy * dy).sqrt()
}

/// Android `Path.op` enum values.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PathOp {
    Difference,
    Intersect,
    Union,
    Xor,
    ReverseDifference,
}

impl PathOp {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => PathOp::Difference,
            1 => PathOp::Intersect,
            2 => PathOp::Union,
            3 => PathOp::Xor,
            4 => PathOp::ReverseDifference,
            _ => return None,
        })
    }
}

impl Path {
    /// Boolean path ops. tiny-skia does not implement them; this always
    /// reports `RENDER_UNSUPPORTED_OP` naming the op so the caller can mark
    /// reduced fidelity rather than silently dropping geometry.
    pub fn op(&mut self, _other: &Path, op: PathOp) -> Result<(), RenderError> {
        Err(RenderError::unsupported(format!("Path.op {op:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_bounds_and_is_rect() {
        let mut p = Path::new();
        p.add_rect(1.0, 2.0, 11.0, 22.0, false);
        assert!(p.is_rect());
        let b = p.compute_bounds().unwrap();
        assert_eq!(b, RectF::new(1.0, 2.0, 11.0, 22.0));
    }

    #[test]
    fn transform_moves_points() {
        let mut p = Path::new();
        p.move_to(0.0, 0.0);
        p.line_to(10.0, 0.0);
        let mut m = Matrix3x3::identity();
        m.set_translate(5.0, 7.0);
        p.transform(&m);
        assert_eq!(p.compute_bounds().unwrap(), RectF::new(5.0, 7.0, 15.0, 7.0));
    }

    #[test]
    fn arc_to_quarter_circle_hits_endpoint() {
        let mut p = Path::new();
        // 0° is (10,5); 90° (clockwise, y down) is (5,10).
        p.arc_to(RectF::new(0.0, 0.0, 10.0, 10.0), 0.0, 90.0, true);
        let b = p.compute_bounds().unwrap();
        assert!((b.left - 5.0).abs() < 0.1, "{b:?}");
        assert!((b.right - 10.0).abs() < 0.1, "{b:?}");
        assert!((b.top - 5.0).abs() < 0.1, "{b:?}");
        assert!((b.bottom - 10.0).abs() < 0.1, "{b:?}");
    }

    #[test]
    fn path_measure_length_of_square() {
        let mut p = Path::new();
        p.add_rect(0.0, 0.0, 10.0, 10.0, false);
        let pm = PathMeasure::new(&p, false);
        assert!((pm.total_length() - 40.0).abs() < 0.5, "{}", pm.total_length());
        let (pos, tan) = pm.get_pos_tan(5.0).unwrap();
        assert!((pos.x - 5.0).abs() < 0.1);
        assert!((tan.x - 1.0).abs() < 0.01);
    }

    #[test]
    fn conic_weight_one_matches_quad() {
        let mut a = Path::new();
        a.move_to(0.0, 0.0);
        a.conic_to(5.0, 10.0, 10.0, 0.0, 1.0);
        let mut b = Path::new();
        b.move_to(0.0, 0.0);
        b.quad_to(5.0, 10.0, 10.0, 0.0);
        assert_eq!(a.verbs(), b.verbs());
    }

    #[test]
    fn path_op_is_explicitly_unsupported() {
        let mut a = Path::from_rect(RectF::new(0.0, 0.0, 10.0, 10.0));
        let b = Path::from_rect(RectF::new(5.0, 5.0, 15.0, 15.0));
        let err = a.op(&b, PathOp::Union).unwrap_err();
        assert_eq!(err.code(), "RENDER_UNSUPPORTED_OP");
    }
}
