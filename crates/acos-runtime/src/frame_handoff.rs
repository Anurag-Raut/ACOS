//! T-34 frame handoff: `HardwareRenderer` → compositor → completed-frame store.
//!
//! [`FrameHandoffSink`] is the production [`FrameSink`] installed into
//! `acos-vm`'s `gfx_hwui` seam. Each `nSyncAndDrawFrame` hands the rasterised
//! RGBA8888 buffer to the [`WmSurfaceBackend`] bound by the WMS backend, which
//! attaches it to the real `SurfaceControl` layer and presents the compositor.
//! The resulting [`CompletedFrame`] is stored in a [`FrameStore`] so screenshot
//! and UI-tree consumers bind to the same completed frame.
//!
//! A frame is published **only** here, on a real buffer submission.
//! `IWindowSession.finishDrawing` is recorded by the WMS layer and never calls
//! this path (`docs/FINDINGS.md`, advisor rule).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use acos_vm::natives::fwk_surface::{
    clear_backend, install_backend, SurfaceBackend, SurfaceOp,
};
use acos_vm::natives::gfx_hwui::{install_sink, FrameSink, MemorySink};

use crate::wms_java::WmSurfaceBackend;

/// One completed frame as observed at the compositor output.
///
/// `frame_id` is the compositor's monotonic frame sequence; `window` is the
/// `SurfaceControl` layer handle the WMS backend bound; `content_hash` is the
/// compositor's stable 128-bit hash; `cause_id` names the action that produced
/// the content.
#[derive(Clone)]
pub struct CompletedFrame {
    /// Monotonic compositor frame sequence.
    pub frame_id: u64,
    /// `SurfaceControl` layer handle.
    pub window: u64,
    /// Stable 128-bit content hash of the composed pixels.
    pub content_hash: u128,
    /// Action/request id that caused the content.
    pub cause_id: i64,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Premultiplied RGBA8888 pixels.
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for CompletedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletedFrame")
            .field("frame_id", &self.frame_id)
            .field("window", &self.window)
            .field("content_hash", &self.content_hash)
            .field("cause_id", &self.cause_id)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixels_len", &self.pixels.len())
            .finish()
    }
}

struct FrameStoreInner {
    latest: Option<CompletedFrame>,
    published: u64,
}

/// A bounded-wait store for the most recent completed frame.
///
/// `latest()` reads the current frame; `wait_for_frame_after(id)` waits until a
/// frame strictly newer than `id` is published or the timeout expires. The
/// store never blocks an idle reader forever, so a screenshot request cannot
/// hang a caller when nothing new is drawn.
pub struct FrameStore {
    inner: Mutex<FrameStoreInner>,
    changed: Condvar,
}

impl std::fmt::Debug for FrameStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f.debug_struct("FrameStore")
            .field("published", &inner.published)
            .field("latest", &inner.latest.as_ref().map(|f| f.frame_id))
            .finish()
    }
}

impl Default for FrameStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(FrameStoreInner {
                latest: None,
                published: 0,
            }),
            changed: Condvar::new(),
        }
    }

    /// The most recently published frame, if any.
    #[must_use]
    pub fn latest(&self) -> Option<CompletedFrame> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.latest.clone()
    }

    /// Wait up to `timeout` for a frame with `frame_id > after`.
    ///
    /// Returns `None` when the timeout expires without a newer frame. A frame
    /// already newer than `after` is returned immediately.
    #[must_use]
    pub fn wait_for_frame_after(&self, after: u64, timeout: Duration) -> Option<CompletedFrame> {
        let deadline = Instant::now() + timeout;
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(frame) = &inner.latest {
                if frame.frame_id > after {
                    return Some(frame.clone());
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let remaining = deadline - now;
            let (next, _) = self
                .changed
                .wait_timeout(inner, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner = next;
        }
    }

    /// Number of frames published through this store.
    #[must_use]
    pub fn published_count(&self) -> u64 {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.published
    }

    fn publish(&self, frame: CompletedFrame) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.published += 1;
        inner.latest = Some(frame);
        drop(inner);
        self.changed.notify_all();
    }
}

/// The production [`FrameSink`]: submit to the WMS compositor and publish the
/// completed frame.
pub struct FrameHandoffSink {
    backend: Rc<RefCell<WmSurfaceBackend>>,
    store: Arc<FrameStore>,
}

impl FrameHandoffSink {
    /// Build a sink over an explicit backend and store.
    ///
    /// `install_frame_handoff` uses this with the thread-local backend; the
    /// integration test builds its own to observe the same path without
    /// replacing the process-wide installation.
    #[must_use]
    pub fn new(backend: Rc<RefCell<WmSurfaceBackend>>, store: Arc<FrameStore>) -> Self {
        Self { backend, store }
    }

    /// The store this sink publishes into.
    #[must_use]
    pub fn store(&self) -> &Arc<FrameStore> {
        &self.store
    }
}

impl std::fmt::Debug for FrameHandoffSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameHandoffSink")
            .field("store", &self.store)
            .finish()
    }
}

impl FrameSink for FrameHandoffSink {
    fn submit_frame(
        &mut self,
        window_id: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
        cause_id: i64,
    ) -> u64 {
        match self
            .backend
            .borrow_mut()
            .submit_frame(window_id, width, height, rgba, cause_id)
        {
            Ok(frame) => {
                let id = frame.frame_id;
                self.store.publish(frame);
                id
            }
            Err(e) => {
                // A failed submission must not fabricate a frame id or pixels.
                eprintln!("frame_handoff: submit_frame failed: {e}");
                0
            }
        }
    }
}

/// The `view.surface` backend shared between the surface natives and the frame
/// sink. Both see the same `WmSurfaceBackend`, so a layer created by
/// `SurfaceControl` is the layer a frame is submitted to.
struct SharedBackend(Rc<RefCell<WmSurfaceBackend>>);

impl std::fmt::Debug for SharedBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedBackend").finish()
    }
}

impl SurfaceBackend for SharedBackend {
    fn create_layer(&mut self, handle: u64, name: &str, width: i32, height: i32) -> u64 {
        self.0
            .borrow_mut()
            .create_layer(handle, name, width, height)
    }

    fn apply(&mut self, ops: &[SurfaceOp]) -> Result<u64, String> {
        self.0.borrow_mut().apply(ops)
    }

    fn submit_buffer(&mut self, layer: u64, frame_number: u64) -> Result<u64, String> {
        self.0.borrow_mut().submit_buffer(layer, frame_number)
    }

    fn release(&mut self, layer: u64) {
        self.0.borrow_mut().release(layer);
    }

    fn generation(&self) -> u64 {
        self.0.borrow().generation()
    }
}

thread_local! {
    static INSTALLED: RefCell<Option<Arc<FrameStore>>> = const { RefCell::new(None) };
}

/// Install the production `view.surface` backend and the frame sink, returning
/// the completed-frame store.
///
/// Idempotent on the calling host thread: a second call returns the same store
/// instead of replacing the installed backend/sink.
pub fn install_frame_handoff() -> Result<Arc<FrameStore>, String> {
    if let Some(store) = INSTALLED.with(|i| i.borrow().clone()) {
        return Ok(store);
    }
    let backend = Rc::new(RefCell::new(WmSurfaceBackend::new()?));
    let store = Arc::new(FrameStore::new());
    install_backend(Box::new(SharedBackend(Rc::clone(&backend))));
    install_sink(Box::new(FrameHandoffSink {
        backend,
        store: Arc::clone(&store),
    }));
    INSTALLED.with(|i| *i.borrow_mut() = Some(Arc::clone(&store)));
    Ok(store)
}

/// Remove the installed backend/sink and return the last store (test teardown).
pub fn clear_frame_handoff() {
    INSTALLED.with(|i| *i.borrow_mut() = None);
    clear_backend();
    install_sink(Box::new(MemorySink::default()));
}
