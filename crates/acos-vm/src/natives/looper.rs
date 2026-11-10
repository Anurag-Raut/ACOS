//! `android.os.MessageQueue` natives backed by a real `epoll` + `eventfd`.
//!
//! The VM runs cooperative green threads on a single host thread, so a blocking
//! `nativePollOnce` cannot simply park the host: it must yield to the other
//! green threads and only block the host once every runnable thread has parked.
//! The split is:
//!
//! * [`native_poll_once`] marks the calling green thread [`ThreadStatus::Parked`]
//!   and returns. The native frame is popped by the interpreter as usual, so
//!   when the thread is woken it resumes *after* the `nativePollOnce` call in
//!   `MessageQueue.next()` and re-reads the Java queue.
//! * [`pump`] is called by [`crate::vm::Vm::run_inner`] whenever the interpreter
//!   scheduler runs out of runnable threads. It blocks the host on the process
//!   `epoll` set with the earliest parked deadline and wakes the corresponding
//!   green thread. With no messages and `-1` timeout this is a single
//!   `epoll_wait(2)` with no timeout: zero CPU and no periodic wakeups.
//!
//! All file descriptors live in a per-host-thread process registry. A
//! [`LooperHandle`] is a reference-counted handle to one MessageQueue's
//! `eventfd`/`epoll` pair; cloning it and calling [`LooperHandle::wake`] from a
//! Binder/vsync host thread is how those subsystems schedule the looper.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustix::event::epoll::{self, CreateFlags, Event, EventData, EventFlags};
use rustix::event::{eventfd, EventfdFlags};
use rustix::io::Errno;
use rustix::time::Timespec;

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::vm::{ThreadStatus, Vm};

// Process-wide (per host thread) looper registry. One VM runs on one host
// thread, so this keeps parallel tests from sharing an `epoll` set.
thread_local! {
    static PROC: RefCell<Proc> = RefCell::new(Proc::new());
}

const EPOLL_BATCH: usize = 32;

/// Shared file descriptor state for one MessageQueue. The `Arc` is what makes
/// [`LooperHandle::wake`] usable from a non-VM host thread.
#[derive(Debug)]
struct Shared {
    id: u64,
    epoll: Arc<OwnedFd>,
    wake_fd: Arc<OwnedFd>,
    /// Extra fds registered through [`LooperHandle::add_fd`], keyed by raw fd.
    registrations: Mutex<HashMap<RawFd, Vec<u64>>>,
}

/// A sendable handle to one `MessageQueue`'s native looper.
///
/// `Binder` delivery, vsync timers and input plumbing can clone this and call
/// [`LooperHandle::wake`] from any thread to schedule the owning looper.
#[derive(Clone, Debug)]
pub struct LooperHandle {
    shared: Arc<Shared>,
}

impl LooperHandle {
    /// The opaque pointer value handed to Java as `MessageQueue.mPtr`.
    pub fn id(&self) -> u64 {
        self.shared.id
    }

    /// Wake the looper. Safe to call from any thread; a no-overflow `eventfd`
    /// write that the parked process `epoll_wait` observes.
    pub fn wake(&self) -> io::Result<()> {
        let one: u64 = 1;
        rustix::io::write(&*self.shared.wake_fd, &one.to_ne_bytes())?;
        Ok(())
    }

    /// Register `fd` with the process `epoll` set and remember `callback_id`.
    /// Re-registering an fd replaces its callback list.
    pub fn add_fd<Fd: AsFd>(
        &self,
        fd: Fd,
        events: EventFlags,
        callback_id: u64,
    ) -> io::Result<()> {
        let raw = fd.as_fd().as_raw_fd();
        epoll::add(
            &*self.shared.epoll,
            fd,
            EventData::new_u64(self.shared.id),
            events | EventFlags::IN,
        )?;
        self.shared
            .registrations
            .lock()
            .expect("looper registrations mutex")
            .insert(raw, vec![callback_id]);
        Ok(())
    }

    /// Remove `fd` from the process `epoll` set.
    pub fn remove_fd<Fd: AsFd>(&self, fd: Fd) -> io::Result<()> {
        let raw = fd.as_fd().as_raw_fd();
        let _ = epoll::delete(&*self.shared.epoll, fd);
        self.shared
            .registrations
            .lock()
            .expect("looper registrations mutex")
            .remove(&raw);
        Ok(())
    }

    /// Block the *calling host thread* on the process `epoll` set until this
    /// looper is woken or `timeout` elapses. Returns `true` when an event for
    /// this looper arrived. Used by Rust services that want synchronous waits
    /// and by the looper unit tests.
    pub fn wait(&self, timeout: Option<Duration>) -> bool {
        self.wait_inner(timeout)
    }

    fn wait_inner(&self, timeout: Option<Duration>) -> bool {
        // One `epoll_wait` batch: the event data is the looper id, so we only
        // take our own events. Other loopers share the process epoll; their
        // pending events stay queued for their own `pump`/`wait` (draining them
        // here would steal their wake, and re-waiting would busy-spin on them).
        let mut events = empty_events();
        let ts = timeout.map(timespec);
        let n = wait_epoll(&self.shared.epoll, &mut events, ts.as_ref());
        let hit = events[..n]
            .iter()
            .any(|ev| ev.data.u64() == self.shared.id);
        if hit {
            drain_wake(&self.shared.wake_fd);
        }
        hit
    }
}

/// Create a looper in the current host thread's process registry.
pub fn create_looper() -> io::Result<LooperHandle> {
    PROC.with(|p| p.borrow_mut().create_looper())
}

/// Destroy a looper previously created by [`create_looper`] or `nativeInit`.
pub fn destroy_looper(id: u64) {
    PROC.with(|p| {
        p.borrow_mut().loopers.remove(&id);
    });
}

/// Look up the looper created for an opaque `MessageQueue.mPtr` value. Only
/// valid on the host thread that runs the VM.
pub fn handle_for_ptr(ptr: u64) -> Option<LooperHandle> {
    PROC.with(|p| p.borrow().loopers.get(&ptr).map(|l| l.handle.clone()))
}

#[derive(Debug)]
struct LooperState {
    handle: LooperHandle,
    parked_thread: Option<usize>,
    deadline: Option<Instant>,
}

#[derive(Debug)]
struct Proc {
    epoll: Option<Arc<OwnedFd>>,
    loopers: HashMap<u64, LooperState>,
    next_id: u64,
}

impl Proc {
    fn new() -> Proc {
        Proc {
            epoll: None,
            loopers: HashMap::new(),
            next_id: 0,
        }
    }

    fn epoll(&mut self) -> io::Result<Arc<OwnedFd>> {
        if let Some(fd) = &self.epoll {
            return Ok(fd.clone());
        }
        let fd = Arc::new(epoll::create(CreateFlags::CLOEXEC)?);
        self.epoll = Some(fd.clone());
        Ok(fd)
    }

    fn create_looper(&mut self) -> io::Result<LooperHandle> {
        let epoll = self.epoll()?;
        let wake_fd = eventfd(0, EventfdFlags::NONBLOCK | EventfdFlags::CLOEXEC)?;
        let wake_fd = Arc::new(wake_fd);
        self.next_id += 1;
        let id = self.next_id;
        epoll::add(&*epoll, &*wake_fd, EventData::new_u64(id), EventFlags::IN)?;
        let shared = Arc::new(Shared {
            id,
            epoll,
            wake_fd,
            registrations: Mutex::new(HashMap::new()),
        });
        let handle = LooperHandle { shared };
        self.loopers.insert(
            id,
            LooperState {
                handle: handle.clone(),
                parked_thread: None,
                deadline: None,
            },
        );
        Ok(handle)
    }
}

/// The scheduler hook: called when the interpreter has no runnable green
/// thread left. Blocks the host on the process `epoll` set until a parked
/// looper is woken or its deadline expires. Returns `true` if it made
/// progress (so [`crate::vm::Vm::run_inner`] should re-enter the scheduler).
pub(crate) fn pump(vm: &mut Vm) -> VmResult<bool> {
    // A run whose only thread is parked here must still obey the host
    // watchdog; check before blocking and again when the wait returns.
    vm.host.check()?;
    let now = Instant::now();
    let mut expired: Vec<usize> = Vec::new();
    let mut next_deadline: Option<Instant> = None;
    PROC.with(|p| {
        let p = p.borrow();
        for l in p.loopers.values() {
            match (l.parked_thread, l.deadline) {
                (Some(t), Some(d)) if d <= now => expired.push(t),
                (Some(_), Some(d)) => {
                    next_deadline = Some(match next_deadline {
                        Some(m) => m.min(d),
                        None => d,
                    });
                }
                _ => {}
            }
        }
    });

    if !expired.is_empty() {
        for t in &expired {
            if let Some(th) = vm.threads.get_mut(*t) {
                if th.status == ThreadStatus::Parked {
                    th.status = ThreadStatus::Runnable;
                }
            }
        }
        PROC.with(|p| {
            let mut p = p.borrow_mut();
            for l in p.loopers.values_mut() {
                if l.parked_thread.map(|t| expired.contains(&t)).unwrap_or(false) {
                    l.parked_thread = None;
                    l.deadline = None;
                }
            }
        });
        return Ok(true);
    }

    let epoll_fd = PROC.with(|p| {
        let p = p.borrow();
        if !p.loopers.values().any(|l| l.parked_thread.is_some()) {
            return None;
        }
        p.epoll.clone()
    });
    let Some(epoll_fd) = epoll_fd else {
        return Ok(false);
    };

    // Block no longer than the earliest parked deadline *or* the remaining
    // wall-clock budget, so a permanently parked looper cannot hang the run
    // past `ACOS_WALL_MAX_SECS`.
    let deadline_wait = next_deadline.and_then(|d| d.checked_duration_since(Instant::now()));
    let timeout = match (deadline_wait, vm.host.remaining_wall()) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    let mut events = empty_events();
    let ts = timeout.map(timespec);
    let n = wait_epoll(&epoll_fd, &mut events, ts.as_ref());

    let mut woken: Vec<usize> = Vec::new();
    for ev in &events[..n] {
        let id = ev.data.u64();
        PROC.with(|p| {
            let p = p.borrow();
            if let Some(l) = p.loopers.get(&id) {
                if let Some(t) = l.parked_thread {
                    // Only mark once per event batch.
                    if !woken.contains(&t) {
                        woken.push(t);
                    }
                }
                drain_wake(&l.handle.shared.wake_fd);
            }
        });
    }
    for t in &woken {
        if let Some(th) = vm.threads.get_mut(*t) {
            if th.status == ThreadStatus::Parked {
                th.status = ThreadStatus::Runnable;
            }
        }
    }
    if !woken.is_empty() {
        PROC.with(|p| {
            let mut p = p.borrow_mut();
            for l in p.loopers.values_mut() {
                if l.parked_thread.map(|t| woken.contains(&t)).unwrap_or(false) {
                    l.parked_thread = None;
                    l.deadline = None;
                }
            }
        });
    }
    vm.host.check()?;
    // Even on a pure timeout (`n == 0`) loop again so the deadline check runs.
    Ok(true)
}

// ---------------------------------------------------------------------------
// Native methods
// ---------------------------------------------------------------------------

/// Register the `android.os.MessageQueue` natives (plus the small `Binder`
/// caller-identity shims `Looper.loop` needs).
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let mq = "Landroid/os/MessageQueue;";
    vm.register_native(mq, "nativeInit", "()J", native_init);
    vm.register_native(mq, "nativeDestroy", "(J)V", native_destroy);
    vm.register_native(mq, "nativePollOnce", "(JI)V", native_poll_once);
    vm.register_native(mq, "nativeWake", "(J)V", native_wake);
    vm.register_native(mq, "nativeIsPolling", "(J)Z", native_is_polling);
    vm.register_native(
        mq,
        "nativeSetFileDescriptorEvents",
        "(JII)V",
        native_set_file_descriptor_events,
    );

    // The `android.os.Binder` caller-identity / command-queue natives now live
    // in `natives/binder.rs` (T-19), backed by the real transport. They used to
    // be registered here as no-op shims so `Looper.loop`'s
    // `clearCallingIdentity`/`restoreCallingIdentity` wrapper could run; the
    // full Binder native set replaces them.
    Ok(())
}

fn native_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    match create_looper() {
        Ok(h) => {
            vm.ret_i64(ctx, h.id() as i64);
            Ok(())
        }
        Err(e) => Err(vm.throw_new(
            "Ljava/lang/RuntimeException;",
            Some(&format!("nativeInit: {e}")),
        )),
    }
}

fn native_destroy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ptr = vm.arg_i64(ctx, 0) as u64;
    destroy_looper(ptr);
    Ok(())
}

fn native_poll_once(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let timeout_ms = vm.arg_i32(ctx, 1);
    let t = vm.current_thread;

    let Some(handle) = handle_for_ptr(ptr) else {
        return Err(vm.throw_new(
            "Ljava/lang/RuntimeException;",
            Some("nativePollOnce: unknown MessageQueue pointer"),
        ));
    };

    // A wake that arrived before we parked must not be lost: a non-blocking
    // poll observes it and returns immediately.
    if handle.wait(Some(Duration::ZERO)) {
        // An input fd registered through `LooperHandle::add_fd` is readable on
        // the same looper id; dispatch it on this (the looper) green thread.
        let _ = crate::natives::fwk_input::on_looper_wake(vm, ptr);
        return Ok(());
    }
    if timeout_ms == 0 {
        return Ok(());
    }

    let deadline = if timeout_ms < 0 {
        None
    } else {
        Some(Instant::now() + Duration::from_millis(timeout_ms as u64))
    };
    PROC.with(|p| {
        let mut p = p.borrow_mut();
        if let Some(l) = p.loopers.get_mut(&ptr) {
            l.parked_thread = Some(t);
            l.deadline = deadline;
        }
    });
    vm.threads[t].status = ThreadStatus::Parked;
    Ok(())
}

fn native_wake(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let Some(handle) = handle_for_ptr(ptr) else {
        return Err(vm.throw_new(
            "Ljava/lang/RuntimeException;",
            Some("nativeWake: unknown MessageQueue pointer"),
        ));
    };
    if let Err(e) = handle.wake() {
        return Err(vm.throw_new(
            "Ljava/lang/RuntimeException;",
            Some(&format!("nativeWake: {e}")),
        ));
    }
    // If the looper's green thread is parked in this VM, wake it immediately so
    // the interpreter picks it without waiting for the next scheduler idle.
    let parked = PROC.with(|p| {
        p.borrow()
            .loopers
            .get(&ptr)
            .and_then(|l| l.parked_thread)
    });
    if let Some(t) = parked {
        if let Some(th) = vm.threads.get_mut(t) {
            if th.status == ThreadStatus::Parked {
                th.status = ThreadStatus::Runnable;
            }
        }
        PROC.with(|p| {
            if let Some(l) = p.borrow_mut().loopers.get_mut(&ptr) {
                l.parked_thread = None;
                l.deadline = None;
            }
        });
    }
    Ok(())
}

fn native_is_polling(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let polling = PROC.with(|p| {
        p.borrow()
            .loopers
            .get(&ptr)
            .map(|l| l.parked_thread.is_some())
            .unwrap_or(false)
    });
    vm.ret_bool(ctx, polling);
    Ok(())
}

fn native_set_file_descriptor_events(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let fd: RawFd = vm.arg_i32(ctx, 1);
    let events = vm.arg_i32(ctx, 2);
    let Some(handle) = handle_for_ptr(ptr) else {
        return Err(vm.throw_new(
            "Ljava/lang/RuntimeException;",
            Some("nativeSetFileDescriptorEvents: unknown MessageQueue pointer"),
        ));
    };
    if fd < 0 {
        return Ok(());
    }
    let result = if events == 0 {
        // The Java side stores the fd in a `FileDescriptor`; a bare raw fd is
        // enough for `epoll_ctl(DEL)`.
        remove_raw_fd(&handle, fd)
    } else {
        add_raw_fd(&handle, fd, events)
    };
    if let Err(e) = result {
        return Err(vm.throw_new(
            "Ljava/lang/RuntimeException;",
            Some(&format!("nativeSetFileDescriptorEvents: {e}")),
        ));
    }
    Ok(())
}

/// Record a Java-side fd request.
///
/// `MessageQueue.nativeSetFileDescriptorEvents` only receives a raw `int` fd, and
/// `acos-vm` is `#![forbid(unsafe_code)]`, so the fd cannot be wrapped in a
/// `BorrowedFd` for `epoll_ctl` here. The request is recorded so the Rust-side
/// [`LooperHandle::add_fd`] API (which takes `AsFd`) can reconcile it once the
/// owning subsystem (input/vsync/Binder) passes a real borrowed fd.
fn add_raw_fd(handle: &LooperHandle, fd: i32, events: i32) -> io::Result<()> {
    let raw = fd as RawFd;
    handle
        .shared
        .registrations
        .lock()
        .expect("looper registrations mutex")
        .insert(raw, vec![events as u64]);
    Ok(())
}

fn remove_raw_fd(handle: &LooperHandle, fd: i32) -> io::Result<()> {
    let raw = fd as RawFd;
    handle
        .shared
        .registrations
        .lock()
        .expect("looper registrations mutex")
        .remove(&raw);
    Ok(())
}

// ---------------------------------------------------------------------------
// epoll helpers
// ---------------------------------------------------------------------------

fn empty_events() -> [Event; EPOLL_BATCH] {
    [Event {
        flags: EventFlags::empty(),
        data: EventData::new_u64(0),
    }; EPOLL_BATCH]
}

fn timespec(d: Duration) -> Timespec {
    Timespec {
        tv_sec: d.as_secs() as i64,
        tv_nsec: d.subsec_nanos() as _,
    }
}

fn wait_epoll(epoll_fd: &OwnedFd, events: &mut [Event], timeout: Option<&Timespec>) -> usize {
    loop {
        match epoll::wait(epoll_fd, &mut *events, timeout) {
            Ok(n) => return n,
            Err(Errno::INTR) => continue,
            Err(_) => return 0,
        }
    }
}

fn drain_wake(wake_fd: &OwnedFd) {
    let mut buf = [0u8; 8];
    let _ = rustix::io::read(wake_fd, &mut buf);
}
