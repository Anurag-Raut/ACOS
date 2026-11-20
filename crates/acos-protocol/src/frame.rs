//! Frame metadata: the evidence identity that makes stale screenshots
//! impossible to confuse with fresh ones.
//!
//! # Coordinate contract (W58)
//!
//! There are exactly two pixel spaces in ACOS and they are named everywhere:
//!
//! * **display space** — the Android logical display (`[0,0][1080,2400]` on the
//!   API-34 reference). UI-tree bounds, tap `x`/`y`, `assert_region` and
//!   `assert_screenshot.region_masks` are all in this space. It is what
//!   `View.getLocationOnScreen`, `uiautomator dump` and `adb shell input tap`
//!   use, so an agent can never be off by the status-bar inset on this side.
//! * **frame space** — the pixels of one captured frame (a PNG), `width` x
//!   `height`, with its display-space origin in [`FrameMeta::display_origin`].
//!   The ART runtime composites the display's *application area* only
//!   (`[0,128][1080,2337]`), so the PNG is not the full display and
//!   `png_y = display_y - display_origin.y`.
//!
//! Every frame that does not cover the whole display must publish
//! `display_origin` + `display_size`; a frame that covers the whole display
//! omits `display_origin` (identity mapping, the AOSP `screencap` case). Use
//! [`FrameMeta::frame_rect_for_display`] to cross the boundary — never subtract
//! an offset by hand.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ui::Bounds;

/// Metadata for one produced frame.
///
/// A frame answers a *cause* (the request/action id that produced it) and has a
/// monotonically increasing per-session `frame_seq`. `content_hash` is the
/// SHA-256 of the encoded pixels. `meaningful` is false for blank/placeholder
/// frames; a placeholder must never satisfy a UI assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FrameMeta {
    /// Monotonic per-session frame sequence number.
    pub frame_seq: u64,
    /// RFC 3339 UTC timestamp when the frame was produced by ACOS.
    pub produced_at: String,
    /// The request id that caused this frame (for example `req_19`).
    pub cause_id: String,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Display density in dots per inch.
    pub density: u32,
    /// SHA-256 of the encoded frame pixels (lowercase hex).
    pub content_hash: String,
    /// True only when the frame reflects real rendered UI content.
    pub meaningful: bool,
    /// Display-space coordinates of this frame's pixel `(0,0)`.
    ///
    /// Absent means the frame covers the whole display and `(0,0)` is display
    /// `(0,0)` (the AOSP `screencap` case). A present value is authoritative: an
    /// agent must add it to convert a frame pixel into display coordinates, and
    /// subtract it for the reverse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_origin: Option<[i32; 2]>,
    /// Full display size in pixels — the space `display_origin` and every UI
    /// tree bound are expressed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_size: Option<[u32; 2]>,
}

impl FrameMeta {
    /// Build frame metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        frame_seq: u64,
        produced_at: impl Into<String>,
        cause_id: impl Into<String>,
        width: u32,
        height: u32,
        density: u32,
        content_hash: impl Into<String>,
        meaningful: bool,
    ) -> Self {
        FrameMeta {
            frame_seq,
            produced_at: produced_at.into(),
            cause_id: cause_id.into(),
            width,
            height,
            density,
            content_hash: content_hash.into(),
            meaningful,
            display_origin: None,
            display_size: None,
        }
    }

    /// Declare where this frame sits on the display.
    ///
    /// `origin` is the display coordinate of the frame's pixel `(0,0)`;
    /// `size` is the full display size. Backends that capture the display's
    /// application area publish `[0, status_bar_height]` here, which is the
    /// difference between a tree bound and a PNG pixel.
    #[must_use]
    pub fn with_display_geometry(mut self, origin: [i32; 2], size: [u32; 2]) -> Self {
        self.display_origin = Some(origin);
        self.display_size = Some(size);
        self
    }

    /// The display-space origin of frame pixel `(0,0)` (`[0,0]` when absent).
    pub fn display_origin_px(&self) -> [i32; 2] {
        self.display_origin.unwrap_or([0, 0])
    }

    /// The display-space rectangle this frame covers (`[left, top, right, bottom]`).
    pub fn display_rect(&self) -> Bounds {
        let [ox, oy] = self.display_origin_px();
        Bounds::new(ox, oy, ox + self.width as i32, oy + self.height as i32)
    }

    /// Convert a **display-space** rectangle to this frame's pixel rectangle.
    ///
    /// Fails when any part of the rectangle is outside the frame (for example a
    /// region in the status bar, which the ART frame does not contain); the
    /// message names both spaces so the caller can report it instead of silently
    /// cropping a shifted region.
    pub fn frame_rect_for_display(&self, display: Bounds) -> Result<Bounds, String> {
        let covered = self.display_rect();
        if display.left() < covered.left()
            || display.top() < covered.top()
            || display.right() > covered.right()
            || display.bottom() > covered.bottom()
        {
            return Err(format!(
                "display region {display} is not inside the frame: this frame is {}x{} 
                 pixels at display origin [{},{}], i.e. display {covered}; the display area 
                 outside that rectangle (status bar / navigation bar) has no pixels in this 
                 frame",
                self.width,
                self.height,
                covered.left(),
                covered.top()
            ));
        }
        let [ox, oy] = self.display_origin_px();
        Ok(Bounds::new(
            display.left() - ox,
            display.top() - oy,
            display.right() - ox,
            display.bottom() - oy,
        ))
    }

    /// True when this frame was produced strictly after `after_seq`.
    ///
    /// A capture request with `after_seq = N` may only accept a frame whose
    /// `frame_seq > N`. This is the stale-frame rule.
    pub const fn is_fresh_after(&self, after_seq: u64) -> bool {
        self.frame_seq > after_seq
    }

    /// A stable, agent-readable label (`frame 19 at req_19`).
    pub fn label(&self) -> String {
        format!("frame {} at {}", self.frame_seq, self.cause_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_is_strict() {
        let f = FrameMeta::new(
            19,
            "2026-11-10T13:51:14Z",
            "req_19",
            1080,
            2400,
            420,
            "abc",
            true,
        );
        assert!(f.is_fresh_after(18));
        assert!(!f.is_fresh_after(19));
        assert!(!f.is_fresh_after(20));
        assert_eq!(f.label(), "frame 19 at req_19");
    }

    /// The reference display: 1080x2400, the ART frame is the app area
    /// `[0,128][1080,2337]` = 1080x2209 pixels.
    fn art_frame() -> FrameMeta {
        FrameMeta::new(
            34,
            "2026-11-12T05:55:27Z",
            "art",
            1080,
            2209,
            420,
            "h",
            true,
        )
        .with_display_geometry([0, 128], [1080, 2400])
    }

    #[test]
    fn a_whole_display_frame_needs_no_origin() {
        let full = FrameMeta::new(1, "t", "c", 1080, 2400, 420, "h", true);
        assert_eq!(full.display_origin_px(), [0, 0]);
        assert_eq!(full.display_rect(), Bounds::new(0, 0, 1080, 2400));
        assert_eq!(
            full.frame_rect_for_display(Bounds::new(10, 20, 30, 40))
                .unwrap(),
            Bounds::new(10, 20, 30, 40)
        );
    }

    /// A tree bound and its PNG crop are the same physical rectangle. This is
    /// the round-trip an agent (and `assert_region`) relies on.
    #[test]
    fn tree_bound_to_frame_crop_round_trip() {
        let frame = art_frame();
        // `android:id/parentPanel` as the live tree reports it (display space).
        let parent_panel = Bounds::new(70, 1018, 1010, 1447);
        let crop = frame.frame_rect_for_display(parent_panel).unwrap();
        assert_eq!(crop, Bounds::new(70, 890, 1010, 1319));
        assert_eq!(crop.width(), parent_panel.width());
        assert_eq!(crop.height(), parent_panel.height());
        // And back: the frame rectangle maps onto the tree bound it came from.
        let [ox, oy] = frame.display_origin_px();
        assert_eq!(
            Bounds::new(
                crop.left() + ox,
                crop.top() + oy,
                crop.right() + ox,
                crop.bottom() + oy
            ),
            parent_panel
        );
    }

    #[test]
    fn the_status_bar_has_no_pixels_in_an_app_area_frame() {
        let frame = art_frame();
        let error = frame
            .frame_rect_for_display(Bounds::new(0, 0, 1080, 128))
            .unwrap_err();
        assert!(error.contains("display region"), "{error}");
        assert!(error.contains("display origin [0,128]"), "{error}");
        // A region half in the status bar is refused, never clipped silently.
        assert!(frame
            .frame_rect_for_display(Bounds::new(0, 100, 10, 200))
            .is_err());
        // The bottom navigation band is equally outside the frame.
        assert!(frame
            .frame_rect_for_display(Bounds::new(0, 2300, 10, 2350))
            .is_err());
        // A region exactly at the frame's top row is inside.
        assert_eq!(
            frame
                .frame_rect_for_display(Bounds::new(0, 128, 10, 200))
                .unwrap(),
            Bounds::new(0, 0, 10, 72)
        );
    }

    #[test]
    fn display_geometry_serde_is_additive() {
        // An artifact written before W58 (no fields) still deserializes, and maps
        // identity, so replayed evidence is never reinterpreted.
        let old: FrameMeta = serde_json::from_str(
            r#"{"frame_seq":3,"produced_at":"t","cause_id":"c","width":1080,"height":2400,"density":420,"content_hash":"h","meaningful":true}"#,
        )
        .unwrap();
        assert_eq!(old.display_origin, None);
        assert_eq!(old.display_origin_px(), [0, 0]);
        // Geometry that is the identity is omitted from new artifacts.
        let full = FrameMeta::new(1, "t", "c", 1080, 2400, 420, "h", true);
        let json = serde_json::to_string(&full).unwrap();
        assert!(!json.contains("display_origin"), "{json}");
        let app = art_frame();
        let json = serde_json::to_string(&app).unwrap();
        assert!(json.contains(r#""display_origin":[0,128]"#), "{json}");
        assert!(json.contains(r#""display_size":[1080,2400]"#), "{json}");
    }
}
