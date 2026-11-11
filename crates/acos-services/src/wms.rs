//! `svc.wms`: the service side the app talks to for `IWindowManager` /
//! `IWindowSession` (BACKLOG T-29).
//!
//! The VM-independent window policy lives in `acos-wm` (`crates/acos-wm`); this
//! module is the **service-object** face of it:
//!
//! * [`WmsState`] owns one `acos_wm::wms::WindowManager` plus the session the
//!   app's `IWindowSession` binder maps to, and exposes the typed operations
//!   (`open_session`, `add_to_display_as_user`, `relayout`, `finish_drawing`,
//!   `set_insets`, `update_requested_visible_types`, …).
//! * [`WmsService`] is the lazily-constructed [`ServiceObject`] the
//!   `IServiceManager` name `window` resolves to and the direct handler for the
//!   scalar `IWindowManager` methods (`useBLAST`, `hasNavigationBar`,
//!   `isInTouchMode`, `setInTouchMode`, `getCurrentAnimatorScale`).
//!
//! # Why the parcelables are written by `acos-runtime`, not here
//!
//! `addToDisplayAsUser`/`relayout` carry `WindowManager$LayoutParams` in and
//! `ClientWindowFrames` / `MergedConfiguration` / `SurfaceControl` /
//! `InsetsState` / `InsetsSourceControl$Array` out. Their byte layouts are
//! already transcribed from the framework `writeToParcel` in
//! [`acos_wm::codec`] (see `results/wm/parcelable_layouts.txt`); duplicating
//! them in the flat `FieldOp` engine here would be a second, weaker source of
//! truth (the engine cannot express inline nested payloads such as
//! `MergedConfiguration`'s three raw `Configuration`s). The Binder transport
//! therefore routes the two WMS descriptors to
//! `acos_runtime::wms_java`, which decodes/encodes with those codecs and calls
//! straight into [`WmsState`]. The scalar `IWindowManager` methods are answered
//! here too so the dispatcher has a real handler for them.
//!
//! State is keyed per session exactly as `acos-wm` does; the reference device is
//! the hello-views geometry (1080x2400, density 420, status bar 128, nav 63).

use std::sync::{Arc, Mutex};

use acos_parcel::binder::BinderId;
use acos_wm::codec::LayoutParams;
use acos_wm::display::DisplaySpec;
use acos_wm::error::{Result as WmResult, WmError};
use acos_wm::wms::{AddWindowResult, RelayoutResult, SessionId, WindowId, WindowManager};

use acos_aidl::MethodInfo;

use crate::host::{CallResult, ServiceObject, Wiring};
use crate::value::Value;

/// Extra state the launch-time `IWindowManager` calls need but `acos-wm` does
/// not model (they are device facts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayFacts {
    /// `getInitialDisplaySize` width.
    pub width: i32,
    /// `getInitialDisplaySize` height.
    pub height: i32,
    /// `getInitialDisplayDensity`/`getBaseDisplayDensity`.
    pub density_dpi: i32,
    /// `hasNavigationBar`.
    pub has_navigation_bar: bool,
    /// `getDisplayImePolicy` (`DISPLAY_IME_POLICY_LOCAL`).
    pub ime_policy: i32,
}

impl DisplayFacts {
    /// The reference hello-views device, with any active display override
    /// applied so `getInitialDisplaySize` / `getBaseDisplayDensity` follow
    /// `acos launch --density` / `--display` / `--reference`.
    #[must_use]
    pub fn reference() -> Self {
        if let Some(o) = crate::display::display_override() {
            return Self {
                width: o.width,
                height: o.height,
                density_dpi: o.density_dpi,
                has_navigation_bar: o.nav_bar_bottom > 0,
                ime_policy: 0,
            };
        }
        Self {
            width: acos_wm::display::DISPLAY_WIDTH,
            height: acos_wm::display::DISPLAY_HEIGHT,
            density_dpi: acos_wm::display::DENSITY_DPI as i32,
            has_navigation_bar: true,
            ime_policy: 0,
        }
    }
}

/// The window-manager state machine plus the session a single app process uses.
#[derive(Debug)]
pub struct WmsState {
    /// The `acos-wm` state machine.
    pub wm: WindowManager,
    /// Device facts not owned by `acos-wm`.
    pub facts: DisplayFacts,
    /// Session created for the app's `IWindowSession`.
    pub session: SessionId,
    /// The synthetic `IWindowSession` binder returned by `openSession`.
    pub session_binder: BinderId,
    /// Number of `finishDrawing` calls recorded.
    pub finish_drawing_count: u64,
    /// Number of real buffer submissions published through the compositor
    /// backend. `finishDrawing` alone never increments this.
    pub frames_published: u64,
    /// Service-side input dispatcher (T-36). The runtime installs the
    /// per-window frame writers; this state owns hit-testing, sequencing and
    /// ack accounting.
    pub input: crate::input::InputDispatcher,
}

impl Default for WmsState {
    fn default() -> Self {
        Self::new()
    }
}

impl WmsState {
    /// A state machine on the reference display with one open session.
    #[must_use]
    pub fn new() -> Self {
        let display = match crate::display::display_override() {
            Some(o) => DisplaySpec::custom(
                o.width,
                o.height,
                o.density_dpi as u32,
                o.status_bar_top,
                o.nav_bar_bottom,
            ),
            None => DisplaySpec::reference_1080x2400(),
        };
        let mut wm = WindowManager::new(display);
        let (session, session_binder) = wm.open_session(None);
        Self {
            wm,
            facts: DisplayFacts::reference(),
            session,
            session_binder,
            finish_drawing_count: 0,
            frames_published: 0,
            input: crate::input::InputDispatcher::new(),
        }
    }

    /// `IWindowManager.openSession`: allocate a fresh session and return its
    /// `IWindowSession` binder.
    pub fn open_session(&mut self) -> BinderId {
        let (session, binder) = self.wm.open_session(None);
        self.session = session;
        self.session_binder = binder;
        binder
    }

    /// `IWindowSession.addToDisplayAsUser`.
    pub fn add_to_display_as_user(
        &mut self,
        window: BinderId,
        params: LayoutParams,
        requested_visible_types: i32,
    ) -> WmResult<AddWindowResult> {
        self.wm
            .add_to_display_as_user(self.session, window, params, requested_visible_types)
    }

    /// `IWindowSession.relayout` without a client-measured size.
    pub fn relayout(
        &mut self,
        window: WindowId,
        params: LayoutParams,
        sync_seq: i32,
    ) -> WmResult<RelayoutResult> {
        self.relayout_with_requested(window, params, -1, -1, sync_seq)
    }

    /// `IWindowSession.relayout` with the client-reported measured size.
    ///
    /// The input dispatcher's routing frame is kept consistent with the frame
    /// the client lays out in (the real WMS frame), so a `WRAP_CONTENT` dialog
    /// whose frame is the measured size still hit-tests its buttons.
    pub fn relayout_with_requested(
        &mut self,
        window: WindowId,
        params: LayoutParams,
        requested_width: i32,
        requested_height: i32,
        sync_seq: i32,
    ) -> WmResult<RelayoutResult> {
        let r = self.wm.relayout_with_requested(
            self.session,
            window,
            params,
            requested_width,
            requested_height,
            sync_seq,
        )?;
        self.input.update_frame(window, r.frames.frame);
        Ok(r)
    }

    /// `IWindowSession.remove`.
    pub fn remove(&mut self, window: WindowId) -> WmResult<()> {
        self.wm.remove(window)?;
        self.input.remove_window(window);
        Ok(())
    }

    /// `IWindowSession.finishDrawing`: records the drawing callback. It does
    /// **not** publish a frame; only a real compositor buffer submission does
    /// (see `acos-runtime`'s `WmSurfaceBackend`).
    pub fn finish_drawing(&mut self, window: WindowId, seq: i32) -> WmResult<()> {
        self.wm.finish_drawing(window, seq)?;
        self.finish_drawing_count += 1;
        Ok(())
    }

    /// Record a real buffer submission (called by the compositor backend).
    pub fn note_buffer_submitted(&mut self) {
        self.frames_published += 1;
    }

    /// `IWindowSession.setInsets` (oneway).
    pub fn set_insets(
        &mut self,
        window: WindowId,
        touchable_region: Option<acos_wm::geom::RectI>,
        content_insets: Option<acos_wm::geom::RectI>,
    ) -> WmResult<()> {
        self.wm.set_insets(window, touchable_region, content_insets)
    }

    /// `IWindowSession.updateRequestedVisibleTypes` (oneway).
    pub fn update_requested_visible_types(
        &mut self,
        window: WindowId,
        types: i32,
    ) -> WmResult<()> {
        self.wm.update_requested_visible_types(window, types)
    }

    /// `IWindowSession.reportSystemGestureExclusionChanged` (oneway; recorded by
    /// validating that the window exists).
    pub fn report_system_gesture_exclusion_changed(&mut self, window: WindowId) -> WmResult<()> {
        self.wm
            .windows
            .get_mut(&window)
            .ok_or(WmError::UnknownWindow(window))
            .map(|_| ())
    }

    /// `IWindowSession.pokeDrawLock` (oneway at the Java level; recorded).
    pub fn poke_draw_lock(&mut self, window: WindowId) -> WmResult<()> {
        self.wm
            .windows
            .get_mut(&window)
            .ok_or(WmError::UnknownWindow(window))
            .map(|_| ())
    }

    /// `IWindowSession.onRectangleOnScreenRequested` (oneway).
    pub fn on_rectangle_on_screen_requested(
        &mut self,
        window: WindowId,
        rect: Option<acos_wm::geom::RectI>,
    ) -> WmResult<()> {
        self.wm.on_rectangle_on_screen_requested(window, rect)
    }

    /// `IWindowManager.useBLAST`.
    #[must_use]
    pub const fn use_blast(&self) -> bool {
        true
    }

    /// `IWindowManager.getCurrentAnimatorScale`.
    #[must_use]
    pub const fn current_animator_scale(&self) -> f32 {
        1.0
    }

    /// `IWindowManager.hasNavigationBar`.
    #[must_use]
    pub fn has_navigation_bar(&self, display_id: i32) -> bool {
        display_id == self.wm.display.id && self.facts.has_navigation_bar
    }

    /// `IWindowManager.isInTouchMode`.
    #[must_use]
    pub fn is_in_touch_mode(&self, display_id: i32) -> bool {
        self.wm.is_in_touch_mode(display_id)
    }

    /// `IWindowManager.setInTouchMode`.
    pub fn set_in_touch_mode(&mut self, in_touch_mode: bool, display_id: i32) {
        self.wm.set_in_touch_mode(in_touch_mode, display_id);
    }
}

/// The lazily-constructed service object for the `window` service.
///
/// It answers the scalar `IWindowManager` methods itself and leaves the
/// `IWindowSession`/parcelable path to `acos_runtime::wms_java` (which has the
/// VM-facing codecs). Direct dispatch of an `IWindowSession` method here is a
/// structured `UNSUPPORTED_ANDROID_API`, never a default success.
pub struct WmsService {
    state: Arc<Mutex<WmsState>>,
    _binder: BinderId,
}

impl std::fmt::Debug for WmsService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WmsService")
            .field("state", &self.state.lock().map(|s| s.facts))
            .finish()
    }
}

impl WmsService {
    /// Build the service around shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<WmsState>>, wiring: Wiring) -> Self {
        Self {
            state,
            _binder: wiring.self_binder,
        }
    }

    /// The shared state (for the runtime that owns the wire codecs).
    #[must_use]
    pub fn shared_state(&self) -> Arc<Mutex<WmsState>> {
        Arc::clone(&self.state)
    }

    fn unsupported(method: &MethodInfo) -> crate::error::ServiceError {
        crate::error::ServiceError::UnsupportedAndroidApi {
            interface: "android.view.IWindowSession".to_string(),
            code: method.code,
            method: method.name.clone(),
        }
    }
}

impl ServiceObject for WmsService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        args: &[Value],
    ) -> crate::error::Result<CallResult> {
        if interface != "android.view.IWindowManager" {
            return Err(Self::unsupported(method));
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let reply = match method.name.as_str() {
            "useBLAST" => vec![Value::Bool(state.use_blast())],
            "getCurrentAnimatorScale" => vec![Value::Float(state.current_animator_scale())],
            "hasNavigationBar" => {
                let display = args.first().and_then(Value::as_int).unwrap_or(0);
                vec![Value::Bool(state.has_navigation_bar(display))]
            }
            "isInTouchMode" => {
                let display = args.first().and_then(Value::as_int).unwrap_or(0);
                vec![Value::Bool(state.is_in_touch_mode(display))]
            }
            "setInTouchMode" => {
                let on = args.first().and_then(Value::as_bool).unwrap_or(false);
                let display = args.get(1).and_then(Value::as_int).unwrap_or(0);
                state.set_in_touch_mode(on, display);
                vec![]
            }
            // The parcelable-bearing methods are served by `wms_java`.
            _ => return Err(Self::unsupported(method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

/// Add the `window` service name and a `WmsService` entry to the standard host.
///
/// Kept in this module so `registry` only needs the one-line call; the shared
/// state is returned so the runtime's transport can drive the same object.
pub fn install(host: &mut crate::host::ServiceHost) -> Arc<Mutex<WmsState>> {
    let state = Arc::new(Mutex::new(WmsState::new()));
    let factory_state = Arc::clone(&state);
    host.register(
        "window",
        "android.view.IWindowManager",
        move |w| Box::new(WmsService::new(Arc::clone(&factory_state), w)) as Box<dyn ServiceObject>,
    );
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_wm::codec::inset_type;
    use acos_wm::geom::RectI;
    use acos_wm::wms::{ADD_BAD_APP_TOKEN, ADD_OKAY, RELAYOUT_RES_FIRST_TIME};

    fn app_params() -> LayoutParams {
        LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity")
    }

    #[test]
    fn add_relayout_and_finish_drawing_are_recorded() {
        let mut st = WmsState::new();
        let add = st
            .add_to_display_as_user(BinderId(0x11), app_params(), 0)
            .unwrap();
        assert_eq!(add.result, ADD_OKAY);
        assert_eq!(add.out_frame, RectI::new(0, 128, 1080, 2337));
        assert_eq!(
            add.insets_state.visible_insets_for(inset_type::STATUS_BAR).top,
            128,
            "status bar inset"
        );
        assert_eq!(
            add.insets_state
                .visible_insets_for(inset_type::NAVIGATION_BAR)
                .bottom,
            63,
            "nav bar inset"
        );

        let first = st.relayout(add.window, app_params(), -1).unwrap();
        let second = st.relayout(add.window, app_params(), -1).unwrap();
        assert_eq!(first.frames.frame, second.frames.frame);
        assert_eq!(first.surface.handle, second.surface.handle);
        assert_ne!(first.result & RELAYOUT_RES_FIRST_TIME, 0);
        assert_eq!(second.result & RELAYOUT_RES_FIRST_TIME, 0);

        assert_eq!(st.frames_published, 0);
        st.finish_drawing(add.window, 0).unwrap();
        assert_eq!(st.finish_drawing_count, 1);
        assert_eq!(
            st.frames_published, 0,
            "finishDrawing alone must not publish a frame"
        );
        st.note_buffer_submitted();
        assert_eq!(st.frames_published, 1);
    }

    #[test]
    fn open_session_returns_a_fresh_session_binder() {
        let mut st = WmsState::new();
        let a = st.session_binder;
        let b = st.open_session();
        assert_ne!(a, b);
        assert_eq!(st.session_binder, b);
    }

    #[test]
    fn duplicate_add_is_structured() {
        let mut st = WmsState::new();
        st.add_to_display_as_user(BinderId(7), app_params(), 0)
            .unwrap();
        let dup = st
            .add_to_display_as_user(BinderId(7), app_params(), 0)
            .unwrap();
        assert_eq!(dup.result, acos_wm::wms::ADD_DUPLICATE_ADD);
        assert!(matches!(
            st.relayout(999, app_params(), -1),
            Err(WmError::UnknownWindow(999))
        ));
    }

    #[test]
    fn null_window_is_bad_app_token() {
        let mut st = WmsState::new();
        let r = st
            .add_to_display_as_user(BinderId(0), app_params(), 0)
            .unwrap();
        assert_eq!(r.result, ADD_BAD_APP_TOKEN);
    }

    #[test]
    fn scalar_window_manager_facts() {
        let st = WmsState::new();
        assert!(st.use_blast());
        assert_eq!(st.current_animator_scale(), 1.0);
        assert!(st.has_navigation_bar(0));
        assert!(st.is_in_touch_mode(0));
    }
}
