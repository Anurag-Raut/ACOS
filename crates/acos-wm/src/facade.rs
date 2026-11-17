//! The `SessionWm` facade: one composed WMS + compositor + input + IME + vsync
//! stack per session.
//!
//! This is the object the future native layer binds to (it owns every piece of
//! the window/input/IME state) and the object the agent API drives (`capture a
//! fresh frame with a cause id`, `inject an input and wait for its ack`, `hand
//! over the UI tree`).
//!
//! ```text
//! add_window()      -> IWindowSession.addToDisplayAsUser + compositor layer + input route
//! relayout()        -> IWindowSession.relayout
//! finish_drawing()  -> SurfaceControl.Transaction commit + present() (frame_seq++)
//! capture_fresh()   -> latest frame with frame_seq > after_seq
//! tap()/swipe()/key()/type_text() -> input/IME injection, returns ack sequences
//! pump_vsync()      -> vsync deliveries when the clock has reached the deadline
//! ```

use std::sync::Arc;

use acos_parcel::binder::BinderId;
use acos_protocol::ui::UiTree;
use acos_render::{Bitmap, Frame};

use crate::clock::{Clock, RealClock};
use crate::codec::{InsetsState, LayoutParams};
use crate::compositor::{Compositor, Transaction};
use crate::display::DisplaySpec;
use crate::error::{Result, WmError};
use crate::geom::RectI;
use crate::ime::{EditorInfo, ImeManager, InputConnection};
use crate::input::{AnrEvent, InputDispatcher, WindowInfo};
use crate::vsync::{VsyncDelivery, VsyncGenerator};
use crate::wms::{AddWindowResult, RelayoutResult, SessionId, WindowId, WindowManager};

/// A fresh frame with its causal identity.
#[derive(Debug, Clone)]
pub struct FrameView {
    /// The underlying `acos-render` frame.
    pub frame: Frame,
    /// The request/action that caused it.
    pub cause_id: i64,
}

impl FrameView {
    /// Monotonic frame sequence.
    #[must_use]
    pub fn frame_seq(&self) -> u64 {
        self.frame.seq
    }

    /// Content hash (128-bit).
    #[must_use]
    pub fn content_hash(&self) -> String {
        self.frame.hash_hex()
    }

    /// SHA-256 of the pixels.
    #[must_use]
    pub fn sha256(&self) -> String {
        self.frame.sha256()
    }

    /// `frame N at cause M`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("frame {} at cause {}", self.frame.seq, self.cause_id)
    }
}

/// The result of an injected action.
#[derive(Debug, Clone, PartialEq)]
pub struct InputOutcome {
    /// The action's cause id.
    pub cause_id: i64,
    /// Delivered event sequence numbers (to be acked).
    pub seqs: Vec<u32>,
    /// Target window of the first event.
    pub delivered_to: Option<WindowId>,
}

/// The UI-tree handoff hook (T-40): the native layer implements this by walking
/// the real view hierarchy through the bridge.
pub trait UiTreeProvider: Send + Sync + std::fmt::Debug {
    /// Produce the current UI tree.
    fn ui_tree(&self) -> UiTree;
}

/// One session's full window/input/IME stack.
pub struct SessionWm {
    /// Session id.
    pub id: SessionId,
    /// Display geometry.
    pub display: DisplaySpec,
    /// Window manager state machine.
    pub wm: WindowManager,
    /// SurfaceFlinger replacement.
    pub compositor: Compositor,
    /// Input dispatcher.
    pub input: InputDispatcher,
    /// IME manager.
    pub ime: ImeManager,
    /// Vsync generator.
    pub vsync: VsyncGenerator,
    /// Clock seam shared by all sub-machines.
    pub clock: Arc<dyn Clock>,
    ui_tree: Option<Arc<dyn UiTreeProvider>>,
    frame_seq_at_last_ack: u64,
}

impl std::fmt::Debug for SessionWm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionWm")
            .field("id", &self.id)
            .field("display", &self.display)
            .field("wm", &self.wm)
            .field("compositor", &self.compositor)
            .field("input", &self.input)
            .field("vsync", &self.vsync)
            .finish()
    }
}

impl SessionWm {
    /// Create a session with a real clock.
    pub fn new(display: DisplaySpec) -> Result<Self> {
        Self::with_clock(display, Arc::new(RealClock::new()))
    }

    /// Create a session with an explicit clock (virtual in tests).
    pub fn with_clock(display: DisplaySpec, clock: Arc<dyn Clock>) -> Result<Self> {
        let compositor = Compositor::new(display)?;
        let input = InputDispatcher::new(clock.clone());
        let vsync = VsyncGenerator::new(display, clock.clone());
        let mut wm = WindowManager::new(display);
        let (id, _binder) = wm.open_session(None);
        Ok(Self {
            id,
            display,
            wm,
            compositor,
            input,
            ime: ImeManager::new(),
            vsync,
            clock,
            ui_tree: None,
            frame_seq_at_last_ack: 0,
        })
    }

    /// Attach the UI-tree provider (T-40 handoff).
    pub fn set_ui_tree_provider(&mut self, provider: Arc<dyn UiTreeProvider>) {
        self.ui_tree = Some(provider);
    }

    /// The current UI tree, if a provider is attached.
    #[must_use]
    pub fn ui_tree(&self) -> Option<UiTree> {
        self.ui_tree.as_ref().map(|p| p.ui_tree())
    }

    /// `IWindowSession.addToDisplayAsUser`: register the window with the WMS,
    /// create its compositor layer, and route input to its channel.
    pub fn add_window(
        &mut self,
        window: BinderId,
        params: LayoutParams,
        requested_visible_types: i32,
    ) -> Result<AddWindowResult> {
        let result =
            self.wm
                .add_to_display_as_user(self.id, window, params, requested_visible_types)?;
        if result.result != crate::wms::ADD_OKAY {
            return Ok(result);
        }
        // Create the compositor layer keyed by the window's surface handle.
        let layer = self
            .wm
            .windows
            .get(&result.window)
            .and_then(|w| w.surface_handle)
            .unwrap_or(result.window);
        self.compositor.create_layer(
            Some(result.window),
            format!("window-{}", result.window),
            result.window as i64,
        );
        // Register the input route with the dispatcher end of the channel.
        let frame = self
            .wm
            .windows
            .get(&result.window)
            .map(|w| w.frame)
            .unwrap_or_default();
        let info = WindowInfo {
            window: result.window,
            session: self.id,
            frame,
            z: result.window as i64,
            visible: true,
            focusable: true,
            touchable: true,
            display_id: self.display.id,
        };
        if let Some(channel) = self.wm.channels.get(result.input_channel.channel_handle) {
            self.input.register_window(info, channel.dispatcher.clone());
        }
        let _ = layer;
        Ok(result)
    }

    /// `IWindowSession.relayout`.
    pub fn relayout(
        &mut self,
        window: WindowId,
        params: LayoutParams,
        sync_seq: i32,
    ) -> Result<RelayoutResult> {
        let r = self.wm.relayout(self.id, window, params, sync_seq)?;
        if let Some(info) = self.input.windows.get_mut(&window) {
            info.frame = r.frames.frame;
        }
        Ok(r)
    }

    /// `IWindowSession.finishDrawing`: attach the client's rendered buffer to
    /// the window layer and composite a new frame.
    pub fn finish_drawing(
        &mut self,
        window: WindowId,
        buffer: Bitmap,
        frame_number: u64,
        cause_id: i64,
    ) -> Result<Option<u64>> {
        self.wm.finish_drawing(window, -1)?;
        let layer = self
            .compositor
            .layer_for_window(window)
            .ok_or(WmError::UnknownWindow(window))?;
        self.compositor
            .set_buffer(layer, buffer, frame_number, cause_id)?;
        self.compositor.apply(&Transaction::SetVisibility {
            layer,
            visible: true,
        })?;
        let seq = self.compositor.present(cause_id)?;
        if let Some(seq) = seq {
            self.input.current_frame_seq = seq;
        }
        Ok(seq)
    }

    /// Remove a window.
    pub fn remove_window(&mut self, window: WindowId) -> Result<()> {
        self.wm.remove(window)?;
        if let Some(l) = self.compositor.layer_for_window(window) {
            self.compositor.remove_layer(l);
        }
        self.input.remove_window(window);
        Ok(())
    }

    /// Capture the frame if strictly newer than `after_seq`.
    pub fn capture_fresh(&self, after_seq: u64) -> Result<FrameView> {
        let frame = self.compositor.capture_fresh(after_seq)?;
        Ok(FrameView {
            cause_id: frame.cause_id,
            frame,
        })
    }

    /// The latest frame, fresh or not.
    #[must_use]
    pub fn capture(&self) -> Option<FrameView> {
        self.compositor.capture().map(|frame| FrameView {
            cause_id: frame.cause_id,
            frame,
        })
    }

    /// Inject a tap and return the delivered sequences.
    pub fn tap(&mut self, x: f32, y: f32, cause_id: i64) -> Result<InputOutcome> {
        let delivered_to = self.input.hit_test(self.id, x, y);
        let seqs = self.input.inject_tap(self.id, x, y, cause_id)?;
        Ok(InputOutcome {
            cause_id,
            seqs,
            delivered_to,
        })
    }

    /// Inject an interpolated swipe.
    pub fn swipe(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        duration_nanos: i64,
        steps: usize,
        cause_id: i64,
    ) -> Result<InputOutcome> {
        let delivered_to = self.input.hit_test(self.id, from.0, from.1);
        let seqs = self
            .input
            .inject_swipe(self.id, from, to, duration_nanos, steps, cause_id)?;
        Ok(InputOutcome {
            cause_id,
            seqs,
            delivered_to,
        })
    }

    /// Inject a key press/release pair.
    pub fn key(&mut self, key_code: i32, cause_id: i64) -> Result<InputOutcome> {
        let delivered_to = self.input.focus_for(self.id);
        let seqs = self.input.inject_key(self.id, key_code, cause_id)?;
        Ok(InputOutcome {
            cause_id,
            seqs,
            delivered_to,
        })
    }

    /// Type `text` into the focused text field through the IME manager.
    ///
    /// This is `IRemoteInputConnection.commitText`, not a burst of key events:
    /// it is what an IME does and what the measured interaction trace shows
    /// (15 `IRemoteInputConnection` transports).
    pub fn type_text(
        &mut self,
        client: BinderId,
        text: &str,
        cause_id: i64,
    ) -> Result<InputOutcome> {
        self.ime.commit_text(client, text, 1)?;
        Ok(InputOutcome {
            cause_id,
            seqs: vec![],
            delivered_to: self.input.focus_for(self.id),
        })
    }

    /// Acknowledge one injected event (`finishInputEvent`).
    pub fn ack(&mut self, seq: u32) -> Result<crate::input::AckRecord> {
        let rec = self.input.finish_input_event(seq)?;
        self.frame_seq_at_last_ack = rec.frame_at_ack;
        Ok(rec)
    }

    /// The frame sequence observed at the last ack.
    #[must_use]
    pub fn frame_seq_at_last_ack(&self) -> u64 {
        self.frame_seq_at_last_ack
    }

    /// Poll the vsync generator at the current clock time.
    pub fn pump_vsync(&mut self) -> Vec<VsyncDelivery> {
        let now = self.clock.now_nanos();
        self.vsync.poll(now)
    }

    /// Poll ANR timeouts at the current clock time.
    pub fn pump_anr(&mut self) -> Vec<AnrEvent> {
        let now = self.clock.now_nanos();
        self.input.poll_anr(now)
    }

    /// The current `InsetsState` for `window`.
    pub fn insets_state(&self, window: WindowId) -> Option<&InsetsState> {
        self.wm.windows.get(&window).map(|w| &w.insets_state)
    }

    /// The current window frame.
    #[must_use]
    pub fn window_frame(&self, window: WindowId) -> Option<RectI> {
        self.wm.windows.get(&window).map(|w| w.frame)
    }

    /// Register the native layer's `IRemoteInputConnection` implementation.
    pub fn register_input_connection(
        &mut self,
        connection: BinderId,
        cb: Arc<dyn InputConnection>,
    ) {
        self.ime.register_connection(connection, cb);
    }

    /// Convenience: bind an IME client to a text field.
    pub fn bind_text_field(
        &mut self,
        client: BinderId,
        connection: BinderId,
        editor: EditorInfo,
    ) -> Result<()> {
        self.ime.add_client(client, Some(connection), 0)?;
        let _ =
            self.ime
                .start_input_or_window_gained_focus(client, &editor, Some(connection), 1)?;
        Ok(())
    }
}
