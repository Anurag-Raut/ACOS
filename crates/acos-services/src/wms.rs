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
use acos_wm::codec::{LayoutParams, SurfaceControlInfo};
use acos_wm::display::DisplaySpec;
/// W7: re-export the app-window inset selector so every engine
/// (`acos-art launch`, the daemon, the verifier) spells it the same way.
pub use acos_wm::display::{InsetsMode, DEFAULT_INSETS_MODE, INSETS_ENV};
use acos_wm::error::{Result as WmResult, WmError};
use acos_wm::wms::{AddWindowResult, RelayoutResult, SessionId, WindowId, WindowManager};

/// Parse an `ACOS_WM_INSETS` value (`carve` / `report`).
#[must_use]
pub fn parse_insets_mode(value: &str) -> Option<InsetsMode> {
    InsetsMode::parse(value)
}

/// W47: the stock `Configuration.screenLayout` for the reference display;
/// `acos-art` forwards it to the launch guest (`ACOSLAUNCH_SCREEN_LAYOUT`).
pub use acos_wm::wms::REFERENCE_SCREEN_LAYOUT;

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

/// A real compositor layer bound to one `acos-wm` window surface handle (W5).
///
/// `acos-wm` allocates a *synthetic* handle per window (`0x1000 + n`) which is
/// not a binder. This record maps it to the layer `surface_client` issued, so
/// the `SurfaceControl` that goes on the wire carries the compositor's client
/// node, a registered layer-handle binder and the compositor's layer id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceLayerBinding {
    /// The layer-handle binder `createSurface` issued (what a transaction names).
    pub handle: BinderId,
    /// `CreateSurfaceResult.layerId`.
    pub layer_id: u64,
}

/// One `IWindowSession` of this host (stock `com.android.server.wm.Session`,
/// `WindowManagerService.openSession`).
///
/// Stock creates a **new** session object inside `openSession` and answers with
/// its `IWindowSession` binder, so every app process talks to its own node and
/// every window operation is validated against the session the call arrived on.
/// MPROC5b measured what a single shared node does: proc-b's `openSession`
/// overwrote the one session id and proc-a's next `relayout` was refused
/// (`unknown session 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionEntry {
    /// The `acos-wm` session id this node serves.
    pub id: SessionId,
    /// The `ServiceHost` node the `openSession` reply carried.
    pub binder: BinderId,
    /// The app process the session's `IWindow` binders belong to.
    ///
    /// Stock stores `Session.mClientPid` from `Binder.getCallingPid()` at
    /// `openSession`. ACOS has no caller identity on the host yet (the queued
    /// BINDERPID track), so this is the process the host recorded as the owner
    /// of the `IWindow` binder the session added — the same process by
    /// construction, and never a guess. `None` until the first window is added.
    pub process: Option<u32>,
}

/// The window-manager state machine plus every open `IWindowSession`.
#[derive(Debug)]
pub struct WmsState {
    /// The `acos-wm` state machine.
    pub wm: WindowManager,
    /// Device facts not owned by `acos-wm`.
    pub facts: DisplayFacts,
    /// Every open session, by `acos-wm` session id.
    pub sessions: std::collections::HashMap<SessionId, SessionEntry>,
    /// The **VM transport's** current session.
    ///
    /// `acos-runtime`'s `wms_java` routes `IWindowSession` transactions by
    /// descriptor (one guest per VM, no per-session binder identity on that
    /// transport), so it needs one name for "the session the app is using".
    /// The persistent host never reads this field: each session node passes its
    /// own id to every operation (MPROC5c).
    pub current: SessionId,
    /// The compositor registry, when one is installed. A session death destroys
    /// its windows' layers through it (W42: the layer *is* the window's pixels).
    pub compositor: Option<Arc<Mutex<crate::surface_client::SurfaceState>>>,
    /// Number of `finishDrawing` calls recorded.
    pub finish_drawing_count: u64,
    /// Number of real buffer submissions published through the compositor
    /// backend. `finishDrawing` alone never increments this.
    pub frames_published: u64,
    /// Service-side input dispatcher (T-36). The runtime installs the
    /// per-window frame writers; this state owns hit-testing, sequencing and
    /// ack accounting.
    pub input: crate::input::InputDispatcher,
    /// The real compositor layer behind each `acos-wm` window surface handle
    /// (W5). Populated on the first `relayout` of a window.
    pub surface_layers: std::collections::HashMap<u64, SurfaceLayerBinding>,
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
        Self::new_with_insets(InsetsMode::from_env(acos_wm::display::DEFAULT_INSETS_MODE))
    }

    /// W7b: [`Self::new`] with an explicit app-window inset model.
    ///
    /// Tests and replay drivers use this so their frame expectations do not
    /// depend on the caller's `ACOS_WM_INSETS`.
    #[must_use]
    pub fn new_with_insets(insets_mode: InsetsMode) -> Self {
        let display = match crate::display::display_override() {
            Some(o) => DisplaySpec::custom_with_insets(
                o.width,
                o.height,
                o.density_dpi as u32,
                o.status_bar_top,
                o.nav_bar_bottom,
                insets_mode,
            ),
            None => DisplaySpec::reference_with_insets(insets_mode),
        };
        let mut wm = WindowManager::new(display);
        let (session, session_binder) = wm.open_session(None);
        let mut sessions = std::collections::HashMap::new();
        sessions.insert(
            session,
            SessionEntry {
                id: session,
                binder: session_binder,
                process: None,
            },
        );
        Self {
            wm,
            facts: DisplayFacts::reference(),
            sessions,
            current: session,
            compositor: None,
            finish_drawing_count: 0,
            frames_published: 0,
            input: crate::input::InputDispatcher::new(),
            surface_layers: std::collections::HashMap::new(),
        }
    }

    /// Install the compositor registry so a session death can destroy its
    /// windows' layers (the `registry` install path does this).
    pub fn set_compositor(&mut self, compositor: Arc<Mutex<crate::surface_client::SurfaceState>>) {
        self.compositor = Some(compositor);
    }

    /// The VM transport's current session (see [`WmsState::current`]).
    #[must_use]
    pub fn current_session(&self) -> SessionId {
        self.current
    }

    /// The `IWindowSession` binder of the VM transport's current session.
    #[must_use]
    pub fn session_binder(&self) -> BinderId {
        self.sessions
            .get(&self.current)
            .map(|entry| entry.binder)
            .unwrap_or(BinderId(0))
    }

    /// Open a session for the **VM transport** and return its synthetic binder.
    pub fn open_session(&mut self) -> BinderId {
        let (session, binder) = self.wm.open_session(None);
        self.sessions.insert(
            session,
            SessionEntry {
                id: session,
                binder,
                process: None,
            },
        );
        self.current = session;
        binder
    }

    /// Allocate a session without a host node yet (the persistent host's
    /// `IWindowManager.openSession` registers one per call).
    pub fn allocate_session(&mut self) -> SessionId {
        let (session, binder) = self.wm.open_session(None);
        self.sessions.insert(
            session,
            SessionEntry {
                id: session,
                binder,
                process: None,
            },
        );
        session
    }

    /// Record the host node that serves `session` (the binder the `openSession`
    /// reply carries).
    pub fn attach_host_node(&mut self, session: SessionId, binder: BinderId) {
        if let Some(entry) = self.sessions.get_mut(&session) {
            entry.binder = binder;
        }
    }

    /// Record which app process `session` belongs to (the owner of its first
    /// `IWindow`). Ignored for an unknown session.
    pub fn record_session_process(&mut self, session: SessionId, process: u32) {
        if let Some(entry) = self.sessions.get_mut(&session) {
            entry.process.get_or_insert(process);
        }
    }

    /// The session that owns `window`, from the window records.
    #[must_use]
    pub fn session_of_window(&self, window: WindowId) -> Option<SessionId> {
        self.wm.window_session(window)
    }

    /// Refuse an operation whose window belongs to **another** session.
    ///
    /// This is the stock check (`WindowManagerService.windowForClientLocked`
    /// resolves the window through the calling `Session`): a `relayout` that
    /// arrives on proc-b's session node for proc-a's window is
    /// `unknown session`, exactly like the image's `IllegalArgumentException`.
    fn check_session_window(&self, session: SessionId, window: WindowId) -> WmResult<()> {
        match self.wm.window_session(window) {
            Some(owner) if owner == session => Ok(()),
            Some(_) => Err(WmError::UnknownSession(session)),
            None => Err(WmError::UnknownWindow(window)),
        }
    }

    /// The windows one session owns, in creation order.
    #[must_use]
    pub fn session_windows(&self, session: SessionId) -> Vec<WindowId> {
        self.wm
            .sessions
            .get(&session)
            .map(|record| record.windows.clone())
            .unwrap_or_default()
    }

    /// Session death (stock `Session.kill`): drop the session, its windows, and
    /// their compositor layers.
    ///
    /// Returns the host node that must be unregistered and the windows dropped.
    /// Unknown session: `None` (a second death report is not an error).
    pub fn close_session(&mut self, session: SessionId) -> Option<(BinderId, Vec<WindowId>)> {
        let entry = self.sessions.remove(&session)?;
        for window in self.session_windows(session) {
            if let Some(compositor) = self.compositor.clone() {
                self.destroy_surface_layer(window, &compositor);
            }
            self.input.remove_window(window);
        }
        let windows = self.wm.close_session(session);
        if self.current == session {
            // The VM transport lost its session: fall back to any other open
            // session so a later call is refused per-window, not by id.
            self.current = self.sessions.keys().copied().next().unwrap_or(u64::MAX);
        }
        Some((entry.binder, windows))
    }

    /// Every session recorded as belonging to process `tag`.
    #[must_use]
    pub fn sessions_of_process(&self, tag: u32) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self
            .sessions
            .values()
            .filter(|entry| entry.process == Some(tag))
            .map(|entry| entry.id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Whether process `tag` owns a **visible** window.
    ///
    /// MPROC5c's idle kill reads this: a process whose window is on screen is
    /// not idle, whatever its activity records say (the window is the surface
    /// the user can see).
    #[must_use]
    pub fn process_has_visible_window(&self, tag: u32) -> bool {
        self.sessions.values().any(|entry| {
            entry.process == Some(tag)
                && self
                    .session_windows(entry.id)
                    .iter()
                    .any(|w| self.wm.windows.get(w).is_some_and(|record| record.visible))
        })
    }

    /// Session death for a process: every session whose window binders the
    /// process owned, plus every session whose recorded process tag is `tag`
    /// (a session that never added a window is still the process's).
    ///
    /// Returns the host nodes to unregister, one per closed session.
    pub fn close_process_sessions(
        &mut self,
        tag: u32,
        window_binders: &[BinderId],
        registrar_owners: impl Fn(BinderId) -> Option<u32>,
    ) -> Vec<(SessionId, BinderId, Vec<WindowId>)> {
        let mut victims: Vec<(SessionId, Option<u32>)> = Vec::new();
        for entry in self.sessions.values() {
            let owns_window = self
                .session_windows(entry.id)
                .iter()
                .any(|w| window_binders.contains(&BinderId(*w)));
            let process = entry.process.or_else(|| {
                self.session_windows(entry.id)
                    .first()
                    .and_then(|w| registrar_owners(BinderId(*w)))
            });
            if (process == Some(tag) || owns_window) && process.is_some() {
                victims.push((entry.id, process));
            }
        }
        victims.sort_unstable();
        let mut closed = Vec::new();
        for (session, _) in victims {
            if let Some((binder, windows)) = self.close_session(session) {
                closed.push((session, binder, windows));
            }
        }
        closed
    }

    /// ROT3: rotate the display to `rotation` (0..3, a target rotation).
    ///
    /// The WMS half of a display rotation: `DisplaySpec::rotated` swaps the
    /// frame, moves the bars (AOSP `DisplayPolicy` puts the status bar on the
    /// top edge of the *rotated* display) and re-derives app bounds/insets, so
    /// every later `relayout` reply carries the new frame and the new
    /// `MergedConfiguration`.
    pub fn rotate(&mut self, rotation: u8) -> DisplaySpec {
        let display = self.wm.display.rotated(rotation);
        self.wm.display = display;
        self.facts.width = display.frame.width();
        self.facts.height = display.frame.height();
        display
    }

    /// Bind `surface` to a real layer in the compositor-client registry and
    /// rewrite the control so the guest's `SurfaceControl` carries that
    /// compositor's client binder, layer-handle binder and layer id (W5).
    ///
    /// Idempotent per `acos-wm` surface handle: repeated relayouts of the same
    /// window reuse one layer (the handle pool is bounded at
    /// [`crate::surface_client::LAYER_HANDLE_POOL`]). Returns `None` when the
    /// pool is exhausted or the control has no handle; the caller then leaves
    /// the W3 placeholder body in place rather than fabricating a layer.
    pub fn bind_surface_layer(
        &mut self,
        surface: &mut SurfaceControlInfo,
        compositor: &Arc<Mutex<crate::surface_client::SurfaceState>>,
    ) -> Option<SurfaceLayerBinding> {
        let wm_handle = surface.handle?;
        let client = {
            let state = compositor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.client_binder
        };
        if client.is_null() {
            // No `ISurfaceComposerClient` node installed: the placeholder is
            // the honest answer (W3), and no layer can be created.
            return None;
        }
        let binding = if let Some(binding) = self.surface_layers.get(&wm_handle).copied() {
            binding
        } else {
            let mut state = compositor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (handle, layer_id) = state.create_layer(
                &surface.name,
                surface.width.max(0).unsigned_abs(),
                surface.height.max(0).unsigned_abs(),
                1, // PIXEL_FORMAT_RGBA_8888, the format the control writes
                0,
                None,
            )?;
            drop(state);
            let binding = SurfaceLayerBinding {
                handle,
                layer_id: layer_id.unsigned_abs().into(),
            };
            self.surface_layers.insert(wm_handle, binding);
            binding
        };
        surface.client = Some(client);
        surface.layer_binder = Some(binding.handle);
        surface.handle = Some(binding.layer_id);
        Some(binding)
    }

    /// Push a window's z-order, frame origin and dim amount into the compositor
    /// registry (**W35**).
    ///
    /// Those three values are the *window manager's*: the guest never puts a
    /// window frame, a z-order against other windows or a dim layer on the
    /// layer wire (`layer_state_t` has x/y/z but not the frame origin, and there
    /// is no dim in `gui::WindowInfo` beyond `FLAG_DIM_BEHIND` in
    /// `layoutParamsFlags` — the dim layer is `mDimLayer`, which the real
    /// `WindowManagerService` owns). The C1 composite therefore reads them from
    /// here: on every `relayout`, so a dialog that appears, moves or dims is
    /// what the next captured frame shows.
    ///
    /// `false` when the window or the layer is unknown (never a fabricated
    /// placement).
    pub fn publish_layer_placement(
        &self,
        binding: SurfaceLayerBinding,
        window: WindowId,
        compositor: &Arc<Mutex<crate::surface_client::SurfaceState>>,
    ) -> bool {
        let Some(record) = self.wm.windows.get(&window) else {
            return false;
        };
        let dim_amount = if record.params.flags & crate::surface_client::FLAG_DIM_BEHIND != 0 {
            record.params.dim_amount.clamp(0.0, 1.0)
        } else {
            0.0
        };
        // W54: the layer is the window's *surface*, which stock sizes as
        // `frame + surfaceInsets` and positions at `frame − surfaceInsets`
        // (`WindowState.getSurfaceWidth/Height` + `setSurfaceBoundariesLocked`;
        // `SurfaceView.java:1180` converts a frame into this space the same
        // way). The client draws its content at the inset offset inside that
        // larger buffer, so dropping the offset draws an elevation-shadowed
        // dialog 2·Z too far right and down, clipped by the buffer edge.
        let insets = record.params.surface_insets;
        let placement = crate::surface_client::LayerPlacement {
            x: record.frame.left - insets.left,
            y: record.frame.top - insets.top,
            z: i32::try_from(record.z).unwrap_or(i32::MAX),
            alpha: record.params.alpha.clamp(0.0, 1.0),
            dim_amount,
            // `FLAG_SECURE` is the window's, so the compositor can blank a
            // captured frame instead of publishing the app's pixels.
            secure: record.params.flags & crate::surface_client::FLAG_SECURE != 0,
        };
        let mut state = compositor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.set_layer_placement(binding.handle, placement)
    }

    /// `IWindowSession.addToDisplayAsUser` on `session`.
    pub fn add_to_display_as_user_for(
        &mut self,
        session: SessionId,
        window: BinderId,
        params: LayoutParams,
        requested_visible_types: i32,
    ) -> WmResult<AddWindowResult> {
        self.wm
            .add_to_display_as_user(session, window, params, requested_visible_types)
    }

    /// `IWindowSession.addToDisplayAsUser` (VM transport's current session).
    pub fn add_to_display_as_user(
        &mut self,
        window: BinderId,
        params: LayoutParams,
        requested_visible_types: i32,
    ) -> WmResult<AddWindowResult> {
        self.add_to_display_as_user_for(self.current, window, params, requested_visible_types)
    }

    /// `IWindowSession.relayout` on `session`, without a client-measured size.
    pub fn relayout_for(
        &mut self,
        session: SessionId,
        window: WindowId,
        params: LayoutParams,
        sync_seq: i32,
    ) -> WmResult<RelayoutResult> {
        self.relayout_with_requested_for(session, window, params, -1, -1, sync_seq)
    }

    /// `IWindowSession.relayout` without a client-measured size (VM transport).
    pub fn relayout(
        &mut self,
        window: WindowId,
        params: LayoutParams,
        sync_seq: i32,
    ) -> WmResult<RelayoutResult> {
        self.relayout_for(self.current, window, params, sync_seq)
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
        self.relayout_with_requested_for(
            self.current,
            window,
            params,
            requested_width,
            requested_height,
            sync_seq,
        )
    }

    /// [`Self::relayout_with_requested`] on an explicit session: the call is
    /// validated against the session node it arrived on (MPROC5c).
    pub fn relayout_with_requested_for(
        &mut self,
        session: SessionId,
        window: WindowId,
        params: LayoutParams,
        requested_width: i32,
        requested_height: i32,
        sync_seq: i32,
    ) -> WmResult<RelayoutResult> {
        self.check_session_window(session, window)?;
        let r = self.wm.relayout_with_requested(
            session,
            window,
            params,
            requested_width,
            requested_height,
            sync_seq,
        )?;
        // LAYER0: the routing frame, the layer-stack z and the FLAG_NOT_TOUCHABLE
        // answer are all refreshed together, so a relayout cannot leave the
        // dispatcher hit-testing against a stale stack.
        if let Some(record) = self.wm.windows.get(&window) {
            self.input.refresh_window(record);
        }
        Ok(r)
    }

    /// `IWindowSession.remove` (VM transport's current session).
    pub fn remove(&mut self, window: WindowId) -> WmResult<()> {
        self.remove_for(self.current, window)
    }

    /// `IWindowSession.remove` on `session`.
    pub fn remove_for(&mut self, session: SessionId, window: WindowId) -> WmResult<()> {
        self.check_session_window(session, window)?;
        self.wm.remove(window)?;
        self.input.remove_window(window);
        // The remaining windows' `z` are relative stack positions and just
        // shifted; a stale one would route a touch to the wrong window.
        let ids: Vec<WindowId> = self.wm.windows.keys().copied().collect();
        for id in ids {
            if let Some(record) = self.wm.windows.get(&id) {
                self.input.refresh_window(record);
            }
        }
        Ok(())
    }

    /// Destroy the compositor layer subtree bound to `window` (**W42**).
    ///
    /// Stock: `WindowManagerService.removeWindow` destroys the window's
    /// `SurfaceControl`, and `SurfaceFlinger` drops the layer together with its
    /// children (including the BLAST child that holds the pixels). Without this
    /// the composite kept drawing a destroyed Activity's stored buffer (W38
    /// residual §5.3). The registry is keyed by the window's `acos-wm` surface
    /// handle ([`acos_wm::wms::WindowRecord::surface_handle`]), so this must run
    /// **before** `IWindowSession.remove` drops the window record. Returns the
    /// number of layers dropped; `None` when the window never bound a layer
    /// (nothing to destroy — never a fabricated count).
    pub fn destroy_surface_layer(
        &mut self,
        window: WindowId,
        compositor: &Arc<Mutex<crate::surface_client::SurfaceState>>,
    ) -> Option<usize> {
        let surface_handle = self.wm.windows.get(&window)?.surface_handle?;
        let binding = self.surface_layers.remove(&surface_handle)?;
        let mut state = compositor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dropped = state.destroy_layer_tree(binding.handle);
        if std::env::var_os("ACOS_WMS_TRACE").is_some() {
            eprintln!(
                "ACOSWMS destroy-layer window={:#x} surface={surface_handle:#x} layer={:#x} dropped={dropped}",
                window, binding.handle.0
            );
        }
        Some(dropped)
    }

    /// Hide or show a window's layer (`eLayerHidden`), for a `relayout` whose
    /// `viewVisibility` is `GONE`/`VISIBLE` (**W42**). `false` when the window
    /// has no bound layer.
    pub fn set_surface_visible(
        &self,
        window: WindowId,
        visible: bool,
        compositor: &Arc<Mutex<crate::surface_client::SurfaceState>>,
    ) -> bool {
        let Some(surface_handle) = self
            .wm
            .windows
            .get(&window)
            .and_then(|record| record.surface_handle)
        else {
            return false;
        };
        let Some(binding) = self.surface_layers.get(&surface_handle) else {
            return false;
        };
        let mut state = compositor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.set_layer_hidden(binding.handle, !visible)
    }

    /// `IWindowSession.finishDrawing`: records the drawing callback. It does
    /// **not** publish a frame; only a real compositor buffer submission does
    /// (see `acos-runtime`'s `WmSurfaceBackend`).
    pub fn finish_drawing(&mut self, window: WindowId, seq: i32) -> WmResult<()> {
        self.finish_drawing_for(self.current, window, seq)
    }

    /// `IWindowSession.finishDrawing` on `session`.
    pub fn finish_drawing_for(
        &mut self,
        session: SessionId,
        window: WindowId,
        seq: i32,
    ) -> WmResult<()> {
        self.check_session_window(session, window)?;
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
        self.set_insets_for(self.current, window, touchable_region, content_insets)
    }

    /// `IWindowSession.setInsets` on `session`.
    pub fn set_insets_for(
        &mut self,
        session: SessionId,
        window: WindowId,
        touchable_region: Option<acos_wm::geom::RectI>,
        content_insets: Option<acos_wm::geom::RectI>,
    ) -> WmResult<()> {
        self.check_session_window(session, window)?;
        self.wm.set_insets(window, touchable_region, content_insets)
    }

    /// `IWindowSession.updateRequestedVisibleTypes` (oneway).
    pub fn update_requested_visible_types(&mut self, window: WindowId, types: i32) -> WmResult<()> {
        self.update_requested_visible_types_for(self.current, window, types)
    }

    /// `IWindowSession.updateRequestedVisibleTypes` on `session`.
    pub fn update_requested_visible_types_for(
        &mut self,
        session: SessionId,
        window: WindowId,
        types: i32,
    ) -> WmResult<()> {
        self.check_session_window(session, window)?;
        self.wm.update_requested_visible_types(window, types)
    }

    /// `IWindowSession.reportSystemGestureExclusionChanged` (oneway; recorded by
    /// validating that the window exists).
    pub fn report_system_gesture_exclusion_changed(&mut self, window: WindowId) -> WmResult<()> {
        self.report_system_gesture_exclusion_changed_for(self.current, window)
    }

    /// `IWindowSession.reportSystemGestureExclusionChanged` on `session`.
    pub fn report_system_gesture_exclusion_changed_for(
        &mut self,
        session: SessionId,
        window: WindowId,
    ) -> WmResult<()> {
        self.check_session_window(session, window)
    }

    /// `IWindowSession.pokeDrawLock` (oneway at the Java level; recorded).
    pub fn poke_draw_lock(&mut self, window: WindowId) -> WmResult<()> {
        self.poke_draw_lock_for(self.current, window)
    }

    /// `IWindowSession.pokeDrawLock` on `session`.
    pub fn poke_draw_lock_for(&mut self, session: SessionId, window: WindowId) -> WmResult<()> {
        self.check_session_window(session, window)
    }

    /// `IWindowSession.onRectangleOnScreenRequested` (oneway).
    pub fn on_rectangle_on_screen_requested(
        &mut self,
        window: WindowId,
        rect: Option<acos_wm::geom::RectI>,
    ) -> WmResult<()> {
        self.on_rectangle_on_screen_requested_for(self.current, window, rect)
    }

    /// `IWindowSession.onRectangleOnScreenRequested` on `session`.
    pub fn on_rectangle_on_screen_requested_for(
        &mut self,
        session: SessionId,
        window: WindowId,
        rect: Option<acos_wm::geom::RectI>,
    ) -> WmResult<()> {
        self.check_session_window(session, window)?;
        self.wm.on_rectangle_on_screen_requested(window, rect)
    }

    /// `IWindowManager.useBLAST`.
    #[must_use]
    pub const fn use_blast(&self) -> bool {
        true
    }

    /// `IWindowManager.getCurrentAnimatorScale`.
    ///
    /// P12: the per-session animation scale installed by the launcher (the
    /// real-Android path is the `animator_duration_scale` setting observed by
    /// the system process, which then calls `IWindowSessionCallback`).
    #[must_use]
    pub fn current_animator_scale(&self) -> f32 {
        crate::animation::scale()
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

    /// `IWindowManager.isKeyguardLocked`.
    ///
    /// ACOS has no keyguard: nothing ever shows a lock screen (the launcher goes
    /// straight to the activity, and `WmsState` has no KeyguardController).
    /// `false` is therefore the true answer for this device, not a default: on a
    /// phone with no lock screen `KeyguardManager.isKeyguardLocked()` answers
    /// `false` (`KeyguardManager.java:579-585` reads
    /// `WindowManagerService.mKeyguardController.isKeyguardShowing()`, which is
    /// false while no keyguard exists). GATENBOOT: Gaten's accessibility service
    /// calls `KeyguardManager.isKeyguardLocked()` from `onServiceConnected`, so
    /// an unsupported error here kills the service process at bind time.
    #[must_use]
    pub const fn is_keyguard_locked(&self) -> bool {
        false
    }

    /// `IWindowManager.isKeyguardSecure(int userId)`.
    ///
    /// As [`Self::is_keyguard_locked`]: no keyguard, so no user has a secure lock
    /// screen (`KeyguardController.isKeyguardSecure` is false without one). A
    /// locked SIM card also makes AOSP answer `true`; ACOS has no SIM card state
    /// (the telephony module is scripted), so nothing can be SIM-locked either.
    #[must_use]
    pub const fn is_keyguard_secure(&self) -> bool {
        false
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
    /// The per-call session factory (`openSession` → a new session **and** a
    /// new host node). `None` before `registry` has finished installing the
    /// pieces a session needs (window-id node, compositor, input producer): an
    /// `openSession` then fails structurally instead of answering a binder the
    /// host cannot route.
    sessions: Arc<Mutex<Option<crate::window_session::SessionFactory>>>,
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
    pub fn new(
        state: Arc<Mutex<WmsState>>,
        wiring: Wiring,
        sessions: Arc<Mutex<Option<crate::window_session::SessionFactory>>>,
    ) -> Self {
        Self {
            state,
            _binder: wiring.self_binder,
            sessions,
        }
    }

    /// The shared state (for the runtime that owns the wire codecs).
    #[must_use]
    pub fn shared_state(&self) -> Arc<Mutex<WmsState>> {
        Arc::clone(&self.state)
    }

    fn unsupported(interface: &str, method: &MethodInfo) -> crate::error::ServiceError {
        crate::error::ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        }
    }
}

impl ServiceObject for WmsService {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        args: &[Value],
    ) -> crate::error::Result<CallResult> {
        if interface != "android.view.IWindowManager" {
            return Err(Self::unsupported(interface, method));
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
            // AOSP's keyguard queries: this device has no lock screen, so the
            // honest answer is "not locked, not secure" (see the `WmsState`
            // methods' AOSP evidence). Gaten's accessibility service asks this
            // while connecting, before it adds its overlay window.
            "isKeyguardLocked" => vec![Value::Bool(state.is_keyguard_locked())],
            "isKeyguardSecure" => vec![Value::Bool(state.is_keyguard_secure())],
            // `IWindowManager.openSession(IWindowSessionCallback)` creates a
            // **new** session and answers **that session's** `IWindowSession`
            // binder, exactly like stock `WindowManagerService.openSession`
            // (`new Session(this, callback)`). MPROC5b measured why one shared
            // node cannot work for two processes: the second `openSession`
            // overwrote the first session id and the first process's next
            // `relayout` was refused (`unknown session 2`). A new session
            // record is allocated on every call, exactly like the real WMS.
            "openSession" => {
                let factory = self
                    .sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                    .ok_or_else(|| {
                        crate::error::ServiceError::Internal(
                            "android.view.IWindowSession cannot be opened: the session factory                              is not installed on the host"
                                .into(),
                        )
                    })?;
                // The factory takes the state lock itself; drop ours first.
                drop(state);
                let binder = factory.open();
                return Ok(CallResult {
                    reply: vec![Value::Binder(Some(binder))],
                    deliveries: Vec::new(),
                });
            }
            // The parcelable-bearing methods are served by `wms_java`.
            _ => return Err(Self::unsupported(interface, method)),
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
    install_with_insets(host, None).state
}

/// What [`install_with_insets`] hands the registry: the shared state and the
/// slot the per-call session factory goes into once the pieces a session needs
/// (the `IWindowId` node, the compositor, the input producer) are installed.
#[derive(Clone, Debug)]
pub struct WmsInstall {
    /// The state machine every `IWindowSession` node serves.
    pub state: Arc<Mutex<WmsState>>,
    /// The session-factory slot (`registry` fills it).
    pub sessions: Arc<Mutex<Option<crate::window_session::SessionFactory>>>,
}

/// W7b: [`install`] with an explicitly chosen app-window inset model.
///
/// `None` keeps the process environment / build default. Tests that pin the
/// pre-W7 carved wire geometry use `Some(InsetsMode::Carve)` instead of
/// mutating the process environment (which is not thread-safe).
pub fn install_with_insets(
    host: &mut crate::host::ServiceHost,
    insets: Option<InsetsMode>,
) -> WmsInstall {
    let state = Arc::new(Mutex::new(match insets {
        Some(mode) => WmsState::new_with_insets(mode),
        None => WmsState::new(),
    }));
    let sessions: Arc<Mutex<Option<crate::window_session::SessionFactory>>> =
        Arc::new(Mutex::new(None));
    let factory_state = Arc::clone(&state);
    let factory_sessions = Arc::clone(&sessions);
    host.register("window", "android.view.IWindowManager", move |w| {
        Box::new(WmsService::new(
            Arc::clone(&factory_state),
            w,
            Arc::clone(&factory_sessions),
        )) as Box<dyn ServiceObject>
    });
    WmsInstall { state, sessions }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_inset_model_is_report() {
        // ADR-0026 decision 6: phone-like edge-to-edge is the build default.
        assert_eq!(DEFAULT_INSETS_MODE, InsetsMode::Report);
    }

    #[test]
    fn report_model_gives_the_app_the_full_display() {
        let mut st = WmsState::new_with_insets(InsetsMode::Report);
        let add = st
            .add_to_display_as_user(BinderId(0x12), app_params(), 0)
            .unwrap();
        assert_eq!(add.result, ADD_OKAY);
        assert_eq!(add.out_frame, RectI::new(0, 0, 1080, 2400));
        assert_eq!(
            add.insets_state
                .visible_insets_for(inset_type::STATUS_BAR)
                .top,
            128,
            "report model still reports the status bar"
        );
    }
    use acos_wm::codec::inset_type;
    use acos_wm::geom::RectI;
    use acos_wm::wms::{ADD_BAD_APP_TOKEN, ADD_OKAY, RELAYOUT_RES_FIRST_TIME};

    fn app_params() -> LayoutParams {
        LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity")
    }

    #[test]
    fn add_relayout_and_finish_drawing_are_recorded() {
        // The pre-W7 carved frame, pinned explicitly: the build default is
        // `report` since W7b (ADR-0026 decision 6), and this test asserts the
        // frozen carve geometry.
        let mut st = WmsState::new_with_insets(InsetsMode::Carve);
        let add = st
            .add_to_display_as_user(BinderId(0x11), app_params(), 0)
            .unwrap();
        assert_eq!(add.result, ADD_OKAY);
        assert_eq!(add.out_frame, RectI::new(0, 128, 1080, 2337));
        assert_eq!(
            add.insets_state
                .visible_insets_for(inset_type::STATUS_BAR)
                .top,
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
        let a = st.session_binder();
        let b = st.open_session();
        assert_ne!(a, b);
        assert_eq!(st.session_binder(), b);
        assert_eq!(st.sessions.len(), 2, "both sessions stay open");
    }

    /// MPROC5c, the MPROC5b failure as a test: with one session per
    /// `openSession`, a window belongs to the session that added it, so a
    /// `relayout` that arrives on **another** session is refused instead of
    /// being validated against a shared "last session".
    #[test]
    fn a_second_session_cannot_relayout_the_first_ones_window() {
        let mut st = WmsState::new();
        let first = st.current;
        let add = st
            .add_to_display_as_user_for(first, BinderId(0x21), app_params(), 0)
            .unwrap();
        assert_eq!(add.result, ADD_OKAY);
        let second = st.allocate_session();
        assert!(
            matches!(
                st.relayout_with_requested_for(second, add.window, app_params(), -1, -1, 0),
                Err(WmError::UnknownSession(s)) if s == second
            ),
            "proc-b's relayout of proc-a's window must be refused (MPROC5b: unknown session 2)"
        );
        // ... while the owning session's own relayout is served.
        st.relayout_with_requested_for(first, add.window, app_params(), -1, -1, 0)
            .expect("the owning session relayouts its own window");
        // Every other window operation is validated the same way.
        assert!(matches!(
            st.remove_for(second, add.window),
            Err(WmError::UnknownSession(s)) if s == second
        ));
        assert!(matches!(
            st.finish_drawing_for(second, add.window, 0),
            Err(WmError::UnknownSession(s)) if s == second
        ));
    }

    /// MPROC5c: a session death drops that session's windows — and only those.
    #[test]
    fn closing_a_session_drops_its_windows_and_keeps_the_others() {
        let mut st = WmsState::new();
        let first = st.current;
        let a = st
            .add_to_display_as_user_for(first, BinderId(0x31), app_params(), 0)
            .unwrap();
        let second = st.allocate_session();
        let b = st
            .add_to_display_as_user_for(second, BinderId(0x32), app_params(), 0)
            .unwrap();
        st.record_session_process(second, 1);
        assert_eq!(st.sessions_of_process(1), vec![second]);
        let closed = st.close_session(second).expect("the session existed");
        assert_eq!(closed.1, vec![b.window]);
        assert!(
            st.wm.windows.contains_key(&a.window),
            "the other session's window stays"
        );
        assert!(matches!(
            st.relayout_for(second, b.window, app_params(), 0),
            Err(WmError::UnknownWindow(w)) if w == b.window
        ));
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
