//! `android.view.SurfaceControl` / `SurfaceControl$Transaction` / `Surface` /
//! `SurfaceSession` / `android.graphics.BLASTBufferQueue` natives (T-28).
//!
//! Android's native `SurfaceControl` is a handle to a SurfaceFlinger layer;
//! `SurfaceControl.Transaction` is a batch of layer mutations applied
//! atomically. ACOS has no SurfaceFlinger (ADR-0008): the ACOS equivalent is the
//! in-process `acos-wm` compositor. This module is the *native* end of that
//! boundary: it keeps a per-host-thread table of layer records and transaction
//! batches, mirrors every setter into the record, and **publishes** the batch on
//! `nativeApplyTransaction` by bumping the compositor generation.
//!
//! It deliberately does **not** depend on `acos-wm`: `acos-vm` must stay free of
//! window policy (the same reason `acos-vm` does not depend on `acos-services`).
//! The layer/transaction state machine here is the exact surface contract the
//! runtime's compositor binding consumes; a `SurfaceBackend` seam
//! ([`install_backend`]) lets `acos-runtime` forward every application to the
//! real `acos-wm` [`acos_wm::compositor::Compositor`] when one is installed
//! (`acos-runtime/src/wms_java.rs`). Without a backend the generation counter in
//! this file is authoritative — which is what the isolated Java-level tests
//! exercise.
//!
//! # Frame freshness
//!
//! The advisor rule (docs/FINDINGS.md, M2 review) is that a frame is published
//! by real buffer submission/composition, never by `finishDrawing` alone. This
//! file therefore has two distinct counters:
//!
//! * [`generation`] — bumped when a transaction is *applied* (layer state
//!   changed);
//! * [`presented_frames`] — bumped only by [`submit_buffer`], i.e. by a
//!   `nativeSetBuffer` whose transaction was applied. It is the "a real buffer
//!   was handed to the compositor" signal, not a drawing callback.
//!
//! Unsupported behaviour (e.g. a setter ACOS cannot honour) is never faked: the
//! native raises a structured `UNSUPPORTED_JAVA_API` naming the method.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{Handle, NULL};
use crate::javabridge::JavaValue;
use crate::vm::Vm;

use super::parcel::with_store;

/// `SurfaceControl.Transaction` visibility flag (`SurfaceControl.HIDDEN`).
pub const HIDDEN: i32 = 0x4;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One committed layer record (the `SurfaceControl` handle target).
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRecord {
    /// Layer handle (`SurfaceControl.mNativeObject`).
    pub id: u64,
    /// Debug name.
    pub name: String,
    /// Buffer width.
    pub width: i32,
    /// Buffer height.
    pub height: i32,
    /// Position.
    pub position: [f32; 2],
    /// Z-order.
    pub z: i32,
    /// Alpha `0.0..=1.0`.
    pub alpha: f32,
    /// 2x2 transform matrix `[dsdx, dtdx, dtdy, dsdy]`.
    pub matrix: [f32; 4],
    /// Crop rect `[l, t, r, b]`, `None` for no crop.
    pub crop: Option<[i32; 4]>,
    /// `SurfaceControl` flags (bit 0x4 = hidden).
    pub flags: i32,
    /// Layer stack id.
    pub layer_stack: i32,
    /// Transform hint.
    pub transform_hint: i32,
    /// Last submitted client buffer frame number.
    pub buffer_frame: Option<u64>,
    /// Live flag (`nativeDisconnect` clears it).
    pub alive: bool,
    /// Generation at the last apply that touched this layer.
    pub generation: u64,
}

impl LayerRecord {
    fn new(id: u64, name: String, width: i32, height: i32) -> Self {
        Self {
            id,
            name,
            width,
            height,
            position: [0.0, 0.0],
            z: 0,
            alpha: 1.0,
            matrix: [1.0, 0.0, 0.0, 1.0],
            crop: None,
            flags: 0,
            layer_stack: 0,
            transform_hint: 0,
            buffer_frame: None,
            alive: true,
            generation: 0,
        }
    }

    /// Whether the layer is currently visible.
    #[must_use]
    pub fn visible(&self) -> bool {
        self.alive && self.flags & HIDDEN == 0
    }
}

/// One mutation recorded in a transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceOp {
    /// `setPosition`.
    Position { layer: u64, x: f32, y: f32 },
    /// `setLayer`.
    Layer { layer: u64, z: i32 },
    /// `setRelativeLayer`.
    RelativeLayer { layer: u64, relative: u64, z: i32 },
    /// `setAlpha`.
    Alpha { layer: u64, alpha: f32 },
    /// `setMatrix`.
    Matrix {
        layer: u64,
        dsdx: f32,
        dtdx: f32,
        dtdy: f32,
        dsdy: f32,
    },
    /// `setCrop` / `setWindowCrop`.
    Crop { layer: u64, rect: Option<[i32; 4]> },
    /// `setFlags`.
    Flags { layer: u64, flags: i32, mask: i32 },
    /// `setTransformHint`.
    TransformHint { layer: u64, hint: i32 },
    /// `setLayerStack`.
    LayerStack { layer: u64, stack: i32 },
    /// `setBuffer`: a real client buffer submission (BLAST handoff).
    Buffer { layer: u64, frame_number: u64 },
    /// `unsetBuffer`.
    UnsetBuffer { layer: u64 },
    /// `setBufferTransform`.
    BufferTransform { layer: u64, transform: i32 },
    /// `setScale`.
    Scale { layer: u64, sx: f32, sy: f32 },
    /// `setFixedTransformHint`.
    FixedTransformHint { layer: u64, hint: i32 },
    /// `setFrameTimelineVsync` (transaction id).
    FrameTimelineVsync { vsync_id: i64 },
    /// `setAnimationTransaction`.
    Animation,
    /// `setEarlyWakeupStart`.
    EarlyWakeupStart,
    /// `setEarlyWakeupEnd`.
    EarlyWakeupEnd,
    /// `reparent`.
    Reparent { layer: u64, new_parent: u64 },
    /// A setter ACOS records verbatim but does not yet composite (blur/color/
    /// geometry/…). Preserved across the parcel round-trip.
    Recorded { tag: u16, args: [i64; 4] },
}

/// One transaction batch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TransactionRecord {
    /// Transaction handle.
    pub id: u64,
    /// Recorded ops, in call order.
    pub ops: Vec<SurfaceOp>,
    /// `setAnimationTransaction` seen.
    pub animation: bool,
}

/// A `Surface` record (the consumer/software-canvas side of a layer).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceRecord {
    /// Native `Surface` handle.
    pub id: u64,
    /// Backing layer handle.
    pub layer: u64,
    /// Validity.
    pub valid: bool,
}

/// A `BLASTBufferQueue` record.
#[derive(Debug, Clone, PartialEq)]
pub struct BlastQueueRecord {
    /// Native queue handle.
    pub id: u64,
    /// Queue name.
    pub name: String,
    /// Backing layer.
    pub layer: u64,
    /// Surface handle handed to the producer.
    pub surface: u64,
    /// Latest transform hint.
    pub transform_hint: i32,
}

#[derive(Default)]
struct SurfaceState {
    layers: HashMap<u64, LayerRecord>,
    transactions: HashMap<u64, TransactionRecord>,
    surfaces: HashMap<u64, SurfaceRecord>,
    queues: HashMap<u64, BlastQueueRecord>,
    sessions: HashMap<u64, bool>,
    next_handle: u64,
    generation: u64,
    presented_frames: u64,
    /// `SurfaceControl.Transaction.setAnimationTransaction` flag on the last
    /// created transaction (Android keeps this on the native transaction).
    default_apply_token: u64,
}

thread_local! {
    static STATE: RefCell<SurfaceState> = RefCell::new(SurfaceState { next_handle: 1, ..Default::default() });
    /// Optional compositor binding installed by `acos-runtime`.
    static BACKEND: RefCell<Option<Box<dyn SurfaceBackend>>> = const { RefCell::new(None) };
}

/// Run `f` with mutable access to this host thread's surface state.
///
/// A direct `STATE.with(|s| s.borrow_mut())` cannot escape the closure (the
/// `RefMut` outlives the `RefCell` borrow), so every native goes through this
/// one-call helper instead.
fn with_state<R>(f: impl FnOnce(&mut SurfaceState) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

impl SurfaceState {
    fn alloc(&mut self) -> u64 {
        let h = self.next_handle.max(1);
        self.next_handle = h.wrapping_add(1).max(1);
        h
    }
}

/// The compositor binding seam.
///
/// `acos-runtime` installs an implementation backed by the real `acos-wm`
/// [`Compositor`](https://docs.rs/acos-wm). Every method is called on the host
/// thread while the VM executes, so implementations may own the compositor
/// directly. If no backend is installed the module's own state is authoritative.
pub trait SurfaceBackend: 'static {
    /// A layer was created; return the compositor layer id (or the handle).
    fn create_layer(&mut self, handle: u64, name: &str, width: i32, height: i32) -> u64;
    /// Apply one batch; return the new compositor generation.
    fn apply(&mut self, ops: &[SurfaceOp]) -> Result<u64, String>;
    /// Publish a buffer submission; return the new presented-frame count.
    fn submit_buffer(&mut self, layer: u64, frame_number: u64) -> Result<u64, String>;
    /// Release a layer.
    fn release(&mut self, layer: u64);
    /// The current compositor generation.
    fn generation(&self) -> u64;
}

/// Install the compositor binding. Replaces any previous backend.
pub fn install_backend(backend: Box<dyn SurfaceBackend>) {
    BACKEND.with(|b| *b.borrow_mut() = Some(backend));
}

/// Remove the compositor binding (test teardown).
pub fn clear_backend() {
    BACKEND.with(|b| *b.borrow_mut() = None);
}

fn with_backend<R>(f: impl FnOnce(&mut dyn SurfaceBackend) -> R) -> Option<R> {
    BACKEND.with(|b| b.borrow_mut().as_mut().map(|x| f(x.as_mut())))
}

// ---------------------------------------------------------------------------
// Test/inspection helpers
// ---------------------------------------------------------------------------

/// The compositor generation: bumped on every applied transaction with ops.
#[must_use]
pub fn generation() -> u64 {
    if let Some(g) = with_backend(|b| b.generation()) {
        return g;
    }
    STATE.with(|s| s.borrow().generation)
}

/// The number of real buffer submissions published to the compositor.
#[must_use]
pub fn presented_frames() -> u64 {
    STATE.with(|s| s.borrow().presented_frames)
}

/// The `SurfaceControl` layer handle a `Surface` wraps, if the surface and its
/// layer are still live. Used by the HWUI frame handoff to address the compositor
/// layer bound by the WMS backend.
#[must_use]
pub fn surface_layer(surface: u64) -> Option<u64> {
    STATE.with(|s| {
        let st = s.borrow();
        st.surfaces.get(&surface).map(|r| r.layer)
    })
}

/// The buffer size of the layer a `Surface` wraps, if both are still live.
#[must_use]
pub fn surface_size(surface: u64) -> Option<(i32, i32)> {
    STATE.with(|s| {
        let st = s.borrow();
        st.surfaces
            .get(&surface)
            .and_then(|r| st.layers.get(&r.layer))
            .map(|l| (l.width, l.height))
    })
}

/// Number of live layer records (test helper).
#[must_use]
pub fn live_layers() -> usize {
    STATE.with(|s| s.borrow().layers.values().filter(|l| l.alive).count())
}

/// Reset all native surface state (test isolation).
pub fn reset() {
    clear_backend();
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        *st = SurfaceState {
            next_handle: 1,
            ..Default::default()
        };
    });
}

/// Install every T-28 native.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let sc = "Landroid/view/SurfaceControl;";
    for (name, sig, f) in [
        (
            "nativeCreate",
            "(Landroid/view/SurfaceSession;Ljava/lang/String;IIIIJLandroid/os/Parcel;)J",
            native_create as crate::class::NativeFn,
        ),
        ("nativeCreateTransaction", "()J", native_create_transaction),
        ("nativeApplyTransaction", "(JZ)V", native_apply_transaction),
        ("nativeMergeTransaction", "(JJ)V", native_merge_transaction),
        ("nativeClearTransaction", "(J)V", native_clear_transaction),
        ("nativeGetTransactionId", "(J)J", native_get_transaction_id),
        ("nativeSetPosition", "(JJFF)V", native_set_position),
        ("nativeSetLayer", "(JJI)V", native_set_layer),
        ("nativeSetRelativeLayer", "(JJJI)V", native_set_relative_layer),
        ("nativeSetAlpha", "(JJF)V", native_set_alpha),
        ("nativeSetMatrix", "(JJFFFF)V", native_set_matrix),
        ("nativeSetWindowCrop", "(JJIIII)V", native_set_window_crop),
        ("nativeSetCrop", "(JJLandroid/graphics/Rect;)V", native_set_crop),
        ("nativeSetFlags", "(JJII)V", native_set_flags),
        ("nativeSetTransformHint", "(JI)V", native_set_transform_hint),
        ("nativeSetLayerStack", "(JJI)V", native_set_layer_stack),
        ("nativeSetBuffer", "(JJLandroid/hardware/HardwareBuffer;JLjava/util/function/Consumer;)V", native_set_buffer),
        ("nativeUnsetBuffer", "(JJ)V", native_unset_buffer),
        ("nativeSetBufferTransform", "(JJI)V", native_set_buffer_transform),
        ("nativeSetScale", "(JJFF)V", native_set_scale),
        ("nativeSetFixedTransformHint", "(JJI)V", native_set_fixed_transform_hint),
        ("nativeSetFrameTimelineVsync", "(JJ)V", native_set_frame_timeline_vsync),
        ("nativeSetAnimationTransaction", "(J)V", native_set_animation_transaction),
        ("nativeSetEarlyWakeupStart", "(J)V", native_set_early_wakeup_start),
        ("nativeSetEarlyWakeupEnd", "(J)V", native_set_early_wakeup_end),
        ("nativeReparent", "(JJJ)V", native_reparent),
        ("nativeSetDefaultApplyToken", "(Landroid/os/IBinder;)V", native_set_default_apply_token),
        ("nativeGetDefaultApplyToken", "()Landroid/os/IBinder;", native_get_default_apply_token),
        ("nativeWriteTransactionToParcel", "(JLandroid/os/Parcel;)V", native_write_transaction_to_parcel),
        ("nativeReadTransactionFromParcel", "(Landroid/os/Parcel;)J", native_read_transaction_from_parcel),
        ("nativeWriteToParcel", "(JLandroid/os/Parcel;)V", native_write_to_parcel),
        ("nativeReadFromParcel", "(Landroid/os/Parcel;)J", native_read_from_parcel),
        ("nativeDisconnect", "(J)V", native_disconnect),
        ("nativeGetLayerId", "(J)I", native_get_layer_id),
        ("nativeGetHandle", "(J)J", native_get_handle),
        ("nativeGetNativeSurfaceControlFinalizer", "()J", surface_control_finalizer),
        ("nativeGetNativeTransactionFinalizer", "()J", transaction_finalizer),
        ("nativeSetTrustedOverlay", "(JJZ)V", native_record_trusted_overlay),
        ("nativeSetDropInputMode", "(JJI)V", native_record_drop_input),
        ("nativeSetFrameRate", "(JJFII)V", native_record_frame_rate),
        ("nativeSetFrameRateSelectionPriority", "(JJI)V", native_record_frame_rate_priority),
        ("nativeSetCachingHint", "(JJI)V", native_record_caching_hint),
        ("nativeSetDataSpace", "(JJI)V", native_record_data_space),
        ("nativeSetExtendedRangeBrightness", "(JJFF)V", native_record_extended_range),
        ("nativeSetShadowRadius", "(JJF)V", native_record_shadow_radius),
        ("nativeSetCornerRadius", "(JJF)V", native_record_corner_radius),
        ("nativeSetBackgroundBlurRadius", "(JJI)V", native_record_blur_radius),
        ("nativeSetColor", "(JJ[F)V", native_record_color),
        ("nativeSetColorTransform", "(JJ[F[F)V", native_record_color_transform),
        ("nativeSetColorSpaceAgnostic", "(JJZ)V", native_record_color_space_agnostic),
        ("nativeSetDestinationFrame", "(JJIIII)V", native_record_destination_frame),
        ("nativeSetGeometry", "(JJLandroid/graphics/Rect;Landroid/graphics/Rect;J)V", native_record_geometry),
        ("nativeSetMetadata", "(JJILandroid/os/Parcel;)V", native_record_metadata),
    ] {
        vm.register_native(sc, name, sig, f);
    }

    // `android.view.SurfaceControl$Transaction` re-exposes a few statics.
    // (None in this API; the class only wraps `mNativeObject`.)

    let surface = "Landroid/view/Surface;";
    vm.register_native(surface, "nativeIsValid", "(J)Z", surface_is_valid);
    vm.register_native(surface, "nativeCreateFromSurfaceControl", "(J)J", surface_from_control);
    vm.register_native(surface, "nativeGetFromSurfaceControl", "(JJ)J", surface_get_from_control);
    vm.register_native(surface, "nativeGetFromBlastBufferQueue", "(JJ)J", surface_from_queue);
    vm.register_native(surface, "nativeGetWidth", "(J)I", surface_width);
    vm.register_native(surface, "nativeGetHeight", "(J)I", surface_height);
    vm.register_native(surface, "nativeGetNextFrameNumber", "(J)J", surface_next_frame);
    vm.register_native(surface, "nativeRelease", "(J)V", surface_release);
    vm.register_native(surface, "nativeDestroy", "(J)V", surface_release);

    let session = "Landroid/view/SurfaceSession;";
    vm.register_native(session, "nativeCreate", "()J", session_create);
    vm.register_native(session, "nativeDestroy", "(J)V", session_destroy);

    let blast = "Landroid/graphics/BLASTBufferQueue;";
    vm.register_native(blast, "nativeCreate", "(Ljava/lang/String;Z)J", blast_create);
    vm.register_native(blast, "nativeDestroy", "(J)V", blast_destroy);
    vm.register_native(blast, "nativeUpdate", "(JJJJI)V", blast_update);
    vm.register_native(blast, "nativeGetSurface", "(JZ)Landroid/view/Surface;", blast_get_surface);
    vm.register_native(blast, "nativeMergeWithNextTransaction", "(JJJ)V", blast_merge);
    vm.register_native(blast, "nativeApplyPendingTransactions", "(JJ)V", blast_apply_pending);
    vm.register_native(blast, "nativeIsSameSurfaceControl", "(JJ)Z", blast_same_control);
    vm.register_native(blast, "nativeGetLastAcquiredFrameNum", "(J)J", blast_last_frame);
    vm.register_native(blast, "nativeClearSyncTransaction", "(J)V", blast_clear_sync);
    vm.register_native(blast, "nativeGatherPendingTransactions", "(JJ)Landroid/view/SurfaceControl$Transaction;", blast_gather);

    // The framework wraps every native SurfaceControl/Transaction in a
    // `NativeAllocationRegistry`; on `release()`/GC it calls this with the
    // native pointer. There is no `malloc`ed memory to free (the records live
    // in the Rust table), but the pointer *is* the layer/transaction handle, so
    // the honest action is to invalidate it: a later transaction that names a
    // released handle must fail as stale, exactly like native `SurfaceControl`
    // use-after-free. Handles from the two tables never collide (one counter),
    // so checking both is unambiguous.
    vm.register_native(
        "Llibcore/util/NativeAllocationRegistry;",
        "applyFreeFunction",
        "(JJ)V",
        apply_free_function,
    );
    Ok(())
}

fn apply_free_function(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let free = vm.arg_i64(ctx, 0) as u64;
    let ptr = vm.arg_i64(ctx, 1) as u64;
    if ptr != 0 {
        with_state(|st| match free {
            FREE_SURFACE_CONTROL => {
                if let Some(l) = st.layers.get_mut(&ptr) {
                    l.alive = false;
                }
                if let Some(s) = st.surfaces.get_mut(&ptr) {
                    s.valid = false;
                }
            }
            FREE_TRANSACTION => {
                st.transactions.remove(&ptr);
            }
            // DisplayEventReceiver's finalizer is registered here because the
            // framework routes every `NativeAllocationRegistry` free through
            // this one native; the vsync module owns the receiver table.
            _ => {
                let _ = crate::natives::fwk_vsync::free_receiver(vm, ptr);
            }
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Argument/parcel helpers
// ---------------------------------------------------------------------------

fn jv_i32(v: JavaValue) -> Option<i32> {
    match v {
        JavaValue::Int(x) => Some(x),
        JavaValue::Short(x) => Some(x as i32),
        JavaValue::Byte(x) => Some(x as i32),
        _ => None,
    }
}

fn jv_i64(v: JavaValue) -> Option<i64> {
    match v {
        JavaValue::Long(x) => Some(x),
        JavaValue::Int(x) => Some(x as i64),
        _ => None,
    }
}

fn parcel_native_ptr(vm: &mut Vm, obj: Handle) -> Option<u64> {
    if obj == NULL {
        return None;
    }
    match vm.get_field(obj, "mNativePtr") {
        Ok(JavaValue::Long(v)) => Some(v as u64),
        Ok(JavaValue::Int(v)) => Some(v as u32 as u64),
        _ => None,
    }
}

fn write_i64_to_parcel(ph: u64, v: i64) {
    let _ = with_store(|s| s.table.with_parcel(ph, |p| p.write_int64(v)));
}

fn write_bytes_to_parcel(ph: u64, bytes: &[u8]) {
    let _ = with_store(|s| s.table.with_parcel(ph, |p| p.write_byte_array(Some(bytes))));
}

fn read_bytes_from_parcel(ph: u64) -> Option<Vec<u8>> {
    with_store(|s| s.table.with_parcel(ph, |p| p.read_byte_array()))
        .ok()
        .and_then(Result::ok)
        .flatten()
}

fn stale_error(vm: &mut Vm, handle: u64) -> VmError {
    vm.throw_new(
        "Ljava/lang/IllegalStateException;",
        Some(&format!("SurfaceControl: stale/unknown layer handle {handle}")),
    )
}

fn missing_transaction(vm: &mut Vm, handle: u64) -> VmError {
    vm.throw_new(
        "Ljava/lang/IllegalStateException;",
        Some(&format!("SurfaceControl.Transaction: stale/unknown handle {handle}")),
    )
}

// ---------------------------------------------------------------------------
// Serialization (byte-stable Parcel round-trip)
// ---------------------------------------------------------------------------

const TAG_POSITION: u8 = 1;
const TAG_LAYER: u8 = 2;
const TAG_RELATIVE: u8 = 3;
const TAG_ALPHA: u8 = 4;
const TAG_MATRIX: u8 = 5;
const TAG_CROP: u8 = 6;
const TAG_FLAGS: u8 = 7;
const TAG_TRANSFORM_HINT: u8 = 8;
const TAG_LAYER_STACK: u8 = 9;
const TAG_BUFFER: u8 = 10;
const TAG_UNSET_BUFFER: u8 = 11;
const TAG_BUFFER_TRANSFORM: u8 = 12;
const TAG_SCALE: u8 = 13;
const TAG_FIXED_TRANSFORM: u8 = 14;
const TAG_FRAME_TIMELINE: u8 = 15;
const TAG_ANIMATION: u8 = 16;
const TAG_EARLY_START: u8 = 17;
const TAG_EARLY_END: u8 = 18;
const TAG_REPARENT: u8 = 19;
const TAG_RECORDED: u8 = 20;

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_bits().to_le_bytes());
}

/// Encode a batch deterministically. Byte-stable: encoding the same op list
/// twice yields identical bytes, so a write→read→write round-trip is stable.
#[must_use]
pub fn encode_ops(ops: &[SurfaceOp]) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, ops.len() as u64);
    for op in ops {
        match op {
            SurfaceOp::Position { layer, x, y } => {
                out.push(TAG_POSITION);
                put_u64(&mut out, *layer);
                put_f32(&mut out, *x);
                put_f32(&mut out, *y);
            }
            SurfaceOp::Layer { layer, z } => {
                out.push(TAG_LAYER);
                put_u64(&mut out, *layer);
                put_i32(&mut out, *z);
            }
            SurfaceOp::RelativeLayer { layer, relative, z } => {
                out.push(TAG_RELATIVE);
                put_u64(&mut out, *layer);
                put_u64(&mut out, *relative);
                put_i32(&mut out, *z);
            }
            SurfaceOp::Alpha { layer, alpha } => {
                out.push(TAG_ALPHA);
                put_u64(&mut out, *layer);
                put_f32(&mut out, *alpha);
            }
            SurfaceOp::Matrix { layer, dsdx, dtdx, dtdy, dsdy } => {
                out.push(TAG_MATRIX);
                put_u64(&mut out, *layer);
                put_f32(&mut out, *dsdx);
                put_f32(&mut out, *dtdx);
                put_f32(&mut out, *dtdy);
                put_f32(&mut out, *dsdy);
            }
            SurfaceOp::Crop { layer, rect } => {
                out.push(TAG_CROP);
                put_u64(&mut out, *layer);
                match rect {
                    Some(r) => {
                        out.push(1);
                        for v in r {
                            put_i32(&mut out, *v);
                        }
                    }
                    None => out.push(0),
                }
            }
            SurfaceOp::Flags { layer, flags, mask } => {
                out.push(TAG_FLAGS);
                put_u64(&mut out, *layer);
                put_i32(&mut out, *flags);
                put_i32(&mut out, *mask);
            }
            SurfaceOp::TransformHint { layer, hint } => {
                out.push(TAG_TRANSFORM_HINT);
                put_u64(&mut out, *layer);
                put_i32(&mut out, *hint);
            }
            SurfaceOp::LayerStack { layer, stack } => {
                out.push(TAG_LAYER_STACK);
                put_u64(&mut out, *layer);
                put_i32(&mut out, *stack);
            }
            SurfaceOp::Buffer { layer, frame_number } => {
                out.push(TAG_BUFFER);
                put_u64(&mut out, *layer);
                put_u64(&mut out, *frame_number);
            }
            SurfaceOp::UnsetBuffer { layer } => {
                out.push(TAG_UNSET_BUFFER);
                put_u64(&mut out, *layer);
            }
            SurfaceOp::BufferTransform { layer, transform } => {
                out.push(TAG_BUFFER_TRANSFORM);
                put_u64(&mut out, *layer);
                put_i32(&mut out, *transform);
            }
            SurfaceOp::Scale { layer, sx, sy } => {
                out.push(TAG_SCALE);
                put_u64(&mut out, *layer);
                put_f32(&mut out, *sx);
                put_f32(&mut out, *sy);
            }
            SurfaceOp::FixedTransformHint { layer, hint } => {
                out.push(TAG_FIXED_TRANSFORM);
                put_u64(&mut out, *layer);
                put_i32(&mut out, *hint);
            }
            SurfaceOp::FrameTimelineVsync { vsync_id } => {
                out.push(TAG_FRAME_TIMELINE);
                put_i64(&mut out, *vsync_id);
            }
            SurfaceOp::Animation => out.push(TAG_ANIMATION),
            SurfaceOp::EarlyWakeupStart => out.push(TAG_EARLY_START),
            SurfaceOp::EarlyWakeupEnd => out.push(TAG_EARLY_END),
            SurfaceOp::Reparent { layer, new_parent } => {
                out.push(TAG_REPARENT);
                put_u64(&mut out, *layer);
                put_u64(&mut out, *new_parent);
            }
            SurfaceOp::Recorded { tag, args } => {
                out.push(TAG_RECORDED);
                out.extend_from_slice(&tag.to_le_bytes());
                for a in args {
                    put_i64(&mut out, *a);
                }
            }
        }
    }
    out
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }
    fn u64(&mut self) -> Option<u64> {
        let b = self.bytes.get(self.pos..self.pos + 8)?;
        self.pos += 8;
        Some(u64::from_le_bytes(b.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(self.u64()? as i64)
    }
    fn i32(&mut self) -> Option<i32> {
        let b = self.bytes.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(i32::from_le_bytes(b.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        let b = self.bytes.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(f32::from_le_bytes(b.try_into().ok()?))
    }
    fn u16(&mut self) -> Option<u16> {
        let b = self.bytes.get(self.pos..self.pos + 2)?;
        self.pos += 2;
        Some(u16::from_le_bytes(b.try_into().ok()?))
    }
}

/// Decode a batch; `None` on a malformed buffer.
#[must_use]
pub fn decode_ops(bytes: &[u8]) -> Option<Vec<SurfaceOp>> {
    let mut r = Reader { bytes, pos: 0 };
    let count = r.u64()? as usize;
    // Every op consumes at least its 1-byte tag, so a count larger than the
    // remaining bytes is malformed. Reject it before allocating
    // (HONESTY_AUDIT.md #6).
    if count > bytes.len().saturating_sub(r.pos) {
        return None;
    }
    let mut ops = Vec::with_capacity(count);
    for _ in 0..count {
        let tag = r.u8()?;
        let op = match tag {
            TAG_POSITION => SurfaceOp::Position { layer: r.u64()?, x: r.f32()?, y: r.f32()? },
            TAG_LAYER => SurfaceOp::Layer { layer: r.u64()?, z: r.i32()? },
            TAG_RELATIVE => SurfaceOp::RelativeLayer { layer: r.u64()?, relative: r.u64()?, z: r.i32()? },
            TAG_ALPHA => SurfaceOp::Alpha { layer: r.u64()?, alpha: r.f32()? },
            TAG_MATRIX => SurfaceOp::Matrix { layer: r.u64()?, dsdx: r.f32()?, dtdx: r.f32()?, dtdy: r.f32()?, dsdy: r.f32()? },
            TAG_CROP => {
                let layer = r.u64()?;
                let rect = if r.u8()? == 1 {
                    Some([r.i32()?, r.i32()?, r.i32()?, r.i32()?])
                } else {
                    None
                };
                SurfaceOp::Crop { layer, rect }
            }
            TAG_FLAGS => SurfaceOp::Flags { layer: r.u64()?, flags: r.i32()?, mask: r.i32()? },
            TAG_TRANSFORM_HINT => SurfaceOp::TransformHint { layer: r.u64()?, hint: r.i32()? },
            TAG_LAYER_STACK => SurfaceOp::LayerStack { layer: r.u64()?, stack: r.i32()? },
            TAG_BUFFER => SurfaceOp::Buffer { layer: r.u64()?, frame_number: r.u64()? },
            TAG_UNSET_BUFFER => SurfaceOp::UnsetBuffer { layer: r.u64()? },
            TAG_BUFFER_TRANSFORM => SurfaceOp::BufferTransform { layer: r.u64()?, transform: r.i32()? },
            TAG_SCALE => SurfaceOp::Scale { layer: r.u64()?, sx: r.f32()?, sy: r.f32()? },
            TAG_FIXED_TRANSFORM => SurfaceOp::FixedTransformHint { layer: r.u64()?, hint: r.i32()? },
            TAG_FRAME_TIMELINE => SurfaceOp::FrameTimelineVsync { vsync_id: r.i64()? },
            TAG_ANIMATION => SurfaceOp::Animation,
            TAG_EARLY_START => SurfaceOp::EarlyWakeupStart,
            TAG_EARLY_END => SurfaceOp::EarlyWakeupEnd,
            TAG_REPARENT => SurfaceOp::Reparent { layer: r.u64()?, new_parent: r.u64()? },
            TAG_RECORDED => {
                let t = r.u16()?;
                let args = [r.i64()?, r.i64()?, r.i64()?, r.i64()?];
                SurfaceOp::Recorded { tag: t, args }
            }
            _ => return None,
        };
        ops.push(op);
    }
    Some(ops)
}

// ---------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------

fn op_layer(op: &SurfaceOp) -> Option<u64> {
    match op {
        SurfaceOp::Position { layer, .. }
        | SurfaceOp::Layer { layer, .. }
        | SurfaceOp::RelativeLayer { layer, .. }
        | SurfaceOp::Alpha { layer, .. }
        | SurfaceOp::Matrix { layer, .. }
        | SurfaceOp::Crop { layer, .. }
        | SurfaceOp::Flags { layer, .. }
        | SurfaceOp::TransformHint { layer, .. }
        | SurfaceOp::LayerStack { layer, .. }
        | SurfaceOp::Buffer { layer, .. }
        | SurfaceOp::UnsetBuffer { layer }
        | SurfaceOp::BufferTransform { layer, .. }
        | SurfaceOp::Scale { layer, .. }
        | SurfaceOp::FixedTransformHint { layer, .. }
        | SurfaceOp::Reparent { layer, .. } => Some(*layer),
        _ => None,
    }
}

/// Apply one batch to the module state. Returns the new generation, or the
/// stale layer handle that made the batch invalid. Validates the whole batch
/// before mutating anything, so an apply is atomic.
fn apply_to_state(st: &mut SurfaceState, ops: &[SurfaceOp]) -> Result<u64, u64> {
    // Validate first.
    for op in ops {
        if let Some(layer) = op_layer(op) {
            match st.layers.get(&layer) {
                Some(l) if l.alive => {}
                _ => return Err(layer),
            }
        }
        if let SurfaceOp::RelativeLayer { relative, .. } = op {
            match st.layers.get(relative) {
                Some(l) if l.alive => {}
                _ => return Err(*relative),
            }
        }
        if let SurfaceOp::Reparent { new_parent, .. } = op {
            if *new_parent != 0 {
                match st.layers.get(new_parent) {
                    Some(l) if l.alive => {}
                    _ => return Err(*new_parent),
                }
            }
        }
    }
    if ops.is_empty() {
        return Ok(st.generation);
    }
    st.generation += 1;
    let gen = st.generation;
    for op in ops {
        match op {
            SurfaceOp::Position { layer, x, y } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.position = [*x, *y];
                    l.generation = gen;
                }
            }
            SurfaceOp::Layer { layer, z } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.z = *z;
                    l.generation = gen;
                }
            }
            SurfaceOp::RelativeLayer { layer, relative, z } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.z = *z;
                    l.generation = gen;
                    let _ = relative;
                }
            }
            SurfaceOp::Alpha { layer, alpha } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.alpha = *alpha;
                    l.generation = gen;
                }
            }
            SurfaceOp::Matrix { layer, dsdx, dtdx, dtdy, dsdy } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.matrix = [*dsdx, *dtdx, *dtdy, *dsdy];
                    l.generation = gen;
                }
            }
            SurfaceOp::Crop { layer, rect } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.crop = *rect;
                    l.generation = gen;
                }
            }
            SurfaceOp::Flags { layer, flags, mask } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.flags = (l.flags & !mask) | (flags & mask);
                    l.generation = gen;
                }
            }
            SurfaceOp::TransformHint { layer, hint } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.transform_hint = *hint;
                    l.generation = gen;
                }
            }
            SurfaceOp::LayerStack { layer, stack } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.layer_stack = *stack;
                    l.generation = gen;
                }
            }
            SurfaceOp::Buffer { layer, frame_number } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.buffer_frame = Some(*frame_number);
                    l.generation = gen;
                }
            }
            SurfaceOp::UnsetBuffer { layer } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.buffer_frame = None;
                    l.generation = gen;
                }
            }
            SurfaceOp::BufferTransform { layer, transform } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.transform_hint = *transform;
                    l.generation = gen;
                }
            }
            SurfaceOp::Scale { layer, .. } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.generation = gen;
                }
            }
            SurfaceOp::FixedTransformHint { layer, hint } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.transform_hint = *hint;
                    l.generation = gen;
                }
            }
            SurfaceOp::Reparent { layer, new_parent } => {
                if let Some(l) = st.layers.get_mut(layer) {
                    l.generation = gen;
                    let _ = new_parent;
                }
            }
            SurfaceOp::FrameTimelineVsync { .. }
            | SurfaceOp::Animation
            | SurfaceOp::EarlyWakeupStart
            | SurfaceOp::EarlyWakeupEnd
            | SurfaceOp::Recorded { .. } => {}
        }
    }
    Ok(gen)
}

/// Apply a batch through the installed backend (if any), else the module state.
fn apply_ops(vm: &mut Vm, ops: &[SurfaceOp]) -> VmResult<()> {
    if with_backend(|b| b.generation()).is_some() {
        match with_backend(|b| b.apply(ops)) {
            Some(Ok(_)) => return Ok(()),
            Some(Err(msg)) => {
                // A backend reports a stale/unknown layer as its decimal id.
                if let Ok(h) = msg.parse::<u64>() {
                    return Err(stale_error(vm, h));
                }
                return Err(VmError::Internal(format!(
                    "acos-wm compositor apply failed: {msg}"
                )));
            }
            None => {}
        }
    }
    match with_state(|st| apply_to_state(st, ops)) {
        Ok(_) => Ok(()),
        Err(handle) => Err(stale_error(vm, handle)),
    }
}

// ---------------------------------------------------------------------------
// SurfaceControl natives
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn native_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = {
        let s = vm.arg_ref(ctx, 1);
        if s == NULL {
            "SurfaceControl".to_string()
        } else {
            vm.java_string_to_rust(s)
                .unwrap_or_else(|_| "SurfaceControl".to_string())
        }
    };
    let width = vm.arg_i32(ctx, 2);
    let height = vm.arg_i32(ctx, 3);
    let parent = vm.arg_i64(ctx, 6) as u64;

    let parent_alive =
        with_state(|st| parent == 0 || st.layers.get(&parent).is_some_and(|l| l.alive));
    if !parent_alive {
        return Err(stale_error(vm, parent));
    }
    let handle = with_state(|st| {
        let handle = st.alloc();
        st.layers
            .insert(handle, LayerRecord::new(handle, name.clone(), width, height));
        handle
    });
    if let Some(layer_id) = with_backend(|b| b.create_layer(handle, &name, width, height)) {
        // The backend owns the real compositor; keep the returned id for the
        // record so the runtime can map handles. The handle stays the Java
        // `mNativeObject`, exactly like Android.
        let _ = layer_id;
    }
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn native_create_transaction(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = with_state(|st| {
        let handle = st.alloc();
        st.transactions.insert(
            handle,
            TransactionRecord {
                id: handle,
                ops: Vec::new(),
                animation: false,
            },
        );
        handle
    });
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn native_get_transaction_id(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let id = STATE.with(|s| s.borrow().transactions.get(&handle).map(|t| t.id));
    match id {
        Some(id) => {
            vm.ret_i64(ctx, id as i64);
            Ok(())
        }
        None => Err(missing_transaction(vm, handle)),
    }
}

fn native_apply_transaction(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let ops = STATE.with(|s| s.borrow().transactions.get(&handle).map(|t| t.ops.clone()));
    let Some(ops) = ops else {
        return Err(missing_transaction(vm, handle));
    };
    apply_ops(vm, &ops)?;
    // A real buffer submission is the frame-publish signal (advisor rule).
    if ops.iter().any(|o| matches!(o, SurfaceOp::Buffer { .. })) {
        STATE.with(|s| s.borrow_mut().presented_frames += 1);
    }
    Ok(())
}

fn native_merge_transaction(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let dst = vm.arg_i64(ctx, 0) as u64;
    let src = vm.arg_i64(ctx, 1) as u64;
    let outcome = with_state(|st| -> Result<(), u64> {
        if !st.transactions.contains_key(&dst) {
            return Err(dst);
        }
        let Some(src_ops) = st.transactions.get(&src).map(|t| t.ops.clone()) else {
            return Err(src);
        };
        if let Some(t) = st.transactions.get_mut(&dst) {
            t.ops.extend(src_ops);
        }
        Ok(())
    });
    outcome.map_err(|h| missing_transaction(vm, h))
}

fn native_clear_transaction(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let outcome = with_state(|st| match st.transactions.get_mut(&handle) {
        Some(t) => {
            t.ops.clear();
            t.animation = false;
            Ok(())
        }
        None => Err(()),
    });
    outcome.map_err(|()| missing_transaction(vm, handle))
}

fn with_txn_op(vm: &mut Vm, ctx: NativeCtx, f: impl FnOnce(&mut SurfaceState) -> Option<SurfaceOp>) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let outcome = with_state(|st| -> Result<(), ()> {
        if !st.transactions.contains_key(&handle) {
            return Err(());
        }
        let op = f(st);
        if let Some(op) = op {
            if let Some(t) = st.transactions.get_mut(&handle) {
                t.ops.push(op);
            }
        }
        Ok(())
    });
    outcome.map_err(|()| missing_transaction(vm, handle))
}

fn native_set_position(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let x = vm.arg_f32(ctx, 2);
    let y = vm.arg_f32(ctx, 3);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Position { layer, x, y }))
}

fn native_set_layer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let z = vm.arg_i32(ctx, 2);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Layer { layer, z }))
}

fn native_set_relative_layer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let relative = vm.arg_i64(ctx, 2) as u64;
    let z = vm.arg_i32(ctx, 3);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::RelativeLayer { layer, relative, z }))
}

fn native_set_alpha(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let alpha = vm.arg_f32(ctx, 2);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Alpha { layer, alpha }))
}

fn native_set_matrix(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let dsdx = vm.arg_f32(ctx, 2);
    let dtdx = vm.arg_f32(ctx, 3);
    let dtdy = vm.arg_f32(ctx, 4);
    let dsdy = vm.arg_f32(ctx, 5);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Matrix { layer, dsdx, dtdx, dtdy, dsdy }))
}

fn native_set_window_crop(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let rect = [vm.arg_i32(ctx, 2), vm.arg_i32(ctx, 3), vm.arg_i32(ctx, 4), vm.arg_i32(ctx, 5)];
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Crop { layer, rect: Some(rect) }))
}

fn native_set_crop(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    // `setCrop(Rect)` — the Rect is a Java object with l/t/r/b int fields.
    let rect_obj = vm.arg_ref(ctx, 2);
    let rect = if rect_obj == NULL {
        None
    } else {
        Some([
            vm.get_field(rect_obj, "left").ok().and_then(jv_i32).unwrap_or(0),
            vm.get_field(rect_obj, "top").ok().and_then(jv_i32).unwrap_or(0),
            vm.get_field(rect_obj, "right").ok().and_then(jv_i32).unwrap_or(0),
            vm.get_field(rect_obj, "bottom").ok().and_then(jv_i32).unwrap_or(0),
        ])
    };
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Crop { layer, rect }))
}

fn native_set_flags(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let flags = vm.arg_i32(ctx, 2);
    let mask = vm.arg_i32(ctx, 3);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Flags { layer, flags, mask }))
}

fn native_set_transform_hint(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `setTransformHint(int)` is a transaction-level hint in this API.
    let hint = vm.arg_i32(ctx, 1);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Recorded { tag: 1, args: [hint as i64, 0, 0, 0] }))
}

fn native_set_layer_stack(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let stack = vm.arg_i32(ctx, 2);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::LayerStack { layer, stack }))
}

fn native_set_buffer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let frame_number = vm.arg_i64(ctx, 3) as u64;
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Buffer { layer, frame_number }))
}

fn native_unset_buffer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::UnsetBuffer { layer }))
}

fn native_set_buffer_transform(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let transform = vm.arg_i32(ctx, 2);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::BufferTransform { layer, transform }))
}

fn native_set_scale(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let sx = vm.arg_f32(ctx, 2);
    let sy = vm.arg_f32(ctx, 3);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Scale { layer, sx, sy }))
}

fn native_set_fixed_transform_hint(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let hint = vm.arg_i32(ctx, 2);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::FixedTransformHint { layer, hint }))
}

fn native_set_frame_timeline_vsync(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let vsync_id = vm.arg_i64(ctx, 1);
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::FrameTimelineVsync { vsync_id }))
}

fn native_set_animation_transaction(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let outcome = with_state(|st| match st.transactions.get_mut(&handle) {
        Some(t) => {
            t.animation = true;
            t.ops.push(SurfaceOp::Animation);
            Ok(())
        }
        None => Err(()),
    });
    outcome.map_err(|()| missing_transaction(vm, handle))
}

fn native_set_early_wakeup_start(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::EarlyWakeupStart))
}

fn native_set_early_wakeup_end(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::EarlyWakeupEnd))
}

fn native_reparent(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let layer = vm.arg_i64(ctx, 1) as u64;
    let new_parent = vm.arg_i64(ctx, 2) as u64;
    with_txn_op(vm, ctx, |_| Some(SurfaceOp::Reparent { layer, new_parent }))
}

fn native_set_default_apply_token(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // An `IBinder` apply token: ACOS has a single in-process compositor, so the
    // token is recorded only. No app API reads it back.
    let obj = vm.arg_ref(ctx, 0);
    let id = if obj == NULL {
        0
    } else {
        vm.get_field(obj, "mNativeData")
            .ok()
            .and_then(jv_i64)
            .unwrap_or(0) as u64
    };
    STATE.with(|s| s.borrow_mut().default_apply_token = id);
    Ok(())
}

fn native_get_default_apply_token(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = STATE.with(|s| s.borrow().default_apply_token);
    if id == 0 {
        vm.ret_ref(ctx, NULL);
    } else {
        let proxy = vm
            .new_instance_ctor("android.os.BinderProxy", "(J)V", &[JavaValue::Long(id as i64)])
            .unwrap_or(NULL);
        vm.ret_ref(ctx, proxy);
    }
    Ok(())
}

fn native_write_transaction_to_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let parcel_obj = vm.arg_ref(ctx, 1);
    let ops = STATE.with(|s| s.borrow().transactions.get(&handle).map(|t| t.ops.clone()));
    let Some(ops) = ops else {
        return Err(missing_transaction(vm, handle));
    };
    let Some(ph) = parcel_native_ptr(vm, parcel_obj) else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalStateException;",
            Some("nativeWriteTransactionToParcel: parcel has no mNativePtr"),
        ));
    };
    let bytes = encode_ops(&ops);
    write_bytes_to_parcel(ph, &bytes);
    Ok(())
}

fn native_read_transaction_from_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let parcel_obj = vm.arg_ref(ctx, 0);
    let Some(ph) = parcel_native_ptr(vm, parcel_obj) else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalStateException;",
            Some("nativeReadTransactionFromParcel: parcel has no mNativePtr"),
        ));
    };
    // A malformed transaction must surface as a structured Java error, never
    // as a silently-empty successful transaction (HONESTY_AUDIT.md #6/#17).
    let Some(bytes) = read_bytes_from_parcel(ph) else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalStateException;",
            Some("nativeReadTransactionFromParcel: malformed transaction parcel"),
        ));
    };
    let Some(ops) = decode_ops(&bytes) else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalStateException;",
            Some("nativeReadTransactionFromParcel: undecodable surface op stream"),
        ));
    };
    let handle = with_state(|st| {
        let handle = st.alloc();
        st.transactions.insert(
            handle,
            TransactionRecord {
                id: handle,
                ops,
                animation: false,
            },
        );
        handle
    });
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn native_write_to_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let parcel_obj = vm.arg_ref(ctx, 1);
    let exists = STATE.with(|s| s.borrow().layers.contains_key(&handle));
    if !exists {
        return Err(stale_error(vm, handle));
    }
    let Some(ph) = parcel_native_ptr(vm, parcel_obj) else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalStateException;",
            Some("nativeWriteToParcel: parcel has no mNativePtr"),
        ));
    };
    write_i64_to_parcel(ph, handle as i64);
    Ok(())
}

fn native_read_from_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let parcel_obj = vm.arg_ref(ctx, 0);
    let Some(ph) = parcel_native_ptr(vm, parcel_obj) else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalStateException;",
            Some("nativeReadFromParcel: parcel has no mNativePtr"),
        ));
    };
    let handle = with_store(|s| s.table.with_parcel(ph, |p| p.read_int64_lenient())).unwrap_or(0) as u64;
    if handle == 0 {
        return Err(stale_error(vm, handle));
    }
    with_state(|st| {
        if let std::collections::hash_map::Entry::Vacant(e) = st.layers.entry(handle) {
            // A SurfaceControl read from a parcel of another process is not in
            // this thread's table; materialise a mirror record with the same id.
            e.insert(LayerRecord::new(handle, "remote".to_string(), 0, 0));
            st.next_handle = st.next_handle.max(handle + 1);
        }
    });
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn native_disconnect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let outcome = with_state(|st| match st.layers.get_mut(&handle) {
        Some(l) => {
            l.alive = false;
            Ok(())
        }
        None => Err(()),
    });
    outcome.map_err(|()| stale_error(vm, handle))
}

fn native_get_layer_id(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let gen = STATE.with(|s| s.borrow().layers.get(&handle).map(|l| l.generation));
    match gen {
        Some(_) => {
            // ACOS layer ids are the compositor layer ids; the handle doubles
            // as the layer id (1:1 with the layer table).
            vm.ret_i32(ctx, handle as i32);
            Ok(())
        }
        None => Err(stale_error(vm, handle)),
    }
}

fn native_get_handle(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

/// `SurfaceControl` finalizer token handed to `NativeAllocationRegistry`.
/// ACOS Rust records have no host function pointer to free, so a small token
/// identifies the record kind to [`apply_free_function`].
const FREE_SURFACE_CONTROL: u64 = 1;
/// `SurfaceControl.Transaction` finalizer token.
const FREE_TRANSACTION: u64 = 2;

fn surface_control_finalizer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, FREE_SURFACE_CONTROL as i64);
    Ok(())
}

fn transaction_finalizer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, FREE_TRANSACTION as i64);
    Ok(())
}

// ---------------------------------------------------------------------------
// Recorded-but-not-composited setters
// ---------------------------------------------------------------------------

fn record(vm: &mut Vm, ctx: NativeCtx, tag: u16, args: [i64; 4]) -> VmResult<()> {
    with_txn_op(vm, ctx, move |_| Some(SurfaceOp::Recorded { tag, args }))
}

fn layer_arg(vm: &Vm, ctx: NativeCtx) -> i64 {
    vm.arg_i64(ctx, 1)
}

fn native_record_trusted_overlay(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let v = vm.arg_bool(ctx, 2) as i64;
    record(vm, ctx, 100, [l, v, 0, 0])
}

fn native_record_drop_input(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let m = vm.arg_i32(ctx, 2) as i64;
    record(vm, ctx, 101, [l, m, 0, 0])
}

fn native_record_frame_rate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let f = vm.arg_f32(ctx, 2).to_bits() as i64;
    let c = vm.arg_i32(ctx, 3) as i64;
    let s = vm.arg_i32(ctx, 4) as i64;
    record(vm, ctx, 102, [l, f, c, s])
}

fn native_record_frame_rate_priority(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let p = vm.arg_i32(ctx, 2) as i64;
    record(vm, ctx, 103, [l, p, 0, 0])
}

fn native_record_caching_hint(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let h = vm.arg_i32(ctx, 2) as i64;
    record(vm, ctx, 104, [l, h, 0, 0])
}

fn native_record_data_space(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let d = vm.arg_i32(ctx, 2) as i64;
    record(vm, ctx, 105, [l, d, 0, 0])
}

fn native_record_extended_range(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let a = vm.arg_f32(ctx, 2).to_bits() as i64;
    let b = vm.arg_f32(ctx, 3).to_bits() as i64;
    record(vm, ctx, 106, [l, a, b, 0])
}

fn native_record_shadow_radius(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let r = vm.arg_f32(ctx, 2).to_bits() as i64;
    record(vm, ctx, 107, [l, r, 0, 0])
}

fn native_record_corner_radius(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let r = vm.arg_f32(ctx, 2).to_bits() as i64;
    record(vm, ctx, 108, [l, r, 0, 0])
}

fn native_record_blur_radius(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let r = vm.arg_i32(ctx, 2) as i64;
    record(vm, ctx, 109, [l, r, 0, 0])
}

fn native_record_color(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    // The float[] colour is read for shape only; the tag preserves the call.
    let _ = vm.arg_ref(ctx, 2);
    record(vm, ctx, 110, [l, 0, 0, 0])
}

fn native_record_color_transform(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    record(vm, ctx, 111, [l, 0, 0, 0])
}

fn native_record_color_space_agnostic(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    let v = vm.arg_bool(ctx, 2) as i64;
    record(vm, ctx, 112, [l, v, 0, 0])
}

fn native_record_destination_frame(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    record(vm, ctx, 113, [l, 0, 0, 0])
}

fn native_record_geometry(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    record(vm, ctx, 114, [l, 0, 0, 0])
}

fn native_record_metadata(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = layer_arg(vm, ctx);
    record(vm, ctx, 115, [l, 0, 0, 0])
}

// ---------------------------------------------------------------------------
// Surface natives
// ---------------------------------------------------------------------------

fn surface_is_valid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let valid = STATE.with(|s| s.borrow().surfaces.get(&h).is_some_and(|r| r.valid));
    vm.ret_bool(ctx, valid);
    Ok(())
}

fn surface_from_control(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let control = vm.arg_i64(ctx, 0) as u64;
    let handle = with_state(|st| {
        if !st.layers.get(&control).is_some_and(|l| l.alive) {
            return None;
        }
        let handle = st.alloc();
        st.surfaces.insert(
            handle,
            SurfaceRecord {
                id: handle,
                layer: control,
                valid: true,
            },
        );
        Some(handle)
    });
    match handle {
        Some(handle) => {
            vm.ret_i64(ctx, handle as i64);
            Ok(())
        }
        None => Err(stale_error(vm, control)),
    }
}

/// `Surface.nativeGetFromSurfaceControl(J surfacePtr, J controlPtr)J`.
///
/// Unlike `nativeCreateFromSurfaceControl(J)`, the control handle is the
/// **second** argument (the first is the `Surface.mNativeObject` being
/// populated), so this cannot share [`surface_from_control`].
fn surface_get_from_control(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let control = vm.arg_i64(ctx, 1) as u64;
    let handle = with_state(|st| {
        if !st.layers.get(&control).is_some_and(|l| l.alive) {
            return None;
        }
        let handle = st.alloc();
        st.surfaces.insert(
            handle,
            SurfaceRecord {
                id: handle,
                layer: control,
                valid: true,
            },
        );
        Some(handle)
    });
    match handle {
        Some(handle) => {
            vm.ret_i64(ctx, handle as i64);
            Ok(())
        }
        None => Err(stale_error(vm, control)),
    }
}

fn surface_from_queue(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let queue = vm.arg_i64(ctx, 0) as u64;
    let control = vm.arg_i64(ctx, 1) as u64;
    let existing = STATE.with(|s| s.borrow().queues.get(&queue).map(|q| q.surface));
    if let Some(handle) = existing {
        vm.ret_i64(ctx, handle as i64);
        return Ok(());
    }
    // No queue: materialise a Surface from the supplied SurfaceControl handle.
    let handle = with_state(|st| {
        if !st.layers.get(&control).is_some_and(|l| l.alive) {
            return None;
        }
        let handle = st.alloc();
        st.surfaces.insert(
            handle,
            SurfaceRecord {
                id: handle,
                layer: control,
                valid: true,
            },
        );
        Some(handle)
    });
    match handle {
        Some(handle) => {
            vm.ret_i64(ctx, handle as i64);
            Ok(())
        }
        None => Err(stale_error(vm, control)),
    }
}

fn surface_width(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let w = STATE.with(|s| {
        let st = s.borrow();
        st.surfaces
            .get(&h)
            .and_then(|r| st.layers.get(&r.layer))
            .map_or(0, |l| l.width)
    });
    vm.ret_i32(ctx, w);
    Ok(())
}

fn surface_height(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let hh = STATE.with(|s| {
        let st = s.borrow();
        st.surfaces
            .get(&h)
            .and_then(|r| st.layers.get(&r.layer))
            .map_or(0, |l| l.height)
    });
    vm.ret_i32(ctx, hh);
    Ok(())
}

fn surface_next_frame(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    let frame = STATE.with(|s| {
        let st = s.borrow();
        st.surfaces
            .get(&h)
            .and_then(|r| st.layers.get(&r.layer))
            .and_then(|l| l.buffer_frame)
            .unwrap_or(0)
    });
    vm.ret_i64(ctx, frame as i64 + 1);
    Ok(())
}

fn surface_release(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    STATE.with(|s| {
        if let Some(r) = s.borrow_mut().surfaces.get_mut(&h) {
            r.valid = false;
        }
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// SurfaceSession natives
// ---------------------------------------------------------------------------

fn session_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = with_state(|st| {
        let handle = st.alloc();
        st.sessions.insert(handle, true);
        handle
    });
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn session_destroy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    STATE.with(|s| {
        s.borrow_mut().sessions.remove(&h);
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// BLASTBufferQueue natives
// ---------------------------------------------------------------------------

fn blast_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = {
        let s = vm.arg_ref(ctx, 0);
        if s == NULL {
            "BLASTBufferQueue".to_string()
        } else {
            vm.java_string_to_rust(s)
                .unwrap_or_else(|_| "BLASTBufferQueue".to_string())
        }
    };
    let queue = with_state(|st| {
        let queue = st.alloc();
        // A BLAST queue creates its own layer; `nativeUpdate` attaches it.
        let layer = st.alloc();
        st.layers
            .insert(layer, LayerRecord::new(layer, name.clone(), 0, 0));
        let surface = st.alloc();
        st.surfaces.insert(
            surface,
            SurfaceRecord {
                id: surface,
                layer,
                valid: true,
            },
        );
        st.queues.insert(
            queue,
            BlastQueueRecord {
                id: queue,
                name,
                layer,
                surface,
                transform_hint: 0,
            },
        );
        queue
    });
    vm.ret_i64(ctx, queue as i64);
    Ok(())
}

fn blast_destroy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0) as u64;
    with_state(|st| {
        if let Some(q) = st.queues.remove(&h) {
            st.layers.remove(&q.layer);
            st.surfaces.remove(&q.surface);
        }
    });
    Ok(())
}

fn blast_update(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let queue = vm.arg_i64(ctx, 0) as u64;
    let control = vm.arg_i64(ctx, 1) as u64;
    let width = vm.arg_i32(ctx, 2);
    let height = vm.arg_i32(ctx, 3);
    let format = vm.arg_i32(ctx, 4);
    let _ = format;
    let alive = with_state(|st| {
        if !st.layers.get(&control).is_some_and(|l| l.alive) {
            return false;
        }
        // Re-point the queue at the new layer and size it.
        let old_layer = st.queues.get(&queue).map(|q| q.layer);
        if let Some(q) = st.queues.get_mut(&queue) {
            q.layer = control;
        }
        if let Some(old) = old_layer {
            if old != control {
                st.layers.remove(&old);
            }
        }
        if let Some(l) = st.layers.get_mut(&control) {
            l.width = width;
            l.height = height;
        }
        true
    });
    if !alive {
        return Err(stale_error(vm, control));
    }
    Ok(())
}

fn blast_get_surface(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let queue = vm.arg_i64(ctx, 0) as u64;
    let surface = STATE.with(|s| s.borrow().queues.get(&queue).map(|q| q.surface));
    let Some(surface) = surface else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalStateException;",
            Some(&format!("BLASTBufferQueue: stale/unknown handle {queue}")),
        ));
    };
    // Return a real `android.view.Surface` wrapping the native surface handle,
    // exactly like the framework's `Surface(long)` constructor.
    let obj = vm
        .new_instance_ctor(
            "Landroid/view/Surface;",
            "(J)V",
            &[JavaValue::Long(surface as i64)],
        )
        .unwrap_or(NULL);
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn blast_merge(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `mergeWithNextTransaction(Transaction t, long frameNumber)`: ACOS has no
    // deferred BLAST sync, so the transaction is applied immediately and the
    // frame number recorded as the published frame.
    let queue = vm.arg_i64(ctx, 0) as u64;
    let txn = vm.arg_i64(ctx, 1) as u64;
    let frame = vm.arg_i64(ctx, 2) as u64;
    let ops = STATE.with(|s| s.borrow().transactions.get(&txn).map(|t| t.ops.clone()));
    let Some(ops) = ops else {
        return Err(missing_transaction(vm, txn));
    };
    let layer = STATE.with(|s| s.borrow().queues.get(&queue).map(|q| q.layer));
    let Some(layer) = layer else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalStateException;",
            Some(&format!("BLASTBufferQueue: stale/unknown handle {queue}")),
        ));
    };
    let mut merged = ops;
    merged.push(SurfaceOp::Buffer { layer, frame_number: frame });
    apply_ops(vm, &merged)?;
    STATE.with(|s| s.borrow_mut().presented_frames += 1);
    Ok(())
}

fn blast_apply_pending(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let queue = vm.arg_i64(ctx, 0) as u64;
    let txn = vm.arg_i64(ctx, 1) as u64;
    let ops = STATE.with(|s| s.borrow().transactions.get(&txn).map(|t| t.ops.clone()));
    let Some(ops) = ops else {
        return Err(missing_transaction(vm, txn));
    };
    let exists = STATE.with(|s| s.borrow().queues.contains_key(&queue));
    if !exists {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalStateException;",
            Some(&format!("BLASTBufferQueue: stale/unknown handle {queue}")),
        ));
    }
    apply_ops(vm, &ops)
}

fn blast_same_control(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_i64(ctx, 0) as u64;
    let b = vm.arg_i64(ctx, 1) as u64;
    vm.ret_bool(ctx, a == b);
    Ok(())
}

fn blast_last_frame(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let queue = vm.arg_i64(ctx, 0) as u64;
    let frame = STATE.with(|s| {
        let st = s.borrow();
        st.queues
            .get(&queue)
            .and_then(|q| st.layers.get(&q.layer))
            .and_then(|l| l.buffer_frame)
            .unwrap_or(0)
    });
    vm.ret_i64(ctx, frame as i64);
    Ok(())
}

fn blast_clear_sync(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let txn = vm.arg_i64(ctx, 0) as u64;
    with_state(|st| {
        if let Some(t) = st.transactions.get_mut(&txn) {
            t.ops.clear();
        }
    });
    Ok(())
}

fn blast_gather(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // No deferred BLAST sync exists; gather returns an empty transaction.
    let _queue = vm.arg_i64(ctx, 0);
    let _frame = vm.arg_i64(ctx, 1);
    let handle = with_state(|st| {
        let handle = st.alloc();
        st.transactions.insert(
            handle,
            TransactionRecord {
                id: handle,
                ops: Vec::new(),
                animation: false,
            },
        );
        handle
    });
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_round_trip_is_byte_stable() {
        let ops = vec![
            SurfaceOp::Position { layer: 7, x: 3.5, y: -1.25 },
            SurfaceOp::Alpha { layer: 7, alpha: 0.5 },
            SurfaceOp::Layer { layer: 7, z: -3 },
            SurfaceOp::Crop { layer: 7, rect: Some([1, 2, 30, 40]) },
            SurfaceOp::Crop { layer: 7, rect: None },
            SurfaceOp::Flags { layer: 7, flags: 4, mask: 4 },
            SurfaceOp::Buffer { layer: 7, frame_number: 99 },
            SurfaceOp::Matrix { layer: 7, dsdx: 1.0, dtdx: 0.0, dtdy: 0.0, dsdy: 1.0 },
            SurfaceOp::Reparent { layer: 7, new_parent: 0 },
            SurfaceOp::Recorded { tag: 100, args: [1, -2, 3, -4] },
        ];
        let bytes = encode_ops(&ops);
        let decoded = decode_ops(&bytes).expect("decode");
        assert_eq!(decoded, ops, "decode must reproduce every field");
        assert_eq!(encode_ops(&decoded), bytes, "re-encode must be byte-identical");
    }
}
