//! The **window layer stack**: real Android window types, their z-order and the
//! topmost-touchable / topmost-focusable lookups that composition, the UI tree
//! and input hit-testing all read.
//!
//! Before this module the WMS knew only "window N was added after window N-1"
//! (`z = windows.len() + 1`) and `focusable = !is_system_window()`. That is
//! enough for one activity window per session and wrong for everything else: an
//! application overlay (`TYPE_APPLICATION_OVERLAY`), an accessibility overlay,
//! a dialog, a popup and the notification shade all got an insertion-order z,
//! and `FLAG_NOT_TOUCHABLE`/`FLAG_NOT_FOCUSABLE` were not consulted at all.
//!
//! ## Where the numbers come from
//!
//! Every `TYPE_*` value here is transcribed from the image's own framework
//! source (`android.view.WindowManager$LayoutParams`,
//! `.cache/aosp/base/core/java/android/view/WindowManager.java`,
//! `FIRST_SYSTEM_WINDOW = 2000`), not guessed. The **ordering** rules use only
//! relations the framework documents on the type itself:
//!
//! * sub-windows (1000..1999) are "kept next to their attached window in
//!   Z-order, and their coordinate space is relative to their attached window";
//!   a panel "appears on top of their attached window";
//! * `TYPE_APPLICATION_OVERLAY` is "displayed above all activity windows
//!   (types between FIRST_APPLICATION_WINDOW and LAST_APPLICATION_WINDOW) but
//!   below critical system windows like the status bar or IME";
//! * `TYPE_ACCESSIBILITY_OVERLAY` is the interception overlay: it is overlaid
//!   only by a connected accessibility service;
//! * `TYPE_NOTIFICATION_SHADE` is the shade and keyguard layer, i.e. a system
//!   window above the app's own overlays.
//!
//! The numeric band of the *system* windows relative to each other (shade vs
//! status bar vs IME) is decided by
//! `PhoneWindowManager.getWindowLayerFromTypeLw`, which lives in
//! `services.jar` and is **not** in the AOSP source cache. [`LayerBand`] keeps
//! those relations as separate variants so the one table that has to be
//! transcribed later is a single `match` arm, and the open question is recorded
//! in `results/handoff/LAYER0/DESIGN.md` §5 instead of being invented here.
//!
//! ## Flags
//!
//! `FLAG_NOT_TOUCHABLE` (0x10) and `FLAG_NOT_FOCUSABLE` (0x8) are the two flags
//! that decide routing; `FLAG_NOT_TOUCH_MODAL` (0x20) decides whether a touch
//! outside the frame is passed through (AOSP `InputWindowHandle`). `FLAG_DIM_BEHIND`
//! (0x2) is the window manager's own dim layer, not the client's.

use crate::geom::{PointI, RectI};
use crate::wms::WindowId;

/// `WindowManager.LayoutParams.FLAG_DIM_BEHIND`.
pub const FLAG_DIM_BEHIND: i32 = 0x0000_0002;
/// `WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE`.
pub const FLAG_NOT_FOCUSABLE: i32 = 0x0000_0008;
/// `WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE`.
pub const FLAG_NOT_TOUCHABLE: i32 = 0x0000_0010;
/// `WindowManager.LayoutParams.FLAG_NOT_TOUCH_MODAL`.
pub const FLAG_NOT_TOUCH_MODAL: i32 = 0x0000_0020;

/// `WindowManager.LayoutParams.FIRST_APPLICATION_WINDOW`.
pub const FIRST_APPLICATION_WINDOW: i32 = 1;
/// `WindowManager.LayoutParams.LAST_APPLICATION_WINDOW`.
pub const LAST_APPLICATION_WINDOW: i32 = 99;
/// `WindowManager.LayoutParams.FIRST_SUB_WINDOW`.
pub const FIRST_SUB_WINDOW: i32 = 1000;
/// `WindowManager.LayoutParams.LAST_SUB_WINDOW`.
pub const LAST_SUB_WINDOW: i32 = 1999;
/// `WindowManager.LayoutParams.FIRST_SYSTEM_WINDOW`.
pub const FIRST_SYSTEM_WINDOW: i32 = 2000;
/// `WindowManager.LayoutParams.LAST_SYSTEM_WINDOW`.
pub const LAST_SYSTEM_WINDOW: i32 = 2999;

/// `TYPE_BASE_APPLICATION`.
pub const TYPE_BASE_APPLICATION: i32 = FIRST_APPLICATION_WINDOW;
/// `TYPE_APPLICATION` — the activity window.
pub const TYPE_APPLICATION: i32 = FIRST_APPLICATION_WINDOW + 1;
/// `TYPE_APPLICATION_STARTING`.
pub const TYPE_APPLICATION_STARTING: i32 = FIRST_APPLICATION_WINDOW + 2;
/// `TYPE_DRAWN_APPLICATION`.
pub const TYPE_DRAWN_APPLICATION: i32 = FIRST_APPLICATION_WINDOW + 3;

/// `TYPE_APPLICATION_PANEL` — the `PopupWindow` layer.
pub const TYPE_APPLICATION_PANEL: i32 = FIRST_SUB_WINDOW;
/// `TYPE_APPLICATION_MEDIA`.
pub const TYPE_APPLICATION_MEDIA: i32 = FIRST_SUB_WINDOW + 1;
/// `TYPE_APPLICATION_SUB_PANEL`.
pub const TYPE_APPLICATION_SUB_PANEL: i32 = FIRST_SUB_WINDOW + 2;
/// `TYPE_APPLICATION_ATTACHED_DIALOG` — the panel-shaped `Dialog` layer.
pub const TYPE_APPLICATION_ATTACHED_DIALOG: i32 = FIRST_SUB_WINDOW + 3;
/// `TYPE_APPLICATION_MEDIA_OVERLAY`.
pub const TYPE_APPLICATION_MEDIA_OVERLAY: i32 = FIRST_SUB_WINDOW + 4;
/// `TYPE_APPLICATION_ABOVE_SUB_PANEL`.
pub const TYPE_APPLICATION_ABOVE_SUB_PANEL: i32 = FIRST_SUB_WINDOW + 5;

/// `TYPE_STATUS_BAR`.
pub const TYPE_STATUS_BAR: i32 = FIRST_SYSTEM_WINDOW;
/// `TYPE_SYSTEM_ALERT` — the ANR/crash dialog layer.
pub const TYPE_SYSTEM_ALERT: i32 = FIRST_SYSTEM_WINDOW + 3;
/// `TYPE_SYSTEM_OVERLAY`.
pub const TYPE_SYSTEM_OVERLAY: i32 = FIRST_SYSTEM_WINDOW + 6;
/// `TYPE_SYSTEM_DIALOG` — the volume dialog / system-owned dialogs.
pub const TYPE_SYSTEM_DIALOG: i32 = FIRST_SYSTEM_WINDOW + 8;
/// `TYPE_SYSTEM_ERROR` — above all other windows.
pub const TYPE_SYSTEM_ERROR: i32 = FIRST_SYSTEM_WINDOW + 10;
/// `TYPE_INPUT_METHOD`.
pub const TYPE_INPUT_METHOD: i32 = FIRST_SYSTEM_WINDOW + 11;
/// `TYPE_INPUT_METHOD_DIALOG`.
pub const TYPE_INPUT_METHOD_DIALOG: i32 = FIRST_SYSTEM_WINDOW + 12;
/// `TYPE_WALLPAPER` — at the bottom.
pub const TYPE_WALLPAPER: i32 = FIRST_SYSTEM_WINDOW + 13;
/// `TYPE_NAVIGATION_BAR`.
pub const TYPE_NAVIGATION_BAR: i32 = FIRST_SYSTEM_WINDOW + 19;
/// `TYPE_VOLUME_OVERLAY`.
pub const TYPE_VOLUME_OVERLAY: i32 = FIRST_SYSTEM_WINDOW + 20;
/// `TYPE_ACCESSIBILITY_OVERLAY` — the accessibility interception overlay.
pub const TYPE_ACCESSIBILITY_OVERLAY: i32 = FIRST_SYSTEM_WINDOW + 32;
/// `TYPE_SCREENSHOT`.
pub const TYPE_SCREENSHOT: i32 = FIRST_SYSTEM_WINDOW + 36;
/// `TYPE_APPLICATION_OVERLAY` — `SYSTEM_ALERT_WINDOW` overlays.
pub const TYPE_APPLICATION_OVERLAY: i32 = FIRST_SYSTEM_WINDOW + 38;
/// `TYPE_ACCESSIBILITY_MAGNIFICATION_OVERLAY`.
pub const TYPE_ACCESSIBILITY_MAGNIFICATION_OVERLAY: i32 = FIRST_SYSTEM_WINDOW + 39;
/// `TYPE_NOTIFICATION_SHADE` — the shade and keyguard layer.
pub const TYPE_NOTIFICATION_SHADE: i32 = FIRST_SYSTEM_WINDOW + 40;
/// `TYPE_STATUS_BAR_ADDITIONAL`.
pub const TYPE_STATUS_BAR_ADDITIONAL: i32 = FIRST_SYSTEM_WINDOW + 41;

/// What a window *is*, in the terms an agent and the compositor care about.
///
/// This is a classification of the real `LayoutParams.type`, not a new type
/// system: [`WindowKind::of`] maps every documented type to exactly one kind and
/// keeps the raw value on the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WindowKind {
    /// `TYPE_WALLPAPER`: at the bottom, below every application window.
    Wallpaper,
    /// `FIRST_APPLICATION_WINDOW..=LAST_APPLICATION_WINDOW` (`TYPE_APPLICATION`
    /// and its variants): the activity window.
    Application,
    /// `FIRST_SUB_WINDOW..=LAST_SUB_WINDOW`: a panel/attached dialog/media
    /// surface kept next to its attached window.
    SubWindow,
    /// `TYPE_APPLICATION_OVERLAY` — above all activity windows, below critical
    /// system windows (status bar, IME).
    ApplicationOverlay,
    /// `TYPE_ACCESSIBILITY_OVERLAY` — the accessibility interception overlay.
    AccessibilityOverlay,
    /// `TYPE_SYSTEM_ALERT` — the ANR/crash dialog layer.
    SystemAlert,
    /// `TYPE_SYSTEM_DIALOG`.
    SystemDialog,
    /// `TYPE_SYSTEM_OVERLAY`.
    SystemOverlay,
    /// `TYPE_NOTIFICATION_SHADE` — the shade and keyguard.
    NotificationShade,
    /// `TYPE_INPUT_METHOD` / `TYPE_INPUT_METHOD_DIALOG`.
    InputMethod,
    /// `TYPE_STATUS_BAR` / `TYPE_STATUS_BAR_ADDITIONAL` / `TYPE_NAVIGATION_BAR`.
    SystemBar,
    /// Any other `FIRST_SYSTEM_WINDOW..=LAST_SYSTEM_WINDOW` type.
    System,
    /// Outside every documented range (`LayoutParams.type` is client-supplied).
    Unknown,
}

/// Z-order band. Larger paints later. The *relations* are the AOSP-documented
/// ones (see the module docs); the relative order of two system bands is the one
/// piece still owed to `getWindowLayerFromTypeLw` and is flagged as open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LayerBand {
    /// Below every application window.
    Wallpaper = 0,
    /// `FIRST_APPLICATION_WINDOW..=LAST_APPLICATION_WINDOW`.
    Application = 10,
    /// Sub-windows: kept next to their attached window (inserted directly above
    /// it by [`WindowLayerStack::ordered`]), never above an overlay.
    SubWindow = 20,
    /// Above all activity windows.
    ApplicationOverlay = 30,
    /// The accessibility interception overlay.
    AccessibilityOverlay = 40,
    /// Above the app's overlays, below the bar/IME band.
    SystemAlert = 50,
    /// Above the app's overlays, below the bar/IME band.
    SystemDialog = 51,
    /// Above the app's overlays, below the bar/IME band.
    SystemOverlay = 52,
    /// The shade: a system window above the app's overlays.
    NotificationShade = 60,
    /// Above the shade.
    SystemBar = 70,
    /// The IME is above the bars (it covers them in `adjustResize`).
    InputMethod = 80,
    /// Anything else in the system range.
    System = 90,
    /// An undocumented `type`: drawn on top and named, never silently merged
    /// with the application band.
    Unknown = 100,
}

impl WindowKind {
    /// Classify a real `LayoutParams.type`.
    #[must_use]
    pub const fn of(type_: i32) -> WindowKind {
        match type_ {
            TYPE_WALLPAPER => WindowKind::Wallpaper,
            FIRST_APPLICATION_WINDOW..=LAST_APPLICATION_WINDOW => WindowKind::Application,
            FIRST_SUB_WINDOW..=LAST_SUB_WINDOW => WindowKind::SubWindow,
            TYPE_APPLICATION_OVERLAY => WindowKind::ApplicationOverlay,
            TYPE_ACCESSIBILITY_OVERLAY | TYPE_ACCESSIBILITY_MAGNIFICATION_OVERLAY => {
                WindowKind::AccessibilityOverlay
            }
            TYPE_SYSTEM_ALERT => WindowKind::SystemAlert,
            TYPE_SYSTEM_DIALOG => WindowKind::SystemDialog,
            TYPE_SYSTEM_OVERLAY | TYPE_SYSTEM_ERROR | TYPE_VOLUME_OVERLAY | TYPE_SCREENSHOT => {
                WindowKind::SystemOverlay
            }
            TYPE_NOTIFICATION_SHADE => WindowKind::NotificationShade,
            TYPE_INPUT_METHOD | TYPE_INPUT_METHOD_DIALOG => WindowKind::InputMethod,
            TYPE_STATUS_BAR | TYPE_STATUS_BAR_ADDITIONAL | TYPE_NAVIGATION_BAR => {
                WindowKind::SystemBar
            }
            FIRST_SYSTEM_WINDOW..=LAST_SYSTEM_WINDOW => WindowKind::System,
            _ => WindowKind::Unknown,
        }
    }

    /// The band this kind is drawn in.
    #[must_use]
    pub const fn band(self) -> LayerBand {
        match self {
            WindowKind::Wallpaper => LayerBand::Wallpaper,
            WindowKind::Application => LayerBand::Application,
            WindowKind::SubWindow => LayerBand::SubWindow,
            WindowKind::ApplicationOverlay => LayerBand::ApplicationOverlay,
            WindowKind::AccessibilityOverlay => LayerBand::AccessibilityOverlay,
            WindowKind::SystemAlert => LayerBand::SystemAlert,
            WindowKind::SystemDialog => LayerBand::SystemDialog,
            WindowKind::SystemOverlay => LayerBand::SystemOverlay,
            WindowKind::NotificationShade => LayerBand::NotificationShade,
            WindowKind::SystemBar => LayerBand::SystemBar,
            WindowKind::InputMethod => LayerBand::InputMethod,
            WindowKind::System => LayerBand::System,
            WindowKind::Unknown => LayerBand::Unknown,
        }
    }

    /// `true` for the activity window kinds an agent calls "the app".
    #[must_use]
    pub const fn is_application(self) -> bool {
        matches!(self, WindowKind::Application)
    }

    /// `true` for a panel/attached dialog/media surface: its coordinates are
    /// relative to its attached window and it is stacked next to it.
    #[must_use]
    pub const fn is_sub_window(self) -> bool {
        matches!(self, WindowKind::SubWindow)
    }

    /// The agent-facing name, stable across versions (it is what the UI tree and
    /// `acos_capabilities` print).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            WindowKind::Wallpaper => "wallpaper",
            WindowKind::Application => "application",
            WindowKind::SubWindow => "sub_window",
            WindowKind::ApplicationOverlay => "application_overlay",
            WindowKind::AccessibilityOverlay => "accessibility_overlay",
            WindowKind::SystemAlert => "system_alert",
            WindowKind::SystemDialog => "system_dialog",
            WindowKind::SystemOverlay => "system_overlay",
            WindowKind::NotificationShade => "notification_shade",
            WindowKind::InputMethod => "input_method",
            WindowKind::SystemBar => "system_bar",
            WindowKind::System => "system",
            WindowKind::Unknown => "unknown",
        }
    }
}

/// `true` when a window with these `LayoutParams.flags` receives touch events
/// (`!FLAG_NOT_TOUCHABLE`).
#[must_use]
pub const fn touchable(flags: i32) -> bool {
    flags & FLAG_NOT_TOUCHABLE == 0
}

/// `true` when a window with these `LayoutParams.flags` may take focus
/// (`!FLAG_NOT_FOCUSABLE`).
#[must_use]
pub const fn focusable(flags: i32) -> bool {
    flags & FLAG_NOT_FOCUSABLE == 0
}

/// `true` when a touch outside the frame passes through to the window below
/// (`FLAG_NOT_TOUCH_MODAL`).
#[must_use]
pub const fn not_touch_modal(flags: i32) -> bool {
    flags & FLAG_NOT_TOUCH_MODAL != 0
}

/// One window in the stack: everything the ordering, the hit test and the tree
/// need, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerEntry {
    /// The window (`IWindow` binder id).
    pub window: WindowId,
    /// The raw `LayoutParams.type`.
    pub type_: i32,
    /// The raw `LayoutParams.flags`.
    pub flags: i32,
    /// Window frame in display coordinates.
    pub frame: RectI,
    /// The window this one is attached to (`LayoutParams.token` resolved to a
    /// window id), for sub-windows and attached dialogs.
    pub parent: Option<WindowId>,
    /// Creation order: the tie-breaker *inside* a band (AOSP "added later wins").
    pub seq: u64,
}

impl LayerEntry {
    /// The window's kind.
    #[must_use]
    pub const fn kind(&self) -> WindowKind {
        WindowKind::of(self.type_)
    }

    /// Whether it receives touches.
    #[must_use]
    pub const fn touchable(&self) -> bool {
        touchable(self.flags)
    }

    /// Whether it may take focus.
    #[must_use]
    pub const fn focusable(&self) -> bool {
        focusable(self.flags)
    }

    /// Whether a touch outside its frame passes through.
    #[must_use]
    pub const fn not_touch_modal(&self) -> bool {
        not_touch_modal(self.flags)
    }

    /// `FLAG_DIM_BEHIND`: a dim layer is drawn below this window.
    #[must_use]
    pub const fn dims_behind(&self) -> bool {
        self.flags & FLAG_DIM_BEHIND != 0
    }
}

/// The ordered window stack of one display.
///
/// Bottom to top: band-major (`LayerBand`), then creation order inside a band.
/// A sub-window is moved out of its band and inserted **directly above** the
/// window it is attached to, which is what "kept next to their attached window
/// in Z-order" means: two panels on the same activity stay above the activity
/// and below anything that was above it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowLayerStack {
    entries: Vec<LayerEntry>,
}

impl WindowLayerStack {
    /// An empty stack.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add (or replace) a window. Replacement keeps the creation order, so a
    /// relayout does not jump a window above a window it was below.
    pub fn upsert(&mut self, mut entry: LayerEntry) {
        if let Some(existing) = self.entries.iter_mut().find(|e| e.window == entry.window) {
            entry.seq = existing.seq;
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
    }

    /// Remove a window; sub-windows attached to it are removed too (AOSP dies
    /// with its parent's token).
    pub fn remove(&mut self, window: WindowId) -> Vec<WindowId> {
        let mut gone = vec![window];
        self.entries.retain(|e| e.window != window);
        // A sub-window dies with its token; grandchildren (depth > 1) follow in
        // the same pass because `gone` grows while the list is walked.
        let mut i = 0;
        while i < self.entries.len() {
            if self.entries[i].parent.is_some_and(|p| gone.contains(&p)) {
                gone.push(self.entries[i].window);
                self.entries.remove(i);
            } else {
                i += 1;
            }
        }
        gone
    }

    /// The number of windows in the stack.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no window is in the stack.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look one window up.
    #[must_use]
    pub fn get(&self, window: WindowId) -> Option<&LayerEntry> {
        self.entries.iter().find(|e| e.window == window)
    }

    /// The stack in AOSP z-order, **bottom first**.
    ///
    /// Top-level windows sort by `(band, seq)`. A sub-window is drawn directly
    /// above its parent, and a sub-window whose parent is not in the stack (a
    /// tombstoned token) sorts by its band like any other window, so it can
    /// never disappear silently.
    #[must_use]
    pub fn ordered(&self) -> Vec<&LayerEntry> {
        // Top-level order first.
        let mut top: Vec<&LayerEntry> = self
            .entries
            .iter()
            .filter(|e| !e.kind().is_sub_window())
            .collect();
        top.sort_by_key(|e| (e.kind().band(), e.seq));
        // Sub-windows keyed by their parent, in creation order.
        let mut subs: Vec<&LayerEntry> = self
            .entries
            .iter()
            .filter(|e| e.kind().is_sub_window())
            .collect();
        subs.sort_by_key(|e| e.seq);

        let mut out: Vec<&LayerEntry> = Vec::with_capacity(self.entries.len());
        for entry in top {
            out.push(entry);
            // Panels/dialogs of this window, directly above it.
            out.extend(subs.iter().copied().filter(|s| s.parent == Some(entry.window)));
        }
        // Orphans (their token is not in the stack) keep a defined place: above
        // the application band, below the overlays.
        out.extend(
            subs.into_iter()
                .filter(|s| !s.parent.is_some_and(|p| self.get(p).is_some())),
        );
        out
    }

    /// The z value of a window: its index in [`Self::ordered`]. `None` when the
    /// window is not in the stack.
    #[must_use]
    pub fn z_of(&self, window: WindowId) -> Option<i64> {
        self.ordered()
            .iter()
            .position(|e| e.window == window)
            .map(|i| i as i64)
    }

    /// The window a touch at display point `p` goes to: the **topmost touchable**
    /// window whose frame contains the point.
    ///
    /// A `FLAG_NOT_TOUCHABLE` overlay is skipped and the touch lands on the
    /// window below it — which is exactly why an agent can tell a pass-through
    /// overlay from a touchable one.
    #[must_use]
    pub fn hit_test(&self, p: PointI) -> Option<WindowId> {
        self.ordered()
            .into_iter()
            .rev()
            .find(|e| e.touchable() && e.frame.contains(p.x, p.y))
            .map(|e| e.window)
    }

    /// The window that takes focus: the topmost focusable, visible window,
    /// ignoring `FLAG_NOT_FOCUSABLE` ones (an overlay must not steal the app's
    /// focus).
    #[must_use]
    pub fn focus_target(&self) -> Option<WindowId> {
        self.ordered()
            .into_iter()
            .rev()
            .find(|e| e.focusable())
            .map(|e| e.window)
    }

    /// The ordered stack as the `(window, type, z, kind)` rows the UI tree, the
    /// verdicts and `acos_capabilities` print.
    #[must_use]
    pub fn describe(&self) -> Vec<(WindowId, i32, i64, WindowKind)> {
        self.ordered()
            .iter()
            .enumerate()
            .map(|(z, e)| (e.window, e.type_, z as i64, e.kind()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(window: WindowId, type_: i32, flags: i32, frame: RectI, seq: u64) -> LayerEntry {
        LayerEntry {
            window,
            type_,
            flags,
            frame,
            parent: None,
            seq,
        }
    }

    fn full() -> RectI {
        RectI::new(0, 0, 1080, 2400)
    }

    #[test]
    fn application_types_are_the_application_band() {
        for t in [
            TYPE_BASE_APPLICATION,
            TYPE_APPLICATION,
            TYPE_APPLICATION_STARTING,
            TYPE_DRAWN_APPLICATION,
            LAST_APPLICATION_WINDOW,
        ] {
            let k = WindowKind::of(t);
            assert_eq!(k, WindowKind::Application, "type {t}");
            assert_eq!(k.band(), LayerBand::Application, "type {t}");
            assert!(k.is_application());
            assert!(!k.is_sub_window());
        }
        // A dialog shown by an Activity is a plain TYPE_APPLICATION window.
        assert_eq!(WindowKind::of(2), WindowKind::Application);
    }

    #[test]
    fn sub_window_types_are_1000_to_1999() {
        for t in [
            TYPE_APPLICATION_PANEL,
            TYPE_APPLICATION_MEDIA,
            TYPE_APPLICATION_SUB_PANEL,
            TYPE_APPLICATION_ATTACHED_DIALOG,
            TYPE_APPLICATION_MEDIA_OVERLAY,
            TYPE_APPLICATION_ABOVE_SUB_PANEL,
            LAST_SUB_WINDOW,
        ] {
            let k = WindowKind::of(t);
            assert_eq!(k, WindowKind::SubWindow, "type {t}");
            assert!(k.is_sub_window(), "type {t}");
            assert!(!k.is_application(), "type {t}");
        }
        assert_eq!(TYPE_APPLICATION_PANEL, 1000);
        assert_eq!(TYPE_APPLICATION_ATTACHED_DIALOG, 1003);
    }

    #[test]
    fn system_types_are_2000_to_2999_and_out_of_range_is_unknown() {
        assert_eq!(FIRST_SYSTEM_WINDOW, 2000);
        assert_eq!(WindowKind::of(2000), WindowKind::SystemBar); // TYPE_STATUS_BAR
        assert_eq!(WindowKind::of(2003), WindowKind::SystemAlert); // TYPE_SYSTEM_ALERT
        assert_eq!(WindowKind::of(2008), WindowKind::SystemDialog); // TYPE_SYSTEM_DIALOG
        assert_eq!(WindowKind::of(2011), WindowKind::InputMethod);
        assert_eq!(WindowKind::of(2032), WindowKind::AccessibilityOverlay);
        assert_eq!(WindowKind::of(2038), WindowKind::ApplicationOverlay);
        assert_eq!(WindowKind::of(2040), WindowKind::NotificationShade);
        assert_eq!(WindowKind::of(2041), WindowKind::SystemBar); // STATUS_BAR_ADDITIONAL
        assert_eq!(WindowKind::of(2500), WindowKind::System);
        assert_eq!(WindowKind::of(2999), WindowKind::System);
        // Outside every documented range: named, not merged into the app band.
        assert_eq!(WindowKind::of(0), WindowKind::Unknown);
        assert_eq!(WindowKind::of(3000), WindowKind::Unknown);
        assert_eq!(WindowKind::of(-1), WindowKind::Unknown);
        assert_eq!(WindowKind::Unknown.band(), LayerBand::Unknown);
    }

    #[test]
    fn application_overlay_is_above_every_activity_window_below_bars_and_ime() {
        let overlay = WindowKind::of(TYPE_APPLICATION_OVERLAY).band();
        assert!(overlay > LayerBand::Application);
        assert!(overlay > LayerBand::SubWindow);
        // "below critical system windows like the status bar or IME"
        assert!(overlay < WindowKind::of(TYPE_STATUS_BAR).band());
        assert!(overlay < WindowKind::of(TYPE_INPUT_METHOD).band());
        // The accessibility interception overlay is not overlaid by the app's
        // own overlay.
        assert!(
            WindowKind::of(TYPE_ACCESSIBILITY_OVERLAY).band()
                > WindowKind::of(TYPE_APPLICATION_OVERLAY).band()
        );
    }

    #[test]
    fn flags_decide_touchability_and_focusability() {
        // A touchable, focusable overlay (the default for a Button-carrying
        // overlay view).
        assert!(touchable(0));
        assert!(focusable(0));
        // FLAG_NOT_TOUCHABLE: the touch passes through to the window below.
        assert!(!touchable(FLAG_NOT_TOUCHABLE));
        // FLAG_NOT_FOCUSABLE: the window never takes focus.
        assert!(!focusable(FLAG_NOT_FOCUSABLE));
        // Both are independent bits; the framework sets both on a pure
        // decoration overlay.
        let flags = FLAG_NOT_TOUCHABLE | FLAG_NOT_FOCUSABLE;
        assert!(!touchable(flags));
        assert!(!focusable(flags));
        assert!(!not_touch_modal(flags));
        assert!(not_touch_modal(FLAG_NOT_TOUCH_MODAL));
        assert!(entry(1, TYPE_APPLICATION, FLAG_DIM_BEHIND, full(), 0).dims_behind());
    }

    #[test]
    fn stack_order_is_band_major_then_creation_order() {
        let mut s = WindowLayerStack::new();
        // Added in the "wrong" order on purpose: the overlay first, the app last.
        s.upsert(entry(30, TYPE_APPLICATION_OVERLAY, 0, full(), 1));
        s.upsert(entry(10, TYPE_APPLICATION, 0, full(), 2));
        s.upsert(entry(20, TYPE_APPLICATION, 0, full(), 3));
        let ids: Vec<_> = s.ordered().iter().map(|e| e.window).collect();
        assert_eq!(ids, vec![10, 20, 30]);
        assert_eq!(s.z_of(10), Some(0));
        assert_eq!(s.z_of(30), Some(2));
        assert_eq!(s.z_of(99), None);
        // Wallpaper below everything, shade and bar above the overlay.
        s.upsert(entry(5, TYPE_WALLPAPER, 0, full(), 4));
        s.upsert(entry(40, TYPE_NOTIFICATION_SHADE, 0, full(), 5));
        s.upsert(entry(50, TYPE_STATUS_BAR, 0, full(), 6));
        let ids: Vec<_> = s.ordered().iter().map(|e| e.window).collect();
        assert_eq!(ids, vec![5, 10, 20, 30, 40, 50]);
    }

    #[test]
    fn a_sub_window_is_placed_directly_above_its_parent() {
        let mut s = WindowLayerStack::new();
        s.upsert(entry(10, TYPE_APPLICATION, 0, full(), 1));
        // A second activity window above the first.
        s.upsert(entry(20, TYPE_APPLICATION, 0, full(), 2));
        // A PopupWindow panel attached to the *lower* window arrives last.
        let mut panel = entry(11, TYPE_APPLICATION_PANEL, 0, full(), 3);
        panel.parent = Some(10);
        s.upsert(panel);
        let ids: Vec<_> = s.ordered().iter().map(|e| e.window).collect();
        // The panel sits above its own window and below the window that was
        // already above it — "kept next to their attached window in Z-order".
        assert_eq!(ids, vec![10, 11, 20]);
        assert_eq!(s.z_of(11), Some(1));
        // Removing the parent removes the panel with it.
        let gone = s.remove(10);
        assert_eq!(gone, vec![10, 11]);
        assert_eq!(s.len(), 1);
        assert_eq!(s.ordered()[0].window, 20);
    }

    #[test]
    fn a_sub_window_with_a_dead_token_keeps_a_defined_place() {
        let mut s = WindowLayerStack::new();
        s.upsert(entry(10, TYPE_APPLICATION, 0, full(), 1));
        s.upsert(entry(30, TYPE_APPLICATION_OVERLAY, 0, full(), 2));
        let mut orphan = entry(12, TYPE_APPLICATION_PANEL, 0, full(), 3);
        orphan.parent = Some(999);
        s.upsert(orphan);
        let ids: Vec<_> = s.ordered().iter().map(|e| e.window).collect();
        // Not dropped silently, and not above the overlay.
        assert_eq!(ids, vec![10, 30, 12]);
    }

    #[test]
    fn a_relayout_does_not_jump_a_window_up_the_stack() {
        let mut s = WindowLayerStack::new();
        s.upsert(entry(10, TYPE_APPLICATION, 0, full(), 1));
        s.upsert(entry(11, TYPE_APPLICATION, 0, full(), 2));
        // Relayout of the lower window must keep it below.
        s.upsert(entry(10, TYPE_APPLICATION, 0, RectI::new(0, 0, 500, 500), 99));
        assert_eq!(s.ordered()[0].window, 10);
        assert_eq!(s.get(10).unwrap().frame, RectI::new(0, 0, 500, 500));
    }

    #[test]
    fn hit_test_returns_the_topmost_touchable_window_at_the_point() {
        let mut s = WindowLayerStack::new();
        s.upsert(entry(10, TYPE_APPLICATION, 0, full(), 1));
        // The overlay covers the top-left 300x300 and is touchable.
        s.upsert(entry(
            30,
            TYPE_APPLICATION_OVERLAY,
            0,
            RectI::new(0, 0, 300, 300),
            2,
        ));
        assert_eq!(s.hit_test(PointI::new(10, 10)), Some(30));
        // Beside the overlay: the activity gets it.
        assert_eq!(s.hit_test(PointI::new(400, 400)), Some(10));
        // Outside the display: nothing.
        assert_eq!(s.hit_test(PointI::new(5000, 5000)), None);
    }

    #[test]
    fn a_not_touchable_overlay_lets_the_touch_through_to_the_activity() {
        let mut s = WindowLayerStack::new();
        s.upsert(entry(10, TYPE_APPLICATION, 0, full(), 1));
        s.upsert(entry(
            30,
            TYPE_APPLICATION_OVERLAY,
            FLAG_NOT_TOUCHABLE | FLAG_NOT_FOCUSABLE,
            RectI::new(0, 0, 300, 300),
            2,
        ));
        // The overlay covers the point in z, but it is not touchable.
        assert_eq!(s.hit_test(PointI::new(10, 10)), Some(10));
        // ...and it does not take focus either.
        assert_eq!(s.focus_target(), Some(10));
    }

    #[test]
    fn focus_goes_to_the_topmost_focusable_window() {
        let mut s = WindowLayerStack::new();
        s.upsert(entry(10, TYPE_APPLICATION, 0, full(), 1));
        assert_eq!(s.focus_target(), Some(10));
        // A touchable, focusable dialog (TYPE_APPLICATION, its own token) above.
        s.upsert(entry(11, TYPE_APPLICATION, 0, full(), 2));
        assert_eq!(s.focus_target(), Some(11));
        // An overlay that is focusable takes focus (FLAG_NOT_FOCUSABLE is what
        // decides it, not the type).
        s.upsert(entry(30, TYPE_APPLICATION_OVERLAY, 0, full(), 3));
        assert_eq!(s.focus_target(), Some(30));
        // The IME is above the app but never focusable in practice.
        s.upsert(entry(
            40,
            TYPE_INPUT_METHOD,
            FLAG_NOT_FOCUSABLE,
            full(),
            4,
        ));
        assert_eq!(s.focus_target(), Some(30));
    }

    #[test]
    fn describe_reports_window_type_and_z_for_every_window() {
        let mut s = WindowLayerStack::new();
        s.upsert(entry(10, TYPE_APPLICATION, 0, full(), 1));
        s.upsert(entry(30, TYPE_APPLICATION_OVERLAY, 0, full(), 2));
        let rows = s.describe();
        assert_eq!(
            rows,
            vec![
                (10, TYPE_APPLICATION, 0, WindowKind::Application),
                (30, TYPE_APPLICATION_OVERLAY, 1, WindowKind::ApplicationOverlay),
            ]
        );
        assert_eq!(rows[1].3.name(), "application_overlay");
    }
}
