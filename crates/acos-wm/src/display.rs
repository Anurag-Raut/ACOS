//! The real reference display geometry, read out of the ART emulator capture
//! (`fixtures/reference/hello-views/art-api34-x86_64/{display.txt,device-config.json}`).
//!
//! Every number here is MEASURED, not chosen:
//!
//! | value | source line in `display.txt` |
//! |---|---|
//! | `init=1080x2400 420dpi` | `Display: mDisplayId=0` |
//! | app bounds `[0,128][1080,2337]` | `mAppBounds=Rect(0, 128 - 1080, 2337)` |
//! | status bar `[0,0][1080,128]` | `InsetsSource ... type=statusBars frame=[0,0][1080,128]` |
//! | nav bar `[0,2337][1080,2400]` | `type=navigationBars frame=[0,2337][1080,2400]` |
//! | IME `[0,2274][1080,2400]` | `type=ime ... visibleFrame=[0,2274][1080,2400]` |
//! | cutout `[0,0][1080,128]` | `type=displayCutout frame=[0,0][1080,128]` |
//! | app window frame `[0,128][1080,2337]` | `nonDecorFrame=[0,128][1080,2337]` |

use crate::geom::{InsetsI, PointI, RectI};

/// Logical display width in pixels.
pub const DISPLAY_WIDTH: i32 = 1080;
/// Logical display height in pixels.
pub const DISPLAY_HEIGHT: i32 = 2400;
/// `densityDpi=420` (device-config.json).
pub const DENSITY_DPI: u32 = 420;
/// `density=2.625` (`420/160`), as reported by `DisplayMetrics`.
pub const DENSITY: f32 = 2.625;
/// `refreshRate=60.000004` (the emulator's reported mode).
pub const REFRESH_HZ: f32 = 60.000_004;
/// One frame at 60 Hz: `1e9 / 60` rounded.
pub const FRAME_INTERVAL_NANOS: i64 = 16_666_667;
/// Status-bar top inset (`[0,0][1080,128]`).
pub const STATUS_BAR_TOP: i32 = 128;
/// Navigation-bar bottom inset (`[0,2337][1080,2400]`).
pub const NAV_BAR_BOTTOM: i32 = 63;
/// IME height when visible (`2400 - 2274`).
pub const IME_VISIBLE_HEIGHT: i32 = 126;

/// Bounds of every system bar / gesture inset source on the reference display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemBars {
    /// `type=statusBars frame=[0,0][1080,128]`, source id `0x9b1a0000`.
    pub status_bar: RectI,
    /// `type=tappableElement frame=[0,0][1080,128]`, source id `0x9b1a0006`.
    pub status_bar_tappable: RectI,
    /// `type=navigationBars frame=[0,2337][1080,2400]`, source id `0xfc050001`.
    pub nav_bar: RectI,
    /// `type=tappableElement frame=[0,0][0,0]`, source id `0xfc050006`.
    pub nav_bar_tappable: RectI,
    /// `type=ime visibleFrame=[0,2274][1080,2400]`, source id `3` (hidden).
    pub ime_visible: RectI,
    /// `type=displayCutout frame=[0,0][1080,128]`, source id `27`.
    pub display_cutout: RectI,
    /// `type=mandatorySystemGestures frame=[0,0][1080,160]`, source id `0x9b1a0005`.
    pub mandatory_system_gestures_top: RectI,
    /// `type=mandatorySystemGestures frame=[0,2316][1080,2400]`, id `0xfc050005`.
    pub mandatory_system_gestures_bottom: RectI,
    /// `type=systemGestures frame=[0,0][78,2400]`, source id `0xfc050004`.
    pub system_gestures_left: RectI,
    /// `type=systemGestures frame=[1002,0][1080,2400]`, source id `0xfc050024`.
    pub system_gestures_right: RectI,
}

impl SystemBars {
    /// The exact reference bars.
    #[must_use]
    pub const fn reference() -> Self {
        Self {
            status_bar: RectI::new(0, 0, DISPLAY_WIDTH, STATUS_BAR_TOP),
            status_bar_tappable: RectI::new(0, 0, DISPLAY_WIDTH, STATUS_BAR_TOP),
            nav_bar: RectI::new(0, DISPLAY_HEIGHT - NAV_BAR_BOTTOM, DISPLAY_WIDTH, DISPLAY_HEIGHT),
            nav_bar_tappable: RectI::new(0, 0, 0, 0),
            ime_visible: RectI::new(
                0,
                DISPLAY_HEIGHT - IME_VISIBLE_HEIGHT,
                DISPLAY_WIDTH,
                DISPLAY_HEIGHT,
            ),
            display_cutout: RectI::new(0, 0, DISPLAY_WIDTH, STATUS_BAR_TOP),
            mandatory_system_gestures_top: RectI::new(0, 0, DISPLAY_WIDTH, 160),
            mandatory_system_gestures_bottom: RectI::new(0, 2316, DISPLAY_WIDTH, DISPLAY_HEIGHT),
            system_gestures_left: RectI::new(0, 0, 78, DISPLAY_HEIGHT),
            system_gestures_right: RectI::new(1002, 0, DISPLAY_WIDTH, DISPLAY_HEIGHT),
        }
    }

    /// The bars visible to an application window (status + nav), as an
    /// `InsetsI` relative to the display frame.
    #[must_use]
    pub const fn app_insets(&self) -> InsetsI {
        InsetsI::new(
            0,
            self.status_bar.height(),
            0,
            self.nav_bar.height(),
        )
    }
}

/// One logical display plus its system bars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplaySpec {
    /// `mDisplayId=0`.
    pub id: i32,
    /// Full display frame `[0,0][1080,2400]`.
    pub frame: RectI,
    /// `mAppBounds=Rect(0, 128 - 1080, 2337)`: the window frame a normal app gets.
    pub app_bounds: RectI,
    /// `densityDpi`.
    pub density_dpi: u32,
    /// `density`.
    pub density: f32,
    /// `refreshRate`.
    pub refresh_hz: f32,
    /// One vsync interval in nanoseconds.
    pub frame_interval_nanos: i64,
    /// Bar/gesture geometry.
    pub bars: SystemBars,
    /// Current rotation (0..3).
    pub rotation: u8,
}

impl Default for DisplaySpec {
    fn default() -> Self {
        Self::reference_1080x2400()
    }
}

impl DisplaySpec {
    /// The phone this whole project is calibrated against.
    #[must_use]
    pub const fn reference_1080x2400() -> Self {
        Self {
            id: 0,
            frame: RectI::new(0, 0, DISPLAY_WIDTH, DISPLAY_HEIGHT),
            app_bounds: RectI::new(0, STATUS_BAR_TOP, DISPLAY_WIDTH, DISPLAY_HEIGHT - NAV_BAR_BOTTOM),
            density_dpi: DENSITY_DPI,
            density: DENSITY,
            refresh_hz: REFRESH_HZ,
            frame_interval_nanos: FRAME_INTERVAL_NANOS,
            bars: SystemBars::reference(),
            rotation: 0,
        }
    }

    /// `IWindowManager.hasNavigationBar` — the reference has `NavigationBar0`.
    #[must_use]
    pub const fn has_navigation_bar(&self) -> bool {
        !self.bars.nav_bar.is_empty()
    }

    /// The insets the window manager subtracts from the full frame for an app.
    #[must_use]
    pub const fn app_insets(&self) -> InsetsI {
        self.bars.app_insets()
    }

    /// `addToDisplayAsUser`'s `outFrame` for a normal app window: the app bounds.
    #[must_use]
    pub const fn window_frame(&self) -> RectI {
        self.app_bounds
    }

    /// `InsetsSourceControl` surface positions from the dumpsys `Control map`:
    /// `ime → (0,128)`, `statusBars → (0,0)`, `navigationBars → (0,2274)`.
    #[must_use]
    pub const fn source_control_positions(&self) -> [(&'static str, i32, PointI, InsetsI); 3] {
        [
            ("ime", 3, PointI::new(0, STATUS_BAR_TOP), InsetsI::new(0, 0, 0, 883)),
            ("statusBars", 0x9b1a_0000u32 as i32, PointI::new(0, 0), InsetsI::new(0, STATUS_BAR_TOP, 0, 0)),
            (
                "navigationBars",
                0xfc05_0001u32 as i32,
                PointI::new(0, 2274),
                InsetsI::new(0, 0, 0, NAV_BAR_BOTTOM),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_numbers_are_the_measured_ones() {
        let d = DisplaySpec::reference_1080x2400();
        assert_eq!(d.frame, RectI::new(0, 0, 1080, 2400));
        assert_eq!(d.app_bounds, RectI::new(0, 128, 1080, 2337));
        assert_eq!(d.app_insets(), InsetsI::new(0, 128, 0, 63));
        assert_eq!(d.bars.ime_visible.height(), 126);
        assert_eq!(d.bars.ime_visible.top, 2274);
    }

    #[test]
    fn control_positions_match_dumpsys() {
        let d = DisplaySpec::reference_1080x2400();
        let c = d.source_control_positions();
        assert_eq!(c[0].2, PointI::new(0, 128));
        assert_eq!(c[2].2, PointI::new(0, 2274));
        assert_eq!(c[2].3.bottom, 63);
    }
}
