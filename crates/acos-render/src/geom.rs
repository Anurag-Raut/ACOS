//! Floating-point and integer geometry used across the renderer.

use crate::path::Path;

/// A floating-point point, mirroring `android.graphics.PointF`.
#[derive(Copy, Clone, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PointF {
    pub x: f32,
    pub y: f32,
}

impl PointF {
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        PointF { x, y }
    }
}

/// A floating-point rectangle, mirroring `android.graphics.RectF`.
#[derive(Copy, Clone, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl RectF {
    #[inline]
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        RectF {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn from_xywh(x: f32, y: f32, w: f32, h: f32) -> Self {
        RectF::new(x, y, x + w, y + h)
    }

    #[inline]
    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    #[inline]
    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }

    #[inline]
    pub fn center_x(&self) -> f32 {
        (self.left + self.right) * 0.5
    }

    #[inline]
    pub fn center_y(&self) -> f32 {
        (self.top + self.bottom) * 0.5
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    pub fn set_empty(&mut self) {
        *self = RectF::new(0.0, 0.0, 0.0, 0.0);
    }

    pub fn union(&self, other: &RectF) -> RectF {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        RectF::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }

    pub fn intersect(&self, other: &RectF) -> Option<RectF> {
        let r = RectF::new(
            self.left.max(other.left),
            self.top.max(other.top),
            self.right.min(other.right),
            self.bottom.min(other.bottom),
        );
        if r.is_empty() {
            None
        } else {
            Some(r)
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn round_out(self) -> Rect {
        Rect::new(
            self.left.floor() as i32,
            self.top.floor() as i32,
            self.right.ceil() as i32,
            self.bottom.ceil() as i32,
        )
    }
}

/// An integer rectangle, mirroring `android.graphics.Rect`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Hash, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    #[inline]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[inline]
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    #[inline]
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    pub fn to_rectf(self) -> RectF {
        RectF::new(
            self.left as f32,
            self.top as f32,
            self.right as f32,
            self.bottom as f32,
        )
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        Rect::new(
            self.left.max(other.left),
            self.top.max(other.top),
            self.right.min(other.right),
            self.bottom.min(other.bottom),
        )
    }

    /// Split into non-overlapping rows (used by region rasterisation).
    pub fn to_tiny_rect(self) -> Option<tiny_skia::IntRect> {
        tiny_skia::IntRect::from_ltrb(self.left, self.top, self.right, self.bottom)
    }
}

/// Android `android.graphics.Insets`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Insets {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Android `android.graphics.Outline` / `RenderNode` outline. Only the shapes
/// Android's `ViewOutlineProvider` commonly sets are modelled.
#[derive(Clone, PartialEq, Debug)]
pub enum Outline {
    /// No outline (skip clipping/shadow).
    None,
    /// An empty (zero-area) outline.
    Empty,
    Rect(RectF),
    RoundRect {
        rect: RectF,
        radius_x: f32,
        radius_y: f32,
    },
    Path {
        path: Path,
        alpha: f32,
    },
}

impl Outline {
    pub fn is_empty(&self) -> bool {
        match self {
            Outline::None | Outline::Empty => true,
            Outline::Rect(r) => r.is_empty(),
            Outline::RoundRect { rect, .. } => rect.is_empty(),
            Outline::Path { path, .. } => path.is_empty(),
        }
    }

    pub fn bounds(&self) -> Option<RectF> {
        match self {
            Outline::Rect(r) => Some(*r),
            Outline::RoundRect { rect, .. } => Some(*rect),
            Outline::Path { path, .. } => path.compute_bounds(),
            _ => None,
        }
    }

    pub fn as_path(&self) -> Option<Path> {
        match self {
            Outline::Rect(r) => Some(Path::from_rect(*r)),
            Outline::RoundRect { rect, radius_x, radius_y } => {
                Some(Path::from_round_rect(*rect, *radius_x, *radius_y))
            }
            Outline::Path { path, .. } => Some(path.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_out_covers_fractional_rect() {
        let r = RectF::new(0.2, 0.7, 10.1, 20.9);
        assert_eq!(r.round_out(), Rect::new(0, 0, 11, 21));
    }

    #[test]
    fn intersect_empty() {
        let a = RectF::new(0.0, 0.0, 10.0, 10.0);
        let b = RectF::new(20.0, 20.0, 30.0, 30.0);
        assert!(a.intersect(&b).is_none());
    }
}
