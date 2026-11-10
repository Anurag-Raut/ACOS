//! The VM-facing wire codecs and compositor binding for the ACOS window
//! manager (BACKLOG T-29/T-34).
//!
//! The Binder transport routes the two WMS interface tokens here, before the
//! generic `ServiceHost` dispatch:
//!
//! * `android.view.IWindowManager` — `openSession` and the scalar display
//!   facts (`useBLAST`, `getCurrentAnimatorScale`, `hasNavigationBar`,
//!   `isInTouchMode`, `setInTouchMode`, `getInitialDisplaySize`,
//!   `getBaseDisplaySize`, `getInitialDisplayDensity`, `getBaseDisplayDensity`,
//!   `getDisplayImePolicy`).
//! * `android.view.IWindowSession` — `addToDisplayAsUser`, `addToDisplay`,
//!   `relayout`, `remove`, `finishDrawing`, `updateRequestedVisibleTypes`,
//!   `pokeDrawLock`, `onRectangleOnScreenRequested`,
//!   `reportSystemGestureExclusionChanged`, `setOnBackInvokedCallbackInfo`,
//!   `getDisplayId`, `reportDrawn`.
//!
//! The request arguments and reply bodies are encoded with the framework's own
//! field order as transcribed in [`acos_wm::codec`] (`LayoutParams`,
//! `ClientWindowFrames`, `MergedConfiguration`, `SurfaceControl`, `InsetsState`,
//! `InsetsSourceControl$Array`, `Point`, `Rect`). That is why this lives in
//! `acos-runtime` and not in `acos-vm`: `acos-vm` must stay free of `acos-wm`
//! policy.
//!
//! # Frame freshness
//!
//! `finishDrawing` is recorded but **never** publishes a frame. The
//! [`WmSurfaceBackend`] installed into `acos-vm`'s `view.surface` seam publishes
//! a frame only on a real `SurfaceOp::Buffer` submission, which is the advisor
//! rule from `docs/FINDINGS.md`.

use std::cell::RefCell;
use std::collections::HashMap;

use acos_parcel::binder::BinderId;
use acos_parcel::{Parcel, ParcelError};
use acos_services::wms::WmsState;
use acos_vm::natives::fwk_surface::{install_backend, presented_frames, SurfaceBackend, SurfaceOp};
use acos_wm::codec::{
    ClientWindowFrames, InsetsSourceControlArray, InsetsState, LayoutParams, MergedConfiguration,
    ParcelCodec, SurfaceControlInfo,
};
use acos_wm::geom::{PointI, RectI};
use acos_wm::wms::ADD_OKAY;

thread_local! {
    static WMS: RefCell<WmsState> = RefCell::new(WmsState::new());
    static BACKEND_INSTALLED: RefCell<bool> = const { RefCell::new(false) };
}

/// Run `f` with the process-global WMS state.
pub fn with_wms<R>(f: impl FnOnce(&mut WmsState) -> R) -> R {
    WMS.with(|w| f(&mut w.borrow_mut()))
}

/// True when `descriptor` is answered by this module.
#[must_use]
pub fn owns(descriptor: Option<&str>) -> bool {
    matches!(
        descriptor,
        Some("android.view.IWindowManager") | Some("android.view.IWindowSession")
    )
}

type WResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("wms_java: {e}")
}

fn perr(e: ParcelError) -> String {
    format!("wms_java: parcel: {e}")
}

// ---------------------------------------------------------------------------
// Argument decoding
// ---------------------------------------------------------------------------

/// The decoded arguments of one WMS call, split by kind.
#[derive(Debug, Default)]
struct Args {
    binders: Vec<Option<BinderId>>,
    ints: Vec<i32>,
    bools: Vec<bool>,
    params: Option<LayoutParams>,
    /// Raw native transaction bytes (`SurfaceControl$Transaction`), if any.
    #[allow(dead_code)]
    transaction: Option<Vec<u8>>,
}

impl Args {
    fn binder(&self, i: usize) -> Option<BinderId> {
        self.binders.get(i).copied().flatten()
    }
    fn int(&self, i: usize) -> i32 {
        self.ints.get(i).copied().unwrap_or(0)
    }
}

fn decode_args(method: &acos_aidl::MethodInfo, p: &mut Parcel) -> WResult<Args> {
    use acos_aidl::WireOp;
    let mut out = Args::default();
    for op in &method.args {
        match op {
            WireOp::Binder { .. } => out.binders.push(p.read_strong_binder().map_err(perr)?),
            WireOp::I32 => out.ints.push(p.read_int32().map_err(perr)?),
            WireOp::I64 => {
                let _ = p.read_int64().map_err(perr)?;
            }
            WireOp::Bool => out.bools.push(p.read_bool().map_err(perr)?),
            WireOp::F32 => {
                let _ = p.read_float().map_err(perr)?;
            }
            WireOp::Str16 => {
                let _ = p.read_string16().map_err(perr)?;
            }
            WireOp::Str8 => {
                let _ = p.read_string8().map_err(perr)?;
            }
            WireOp::TypedObject { class, .. } => {
                let present = p.read_int32().map_err(perr)? != 0;
                if present {
                    decode_typed_body(class, p, &mut out)?;
                }
            }
            WireOp::TypedList { .. } | WireOp::TypedArray { .. } => {
                let n = p.read_int32().map_err(perr)?;
                for _ in 0..n.max(0) {
                    // `writeTypedObject` per element.
                    let present = p.read_int32().map_err(perr)? != 0;
                    if present {
                        decode_typed_body("android.graphics.Rect", p, &mut out)?;
                    }
                }
            }
            WireOp::Parcelable { class } => decode_typed_body(class, p, &mut out)?,
            other => {
                return Err(err(format!(
                    "{}: undecoded argument {}",
                    method.name,
                    other.render()
                )))
            }
        }
    }
    Ok(out)
}

fn decode_typed_body(class: &str, p: &mut Parcel, out: &mut Args) -> WResult<()> {
    match class {
        "android.view.WindowManager$LayoutParams" => {
            out.params = Some(LayoutParams::read_from_parcel(p).map_err(err)?);
        }
        "android.view.SurfaceControl$Transaction" => {
            // `Transaction.writeToParcel` writes a native byte array holding the
            // encoded ops (`acos-vm`'s `view.surface` codec).
            out.transaction = p.read_byte_array().map_err(perr)?;
        }
        "android.graphics.Rect" => {
            let _ = RectI::read_from_parcel(p).map_err(err)?;
        }
        "android.graphics.Region" => {
            // Native-backed; the launch path does not need its contents.
            let _ = p.read_int32().map_err(perr)?;
        }
        "android.window.OnBackInvokedCallbackInfo" => {
            // Oneway and last: its binder payload is irrelevant here.
        }
        other => return Err(err(format!("undecoded parcelable argument {other}"))),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reply encoding
// ---------------------------------------------------------------------------

fn present<T: ParcelCodec>(p: &mut Parcel, v: Option<&T>) -> WResult<()> {
    match v {
        Some(v) => {
            p.write_int32(1);
            v.write_to_parcel(p).map_err(err)
        }
        None => {
            p.write_int32(0);
            Ok(())
        }
    }
}

fn encode_add_result(
    result: i32,
    add: Option<&acos_wm::wms::AddWindowResult>,
    p: &mut Parcel,
) -> WResult<()> {
    p.write_int32(result);
    match add {
        Some(a) if result == ADD_OKAY => {
            present(p, Some(&a.input_channel))?;
            present(p, Some(&a.insets_state))?;
            present(p, Some(&a.insets_controls))?;
            present(p, Some(&a.out_frame))?;
            p.write_float_array(Some(&a.out_float)).map_err(perr)?;
        }
        _ => {
            p.write_int32(0);
            p.write_int32(0);
            p.write_int32(0);
            p.write_int32(0);
            p.write_float_array(Some(&[])).map_err(perr)?;
        }
    }
    Ok(())
}

fn encode_relayout(r: &acos_wm::wms::RelayoutResult, p: &mut Parcel) -> WResult<()> {
    p.write_int32(r.result);
    present(p, Some(&r.frames))?;
    present(p, Some(&r.merged_configuration))?;
    present(p, Some(&r.surface))?;
    present(p, Some(&r.insets_state))?;
    present(p, Some(&r.insets_controls))?;
    // Trailing `Bundle` is null.
    p.write_int32(0);
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Handle one `IWindowManager`/`IWindowSession` transaction, writing the full
/// reply (including the exception header) into `reply`.
pub fn transact(
    _binder: BinderId,
    code: u32,
    data: &mut Parcel,
    reply: &mut Parcel,
    _flags: u32,
) -> Result<(), String> {
    let descriptor = data
        .peek_interface_token()
        .map_err(perr)?
        .ok_or_else(|| err("missing interface token"))?;
    let method = acos_aidl::lookup(&descriptor, code)
        .ok_or_else(|| err(format!("unknown method code {code} for {descriptor}")))?;
    data.enforce_interface(&descriptor).map_err(perr)?;
    let args = decode_args(method, data)?;

    reply.reset();
    acos_services::exception::write_no_exception(reply);
    let name = method.name.as_str();

    match descriptor.as_str() {
        "android.view.IWindowManager" => handle_manager(name, method, &args, reply),
        "android.view.IWindowSession" => handle_session(name, method, &args, reply),
        _ => Err(err(format!("unexpected descriptor {descriptor}"))),
    }
}

fn handle_manager(
    name: &str,
    method: &acos_aidl::MethodInfo,
    args: &Args,
    reply: &mut Parcel,
) -> WResult<()> {
    match name {
        "openSession" => {
            let binder = with_wms(WmsState::open_session);
            reply.write_strong_binder_opt(Some(binder));
        }
        "useBLAST" => reply.write_bool(with_wms(|s| s.use_blast())),
        "getCurrentAnimatorScale" => reply.write_float(with_wms(|s| s.current_animator_scale())),
        "hasNavigationBar" => {
            let d = args.int(0);
            reply.write_bool(with_wms(|s| s.has_navigation_bar(d)));
        }
        "isInTouchMode" => {
            let d = args.int(0);
            reply.write_bool(with_wms(|s| s.is_in_touch_mode(d)));
        }
        "setInTouchMode" => {
            let on = args.bools.first().copied().unwrap_or(false);
            let d = args.int(0);
            with_wms(|s| s.set_in_touch_mode(on, d));
        }
        "getInitialDisplaySize" | "getBaseDisplaySize" => {
            let point = with_wms(|s| PointI::new(s.facts.width, s.facts.height));
            present(reply, Some(&point))?;
        }
        "getInitialDisplayDensity" | "getBaseDisplayDensity" => {
            reply.write_int32(with_wms(|s| s.facts.density_dpi));
        }
        "getDisplayImePolicy" => {
            reply.write_int32(with_wms(|s| s.facts.ime_policy));
        }
        _ => {
            return Err(err(format!(
                "UNSUPPORTED android.view.IWindowManager.{} (code {})",
                name, method.code
            )))
        }
    }
    Ok(())
}

fn handle_session(
    name: &str,
    method: &acos_aidl::MethodInfo,
    args: &Args,
    reply: &mut Parcel,
) -> WResult<()> {
    let window = args.binder(0);
    match name {
        "addToDisplayAsUser" | "addToDisplay" => {
            let params = args
                .params
                .clone()
                .ok_or_else(|| err("addToDisplayAsUser: missing LayoutParams"))?;
            let window = window.ok_or_else(|| err("addToDisplayAsUser: null IWindow"))?;
            let result = with_wms(|s| s.add_to_display_as_user(window, params, 0)).map_err(err)?;
            encode_add_result(result.result, Some(&result), reply)?;
        }
        "addToDisplayWithoutInputChannel" => {
            let params = args
                .params
                .clone()
                .ok_or_else(|| err("addToDisplayWithoutInputChannel: missing LayoutParams"))?;
            let window = window.ok_or_else(|| err("null IWindow"))?;
            let result = with_wms(|s| s.add_to_display_as_user(window, params, 0)).map_err(err)?;
            encode_add_result(result.result, Some(&result), reply)?;
        }
        "relayout" => {
            let params = args
                .params
                .clone()
                .ok_or_else(|| err("relayout: missing LayoutParams"))?;
            let window = window.ok_or_else(|| err("relayout: null IWindow"))?;
            let sync_seq = args.ints.first().copied().unwrap_or(-1);
            let r = with_wms(|s| s.relayout(window.0, params, sync_seq)).map_err(err)?;
            encode_relayout(&r, reply)?;
        }
        "remove" => {
            if let Some(w) = window {
                with_wms(|s| s.remove(w.0)).map_err(err)?;
            }
        }
        "finishDrawing" => {
            if let Some(w) = window {
                let seq = args.ints.first().copied().unwrap_or(-1);
                with_wms(|s| s.finish_drawing(w.0, seq)).map_err(err)?;
            }
        }
        "setInsets" => {
            if let Some(w) = window {
                with_wms(|s| s.set_insets(w.0, None, None)).map_err(err)?;
            }
        }
        "updateRequestedVisibleTypes" => {
            if let Some(w) = window {
                let types = args.ints.first().copied().unwrap_or(0);
                with_wms(|s| s.update_requested_visible_types(w.0, types)).map_err(err)?;
            }
        }
        "reportSystemGestureExclusionChanged" => {
            if let Some(w) = window {
                with_wms(|s| s.report_system_gesture_exclusion_changed(w.0)).map_err(err)?;
            }
        }
        "pokeDrawLock" => {
            if let Some(w) = window {
                with_wms(|s| s.poke_draw_lock(w.0)).map_err(err)?;
            }
        }
        "onRectangleOnScreenRequested" => {
            if let Some(w) = window {
                with_wms(|s| s.on_rectangle_on_screen_requested(w.0, None)).map_err(err)?;
            }
        }
        "setOnBackInvokedCallbackInfo" => {
            // Oneway; accepted by touching the window record.
            if let Some(w) = window {
                let _ = with_wms(|s| s.wm.windows.contains_key(&w.0));
            }
        }
        "getDisplayId" => {
            reply.write_int32(with_wms(|s| s.wm.display.id));
        }
        "reportDrawn" | "reportDrawnAsync" => {
            if let Some(w) = window {
                with_wms(|s| s.finish_drawing(w.0, -1)).map_err(err)?;
            }
        }
        _ => {
            return Err(err(format!(
                "UNSUPPORTED android.view.IWindowSession.{} (code {})",
                name, method.code
            )))
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The `acos-wm`-backed `view.surface` compositor binding
// ---------------------------------------------------------------------------

/// A [`SurfaceBackend`] forwarding applied transactions to an `acos-wm`
/// `Compositor`. A frame is published only on a real buffer submission.
pub struct WmSurfaceBackend {
    compositor: acos_wm::compositor::Compositor,
    layers: HashMap<u64, u64>,
}

impl std::fmt::Debug for WmSurfaceBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WmSurfaceBackend")
            .field("layers", &self.layers.len())
            .finish()
    }
}

impl WmSurfaceBackend {
    /// Create the backend on the reference display.
    pub fn new() -> WResult<Self> {
        let compositor = acos_wm::compositor::Compositor::new(
            acos_wm::display::DisplaySpec::reference_1080x2400(),
        )
        .map_err(err)?;
        Ok(Self {
            compositor,
            layers: HashMap::new(),
        })
    }

    fn layer(&self, handle: u64) -> Option<u64> {
        self.layers.get(&handle).copied()
    }
}

impl SurfaceBackend for WmSurfaceBackend {
    fn create_layer(&mut self, handle: u64, name: &str, _width: i32, _height: i32) -> u64 {
        let layer = self
            .compositor
            .create_layer(None, name.to_string(), handle as i64);
        self.layers.insert(handle, layer);
        layer
    }

    fn apply(&mut self, ops: &[SurfaceOp]) -> Result<u64, String> {
        use acos_wm::compositor::Transaction;
        use acos_wm::geom::PointI;
        for op in ops {
            let txn = match op {
                SurfaceOp::Alpha { layer, alpha } => {
                    let Some(l) = self.layer(*layer) else { continue };
                    Transaction::SetAlpha {
                        layer: l,
                        alpha: *alpha,
                    }
                }
                SurfaceOp::Position { layer, x, y } => {
                    let Some(l) = self.layer(*layer) else { continue };
                    Transaction::SetPosition {
                        layer: l,
                        position: PointI::new(*x as i32, *y as i32),
                    }
                }
                SurfaceOp::Layer { layer, z } => {
                    let Some(l) = self.layer(*layer) else { continue };
                    Transaction::SetLayer {
                        layer: l,
                        z: *z as i64,
                    }
                }
                // Buffers are published by `submit_buffer`; every other setter
                // is recorded by the native layer and does not affect the
                // reference compositor's damage model yet.
                _ => continue,
            };
            self.compositor.apply(&txn).map_err(err)?;
        }
        Ok(self.compositor.generation())
    }

    fn submit_buffer(&mut self, layer: u64, _frame_number: u64) -> Result<u64, String> {
        if self.layer(layer).is_none() {
            return Err(format!("wms_java: submit_buffer for unknown layer {layer}"));
        }
        // A real buffer submission is the frame-publish signal.
        let _ = self.compositor.present(0).map_err(err)?;
        Ok(self.compositor.presents())
    }

    fn release(&mut self, layer: u64) {
        if let Some(l) = self.layers.remove(&layer) {
            let _ = self.compositor.remove_layer(l);
        }
    }

    fn generation(&self) -> u64 {
        self.compositor.generation()
    }
}

/// Install the `acos-wm` compositor binding into `acos-vm`'s `view.surface` seam
/// if it is not already installed. Idempotent.
pub fn install_surface_backend() -> WResult<()> {
    BACKEND_INSTALLED.with(|installed| {
        if *installed.borrow() {
            return Ok(());
        }
        match WmSurfaceBackend::new() {
            Ok(b) => {
                install_backend(Box::new(b));
                *installed.borrow_mut() = true;
                Ok(())
            }
            Err(e) => Err(e),
        }
    })
}

/// Number of frames published through the `view.surface` seam (test helper).
#[must_use]
pub fn published_frames() -> u64 {
    presented_frames()
}

/// `RELAYOUT_RES_FIRST_TIME`.
#[must_use]
pub const fn relayout_first_time() -> i32 {
    acos_wm::wms::RELAYOUT_RES_FIRST_TIME
}

/// `RELAYOUT_RES_SURFACE_CHANGED`.
#[must_use]
pub const fn relayout_surface_changed() -> i32 {
    acos_wm::wms::RELAYOUT_RES_SURFACE_CHANGED
}

/// The reference `ClientWindowFrames` for the hello-views window.
#[must_use]
pub fn reference_frames() -> ClientWindowFrames {
    ClientWindowFrames {
        frame: RectI::new(0, 128, 1080, 2337),
        display_frame: RectI::new(0, 0, 1080, 2400),
        parent_frame: RectI::new(0, 128, 1080, 2337),
        // `acos-wm` reports the parent frame as the attached frame for the
        // top-level app window, matching the measured capture.
        attached_frame: Some(RectI::new(0, 128, 1080, 2337)),
        is_parent_frame_clipped_by_display_cutout: false,
        compat_scale: 1.0,
    }
}

/// The reference `InsetsState` for a hidden IME.
#[must_use]
pub fn reference_insets() -> InsetsState {
    acos_wm::wms::reference_insets_state(
        &acos_wm::display::DisplaySpec::reference_1080x2400(),
        false,
    )
}

/// The reference `InsetsSourceControl$Array`.
#[must_use]
pub fn reference_insets_controls() -> InsetsSourceControlArray {
    acos_wm::wms::reference_insets_controls(
        &acos_wm::display::DisplaySpec::reference_1080x2400(),
        0x1000,
    )
}

/// The reference `MergedConfiguration`.
#[must_use]
pub fn reference_merged_configuration() -> MergedConfiguration {
    acos_wm::wms::reference_merged_configuration(
        &acos_wm::display::DisplaySpec::reference_1080x2400(),
    )
}

/// A `SurfaceControl` description for a layer handle (test helper).
#[must_use]
pub fn surface_control(name: &str, width: i32, height: i32, handle: u64) -> SurfaceControlInfo {
    SurfaceControlInfo {
        name: name.to_string(),
        width,
        height,
        handle: Some(handle),
    }
}
