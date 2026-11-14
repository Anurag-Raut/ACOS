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
/// ADR-0026 decision 6 makes AOSP's phone-like edge-to-edge model the default
/// (W7b): an Activity owns the full display and is told the system-bar insets,
/// exactly as on a real phone. `carve` stays selectable for the pre-W7 shim
/// (`ACOS_WM_INSETS=carve`, `acos launch --insets carve`, a scenario's
/// `insets: carve`); the frozen carve baselines are then opt-in, not ambient.
pub const DEFAULT_INSETS_MODE: InsetsMode = InsetsMode::Report;

/// App-window inset model selected by [`INSETS_ENV`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InsetsMode {
    /// Pre-W7: the window manager subtracts the system bars from the frame.
    Carve,
    /// AOSP: full-display window plus a reported `InsetsState`.
    #[default]
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
    ///
    /// Which edge a bar occupies is read off the bar rect itself (a bar either
    /// spans the frame's width and sits on a horizontal edge, or spans the
    /// frame's height and sits on a vertical edge), so the answer follows the
    /// rotation instead of assuming the portrait layout. [`Self::app_insets_for`]
    /// is the display-aware form; this one uses [`RectI::DISPLAY_1080X2400`].
    #[must_use]
    pub fn app_insets(&self) -> InsetsI {
        self.app_insets_for(RectI::DISPLAY_1080X2400)
    }

    /// [`Self::app_insets`] against an explicit display frame.
    #[must_use]
    pub fn app_insets_for(&self, frame: RectI) -> InsetsI {
        let status = edge_inset(self.status_bar, frame);
        let nav = edge_inset(self.nav_bar, frame);
        InsetsI::new(
            status.left + nav.left,
            status.top + nav.top,
            status.right + nav.right,
            status.bottom + nav.bottom,
        )
    }

    /// One quarter turn into the `src_h x src_w` frame.
    ///
    /// The rects are turned with [`RectI::rotate_quarters`] (a `Surface.ROTATION_90`
    /// clockwise turn), **except the navigation bar**: `DisplayPolicy`'s phone
    /// rule is `mNavigationBarCanMove == false`, so the navigation bar hugs the
    /// bottom edge of the frame in every rotation. A reference API-34 phone's
    /// `mDecorInsetsInfo` says exactly that — `ROTATION_0` `[0,128][0,63]`,
    /// `ROTATION_90` `[128,0][0,63]`, `ROTATION_180` `[0,0][0,191]` (status bar
    /// 128 + nav 63 both at the bottom), `ROTATION_270` `[0,0][128,63]` — i.e.
    /// only the status bar travels to a side edge.
    #[must_use]
    pub fn rotate_quarters(&self, src_w: i32, src_h: i32) -> Self {
        let r = |rect: RectI| rect.rotate_quarters(src_w, src_h);
        let new_h = src_w;
        let new_w = src_h;
        Self {
            status_bar: r(self.status_bar),
            status_bar_tappable: r(self.status_bar_tappable),
            // The navigation bar cannot move: it stays on the bottom edge of
            // the new frame (`NAV_BAR_BOTTOM`), keeping its thickness.
            nav_bar: RectI::new(0, new_h - self.nav_bar.height(), new_w, new_h),
            nav_bar_tappable: r(self.nav_bar_tappable),
            ime_visible: r(self.ime_visible),
            display_cutout: r(self.display_cutout),
            mandatory_system_gestures_top: r(self.mandatory_system_gestures_top),
            mandatory_system_gestures_bottom: r(self.mandatory_system_gestures_bottom),
            system_gestures_left: r(self.system_gestures_left),
            system_gestures_right: r(self.system_gestures_right),
        }
    }
}

/// The thickness `rect` takes away from `frame`, on the edge it hugs.
///
/// A bar that spans the frame's width is on a horizontal edge; one that spans
/// its height is on a vertical edge. Anything else (an empty bar, the
/// `[0,0][0,0]` tappable placeholders) contributes no inset.
fn edge_inset(rect: RectI, frame: RectI) -> InsetsI {
    if rect.is_empty() {
        return InsetsI::new(0, 0, 0, 0);
    }
    let spans_width = rect.width() >= frame.width();
    let spans_height = rect.height() >= frame.height();
    match (spans_width, spans_height) {
        (true, false) => {
            if rect.top <= frame.top {
                InsetsI::new(0, rect.height(), 0, 0)
            } else {
                InsetsI::new(0, 0, 0, rect.height())
            }
        }
        (false, true) => {
            if rect.left <= frame.left {
                InsetsI::new(rect.width(), 0, 0, 0)
            } else {
                InsetsI::new(0, 0, rect.width(), 0)
            }
        }
        // A corner-hugging or degenerate rect is not a bar edge.
        _ => InsetsI::new(0, 0, 0, 0),
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
    pub fn app_insets(&self) -> InsetsI {
        self.bars.app_insets_for(self.frame)
    }

    /// The display rotated to `rotation` (`0..3`, AOSP `Surface.ROTATION_*`),
    /// one quarter turn per step.
    ///
    /// The display frame, every system-bar rect and the app window frame move
    /// with it; the density and refresh rate do not (a rotation changes the
    /// frame the window is laid out in, not the panel). Rotating to the current
    /// rotation is a no-op, and `rotation + 4` wraps.
    ///
    /// This is the geometry half of AOSP's `DisplayContent.updateRotationUnchecked`:
    /// the new frame is what `IWindowSession.relayout` answers with and what the
    /// new `Configuration`'s `screenWidthDp`/`screenHeightDp` and
    /// `windowConfiguration` bounds are computed from.
    #[must_use]
    pub fn rotated(&self, rotation: u8) -> Self {
        let target = rotation % 4;
        let mut out = *self;
        // Step one quarter turn at a time: the frame dimensions swap on every
        // step, so the rect transform always has the right source size.
        let mut guard = 0;
        while out.rotation % 4 != target && guard < 4 {
            let w = out.frame.width();
            let h = out.frame.height();
            out.frame = RectI::new(0, 0, h, w);
            out.bars = out.bars.rotate_quarters(w, h);
            out.rotation = (out.rotation + 1) % 4;
            guard += 1;
        }
        out.app_bounds = RectI::new(
            out.frame.left + out.bars.app_insets_for(out.frame).left,
            out.frame.top + out.bars.app_insets_for(out.frame).top,
            out.frame.right - out.bars.app_insets_for(out.frame).right,
            out.frame.bottom - out.bars.app_insets_for(out.frame).bottom,
        );
        out
    }

    /// `true` when the display is taller than it is wide (`ORIENTATION_PORTRAIT`
    /// otherwise, `ORIENTATION_LANDSCAPE`).
    #[must_use]
    pub const fn is_landscape(&self) -> bool {
        self.frame.width() > self.frame.height()
    }

    /// `Configuration.orientation` for this display.
    #[must_use]
    pub const fn orientation(&self) -> crate::Orientation {
        if self.is_landscape() {
            crate::Orientation::Landscape
        } else {
            crate::Orientation::Portrait
        }
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

/// `ActivityInfo.CONFIG_ORIENTATION` (`android:configChanges="orientation"`).
pub const CONFIG_ORIENTATION: i32 = 0x0000_0080;
/// `ActivityInfo.CONFIG_SCREEN_SIZE` (`screenSize`).
pub const CONFIG_SCREEN_SIZE: i32 = 0x0000_0400;
/// `ActivityInfo.CONFIG_SCREEN_LAYOUT` (`screenLayout`).
pub const CONFIG_SCREEN_LAYOUT: i32 = 0x0000_0100;
/// `ActivityInfo.CONFIG_SMALLEST_SCREEN_SIZE` (`smallestScreenSize`).
pub const CONFIG_SMALLEST_SCREEN_SIZE: i32 = 0x0000_0800;

/// What one display rotation changes in the `Configuration`, and whether the
/// Activity handles all of it itself.
///
/// AOSP decides the same question in `ActivityRecord.updateConfigurationLocked`
/// → `getRealConfigChanged()` — the bitmask an Activity declares in its
/// manifest (`android:configChanges`) is compared against what the rotation
/// delta touches; anything the Activity did not declare recreates it through
/// `ActivityRelaunchItem`. A rotation always changes these four bits:
///
/// * `orientation` (portrait ↔ landscape),
/// * `screenSize` (`screenWidthDp`/`screenHeightDp`),
/// * `screenLayout` (the long/size bucket of the new aspect),
/// * `smallestScreenSize` (the short edge in dp).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationRelaunch {
    /// The `android:configChanges` bitmask declared by the Activity.
    pub declared: i32,
}

impl RotationRelaunch {
    /// The bits a rotation of this display changes.
    #[must_use]
    pub const fn rotation_changes() -> i32 {
        CONFIG_ORIENTATION | CONFIG_SCREEN_SIZE | CONFIG_SCREEN_LAYOUT | CONFIG_SMALLEST_SCREEN_SIZE
    }

    /// The decision for one Activity's declared `configChanges`.
    #[must_use]
    pub const fn for_rotation(declared: i32) -> Self {
        Self { declared }
    }

    /// The bits the rotation changes that the Activity did **not** declare.
    #[must_use]
    pub const fn missing(self) -> i32 {
        Self::rotation_changes() & !self.declared
    }

    /// `true` when AOSP recreates the Activity (delivers
    /// `ActivityRelaunchItem`), `false` when it delivers
    /// `ActivityConfigurationChangeItem` and the Activity keeps its instance.
    #[must_use]
    pub const fn relaunches(self) -> bool {
        self.missing() != 0
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

    #[test]
    fn rotate_to_landscape_swaps_the_frame_and_moves_the_bars() {
        let portrait = DisplaySpec::reference_1080x2400();
        let land = portrait.rotated(1);
        assert_eq!(land.frame, RectI::new(0, 0, 2400, 1080));
        assert_eq!(land.rotation, 1);
        assert!(land.is_landscape());
        assert_eq!(land.orientation(), crate::Orientation::Landscape);
        // Density and refresh rate are panel properties: a rotation keeps them.
        assert_eq!(land.density_dpi, portrait.density_dpi);
        assert_eq!(land.refresh_hz, portrait.refresh_hz);
        // The portrait status bar [0,0][1080,128] becomes the left-edge bar
        // [0,0][128,1080] (a reference phone's `mDecorInsetsInfo` ROTATION_90
        // is `nonDecorInsets=[128,0][0,63]`), while the navigation bar stays on
        // the bottom edge (`mNavigationBarCanMove == false`).
        assert_eq!(land.bars.status_bar, RectI::new(0, 0, 128, 1080));
        assert_eq!(land.bars.nav_bar, RectI::new(0, 1017, 2400, 1080));
        assert_eq!(land.app_insets(), InsetsI::new(128, 0, 0, 63));
        assert_eq!(land.app_bounds, RectI::new(128, 0, 2400, 1017));
        // ... and the dp screen size follows the frame, not the rotation.
        assert_eq!(land.screen_width_dp(), 914);
        assert_eq!(land.screen_height_dp(), 411);
    }

    #[test]
    fn rotate_back_restores_the_portrait_geometry() {
        let portrait = DisplaySpec::reference_1080x2400();
        for target in [0u8, 1, 2, 3, 0, 4] {
            let there = portrait.rotated(target);
            let back = there.rotated(0);
            assert_eq!(back.frame, portrait.frame, "rotation {target} frame");
            assert_eq!(back.bars, portrait.bars, "rotation {target} bars");
            assert_eq!(back.app_bounds, portrait.app_bounds, "rotation {target} app bounds");
            assert_eq!(back.rotation, 0);
        }
        // Half turn keeps the portrait frame; the status bar moves to the
        // bottom edge, where the navigation bar already is (ROTATION_180 in a
        // reference phone's `mDecorInsetsInfo` is `nonDecorInsets=[0,0][0,191]`).
        let half = portrait.rotated(2);
        assert_eq!(half.frame, portrait.frame);
        assert!(!half.is_landscape());
        assert_eq!(half.bars.status_bar, RectI::new(0, 2272, 1080, 2400));
        assert_eq!(half.bars.nav_bar, RectI::new(0, 2337, 1080, 2400));
        assert_eq!(half.app_insets(), InsetsI::new(0, 0, 0, 191));
        assert_eq!(half.app_bounds, RectI::new(0, 0, 1080, 2209));
    }

    /// The four rotations against the reference device's own numbers.
    ///
    /// Source: `fixtures/reference/hello-views/art-api34-x86_64/display.txt`
    /// (`dumpsys window displays`, API-34 x86_64 phone, 1080x2400 @ 420 dpi):
    ///
    /// ```text
    /// mDecorInsetsInfo:
    ///   ROTATION_0={nonDecorInsets=[0,128][0,63], nonDecorFrame=[0,128][1080,2337]}
    ///   ROTATION_90={nonDecorInsets=[128,0][0,63], nonDecorFrame=[128,0][2400,1017]}
    ///   ROTATION_180={nonDecorInsets=[0,0][0,191], nonDecorFrame=[0,0][1080,2209]}
    ///   ROTATION_270={nonDecorInsets=[0,0][128,63], nonDecorFrame=[0,0][2272,1017]}
    /// ```
    ///
    /// The app window frame (`nonDecorFrame`) is what the app is laid out in,
    /// and it is exactly `frame - app_insets()` here; the UI-tree bounds of the
    /// oracle capture (`ui_after_recreate.xml`: root `[128,0][2400,1080]`,
    /// `decor_content_parent` `[128,0][2400,1017]`) sit inside it.
    #[test]
    fn every_rotation_matches_the_reference_decor_insets() {
        let d = DisplaySpec::reference_1080x2400();
        // (rotation, status bar edge, nav bar edge, insets, non-decor frame)
        let table: [(u8, RectI, RectI, InsetsI, RectI); 4] = [
            (
                0,
                RectI::new(0, 0, 1080, 128),
                RectI::new(0, 2337, 1080, 2400),
                InsetsI::new(0, 128, 0, 63),
                RectI::new(0, 128, 1080, 2337),
            ),
            (
                1,
                RectI::new(0, 0, 128, 1080),
                RectI::new(0, 1017, 2400, 1080),
                InsetsI::new(128, 0, 0, 63),
                RectI::new(128, 0, 2400, 1017),
            ),
            (
                2,
                RectI::new(0, 2272, 1080, 2400),
                RectI::new(0, 2337, 1080, 2400),
                InsetsI::new(0, 0, 0, 191),
                RectI::new(0, 0, 1080, 2209),
            ),
            (
                3,
                RectI::new(2272, 0, 2400, 1080),
                RectI::new(0, 1017, 2400, 1080),
                InsetsI::new(0, 0, 128, 63),
                RectI::new(0, 0, 2272, 1017),
            ),
        ];
        for (rotation, status, nav, insets, non_decor) in table {
            let r = d.rotated(rotation);
            assert_eq!(r.bars.status_bar, status, "rotation {rotation} status bar");
            assert_eq!(r.bars.nav_bar, nav, "rotation {rotation} nav bar");
            assert_eq!(r.app_insets(), insets, "rotation {rotation} insets");
            assert_eq!(r.app_bounds, non_decor, "rotation {rotation} app frame");
            // Every bar rect stays inside the rotated frame.
            for rect in [
                r.bars.status_bar,
                r.bars.nav_bar,
                r.bars.system_gestures_left,
                r.bars.system_gestures_right,
            ] {
                assert_eq!(rect.intersect(r.frame), rect, "rotation {rotation} bar inside frame");
            }
        }
    }

    #[test]
    fn rotated_twice_is_a_half_turn_and_the_other_quarter_is_its_mirror() {
        let d = DisplaySpec::reference_1080x2400();
        // `rotated` names the target rotation, so asking twice is the same as
        // asking once; a step from either side lands on the same geometry.
        assert_eq!(d.rotated(1).rotated(1), d.rotated(1));
        assert_eq!(d.rotated(1).rotated(2), d.rotated(2));
        // Rotation 3 is the other landscape: same frame, the status bar on the
        // opposite edge, the navigation bar still on the bottom edge.
        let other = d.rotated(3);
        assert_eq!(other.frame, RectI::new(0, 0, 2400, 1080));
        assert_eq!(other.bars.status_bar, RectI::new(2272, 0, 2400, 1080));
        assert_eq!(other.bars.nav_bar, RectI::new(0, 1017, 2400, 1080));
        assert_eq!(other.app_insets(), InsetsI::new(0, 0, 128, 63));
        assert_ne!(other.bars, d.rotated(1).bars);
        assert_eq!(d.rotated(1).rotated(0), d);
    }

    #[test]
    fn config_change_bits_follow_the_manifest_that_declares_them() {
        // `android:configChanges="orientation|screenSize"` handles a rotation.
        let handled = CONFIG_ORIENTATION | CONFIG_SCREEN_SIZE | CONFIG_SCREEN_LAYOUT
            | CONFIG_SMALLEST_SCREEN_SIZE;
        assert!(!RotationRelaunch::for_rotation(handled).relaunches());
        // The framework default (no `configChanges`) recreates the Activity.
        assert!(RotationRelaunch::for_rotation(0).relaunches());
        // Declaring only `orientation` is not enough: `screenSize` changed too.
        assert!(RotationRelaunch::for_rotation(CONFIG_ORIENTATION).relaunches());
        assert_eq!(
            RotationRelaunch::for_rotation(CONFIG_ORIENTATION).missing(),
            CONFIG_SCREEN_SIZE | CONFIG_SCREEN_LAYOUT | CONFIG_SMALLEST_SCREEN_SIZE
        );
    }
}
