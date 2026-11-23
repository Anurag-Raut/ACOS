//! `svc.wms`: the IWindowSession / IWindowManager window state machine.
//!
//! This is the VM-independent half of BACKLOG T-29: it decodes the real
//! `addToDisplayAsUser`/`relayout` arguments (via [`crate::codec`]) and produces
//! the typed out-parameters (`ClientWindowFrames`, `MergedConfiguration`,
//! `SurfaceControl`, `InsetsState`, `InsetsSourceControl$Array`) whose byte
//! layouts were derived from the framework DEX.
//!
//! The window frame is not guessed: for the reference device it is the measured
//! `mAppBounds=Rect(0, 128 - 1080, 2337)` and the inset sources are the exact
//! ten `InsetsSource` rows from `display.txt` (status bar 128 px, nav bar
//! 63 px, IME `[0,2274][1080,2400]`, cutout 128 px).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use acos_parcel::binder::BinderId;

use crate::callbacks::{CallbackQueue, WmCallback};
use crate::codec::{
    inset_type, ClientWindowFrames, Configuration, Cutout, CutoutPathParserInfo, DisplayCutout,
    DisplayShape, InsetsSource, InsetsSourceControl, InsetsSourceControlArray, InsetsState,
    LayoutParams, MergedConfiguration, PrivacyIndicatorBounds, RoundedCorner, RoundedCorners,
    SurfaceControlInfo, WindowConfiguration,
};
use crate::display::{DisplaySpec, InsetsMode, TaskRecord, WINDOWING_MODE_FULLSCREEN};
use crate::error::{Result, WmError};
use crate::geom::{InsetsI, PointI, RectI};
use crate::transport::{Channel, InputChannelRegistry};

/// A window is identified by its `IWindow` binder id.
pub type WindowId = u64;
/// A session is identified by the `IWindowSession` binder id.
pub type SessionId = u64;
/// Default primary task id (matches `acos_services::atms::ActivityRecord::task_id`).
pub const DEFAULT_TASK_ID: i32 = 10;

/// `IWindowSession.ADD_OKAY`.
pub const ADD_OKAY: i32 = 0;
/// `ADD_BAD_APP_TOKEN`.
pub const ADD_BAD_APP_TOKEN: i32 = 1;
/// `ADD_BAD_SUBWINDOW_TOKEN`.
pub const ADD_BAD_SUBWINDOW_TOKEN: i32 = 2;
/// `ADD_NOT_APP_TOKEN`.
pub const ADD_NOT_APP_TOKEN: i32 = 3;
/// `ADD_DUPLICATE_ADD`.
pub const ADD_DUPLICATE_ADD: i32 = 5;

/// `IWindowSession.RELAYOUT_RES_FIRST_TIME`.
pub const RELAYOUT_RES_FIRST_TIME: i32 = 1;
/// `RELAYOUT_RES_SURFACE_CHANGED`.
pub const RELAYOUT_RES_SURFACE_CHANGED: i32 = 2;
/// `RELAYOUT_RES_SURFACE_RESIZED`.
pub const RELAYOUT_RES_SURFACE_RESIZED: i32 = 4;
/// `RELAYOUT_RES_CONSUME_ALWAYS_SYSTEM_BARS`.
pub const RELAYOUT_RES_CONSUME_ALWAYS_SYSTEM_BARS: i32 = 8;

/// The `InsetsSource` id of the rotated-configuration status-bar strip
/// ([`DisplaySpec::config_insets`]'s top strip).
///
/// It is deliberately outside the reference phone's id space (the status bar is
/// `0x9b1a0000`, the nav bar `0xfc050001`, the IME `3`, the cutout `27`): the
/// strip is not a window with a leash, it is the `configInsets` row of
/// `DisplayPolicy.DecorInsetsInfo`, and the guest needs it as a distinct source
/// because `InsetsState` is keyed by id (`SparseArray`), not by type.
pub const CONFIG_STATUS_BAR_SOURCE_ID: i32 = 0x9b1a_0c00u32 as i32;

/// Lifecycle of a window record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    /// `addToDisplayAsUser` accepted, no surface yet.
    Added,
    /// `relayout` created the surface.
    HasSurface,
    /// `finishDrawing` delivered a buffer.
    Drawn,
    /// Removed from the session.
    Removed,
}

/// Everything the WMS knows about one window.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowRecord {
    /// Window id (`IWindow` binder id).
    pub id: WindowId,
    /// Owning session.
    pub session: SessionId,
    /// `IWindow` callback binder.
    pub callback: Option<BinderId>,
    /// Latest `LayoutParams`.
    pub params: LayoutParams,
    /// Window frame in display coordinates.
    pub frame: RectI,
    /// Full display frame.
    pub display_frame: RectI,
    /// Parent frame (same as `frame` for top-level windows).
    pub parent_frame: RectI,
    /// `true` when the frame is attached inside a parent.
    pub attached_in_parent: bool,
    /// Requested visibility.
    pub visible: bool,
    /// Whether a `SurfaceControl` exists.
    pub has_surface: bool,
    /// `SurfaceControl` layer handle.
    pub surface_handle: Option<u64>,
    /// `SurfaceControl` name.
    pub surface_name: String,
    /// Input-channel registry handle.
    pub input_channel_handle: i32,
    /// `updateRequestedVisibleTypes` bitmask.
    pub requested_visible_types: i32,
    /// Whether the window may hold focus.
    pub focusable: bool,
    /// Z-order: the window's index in the layer stack (AOSP policy layer, then
    /// creation order; sub-windows next to their parent). See [`crate::layers`].
    pub z: i64,
    /// Creation order inside the display, the same-band tie-breaker
    /// (`WindowManagerPolicy` "added later wins"). Unlike the `IWindow` binder
    /// id this is monotonic, so a dialog added after the activity is above it.
    pub seq: u64,
    /// The window's kind, classified from `LayoutParams.type`.
    pub kind: crate::layers::WindowKind,
    /// Monotonic relayout sequence (the `syncSeqId`).
    pub relayout_seq: u64,
    /// Last insets state sent to the client.
    pub insets_state: InsetsState,
    /// Last insets controls sent to the client.
    pub insets_controls: InsetsSourceControlArray,
    /// Lifecycle.
    pub state: WindowState,
    /// `true` once `relayout` has run at least once.
    pub first_relayout_done: bool,
    /// `true` once `relayout` / `relayoutAsync` has observed `View.VISIBLE` (`0`).
    /// Before `dispatchAppVisibility(true)` reaches `ViewRootImpl`, the
    /// initial pre-visibility `relayout` passes `View.GONE` (`8`); ignoring
    /// that pre-visibility `GONE` prevents a spurious WM focus loss/regain
    /// bounce on every cold start.
    pub ever_visible: bool,
}

impl WindowRecord {
    /// The frame reported to the client.
    #[must_use]
    pub fn client_frames(&self) -> ClientWindowFrames {
        ClientWindowFrames {
            frame: self.frame,
            display_frame: self.display_frame,
            parent_frame: self.parent_frame,
            attached_frame: if self.attached_in_parent {
                Some(self.parent_frame)
            } else {
                None
            },
            is_parent_frame_clipped_by_display_cutout: false,
            compat_scale: 1.0,
        }
    }

    /// `LayoutParams.surfaceInsets` — the client's own drawing surface insets
    /// (**W54**).
    ///
    /// Stock `ViewRootImpl` computes them in `performTraversals`
    /// (`WindowManager.LayoutParams.setSurfaceInsets`: `ceil(view.getZ() * 2)`
    /// per side) and `ThreadedRenderer.setup` makes the drawing surface
    /// `frame + insets` while translating the canvas by `(insetLeft, insetTop)`
    /// (`ThreadedRenderer.java:515-535`, `:741`). The window manager therefore
    /// owns the other half of that contract: the window's *surface* is the
    /// frame grown by the insets, and the layer sits at `frame − insets` (see
    /// `SurfaceView.java:1180`, which converts a frame into the window layer's
    /// coordinate space exactly that way). This is what puts an elevation-
    /// shadowed dialog's card at its frame instead of 2·Z inside a too-small
    /// buffer.
    #[must_use]
    pub fn surface_insets(&self) -> RectI {
        self.params.surface_insets
    }

    /// The window's `SurfaceControl` size: frame + surface insets.
    #[must_use]
    pub fn surface_size(&self) -> (i32, i32) {
        let insets = self.surface_insets();
        (
            (self.frame.width() + insets.left + insets.right).max(0),
            (self.frame.height() + insets.top + insets.bottom).max(0),
        )
    }
}

/// `addToDisplayAsUser` out-parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct AddWindowResult {
    /// `ADD_OKAY` (0) on success.
    pub result: i32,
    /// The new input channel (handle + token).
    pub input_channel: crate::codec::InputChannelParcel,
    /// Insets state for the window.
    pub insets_state: InsetsState,
    /// Insets source controls.
    pub insets_controls: InsetsSourceControlArray,
    /// `outFrame`.
    pub out_frame: RectI,
    /// `outFloat` (compat scale).
    pub out_float: Vec<f32>,
    /// The window id assigned.
    pub window: WindowId,
}

/// `relayout` out-parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct RelayoutResult {
    /// Relayou result flags.
    pub result: i32,
    /// `ClientWindowFrames`.
    pub frames: ClientWindowFrames,
    /// `MergedConfiguration`.
    pub merged_configuration: MergedConfiguration,
    /// The window's `SurfaceControl`.
    pub surface: SurfaceControlInfo,
    /// `InsetsState`.
    pub insets_state: InsetsState,
    /// `InsetsSourceControl$Array`.
    pub insets_controls: InsetsSourceControlArray,
    /// The `syncSeqId` echoed back.
    pub sync_seq: u64,
}

/// Per-session state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionRecord {
    /// `IWindowSessionCallback` binder.
    pub callback: Option<BinderId>,
    /// Windows in creation order.
    pub windows: Vec<WindowId>,
}

/// The window manager state machine.
#[derive(Debug)]
pub struct WindowManager {
    /// Display geometry.
    pub display: DisplaySpec,
    /// Sessions by id.
    pub sessions: HashMap<SessionId, SessionRecord>,
    /// Windows by id.
    pub windows: BTreeMap<WindowId, WindowRecord>,
    /// Input channels handed to windows.
    pub channels: InputChannelRegistry,
    /// Outgoing `IWindow` callbacks.
    pub callbacks: CallbackQueue,
    /// Currently focused window.
    pub focus: Option<WindowId>,
    /// Touch-mode flag (device-wide).
    pub touch_mode: bool,
    /// `SurfaceControl` layer id allocator.
    next_layer: u64,
    /// Session id allocator.
    next_session: u64,
    /// Window creation-order allocator (a same-policy-layer tie-breaker).
    next_window_seq: u64,
    /// `TYPE_ACCESSIBILITY_OVERLAY` window tokens the accessibility manager
    /// service created (`WindowManagerService.addWindowToken(token,
    /// TYPE_ACCESSIBILITY_OVERLAY, ...)`).
    ///
    /// AOSP `sanitizeWindowType` (`WindowManagerService.java:9914-9928`) only
    /// assigns `TYPE_ACCESSIBILITY_OVERLAY` to a client whose token *is* such a
    /// token, and `addWindow` answers `ADD_BAD_APP_TOKEN` for an
    /// accessibility-overlay window added with a token of another type
    /// (`WindowManagerService.java:1630-1640`). Without this set any app could
    /// stack a window above the accessibility overlay layer.
    overlay_tokens: BTreeSet<u64>,
    /// Per-task windowing bounds (`docs/MULTIWINDOW_PLAN.md` §4.1).
    /// Empty in fullscreen sessions so existing fullscreen behaviour is byte-identical.
    pub tasks: BTreeMap<i32, TaskRecord>,
    /// Optional explicit window → task id mapping. Application windows without
    /// an entry fall back to [`Self::active_task`] when a task override is active.
    pub window_task: HashMap<WindowId, i32>,
    /// Active task id for single-task multi-window overrides (slice 1).
    pub active_task: Option<i32>,
}

impl WindowManager {
    /// A window manager for `display`.
    #[must_use]
    pub fn new(display: DisplaySpec) -> Self {
        Self {
            display,
            sessions: HashMap::new(),
            windows: BTreeMap::new(),
            channels: InputChannelRegistry::new(),
            callbacks: CallbackQueue::new(),
            focus: None,
            touch_mode: true,
            next_layer: 0x1000,
            next_session: 0,
            next_window_seq: 0,
            overlay_tokens: BTreeSet::new(),
            tasks: BTreeMap::new(),
            window_task: HashMap::new(),
            active_task: None,
        }
    }

    /// `IWindowManager.openSession`: allocate a session and return its id and
    /// the synthetic `IWindowSession` binder id.
    pub fn open_session(&mut self, callback: Option<BinderId>) -> (SessionId, BinderId) {
        let id = self.next_session;
        self.next_session += 1;
        self.sessions.insert(
            id,
            SessionRecord {
                callback,
                windows: Vec::new(),
            },
        );
        (id, BinderId(0x5e55_0000 + id))
    }

    /// `IWindowSession.addToDisplayAsUser`.
    ///
    /// Creates the window record, a fresh input channel, the reference
    /// `InsetsState` and `InsetsSourceControl[]`, and queues the outgoing
    /// visibility/focus/insets callbacks in the measured order.
    pub fn add_to_display_as_user(
        &mut self,
        session: SessionId,
        window: BinderId,
        params: LayoutParams,
        requested_visible_types: i32,
    ) -> Result<AddWindowResult> {
        if !self.sessions.contains_key(&session) {
            return Err(WmError::UnknownSession(session));
        }
        if window.is_null() {
            return Ok(AddWindowResult {
                result: ADD_BAD_APP_TOKEN,
                input_channel: crate::codec::InputChannelParcel::absent(),
                insets_state: empty_insets_state(&self.display),
                insets_controls: InsetsSourceControlArray(vec![]),
                out_frame: RectI::default(),
                out_float: vec![],
                window: 0,
            });
        }
        if self.windows.contains_key(&window.0) {
            return Ok(AddWindowResult {
                result: ADD_DUPLICATE_ADD,
                input_channel: crate::codec::InputChannelParcel::absent(),
                insets_state: empty_insets_state(&self.display),
                insets_controls: InsetsSourceControlArray(vec![]),
                out_frame: RectI::default(),
                out_float: vec![],
                window: window.0,
            });
        }
        // AOSP `addWindow`/`sanitizeWindowType`: an accessibility overlay is
        // only accepted on the accessibility manager's own overlay token. Any
        // other type keeps the previous behaviour (the add path has no token
        // policy for it yet).
        if params.type_ == crate::layers::TYPE_ACCESSIBILITY_OVERLAY
            && !params
                .token
                .is_some_and(|token| self.overlay_tokens.contains(&token.0))
        {
            return Ok(AddWindowResult {
                result: ADD_BAD_APP_TOKEN,
                input_channel: crate::codec::InputChannelParcel::absent(),
                insets_state: empty_insets_state(&self.display),
                insets_controls: InsetsSourceControlArray(vec![]),
                out_frame: RectI::default(),
                out_float: vec![],
                window: 0,
            });
        }

        let kind = crate::layers::WindowKind::of(params.type_);
        let task = self.task_for_new_window(window.0, kind);
        let frame = compute_frame_for_requested_in_task(
            &self.display,
            &params,
            UNSPECIFIED_LENGTH,
            UNSPECIFIED_LENGTH,
            task.as_ref(),
        );
        let display_frame = task
            .filter(|t| t.is_multi_window())
            .map(|t| t.bounds)
            .unwrap_or(self.display.frame);
        let seq = self.next_window_seq;
        self.next_window_seq += 1;
        let layer = self.alloc_layer();
        let name = format!(
            "Surface(name={}/{})/@0x{:x}",
            params.package_name.as_deref().unwrap_or("app"),
            params.title.as_deref().unwrap_or("window"),
            layer
        );
        let id = window.0;
        // A real fd-backed pair so the app's `InputChannel.nativeReadFromParcel`
        // can install a real `InputEventReceiver`; fall back to the in-memory
        // pair (unit tests without fds) if the socketpair cannot be created.
        let channel_name = format!("{name} client");
        let channel = Channel::socketpair(channel_name.clone(), 0)
            .unwrap_or_else(|_| Channel::in_memory(channel_name, 0));
        let channel_handle = self.channels.register(channel);
        let insets_state = reference_insets_state(&self.display, false);
        // The controls describe inset-animation leash surfaces the window
        // manager owns. They are distinct layers: `base_layer + id` would make
        // the status-bar leash *be* the window layer, so a released leash would
        // invalidate the window's SurfaceControl. Allocate a fresh handle for
        // each leash instead.
        let mut insets_controls = reference_insets_controls(&self.display, layer);
        for control in &mut insets_controls.0 {
            if let Some(leash) = control.leash.as_mut() {
                leash.handle = Some(self.alloc_layer());
            }
        }
        // A window may hold focus when the framework would let it: it does not
        // set `FLAG_NOT_FOCUSABLE`. LAYER1: the old `!is_system_window` guard
        // was wrong for a focusable `TYPE_APPLICATION_OVERLAY` (AOSP lets it
        // receive keys) and disagreed with the layer stack's `focus_target`.
        let focusable = crate::layers::focusable(params.flags);
        // Provisional z; `renumber_z` sets the real layer-stack index once the
        // record is in `self.windows`.
        let z = 0;

        let record = WindowRecord {
            id,
            session,
            callback: Some(window),
            params,
            frame,
            display_frame,
            parent_frame: frame,
            attached_in_parent: true,
            visible: true,
            has_surface: false,
            surface_handle: Some(layer),
            surface_name: name,
            input_channel_handle: channel_handle,
            requested_visible_types,
            focusable,
            z,
            seq,
            kind,
            relayout_seq: 0,
            insets_state: insets_state.clone(),
            insets_controls: insets_controls.clone(),
            state: WindowState::Added,
            first_relayout_done: false,
            ever_visible: false,
        };
        self.windows.insert(id, record);
        if let Some(t) = task {
            self.window_task.insert(id, t.task_id);
        }
        if let Some(s) = self.sessions.get_mut(&session) {
            s.windows.push(id);
        }
        self.renumber_z();

        // Measured launch order: visibility → focus → insetsControlChanged.
        self.callbacks
            .push(id, WmCallback::DispatchAppVisibility { visible: true });
        // LAYER1: focus follows the layer stack, not "first focusable add":
        // a dialog above the activity takes focus; a FLAG_NOT_FOCUSABLE
        // overlay above it does not. The transition is published as
        // WindowFocusChanged(false) for the old window and (true) for the new
        // one, on add *and* remove (`WmsState::remove`), so a listener sees the
        // same focus the hit test and the tree report.
        self.publish_focus_transition();
        self.callbacks.push(
            id,
            WmCallback::InsetsControlChanged {
                state: Box::new(insets_state.clone()),
                controls: Box::new(insets_controls.clone()), ime: false, },
        );

        let result = AddWindowResult {
            result: ADD_OKAY,
            input_channel: crate::codec::InputChannelParcel {
                name: self
                    .channels
                    .get(channel_handle)
                    .map(|c| c.name.clone())
                    .unwrap_or_default(),
                present: true,
                channel_handle,
                token: Some(window),
            },
            insets_state,
            insets_controls,
            out_frame: frame,
            out_float: vec![1.0],
            window: id,
        };
        Ok(result)
    }

    /// `IWindowSession.relayout` without a client-measured size.
    pub fn relayout(
        &mut self,
        session: SessionId,
        window: WindowId,
        params: LayoutParams,
        sync_seq: i32,
    ) -> Result<RelayoutResult> {
        self.relayout_with_requested(
            session,
            window,
            params,
            UNSPECIFIED_LENGTH,
            UNSPECIFIED_LENGTH,
            sync_seq,
        )
    }

    /// `IWindowSession.relayout` with the client-reported measured size.
    ///
    /// `requested_width`/`requested_height` are the six-int `relayout`
    /// arguments (`requestedWidth`, `requestedHeight`, …) the real WMS stores
    /// in `WindowState.mRequestedWidth/mRequestedHeight` and feeds to
    /// `WindowLayout.computeFrames`. A `WRAP_CONTENT` window takes its frame
    /// size from them; a `-1` ([`UNSPECIFIED_LENGTH`]) falls back to the layout
    /// params or the parent frame, exactly like the framework.
    pub fn relayout_with_requested(
        &mut self,
        session: SessionId,
        window: WindowId,
        params: LayoutParams,
        requested_width: i32,
        requested_height: i32,
        sync_seq: i32,
    ) -> Result<RelayoutResult> {
        let display = self.display;
        let task = self.task_for_window(window);
        let merged = merged_configuration_for_task(&display, task.as_ref());
        let record = self
            .windows
            .get_mut(&window)
            .ok_or(WmError::UnknownWindow(window))?;
        if record.session != session {
            return Err(WmError::UnknownSession(session));
        }
        let old_frame = record.frame;
        let old_surface_size = record.surface_size();
        let first = !record.first_relayout_done;
        record.params = params;
        record.frame = compute_frame_for_requested_in_task(
            &display,
            &record.params,
            requested_width,
            requested_height,
            task.as_ref(),
        );
        record.display_frame = task
            .filter(|t| t.is_multi_window())
            .map(|t| t.bounds)
            .unwrap_or(display.frame);
        record.parent_frame = record.frame;
        let (surface_width, surface_height) = record.surface_size();
        // Stock resizes the window surface when the frame *or* the surface
        // insets change (`RELAYOUT_RES_SURFACE_RESIZED`).
        let resized =
            record.frame != old_frame || (surface_width, surface_height) != old_surface_size;
        record.relayout_seq += 1;
        let sync_seq_out = if sync_seq >= 0 {
            sync_seq as u64
        } else {
            record.relayout_seq
        };
        record.first_relayout_done = true;
        record.state = WindowState::HasSurface;
        record.has_surface = true;

        let frames = record.client_frames();
        // W54: the surface the client draws into is the frame grown by the
        // client's surface insets (stock `WindowState.getSurfaceWidth/Height`).
        // The client already sized its hardware renderer that way
        // (`ThreadedRenderer.setup`), so a surface at the bare frame size clips
        // the card and leaves the inset band transparent.
        let surface = SurfaceControlInfo {
            name: record.surface_name.clone(),
            width: surface_width,
            height: surface_height,
            handle: record.surface_handle,
            // The services layer binds this control to a real compositor layer
            // (`SurfaceState::bind_layer`, W5) before it goes on the wire.
            client: None,
            layer_binder: None,
        };
        let insets_state = record.insets_state.clone();
        let insets_controls = record.insets_controls.clone();

        let mut result = 0;
        if first {
            result |= RELAYOUT_RES_FIRST_TIME;
            result |= RELAYOUT_RES_SURFACE_CHANGED;
        }
        if resized {
            result |= RELAYOUT_RES_SURFACE_RESIZED;
        }

        // Outgoing callbacks: resized then insetsControlChanged (measured).
        self.callbacks.push(
            window,
            WmCallback::Resized {
                frames: Box::new(frames.clone()),
                report_draw: true,
                merged_configuration: Box::new(merged.clone()),
                insets_state: Box::new(insets_state.clone()),
                force_layout: false,
                always_consume_system_bars: false,
                display_id: display.id,
                sync_seq_id: sync_seq_out as i32,
                drag_resizing: false,
            },
        );
        self.callbacks.push(
            window,
            WmCallback::InsetsControlChanged {
                state: Box::new(insets_state.clone()),
                controls: Box::new(insets_controls.clone()), ime: false, },
        );
        // LAYER1: `relayout` carries the current `LayoutParams.flags`, so a
        // window that just became/stopped being `FLAG_NOT_FOCUSABLE` changes
        // who the stack's `focus_target()` is. Publish that transition here so
        // focus never disagrees with `hit_test`/the tree.
        self.publish_focus_transition();

        Ok(RelayoutResult {
            result,
            frames,
            merged_configuration: merged,
            surface,
            insets_state,
            insets_controls,
            sync_seq: sync_seq_out,
        })
    }

    /// Toggle a window's visibility (`View.VISIBLE` vs `INVISIBLE`/`GONE`)
    /// from `relayout` / `relayoutAsync`.
    ///
    /// Hidden windows leave the visible layer stack (`layer_stack()` skips
    /// `!record.visible`) and cannot hold focus; re-showing an existing
    /// window allocates a fresh sequence number so it rises to the top of its
    /// band and takes focus, matching AOSP task-to-front ordering.
    pub fn set_window_visible(&mut self, window: WindowId, visible: bool) -> Result<bool> {
        let changed = {
            let record = self
                .windows
                .get_mut(&window)
                .ok_or(WmError::UnknownWindow(window))?;
            if visible {
                record.ever_visible = true;
            }
            if record.visible == visible {
                false
            } else {
                if visible {
                    let seq = self.next_window_seq;
                    self.next_window_seq += 1;
                    record.seq = seq;
                }
                record.visible = visible;
                true
            }
        };
        if changed {
            self.renumber_z();
            self.publish_focus_transition();
        }
        Ok(changed)
    }

    /// Update window visibility from `relayout` / `relayoutAsync`, ignoring an
    /// initial pre-visibility `INVISIBLE`/`GONE` before `View.VISIBLE` (`0`)
    /// has ever been observed (i.e. before `dispatchAppVisibility(true)`).
    pub fn set_relayout_window_visible(&mut self, window: WindowId, visible: bool) -> Result<bool> {
        let should_apply = {
            let record = self
                .windows
                .get_mut(&window)
                .ok_or(WmError::UnknownWindow(window))?;
            if visible {
                record.ever_visible = true;
                true
            } else {
                record.ever_visible
            }
        };
        if !should_apply {
            return Ok(false);
        }
        self.set_window_visible(window, visible)
    }

    /// Session death (stock `Session.kill` / process death): drop the session
    /// and every window it owns, returning the window ids that went with it.
    ///
    /// An unknown session is not an error here: the caller is a teardown path
    /// that races nothing (a second death report must not fail).
    pub fn close_session(&mut self, session: SessionId) -> Vec<WindowId> {
        let Some(record) = self.sessions.remove(&session) else {
            return Vec::new();
        };
        let windows = record.windows.clone();
        for window in &windows {
            let _ = self.remove(*window);
        }
        windows
    }

    /// The session that owns `window`, when the window is known.
    #[must_use]
    pub fn window_session(&self, window: WindowId) -> Option<SessionId> {
        self.windows.get(&window).map(|record| record.session)
    }

    /// `IWindowSession.remove`.
    pub fn remove(&mut self, window: WindowId) -> Result<()> {
        let record = self
            .windows
            .remove(&window)
            .ok_or(WmError::UnknownWindow(window))?;
        self.window_task.remove(&window);
        self.channels.remove(record.input_channel_handle);
        if let Some(s) = self.sessions.get_mut(&record.session) {
            s.windows.retain(|w| *w != window);
        }
        self.renumber_z();
        if self.focus == Some(window) {
            // The focused window just went away: focus the topmost remaining
            // focusable window (layer order, not insertion order), and publish
            // the transition to it. The removed window's channel is closed.
            self.focus = None;
            self.publish_focus_transition();
        }
        Ok(())
    }

    /// Move focus to the topmost focusable window and queue the two
    /// `WindowFocusChanged` callbacks. No-op when focus did not change.
    fn publish_focus_transition(&mut self) {
        let target = self.focus_target();
        let old = self.focus;
        if old == target {
            return;
        }
        if let Some(old) = old {
            if self.windows.contains_key(&old) {
                self.callbacks
                    .push(old, WmCallback::WindowFocusChanged { focused: false });
            }
        }
        if let Some(new) = target {
            self.callbacks
                .push(new, WmCallback::WindowFocusChanged { focused: true });
        }
        self.focus = target;
    }

    /// `IWindowSession.finishDrawing`: record that the buffer arrived for
    /// `window`. The compositor handoff happens in [`crate::facade::SessionWm`].
    pub fn finish_drawing(&mut self, window: WindowId, seq: i32) -> Result<WindowId> {
        let record = self
            .windows
            .get_mut(&window)
            .ok_or(WmError::UnknownWindow(window))?;
        record.state = WindowState::Drawn;
        if seq >= 0 {
            // The client echoes the frame's syncSeqId; nothing else to do here.
            let _ = seq;
        }
        Ok(window)
    }

    /// `IWindowSession.reportDrawn`.
    pub fn report_drawn(&mut self, window: WindowId) -> Result<()> {
        self.finish_drawing(window, -1).map(|_| ())
    }

    /// `IWindowSession.getDisplayId`.
    #[must_use]
    pub fn get_display_id(&self, _window: WindowId) -> i32 {
        self.display.id
    }

    /// `IWindowSession.setInsets` (oneway, recorded).
    pub fn set_insets(
        &mut self,
        window: WindowId,
        _touchable_region: Option<RectI>,
        _content_insets: Option<RectI>,
    ) -> Result<()> {
        self.windows
            .get_mut(&window)
            .ok_or(WmError::UnknownWindow(window))
            .map(|_| ())
    }

    /// `IWindowSession.updateRequestedVisibleTypes` (oneway).
    pub fn update_requested_visible_types(&mut self, window: WindowId, types: i32) -> Result<()> {
        let display = self.display;
        let ime_visible = types & inset_type::IME != 0;
        let (state, controls) = {
            let record = self
                .windows
                .get_mut(&window)
                .ok_or(WmError::UnknownWindow(window))?;
            record.requested_visible_types = types;
            // IME visibility tracks the requested types' ITYPE_IME bit.
            let state = reference_insets_state(&display, ime_visible);
            if ime_visible {
                let controls = record.insets_controls.clone();
                record.insets_state = state.clone();
                (Some(state), Some(controls))
            } else {
                record.insets_state = state;
                (None, None)
            }
        };
        if let (Some(state), Some(controls)) = (state, controls) {
            self.callbacks.push(
                window,
                WmCallback::ShowInsets {
                    types: inset_type::IME,
                    from_ime: false,
                    token: None,
                },
            );
            self.callbacks.push(
                window,
                WmCallback::InsetsControlChanged {
                    state: Box::new(state),
                    controls: Box::new(controls), ime: false, },
            );
        }
        Ok(())
    }

    /// The in-process IME showed or hid its (null) surface.
    ///
    /// The IMMS side of `InputMethodManagerService.showSoftInput` /
    /// `hideSoftInput`: the IME bit of the focused window's requested-visible
    /// types follows the IME, the window's `InsetsState` is recomputed, and the
    /// app-visible `IWindow.showInsets`/`hideInsets` + `insetsControlChanged`
    /// callbacks are queued in **both** directions (a phone's app sees the IME
    /// insets appear and disappear, which is what triggers its relayout).
    pub fn set_ime_visible(&mut self, window: WindowId, visible: bool) -> Result<()> {
        let display = self.display;
        let (state, controls) = {
            let record = self
                .windows
                .get_mut(&window)
                .ok_or(WmError::UnknownWindow(window))?;
            let types = if visible {
                record.requested_visible_types | inset_type::IME
            } else {
                record.requested_visible_types & !inset_type::IME
            };
            record.requested_visible_types = types;
            let state = reference_insets_state(&display, visible);
            let mut controls = record.insets_controls.clone();
            // The control is what the app's `InsetsController` reads first: an
            // IME control that says `initially_visible=false` while the state
            // says visible makes the framework request `hideSoftInput` right
            // back (measured: two `hideSoftInput` calls per show). Keep the
            // control's flag in step with the state.
            for control in &mut controls.0 {
                if control.type_ == inset_type::IME {
                    control.initially_visible = visible;
                }
            }
            record.insets_controls = controls.clone();
            record.insets_state = state.clone();
            (state, controls)
        };
        let token = None;
        if visible {
            self.callbacks.push(
                window,
                WmCallback::ShowInsets {
                    types: inset_type::IME,
                    from_ime: true,
                    token,
                },
            );
        } else {
            self.callbacks.push(
                window,
                WmCallback::HideInsets {
                    types: inset_type::IME,
                    from_ime: true,
                    token,
                },
            );
        }
        self.callbacks.push(
            window,
            WmCallback::InsetsControlChanged {
                state: Box::new(state),
                controls: Box::new(controls),
                ime: true,
            },
        );
        Ok(())
    }

    /// `IWindowSession.onRectangleOnScreenRequested` (oneway; recorded).
    pub fn on_rectangle_on_screen_requested(
        &mut self,
        window: WindowId,
        _rect: Option<RectI>,
    ) -> Result<()> {
        self.windows
            .get_mut(&window)
            .ok_or(WmError::UnknownWindow(window))
            .map(|_| ())
    }

    /// `IWindowManager.useBLAST`.
    #[must_use]
    pub const fn use_blast(&self) -> bool {
        true
    }

    /// `IWindowManager.getCurrentAnimatorScale`.
    #[must_use]
    pub const fn get_current_animator_scale(&self) -> f32 {
        1.0
    }

    /// `IWindowManager.hasNavigationBar`.
    #[must_use]
    pub fn has_navigation_bar(&self, display_id: i32) -> bool {
        display_id == self.display.id && self.display.has_navigation_bar()
    }

    /// `IWindowManager.isInTouchMode`.
    #[must_use]
    pub fn is_in_touch_mode(&self, display_id: i32) -> bool {
        display_id == self.display.id && self.touch_mode
    }

    /// `IWindowManager.setInTouchMode`.
    pub fn set_in_touch_mode(&mut self, in_touch_mode: bool, display_id: i32) {
        if display_id == self.display.id {
            self.touch_mode = in_touch_mode;
        }
    }

    /// The window currently holding focus.
    #[must_use]
    pub fn focused_window(&self) -> Option<&WindowRecord> {
        self.focus.and_then(|id| self.windows.get(&id))
    }

    /// The display's window layer stack: every live window with its real type,
    /// frame, flags and attachment, in AOSP z-order.
    ///
    /// This is the one description of the stack that composition (C1's layer
    /// placement), the UI tree and input hit-testing read, so an overlay cannot
    /// be drawn in one order and tapped in another.
    #[must_use]
    pub fn layer_stack(&self) -> crate::layers::WindowLayerStack {
        let mut stack = crate::layers::WindowLayerStack::new();
        for record in self.windows.values() {
            if !record.visible {
                continue;
            }
            // A sub-window's `LayoutParams.token` is the window it is attached
            // to; only a token that names a live window becomes the parent.
            let parent = record
                .params
                .token
                .filter(|t| self.windows.contains_key(&t.0))
                .map(|t| t.0);
            stack.upsert(crate::layers::LayerEntry {
                window: record.id,
                type_: record.params.type_,
                flags: record.params.flags,
                frame: record.frame,
                parent,
                seq: record.seq,
                // ACOS runs a normal (non-system) app: no `INTERNAL_SYSTEM_WINDOW`,
                // so the alert/system-overlay/system-error arms use their low
                // layers (WindowManagerPolicy.java:553,583,596).
                can_add_internal_system_window: false,
            });
        }
        stack
    }

    /// Re-index every window's `z` from the layer stack. Called after an add and
    /// after a remove, so `z` is always the stack position (band-major, then
    /// creation order, with sub-windows next to their parent).
    fn renumber_z(&mut self) {
        let stack = self.layer_stack();
        for record in self.windows.values_mut() {
            if let Some(z) = stack.z_of(record.id) {
                record.z = z;
            }
        }
    }

    /// The window a touch at display point `p` goes to: the topmost **touchable**
    /// window whose frame contains the point (`FLAG_NOT_TOUCHABLE` is skipped).
    #[must_use]
    pub fn hit_test(&self, point: PointI) -> Option<WindowId> {
        self.layer_stack().hit_test(point)
    }

    /// Every touchable window under `p`, topmost first (`InputDispatcher`'s own
    /// search order; the input host skips the ones with no live input channel).
    #[must_use]
    pub fn hit_test_candidates(&self, point: PointI) -> Vec<WindowId> {
        self.layer_stack().hit_test_candidates(point)
    }

    /// The window that may take focus: the topmost focusable window.
    #[must_use]
    pub fn focus_target(&self) -> Option<WindowId> {
        self.layer_stack().focus_target()
    }

    /// Compute the frame for `params` on this display.
    #[must_use]
    pub fn compute_frame(&self, params: &LayoutParams) -> RectI {
        compute_frame_for(&self.display, params)
    }

    /// A reference `MergedConfiguration` matching `display.txt`'s
    /// `overrideConfig` line.
    #[must_use]
    pub fn merged_configuration(&self) -> MergedConfiguration {
        reference_merged_configuration(&self.display)
    }

    /// `WindowManagerService.addWindowToken(token, TYPE_ACCESSIBILITY_OVERLAY,
    /// displayId)`: the accessibility manager service created an overlay window
    /// token, and a `TYPE_ACCESSIBILITY_OVERLAY` window may now be added on it.
    pub fn add_overlay_token(&mut self, token: BinderId) {
        self.overlay_tokens.insert(token.0);
    }

    /// `WindowManagerService.removeWindowToken`: the service is gone, so the
    /// token stops authorising overlays.
    pub fn remove_overlay_token(&mut self, token: BinderId) {
        self.overlay_tokens.remove(&token.0);
    }

    /// True when `token` is an accessibility overlay token this state created.
    #[must_use]
    pub fn is_overlay_token(&self, token: BinderId) -> bool {
        self.overlay_tokens.contains(&token.0)
    }

    /// Set or update the multi-window bounds and windowing mode of `task_id`
    /// (`docs/MULTIWINDOW_PLAN.md` §4.1).
    ///
    /// When `windowing_mode` is fullscreen/undefined, clears any task override
    /// for `task_id` so subsequent relayouts return the exact fullscreen bytes.
    pub fn set_task_bounds(
        &mut self,
        task_id: i32,
        bounds: RectI,
        windowing_mode: i32,
    ) -> TaskRecord {
        let record = self.display.task_record(task_id, windowing_mode, bounds);
        if record.is_multi_window() {
            self.tasks.insert(task_id, record);
            self.active_task = Some(task_id);
            for (win_id, win) in &self.windows {
                if win.kind == crate::layers::WindowKind::Application {
                    self.window_task.entry(*win_id).or_insert(task_id);
                }
            }
        } else {
            self.clear_task_bounds(task_id);
        }
        record
    }

    /// Clear any multi-window bounds override for `task_id`, restoring fullscreen
    /// relayout and configuration bytes.
    pub fn clear_task_bounds(&mut self, task_id: i32) -> Option<TaskRecord> {
        let removed = self.tasks.remove(&task_id);
        self.window_task.retain(|_, tid| *tid != task_id);
        if self.active_task == Some(task_id) {
            self.active_task = self.tasks.keys().next().copied();
        }
        removed
    }

    /// Clear every task bounds override (e.g., when detaching `multiwindow`).
    pub fn clear_all_task_bounds(&mut self) {
        self.tasks.clear();
        self.window_task.clear();
        self.active_task = None;
    }

    /// Current task bounds override for `task_id`, if any.
    #[must_use]
    pub fn task_bounds(&self, task_id: i32) -> Option<TaskRecord> {
        self.tasks.get(&task_id).copied()
    }

    /// Explicitly associate `window` with `task_id`.
    pub fn bind_window_task(&mut self, window: WindowId, task_id: i32) {
        self.window_task.insert(window, task_id);
    }

    /// Resolve the task bounds that govern an existing window. System windows
    /// (overlays, IME, status/nav bars) never inherit task bounds.
    #[must_use]
    pub fn task_for_window(&self, window: WindowId) -> Option<TaskRecord> {
        let record = self.windows.get(&window)?;
        self.task_for_new_window(window, record.kind)
    }

    /// Resolve the task bounds for a window by id and [`crate::layers::WindowKind`].
    #[must_use]
    fn task_for_new_window(
        &self,
        window: WindowId,
        kind: crate::layers::WindowKind,
    ) -> Option<TaskRecord> {
        if !matches!(
            kind,
            crate::layers::WindowKind::Application | crate::layers::WindowKind::SubWindow
        ) {
            return None;
        }
        let task_id = self
            .window_task
            .get(&window)
            .copied()
            .or(self.active_task)?;
        self.tasks.get(&task_id).copied()
    }

    fn alloc_layer(&mut self) -> u64 {
        self.next_layer += 1;
        self.next_layer
    }
}

/// `WindowManager.LayoutParams.MATCH_PARENT`.
pub const MATCH_PARENT: i32 = -1;
/// `WindowManager.LayoutParams.WRAP_CONTENT`.
pub const WRAP_CONTENT: i32 = -2;
/// `WindowLayout.UNSPECIFIED_LENGTH`: no client-measured size yet.
pub const UNSPECIFIED_LENGTH: i32 = -1;

/// Frame computation for `params` without a client-measured size.
///
/// Equivalent to [`compute_frame_for_requested`] with [`UNSPECIFIED_LENGTH`]
/// requested dimensions, which is the real WMS's first-layout path
/// (`WindowLayout.computeFrames` falls back to the layout params / parent
/// frame).
#[must_use]
pub fn compute_frame_for(display: &DisplaySpec, params: &LayoutParams) -> RectI {
    compute_frame_for_requested(display, params, UNSPECIFIED_LENGTH, UNSPECIFIED_LENGTH)
}

/// Apply one bar rect to `frame` on the edge where it intersects `bounds`
/// (matching AOSP `InsetsSource.calculateInsets` + `WindowLayout.computeFrames`).
fn apply_bar_inset(bar: RectI, bounds: RectI, sides: i32, frame: &mut RectI) {
    if bar.is_empty() || bounds.is_empty() {
        return;
    }
    let inter = RectI::new(
        bar.left.max(bounds.left),
        bar.top.max(bounds.top),
        bar.right.min(bounds.right),
        bar.bottom.min(bounds.bottom),
    );
    if inter.is_empty() {
        return;
    }
    match (
        inter.width() == bounds.width(),
        inter.height() == bounds.height(),
    ) {
        (true, false) => {
            if inter.top == bounds.top && sides & 0x02 != 0 {
                frame.top = frame.top.max(inter.bottom);
            } else if inter.bottom == bounds.bottom && sides & 0x08 != 0 {
                frame.bottom = frame.bottom.min(inter.top);
            }
        }
        (false, true) => {
            if inter.left == bounds.left && sides & 0x01 != 0 {
                frame.left = frame.left.max(inter.right);
            } else if inter.right == bounds.right && sides & 0x04 != 0 {
                frame.right = frame.right.min(inter.left);
            }
        }
        _ => {}
    }
}

/// Subtract the `InsetsState` sources a window asked to fit, exactly as
/// `WindowLayout.computeFrames` does with
/// `state.calculateInsets(windowBounds, attrs.getFitInsetsTypes(), ...)`.
///
/// A zero `fitInsetsSides` (the field's pre-initialised value in
/// [`LayoutParams::default`]) means `Side.all()`, which is the framework's
/// own default.
fn fit_insets_frame(display: &DisplaySpec, params: &LayoutParams, bounds: RectI) -> RectI {
    let types = params.fit_insets_types;
    if types == 0 {
        return bounds;
    }
    let sides = if params.fit_insets_sides == 0 {
        0x0f // WindowInsets.Side.LEFT|TOP|RIGHT|BOTTOM
    } else {
        params.fit_insets_sides
    };
    let mut frame = bounds;
    if types & inset_type::STATUS_BAR != 0 {
        apply_bar_inset(display.bars.status_bar, bounds, sides, &mut frame);
    }
    if types & inset_type::NAVIGATION_BAR != 0 {
        apply_bar_inset(display.bars.nav_bar, bounds, sides, &mut frame);
    }
    if types & inset_type::SYSTEM_GESTURES != 0 {
        apply_bar_inset(display.bars.system_gestures_left, bounds, sides, &mut frame);
        apply_bar_inset(display.bars.system_gestures_right, bounds, sides, &mut frame);
    }
    frame.right = frame.right.max(frame.left);
    frame.bottom = frame.bottom.max(frame.top);
    frame
}

/// `WindowLayout.computeFrames` for the fullscreen, `compatScale == 1` case.
///
/// Equivalent to [`compute_frame_for_requested_in_task`] with `task = None`.
#[must_use]
pub fn compute_frame_for_requested(
    display: &DisplaySpec,
    params: &LayoutParams,
    requested_width: i32,
    requested_height: i32,
) -> RectI {
    compute_frame_for_requested_in_task(display, params, requested_width, requested_height, None)
}

/// `WindowLayout.computeFrames` with an optional multi-window [`TaskRecord`]
/// parent frame (`compatScale == 1`).
///
/// This mirrors `frameworks/base/core/java/android/view/WindowLayout.java`:
/// a `MATCH_PARENT` axis takes the parent-frame extent, a `WRAP_CONTENT` axis
/// takes the client-reported `requestedWidth`/`requestedHeight` (the real WMS's
/// `WindowState.mRequestedWidth/mRequestedHeight`), and a fixed size is used
/// verbatim. The frame is then positioned with `Gravity.apply` inside the
/// parent frame and nudged on-screen with `Gravity.applyDisplay`.
///
/// When `task` is `Some(t)` in multi-window mode, the parent frame is
/// `t.window_frame(display.insets_mode)` instead of `display.window_frame()`.
/// When `task` is `None` or fullscreen, the frame is identical to fullscreen.
#[must_use]
pub fn compute_frame_for_requested_in_task(
    display: &DisplaySpec,
    params: &LayoutParams,
    requested_width: i32,
    requested_height: i32,
    task: Option<&TaskRecord>,
) -> RectI {
    let window_bounds = match task {
        Some(t) if t.is_multi_window() => t.window_frame(display.insets_mode),
        _ => display.window_frame(),
    };
    let out_display_frame = match display.insets_mode {
        InsetsMode::Carve => window_bounds,
        InsetsMode::Report => fit_insets_frame(display, params, window_bounds),
    };
    let out_parent_frame = out_display_frame;
    let pw = out_parent_frame.width();
    let ph = out_parent_frame.height();

    // `WindowLayout.computeFrames`: `rw`/`rh` are UNSPECIFIED_LENGTH until the
    // view hierarchy has been measured; then fall back to the layout params or
    // the parent frame.
    let mut rw = requested_width;
    let mut rh = requested_height;
    if rw == UNSPECIFIED_LENGTH {
        rw = if params.width >= 0 { params.width } else { pw };
    }
    if rh == UNSPECIFIED_LENGTH {
        rh = if params.height >= 0 {
            params.height
        } else {
            ph
        };
    }

    let w = if params.width == MATCH_PARENT { pw } else { rw };
    let h = if params.height == MATCH_PARENT {
        ph
    } else {
        rh
    };

    let x = params.x + (params.horizontal_margin * pw as f32) as i32;
    let y = params.y + (params.vertical_margin * ph as f32) as i32;

    let mut out = RectI::default();
    gravity_apply(params.gravity, w, h, out_parent_frame, x, y, &mut out);
    gravity_apply_display(params.gravity, out_display_frame, &mut out);
    out
}

/// `Gravity.AXIS_PULL_BEFORE`.
const AXIS_PULL_BEFORE: i32 = 0x0002;
/// `Gravity.AXIS_PULL_AFTER`.
const AXIS_PULL_AFTER: i32 = 0x0004;
/// `Gravity.AXIS_CLIP`.
const AXIS_CLIP: i32 = 0x0008;
/// `Gravity.DISPLAY_CLIP_VERTICAL`.
const DISPLAY_CLIP_VERTICAL: i32 = 0x1000_0000;
/// `Gravity.DISPLAY_CLIP_HORIZONTAL`.
const DISPLAY_CLIP_HORIZONTAL: i32 = 0x0100_0000;

/// `android.view.Gravity.apply(int, int, int, Rect, int, int, Rect)` (LTR).
fn gravity_apply(
    gravity: i32,
    w: i32,
    h: i32,
    container: RectI,
    x_adj: i32,
    y_adj: i32,
    out: &mut RectI,
) {
    // Horizontal axis. The mask keeps only the pull bits; the `AXIS_SPECIFIED`
    // bit selects start/end alignment over centering.
    match gravity & (AXIS_PULL_BEFORE | AXIS_PULL_AFTER) {
        0 => {
            out.left = container.left + (container.width() - w) / 2 + x_adj;
            out.right = out.left + w;
            if gravity & AXIS_CLIP == AXIS_CLIP {
                out.left = out.left.max(container.left);
                out.right = out.right.min(container.right);
            }
        }
        value if value == AXIS_PULL_BEFORE => {
            out.left = container.left + x_adj;
            out.right = out.left + w;
            if gravity & AXIS_CLIP == AXIS_CLIP {
                out.right = out.right.min(container.right);
            }
        }
        value if value == AXIS_PULL_AFTER => {
            out.right = container.right - x_adj;
            out.left = out.right - w;
            if gravity & AXIS_CLIP == AXIS_CLIP {
                out.left = out.left.max(container.left);
            }
        }
        _ => {
            out.left = container.left + x_adj;
            out.right = container.right + x_adj;
        }
    }

    // Vertical axis (`AXIS_Y_SHIFT == 4`).
    match gravity & ((AXIS_PULL_BEFORE | AXIS_PULL_AFTER) << 4) {
        0 => {
            out.top = container.top + (container.height() - h) / 2 + y_adj;
            out.bottom = out.top + h;
            if gravity & (AXIS_CLIP << 4) == AXIS_CLIP << 4 {
                out.top = out.top.max(container.top);
                out.bottom = out.bottom.min(container.bottom);
            }
        }
        value if value == AXIS_PULL_BEFORE << 4 => {
            out.top = container.top + y_adj;
            out.bottom = out.top + h;
            if gravity & (AXIS_CLIP << 4) == AXIS_CLIP << 4 {
                out.bottom = out.bottom.min(container.bottom);
            }
        }
        value if value == AXIS_PULL_AFTER << 4 => {
            out.bottom = container.bottom - y_adj;
            out.top = out.bottom - h;
            if gravity & (AXIS_CLIP << 4) == AXIS_CLIP << 4 {
                out.top = out.top.max(container.top);
            }
        }
        _ => {
            out.top = container.top + y_adj;
            out.bottom = container.bottom + y_adj;
        }
    }
}

/// `android.view.Gravity.applyDisplay(int, Rect, Rect)`.
fn gravity_apply_display(gravity: i32, display: RectI, obj: &mut RectI) {
    if gravity & DISPLAY_CLIP_VERTICAL != 0 {
        obj.top = obj.top.max(display.top);
        obj.bottom = obj.bottom.min(display.bottom);
    } else {
        let mut off = 0;
        if obj.top < display.top {
            off = display.top - obj.top;
        } else if obj.bottom > display.bottom {
            off = display.bottom - obj.bottom;
        }
        if off != 0 {
            if obj.height() > display.height() {
                obj.top = display.top;
                obj.bottom = display.bottom;
            } else {
                obj.top += off;
                obj.bottom += off;
            }
        }
    }

    if gravity & DISPLAY_CLIP_HORIZONTAL != 0 {
        obj.left = obj.left.max(display.left);
        obj.right = obj.right.min(display.right);
    } else {
        let mut off = 0;
        if obj.left < display.left {
            off = display.left - obj.left;
        } else if obj.right > display.right {
            off = display.right - obj.right;
        }
        if off != 0 {
            if obj.width() > display.width() {
                obj.left = display.left;
                obj.right = display.right;
            } else {
                obj.left += off;
                obj.right += off;
            }
        }
    }
}

/// `true` when `frame` touches all display edges (fullscreen).
#[must_use]
pub fn is_fullscreen(display: &DisplaySpec, frame: RectI) -> bool {
    frame.left <= display.frame.left
        && frame.top <= display.frame.top
        && frame.right >= display.frame.right
        && frame.bottom >= display.frame.bottom
}

/// A minimal `InsetsState` with just the display frame (error replies).
#[must_use]
pub fn empty_insets_state(display: &DisplaySpec) -> InsetsState {
    InsetsState {
        display_frame: display.frame,
        display_cutout: Cutout::NoCutout,
        rounded_corners: None,
        rounded_corner_frame: RectI::default(),
        privacy_indicator_bounds: None,
        display_shape: Some(DisplayShape::none()),
        sources: vec![],
    }
}

/// The exact ten `InsetsSource` rows from `display.txt`.
///
/// ROT6: when the display is rotated, one extra status-bars source carries the
/// **config status strip** — `DisplayPolicy.DecorInsetsInfo.configInsets`'s top
/// strip ([`DisplaySpec::config_insets`], 24 dp = 63 px on the reference phone).
/// The guest only ever sees insets through `InsetsState` (the config/non-decor
/// split is `WindowManagerService`-internal), so the strip the app's own decor
/// draws its status bar background into has to arrive as a source: on the
/// reference phone's ROTATION_90 tree the app reserves *both* the 128 px left
/// strip (the physical bar, `nonDecorInsets.left`) and a 63 px top strip
/// (`configInsets.top`) — `ui_after_recreate.xml`: root `[128,0][2400,1080]`,
/// `statusBarBackground [128,0][2400,63]`, `action_bar [128,63]`,
/// `content [128,189][2400,1017]`. It has no surface of its own, so no
/// `InsetsSourceControl` is emitted for it.
#[must_use]
pub fn reference_insets_state(display: &DisplaySpec, ime_visible: bool) -> InsetsState {
    let b = display.bars;
    let ime_frame = if ime_visible {
        b.ime_visible
    } else {
        RectI::new(0, 0, 0, 0)
    };
    let mut sources = vec![
        InsetsSource::new(
            0x9b1a_0000u32 as i32,
            inset_type::STATUS_BAR,
            b.status_bar,
            true,
        ),
        InsetsSource::new(
            0xfc05_0001u32 as i32,
            inset_type::NAVIGATION_BAR,
            b.nav_bar,
            true,
        ),
        InsetsSource {
            id: 3,
            type_: inset_type::IME,
            frame: ime_frame,
            visible_frame: Some(b.ime_visible),
            visible: ime_visible,
            flags: 0,
            insets_rounded_corner_frame: false,
        },
        InsetsSource::new(27, inset_type::DISPLAY_CUTOUT, b.display_cutout, true),
        InsetsSource::new(
            0x9b1a_0005u32 as i32,
            inset_type::MANDATORY_SYSTEM_GESTURES,
            b.mandatory_system_gestures_top,
            true,
        ),
        InsetsSource::new(
            0xfc05_0005u32 as i32,
            inset_type::MANDATORY_SYSTEM_GESTURES,
            b.mandatory_system_gestures_bottom,
            true,
        ),
        InsetsSource::new(
            0xfc05_0004u32 as i32,
            inset_type::SYSTEM_GESTURES,
            b.system_gestures_left,
            true,
        ),
        InsetsSource::new(
            0xfc05_0024u32 as i32,
            inset_type::SYSTEM_GESTURES,
            b.system_gestures_right,
            true,
        ),
        InsetsSource::new(
            0x9b1a_0006u32 as i32,
            inset_type::TOP_TAPPABLE_ELEMENT,
            b.status_bar_tappable,
            true,
        ),
        InsetsSource::new(
            0xfc05_0006u32 as i32,
            inset_type::BOTTOM_TAPPABLE_ELEMENT,
            b.nav_bar_tappable,
            true,
        ),
    ];
    if !b.status_bar_config.is_empty() {
        // The config strip is a status-bars inset the app's decor consumes; it is
        // a second source of the same type, which `InsetsState.calculateInsets`
        // takes the per-edge maximum of (`Insets.max`), so the window sees the
        // physical bar's edge *and* the config strip.
        sources.push(InsetsSource::new(
            CONFIG_STATUS_BAR_SOURCE_ID,
            inset_type::STATUS_BAR,
            b.status_bar_config,
            true,
        ));
    }
    InsetsState {
        display_frame: display.frame,
        display_cutout: reference_cutout(display),
        rounded_corners: Some(reference_rounded_corners()),
        rounded_corner_frame: RectI::new(0, 0, 0, 0),
        privacy_indicator_bounds: Some(PrivacyIndicatorBounds {
            static_bounds: vec![RectI::new(827, 0, 1043, 128)],
            rotation: 0,
        }),
        // The framework dereferences `mDisplayShape`, so it must be a real
        // parcel object; `DisplayShape.NONE` is the rectangular-display value.
        display_shape: Some(DisplayShape::none()),
        sources,
    }
}

/// The reference `DisplayCutout` (safe inset 128 px, one 118x128 cutout rect).
#[must_use]
pub fn reference_cutout(display: &DisplaySpec) -> Cutout {
    Cutout::Present(DisplayCutout {
        safe_insets: Some(RectI::new(0, display.bars.status_bar.height(), 0, 0)),
        bounds: vec![
            RectI::new(0, 0, 0, 0),
            RectI::new(492, 0, 610, 128),
            RectI::new(0, 0, 0, 0),
            RectI::new(0, 0, 0, 0),
        ],
        waterfall_insets: Some(InsetsI::default()),
        path_parser_info: CutoutPathParserInfo {
            display_width: display.frame.width(),
            display_height: display.frame.height(),
            physical_display_width: display.frame.width(),
            physical_display_height: display.frame.height(),
            density: display.density,
            cutout_spec: "M 507,64 a 33,33 0 1 0 66,0 33,33 0 1 0 -66,0 Z @left".to_string(),
            rotation: 0,
            scale: 1.0,
            physical_pixel_display_size_ratio: 1.0,
        },
    })
}

/// The reference four 28 px rounded corners.
#[must_use]
pub fn reference_rounded_corners() -> RoundedCorners {
    RoundedCorners::Corners(vec![
        RoundedCorner {
            position: 0,
            radius: 28,
            center: PointI { x: 28, y: 28 },
        },
        RoundedCorner {
            position: 1,
            radius: 28,
            center: PointI { x: 1052, y: 28 },
        },
        RoundedCorner {
            position: 2,
            radius: 28,
            center: PointI { x: 1052, y: 2372 },
        },
        RoundedCorner {
            position: 3,
            radius: 28,
            center: PointI { x: 28, y: 2372 },
        },
    ])
}

/// The three `InsetsSourceControl` entries from the dumpsys `Control map`.
#[must_use]
pub fn reference_insets_controls(
    display: &DisplaySpec,
    base_layer: u64,
) -> InsetsSourceControlArray {
    let leash = |id: u64, label: &str| SurfaceControlInfo {
        name: format!("Surface(name={label} animation-leash of insets_animation)/@0x{id:x}"),
        width: display.frame.width(),
        height: display.frame.height(),
        handle: Some(base_layer + id),
        // Insets leashes stay W3 placeholders: no compositor layer exists for
        // them, so the placeholder binders (and their structured
        // `UNKNOWN_BINDER`/no-layer behaviour) are the honest answer.
        client: None,
        layer_binder: None,
    };
    InsetsSourceControlArray(vec![
        InsetsSourceControl {
            id: 3,
            type_: inset_type::IME,
            leash: Some(leash(3, "InputMethod")),
            initially_visible: false,
            surface_position: Some(PointI { x: 0, y: 128 }),
            insets_hint: Some(InsetsI::new(0, 0, 0, 883)),
            skip_animation_once: false,
        },
        InsetsSourceControl {
            id: 0x9b1a_0000u32 as i32,
            type_: inset_type::STATUS_BAR,
            leash: Some(leash(0, "StatusBar")),
            initially_visible: true,
            surface_position: Some(PointI { x: 0, y: 0 }),
            insets_hint: Some(InsetsI::new(0, 128, 0, 0)),
            skip_animation_once: false,
        },
        InsetsSourceControl {
            id: 0xfc05_0001u32 as i32,
            type_: inset_type::NAVIGATION_BAR,
            leash: Some(leash(1, "NavigationBar0")),
            initially_visible: true,
            surface_position: Some(PointI { x: 0, y: 2274 }),
            insets_hint: Some(InsetsI::new(0, 0, 0, 63)),
            skip_animation_once: false,
        },
    ])
}

/// `Configuration.screenLayout` for the measured portrait display.
///
/// AOSP does not leave the size/long bits undefined: `ResourcesManager`
/// calls `Configuration.reduceScreenLayout(screenLayout, longSizeDp,
/// shortSizeDp)` after it derives the dp bounds from the display metrics.
/// For the API-34 reference (`ldltr nrml long`, 842x411 dp) that is
/// `LAYOUTDIR_LTR | SIZE_NORMAL | LONG_YES` = `0x62`.
///
/// The old value `0x1a` had size nibble `0xa` (not a
/// `SCREENLAYOUT_SIZE_*` constant), and the launch guest's bind configuration
/// left the whole field at `0`. With no valid size qualifier,
/// `ResTable_config::match` treated the `xlarge`-qualified resources as
/// applicable, so `@dimen/dialog_min_width_minor` resolved to its `(xlarge)`
/// `72%` instead of the default `95%` — a `WRAP_CONTENT` AlertDialog then
/// measured 776 px instead of the stock 1024 px.
#[must_use]
pub const fn screen_layout_for_dp(long_size_dp: i32, short_size_dp: i32) -> i32 {
    // `Configuration.reduceScreenLayout` (AOSP), low byte only: the
    // `SCREENLAYOUT_COMPAT_NEEDED` bit never reaches `ResTable_config`.
    let size = if long_size_dp < 470 {
        0x01 // SIZE_SMALL
    } else if long_size_dp >= 960 && short_size_dp >= 720 {
        0x04 // SIZE_XLARGE
    } else if long_size_dp >= 640 && short_size_dp >= 480 {
        0x03 // SIZE_LARGE
    } else {
        0x02 // SIZE_NORMAL
    };
    let long = long_size_dp >= 470 && ((long_size_dp * 3) / 5) >= (short_size_dp - 1);
    let long_bits = if long { 0x20 } else { 0x10 }; // LONG_YES / LONG_NO
    0x40 /* LAYOUTDIR_LTR */ | size | long_bits
}

/// The reference display's `screenLayout` (`0x62`).
pub const REFERENCE_SCREEN_LAYOUT: i32 = screen_layout_for_dp(842, 411);

/// The `MergedConfiguration` matching the measured `overrideConfig`.
#[must_use]
pub fn reference_merged_configuration(display: &DisplaySpec) -> MergedConfiguration {
    let cfg = Configuration {
        font_scale: 1.0,
        mcc: 310,
        mnc: 260,
        locale_list: Some("en-US".to_string()),
        user_set_locale: false,
        touchscreen: 3,     // TOUCHSCREEN_FINGER
        keyboard: 2,        // KEYBOARD_QWERTY
        keyboard_hidden: 1, // KEYBOARDHIDDEN_YES
        hard_keyboard_hidden: 1,
        navigation: 1, // NAVIGATION_DPAD
        navigation_hidden: 1,
        orientation: if display.is_landscape() { 2 } else { 1 },
        screen_layout: REFERENCE_SCREEN_LAYOUT, // ldltr nrml long (measured `display.txt`)
        color_mode: 0,                          // HDR_UNDEFINED / sRGB
        ui_mode: 0x11,                          // UI_MODE_TYPE_NORMAL | NIGHT_NO
        // W7: under `report` the configuration describes the full display,
        // exactly like the device (`sw432dp w432dp h960dp` on the Moto G35 at
        // 400 dpi). `carve` keeps the measured API-34 application-area values
        // so the frozen baselines are unchanged.
        screen_width_dp: if display.insets_mode.is_report() || display.rotation != 0 {
            display.screen_width_dp()
        } else {
            411
        },
        screen_height_dp: if display.insets_mode.is_report() || display.rotation != 0 {
            display.screen_height_dp()
        } else {
            842
        },
        smallest_screen_width_dp: if display.insets_mode.is_report() || display.rotation != 0 {
            display.screen_width_dp().min(display.screen_height_dp())
        } else {
            411
        },
        density_dpi: display.density_dpi as i32,
        compat_screen_width_dp: if display.insets_mode.is_report() || display.rotation != 0 {
            display.screen_width_dp()
        } else {
            411
        },
        compat_screen_height_dp: if display.insets_mode.is_report() || display.rotation != 0 {
            display.screen_height_dp()
        } else {
            842
        },
        compat_smallest_screen_width_dp: if display.insets_mode.is_report() || display.rotation != 0
        {
            display.screen_width_dp().min(display.screen_height_dp())
        } else {
            411
        },
        window_configuration: WindowConfiguration {
            bounds: display.frame,
            app_bounds: Some(display.window_frame()),
            max_bounds: display.frame,
            windowing_mode: 1, // WINDOWING_MODE_FULLSCREEN
            activity_type: 0,  // ACTIVITY_TYPE_UNDEFINED
            always_on_top: 0,
            rotation: i32::from(display.rotation),
            display_windowing_mode: 1,
            display_rotation: i32::from(display.rotation),
        },
        assets_seq: 0,
        seq: 1,
        font_weight_adjustment: 0,
        grammatical_gender: 0,
    };
    MergedConfiguration {
        global: cfg.clone(),
        override_config: cfg.clone(),
        merged: cfg,
    }
}

/// Per-task `MergedConfiguration` (`docs/MULTIWINDOW_PLAN.md` §4.1).
///
/// Full-screen tasks (or `None`) return [`reference_merged_configuration`]
/// verbatim so every existing fullscreen gate produces byte-identical parcels.
/// Multi-window tasks keep `global` display-level and put the task's bounds,
/// `appBounds`, `windowingMode`, and task-relative `screenWidthDp` /
/// `screenHeightDp` in `override_config` and `merged` while keeping display-level
/// `screenLayout`, `orientation`, `smallestScreenWidthDp` and `densityDpi`.
#[must_use]
pub fn merged_configuration_for_task(
    display: &DisplaySpec,
    task: Option<&TaskRecord>,
) -> MergedConfiguration {
    let base = reference_merged_configuration(display);
    let Some(task) = task.filter(|t| t.is_multi_window()) else {
        return base;
    };
    let task_w_dp = display.px_to_dp(task.app_bounds.width());
    let task_h_dp = display.px_to_dp(task.app_bounds.height());
    let mut override_config = base.global.clone();
    override_config.screen_width_dp = task_w_dp;
    override_config.screen_height_dp = task_h_dp;
    override_config.compat_screen_width_dp = task_w_dp;
    override_config.compat_screen_height_dp = task_h_dp;
    override_config.window_configuration = WindowConfiguration {
        bounds: task.bounds,
        app_bounds: Some(task.app_bounds),
        max_bounds: display.frame,
        windowing_mode: task.windowing_mode,
        activity_type: 0, // ACTIVITY_TYPE_UNDEFINED
        always_on_top: 0,
        rotation: i32::from(display.rotation),
        display_windowing_mode: WINDOWING_MODE_FULLSCREEN,
        display_rotation: i32::from(display.rotation),
    };
    // Keep seq = 0 in the override so `Configuration.isOtherSeqNewer` always
    // accepts subsequent task bounds updates on the guest (`Configuration.java:2043`).
    override_config.seq = 0;

    let mut merged = override_config.clone();
    merged.seq = base.global.seq;

    MergedConfiguration {
        global: base.global,
        override_config,
        merged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_params() -> LayoutParams {
        LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity")
    }

    /// W47: the configuration the WMS sends (and the launch guest applies)
    /// must carry the stock screen-layout size/long bits. With them undefined
    /// (`0x1a` here, `0x0` in the guest's bind config before the fix) the
    /// resource matcher picked `values-xlarge` and
    /// `dialog_min_width_minor` resolved to 72% instead of 95%.
    /// ROT3: after a display rotation the WMS `MergedConfiguration` describes
    /// the rotated display: landscape orientation, swapped bounds/appBounds and
    /// the dp size of the new frame (carve included, which keeps the frozen
    /// portrait baseline only at rotation 0), with density unchanged.
    #[test]
    fn rotated_display_reports_a_landscape_merged_configuration() {
        let portrait = DisplaySpec::reference_with_insets(InsetsMode::Carve);
        let port = reference_merged_configuration(&portrait);
        assert_eq!(port.global.orientation, 1, "portrait before the rotation");
        assert_eq!(port.global.screen_width_dp, 411, "frozen carve baseline");
        assert_eq!(port.global.screen_height_dp, 842);

        let land = portrait.rotated(1);
        assert_eq!(land.frame.width(), 2400, "frame is swapped");
        assert_eq!(land.frame.height(), 1080);
        let cfg = reference_merged_configuration(&land);
        assert_eq!(cfg.global.orientation, 2, "ORIENTATION_LANDSCAPE");
        assert_eq!(cfg.global.screen_width_dp, 914);
        assert_eq!(cfg.global.screen_height_dp, 411);
        // The smallest width is a display property: it does not rotate.
        assert_eq!(cfg.global.smallest_screen_width_dp, 411);
        assert_eq!(cfg.global.density_dpi, port.global.density_dpi);
        assert_eq!(cfg.global.window_configuration.bounds, land.frame);
        assert_eq!(cfg.global.window_configuration.rotation, 1);
        assert_eq!(cfg.global.window_configuration.display_rotation, 1);
        // 180 degrees is portrait again with a mirrored configuration.
        let half = reference_merged_configuration(&portrait.rotated(2));
        assert_eq!(half.global.orientation, 1);
        assert_eq!(half.global.window_configuration.rotation, 2);
    }

    #[test]
    fn reference_screen_layout_is_normal_long_not_xlarge() {
        assert_eq!(REFERENCE_SCREEN_LAYOUT, 0x62);
        assert_eq!(REFERENCE_SCREEN_LAYOUT & 0x0f, 0x02, "SIZE_NORMAL");
        assert_eq!(REFERENCE_SCREEN_LAYOUT & 0x30, 0x20, "LONG_YES");
        assert_eq!(REFERENCE_SCREEN_LAYOUT & 0xc0, 0x40, "LAYOUTDIR_LTR");
        assert_ne!(
            REFERENCE_SCREEN_LAYOUT & 0x0f,
            0x04,
            "must not be SIZE_XLARGE"
        );

        // Negative check: the pre-fix values really are different. `0x1a` is
        // not a valid size (0xa), and the guest's `0` is SIZE_UNDEFINED; both
        // let the xlarge qualifier win, so neither can satisfy this test.
        for pre_fix in [0x1a, 0x00] {
            assert_ne!(pre_fix, REFERENCE_SCREEN_LAYOUT);
            assert_ne!(pre_fix & 0x0f, 0x02, "pre-fix size nibble {pre_fix:#x}");
        }

        // AOSP thresholds, so the helper is not a one-value table.
        assert_eq!(screen_layout_for_dp(400, 300) & 0x0f, 0x01); // SMALL
        assert_eq!(screen_layout_for_dp(700, 500) & 0x0f, 0x03); // LARGE
        assert_eq!(screen_layout_for_dp(1000, 800) & 0x0f, 0x04); // XLARGE
    }

    /// MODA11Y5: AOSP `WindowManagerService.addWindow` accepts a
    /// `TYPE_ACCESSIBILITY_OVERLAY` window only on the accessibility manager's
    /// own overlay window token (`addWindow` 1630-1640, `sanitizeWindowType`
    /// 9914-9928). Without the token check any app could add a window into the
    /// accessibility overlay layer, above every activity.
    #[test]
    fn accessibility_overlay_needs_the_accessibility_overlay_token() {
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        let token = BinderId(0x0A11_0000_0000_7001);
        let mut params = LayoutParams::application("dev.acos.fixtures.a11yprobe", "overlay");
        params.type_ = crate::layers::TYPE_ACCESSIBILITY_OVERLAY;
        params.token = Some(token);

        // No token was created: the add is refused, exactly like AOSP's
        // `ADD_BAD_APP_TOKEN`, and no window is created.
        let refused = wm
            .add_to_display_as_user(sess, BinderId(0x7001), params.clone(), 0)
            .expect("the add path answers, it does not error");
        assert_eq!(refused.result, ADD_BAD_APP_TOKEN);
        assert!(wm.windows.is_empty(), "a refused add creates no window");

        // A foreign token is still refused (the token is the authority, not the
        // window type alone).
        let mut foreign = params.clone();
        foreign.token = Some(BinderId(0xBAD0_0000));
        let refused = wm
            .add_to_display_as_user(sess, BinderId(0x7002), foreign, 0)
            .expect("the add path answers");
        assert_eq!(refused.result, ADD_BAD_APP_TOKEN);

        // `addWindowToken`: now the add is accepted and lands in the
        // accessibility overlay policy layer (31).
        wm.add_overlay_token(token);
        assert!(wm.is_overlay_token(token));
        let added = wm
            .add_to_display_as_user(sess, BinderId(0x7003), params, 0)
            .expect("the add path answers");
        assert_eq!(added.result, ADD_OKAY);
        let record = wm.windows.get(&0x7003).expect("the overlay window");
        assert_eq!(record.kind, crate::layers::WindowKind::AccessibilityOverlay);
        assert_eq!(
            crate::layers::policy_layer(crate::layers::TYPE_ACCESSIBILITY_OVERLAY, false),
            31,
            "the overlay's LAYER1 policy layer"
        );

        // `removeWindowToken` (the service went away) revokes the authority.
        wm.remove_overlay_token(token);
        let mut again = LayoutParams::application("dev.acos.fixtures.a11yprobe", "overlay");
        again.type_ = crate::layers::TYPE_ACCESSIBILITY_OVERLAY;
        again.token = Some(token);
        let refused = wm
            .add_to_display_as_user(sess, BinderId(0x7004), again, 0)
            .expect("the add path answers");
        assert_eq!(refused.result, ADD_BAD_APP_TOKEN);
    }

    #[test]
    fn add_returns_measured_app_frame_and_insets() {
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        let r = wm
            .add_to_display_as_user(sess, BinderId(0x1111), app_params(), 0)
            .unwrap();
        assert_eq!(r.result, ADD_OKAY);
        assert_eq!(r.out_frame, RectI::new(0, 128, 1080, 2337));
        assert_eq!(
            r.insets_state
                .visible_insets_for(inset_type::STATUS_BAR)
                .top,
            128
        );
        assert_eq!(
            r.insets_state
                .visible_insets_for(inset_type::NAVIGATION_BAR)
                .bottom,
            63
        );
        assert!(matches!(r.insets_state.display_cutout, Cutout::Present(_)));
    }

    #[test]
    fn relayout_is_stable_and_first_time_only_once() {
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        wm.add_to_display_as_user(sess, BinderId(1), app_params(), 0)
            .unwrap();
        let a = wm.relayout(sess, 1, app_params(), -1).unwrap();
        let b = wm.relayout(sess, 1, app_params(), -1).unwrap();
        assert_eq!(a.frames.frame, b.frames.frame);
        assert_eq!(a.surface.handle, b.surface.handle);
        assert_ne!(a.result & RELAYOUT_RES_FIRST_TIME, 0);
        assert_eq!(b.result & RELAYOUT_RES_FIRST_TIME, 0);
        assert_eq!(b.result & RELAYOUT_RES_SURFACE_RESIZED, 0);
    }

    fn wrap_content_params() -> LayoutParams {
        LayoutParams {
            width: WRAP_CONTENT,
            height: WRAP_CONTENT,
            ..LayoutParams::default()
        }
    }

    /// A re-opened `AlertDialog`-style window: `WRAP_CONTENT` takes the
    /// client-reported measured size, and `Gravity.apply` centers it in the app
    /// bounds ([0,128][1080,2337]) because the default gravity is 0.
    #[test]
    fn wrap_content_window_uses_client_requested_size() {
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        wm.add_to_display_as_user(sess, BinderId(1), wrap_content_params(), 0)
            .unwrap();
        let r = wm
            .relayout_with_requested(sess, 1, wrap_content_params(), 776, 490, -1)
            .unwrap();
        // w=776, h=490 centered in [0,128][1080,2337]:
        //   x = (1080 - 776)/2 = 152, y = 128 + (2209 - 490)/2 = 987.
        assert_eq!(r.frames.frame, RectI::new(152, 987, 928, 1477));
        assert_eq!(r.surface.width, 776);
        assert_eq!(r.surface.height, 490);
    }

    /// The first layout has no measured size yet (`UNSPECIFIED_LENGTH`), so the
    /// frame falls back to the parent frame, matching `WindowLayout`'s
    /// `rw == UNSPECIFIED_LENGTH` branch.
    #[test]
    fn wrap_content_without_measure_falls_back_to_parent() {
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        wm.add_to_display_as_user(sess, BinderId(1), wrap_content_params(), 0)
            .unwrap();
        let r = wm
            .relayout_with_requested(sess, 1, wrap_content_params(), -1, -1, -1)
            .unwrap();
        assert_eq!(r.frames.frame, RectI::new(0, 128, 1080, 2337));
    }

    /// `MATCH_PARENT` ignores the requested size and takes the parent frame,
    /// so the app window frame is unchanged by the requested-size path.
    #[test]
    fn match_parent_window_ignores_requested_size() {
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        wm.add_to_display_as_user(sess, BinderId(1), app_params(), 0)
            .unwrap();
        let r = wm
            .relayout_with_requested(sess, 1, app_params(), 776, 490, -1)
            .unwrap();
        assert_eq!(r.frames.frame, RectI::new(0, 128, 1080, 2337));
    }

    #[test]
    fn duplicate_add_and_unknown_window_are_structured() {
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        wm.add_to_display_as_user(sess, BinderId(9), app_params(), 0)
            .unwrap();
        let dup = wm
            .add_to_display_as_user(sess, BinderId(9), app_params(), 0)
            .unwrap();
        assert_eq!(dup.result, ADD_DUPLICATE_ADD);
        assert!(matches!(
            wm.relayout(sess, 42, app_params(), -1),
            Err(WmError::UnknownWindow(42))
        ));
    }

    #[test]
    fn focus_and_visibility_callbacks_are_queued_in_order() {
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        wm.add_to_display_as_user(sess, BinderId(1), app_params(), 0)
            .unwrap();
        let first = wm.callbacks.pop().unwrap();
        assert!(matches!(
            first.callback,
            WmCallback::DispatchAppVisibility { visible: true }
        ));
        let second = wm.callbacks.pop().unwrap();
        assert!(matches!(
            second.callback,
            WmCallback::WindowFocusChanged { focused: true }
        ));
        let third = wm.callbacks.pop().unwrap();
        assert!(matches!(
            third.callback,
            WmCallback::InsetsControlChanged { ime: false, .. }
        ));
    }

    #[test]
    fn update_requested_visible_types_shows_ime() {
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        wm.add_to_display_as_user(sess, BinderId(1), app_params(), 0)
            .unwrap();
        wm.callbacks.clear();
        wm.update_requested_visible_types(1, inset_type::IME)
            .unwrap();
        let c = wm.callbacks.pop().unwrap();
        assert!(matches!(c.callback, WmCallback::ShowInsets { .. }));
        let state = &wm.windows.get(&1).unwrap().insets_state;
        assert!(state.sources.iter().any(|s| s.id == 3 && s.visible));
    }

    /// The rotated app's `InsetsState` carries the config strip as a status-bars
    /// source, so the guest's own `InsetsState.calculateInsets` produces
    /// `configInsets` for the window frame it is given.
    #[test]
    fn rotated_insets_state_delivers_the_config_strip() {
        use crate::codec::inset_type;
        let portrait = DisplaySpec::reference_1080x2400();
        let land = portrait.rotated(1);
        let p = reference_insets_state(&portrait, false);
        assert_eq!(p.sources.len(), 10, "the reference dump's ten rows");
        let l = reference_insets_state(&land, false);
        assert_eq!(l.sources.len(), 11, "ten rows plus the config strip");
        let strip = l
            .sources
            .iter()
            .find(|s| s.id == CONFIG_STATUS_BAR_SOURCE_ID)
            .expect("config strip source");
        assert_eq!(strip.type_, inset_type::STATUS_BAR);
        assert_eq!(strip.frame, RectI::new(0, 0, 2400, 63));
        assert!(strip.visible);
        // The physical left strip is still there: the config insets are both.
        let physical = l
            .sources
            .iter()
            .find(|s| s.id == 0x9b1a_0000u32 as i32)
            .expect("physical status bar");
        assert_eq!(physical.frame, RectI::new(0, 0, 128, 1080));
        // No control is invented for the strip: it owns no surface.
        let controls = reference_insets_controls(&land, 0x100);
        assert!(!controls
            .0
            .iter()
            .any(|c| c.id == CONFIG_STATUS_BAR_SOURCE_ID));
    }

    // ------------------------------------------------------- LAYER0 (stack)

    fn overlay_params(type_: i32, flags: i32) -> LayoutParams {
        LayoutParams {
            type_,
            flags,
            ..LayoutParams::application("dev.acos.fixtures.overlaystack", "Overlay")
        }
    }

    /// The layer stack the WMS reports is the order C1 draws in and the order
    /// input hit-tests against: the activity bottom, an application overlay
    /// above every activity window, the IME above the overlay.
    ///
    /// Before LAYER0 `z` was the insertion order, so an overlay added before the
    /// activity was drawn *below* it and the tree reported that order as truth.
    #[test]
    fn the_layer_stack_orders_application_overlay_and_ime_by_window_type() {
        use crate::layers::{TYPE_APPLICATION, TYPE_APPLICATION_OVERLAY, TYPE_INPUT_METHOD};
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        // Overlay and IME first, the activity last (a real launch order: the
        // system windows exist before the app's window does).
        wm.add_to_display_as_user(
            sess,
            BinderId(0x20),
            overlay_params(TYPE_INPUT_METHOD, 0),
            0,
        )
        .unwrap();
        wm.add_to_display_as_user(
            sess,
            BinderId(0x10),
            overlay_params(TYPE_APPLICATION_OVERLAY, 0),
            0,
        )
        .unwrap();
        wm.add_to_display_as_user(sess, BinderId(0x01), overlay_params(TYPE_APPLICATION, 0), 0)
            .unwrap();

        let rows = wm.layer_stack().describe();
        let ids: Vec<u64> = rows.iter().map(|r| r.0).collect();
        assert_eq!(ids, vec![0x01u64, 0x10, 0x20], "activity < overlay < IME");
        // `z` is the stack index and is what C1 receives as placement.z.
        assert_eq!(wm.windows[&0x01u64].z, 0);
        assert_eq!(wm.windows[&0x10].z, 1);
        assert_eq!(wm.windows[&0x20].z, 2);
        assert_eq!(wm.windows[&0x10].kind.name(), "application_overlay");
        assert_eq!(wm.windows[&0x20].kind.name(), "input_method");
        // Removing the bottom window renumbers the rest (no hole).
        wm.remove(0x01).unwrap();
        assert_eq!(wm.windows[&0x10u64].z, 0);
        assert_eq!(wm.windows[&0x20].z, 1);
    }

    /// A tap goes to the topmost **touchable** window at the point, so an
    /// overlay that sets `FLAG_NOT_TOUCHABLE` passes touches through to the
    /// activity below it, and a touchable one takes them.
    #[test]
    fn hit_test_and_focus_follow_the_stack_and_the_flags() {
        use crate::layers::{
            FLAG_NOT_FOCUSABLE, FLAG_NOT_TOUCHABLE, TYPE_APPLICATION, TYPE_APPLICATION_OVERLAY,
        };
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        let activity = wm
            .add_to_display_as_user(sess, BinderId(0x01), overlay_params(TYPE_APPLICATION, 0), 0)
            .unwrap()
            .window;
        // The overlay covers the top-left 300x300 of the display.
        let mut params = overlay_params(TYPE_APPLICATION_OVERLAY, 0);
        params.width = 300;
        params.height = 300;
        wm.add_to_display_as_user(sess, BinderId(0x10), params, 0)
            .unwrap();
        let overlay_frame = wm.windows[&0x10].frame;
        let inside = PointI::new(overlay_frame.left + 5, overlay_frame.top + 5);
        // Above every activity window: the overlay's own top-left corner is its.
        assert_eq!(wm.hit_test(inside), Some(0x10));
        assert_eq!(wm.focus_target(), Some(0x10));
        // Beside it: the activity.
        assert_eq!(wm.hit_test(PointI::new(900, 2000)), Some(activity));

        // Flip the overlay to NOT_TOUCHABLE | NOT_FOCUSABLE: the same point now
        // belongs to the activity, and focus goes back to it.
        wm.windows.get_mut(&0x10).unwrap().params.flags = FLAG_NOT_TOUCHABLE | FLAG_NOT_FOCUSABLE;
        assert_eq!(wm.hit_test(inside), Some(activity));
        assert_eq!(wm.focus_target(), Some(activity));
        // It is still drawn: composition order does not depend on touchability.
        assert_eq!(wm.layer_stack().z_of(0x10), Some(1));
    }

    /// WVTAP: the input host needs the **ordered** candidates under a point
    /// (`InputDispatcher` walks the stack from the top and takes the first
    /// window that can receive the event), not just the topmost one: a window
    /// whose input channel is gone must be skipped, never replaced by "the
    /// newest window".
    #[test]
    fn hit_test_candidates_lists_the_stack_from_the_top() {
        use crate::layers::{FLAG_NOT_TOUCHABLE, TYPE_APPLICATION, TYPE_APPLICATION_OVERLAY};
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        let activity = wm
            .add_to_display_as_user(sess, BinderId(0x01), overlay_params(TYPE_APPLICATION, 0), 0)
            .unwrap()
            .window;
        let overlay = wm
            .add_to_display_as_user(sess, BinderId(0x10), overlay_params(TYPE_APPLICATION_OVERLAY, 0), 0)
            .unwrap()
            .window;
        // Both windows cover the display; a point well inside both (a window
        // frame starts below the status bar under the `report` inset model).
        let frame = wm.windows[&activity].frame;
        let point = PointI::new(frame.left + 20, frame.top + 20);
        assert_eq!(
            wm.hit_test_candidates(point),
            vec![overlay, activity],
            "topmost first"
        );
        // A NOT_TOUCHABLE overlay is not a candidate at all (it passes through).
        wm.windows.get_mut(&overlay).unwrap().params.flags = FLAG_NOT_TOUCHABLE;
        assert_eq!(wm.hit_test_candidates(point), vec![activity]);
    }

    /// LAYER1: two windows in the same policy layer are ordered by creation,
    /// not by the numeric `IWindow` binder id. A dialog added after its
    /// activity is above it even when its binder id is smaller.
    #[test]
    fn a_same_layer_window_added_later_is_above_one_with_a_smaller_binder_id() {
        use crate::layers::TYPE_APPLICATION;
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        let activity = wm
            .add_to_display_as_user(sess, BinderId(0x99), overlay_params(TYPE_APPLICATION, 0), 0)
            .unwrap()
            .window;
        let dialog = wm
            .add_to_display_as_user(sess, BinderId(0x11), overlay_params(TYPE_APPLICATION, 0), 0)
            .unwrap()
            .window;
        assert_eq!(wm.windows[&activity].z, 0);
        assert_eq!(wm.windows[&dialog].z, 1);
        assert_eq!(wm.layer_stack().ordered()[1].window, dialog);
    }

    /// LAYER1: focus follows the layer stack on add and on remove, and the
    /// transition is published as `WindowFocusChanged(false)` for the window
    /// that lost focus and `(true)` for the one that gained it.
    #[test]
    fn focus_moves_with_the_layer_stack_on_add_and_remove() {
        use crate::callbacks::WmCallback;
        use crate::layers::{FLAG_NOT_FOCUSABLE, TYPE_APPLICATION, TYPE_APPLICATION_OVERLAY};
        let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
        let (sess, _b) = wm.open_session(None);
        let activity = wm
            .add_to_display_as_user(sess, BinderId(0x01), overlay_params(TYPE_APPLICATION, 0), 0)
            .unwrap()
            .window;
        assert_eq!(wm.focus, Some(activity));
        let dialog = wm
            .add_to_display_as_user(sess, BinderId(0x02), overlay_params(TYPE_APPLICATION, 0), 0)
            .unwrap()
            .window;
        assert_eq!(
            wm.focus,
            Some(dialog),
            "a dialog above the activity takes focus"
        );

        // A focusable overlay above the dialog takes focus too...
        let overlay = wm
            .add_to_display_as_user(
                sess,
                BinderId(0x03),
                overlay_params(TYPE_APPLICATION_OVERLAY, 0),
                0,
            )
            .unwrap()
            .window;
        assert_eq!(wm.focus, Some(overlay));

        // ...but a FLAG_NOT_FOCUSABLE overlay does not, even though it is on top.
        let passive = wm
            .add_to_display_as_user(
                sess,
                BinderId(0x04),
                overlay_params(TYPE_APPLICATION_OVERLAY, FLAG_NOT_FOCUSABLE),
                0,
            )
            .unwrap()
            .window;
        assert_eq!(wm.focus, Some(overlay));
        assert!(wm.windows[&passive].z > wm.windows[&overlay].z);

        let mut transitions = Vec::new();
        while let Some(pc) = wm.callbacks.pop() {
            if let WmCallback::WindowFocusChanged { focused } = pc.callback {
                transitions.push((pc.window, focused));
            }
        }
        // add(activity): activity true; add(dialog): activity false, dialog
        // true; add(overlay): dialog false, overlay true; add(passive): no
        // transition.
        assert_eq!(
            transitions,
            vec![
                (activity, true),
                (activity, false),
                (dialog, true),
                (dialog, false),
                (overlay, true)
            ]
        );

        // Removing the focused overlay publishes (true) for the topmost
        // focusable window that remains (the dialog). The removed window's own
        // channel is closed, so it gets no callback.
        wm.remove(overlay).unwrap();
        assert_eq!(wm.focus, Some(dialog));
        let mut after = Vec::new();
        while let Some(pc) = wm.callbacks.pop() {
            if let WmCallback::WindowFocusChanged { focused } = pc.callback {
                after.push((pc.window, focused));
            }
        }
        assert_eq!(after, vec![(dialog, true)]);
    }

    #[test]
    fn task_bounds_shrink_app_frame_and_clear_restores_exact_fullscreen_bytes() {
        use crate::codec::ParcelCodec;
        use crate::display::{SplitPosition, WINDOWING_MODE_FULLSCREEN, WINDOWING_MODE_MULTI_WINDOW};

        let display = DisplaySpec::reference_with_insets(InsetsMode::Report);
        let mut wm = WindowManager::new(display);
        let (sess, _) = wm.open_session(None);
        let win = wm
            .add_to_display_as_user(sess, BinderId(0x10), app_params(), 0)
            .unwrap()
            .window;

        let full_relayout = wm.relayout(sess, win, app_params(), 1).unwrap();
        assert_eq!(full_relayout.frames.frame, RectI::new(0, 0, 1080, 2400));
        assert_eq!(
            full_relayout.merged_configuration.override_config.window_configuration.windowing_mode,
            WINDOWING_MODE_FULLSCREEN
        );
        let mut full_parcel = acos_parcel::Parcel::new();
        full_relayout
            .merged_configuration
            .write_to_parcel(&mut full_parcel)
            .unwrap();
        let full_bytes = full_parcel.marshall();

        // Enter top 50% split: bounds [0, 0, 1080, 1200], app_bounds [0, 128, 1080, 1200].
        let top_half = display.split_rect(SplitPosition::Top, 0.5);
        assert_eq!(top_half, RectI::new(0, 0, 1080, 1200));
        let task = wm.set_task_bounds(DEFAULT_TASK_ID, top_half, WINDOWING_MODE_MULTI_WINDOW);
        assert_eq!(task.bounds, RectI::new(0, 0, 1080, 1200));
        assert_eq!(task.app_bounds, RectI::new(0, 128, 1080, 1200));
        assert_eq!(wm.task_bounds(DEFAULT_TASK_ID), Some(task));

        let split_relayout = wm.relayout(sess, win, app_params(), 2).unwrap();
        assert_eq!(split_relayout.frames.frame, RectI::new(0, 0, 1080, 1200));
        assert_eq!(
            split_relayout.result & RELAYOUT_RES_SURFACE_RESIZED,
            RELAYOUT_RES_SURFACE_RESIZED
        );
        // Global config stays display-level (FULLSCREEN); override & merged carry task bounds.
        assert_eq!(
            split_relayout.merged_configuration.global.window_configuration.windowing_mode,
            WINDOWING_MODE_FULLSCREEN
        );
        assert_eq!(
            split_relayout.merged_configuration.global.window_configuration.bounds,
            RectI::new(0, 0, 1080, 2400)
        );
        assert_eq!(
            split_relayout.merged_configuration.override_config.window_configuration.windowing_mode,
            WINDOWING_MODE_MULTI_WINDOW
        );
        assert_eq!(
            split_relayout.merged_configuration.override_config.window_configuration.bounds,
            RectI::new(0, 0, 1080, 1200)
        );
        assert_eq!(
            split_relayout.merged_configuration.override_config.window_configuration.app_bounds,
            Some(RectI::new(0, 128, 1080, 1200))
        );
        assert_eq!(
            split_relayout.merged_configuration.override_config.screen_width_dp,
            411
        );
        assert_eq!(
            split_relayout.merged_configuration.override_config.screen_height_dp,
            408
        );
        // Display-level qualifiers stay display-level so screenSize-only apps do not relaunch.
        assert_eq!(
            split_relayout.merged_configuration.override_config.smallest_screen_width_dp,
            split_relayout.merged_configuration.global.smallest_screen_width_dp
        );
        assert_eq!(
            split_relayout.merged_configuration.override_config.screen_layout,
            split_relayout.merged_configuration.global.screen_layout
        );
        assert_eq!(
            split_relayout.merged_configuration.override_config.orientation,
            split_relayout.merged_configuration.global.orientation
        );

        // System windows never inherit task bounds.
        let mut sys_params = app_params();
        sys_params.type_ = crate::layers::TYPE_APPLICATION_OVERLAY;
        let overlay = wm
            .add_to_display_as_user(sess, BinderId(0x20), sys_params.clone(), 0)
            .unwrap()
            .window;
        let overlay_relayout = wm.relayout(sess, overlay, sys_params, 3).unwrap();
        assert_eq!(overlay_relayout.frames.frame, RectI::new(0, 0, 1080, 2400));
        assert_eq!(
            overlay_relayout.merged_configuration.override_config.window_configuration.windowing_mode,
            WINDOWING_MODE_FULLSCREEN
        );

        // Clearing task bounds restores the exact fullscreen MergedConfiguration bytes.
        assert!(wm.clear_task_bounds(DEFAULT_TASK_ID).is_some());
        assert_eq!(wm.task_bounds(DEFAULT_TASK_ID), None);
        let restored = wm.relayout(sess, win, app_params(), 4).unwrap();
        assert_eq!(restored.frames.frame, RectI::new(0, 0, 1080, 2400));
        let mut restored_parcel = acos_parcel::Parcel::new();
        restored
            .merged_configuration
            .write_to_parcel(&mut restored_parcel)
            .unwrap();
        assert_eq!(
            restored_parcel.marshall(),
            full_bytes,
            "clearing task bounds must restore byte-identical fullscreen MergedConfiguration"
        );
    }

    #[test]
    fn set_window_visible_hides_and_reshows_top_window_and_focus() {
        let mut wm = WindowManager::new(DisplaySpec::reference_with_insets(InsetsMode::Carve));
        let (s1, _) = wm.open_session(None);
        let (s2, _) = wm.open_session(None);

        let a = wm
            .add_to_display_as_user(s1, BinderId(0x01), app_params(), 0)
            .unwrap()
            .window;
        let b = wm
            .add_to_display_as_user(s2, BinderId(0x02), app_params(), 0)
            .unwrap()
            .window;
        assert_eq!(wm.focus, Some(b));

        // Hiding B removes it from the visible layer stack and shifts focus to A.
        wm.set_window_visible(b, false).unwrap();
        assert_eq!(wm.focus, Some(a));
        assert_eq!(wm.layer_stack().ordered().len(), 1);
        assert_eq!(wm.layer_stack().ordered()[0].window, a);

        // Hiding A leaves no visible focusable window.
        wm.set_window_visible(a, false).unwrap();
        assert_eq!(wm.focus, None);
        assert!(wm.layer_stack().ordered().is_empty());

        // Re-showing A gives it a fresh sequence number and restores focus; then
        // re-showing B puts B above A again.
        wm.set_window_visible(a, true).unwrap();
        assert_eq!(wm.focus, Some(a));
        let seq_a = wm.windows[&a].seq;
        wm.set_window_visible(b, true).unwrap();
        assert_eq!(wm.focus, Some(b));
        assert!(wm.windows[&b].seq > seq_a);
        assert!(wm.windows[&b].z > wm.windows[&a].z);
    }

    #[test]
    fn set_relayout_window_visible_ignores_pre_visibility_gone() {
        use crate::callbacks::WmCallback;
        let mut wm = WindowManager::new(DisplaySpec::reference_with_insets(InsetsMode::Carve));
        let (s1, _) = wm.open_session(None);

        let a = wm
            .add_to_display_as_user(s1, BinderId(0x01), app_params(), 0)
            .unwrap()
            .window;
        assert_eq!(wm.focus, Some(a));
        assert!(!wm.windows[&a].ever_visible);

        // Drain initial focus-gained callback.
        while wm.callbacks.pop().is_some() {}

        // Pre-visibility View.GONE / INVISIBLE relayout must be a no-op.
        assert!(!wm.set_relayout_window_visible(a, false).unwrap());
        assert_eq!(wm.focus, Some(a));
        assert!(wm.windows[&a].visible);
        assert!(!wm.windows[&a].ever_visible);
        while let Some(pc) = wm.callbacks.pop() {
            assert!(
                !matches!(pc.callback, WmCallback::WindowFocusChanged { .. }),
                "pre-visibility GONE must not emit WindowFocusChanged"
            );
        }

        // First View.VISIBLE relayout arms ever_visible without bouncing focus.
        assert!(!wm.set_relayout_window_visible(a, true).unwrap());
        assert!(wm.windows[&a].ever_visible);
        assert_eq!(wm.focus, Some(a));

        // Subsequent View.GONE / INVISIBLE relayout hides the window and drops focus,
        // and View.VISIBLE restores it.
        assert!(wm.set_relayout_window_visible(a, false).unwrap());
        assert!(!wm.windows[&a].visible);
        assert_eq!(wm.focus, None);
        assert!(wm.set_relayout_window_visible(a, true).unwrap());
        assert!(wm.windows[&a].visible);
        assert_eq!(wm.focus, Some(a));
    }

    #[test]
    fn fit_insets_fits_rotated_status_bar_on_its_actual_edge() {
        let base = DisplaySpec::reference_with_insets(InsetsMode::Report);

        let mut top_only = app_params();
        top_only.fit_insets_types = crate::codec::inset_type::STATUS_BAR;
        top_only.fit_insets_sides = 0x02; // WindowInsets.Side.TOP

        let mut all_sides = app_params();
        all_sides.fit_insets_types = crate::codec::inset_type::STATUS_BAR;
        all_sides.fit_insets_sides = 0; // Side.all()

        // Rotation 0: status bar is at top [0, 0, 1080, 128].
        let r0 = base.rotated(0);
        assert_eq!(compute_frame_for(&r0, &top_only), RectI::new(0, 128, 1080, 2400));
        assert_eq!(compute_frame_for(&r0, &all_sides), RectI::new(0, 128, 1080, 2400));

        // Rotation 2 (180 deg): status bar is at bottom [0, 2272, 1080, 2400].
        // Fitting TOP must not become a top inset or produce a zero-height frame.
        let r2 = base.rotated(2);
        assert_eq!(r2.bars.status_bar, RectI::new(0, 2272, 1080, 2400));
        // The rotated configuration also reserves the 63 px config strip at the
        // top (`reference_insets_state`'s second STATUS_BAR source), and
        // `InsetsState.calculateInsets` takes the per-edge max over both.
        assert_eq!(compute_frame_for(&r2, &top_only), RectI::new(0, 63, 1080, 2400));
        assert_eq!(compute_frame_for(&r2, &all_sides), RectI::new(0, 63, 1080, 2272));

        // Rotation 1 (90 deg): status bar is at left [0, 0, 128, 1080].
        let r1 = base.rotated(1);
        // Plus the config strip [0,0,2400,63]: TOP-only fitting keeps top=63.
        assert_eq!(compute_frame_for(&r1, &top_only), RectI::new(0, 63, 2400, 1080));
        assert_eq!(compute_frame_for(&r1, &all_sides), RectI::new(128, 63, 2400, 1080));

        // Rotation 3 (270 deg): status bar is at right [2272, 0, 2400, 1080].
        let r3 = base.rotated(3);
        assert_eq!(compute_frame_for(&r3, &top_only), RectI::new(0, 63, 2400, 1080));
        assert_eq!(compute_frame_for(&r3, &all_sides), RectI::new(0, 63, 2272, 1080));

        // System gestures on 90 deg rotation sit on top/bottom, not left/right.
        let mut sys_gestures = app_params();
        sys_gestures.fit_insets_types = crate::codec::inset_type::SYSTEM_GESTURES;
        sys_gestures.fit_insets_sides = 0;
        let sg_r1 = compute_frame_for(&r1, &sys_gestures);
        assert_eq!(sg_r1, RectI::new(0, 78, 2400, 1002));
    }
}
