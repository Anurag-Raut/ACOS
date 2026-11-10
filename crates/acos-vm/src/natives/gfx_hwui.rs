//! `android.graphics.HardwareRenderer` (a.k.a. ThreadedRenderer) state machine.
//!
//! This is the ACOS replacement for HWUI's RenderProxy. It owns a
//! `RenderNode` tree reference, rasterises the tree through `acos-render`
//! (ADR-0011, Rust software 2D) and hands the resulting RGBA8888 buffer to a
//! [`FrameSink`]. W19-A provides the production sink (the BLAST/compositor
//! buffer); this module owns the trait and ships an in-memory sink so the
//! recording → raster → pixel path is testable without a window system.
//!
//! The frame-complete / frame-drawing callbacks are registered and stored; ACOS
//! dispatches them from the frame handoff (`T-34`), because a native here must
//! not re-enter the interpreter while a `RenderThread` frame is in progress.

use std::cell::RefCell;

use acos_render::context::RenderContext;
use acos_render::handle::Handle as RHandle;
use acos_render::render_node::DisplayOp;

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::javabridge::JavaValue;
use crate::natives::gfx_handles::{Handle, HandleTable};
use crate::natives::gfx_state::{gj, render_err, rh, stale, with_ctx};
use crate::vm::Vm;

/// A destination for a rasterised frame.
///
/// W19-A implements this against the compositor; `submit_frame` must hand the
/// **completed** RGBA8888 frame to the buffer and return its monotonic frame
/// sequence (the same number the agent-facing frame contract exposes as
/// `frame_seq`).
pub trait FrameSink {
    fn submit_frame(
        &mut self,
        window_id: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
        cause_id: i64,
    ) -> u64;
}

/// A captured frame, for tests and the in-memory sink.
#[derive(Clone, Debug)]
pub struct CapturedFrame {
    pub window_id: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub cause_id: i64,
    pub seq: u64,
}

/// The default sink: keeps the last frame in memory.
#[derive(Default, Debug)]
pub struct MemorySink {
    last: Option<CapturedFrame>,
    seq: u64,
}

impl FrameSink for MemorySink {
    fn submit_frame(
        &mut self,
        window_id: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
        cause_id: i64,
    ) -> u64 {
        self.seq += 1;
        self.last = Some(CapturedFrame {
            window_id,
            width,
            height,
            rgba: rgba.to_vec(),
            cause_id,
            seq: self.seq,
        });
        self.seq
    }
}

thread_local! {
    static SINK: RefCell<Box<dyn FrameSink>> = RefCell::new(Box::new(MemorySink::default()));
    static LAST: RefCell<Option<CapturedFrame>> = const { RefCell::new(None) };
    static HWUI: RefCell<HwuiState> = RefCell::new(HwuiState::new());
    static OBSERVER_ID: RefCell<u64> = const { RefCell::new(1) };
}

/// Install the production compositor sink (W19-A).
pub fn install_sink(sink: Box<dyn FrameSink>) {
    SINK.with(|s| *s.borrow_mut() = sink);
}

/// The most recently submitted frame (test/introspection helper).
pub fn last_frame() -> Option<CapturedFrame> {
    LAST.with(|l| l.borrow().clone())
}

struct Proxy {
    root: RHandle,
    surface: Option<RHandle>,
    surface_size: (u32, u32),
    /// The `SurfaceControl` layer bound by the WMS backend, if any. Frames are
    /// submitted to this layer instead of the synthetic per-proxy window.
    bound_layer: Option<u64>,
    stopped: bool,
    paused: bool,
    name: String,
    /// `frame_seq` of the last successfully submitted frame.
    frames: u64,
    /// Frame-timeline samples received (one per `nSyncAndDrawFrame`).
    samples: u64,
    light_geometry: (f32, f32, f32, f32),
    animating: Vec<RHandle>,
    drawing_enabled: bool,
    frame_callback: u32,
    frame_complete_callback: u32,
    frame_commit_callback: u32,
}

struct HwuiState {
    proxies: HandleTable<Proxy>,
    next_window: u64,
    drawing_enabled: bool,
}

impl HwuiState {
    fn new() -> HwuiState {
        HwuiState {
            proxies: HandleTable::new(),
            next_window: 1,
            drawing_enabled: true,
        }
    }
}

fn proxy(vm: &Vm, ctx: NativeCtx) -> VmResult<Handle> {
    let h = Handle::from_jlong(vm.arg_i64(ctx, 0));
    if HWUI.with(|s| s.borrow().proxies.contains(h)) {
        Ok(h)
    } else {
        Err(stale("HardwareRenderer proxy", vm.arg_i64(ctx, 0)))
    }
}

/// Register the `HardwareRenderer` natives.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    const H: &str = "Landroid/graphics/HardwareRenderer;";
    vm.register_native(H, "nCreateRootRenderNode", "()J", h_create_root);
    vm.register_native(H, "nCreateProxy", "(ZJ)J", h_create_proxy);
    vm.register_native(H, "nDeleteProxy", "(J)V", h_delete_proxy);
    vm.register_native(H, "nDestroy", "(JJ)V", h_delete_proxy);
    vm.register_native(H, "nSetSurface", "(JLandroid/view/Surface;Z)V", h_set_surface);
    vm.register_native(H, "nSetSurfaceControl", "(JJ)V", h_noop);
    vm.register_native(H, "nSyncAndDrawFrame", "(J[JI)I", h_sync_and_draw);
    vm.register_native(H, "nNotifyFramePending", "(J)V", h_noop);
    vm.register_native(H, "nGetRenderThreadTid", "(J)I", h_render_tid);
    vm.register_native(H, "nPause", "(J)Z", h_pause);
    vm.register_native(H, "nSetStopped", "(JZ)V", h_set_stopped);
    vm.register_native(H, "nIsDrawingEnabled", "()Z", h_is_drawing_enabled);
    vm.register_native(H, "nSetDrawingEnabled", "(Z)V", h_set_drawing_enabled);
    vm.register_native(H, "nSetName", "(JLjava/lang/String;)V", h_set_name);
    vm.register_native(H, "nSetLightGeometry", "(JFFFF)V", h_light_geometry);
    vm.register_native(H, "nSetLightAlpha", "(JFF)V", h_noop);
    vm.register_native(H, "nRegisterAnimatingRenderNode", "(JJ)V", h_register_animating);
    vm.register_native(H, "nAllocateBuffers", "(J)V", h_noop);
    vm.register_native(H, "nLoadSystemProperties", "(J)Z", h_ret_false);
    vm.register_native(H, "nFence", "(J)V", h_noop);
    vm.register_native(H, "nForceDrawNextFrame", "(J)V", h_noop);
    vm.register_native(H, "nStopDrawing", "(J)V", h_noop);
    vm.register_native(H, "nNotifyExpensiveFrame", "(J)V", h_noop);
    vm.register_native(H, "nNotifyCallbackPending", "(J)V", h_noop);
    vm.register_native(H, "nRotateProcessStatsBuffer", "()V", h_noop);
    vm.register_native(H, "nSetProcessStatsBuffer", "(I)V", h_noop);
    vm.register_native(H, "nSetRtAnimationsEnabled", "(Z)V", h_noop);
    vm.register_native(H, "nSetDebuggingEnabled", "(Z)V", h_noop);
    vm.register_native(H, "nSetHighContrastText", "(Z)V", h_noop);
    vm.register_native(H, "nSetColorMode", "(JI)F", h_set_color_mode);
    vm.register_native(H, "nSetContextPriority", "(I)V", h_noop);
    vm.register_native(H, "nSetDisplayDensityDpi", "(I)V", h_noop);
    vm.register_native(H, "nSetIsHighEndGfx", "(Z)V", h_noop);
    vm.register_native(H, "nSetIsLowRam", "(Z)V", h_noop);
    vm.register_native(H, "nSetIsSystemOrPersistent", "(Z)V", h_noop);
    vm.register_native(H, "nSetIsolatedProcess", "(Z)V", h_noop);
    vm.register_native(H, "nSetOpaque", "(JZ)V", h_noop);
    vm.register_native(H, "nSetContentDrawBounds", "(JIIII)V", h_noop);
    vm.register_native(H, "nSetForceDark", "(JZ)V", h_noop);
    vm.register_native(H, "nSetSdrWhitePoint", "(JF)V", h_noop);
    vm.register_native(H, "nSetTargetSdrHdrRatio", "(JF)V", h_noop);
    vm.register_native(H, "nTrimCaches", "(I)V", h_noop);
    vm.register_native(H, "nTrimMemory", "(I)V", h_noop);
    vm.register_native(H, "nSetFrameCallback", "(JLandroid/graphics/HardwareRenderer$FrameDrawingCallback;)V", h_set_frame_callback);
    vm.register_native(H, "nSetFrameCompleteCallback", "(JLandroid/graphics/HardwareRenderer$FrameCompleteCallback;)V", h_set_frame_complete_callback);
    vm.register_native(H, "nSetFrameCommitCallback", "(JLandroid/graphics/HardwareRenderer$FrameCommitCallback;)V", h_set_frame_commit_callback);
    vm.register_native(H, "nAddObserver", "(JJ)V", h_noop);
    vm.register_native(H, "nRemoveObserver", "(JJ)V", h_noop);
    const O: &str = "Landroid/graphics/HardwareRendererObserver;";
    vm.register_native(O, "nCreateObserver", "(Ljava/lang/ref/WeakReference;Z)J", h_create_observer);
    vm.register_native(O, "nGetNextBuffer", "(J[J)I", h_ret_zero_int);
    Ok(())
}

fn h_create_root(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = with_ctx(|c| c.render_node_create("Root"));
    vm.ret_i64(ctx, gj(h));
    Ok(())
}

fn h_create_proxy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ltr = vm.arg_bool(ctx, 0);
    let root = rh(vm.arg_i64(ctx, 1));
    if !with_ctx(|c| c.render_nodes.contains(root)) {
        return Err(stale("RenderNode", vm.arg_i64(ctx, 1)));
    }
    let h = HWUI.with(|s| {
        let mut s = s.borrow_mut();
        let _ = ltr;
        s.proxies.insert(Proxy {
            root,
            surface: None,
            surface_size: (0, 0),
            bound_layer: None,
            stopped: false,
            paused: false,
            name: String::new(),
            frames: 0,
            samples: 0,
            light_geometry: (0.0, 0.0, 0.0, 0.0),
            animating: Vec::new(),
            drawing_enabled: true,
            frame_callback: 0,
            frame_complete_callback: 0,
            frame_commit_callback: 0,
        })
    });
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn h_delete_proxy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = Handle::from_jlong(vm.arg_i64(ctx, 0));
    HWUI.with(|s| {
        let _ = s.borrow_mut().proxies.remove(h);
    });
    Ok(())
}

fn h_set_surface(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    // The real `Surface` is owned by W19-A's compositor; resolve its native
    // handle to the `SurfaceControl` layer bound by the WMS backend so the
    // frame handoff can submit to the real layer and use its buffer size.
    // A `null` Surface detaches (Android allows this to tear the renderer off a
    // window).
    let surface_obj = vm.arg_ref(ctx, 1);
    let (bound, bound_size) = if surface_obj != 0 {
        match vm.get_field(surface_obj, "mNativeObject") {
            Ok(JavaValue::Long(v)) => {
                let handle = v as u64;
                (
                    crate::natives::fwk_surface::surface_layer(handle),
                    crate::natives::fwk_surface::surface_size(handle),
                )
            }
            _ => (None, None),
        }
    } else {
        (None, None)
    };
    HWUI.with(|s| {
        if let Ok(p) = s.borrow_mut().proxies.get_mut(h) {
            p.stopped = false;
            p.surface = None;
            p.bound_layer = bound;
            p.surface_size = bound_size
                .map(|(w, h)| (w.max(0) as u32, h.max(0) as u32))
                .unwrap_or((0, 0));
        }
    });
    Ok(())
}

fn h_sync_and_draw(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    let (root, mut surface, mut size, active, bound_layer) = HWUI.with(|s| {
        let s = s.borrow();
        match s.proxies.get(h) {
            Ok(p) => (
                p.root,
                p.surface,
                p.surface_size,
                !p.stopped && p.drawing_enabled,
                p.bound_layer,
            ),
            Err(_) => (RHandle(0), None, (0, 0), false, None),
        }
    });
    if !active {
        // A stopped/disabled renderer accepts the frame request without drawing
        // (Android returns `SYNC_OK`); never fake pixels.
        vm.ret_i32(ctx, 0);
        return Ok(());
    }
    // Rasterise the tree. A surface bound through the WMS backend supplies the
    // real layer size; without one (the in-memory path) size to the subtree.
    let (w, hgt) = if size.0 > 0 && size.1 > 0 {
        size
    } else {
        with_ctx(|c| subtree_extent(c, root))
    };
    let (w, hgt) = (w.max(1), hgt.max(1));
    if surface.is_none() || size != (w, hgt) {
        surface = with_ctx(|c| c.surface_create(w, hgt, 1.0).ok());
        size = (w, hgt);
    }
    let Some(surface) = surface else {
        return Err(VmError::unsupported_api(
            "HardwareRenderer surface allocation",
            "acos-render Surface::new",
        ));
    };
    let cause = HWUI.with(|s| s.borrow().proxies.get(h).map(|p| p.frames as i64 + 1).unwrap_or(1));
    let _ = with_ctx(|c| {
        c.surface_render_node(surface, root, cause)
            .map_err(|e| render_err("HardwareRenderer.syncAndDrawFrame", e))
    })?;
    let rgba = with_ctx(|c| c.surface_rgba8(surface).map_err(|e| render_err("surface_rgba8", e)))?;
    let window = bound_layer.unwrap_or_else(|| HWUI.with(|s| s.borrow().next_window));
    let frame = SINK
        .with(|s| s.borrow_mut().submit_frame(window, w, hgt, &rgba, cause));
    // Record the frame-timeline sample and the compositor frame id (not the
    // renderer's internal raster sequence), and remember the surface for reuse.
    HWUI.with(|s| {
        if let Ok(p) = s.borrow_mut().proxies.get_mut(h) {
            p.samples += 1;
            p.frames = frame;
            p.surface = Some(surface);
            p.surface_size = size;
        }
    });
    LAST.with(|l| {
        *l.borrow_mut() = Some(CapturedFrame {
            window_id: window,
            width: w,
            height: hgt,
            rgba,
            cause_id: cause,
            seq: frame,
        })
    });
    vm.ret_i32(ctx, 0);
    Ok(())
}

/// Union of the subtree's `(right, bottom)` extents, following both child
/// links and `DrawRenderNode` display ops.
fn subtree_extent(ctx: &RenderContext, root: RHandle) -> (u32, u32) {
    let mut max_r = 0i32;
    let mut max_b = 0i32;
    let mut stack = vec![root];
    let mut guard = 0;
    while let Some(h) = stack.pop() {
        guard += 1;
        if guard > 100_000 {
            break;
        }
        let Some(n) = ctx.render_nodes.get(h) else {
            continue;
        };
        max_r = max_r.max(n.right.max(0) + n.translation_x as i32);
        max_b = max_b.max(n.bottom.max(0) + n.translation_y as i32);
        for c in &n.children {
            stack.push(*c);
        }
        for op in &n.display_list {
            if let DisplayOp::DrawRenderNode(c) = op {
                stack.push(*c);
            }
        }
    }
    (max_r.max(0) as u32, max_b.max(0) as u32)
}

fn h_render_tid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // ACOS has no separate RenderThread; the frame is produced on the VM thread
    // that calls `nSyncAndDrawFrame`. `libc::gettid` needs `unsafe`, which the
    // VM forbids, so report the process id (stable and non-zero) and document
    // that ACOS does not distinguish render/UI threads yet.
    let tid = std::process::id() as i32;
    vm.ret_i32(ctx, tid);
    Ok(())
}

fn h_pause(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    let v = HWUI.with(|s| {
        s.borrow_mut()
            .proxies
            .get_mut(h)
            .map(|p| {
                p.paused = true;
                true
            })
            .unwrap_or(false)
    });
    vm.ret_bool(ctx, v);
    Ok(())
}

fn h_set_stopped(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    let v = vm.arg_bool(ctx, 1);
    HWUI.with(|s| {
        if let Ok(p) = s.borrow_mut().proxies.get_mut(h) {
            p.stopped = v;
        }
    });
    Ok(())
}

fn h_is_drawing_enabled(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = HWUI.with(|s| s.borrow().drawing_enabled);
    vm.ret_bool(ctx, v);
    Ok(())
}

fn h_set_drawing_enabled(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_bool(ctx, 0);
    HWUI.with(|s| s.borrow_mut().drawing_enabled = v);
    Ok(())
}

fn h_set_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    let name = {
        let s = vm.arg_ref(ctx, 1);
        if s == 0 { String::new() } else { vm.string_to_rust(s).unwrap_or_default() }
    };
    HWUI.with(|s| {
        if let Ok(p) = s.borrow_mut().proxies.get_mut(h) {
            p.name = name;
        }
    });
    Ok(())
}

fn h_light_geometry(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    let g = (
        vm.arg_f32(ctx, 1),
        vm.arg_f32(ctx, 2),
        vm.arg_f32(ctx, 3),
        vm.arg_f32(ctx, 4),
    );
    HWUI.with(|s| {
        if let Ok(p) = s.borrow_mut().proxies.get_mut(h) {
            p.light_geometry = g;
        }
    });
    Ok(())
}

fn h_register_animating(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    let node = rh(vm.arg_i64(ctx, 1));
    HWUI.with(|s| {
        if let Ok(p) = s.borrow_mut().proxies.get_mut(h) {
            if !p.animating.contains(&node) {
                p.animating.push(node);
            }
        }
    });
    Ok(())
}

fn h_set_frame_callback(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    let cb = vm.arg_ref(ctx, 1);
    HWUI.with(|s| {
        if let Ok(p) = s.borrow_mut().proxies.get_mut(h) {
            p.frame_callback = cb;
        }
    });
    Ok(())
}

fn h_set_frame_complete_callback(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    let cb = vm.arg_ref(ctx, 1);
    HWUI.with(|s| {
        if let Ok(p) = s.borrow_mut().proxies.get_mut(h) {
            p.frame_complete_callback = cb;
        }
    });
    Ok(())
}

fn h_set_frame_commit_callback(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    let cb = vm.arg_ref(ctx, 1);
    HWUI.with(|s| {
        if let Ok(p) = s.borrow_mut().proxies.get_mut(h) {
            p.frame_commit_callback = cb;
        }
    });
    Ok(())
}

fn h_create_observer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // HWUI's observer reads the per-frame profile buffer. ACOS has no Perfetto
    // profile buffer, so the observer is a valid but empty handle: it exists so
    // framework code can register/unregister without a fake frame-timeline
    // sample. `nGetNextBuffer` reports 0 (no samples).
    let h = OBSERVER_ID.with(|n| {
        let mut n = n.borrow_mut();
        *n += 1;
        *n
    });
    vm.ret_i64(ctx, h as i64);
    Ok(())
}

fn h_noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}
fn h_ret_false(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}
fn h_ret_zero_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, 0);
    Ok(())
}
fn h_set_color_mode(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = proxy(vm, ctx)?;
    // ACOS always rasterises sRGB; report the density-ish 1.0 scale.
    let _ = h;
    vm.ret_f32(ctx, 1.0);
    Ok(())
}
