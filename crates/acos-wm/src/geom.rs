//! Small integer geometry types used across the WMS state machines.
//!
//! These mirror `android.graphics.Rect`/`Insets`/`Point` closely enough that the
//! Parcel codecs are a direct transcription of the framework's own
//! `writeToParcel` (see [`crate::codec`]).

use acos_protocol::ui::Bounds;

/// `android.graphics.Rect`: half-open `[left,right) x [top,bottom)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct RectI {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectI {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// The canonical display frame of the reference device, `[0,0][1080,2400]`.
    pub const DISPLAY_1080X2400: RectI = RectI::new(0, 0, 1080, 2400);

    pub const fn width(self) -> i32 {
        self.right - self.left
    }

    pub const fn height(self) -> i32 {
        self.bottom - self.top
    }

    pub const fn is_empty(self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    /// Half-open containment (`Rect.contains`).
    pub const fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// `Rect.intersect`, returning an empty rect when disjoint.
    #[must_use]
    pub fn intersect(self, other: RectI) -> RectI {
        let l = self.left.max(other.left);
        let t = self.top.max(other.top);
        let r = self.right.min(other.right);
        let b = self.bottom.min(other.bottom);
        if l < r && t < b {
            RectI::new(l, t, r, b)
        } else {
            RectI::new(0, 0, 0, 0)
        }
    }

    /// `Rect.union`.
    #[must_use]
    pub fn union(self, other: RectI) -> RectI {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        RectI::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }

    /// Translate by `(dx,dy)` in place.
    pub fn offset(&mut self, dx: i32, dy: i32) {
        self.left += dx;
        self.top += dy;
        self.right += dx;
        self.bottom += dy;
    }

    /// The protocol `Bounds` form (`"[l,t][r,b]"`).
    #[must_use]
    pub fn to_bounds(self) -> Bounds {
        Bounds::new(self.left, self.top, self.right, self.bottom)
    }

    /// Rotate this rect by one quarter turn into the display frame it belongs
    /// to after the rotation.
    ///
    /// `src_w`/`src_h` are the dimensions of the display frame **before** the
    /// turn; the returned rect lives in the `src_h x src_w` frame. The point
    /// map is the one `SurfaceFlinger`/`DisplayFrames` use for a one-step
    /// rotation: `(x, y) -> (src_h - y, x)`, applied to both corners and
    /// normalised afterwards (so a rect keeps its extent, not its sign).
    ///
    /// A rect is rotated as a *region*: the raster does not move, the frame
    /// it is expressed in does. `[0,0][1080,128]` (a portrait status bar)
    /// therefore becomes `[2400-128,0][2400,1080]` on a 1080x2400 display.
    #[must_use]
    pub fn rotate_quarters(self, src_w: i32, src_h: i32) -> RectI {
        debug_assert!(
            src_w > 0 && src_h > 0,
            "a rotated rect needs the source frame's size"
        );
        let (x0, y0) = (src_h - self.top, self.left);
        let (x1, y1) = (src_h - self.bottom, self.right);
        RectI::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
    }
}

/// `android.graphics.Insets`: per-edge thicknesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct InsetsI {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl InsetsI {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.left == 0 && self.top == 0 && self.right == 0 && self.bottom == 0
    }
}

/// `android.graphics.Point`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct PointI {
    pub x: i32,
    pub y: i32,
}

impl PointI {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}
