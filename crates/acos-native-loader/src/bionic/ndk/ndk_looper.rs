//! `libandroid` `ALooper`: the NDK's fd event loop.
//!
//! This is a real implementation, not a shim: ACOS runs guest code on a host
//! thread, so each `ALooper` owns a Linux `epoll` set plus an `eventfd` used by
//! `ALooper_wake`. `ALooper_addFd` registers the guest fd (level-triggered, as
//! bionic does), `ALooper_pollOnce`/`pollAll` wait with the same timeout and
//! return codes as bionic, and callback sources are invoked exactly once per
//! readiness notification.
//!
//! `ALooper_prepare` returns one looper per thread (bionic's contract);
//! `ALooper_forThread` returns the same object. Nothing here touches Android
//! framework state, so no host is required.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// `ALOOPER_POLL_WAKE` — the looper was woken.
pub const ALOOPER_POLL_WAKE: c_int = -1;
/// `ALOOPER_POLL_CALLBACK` — a callback was invoked.
pub const ALOOPER_POLL_CALLBACK: c_int = -2;
/// `ALOOPER_POLL_TIMEOUT` — no event before the timeout.
pub const ALOOPER_POLL_TIMEOUT: c_int = -3;
/// `ALOOPER_POLL_ERROR` — the wait failed.
pub const ALOOPER_POLL_ERROR: c_int = -4;

/// `ALOOPER_EVENT_INPUT`.
pub const ALOOPER_EVENT_INPUT: c_int = 1;
/// `ALOOPER_EVENT_OUTPUT`.
pub const ALOOPER_EVENT_OUTPUT: c_int = 2;
/// `ALOOPER_EVENT_ERROR`.
pub const ALOOPER_EVENT_ERROR: c_int = 4;
/// `ALOOPER_EVENT_HANGUP`.
pub const ALOOPER_EVENT_HANGUP: c_int = 8;
/// `ALOOPER_EVENT_INVALID`.
pub const ALOOPER_EVENT_INVALID: c_int = 16;

/// `ALOOPER_PREPARE_ALLOW_NON_CALLBACKS`.
pub const ALOOPER_PREPARE_ALLOW_NON_CALLBACKS: c_int = 1;

/// `ALooper_callbackFunc`.
pub type ALooperCallback = Option<unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int>;

struct Source {
    ident: c_int,
    callback: ALooperCallback,
    data: *mut c_void,
}

// SAFETY: `Source` only holds guest callback/data pointers, which the NDK
// contract ties to the looper's lifetime; the table is mutex-protected.
unsafe impl Send for Source {}

/// A frame callback queued by `AChoreographer_postFrameCallback` for delivery
/// from its looper's poll.
///
/// `frame_time_nanos` is the vsync timestamp the engine's callback receives;
/// `cb`/`data` are the guest's pointers. Nothing here fakes the frame time: the
/// host computed it from the virtual vsync clock before queueing.
struct PostedFrame {
    frame_time_nanos: i64,
    cb: unsafe extern "C" fn(i64, *mut c_void),
    data: *mut c_void,
}

// SAFETY: the pointers are the guest's callback/data, tied to the looper's
// lifetime; the queue is mutex-protected and only invoked on a polling thread.
unsafe impl Send for PostedFrame {}

struct Looper {
    id: u64,
    epoll: c_int,
    wake: c_int,
    sources: Mutex<HashMap<c_int, Source>>,
    /// Frame callbacks posted by `AChoreographer_postFrameCallback` on this
    /// thread's looper, delivered from `pollOnce`/`pollAll` (never inline from
    /// the post). This is what keeps the engine's frame state consistent: real
    /// Android runs the callback from the looper, after the post returns.
    posted: Mutex<VecDeque<PostedFrame>>,
    refs: AtomicUsize,
}

// SAFETY: file descriptors and mutex-protected state; guest pointers are only
// dereferenced by the guest callback itself on the polling thread.
unsafe impl Send for Looper {}
unsafe impl Sync for Looper {}

fn registry() -> &'static Mutex<HashMap<u64, Arc<Looper>>> {
    static R: OnceLock<Mutex<HashMap<u64, Arc<Looper>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_id() -> u64 {
    static N: AtomicUsize = AtomicUsize::new(0);
    N.fetch_add(1, Ordering::SeqCst) as u64 + 1
}

/// W113-A looper/liveness trace, compiled but inert by default.
///
/// Flip to `true` to log every control-plane event (prepare/addFd/pollOnce
/// non-timeout/wake with the thread id) to stderr. It is **off** in the
/// committed configuration: the trace changes thread timing enough to expose a
/// latent engine race in `VsyncWaiter::ScheduleSecondaryCallback`
/// (`libflutter.so+0x9cffee`, a corrupted vtable pointer), recorded in
/// `results/flutter/FIRST_FRAME.md` (W113-A §4).
const TRACE: bool = false;

/// Bounded diagnostic trace; names the thread and looper of every event.
fn trace(args: std::fmt::Arguments<'_>) {
    if !TRACE {
        return;
    }
    static N: AtomicUsize = AtomicUsize::new(0);
    if N.fetch_add(1, Ordering::Relaxed) < 400 {
        eprintln!(
            "ACOS W113A looper tid={:?} {args}",
            std::thread::current().id()
        );
    }
}

thread_local! {
    static THREAD_LOOPER: RefCell<Option<u64>> = const { RefCell::new(None) };
}

fn make_looper(id: u64) -> Arc<Looper> {
    // SAFETY: plain syscalls; every failure is reported to the caller.
    let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    let wake = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if epoll >= 0 && wake >= 0 {
        // Register the wake eventfd with the epoll set. Without this the
        // `ALooper_wake` write lands on an fd nobody is waiting on, so a wake
        // could never interrupt a blocked `pollOnce` (it silently degraded to a
        // full-timeout wait). `poll` recognises `u64 == wake` and drains it.
        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: wake as u64,
        };
        // SAFETY: both fds were just created successfully and are ours.
        unsafe {
            libc::epoll_ctl(epoll, libc::EPOLL_CTL_ADD, wake, &mut ev);
        }
    }
    Arc::new(Looper {
        id,
        epoll,
        wake,
        sources: Mutex::new(HashMap::new()),
        posted: Mutex::new(VecDeque::new()),
        refs: AtomicUsize::new(1),
    })
}

fn lookup(p: *mut c_void) -> Option<Arc<Looper>> {
    let id = super::handle_id(p)?;
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .cloned()
}

fn register(id: u64, looper: Arc<Looper>) {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, looper);
}

/// `ALooper_prepare(int opts)` — the calling thread's looper.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn alooper_prepare(_opts: c_int) -> *mut c_void {
    if let Some(id) = THREAD_LOOPER.with(|t| *t.borrow()) {
        if let Some(l) = lookup(super::handle(id)) {
            l.refs.fetch_add(1, Ordering::SeqCst);
            return super::handle(id);
        }
    }
    let id = next_id();
    let looper = make_looper(id);
    if looper.epoll < 0 || looper.wake < 0 {
        super::unsupported::<*mut c_void>(
            "libandroid",
            "ALooper_prepare",
            "epoll/eventfd creation failed",
            std::ptr::null_mut(),
        );
        return std::ptr::null_mut();
    }
    register(id, looper);
    THREAD_LOOPER.with(|t| *t.borrow_mut() = Some(id));
    trace(format_args!("prepare id={id}"));
    super::handle(id)
}

/// `ALooper_forThread()` — the calling thread's looper, or NULL.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn alooper_forthread() -> *mut c_void {
    match THREAD_LOOPER.with(|t| *t.borrow()) {
        Some(id) => super::handle(id),
        None => std::ptr::null_mut(),
    }
}

fn epoll_mask(events: c_int) -> u32 {
    let mut m = 0u32;
    if events & ALOOPER_EVENT_INPUT != 0 {
        m |= libc::EPOLLIN as u32;
    }
    if events & ALOOPER_EVENT_OUTPUT != 0 {
        m |= libc::EPOLLOUT as u32;
    }
    // bionic always reports error/hangup.
    m | libc::EPOLLERR as u32 | libc::EPOLLHUP as u32
}

fn out_events(flags: u32) -> c_int {
    let mut e = 0;
    if flags & libc::EPOLLIN as u32 != 0 {
        e |= ALOOPER_EVENT_INPUT;
    }
    if flags & libc::EPOLLOUT as u32 != 0 {
        e |= ALOOPER_EVENT_OUTPUT;
    }
    if flags & libc::EPOLLERR as u32 != 0 {
        e |= ALOOPER_EVENT_ERROR;
    }
    if flags & libc::EPOLLHUP as u32 != 0 {
        e |= ALOOPER_EVENT_HANGUP;
    }
    e
}

/// `ALooper_addFd(ALooper*, int fd, int ident, int events, ALooper_callbackFunc, void*)`.
///
/// # Safety
/// `looper` must be a live `ALooper`; `fd` must be a live file descriptor.
pub unsafe extern "C" fn alooper_addfd(
    looper: *mut c_void,
    fd: c_int,
    ident: c_int,
    events: c_int,
    callback: ALooperCallback,
    data: *mut c_void,
) -> c_int {
    let Some(l) = lookup(looper) else { return -1 };
    if fd < 0 {
        return -1;
    }
    if callback.is_none() && events == 0 {
        // bionic allows a NULL callback only for ALOOPER_PREPARE_ALLOW_NON_CALLBACKS
        // and requires an ident; both are the caller's contract.
    }
    let mut ev = libc::epoll_event {
        events: epoll_mask(events),
        u64: fd as u64,
    };
    if l.epoll < 0 {
        return -1;
    }
    // SAFETY: valid epoll fd and event pointer.
    if unsafe { libc::epoll_ctl(l.epoll, libc::EPOLL_CTL_ADD, fd, &mut ev) } != 0 {
        trace(format_args!(
            "addfd FAIL looper={:#x} fd={fd} ident={ident} events={events} err={:?}",
            l.id,
            std::io::Error::last_os_error()
        ));
        return -1;
    }
    trace(format_args!(
        "addfd looper={:#x} fd={fd} ident={ident} events={events} cb={}",
        l.id,
        callback.is_some()
    ));
    l.sources
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(fd, Source { ident, callback, data });
    1
}

/// `ALooper_removeFd(ALooper*, int fd)`.
///
/// # Safety
/// `looper` must be a live `ALooper`; `fd` must be a live descriptor.
pub unsafe extern "C" fn alooper_removefd(looper: *mut c_void, fd: c_int) -> c_int {
    let Some(l) = lookup(looper) else { return -1 };
    let removed = l
        .sources
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&fd)
        .is_some();
    if !removed {
        return -1;
    }
    // SAFETY: valid epoll fd; removing an fd we registered.
    if unsafe { libc::epoll_ctl(l.epoll, libc::EPOLL_CTL_DEL, fd, std::ptr::null_mut()) } != 0 {
        return -1;
    }
    1
}

fn drain_wake(l: &Looper) {
    let mut v = 0u64;
    // SAFETY: wake is an eventfd owned by this looper.
    unsafe {
        libc::read(l.wake, &mut v as *mut u64 as *mut c_void, 8);
    }
}

/// Wait for readiness. Returns the ready fd sources **and** whether the wake
/// eventfd fired: bionic reports `ALOOPER_POLL_WAKE` for [`alooper_wake`], which
/// is not a timeout even though no fd source was ready.
fn poll(l: &Looper, timeout_ms: c_int) -> Result<(Vec<(c_int, c_int)>, bool), c_int> {
    if l.epoll < 0 {
        return Err(ALOOPER_POLL_ERROR);
    }
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 32];
    // SAFETY: valid epoll fd and a 32-entry output array.
    let n = unsafe {
        libc::epoll_wait(
            l.epoll,
            events.as_mut_ptr(),
            events.len() as c_int,
            timeout_ms,
        )
    };
    if n < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            // A signal is not a wake in bionic's sense; keep it a timeout so
            // `pollAll` recomputes its remaining budget.
            return Ok((Vec::new(), false));
        }
        return Err(ALOOPER_POLL_ERROR);
    }
    let mut ready = Vec::new();
    let mut woke = false;
    for ev in events.iter().take(n as usize) {
        let fd = ev.u64 as c_int;
        if fd == l.wake {
            drain_wake(l);
            woke = true;
            continue;
        }
        let raw = ev.events;
        trace(format_args!(
            "epoll-ready looper={:#x} fd={fd} raw={raw:#x}",
            l.id
        ));
        ready.push((fd, out_events(ev.events)));
    }
    Ok((ready, woke))
}

/// Invoke at most one queued `AChoreographer` frame callback.
///
/// Returns `true` when a callback ran. The callback is invoked on the calling
/// (polling) thread, exactly as bionic's `AChoreographer` does — never from
/// [`post_frame_callback`], which only enqueues.
fn run_one_posted(l: &Looper) -> bool {
    let next = l
        .posted
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pop_front();
    match next {
        Some(p) => {
            static TRACED: AtomicUsize = AtomicUsize::new(0);
            if TRACED.fetch_add(1, Ordering::Relaxed) < 16 {
                eprintln!(
                    "ACOS W108A choreographer_deliver tid={:?} frame_time={} cb={:p} data={:p}",
                    std::thread::current().id(),
                    p.frame_time_nanos,
                    p.cb as *const c_void,
                    p.data
                );
            }
            // SAFETY: the guest registered this C callback for the frame and
            // supplied the opaque data pointer; the NDK contract ties both to
            // the looper's lifetime.
            unsafe { (p.cb)(p.frame_time_nanos, p.data) };
            true
        }
        None => false,
    }
}

/// Queue a frame callback on the calling thread's looper.
///
/// This is the native `AChoreographer` delivery path: the callback is *not*
/// invoked here. It runs later, from this thread's `ALooper_pollOnce` /
/// `ALooper_pollAll` (the engine's own message loop) with `frame_time_nanos`,
/// which is what real Android does. Returns `false` when the calling thread has
/// no prepared `ALooper`; the caller then decides how to surface the frame.
///
/// # Safety
/// `cb` must be a valid C function pointer and `data` the opaque pointer the
/// guest registered with `AChoreographer_postFrameCallback`; both stay valid
/// until the callback is invoked.
pub unsafe fn post_frame_callback(
    frame_time_nanos: i64,
    cb: unsafe extern "C" fn(i64, *mut c_void),
    data: *mut c_void,
) -> bool {
    let Some(l) = THREAD_LOOPER
        .with(|t| *t.borrow())
        .and_then(|id| lookup(super::handle(id)))
    else {
        return false;
    };
    // Bounded diagnostic trace: proves the post is queued (not inline) and
    // names the thread looper that will deliver it.
    static TRACED: AtomicUsize = AtomicUsize::new(0);
    if TRACED.fetch_add(1, Ordering::Relaxed) < 16 {
        eprintln!(
            "ACOS W108A choreographer_post tid={:?} looper={:#x} frame_time={frame_time_nanos} \
             cb={:p} data={:p}",
            std::thread::current().id(),
            l.id,
            cb as *const c_void,
            data
        );
    }
    l.posted
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(PostedFrame {
            frame_time_nanos,
            cb,
            data,
        });
    // Wake a blocked poll so the frame is not delayed a full timeout.
    if l.wake >= 0 {
        let v = 1u64;
        // SAFETY: eventfd owned by this looper, non-blocking.
        unsafe {
            libc::write(l.wake, &v as *const u64 as *const c_void, 8);
        }
    }
    true
}

fn poll_once(l: &Looper, timeout_ms: c_int, out: &mut (c_int, c_int, *mut c_void)) -> c_int {
    // A frame callback queued before the wait must not sit behind the timeout.
    if run_one_posted(l) {
        *out = (-1, 0, std::ptr::null_mut());
        return ALOOPER_POLL_CALLBACK;
    }
    let (ready, woke) = match poll(l, timeout_ms) {
        Ok(r) => r,
        Err(code) => return code,
    };
    if ready.is_empty() {
        // The wait may have been woken by `post_frame_callback` (the wake fd is
        // drained by `poll`), so re-check the queue before reporting a wake.
        if run_one_posted(l) {
            *out = (-1, 0, std::ptr::null_mut());
            return ALOOPER_POLL_CALLBACK;
        }
        // bionic: `ALooper_wake` is `ALOOPER_POLL_WAKE`, never a timeout. This
        // matters because callers treat TIMEOUT as "nothing happened" and
        // re-arm, while WAKE tells them to re-evaluate their task queue.
        return if woke {
            ALOOPER_POLL_WAKE
        } else {
            ALOOPER_POLL_TIMEOUT
        };
    }
    let mut invoked_callback = false;
    for (fd, events) in ready {
        let source = {
            let sources = l.sources.lock().unwrap_or_else(|e| e.into_inner());
            sources.get(&fd).map(|s| (s.ident, s.callback, s.data))
        };
        match source {
            Some((_ident, Some(cb), data)) => {
                out.0 = fd;
                out.1 = events;
                out.2 = data;
                // SAFETY: the guest registered this callback for this looper.
                let keep = unsafe { cb(fd, events, data) };
                invoked_callback = true;
                if keep == 0 {
                    // SAFETY: unregistering a descriptor we own.
                    unsafe {
                        alooper_removefd(super::handle(l.id), fd);
                    }
                }
            }
            Some((ident, None, data)) => {
                out.0 = fd;
                out.1 = events;
                out.2 = data;
                if ident != 0 {
                    return ident;
                }
            }
            None => {}
        }
    }
    if invoked_callback {
        ALOOPER_POLL_CALLBACK
    } else if woke {
        // Only the wake fd fired alongside non-callback sources.
        ALOOPER_POLL_WAKE
    } else {
        ALOOPER_POLL_TIMEOUT
    }
}

/// `ALooper_pollOnce(int timeoutMillis, int* outFd, int* outEvents, void** outData)`.
///
/// # Safety
/// Output pointers may be NULL or writable for their documented types.
pub unsafe extern "C" fn alooper_pollonce(
    timeout_ms: c_int,
    out_fd: *mut c_int,
    out_events: *mut c_int,
    out_data: *mut *mut c_void,
) -> c_int {
    let Some(l) = THREAD_LOOPER
        .with(|t| *t.borrow())
        .and_then(|id| lookup(super::handle(id)))
    else {
        return ALOOPER_POLL_ERROR;
    };
    let mut out = (0, 0, std::ptr::null_mut());
    let r = poll_once(&l, timeout_ms, &mut out);
    if r != ALOOPER_POLL_TIMEOUT {
        trace(format_args!(
            "pollOnce looper={:#x} timeout={timeout_ms} -> {r} fd={} events={}",
            l.id, out.0, out.1
        ));
    }
    if !out_fd.is_null() {
        out_fd.write(out.0);
    }
    if !out_events.is_null() {
        out_events.write(out.1);
    }
    if !out_data.is_null() {
        out_data.write(out.2);
    }
    r
}

/// `ALooper_pollAll(int timeoutMillis, int* outFd, int* outEvents, void** outData)`.
///
/// # Safety
/// Same contract as [`alooper_pollonce`].
pub unsafe extern "C" fn alooper_pollall(
    timeout_ms: c_int,
    out_fd: *mut c_int,
    out_events: *mut c_int,
    out_data: *mut *mut c_void,
) -> c_int {
    let mut remaining = timeout_ms;
    loop {
        let start = std::time::Instant::now();
        let r = alooper_pollonce(remaining, out_fd, out_events, out_data);
        if r != ALOOPER_POLL_TIMEOUT && r != ALOOPER_POLL_CALLBACK {
            return r;
        }
        if r == ALOOPER_POLL_CALLBACK && !out_fd.is_null() {
            // bionic's pollAll stops as soon as a non-callback event is
            // available; a callback was invoked, so report it and return.
            return r;
        }
        if timeout_ms < 0 {
            continue;
        }
        let spent = start.elapsed().as_millis() as c_int;
        remaining = (remaining - spent).max(0);
        if remaining == 0 {
            return ALOOPER_POLL_TIMEOUT;
        }
    }
}

/// `ALooper_wake(ALooper*)` — wake a blocked poll on any thread.
///
/// # Safety
/// `looper` must be a live `ALooper`.
pub unsafe extern "C" fn alooper_wake(looper: *mut c_void) {
    let Some(l) = lookup(looper) else { return };
    trace(format_args!("wake looper={:#x}", l.id));
    if l.wake < 0 {
        return;
    }
    let v = 1u64;
    // SAFETY: eventfd owned by this looper, non-blocking.
    unsafe {
        libc::write(l.wake, &v as *const u64 as *const c_void, 8);
    }
}

/// `ALooper_acquire(ALooper*)`.
///
/// # Safety
/// `looper` must be a live `ALooper`.
pub unsafe extern "C" fn alooper_acquire(looper: *mut c_void) {
    if let Some(l) = lookup(looper) {
        l.refs.fetch_add(1, Ordering::SeqCst);
    }
}

/// `ALooper_release(ALooper*)`.
///
/// # Safety
/// `looper` must be a live `ALooper`; the last release frees it.
pub unsafe extern "C" fn alooper_release(looper: *mut c_void) {
    let Some(id) = super::handle_id(looper) else { return };
    let Some(l) = lookup(looper) else { return };
    if l.refs.fetch_sub(1, Ordering::SeqCst) != 1 {
        return;
    }
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    THREAD_LOOPER.with(|t| {
        if *t.borrow() == Some(id) {
            *t.borrow_mut() = None;
        }
    });
    // SAFETY: this looper's own descriptors.
    unsafe {
        libc::close(l.epoll);
        libc::close(l.wake);
    }
}

/// Table entries for `ALooper`.
pub fn table() -> Vec<(&'static str, *const c_void)> {
    vec![
        ("ALooper_prepare", alooper_prepare as *const c_void),
        ("ALooper_forThread", alooper_forthread as *const c_void),
        ("ALooper_addFd", alooper_addfd as *const c_void),
        ("ALooper_removeFd", alooper_removefd as *const c_void),
        ("ALooper_pollOnce", alooper_pollonce as *const c_void),
        ("ALooper_pollAll", alooper_pollall as *const c_void),
        ("ALooper_wake", alooper_wake as *const c_void),
        ("ALooper_acquire", alooper_acquire as *const c_void),
        ("ALooper_release", alooper_release as *const c_void),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Run one poll on `looper` and capture the out-parameters.
    fn poll1(_looper: *mut c_void, timeout_ms: c_int) -> (c_int, c_int, c_int, *mut c_void) {
        let mut fd = 0;
        let mut events = 0;
        let mut data = std::ptr::null_mut();
        // SAFETY: looper from `alooper_prepare`, out-pointers are local.
        let r = unsafe { alooper_pollonce(timeout_ms, &mut fd, &mut events, &mut data) };
        (r, fd, events, data)
    }

    #[test]
    fn poll_once_times_out_without_events() {
        // SAFETY: prepare gives this test thread a looper of its own.
        unsafe {
            assert!(!alooper_prepare(0).is_null());
        }
        // SAFETY: the looper handle was created above on this thread.
        let looper = unsafe { alooper_forthread() };
        let (r, _, _, _) = poll1(looper, 20);
        assert_eq!(r, ALOOPER_POLL_TIMEOUT);
    }

    #[test]
    fn alooper_wake_reports_poll_wake_not_timeout() {
        // SAFETY: the handle is only passed back to the shim, which validates it.
        unsafe {
            let l = alooper_prepare(0);
            assert!(!l.is_null());
            let l_addr = l as usize;
            let waker = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(25));
                alooper_wake(l_addr as *mut c_void);
            });
            let start = Instant::now();
            let (r, _, _, _) = poll1(l, 2000);
            waker.join().unwrap();
            assert_eq!(r, ALOOPER_POLL_WAKE, "ALooper_wake must not look like a timeout");
            assert!(start.elapsed() < Duration::from_millis(1500));
        }
    }

    #[test]
    fn fd_callback_reports_poll_callback() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn cb(_fd: c_int, _events: c_int, _data: *mut c_void) -> c_int {
            CALLS.fetch_add(1, Ordering::SeqCst);
            1 // keep registered
        }
        // SAFETY: shim calls with a real pipe.
        unsafe {
            let l = alooper_prepare(0);
            let mut fds = [0i32; 2];
            assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
            assert_eq!(alooper_addfd(l, fds[0], 0, 1, Some(cb), std::ptr::null_mut()), 1);
            let b = b"x";
            libc::write(fds[1], b.as_ptr() as *const c_void, 1);
            let (r, fd, events, _) = poll1(l, 1000);
            assert_eq!(r, ALOOPER_POLL_CALLBACK);
            assert_eq!(fd, fds[0]);
            assert_eq!(events, 1);
            assert_eq!(CALLS.load(Ordering::SeqCst), 1);
            let mut buf = [0u8; 1];
            libc::read(fds[0], buf.as_mut_ptr() as *mut c_void, 1);
            alooper_removefd(l, fds[0]);
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn posted_frame_runs_from_poll_once() {
        static FRAMES: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn frame_cb(_frame_time: i64, _data: *mut c_void) {
            FRAMES.fetch_add(1, Ordering::SeqCst);
        }
        // SAFETY: the callback outlives the poll below.
        unsafe {
            assert!(!alooper_prepare(0).is_null());
            assert!(post_frame_callback(16_666_667, frame_cb, std::ptr::null_mut()));
            let looper = alooper_forthread();
            let (r, _, _, _) = poll1(looper, 0);
            assert_eq!(r, ALOOPER_POLL_CALLBACK);
            assert_eq!(FRAMES.load(Ordering::SeqCst), 1);
        }
    }
}
