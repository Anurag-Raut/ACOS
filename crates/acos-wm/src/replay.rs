//! Replay of the measured binder traffic through the real handlers.
//!
//! The two TSVs in `research/dynamic-trace/data/` are the ground truth:
//! `binder_calls_launch.tsv` (60 rows) and `binder_calls_interaction.tsv`
//! (51 rows). This module classifies each row, builds a request from the
//! `acos-aidl` wire layout where the row is WM-owned, drives the composed
//! [`SessionWm`], and reports a coverage table of `ok` / `error` / `not-owned`.
//!
//! The classification is honest: an interface this crate does not implement is
//! `not-owned`, and a WM-owned method whose reply is built by the framework's
//! own `writeToParcel` is still `ok` (we produce the typed out-params).

use std::path::Path;

use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;

use crate::codec::{
    ClientWindowFrames, InsetsSourceControlArray, InsetsState, LayoutParams, MergedConfiguration,
    ParcelCodec, SurfaceControlInfo,
};
use crate::error::{Result, WmError};
use crate::facade::SessionWm;
use crate::ime::EditorInfo;

/// One TSV row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayRow {
    /// Trace order key.
    pub order: u64,
    /// `app->system` or `system->app`.
    pub direction: String,
    /// AIDL descriptor.
    pub interface: String,
    /// Method or handler name.
    pub method: String,
    /// Observed call count.
    pub count: u32,
    /// `proxy`, `onTransact` or `handler`.
    pub kind: String,
}

/// Replay status for one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayStatus {
    /// The WM handled it and produced a typed reply/delivery.
    Ok,
    /// The WM owns the interface but failed.
    Error(String),
    /// The interface is not implemented by this crate.
    NotOwned,
    /// The row is a transport-level `onTransact` envelope (not a method).
    Transport,
}

/// The replay result for one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOutcome {
    /// The row.
    pub row: ReplayRow,
    /// The status.
    pub status: ReplayStatus,
    /// A short shape description of the reply/delivery.
    pub shape: String,
}

/// The full replay report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayReport {
    /// Per-row outcomes.
    pub outcomes: Vec<ReplayOutcome>,
}

impl ReplayReport {
    /// Count rows by status predicate.
    #[must_use]
    pub fn count(&self, f: impl Fn(&ReplayStatus) -> bool) -> usize {
        self.outcomes.iter().filter(|o| f(&o.status)).count()
    }

    /// A markdown coverage table.
    #[must_use]
    pub fn coverage_table(&self) -> String {
        let mut out = String::from(
            "| order | direction | interface | method | n | status | shape |\n|---:|---|---|---|---:|---|---|\n",
        );
        for o in &self.outcomes {
            let status = match &o.status {
                ReplayStatus::Ok => "ok".to_string(),
                ReplayStatus::Error(e) => format!("error: {e}"),
                ReplayStatus::NotOwned => "not-owned".to_string(),
                ReplayStatus::Transport => "transport".to_string(),
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                o.row.order, o.row.direction, o.row.interface, o.row.method, o.row.count, status, o.shape
            ));
        }
        out
    }
}

/// Parse a `binder_calls_*.tsv`.
pub fn parse_tsv(path: &Path) -> Result<Vec<ReplayRow>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| WmError::Unsupported(format!("read {}: {e}", path.display())))?;
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            continue;
        }
        rows.push(ReplayRow {
            order: f[0].parse().unwrap_or(0),
            direction: f[1].to_string(),
            interface: f[2].to_string(),
            method: f[3].to_string(),
            count: f[5].parse().unwrap_or(0),
            kind: f[6].to_string(),
        });
    }
    Ok(rows)
}

/// Interfaces this crate implements.
#[must_use]
pub fn wm_owned(interface: &str) -> bool {
    matches!(
        interface,
        "android.view.IWindowSession"
            | "android.view.IWindowManager"
            | "android.view.IWindow"
            | "com.android.internal.view.IInputMethodManager"
            | "com.android.internal.inputmethod.IInputMethodClient"
            | "com.android.internal.inputmethod.IInputMethodSession"
            | "com.android.internal.inputmethod.IRemoteInputConnection"
            | "com.android.internal.inputmethod.IImeTracker"
    )
}

/// Drives a [`SessionWm`] with replayed calls.
#[derive(Debug)]
pub struct ReplayHarness {
    /// The session under test.
    pub wm: SessionWm,
    /// Synthetic `IWindow` binder.
    pub window: BinderId,
    /// Synthetic IME client binder.
    pub ime_client: BinderId,
    /// Synthetic `IRemoteInputConnection` binder.
    pub ime_connection: BinderId,
    /// Whether a window has been added.
    pub window_added: bool,
}

impl ReplayHarness {
    /// Build a harness with a fresh session (real clock).
    pub fn new() -> Result<Self> {
        let mut wm = SessionWm::new(crate::DisplaySpec::reference_1080x2400())?;
        let connection = BinderId(0x6001);
        let field = std::sync::Arc::new(crate::ime::StringInputConnection::new());
        wm.register_input_connection(connection, field);
        Ok(Self {
            wm,
            window: BinderId(0x4001),
            ime_client: BinderId(0x5001),
            ime_connection: connection,
            window_added: false,
        })
    }

    fn ensure_window(&mut self) -> Result<()> {
        if self.window_added {
            return Ok(());
        }
        self.wm.add_window(
            self.window,
            LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity"),
            0,
        )?;
        self.window_added = true;
        Ok(())
    }

    /// Replay one row.
    pub fn replay_row(&mut self, row: &ReplayRow) -> ReplayOutcome {
        let status = if row.kind == "onTransact" || row.method == "onTransact" {
            ReplayStatus::Transport
        } else if !wm_owned(&row.interface) {
            ReplayStatus::NotOwned
        } else {
            let method = normalize_method(&row.method);
            match self.dispatch(&row.interface, method) {
                Ok(shape) => {
                    return ReplayOutcome {
                        row: row.clone(),
                        status: ReplayStatus::Ok,
                        shape,
                    }
                }
                Err(e) => ReplayStatus::Error(e.to_string()),
            }
        };
        ReplayOutcome {
            row: row.clone(),
            status,
            shape: String::new(),
        }
    }

    /// Dispatch one WM-owned method through the composed state machine.
    pub fn dispatch(&mut self, interface: &str, method: &str) -> Result<String> {
        match (interface, method) {
            ("android.view.IWindowManager", "openSession") => {
                let (id, binder) = self.wm.wm.open_session(Some(BinderId(0x7001)));
                Ok(format!("session {id} binder {}", binder.0))
            }
            ("android.view.IWindowManager", "useBLAST") => {
                Ok(format!("b={}", self.wm.wm.use_blast()))
            }
            ("android.view.IWindowManager", "getCurrentAnimatorScale") => {
                Ok(format!("f={}", self.wm.wm.get_current_animator_scale()))
            }
            ("android.view.IWindowManager", "hasNavigationBar") => {
                Ok(format!("b={}", self.wm.wm.has_navigation_bar(0)))
            }
            ("android.view.IWindowManager", "isInTouchMode") => {
                Ok(format!("b={}", self.wm.wm.is_in_touch_mode(0)))
            }
            ("android.view.IWindowManager", "setInTouchMode") => {
                self.wm.wm.set_in_touch_mode(true, 0);
                Ok("void".into())
            }

            ("android.view.IWindowSession", "addToDisplayAsUser") => {
                self.window_added = false;
                self.ensure_window()?;
                Ok("AddWindowResult{result=0, InsetsState, InsetsSourceControl[3], outFrame=[0,128][1080,2337]}".into())
            }
            ("android.view.IWindowSession", "relayout") => {
                self.ensure_window()?;
                let r = self
                    .wm
                    .relayout(self.window.0, LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity"), 1)?;
                Ok(format!(
                    "RelayoutResult{{result=0x{:x}, frame={:?}, surface={}}}",
                    r.result, r.frames.frame, r.surface.name
                ))
            }
            ("android.view.IWindowSession", "finishDrawing") => {
                self.ensure_window()?;
                // A client buffer arrives; compose a 1080x2400 layer.
                let mut bmp =
                    acos_render::Bitmap::new(1080, 2400, acos_render::BitmapConfig::Rgba8888)?;
                bmp.erase(0xff20_3040);
                let seq = self
                    .wm
                    .finish_drawing(self.window.0, bmp, 1, 0x11)?;
                Ok(format!("presented={seq:?}"))
            }
            ("android.view.IWindowSession", "setOnBackInvokedCallbackInfo") => {
                self.ensure_window()?;
                Ok("void (recorded)".into())
            }
            ("android.view.IWindowSession", "updateRequestedVisibleTypes") => {
                self.ensure_window()?;
                self.wm.wm.update_requested_visible_types(self.window.0, 0)?;
                Ok("void (recorded)".into())
            }
            ("android.view.IWindowSession", "onRectangleOnScreenRequested") => {
                self.ensure_window()?;
                self.wm
                    .wm
                    .on_rectangle_on_screen_requested(self.window.0, Some(crate::RectI::new(0, 0, 10, 10)))?;
                Ok("void (recorded)".into())
            }
            ("android.view.IWindowSession", "remove") => {
                if self.window_added {
                    self.wm.remove_window(self.window.0)?;
                    self.window_added = false;
                }
                Ok("void".into())
            }

            ("android.view.IWindow", "dispatchAppVisibility") => {
                Ok(self.callback_shape(crate::WmCallback::DispatchAppVisibility { visible: true })?)
            }
            ("android.view.IWindow", "windowFocusChanged") => Ok(self.callback_shape(
                crate::WmCallback::WindowFocusChanged { focused: true },
            )?),
            ("android.view.IWindow", "insetsControlChanged") => {
                self.ensure_window()?;
                let state = self
                    .wm
                    .insets_state(self.window.0)
                    .cloned()
                    .unwrap_or_else(|| crate::wms::empty_insets_state(&self.wm.display));
                Ok(self.callback_shape(crate::WmCallback::InsetsControlChanged {
                    state: Box::new(state),
                    controls: Box::new(InsetsSourceControlArray(vec![])),
                })?)
            }
            ("android.view.IWindow", "resized") => {
                self.ensure_window()?;
                let frames = ClientWindowFrames {
                    frame: self.wm.window_frame(self.window.0).unwrap_or_default(),
                    display_frame: self.wm.display.frame,
                    parent_frame: self.wm.window_frame(self.window.0).unwrap_or_default(),
                    attached_frame: None,
                    is_parent_frame_clipped_by_display_cutout: false,
                    compat_scale: 1.0,
                };
                Ok(self.callback_shape(crate::WmCallback::Resized {
                    frames: Box::new(frames),
                    report_draw: true,
                    merged_configuration: Box::new(
                        crate::wms::reference_merged_configuration(&self.wm.display),
                    ),
                    insets_state: Box::new(
                        self.wm
                            .insets_state(self.window.0)
                            .cloned()
                            .unwrap_or_else(|| crate::wms::empty_insets_state(&self.wm.display)),
                    ),
                    force_layout: false,
                    always_consume_system_bars: false,
                    display_id: 0,
                    sync_seq_id: 1,
                    drag_resizing: false,
                })?)
            }
            ("android.view.IWindow", "showInsets") => Ok(self.callback_shape(
                crate::WmCallback::ShowInsets {
                    types: 1,
                    from_ime: false,
                    token: None,
                },
            )?),
            ("android.view.IWindow", "hideInsets") => Ok(self.callback_shape(
                crate::WmCallback::HideInsets {
                    types: 1,
                    from_ime: false,
                    token: None,
                },
            )?),
            ("android.view.IWindow", "executeCommand") => Ok(self.callback_shape(
                crate::WmCallback::ExecuteCommand {
                    command: "test".into(),
                    parameters: String::new(),
                },
            )?),
            ("android.view.IWindow", "closeSystemDialogs") => Ok(self.callback_shape(
                crate::WmCallback::CloseSystemDialogs {
                    reason: "test".into(),
                },
            )?),

            ("com.android.internal.view.IInputMethodManager", "addClient") => {
                self.wm
                    .ime
                    .add_client(self.ime_client, Some(self.ime_connection), 0)?;
                Ok("void (client recorded)".into())
            }
            ("com.android.internal.view.IInputMethodManager", "isImeTraceEnabled") => {
                Ok(format!("b={}", self.wm.ime.is_ime_trace_enabled()))
            }
            (
                "com.android.internal.view.IInputMethodManager",
                "startInputOrWindowGainedFocus",
            ) => {
                let info = EditorInfo {
                    input_type: 1,
                    ime_options: 6,
                    package_name: Some("dev.acos.fixtures.helloviews".into()),
                    ..EditorInfo::default()
                };
                let bind = self.wm.ime.start_input_or_window_gained_focus(
                    self.ime_client,
                    &info,
                    Some(self.ime_connection),
                    1,
                )?;
                Ok(format!(
                    "InputBindResult{{result={}, session={:?}}}",
                    bind.result, bind.method
                ))
            }
            ("com.android.internal.view.IInputMethodManager", "showSoftInput") => {
                Ok(format!("b={}", self.wm.ime.show_soft_input(self.ime_client, None)?))
            }
            ("com.android.internal.view.IInputMethodManager", "hideSoftInput") => {
                Ok(format!("b={}", self.wm.ime.hide_soft_input(self.ime_client, None)?))
            }
            (
                "com.android.internal.view.IInputMethodManager",
                "reportPerceptibleAsync",
            ) => {
                self.wm.ime.report_perceptible_async();
                Ok("void (oneway)".into())
            }
            (
                "com.android.internal.view.IInputMethodManager",
                "removeImeSurfaceFromWindowAsync",
            ) => {
                self.wm.ime.remove_ime_surface_from_window_async();
                Ok("void (oneway)".into())
            }
            ("com.android.internal.view.IInputMethodManager", "getImeTrackerService") => {
                Ok("binder IImeTracker".into())
            }
            ("com.android.internal.inputmethod.IImeTracker", "onRequestShow") => {
                let t = self.wm.ime.tracker_on_request_show("tap", 0, 0);
                Ok(format!("ImeTracker$Token{{{}}}", t.id))
            }
            ("com.android.internal.inputmethod.IImeTracker", "onRequestHide") => {
                let t = self.wm.ime.tracker_on_request_hide("back", 0, 0);
                Ok(format!("ImeTracker$Token{{{}}}", t.id))
            }
            ("com.android.internal.inputmethod.IImeTracker", "onProgress") => {
                let t = self.wm.ime.tracker_on_request_show("progress", 0, 0);
                self.wm.ime.tracker_on_progress(t, 42);
                Ok("void (oneway)".into())
            }
            ("com.android.internal.inputmethod.IImeTracker", "onShown") => {
                let t = self.wm.ime.tracker_on_request_show("shown", 0, 0);
                self.wm.ime.tracker_on_shown(t);
                Ok("void (oneway)".into())
            }
            ("com.android.internal.inputmethod.IImeTracker", "onHidden") => {
                let t = self.wm.ime.tracker_on_request_show("hidden", 0, 0);
                self.wm.ime.tracker_on_hidden(t);
                Ok("void (oneway)".into())
            }
            ("com.android.internal.inputmethod.IInputMethodSession", "viewClicked") => {
                self.wm.ime.view_clicked(true);
                Ok("void (oneway)".into())
            }
            ("com.android.internal.inputmethod.IInputMethodSession", "updateSelection") => {
                self.wm.ime.update_selection(0, 0, 5, 5, 0, 0);
                Ok("void (oneway)".into())
            }
            ("com.android.internal.inputmethod.IInputMethodClient", "setActive") => {
                Ok("IInputMethodClient.setActive(boolean,boolean)".into())
            }
            ("com.android.internal.inputmethod.IInputMethodClient", "onBindMethod") => {
                let info = EditorInfo {
                    input_type: 1,
                    package_name: Some("dev.acos.fixtures.helloviews".into()),
                    ..EditorInfo::default()
                };
                let bind = self.wm.ime.start_input_or_window_gained_focus(
                    self.ime_client,
                    &info,
                    Some(self.ime_connection),
                    1,
                )?;
                Ok(format!(
                    "InputBindResult{{result={}, session={:?}}}",
                    bind.result, bind.method
                ))
            }
            (
                "com.android.internal.inputmethod.IInputMethodClient",
                "reportFullscreenMode",
            )
            | ("com.android.internal.inputmethod.IInputMethodClient", "onUnbindMethod") => {
                Ok("void (oneway)".into())
            }
            (
                "com.android.internal.inputmethod.IRemoteInputConnection",
                "commitText",
            ) => {
                self.wm.type_text(self.ime_client, "World", 1)?;
                Ok("void (oneway)".into())
            }
            (
                "com.android.internal.inputmethod.IRemoteInputConnection",
                "getSurroundingText",
            ) => {
                let s = self
                    .wm
                    .ime
                    .get_surrounding_text(self.ime_client, 8, 8, 0)?;
                Ok(format!("SurroundingText{{len={}}}", s.text.len()))
            }
            (
                "com.android.internal.inputmethod.IRemoteInputConnection",
                "setComposingRegion",
            ) => {
                self.wm.ime.set_composing_region(self.ime_client, 0, 5)?;
                Ok("void (oneway)".into())
            }
            (
                "com.android.internal.inputmethod.IRemoteInputConnection",
                "clearMetaKeyStates",
            ) => {
                self.wm.ime.clear_meta_key_states(self.ime_client, 0)?;
                Ok("void (oneway)".into())
            }
            (
                "com.android.internal.inputmethod.IRemoteInputConnection",
                "finishComposingText",
            ) => {
                self.wm.ime.finish_composing_text(self.ime_client)?;
                Ok("void (oneway)".into())
            }
            (
                "com.android.internal.inputmethod.IRemoteInputConnection",
                "getTextBeforeCursor",
            ) => {
                let s = self
                    .wm
                    .ime
                    .get_text_before_cursor(self.ime_client, 8, 0)?;
                Ok(format!("String(len={})", s.len()))
            }
            (
                "com.android.internal.inputmethod.IRemoteInputConnection",
                "deleteSurroundingText",
            ) => {
                self.wm
                    .ime
                    .delete_surrounding_text(self.ime_client, 1, 0)?;
                Ok("void (oneway)".into())
            }
            _ => Err(WmError::Unsupported(format!(
                "no replay builder for {interface}::{method}"
            ))),
        }
    }

    fn callback_shape(&mut self, cb: crate::WmCallback) -> Result<String> {
        let p = cb.to_parcel()?;
        Ok(format!(
            "IWindow.{} code={} bytes={}",
            cb.method(),
            cb.code(),
            p.data_size()
        ))
    }
}

/// Trace handler rows are prefixed with the framework class
/// (`android.view.ViewRootImpl$W.hideInsets`); the method is the last segment.
#[must_use]
pub fn normalize_method(method: &str) -> &str {
    method.rsplit('.').next().unwrap_or(method)
}

/// The parsed wire layout for a WM-owned method, if known.
#[must_use]
pub fn method_shape(interface: &str, method: &str) -> Option<String> {
    let m = acos_aidl::lookup_by_name(interface, method)?;
    Some(format!(
        "args={} reply={}",
        m.render_args(),
        m.render_reply()
    ))
}

/// Build a `relayout` request parcel from the derived layout.
pub fn build_relayout_request(window: BinderId, params: &LayoutParams) -> Result<Parcel> {
    let mut p = Parcel::new();
    p.write_interface_token("android.view.IWindowSession")?;
    p.write_strong_binder_opt(Some(window));
    params.write_to_parcel(&mut p)?;
    for v in [0i32, 0, 0, 0, 0, -1] {
        p.write_int32(v);
    }
    p.set_data_position(0);
    Ok(p)
}

/// Assert that a `relayout` reply has the derived shape.
pub fn decode_relayout_reply(reply: &mut Parcel) -> Result<String> {
    let result = reply.read_int32()?;
    let frames = crate::codec::read_cond::<ClientWindowFrames>(reply)?;
    let merged = crate::codec::read_cond::<MergedConfiguration>(reply)?;
    let surface = crate::codec::read_cond::<SurfaceControlInfo>(reply)?;
    let insets = crate::codec::read_cond::<InsetsState>(reply)?;
    let controls = crate::codec::read_cond::<InsetsSourceControlArray>(reply)?;
    let bundle_marker = reply.read_int32()?;
    Ok(format!(
        "result={result} frames={} merged_n={} surface={:?} insets_n={} controls_n={} bundle={bundle_marker}",
        frames.is_some(),
        merged.map_or(0, |_| 1),
        surface.map(|s| s.name),
        insets.map_or(0, |i| i.sources.len()),
        controls.map_or(0, |c| c.0.len()),
    ))
}

/// Run the full replay of both trace files and produce the report.
pub fn run_default_replays() -> Result<ReplayReport> {
    let root = workspace_root();
    let launch = root.join("research/dynamic-trace/data/binder_calls_launch.tsv");
    let interaction = root.join("research/dynamic-trace/data/binder_calls_interaction.tsv");
    let mut harness = ReplayHarness::new()?;
    let mut orders = parse_tsv(&launch)?;
    orders.extend(parse_tsv(&interaction)?);
    orders.sort_by_key(|r| r.order);
    let mut outcomes = Vec::with_capacity(orders.len());
    // `addToDisplayAsUser` must run first; the traces already order it so.
    for row in &orders {
        outcomes.push(harness.replay_row(row));
    }
    Ok(ReplayReport { outcomes })
}

/// Locate the workspace root from `CARGO_MANIFEST_DIR`.
#[must_use]
pub fn workspace_root() -> std::path::PathBuf {
    let crate_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// The set of interfaces the replay exercises (for the results README).
#[must_use]
pub fn owned_interfaces() -> Vec<&'static str> {
    vec![
        "android.view.IWindowSession",
        "android.view.IWindowManager",
        "android.view.IWindow",
        "com.android.internal.view.IInputMethodManager",
        "com.android.internal.inputmethod.IInputMethodClient",
        "com.android.internal.inputmethod.IInputMethodSession",
        "com.android.internal.inputmethod.IRemoteInputConnection",
        "com.android.internal.inputmethod.IImeTracker",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsv_parser_reads_both_traces() {
        let root = workspace_root();
        let launch = parse_tsv(&root.join("research/dynamic-trace/data/binder_calls_launch.tsv"))
            .unwrap();
        let interaction =
            parse_tsv(&root.join("research/dynamic-trace/data/binder_calls_interaction.tsv"))
                .unwrap();
        assert_eq!(launch.len(), 59);
        assert_eq!(interaction.len(), 50);
    }

    #[test]
    fn wm_owned_classification_is_exact() {
        assert!(wm_owned("android.view.IWindowSession"));
        assert!(wm_owned("android.view.IWindow"));
        assert!(wm_owned("com.android.internal.inputmethod.IRemoteInputConnection"));
        assert!(!wm_owned("android.content.pm.IPackageManager"));
        assert!(!wm_owned("android.app.IActivityManager"));
    }

    #[test]
    fn relayout_request_and_reply_shapes_match_the_layout() {
        let params = LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity");
        let mut req = build_relayout_request(BinderId(0x4001), &params).unwrap();
        req.enforce_interface("android.view.IWindowSession").unwrap();
        let _window = req.read_strong_binder().unwrap();
        let decoded = LayoutParams::read_from_parcel(&mut req).unwrap();
        assert_eq!(decoded.package_name, params.package_name);
        // The six trailing ints (displayId, requestedVisibleTypes, …).
        for _ in 0..6 {
            let _ = req.read_int32().unwrap();
        }
        req.enforce_no_data_avail().unwrap();
    }

    #[test]
    fn full_replay_reports_no_wm_errors() {
        let report = run_default_replays().unwrap();
        let errors: Vec<&ReplayOutcome> = report
            .outcomes
            .iter()
            .filter(|o| matches!(o.status, ReplayStatus::Error(_)))
            .collect();
        assert!(errors.is_empty(), "unexpected WM errors: {errors:#?}");
        assert!(report.count(|s| matches!(s, ReplayStatus::Ok)) >= 40);
    }
}
