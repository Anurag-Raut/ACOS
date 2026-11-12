//! Display geometry for frame evidence (W58).
//!
//! A frame is not the whole display: the ART runtime composites the display's
//! application area, so the PNG starts at display `y = status_bar_top`. Every
//! frame therefore publishes `display_origin` + `display_size`
//! ([`acos_protocol::FrameMeta`]) so that a tree bound, a tap point and a PNG
//! pixel can be converted without guessing.
//!
//! The origin comes from the runtime's own display model, never from a constant
//! in the verifier:
//!
//! * `ACOS_DISPLAY_OVERRIDE` / `set_display_override` (what `acos launch
//!   --display/--density/--reference` and the `acos serve` daemon install) —
//!   width, height and the status-bar inset are explicit;
//! * otherwise the runtime's reference window frames
//!   (`acos_runtime::wms_java::reference_frames`), which are the measured API-34
//!   display `[0,0][1080,2400]` and app window `[0,128][1080,2337]`.
//!
//! Both are cross-checked against the frame the backend actually produced: the
//! geometry is only published when the frame size equals the application area,
//! which is exactly the rectangle C1 composites. A frame of another size keeps
//! `display_origin: None` (identity) rather than a plausible-looking wrong
//! offset.

use acos_protocol::FrameMeta;

/// Where a frame sits on the display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDisplayGeometry {
    /// Display coordinate of the frame's pixel `(0,0)`.
    pub origin: [i32; 2],
    /// Full display size in pixels.
    pub display_size: [u32; 2],
    /// The application area the runtime composites.
    pub app_size: [u32; 2],
    /// Where the numbers came from (`display-override` / `wms-reference`).
    pub source: &'static str,
}

impl FrameDisplayGeometry {
    /// Attach this geometry to a frame's metadata.
    pub fn apply(self, frame: FrameMeta) -> FrameMeta {
        frame.with_display_geometry(self.origin, self.display_size)
    }
}

/// The display geometry for a frame of `frame_width` x `frame_height` pixels.
///
/// `None` when the frame is not the display's application area: the verifier
/// then publishes no origin, which means "this frame is the whole display"
/// (identity). Guessing an origin for an unexpected frame size is exactly the
/// 128-px error this module exists to prevent.
#[must_use]
pub fn frame_display_geometry(
    frame_width: u32,
    frame_height: u32,
) -> Option<FrameDisplayGeometry> {
    let geometry = match acos_runtime::display_override() {
        Some(display) => {
            let width = display.width.max(0) as u32;
            let height = display.height.max(0) as u32;
            let status_bar = display.status_bar_top.max(0);
            let nav_bar = display.nav_bar_bottom.max(0) as u32;
            FrameDisplayGeometry {
                origin: [0, status_bar],
                display_size: [width, height],
                app_size: [
                    width,
                    height.saturating_sub(status_bar as u32 + nav_bar),
                ],
                source: "display-override",
            }
        }
        None => {
            let frames = acos_runtime::wms_java::reference_frames();
            let app = frames.frame;
            let display = frames.display_frame;
            FrameDisplayGeometry {
                origin: [app.left, app.top],
                display_size: [display.width().max(0) as u32, display.height().max(0) as u32],
                app_size: [app.width().max(0) as u32, app.height().max(0) as u32],
                source: "wms-reference",
            }
        }
    };
    if geometry.app_size != [frame_width, frame_height] {
        return None;
    }
    Some(geometry)
}

/// Sign a frame with this host's display geometry when the backend did not.
///
/// The verifier owns the frame evidence it writes, so it completes the metadata
/// at its own boundary: a backend that reports an app-area frame without saying
/// where it sits on the display still gets `display_origin` in `tree/<seq>.json`
/// and `verdict.json`. A backend that already publishes geometry keeps it, and a
/// frame that is not the application area keeps `None` (identity) — the size
/// check in [`frame_display_geometry`] is what prevents a guessed offset.
#[must_use]
pub fn signed_frame(frame: &FrameMeta) -> FrameMeta {
    if frame.display_origin.is_some() {
        return frame.clone();
    }
    match frame_display_geometry(frame.width, frame.height) {
        Some(geometry) => geometry.apply(frame.clone()),
        None => frame.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_display_puts_an_app_area_frame_at_the_status_bar() {
        // The ART frame is 1080x2209; the display is 1080x2400 with a 128 px
        // status bar and a 63 px navigation bar.
        let geometry = frame_display_geometry(1080, 2209).expect("app-area frame");
        assert_eq!(geometry.origin, [0, 128]);
        assert_eq!(geometry.display_size, [1080, 2400]);
        let frame = FrameMeta::new(1, "t", "c", 1080, 2209, 420, "h", true);
        let frame = geometry.apply(frame);
        assert_eq!(frame.display_origin, Some([0, 128]));
        assert_eq!(frame.display_rect().top(), 128);
        assert_eq!(frame.display_rect().bottom(), 2337);
    }

    #[test]
    fn an_unsigned_app_area_frame_is_signed_and_a_signed_one_is_kept() {
        let unsigned = FrameMeta::new(1, "t", "c", 1080, 2209, 420, "h", true);
        let signed = signed_frame(&unsigned);
        assert_eq!(signed.display_origin, Some([0, 128]));
        // Already-signed geometry is authoritative, not overwritten.
        let declared = FrameMeta::new(2, "t", "c", 1080, 2209, 420, "h", true)
            .with_display_geometry([0, 200], [1080, 2600]);
        assert_eq!(signed_frame(&declared).display_origin, Some([0, 200]));
        // A display-sized frame stays identity-signed (None).
        let full = FrameMeta::new(3, "t", "c", 1080, 2400, 420, "h", true);
        assert_eq!(signed_frame(&full).display_origin, None);
    }

    #[test]
    fn a_frame_of_an_unexpected_size_gets_no_origin() {
        // A backend that synthesizes a display-sized frame (the reference
        // replay does: 1080x2400) must not inherit the app-area origin.
        assert!(frame_display_geometry(1080, 2400).is_none());
        assert!(frame_display_geometry(1080, 700).is_none());
    }
}
