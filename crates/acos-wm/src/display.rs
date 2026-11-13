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

/// Environment selector for the app-window inset model (W7).
///
/// * [`InsetsMode::Carve`] is the pre-W7 shim: the app window frame is the
///   display's application area (`display.app_bounds`), and a real
///   `InsetsState` calculated against that frame reports zero system-bar
///   insets because the bars sit outside the frame.
/// * [`InsetsMode::Report`] follows AOSP's edge-to-edge window model: the app
///   window frame is the full display and the `InsetsState` source frames are
///   reported to the client, which then applies them itself (DecorView margins
///   for a non-edge-to-edge app, app padding for an edge-to-edge one).
pub const INSETS_ENV: &str = "ACOS_WM_INSETS";

/// The default model when `ACOS_WM_INSETS` is unset.
///
/// W7 keeps the pre-W7 behaviour as the default: every frozen screenshot,
/// `assert_region` coordinate and frame-size baseline in the gate list was
/// captured against the carved frame. `report` is opt-in (see EXPERIMENTS
/// E13.5); the code path is exercised by the `w7-insets-report` scenario.
pub const DEFAULT_INSETS_MODE: InsetsMode = InsetsMode::Carve;

/// App-window inset model selected by [`INSETS_ENV`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InsetsMode {
    /// Pre-W7: the window manager subtracts the system bars from the frame.
    #[default]
    Carve,
    /// AOSP: full-display window plus a reported `InsetsState`.
    Report,
}

impl InsetsMode {
    /// Parse the `ACOS_WM_INSETS` spelling (`carve` / `report`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "carve" => Some(Self::Carve),
            "report" => Some(Self::Report),
            _ => None,
        }
    }

    /// The wire/env spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Carve => "carve",
            Self::Report => "report",
        }
    }

    /// Parse `value` or fall back to `default`.
    #[must_use]
    pub fn parse_or(value: &str, default: Self) -> Self {
        Self::parse(value).unwrap_or(default)
    }

    /// The selector from the process environment, falling back to `default`.
    #[must_use]
    pub fn from_env(default: Self) -> Self {
        std::env::var(INSETS_ENV)
            .ok()
            .map(|value| Self::parse_or(&value, default))
            .unwrap_or(default)
    }

    /// `true` for [`InsetsMode::Report`].
    #[must_use]
    pub const fn is_report(self) -> bool {
        matches!(self, Self::Report)
    }
}

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
    /// W7: whether the app window is carved or the insets are reported.
    pub insets_mode: InsetsMode,
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
            insets_mode: InsetsMode::Carve,
        }
    }

    /// The reference display with an explicit W7 inset model.
    #[must_use]
    pub const fn reference_with_insets(insets_mode: InsetsMode) -> Self {
        let mut display = Self::reference_1080x2400();
        display.insets_mode = insets_mode;
        display
    }

    /// A display with explicit geometry, used by `acos launch --density` /
    /// `--display` / `--reference` so ACOS can be laid out on the *same*
    /// display configuration as the phone under test instead of the API-34
    /// emulator reference.
    ///
    /// The status-bar and navigation-bar insets are the only bar geometry the
    /// app window consumes; the gesture/cutout rectangles are scaled from the
    /// reference so the inset state stays internally consistent.
    #[must_use]
    pub fn custom(
        width: i32,
        height: i32,
        density_dpi: u32,
        status_bar_top: i32,
        nav_bar_bottom: i32,
    ) -> Self {
        Self::custom_with_insets(
            width,
            height,
            density_dpi,
            status_bar_top,
            nav_bar_bottom,
            InsetsMode::Carve,
        )
    }

    /// [`Self::custom`] with an explicit W7 inset model.
    #[must_use]
    pub fn custom_with_insets(
        width: i32,
        height: i32,
        density_dpi: u32,
        status_bar_top: i32,
        nav_bar_bottom: i32,
        insets_mode: InsetsMode,
    ) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let density = density_dpi as f32 / 160.0;
        let bars = SystemBars {
            status_bar: RectI::new(0, 0, width, status_bar_top),
            status_bar_tappable: RectI::new(0, 0, width, status_bar_top),
            nav_bar: RectI::new(0, height - nav_bar_bottom, width, height),
            nav_bar_tappable: RectI::new(0, 0, 0, 0),
            ime_visible: RectI::new(0, height, width, height),
            display_cutout: RectI::new(0, 0, width, status_bar_top),
            mandatory_system_gestures_top: RectI::new(0, 0, width, status_bar_top),
            mandatory_system_gestures_bottom: RectI::new(
                0,
                height - nav_bar_bottom,
                width,
                height,
            ),
            system_gestures_left: RectI::new(0, 0, 0, height),
            system_gestures_right: RectI::new(width, 0, width, height),
        };
        Self {
            id: 0,
            frame: RectI::new(0, 0, width, height),
            app_bounds: RectI::new(0, status_bar_top, width, height - nav_bar_bottom),
            density_dpi,
            density,
            refresh_hz: REFRESH_HZ,
            frame_interval_nanos: FRAME_INTERVAL_NANOS,
            bars,
            rotation: 0,
            insets_mode,
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

    /// `addToDisplayAsUser`'s `outFrame` for a normal app window: the app bounds
    /// under [`InsetsMode::Carve`], the full display under
    /// [`InsetsMode::Report`] (AOSP `WindowLayout` with `fitInsetsTypes == 0`,
    /// which `PhoneWindow.generateLayout` sets for every non-floating window).
    #[must_use]
    pub const fn window_frame(&self) -> RectI {
        match self.insets_mode {
            InsetsMode::Carve => self.app_bounds,
            InsetsMode::Report => self.frame,
        }
    }

    /// Screen width in dp at this density (framework rounding).
    #[must_use]
    pub fn screen_width_dp(&self) -> i32 {
        ((self.frame.width() as i64 * 160 + self.density_dpi as i64 / 2)
            / self.density_dpi.max(1) as i64) as i32
    }

    /// Screen height in dp at this density (framework rounding).
    #[must_use]
    pub fn screen_height_dp(&self) -> i32 {
        ((self.frame.height() as i64 * 160 + self.density_dpi as i64 / 2)
            / self.density_dpi.max(1) as i64) as i32
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
    fn custom_display_applies_phone_geometry() {
        // The moto g35 under test: 1080x2400 @ 400 dpi, 108 px status bar,
        // gesture navigation (no navigation-bar inset).
        let d = DisplaySpec::custom(1080, 2400, 400, 108, 0);
        assert_eq!(d.frame, RectI::new(0, 0, 1080, 2400));
        assert_eq!(d.app_bounds, RectI::new(0, 108, 1080, 2400));
        assert_eq!(d.app_insets(), InsetsI::new(0, 108, 0, 0));
        assert_eq!(d.density, 2.5);
        assert_eq!(d.density_dpi, 400);
        assert!(!d.has_navigation_bar());
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
