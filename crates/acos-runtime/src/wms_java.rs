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
use std::os::fd::AsFd;

use acos_parcel::binder::BinderId;
use acos_parcel::{Parcel, ParcelError};
use acos_render::{Bitmap, BitmapConfig};
use acos_services::wms::WmsState;
use acos_vm::natives::binder::BinderDelivery;
use acos_vm::natives::fwk_surface::{presented_frames, SurfaceBackend, SurfaceOp};
use acos_wm::codec::{
    ClientWindowFrames, InsetsSourceControlArray, InsetsState, LayoutParams, MergedConfiguration,
    ParcelCodec, SurfaceControlInfo,
};
use acos_wm::display::{DisplaySpec, InsetsMode};
use acos_wm::geom::{PointI, RectI};
use acos_wm::wms::ADD_OKAY;

use crate::frame_handoff::CompletedFrame;

thread_local! {
    static WMS: RefCell<WmsState> = RefCell::new(WmsState::new());
    static BACKEND_INSTALLED: RefCell<bool> = const { RefCell::new(false) };
}

/// Run `f` with the process-global WMS state.
pub fn with_wms<R>(f: impl FnOnce(&mut WmsState) -> R) -> R {
    WMS.with(|w| f(&mut w.borrow_mut()))
}

/// Drain the outgoing `IWindow` callbacks queued by the last WMS operation
/// (T-37) and turn each into a oneway `BinderDelivery` for the app's
/// `IWindow$Stub`.
///
/// The callback queue is populated by `addToDisplayAsUser` (visibility, focus,
/// insets) and `relayout` (resized, insets) in the measured order; this is the
/// system -> app half of the WMS wire.
pub fn take_deliveries() -> Result<Vec<BinderDelivery>, String> {
    with_wms(|s| {
        let mut out = Vec::new();
        while let Some(pc) = s.wm.callbacks.pop() {
            // API 34's `IWindow` has no `windowFocusChanged` transaction
            // (`TRANSACTION_windowFocusChanged` was removed from the AIDL), so
            // putting code 17 on the wire would make the real `Stub` throw.
            // ACOS keeps the callback model for the replay harness but never
            // fakes a transaction the framework does not define.
            if matches!(
                pc.callback,
                acos_wm::callbacks::WmCallback::WindowFocusChanged { .. }
            ) {
                continue;
            }
            let data = pc.callback.to_parcel().map_err(|e| {
                format!(
                    "wms_java: callback {} encode: {e}",
                    pc.callback.method()
                )
            })?;
            out.push(BinderDelivery {
                target: pc.window,
                code: pc.callback.code(),
                flags: 1,
                data,
            });
        }
        Ok(out)
    })
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

/// The effective request wire ops for `method`.
///
/// The API-34 derived table for `addToDisplay`/`addToDisplayAsUser` carries one
/// trailing `I32` too many: the real server stub reads 3 / 4 ints (userId,
/// viewVisibility, displayId[, requestedVisibleTypes]) before the `InputChannel`
/// out-param. Reading the phantom int made the real `Stub$Proxy` request fail
/// at end-of-parcel, so it is trimmed here rather than faked.
fn effective_args(method: &acos_aidl::MethodInfo) -> Vec<acos_aidl::WireOp> {
    use acos_aidl::WireOp;
    let mut ops = method.args.clone();
    let trim = match method.name.as_str() {
        "addToDisplay" | "addToDisplayWithoutInputChannel" | "addToDisplayAsUser" => 1,
        _ => 0,
    };
    for _ in 0..trim {
        if matches!(ops.last(), Some(WireOp::I32)) {
            ops.pop();
        }
    }
    ops
}

fn decode_args(method: &acos_aidl::MethodInfo, p: &mut Parcel) -> WResult<Args> {
    use acos_aidl::WireOp;
    let mut out = Args::default();
    let ops = effective_args(method);
    for op in &ops {
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

/// Encode the `out InputChannel` parameter exactly as the real framework does.
///
/// The AIDL layer writes the presence marker (`1`/`0`); when present it then
/// calls `InputChannel.writeToParcel`, whose whole body is
/// `nativeWriteToParcel(Parcel, mPtr)`. ACOS's native counterpart
/// (`fwk_input::ic_write_to_parcel`) writes `String16 name`, a file descriptor
/// and the token binder, and `nativeReadFromParcel` reads exactly that. The
/// descriptor is a *duplicate* of the client end of the window's real
/// socketpair, so the app can install an `InputEventReceiver` while the WMS
/// keeps the dispatcher end.
fn encode_input_channel(add: &acos_wm::wms::AddWindowResult, p: &mut Parcel) -> WResult<()> {
    let handle = add.input_channel.channel_handle;
    let fd = with_wms(|s| s.wm.channels.get(handle).and_then(|c| c.duplicate_client_fd()));
    let Some(fd) = fd else {
        // No fd-backed channel (unit-test in-memory pair): write an absent
        // channel rather than a bogus descriptor.
        p.write_int32(0);
        return Ok(());
    };
    p.write_int32(1);
    p.write_string16_opt(Some(&add.input_channel.name)).map_err(perr)?;
    p.write_file_descriptor(fd.as_fd()).map_err(perr)?;
    p.write_strong_binder_opt(add.input_channel.token);
    Ok(())
}

fn encode_add_result(
    result: i32,
    add: Option<&acos_wm::wms::AddWindowResult>,
    p: &mut Parcel,
) -> WResult<()> {
    p.write_int32(result);
    match add {
        Some(a) if result == ADD_OKAY => {
            encode_input_channel(a, p)?;
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
    if std::env::var_os("ACOS_WMS_TRACE").is_some() {
        eprintln!("WMS-TRACE {descriptor} code={code} {}", method.name);
    }
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
            if result.result == ADD_OKAY {
                crate::input_java::install_window_writer(result.window)
                    .map_err(|e| err(format!("input writer: {e}")))?;
            }
            encode_add_result(result.result, Some(&result), reply)?;
        }
        "addToDisplayWithoutInputChannel" => {
            let params = args
                .params
                .clone()
                .ok_or_else(|| err("addToDisplayWithoutInputChannel: missing LayoutParams"))?;
            let window = window.ok_or_else(|| err("null IWindow"))?;
            let result = with_wms(|s| s.add_to_display_as_user(window, params, 0)).map_err(err)?;
            if result.result == ADD_OKAY {
                crate::input_java::install_window_writer(result.window)
                    .map_err(|e| err(format!("input writer: {e}")))?;
            }
            encode_add_result(result.result, Some(&result), reply)?;
        }
        "relayout" => {
            let window = window.ok_or_else(|| err("relayout: null IWindow"))?;
            // `ViewRootImpl` may call `relayout` with a null `attrs` when the
            // window attributes did not change; the real WMS keeps the last
            // stored `LayoutParams` in that case. Fall back to the window
            // record's params instead of dropping the whole reply (which would
            // leave the client's out-params stale).
            let params = match args.params.clone() {
                Some(p) => p,
                None => with_wms(|s| {
                    s.wm.windows.get(&window.0).map(|rec| rec.params.clone())
                })
                .ok_or_else(|| err("relayout: missing LayoutParams and unknown window"))?,
            };
            // The relayout argument order is `requestedWidth, requestedHeight,
            // viewVisibility, flags, seq, lastSyncSeqId`; the sync seq is the
            // fifth int, not the first. The first two are the client's measured
            // (requested) size, which the real WMS uses for a `WRAP_CONTENT`
            // window frame.
            let requested_width = args.ints.first().copied().unwrap_or(-1);
            let requested_height = args.ints.get(1).copied().unwrap_or(-1);
            let sync_seq = args.ints.get(4).copied().unwrap_or(-1);
            let r = with_wms(|s| {
                s.relayout_with_requested(
                    window.0,
                    params,
                    requested_width,
                    requested_height,
                    sync_seq,
                )
            })
            .map_err(err)?;
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
        "getWindowId" => {
            let w = window.ok_or_else(|| err("getWindowId: null IWindow"))?;
            // `IWindowId` is the framework's opaque per-window identity handle
            // (used by the IME/accessibility paths). The only method the
            // framework calls on it is the local `asBinder()`, so a stable
            // synthetic binder per `IWindow` is enough. Without a reply the
            // popup's `ViewRootImpl.setView` aborts while reading the strong
            // binder (`parcel underflow`) and never installs its input
            // receiver, so a tap routed to the popup window is never
            // dispatched to the popup's view tree.
            reply.write_strong_binder_opt(Some(BinderId(0x1d_0000_0000 + w.0)));
        }
        "reportDrawn" | "reportDrawnAsync" => {
            if let Some(w) = window {
                with_wms(|s| s.finish_drawing(w.0, -1)).map_err(err)?;
            }
        }
        // `ViewRootImpl.performTraversals`/`draw` call sites that only need a
        // faithful scalar/oneway answer. The display is 1080x2400@420.
        "outOfMemory" => {
            // The real `WMS` returns false unless the system is under memory
            // pressure; ACOS has no memory-pressure signal, so the honest
            // answer is "not out of memory".
            reply.write_bool(false);
        }
        "cancelDraw" => {
            // `ViewRootImpl` uses this to discard a queued draw when a relayout
            // arrives first; there is no queued draw to cancel in ACOS.
            reply.write_bool(false);
        }
        "relayoutAsync" => {
            // Oneway form of `relayout`. The app does not read a reply; keep the
            // window state in sync by running the same relayout and dropping
            // the result.
            if let Some(w) = window {
                let params = match args.params.clone() {
                    Some(p) => Some(p),
                    None => with_wms(|s| s.wm.windows.get(&w.0).map(|rec| rec.params.clone())),
                };
                if let Some(params) = params {
                    let requested_width = args.ints.first().copied().unwrap_or(-1);
                    let requested_height = args.ints.get(1).copied().unwrap_or(-1);
                    let sync_seq = args.ints.get(4).copied().unwrap_or(-1);
                    let _ = with_wms(|s| {
                        s.relayout_with_requested(
                            w.0,
                            params,
                            requested_width,
                            requested_height,
                            sync_seq,
                        )
                    })
                    .map_err(err)?;
                }
            }
        }
        "clearTouchableRegion" => {
            // Oneway; the WMS clears the window's touchable region. The V1
            // window record has no separate touchable-region field, so touch
            // the record (a missing window is not an error for a oneway).
            if let Some(w) = window {
                let _ = with_wms(|s| s.wm.windows.contains_key(&w.0));
            }
        }
        "performHapticFeedbackAsync" => {
            // Oneway; the launch path does not require a haptic device.
        }
        "reportKeepClearAreasChanged" => {
            // Oneway; keep-clear areas are not modelled by the V1 compositor.
        }
        "dragRecipientEntered" | "dragRecipientExited" | "reportDropResult" => {
            // Drag-and-drop is not on the first-frame launch path; accept the
            // oneway calls without claiming a drag session.
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
    /// Monotonic client frame number handed to the compositor.
    next_frame: u64,
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
            next_frame: 0,
        })
    }

    fn layer(&self, handle: u64) -> Option<u64> {
        self.layers.get(&handle).copied()
    }

    fn ensure_layer(&mut self, handle: u64) -> u64 {
        if let Some(layer) = self.layers.get(&handle) {
            return *layer;
        }
        let layer = self.compositor.create_layer(
            None,
            format!("frame-handoff-{handle}"),
            handle as i64,
        );
        self.layers.insert(handle, layer);
        layer
    }

    /// Submit a completed RGBA8888 frame to the compositor layer bound to
    /// `window` and return the completed-frame record.
    ///
    /// This is the only path that publishes a frame: `finishDrawing` alone
    /// never reaches here.
    pub fn submit_frame(
        &mut self,
        window: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
        cause_id: i64,
    ) -> Result<CompletedFrame, String> {
        let layer = self.ensure_layer(window);
        let bitmap = bitmap_from_rgba(width, height, rgba)?;
        self.next_frame = self.next_frame.wrapping_add(1);
        self.compositor
            .set_buffer(layer, bitmap, self.next_frame, cause_id)
            .map_err(err)?;
        let seq = self.compositor.present(cause_id).map_err(err)?;
        let frame = self
            .compositor
            .capture()
            .ok_or_else(|| "wms_java: no frame after present".to_string())?;
        Ok(CompletedFrame {
            frame_id: seq.unwrap_or(frame.seq),
            window,
            content_hash: frame.content_hash,
            cause_id: frame.cause_id,
            width: frame.width,
            height: frame.height,
            pixels: frame.rgba8().to_vec(),
        })
    }
}

fn bitmap_from_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Bitmap, String> {
    let mut bitmap = Bitmap::new(width, height, BitmapConfig::Rgba8888).map_err(err)?;
    let expected = width as usize * height as usize * 4;
    if rgba.len() < expected {
        return Err(format!(
            "wms_java: rgba buffer {} bytes, expected {expected}",
            rgba.len()
        ));
    }
    for (i, pixel) in bitmap.pixels_mut().iter_mut().enumerate() {
        let o = i * 4;
        *pixel = u32::from_le_bytes([rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]]);
    }
    Ok(bitmap)
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
        match crate::frame_handoff::install_frame_handoff() {
            Ok(_store) => {
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

/// The reference display with the process's `ACOS_WM_INSETS` model.
#[must_use]
pub fn reference_display() -> DisplaySpec {
    DisplaySpec::reference_with_insets(InsetsMode::from_env(
        acos_wm::display::DEFAULT_INSETS_MODE,
    ))
}

/// The reference `ClientWindowFrames` for the hello-views window.
#[must_use]
pub fn reference_frames() -> ClientWindowFrames {
    let display = reference_display();
    ClientWindowFrames {
        frame: display.window_frame(),
        display_frame: display.frame,
        parent_frame: display.window_frame(),
        // `acos-wm` reports the parent frame as the attached frame for the
        // top-level app window, matching the measured capture.
        attached_frame: Some(display.window_frame()),
        is_parent_frame_clipped_by_display_cutout: false,
        compat_scale: 1.0,
    }
}

/// The reference `InsetsState` for a hidden IME.
#[must_use]
pub fn reference_insets() -> InsetsState {
    acos_wm::wms::reference_insets_state(&reference_display(), false)
}

/// The reference `InsetsSourceControl$Array`.
#[must_use]
pub fn reference_insets_controls() -> InsetsSourceControlArray {
    acos_wm::wms::reference_insets_controls(&reference_display(), 0x1000)
}

/// The reference `MergedConfiguration`.
#[must_use]
pub fn reference_merged_configuration() -> MergedConfiguration {
    acos_wm::wms::reference_merged_configuration(&reference_display())
}

/// A `SurfaceControl` description for a layer handle (test helper).
#[must_use]
pub fn surface_control(name: &str, width: i32, height: i32, handle: u64) -> SurfaceControlInfo {
    SurfaceControlInfo {
        name: name.to_string(),
        width,
        height,
        handle: Some(handle),
        client: None,
        layer_binder: None,
    }
}
