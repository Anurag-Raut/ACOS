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

/// A parsed `Res_png_9patch` chunk (the `npTc` chunk passed to
/// `NinePatch.validateNinePatchChunk` / `Canvas.nDrawNinePatch`).
///
/// aapt/aapt2 serialise the chunk as a 32-byte header followed by the div and
/// colour arrays. The header's byte offsets are native-endian; the arrays are
/// written in network (big-endian) order. ACOS consumes the divs to place the
/// nine stretch regions; the colours are retained for completeness
/// (`getTransparentRegion`).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct NinePatchChunk {
    pub x_divs: Vec<i32>,
    pub y_divs: Vec<i32>,
    pub colors: Vec<u32>,
}

impl NinePatchChunk {
    /// Parse the serialized `npTc` bytes. Returns `None` for a truncated or
    /// inconsistent chunk (never a silently wrong stretch).
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 32 {
            return None;
        }
        let nx = bytes[1] as usize;
        let ny = bytes[2] as usize;
        let nc = bytes[3] as usize;
        let u32_le = |o: usize| {
            u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]) as usize
        };
        let (xo, yo, co) = (u32_le(4), u32_le(8), u32_le(28));
        let fits = |o: usize, n: usize| o.checked_add(n * 4).map(|e| e <= bytes.len()).unwrap_or(false);
        if !fits(xo, nx) || !fits(yo, ny) || !fits(co, nc) {
            return None;
        }
        let read_i32 = |o: usize, n: usize| -> Vec<i32> {
            (0..n)
                .map(|i| {
                    let p = o + i * 4;
                    i32::from_be_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]])
                })
                .collect()
        };
        let read_u32 = |o: usize, n: usize| -> Vec<u32> {
            (0..n)
                .map(|i| {
                    let p = o + i * 4;
                    u32::from_be_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]])
                })
                .collect()
        };
        Some(NinePatchChunk {
            x_divs: read_i32(xo, nx),
            y_divs: read_i32(yo, ny),
            colors: read_u32(co, nc),
        })
    }

    /// Serialise to the same `npTc` layout (used by tests and fixtures).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![0u8; 32];
        out[1] = self.x_divs.len() as u8;
        out[2] = self.y_divs.len() as u8;
        out[3] = self.colors.len() as u8;
        let xo = 32usize;
        let yo = xo + self.x_divs.len() * 4;
        let co = yo + self.y_divs.len() * 4;
        out[4..8].copy_from_slice(&(xo as u32).to_le_bytes());
        out[8..12].copy_from_slice(&(yo as u32).to_le_bytes());
        out[28..32].copy_from_slice(&(co as u32).to_le_bytes());
        for d in &self.x_divs {
            out.extend_from_slice(&d.to_be_bytes());
        }
        for d in &self.y_divs {
            out.extend_from_slice(&d.to_be_bytes());
        }
        for c in &self.colors {
            out.extend_from_slice(&c.to_be_bytes());
        }
        out
    }

    /// A chunk with no divs cannot describe stretch regions.
    pub fn is_empty(&self) -> bool {
        self.x_divs.is_empty() && self.y_divs.is_empty()
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
