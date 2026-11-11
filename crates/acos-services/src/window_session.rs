//! `android.view.IWindowSession` served by the `ServiceHost` itself (W1).
//!
//! `ViewRootImpl`'s constructor calls `IWindowManager.openSession` once and then
//! every window operation of the launch goes through the returned
//! `IWindowSession`. P4c §5 recorded the wall: the `window` service answered
//! `openSession` with `UNSUPPORTED_ANDROID_API`, because the session binder
//! `acos-wm` allocates is an id the `ServiceHost` does not know.
//!
//! This module makes the session a real host node:
//!
//! * `registry::install` registers it with [`ServiceHost::register_dynamic`]
//!   under the descriptor `android.view.IWindowSession`, so the binder the
//!   `openSession` reply carries is routable back into this process;
//! * [`WindowSessionService`] answers the transactions `ViewRootImpl` makes, in
//!   the **image's** argument/reply order (see [`codes`]);
//! * the shared [`WmsState`] is the same one the `window` service uses, so the
//!   policy is still `acos-wm`'s.
//!
//! # Why `raw_transact` and not the decoded `WireOp` engine
//!
//! `addToDisplayAsUser`/`relayout` carry `WindowManager$LayoutParams`,
//! `InsetsState`, `InsetsSourceControl$Array`, `ClientWindowFrames`,
//! `MergedConfiguration` and `SurfaceControl` inline. Their byte layouts are
//! transcribed in [`acos_wm::codec`] (DEX-derived, `results/wm/`); re-encoding
//! them through the flat value engine here would be a second, weaker source of
//! truth. `raw_transact` reads and writes the parcel directly with those
//! codecs, exactly like `acos-runtime`'s `wms_java` (the VM path that has been
//! serving the same interface) — the shapes are the same because both use
//! `acos_wm::codec`.
//!
//! # Evidence for the wire shapes
//!
//! Every order below was read out of the **image's** generated stub
//! (`classes4.dex` `Landroid/view/IWindowSession$Stub$Proxy;`, disassembled with
//! `cargo run -p acos-dex --example dexdump`). The transaction codes are the
//! image's, which is one behind the `android-14.0.0_r75` AIDL for the tail of
//! the interface (`grantInputChannel` is 35, not 36; `clearTouchableRegion` is
//! 40, not 41; the image additionally has `transferEmbeddedTouchFocusToHost`),
//! so a table transcribed from the tag alone would dispatch one method off.
//! [`codes`] records the image values.
//!
//! # What is deliberately not real
//!
//! * `relayout`'s `outSurfaceControl` is a **structural placeholder**: the body
//!   is the exact `SurfaceControl` shape (`String8 name`, `width`, `height`,
//!   presence, 8-byte handle) but the handle is not a SurfaceFlinger layer —
//!   no `SurfaceControl$Transaction` is ever applied to it, and the pixel path
//!   still comes from the ACOS renderer, not from a buffer the app queued. What
//!   the real thing needs (a `SurfaceControl` owned by a compositor, a
//!   `Transaction` applier and the vsync/`finishDrawing` handshake) is W-R's
//!   surface/vsync architecture, not designed here.
//! * `setInsets`' `Region` is not decoded (native-backed body, last argument,
//!   oneway); the window's touchable region is not modelled by `acos-wm` V1.
//! * `relayout`'s trailing `out Bundle` (the sync-seq channel) is written null:
//!   ACOS has no surface sync group, and the framework treats null as "no
//!   sync".

use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_wm::codec::{LayoutParams, ParcelCodec};
use acos_wm::geom::RectI;
use acos_wm::wms::{AddWindowResult, RelayoutResult};

use crate::error::{Result as SResult, ServiceError};
use crate::host::ServiceObject;
use crate::wms::WmsState;

/// The `IWindowSession` transaction codes of the **API-34 image**
/// (`classes4.dex`, `IWindowSession$Stub$Proxy`).
pub mod codes {
    /// `addToDisplay(IWindow, LayoutParams, viewVisibility, layerStackId,
    /// requestedVisibleTypes, out InputChannel, out InsetsState,
    /// out InsetsSourceControl.Array, out Rect, out float[])` → `int`.
    pub const ADD_TO_DISPLAY: u32 = 1;
    /// Same as [`ADD_TO_DISPLAY`] plus a `userId`.
    pub const ADD_TO_DISPLAY_AS_USER: u32 = 2;
    /// `addToDisplayWithoutInputChannel` → `int`.
    pub const ADD_TO_DISPLAY_WITHOUT_INPUT_CHANNEL: u32 = 3;
    /// `remove(IBinder clientToken)` (oneway at the Java level, request has a reply).
    pub const REMOVE: u32 = 4;
    /// `relayout(...)` → `int`.
    pub const RELAYOUT: u32 = 5;
    /// `relayoutAsync(...)` (oneway).
    pub const RELAYOUT_ASYNC: u32 = 6;
    /// `outOfMemory(IWindow)` → `boolean`.
    pub const OUT_OF_MEMORY: u32 = 7;
    /// `setInsets(IWindow, touchableInsets, Rect, Rect, Region)` (oneway).
    pub const SET_INSETS: u32 = 8;
    /// `finishDrawing(IWindow, SurfaceControl.Transaction, seqId)` (oneway).
    pub const FINISH_DRAWING: u32 = 9;
    /// `performHapticFeedback(effectId, always[, fromIme])` → `boolean`.
    pub const PERFORM_HAPTIC_FEEDBACK: u32 = 10;
    /// `performHapticFeedbackAsync` (oneway).
    pub const PERFORM_HAPTIC_FEEDBACK_ASYNC: u32 = 11;
    /// `onRectangleOnScreenRequested(IBinder token, Rect rectangle)` (oneway).
    pub const ON_RECTANGLE_ON_SCREEN_REQUESTED: u32 = 25;
    /// `getWindowId(IBinder window)` → `IWindowId`.
    pub const GET_WINDOW_ID: u32 = 26;
    /// `pokeDrawLock(IBinder window)`.
    pub const POKE_DRAW_LOCK: u32 = 27;
    /// `updateRequestedVisibleTypes(IWindow, int)` (oneway).
    pub const UPDATE_REQUESTED_VISIBLE_TYPES: u32 = 32;
    /// `reportSystemGestureExclusionChanged(IWindow, List<Rect>)` (oneway).
    pub const REPORT_SYSTEM_GESTURE_EXCLUSION_CHANGED: u32 = 33;
    /// `reportKeepClearAreasChanged(IWindow, List<Rect>, List<Rect>)` (oneway).
    pub const REPORT_KEEP_CLEAR_AREAS_CHANGED: u32 = 34;
    /// `setOnBackInvokedCallbackInfo(IWindow, OnBackInvokedCallbackInfo)` (oneway).
    pub const SET_ON_BACK_INVOKED_CALLBACK_INFO: u32 = 39;
    /// `clearTouchableRegion(IWindow)`.
    pub const CLEAR_TOUCHABLE_REGION: u32 = 40;
    /// `cancelDraw(IWindow)` → `boolean`.
    pub const CANCEL_DRAW: u32 = 41;
}

/// The descriptor every transaction of this interface carries.
pub const DESCRIPTOR: &str = "android.view.IWindowSession";

/// The host node behind `IWindowManager.openSession`.
///
/// One instance per `ServiceHost`; it shares the [`WmsState`] with the `window`
/// service, so `addToDisplayAsUser` here and `relayout` there see the same
/// windows.
pub struct WindowSessionService {
    state: Arc<Mutex<WmsState>>,
    /// The `IWindowId` binder `getWindowId` answers with (registered as an aux
    /// binder so it is routable rather than a fabricated id).
    window_id_binder: BinderId,
    /// The compositor-client registry `relayout` binds the window's
    /// `SurfaceControl` to (W5). `None` keeps the W3 placeholder body — that is
    /// the state of a host with no compositor installed.
    compositor: Option<Arc<Mutex<crate::surface_client::SurfaceState>>>,
}

impl std::fmt::Debug for WindowSessionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowSessionService")
            .field("window_id", &self.window_id_binder)
            .finish()
    }
}

impl WindowSessionService {
    /// Build the session node around shared `acos-wm` state.
    #[must_use]
    pub fn new(state: Arc<Mutex<WmsState>>, window_id_binder: BinderId) -> Self {
        Self {
            state,
            window_id_binder,
            compositor: None,
        }
    }

    /// Serve `relayout` with a real compositor behind it (W5).
    ///
    /// Without this the reply's `SurfaceControl` carries W3's placeholder
    /// binders; with it, the control names the compositor's
    /// `ISurfaceComposerClient` node and a layer handle that
    /// `createSurface`/`setTransactionState` resolve in that registry.
    #[must_use]
    pub fn with_compositor(
        mut self,
        compositor: Arc<Mutex<crate::surface_client::SurfaceState>>,
    ) -> Self {
        self.compositor = Some(compositor);
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WmsState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn unsupported(code: u32) -> ServiceError {
        ServiceError::UnsupportedAndroidApi {
            interface: DESCRIPTOR.to_string(),
            code,
            method: method_name(code).unwrap_or_else(|| format!("code {code}")),
        }
    }
}

/// The method name for a code, from the `acos-aidl` table (`None` when the
/// table has no entry — the caller then reports `code N`).
#[must_use]
pub fn method_name(code: u32) -> Option<String> {
    acos_aidl::lookup(DESCRIPTOR, code).map(|m| m.name.clone())
}

// ---------------------------------------------------------------------------
// Reads (the order the image's proxy writes)
// ---------------------------------------------------------------------------

fn perr(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::Internal(format!("{DESCRIPTOR}: parcel: {e}"))
}

/// An `acos-wm` policy error: the host turns this into a structured
/// `WM_*` failure for the guest, never a silent success.
fn werr(e: acos_wm::error::WmError) -> ServiceError {
    ServiceError::Internal(format!("{DESCRIPTOR}: {e}"))
}

/// `Parcel.writeTypedObject(LayoutParams)`: presence marker, then the body.
fn read_layout_params(data: &mut Parcel) -> SResult<Option<LayoutParams>> {
    if data.read_int32().map_err(perr)? == 0 {
        return Ok(None);
    }
    LayoutParams::read_from_parcel(data)
        .map(Some)
        .map_err(|e| ServiceError::Internal(format!("{DESCRIPTOR}: LayoutParams: {e}")))
}

/// `Parcel.writeTypedObject(Rect)`: presence marker, then four ints.
fn read_rect(data: &mut Parcel) -> SResult<Option<RectI>> {
    if data.read_int32().map_err(perr)? == 0 {
        return Ok(None);
    }
    RectI::read_from_parcel(data)
        .map(Some)
        .map_err(|e| ServiceError::Internal(format!("{DESCRIPTOR}: Rect: {e}")))
}

/// `writeTypedList(List<Rect>)`: count, then one typed `Rect` per element.
fn skip_typed_rect_list(data: &mut Parcel) -> SResult<()> {
    let count = data.read_int32().map_err(perr)?;
    for _ in 0..count.max(0) {
        let _ = read_rect(data)?;
    }
    Ok(())
}

fn read_window(data: &mut Parcel) -> SResult<Option<BinderId>> {
    data.read_strong_binder().map_err(perr)
}

// ---------------------------------------------------------------------------
// Writes (the order the image's proxy reads)
// ---------------------------------------------------------------------------

/// `Parcel.writeTypedObject` for an out parcelable: `1` + body, or `0`.
fn present<T: ParcelCodec>(reply: &mut Parcel, value: Option<&T>) -> SResult<()> {
    match value {
        Some(v) => {
            reply.write_int32(1);
            v.write_to_parcel(reply)
                .map_err(|e| ServiceError::Internal(format!("{DESCRIPTOR}: {}: {e}", T::NAME)))
        }
        None => {
            reply.write_int32(0);
            Ok(())
        }
    }
}

/// `out InputChannel` exactly as the image's AIDL stub + `InputChannel`
/// natives put it on the wire:
///
/// ```text
/// int32  1                 // AIDL `writeTypedObject(InputChannel)` presence
/// int32  1                 // `nativeReadFromParcel`'s `isInitialized`
/// strong binder  token     // `InputChannel::writeToParcel` (libinput.so:0x78d40)
/// String16       name      //   `writeUtf8AsUtf16`
/// file descriptor fd       //   `writeUniqueFileDescriptor`
/// ```
///
/// The order is **not** `name, fd, token`: `InputChannel::readFromParcel`
/// (`libinput.so:0x78dc0`, reached from
/// `android_view_InputChannel_nativeReadFromParcel` which first reads the
/// `isInitialized` int) reads the token binder, then the name, then the
/// descriptor. Writing the name first desynchronises the whole reply at the
/// first native read, which is what made the guest mis-parse the
/// `InsetsState` that follows (`results/art-runtime/L1.md` E2). Evidence:
/// the image's own `IWindowSession$Stub.onTransact` writes
/// `writeTypedObject(inputChannel, flags)` and `libandroid_runtime.so`'s
/// native write calls `InputChannel::writeToParcel` inline.
///
/// The descriptor is a duplicate of the client end of the window's socketpair,
/// so the framework installs an `InputEventReceiver` on a channel the ACOS
/// dispatcher keeps the other end of.
fn write_input_channel(state: &mut WmsState, add: &AddWindowResult, reply: &mut Parcel) -> SResult<()> {
    let handle = add.input_channel.channel_handle;
    let fd = state
        .wm
        .channels
        .get(handle)
        .and_then(acos_wm::transport::Channel::duplicate_client_fd);
    // The image's stub creates the `InputChannel` object *before* the call
    // (`new-instance ... InputChannel; <init>()V`) and then writes it with
    // `writeTypedObject`, so the AIDL presence marker is **always** `1`; a
    // channel the server never initialised is written as `isInitialized = 0`
    // (`nativeWriteToParcel`: `parcel->writeInt32(0)`), not as an absent
    // object. That is also the failure path of a rejected add.
    reply.write_int32(1);
    let Some(fd) = fd else {
        reply.write_int32(0); // native `isInitialized`
        return Ok(());
    };
    use std::os::fd::AsFd;
    reply.write_int32(1); // native `isInitialized`
    reply.write_strong_binder_opt(add.input_channel.token);
    reply
        .write_string16_opt(Some(&add.input_channel.name))
        .map_err(perr)?;
    reply.write_file_descriptor(fd.as_fd()).map_err(perr)?;
    Ok(())
}

/// `out float[] sizeCompatScale`.
///
/// An AIDL `out float[]` is **caller-allocated**: the client writes its length in
/// the request (`data.writeInt(array.length)`, visible in
/// `IWindowSession$Stub$Proxy.addToDisplayAsUser` as `array-length`), the stub
/// allocates exactly that many floats, and `Parcel.readFloatArray(val)` throws
/// `RuntimeException("bad array lengths")` unless the reply's count matches
/// `val.length` (`Parcel.java:1719`). So the count is the client's length, not
/// ours: pad with `0.0` / truncate the policy values to it.
fn write_size_compat_scale(reply: &mut Parcel, values: &[f32], client_len: i32) -> SResult<()> {
    let len = usize::try_from(client_len).unwrap_or(0);
    let mut out = vec![0.0f32; len];
    for (slot, value) in out.iter_mut().zip(values) {
        *slot = *value;
    }
    reply.write_float_array(Some(&out)).map_err(perr)
}

/// `addToDisplayAsUser`'s reply, in the image's read order:
/// `int result`, then `InputChannel`, `InsetsState`,
/// `InsetsSourceControl$Array`, `Rect attachedFrame` (each typed) and
/// `float[] sizeCompatScale`.
///
/// The image's `onTransact` creates all four out objects before dispatching and
/// writes them unconditionally (`writeTypedObject` + `writeFloatArray`), so the
/// presence markers do not depend on the result: a rejected add still carries a
/// full `InsetsState`/controls body (the values `acos-wm` returns for the
/// rejection) and an uninitialised channel.
fn write_add_result(
    state: &mut WmsState,
    add: &AddWindowResult,
    size_compat_scale_len: i32,
    reply: &mut Parcel,
) -> SResult<()> {
    reply.write_int32(add.result);
    write_input_channel(state, add, reply)?;
    present(reply, Some(&add.insets_state))?;
    present(reply, Some(&add.insets_controls))?;
    present(reply, Some(&add.out_frame))?;
    write_size_compat_scale(reply, &add.out_float, size_compat_scale_len)
}

/// `relayout`'s reply, in the image's read order: `int result`,
/// `ClientWindowFrames`, `MergedConfiguration`, `SurfaceControl`, `InsetsState`,
/// `InsetsSourceControl$Array` (each typed) and the trailing `Bundle`.
fn write_relayout_result(r: &RelayoutResult, reply: &mut Parcel) -> SResult<()> {
    reply.write_int32(r.result);
    present(reply, Some(&r.frames))?;
    present(reply, Some(&r.merged_configuration))?;
    present(reply, Some(&r.surface))?;
    present(reply, Some(&r.insets_state))?;
    present(reply, Some(&r.insets_controls))?;
    // `out Bundle` (the SyncSeqId channel): null, the framework's own "no sync
    // group" case.
    reply.write_int32(0);
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

impl ServiceObject for WindowSessionService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> SResult<crate::host::CallResult> {
        // Every method of this interface carries a parcelable payload; none is
        // expressible as decoded values, so the generic path is a structured
        // error naming the interface and method (never a silent default).
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> SResult<bool> {
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        let mut state = self.lock();
        let window_id = self.window_id_binder;
        match code {
            codes::ADD_TO_DISPLAY | codes::ADD_TO_DISPLAY_AS_USER => {
                // IWindow, LayoutParams (typed), viewVisibility, layerStackId,
                // [userId], requestedVisibleTypes, then `float[]` (its length).
                let window = read_window(data)?.unwrap_or(BinderId(0));
                let params = read_layout_params(data)?;
                let view_visibility = data.read_int32().map_err(perr)?;
                let _layer_stack = data.read_int32().map_err(perr)?;
                let _user_id = if code == codes::ADD_TO_DISPLAY_AS_USER {
                    data.read_int32().map_err(perr)?
                } else {
                    0
                };
                let requested_visible_types = data.read_int32().map_err(perr)?;
                // The client's `float[]` out array length (`array-length` in the
                // image's proxy). The reply count must match it exactly.
                let size_compat_scale_len = data.read_int32().map_err(perr)?;
                let params = params.ok_or_else(|| {
                    ServiceError::Internal(format!(
                        "{DESCRIPTOR}: {}: missing LayoutParams",
                        method_name(code).unwrap_or_else(|| "addToDisplayAsUser".into())
                    ))
                })?;
                let add = state
                    .add_to_display_as_user(window, params, requested_visible_types)
                    .map_err(werr)?;
                crate::exception::write_no_exception(reply);
                write_add_result(&mut state, &add, size_compat_scale_len, reply)?;
                // W5 finding: the real WMS also sends `IWindow`
                // `dispatchAppVisibility(true)` here. Delivering it from this
                // node made the guest hang in the run that tried it, so the
                // queued callbacks are left in `acos-wm` and the wall is
                // recorded in results/art-runtime/W5.md (see
                // `getHostVisibility()`/`View.GONE`).
                // `viewVisibility` is not part of the add policy yet; record it
                // in the trace surface only (see W1.md §4).
                let _ = view_visibility;
            }
            codes::ADD_TO_DISPLAY_WITHOUT_INPUT_CHANNEL => {
                let window = read_window(data)?.unwrap_or(BinderId(0));
                let params = read_layout_params(data)?;
                let _ = data.read_int32().map_err(perr)?; // viewVisibility
                let _ = data.read_int32().map_err(perr)?; // layerStackId
                let size_compat_scale_len = data.read_int32().map_err(perr)?;
                let params = params.ok_or_else(|| {
                    ServiceError::Internal(format!(
                        "{DESCRIPTOR}: addToDisplayWithoutInputChannel: missing LayoutParams"
                    ))
                })?;
                let add = state.add_to_display_as_user(window, params, 0).map_err(werr)?;
                crate::exception::write_no_exception(reply);
                reply.write_int32(add.result);
                // InsetsState, Rect attachedFrame typed; no InputChannel.
                present(reply, Some(&add.insets_state))?;
                present(reply, Some(&add.out_frame))?;
                write_size_compat_scale(reply, &add.out_float, size_compat_scale_len)?;
            }
            codes::REMOVE => {
                if let Some(binder) = read_window(data)? {
                    state.remove(binder.0).map_err(werr)?;
                }
                crate::exception::write_no_exception(reply);
            }
            codes::RELAYOUT => {
                // IWindow, LayoutParams (typed), requestedWidth, requestedHeight,
                // viewVisibility, flags, seq, lastSyncSeqId.
                let window = read_window(data)?.ok_or_else(|| {
                    ServiceError::Internal(format!("{DESCRIPTOR}: relayout: null IWindow"))
                })?;
                let params = read_layout_params(data)?;
                let requested_width = data.read_int32().map_err(perr)?;
                let requested_height = data.read_int32().map_err(perr)?;
                let _view_visibility = data.read_int32().map_err(perr)?;
                let _flags = data.read_int32().map_err(perr)?;
                let seq = data.read_int32().map_err(perr)?;
                let _last_sync_seq_id = data.read_int32().map_err(perr)?;
                // `attrs` is null when the client's attributes did not change;
                // the real WMS then keeps the stored params.
                let params = match params {
                    Some(p) => p,
                    None => state
                        .wm
                        .windows
                        .get(&window.0)
                        .map(|rec| rec.params.clone())
                        .ok_or_else(|| {
                            ServiceError::Internal(format!(
                                "{DESCRIPTOR}: relayout: null attrs and unknown window {}",
                                window.0
                            ))
                        })?,
                };
                let mut r = state.relayout_with_requested(
                    window.0,
                    params,
                    requested_width,
                    requested_height,
                    seq,
                )
                .map_err(werr)?;
                // W5: bind the window's `SurfaceControl` to a real compositor
                // layer before it goes on the wire. A failure (exhausted handle
                // pool) leaves the W3 placeholder body, which is a legal reply
                // but cannot get a BLAST surface — the framework's own
                // "invalid SurfaceControl" path, never a fabricated layer.
                if let Some(compositor) = self.compositor.clone() {
                    if state.bind_surface_layer(&mut r.surface, &compositor).is_none() {
                        eprintln!(
                            "{DESCRIPTOR}: relayout: no compositor layer bound for window {}; \
                             replying with the placeholder SurfaceControl",
                            window.0
                        );
                    }
                }
                crate::exception::write_no_exception(reply);
                write_relayout_result(&r, reply)?;
                // W5 finding: `resized(reportDraw=true)` is the other callback
                // the client needs before it will draw; same open item as the
                // visibility callback above.
            }
            codes::RELAYOUT_ASYNC => {
                // Oneway: same request shape, no reply body.
                let window = read_window(data)?.ok_or_else(|| {
                    ServiceError::Internal(format!("{DESCRIPTOR}: relayoutAsync: null IWindow"))
                })?;
                let params = read_layout_params(data)?;
                let requested_width = data.read_int32().map_err(perr)?;
                let requested_height = data.read_int32().map_err(perr)?;
                let _ = data.read_int32().map_err(perr)?;
                let _ = data.read_int32().map_err(perr)?;
                let seq = data.read_int32().map_err(perr)?;
                let _ = data.read_int32().map_err(perr)?;
                if let Some(params) = params {
                    let _ = state.relayout_with_requested(
                        window.0,
                        params,
                        requested_width,
                        requested_height,
                        seq,
                    )
                    .map_err(werr)?;
                }
            }
            codes::OUT_OF_MEMORY => {
                let _ = read_window(data)?;
                crate::exception::write_no_exception(reply);
                // No memory-pressure signal exists in ACOS, so the honest
                // answer is "the system is not out of memory".
                reply.write_bool(false);
            }
            codes::PERFORM_HAPTIC_FEEDBACK => {
                let _effect = data.read_int32().map_err(perr)?;
                let _always = data.read_bool().map_err(perr)?;
                // The image writes only two arguments here; a third boolean is
                // read leniently when present.
                if data.data_avail() > 0 {
                    let _ = data.read_int32().map_err(perr)?;
                }
                crate::exception::write_no_exception(reply);
                reply.write_bool(false);
            }
            codes::SET_INSETS => {
                let window = read_window(data)?;
                let _touchable_insets = data.read_int32().map_err(perr)?;
                let _content = read_rect(data)?;
                let _visible = read_rect(data)?;
                // Trailing `Region` is native-backed and last; its body is not
                // decoded (W1.md §4).
                if let Some(window) = window {
                    state.set_insets(window.0, None, None).map_err(werr)?;
                }
            }
            codes::FINISH_DRAWING => {
                let window = read_window(data)?;
                // `SurfaceControl$Transaction` is a typed object whose body is
                // the native op buffer. It is **not** interpreted yet (the
                // post-draw transaction is a different flatten from the
                // `setTransactionState` parcel; see results/art-runtime/W5.md).
                // Record its size and whether it carries a `GraphicBuffer`
                // (`GB01`) so the next wall is measurable from the peer log.
                if data.read_int32().map_err(perr)? != 0 {
                    let body = data.read_byte_array().map_err(perr)?;
                    if std::env::var_os("ACOS_W5_BUF").is_some() {
                        let bytes = body.as_deref().unwrap_or(&[]);
                        let has_gb = bytes.windows(4).any(|w| w == b"GB01");
                        eprintln!(
                            "ACOSW5 finishDrawing transaction bytes={} has_gralloc_buffer={has_gb}",
                            bytes.len()
                        );
                    }
                }
                let seq = data.read_int32().map_err(perr)?;
                if let Some(window) = window {
                    state.finish_drawing(window.0, seq).map_err(werr)?;
                }
            }
            codes::ON_RECTANGLE_ON_SCREEN_REQUESTED => {
                let window = read_window(data)?;
                let rect = read_rect(data)?;
                if let Some(window) = window {
                    state.on_rectangle_on_screen_requested(window.0, rect).map_err(werr)?;
                }
            }
            codes::GET_WINDOW_ID => {
                let _ = read_window(data)?;
                crate::exception::write_no_exception(reply);
                reply.write_strong_binder_opt(Some(window_id));
            }
            codes::POKE_DRAW_LOCK => {
                let window = read_window(data)?;
                if let Some(window) = window {
                    state.poke_draw_lock(window.0).map_err(werr)?;
                }
                crate::exception::write_no_exception(reply);
            }
            codes::UPDATE_REQUESTED_VISIBLE_TYPES => {
                let window = read_window(data)?;
                let types = data.read_int32().map_err(perr)?;
                if let Some(window) = window {
                    state.update_requested_visible_types(window.0, types).map_err(werr)?;
                }
            }
            codes::REPORT_SYSTEM_GESTURE_EXCLUSION_CHANGED => {
                let window = read_window(data)?;
                skip_typed_rect_list(data)?;
                if let Some(window) = window {
                    state.report_system_gesture_exclusion_changed(window.0).map_err(werr)?;
                }
            }
            codes::REPORT_KEEP_CLEAR_AREAS_CHANGED => {
                let _window = read_window(data)?;
                skip_typed_rect_list(data)?;
                skip_typed_rect_list(data)?;
            }
            codes::SET_ON_BACK_INVOKED_CALLBACK_INFO => {
                let window = read_window(data)?;
                // `OnBackInvokedCallbackInfo` typed object; the body is not
                // interpreted (nothing invokes back in this launch).
                if let Some(window) = window {
                    let _ = state.wm.windows.contains_key(&window.0);
                }
            }
            codes::CLEAR_TOUCHABLE_REGION => {
                let window = read_window(data)?;
                if let Some(window) = window {
                    let _ = state.wm.windows.contains_key(&window.0);
                }
                crate::exception::write_no_exception(reply);
            }
            codes::CANCEL_DRAW => {
                let _ = read_window(data)?;
                crate::exception::write_no_exception(reply);
                // There is no queued draw to cancel: the ACOS WMS never defers
                // the client's traversal.
                reply.write_bool(false);
            }
            codes::PERFORM_HAPTIC_FEEDBACK_ASYNC => {
                let _effect = data.read_int32().map_err(perr)?;
                let _always = data.read_bool().map_err(perr)?;
            }
            _ => return Err(Self::unsupported(code)),
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_image_codes_are_the_implemented_ones() {
        // Guards against a table transcribed from the tag AIDL: the image's
        // codes for the tail differ by one.
        assert_eq!(codes::ADD_TO_DISPLAY_AS_USER, 2);
        assert_eq!(codes::RELAYOUT, 5);
        assert_eq!(codes::FINISH_DRAWING, 9);
        assert_eq!(codes::GET_WINDOW_ID, 26);
        assert_eq!(codes::POKE_DRAW_LOCK, 27);
        assert_eq!(codes::UPDATE_REQUESTED_VISIBLE_TYPES, 32);
        assert_eq!(codes::SET_ON_BACK_INVOKED_CALLBACK_INFO, 39);
        assert_eq!(codes::CLEAR_TOUCHABLE_REGION, 40);
        assert_eq!(codes::CANCEL_DRAW, 41);
    }
}
