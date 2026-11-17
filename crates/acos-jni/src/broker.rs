//! Brokered JNI for native OS threads (ADR-0021 follow-up).
//!
//! Native engines (Hermes, the Flutter engine, fbjni) call Java from OS threads
//! they created with `pthread_create`. ACOS runs a single, `!Send` VM on one
//! *owner* host thread, so those threads cannot touch the VM directly. This
//! module implements the broker the W54-A ADR left open:
//!
//! * a native OS thread that attached through the process-global slots becomes
//!   a *worker* with a synthetic VM-thread id; every VM-touching JNI slot on
//!   that thread is packaged as a [`Request`] and pushed onto the owner's queue;
//! * the worker then **blocks** on a condition variable until the owner
//!   publishes a reply (or the request times out with
//!   [`JniError::BrokerTimeout`]);
//! * the owner services its queue at VM safe points
//!   ([`acos_vm::natives::safe_point`]): between interpreter batches, in the
//!   looper pump, and — for the synchronous
//!   owner → native → worker → Java → owner pattern — from inside a native
//!   call that explicitly pumps with [`service_current`].
//!
//! # Invariants
//!
//! * **The VM is never touched by two threads at once.** Only the owner
//!   thread runs [`service`], and it only ever executes one request at a time.
//!   The raw `*mut Vm` is never stored in shared state; it lives on the owner's
//!   stack for the duration of one request.
//! * **Worker state lives on the owner.** A worker's local-reference frames,
//!   pending exception and structured error are [`crate::state::ThreadJni`]
//!   records keyed by the worker's synthetic id. Only the owner mutates them,
//!   while servicing that worker's request.
//! * **Re-entrancy is nested, not concurrent.** Servicing a request may run
//!   Java that calls a native, which may pump again; that nested pump runs on
//!   the same owner thread and shares the queue.
//! * **Timeouts fail closed.** A request that is not serviced in time returns
//!   `JNI_BROKER_TIMEOUT` and the slot returns its default; the VM is never
//!   assumed to have made progress.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use acos_vm::natives::safe_point;
use acos_vm::Vm;

use crate::error::JniError;
use crate::state::{current_vm, runtime_state, set_forced_thread, set_vm, State};

/// Default request deadline in milliseconds. Overridable with
/// `ACOS_JNI_BROKER_TIMEOUT_MS`.
const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// One brokered JNI operation. All payloads are plain data so the value may
/// cross threads; no VM pointer, handle table or `Rc` is ever stored here.
#[derive(Debug)]
pub enum Op {
    FindClass {
        name: Vec<u8>,
    },
    NewStringUtf {
        bytes: Vec<u8>,
    },
    NewString {
        units: Vec<u16>,
    },
    MethodCodes {
        mid: usize,
    },
    AllocObject {
        class: usize,
    },
    GetMethodId {
        class: usize,
        name: Vec<u8>,
        sig: Vec<u8>,
        is_static: bool,
    },
    GetFieldId {
        class: usize,
        name: Vec<u8>,
        sig: Vec<u8>,
        is_static: bool,
    },
    CallMethod {
        target: usize,
        mid: usize,
        mode: i32,
        words: Vec<u64>,
    },
    CallNonvirtual {
        obj: usize,
        cls: usize,
        mid: usize,
        words: Vec<u64>,
    },
    GetField {
        obj: usize,
        fid: usize,
    },
    SetField {
        obj: usize,
        fid: usize,
        bits: u64,
    },
    GetStringLength {
        s: usize,
    },
    GetStringUtfChars {
        s: usize,
    },
    ReleaseStringUtfChars {
        s: usize,
        chars: usize,
    },
    ExceptionCheck,
    ExceptionOccurred,
    ExceptionClear,
    PushLocalFrame {
        cap: i32,
    },
    PopLocalFrame {
        result: usize,
    },
    NewGlobalRef {
        obj: usize,
    },
    DeleteGlobalRef {
        obj: usize,
    },
    DeleteLocalRef {
        obj: usize,
    },
    GetObjectClass {
        obj: usize,
    },
    IsSameObject {
        a: usize,
        b: usize,
    },
    Throw {
        obj: usize,
    },
    ThrowNew {
        class: usize,
        message: Vec<u8>,
    },
    GetObjectRefType {
        obj: usize,
    },
    Detach,
}

struct Request {
    worker: usize,
    op: Option<Op>,
    reply: Option<Result<u64, JniError>>,
}

#[derive(Default)]
struct OwnerQueue {
    queue: VecDeque<u64>,
    requests: HashMap<u64, Request>,
    next_request: u64,
}

struct Broker {
    /// The owner host thread of each live runtime, and its request queue.
    owners: HashMap<ThreadId, OwnerQueue>,
    /// Worker ThreadId -> (synthetic id, owner).
    workers: HashMap<ThreadId, (usize, ThreadId)>,
    /// Synthetic ids that have had at least one non-`Detach` request executed.
    /// A worker that never touched the VM can be detached without a round trip.
    used_workers: std::collections::HashSet<usize>,
    next_worker: usize,
    /// How many workers are attached but have not detached yet.
    attached_workers: usize,
}

static BROKER: OnceLock<Mutex<Broker>> = OnceLock::new();
static CV: OnceLock<Condvar> = OnceLock::new();
static OWNER: OnceLock<Mutex<Option<ThreadId>>> = OnceLock::new();

thread_local! {
    static WORKER: Cell<Option<usize>> = const { Cell::new(None) };
    static WORKER_ERROR: RefCell<Option<JniError>> = const { RefCell::new(None) };
}

fn broker() -> &'static Mutex<Broker> {
    BROKER.get_or_init(|| {
        Mutex::new(Broker {
            owners: HashMap::new(),
            workers: HashMap::new(),
            used_workers: std::collections::HashSet::new(),
            next_worker: 0,
            attached_workers: 0,
        })
    })
}

fn condvar() -> &'static Condvar {
    CV.get_or_init(Condvar::new)
}

fn owner_slot() -> &'static Mutex<Option<ThreadId>> {
    OWNER.get_or_init(|| Mutex::new(None))
}

fn timeout() -> Duration {
    let ms = std::env::var("ACOS_JNI_BROKER_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    Duration::from_millis(ms)
}

/// The owner thread id captured for this process, if a runtime exists.
fn global_owner() -> Option<ThreadId> {
    *owner_slot().lock().unwrap_or_else(|e| e.into_inner())
}

/// Record the current thread as a runtime owner. Called by
/// [`crate::state::JniRuntime::new`].
pub(crate) fn set_owner() {
    let mut slot = owner_slot().lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(std::thread::current().id());
    let owner = std::thread::current().id();
    broker()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .owners
        .entry(owner)
        .or_default();
}

/// Forget the owner (the runtime is going away).
pub(crate) fn clear_owner() {
    let mut slot = owner_slot().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(owner) = slot.take() {
        broker()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .owners
            .remove(&owner);
    }
}

/// Register the calling OS thread as a broker worker. Returns its synthetic id.
pub(crate) fn attach_worker() -> usize {
    let owner = global_owner().unwrap_or_else(|| std::thread::current().id());
    attach_worker_to(owner)
}

/// Register the calling OS thread as a worker of `owner`.
///
/// This is the explicit form used by tests and by embedders that already hold
/// the owner id; it bypasses the process-global attach slot (which is only
/// unambiguous when the process hosts a single VM).
pub fn attach_worker_to(owner: ThreadId) -> usize {
    let tid = std::thread::current().id();
    let mut b = broker().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((id, _)) = b.workers.get(&tid) {
        return *id;
    }
    let id = b.next_worker;
    b.next_worker += 1;
    b.attached_workers += 1;
    b.workers.insert(tid, (id, owner));
    b.owners.entry(owner).or_default();
    WORKER.with(|c| c.set(Some(id)));
    id
}

/// The synthetic id of the calling worker, if it attached through the globals.
pub(crate) fn worker_id() -> Option<usize> {
    WORKER.with(Cell::get)
}

/// True when this OS thread must route VM-touching slots through the broker
/// (it is a global-attached worker and has no thread-local runtime).
pub(crate) fn is_worker() -> bool {
    runtime_state().is_none() && worker_id().is_some()
}

/// Number of attached workers that have not detached.
pub fn attached_workers() -> usize {
    broker()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .attached_workers
}

/// Number of queued requests across all owners (test/diagnostic aid).
pub fn pending_requests() -> usize {
    broker()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .owners
        .values()
        .map(|o| o.queue.len())
        .sum()
}

/// Record a structured error on the calling worker.
pub fn set_worker_error(e: JniError) {
    WORKER_ERROR.with(|c| *c.borrow_mut() = Some(e));
}

/// Take the calling worker's last structured error.
pub fn take_worker_error() -> Option<JniError> {
    WORKER_ERROR.with(|c| c.borrow_mut().take())
}

fn worker_lookup(tid: ThreadId) -> Option<(usize, ThreadId)> {
    broker()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .workers
        .get(&tid)
        .copied()
}

/// Submit `op` for the calling worker and block until the owner replies.
pub(crate) fn submit(op: Op, function: &str) -> Result<u64, JniError> {
    let tid = std::thread::current().id();
    let Some((worker, owner)) = worker_lookup(tid) else {
        return Err(JniError::NoVm {
            function: function.to_string(),
        });
    };
    let deadline = Instant::now() + timeout();
    let id = {
        let mut b = broker().lock().unwrap_or_else(|e| e.into_inner());
        let q = b.owners.entry(owner).or_default();
        let id = q.next_request;
        q.next_request += 1;
        q.requests.insert(
            id,
            Request {
                worker,
                op: Some(op),
                reply: None,
            },
        );
        q.queue.push_back(id);
        id
    };
    let mut guard = broker().lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let ready = guard
            .owners
            .get(&owner)
            .and_then(|q| q.requests.get(&id))
            .and_then(|r| r.reply.clone());
        if let Some(reply) = ready {
            if let Some(q) = guard.owners.get_mut(&owner) {
                q.requests.remove(&id);
            }
            drop(guard);
            condvar().notify_all();
            return reply;
        }
        let now = Instant::now();
        if now >= deadline {
            if let Some(q) = guard.owners.get_mut(&owner) {
                q.requests.remove(&id);
                q.queue.retain(|x| *x != id);
            }
            drop(guard);
            return Err(JniError::BrokerTimeout {
                function: function.to_string(),
                millis: timeout().as_millis() as u64,
            });
        }
        let (g, wait) = condvar()
            .wait_timeout(guard, deadline - now)
            .unwrap_or_else(|e| e.into_inner());
        guard = g;
        if wait.timed_out() && Instant::now() >= deadline {
            // Loop once more: the reply could have landed with the timeout.
            let ready = guard
                .owners
                .get(&owner)
                .and_then(|q| q.requests.get(&id))
                .and_then(|r| r.reply.clone());
            if ready.is_none() {
                if let Some(q) = guard.owners.get_mut(&owner) {
                    q.requests.remove(&id);
                    q.queue.retain(|x| *x != id);
                }
                drop(guard);
                return Err(JniError::BrokerTimeout {
                    function: function.to_string(),
                    millis: timeout().as_millis() as u64,
                });
            }
        }
    }
}

/// Enqueue and wait for a VM-touching slot, returning `None` when the calling
/// thread is not a broker worker (the caller then runs the normal path).
pub(crate) fn worker_op(op: Op, function: &str) -> Option<Result<u64, JniError>> {
    if !is_worker() {
        return None;
    }
    Some(submit(op, function))
}

/// Field access helpers routed through the broker.
pub(crate) fn worker_get_field(obj: usize, fid: usize, function: &str) -> Option<u64> {
    match worker_op(Op::GetField { obj, fid }, function) {
        None => None,
        Some(Ok(bits)) => Some(bits),
        Some(Err(e)) => {
            set_worker_error(e);
            Some(0)
        }
    }
}

pub(crate) fn worker_set_field(obj: usize, fid: usize, bits: u64, function: &str) {
    if let Some(Err(e)) = worker_op(Op::SetField { obj, fid, bits }, function) {
        set_worker_error(e);
    }
}

/// Drain and execute the current owner's queued requests. Returns how many
/// were serviced. Called from a VM safe point and from the nested pump.
pub fn service_current() -> usize {
    // SAFETY: `current_vm` is the owner pointer installed by `bind`/the safe
    // point; this function only runs on the owner thread.
    unsafe { service(current_vm()) }
}

/// The safe-point hook installed into the VM.
///
/// The raw pointer contract is the VM's: the hook only runs on the owner
/// thread with its live VM, never concurrently.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn safe_point_hook(vm_ptr: *mut Vm) {
    // SAFETY: the VM invokes this hook on the owner thread with its live
    // pointer; no other thread dereferences it concurrently.
    unsafe { service(vm_ptr) };
}

/// Drain and execute the owner's queued requests against `vm_ptr`.
///
/// # Safety
///
/// `vm_ptr` must be the live owner VM and the caller must be the owner thread.
/// The VM is never handed to a second thread.
pub unsafe fn service(vm_ptr: *mut Vm) -> usize {
    if vm_ptr.is_null() {
        return 0;
    }
    let owner = std::thread::current().id();
    let Some(rt) = runtime_state() else {
        return 0;
    };
    // Keep the thread-local VM pointer consistent for the reused slot code.
    let saved_vm = current_vm();
    // SAFETY: the pointer is the live owner VM for this call; it is restored
    // before returning and never stored in shared state.
    unsafe { set_vm(vm_ptr) };

    let mut serviced = 0usize;
    loop {
        let next = {
            let mut b = broker().lock().unwrap_or_else(|e| e.into_inner());
            match b.owners.get_mut(&owner) {
                Some(q) => q.queue.pop_front(),
                None => None,
            }
        };
        let Some(id) = next else { break };
        let (worker, op) = {
            let mut b = broker().lock().unwrap_or_else(|e| e.into_inner());
            match b
                .owners
                .get_mut(&owner)
                .and_then(|q| q.requests.get_mut(&id))
            {
                Some(req) if req.op.is_some() => (req.worker, req.op.take().unwrap()),
                _ => continue,
            }
        };
        let result = execute(vm_ptr, &rt, worker, op);
        {
            let mut b = broker().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(req) = b
                .owners
                .get_mut(&owner)
                .and_then(|q| q.requests.get_mut(&id))
            {
                req.reply = Some(result);
            }
        }
        condvar().notify_all();
        serviced += 1;
    }

    // SAFETY: restoring the previous pointer, which was valid on this thread.
    unsafe { set_vm(saved_vm) };
    serviced
}

/// Execute one request on the owner thread with the worker's forced thread id.
fn execute(
    vm_ptr: *mut Vm,
    rt: &std::rc::Rc<RefCell<State>>,
    worker: usize,
    op: Op,
) -> Result<u64, JniError> {
    if !matches!(op, Op::Detach) {
        broker()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .used_workers
            .insert(worker);
    }
    // SAFETY: only the owner thread reaches here, once per request; the
    // pointer is the live VM it was handed at a safe point.
    let vm: &mut Vm = unsafe { &mut *vm_ptr };
    let env = rt.borrow().env_slot;
    let before = saved_threads(rt);

    set_forced_thread(Some(worker));
    // Ensure the worker has a base local frame before any slot runs.
    {
        let mut st = rt.borrow_mut();
        st.threads.entry(worker).or_default().ensure_frame();
        st.last_thread = worker;
    }

    let bits = dispatch(vm, rt, env, worker, op);

    set_forced_thread(None);
    restore_threads(rt, before);

    // A structured error recorded during the slot call is the reply's error;
    // pending Java exceptions are delivered through `ExceptionCheck` instead.
    match crate::refs::take_error(rt, worker) {
        Some(e) => Err(e),
        None => Ok(bits),
    }
}

struct SavedThreads {
    last: usize,
    error: Option<JniError>,
}

fn saved_threads(rt: &std::rc::Rc<RefCell<State>>) -> SavedThreads {
    let st = rt.borrow();
    let last = st.last_thread;
    let error = st.threads.get(&last).and_then(|t| t.error.clone());
    SavedThreads { last, error }
}

fn restore_threads(rt: &std::rc::Rc<RefCell<State>>, saved: SavedThreads) {
    let mut st = rt.borrow_mut();
    st.last_thread = saved.last;
    if let Some(t) = st.threads.get_mut(&saved.last) {
        t.error = saved.error;
    }
}

fn cstring(bytes: &[u8]) -> CString {
    CString::new(bytes.to_vec()).unwrap_or_default()
}

fn dispatch(
    vm: &mut Vm,
    rt: &std::rc::Rc<RefCell<State>>,
    env: usize,
    worker: usize,
    op: Op,
) -> u64 {
    use crate::jni::*;
    match op {
        Op::FindClass { name } => {
            let c = cstring(&name);
            // SAFETY: `c` outlives the call; the slot copies the name.
            unsafe { jni_FindClass(env, c.as_ptr()) as u64 }
        }
        Op::NewStringUtf { bytes } => {
            let c = cstring(&bytes);
            unsafe { jni_NewStringUTF(env, c.as_ptr()) as u64 }
        }
        Op::NewString { units } => {
            if units.is_empty() {
                return unsafe { jni_NewString(env, std::ptr::null(), 0) as u64 };
            }
            unsafe { jni_NewString(env, units.as_ptr(), units.len() as i32) as u64 }
        }
        Op::MethodCodes { mid } => crate::jni::acos_jni_method_codes(mid) as u64,
        Op::AllocObject { class } => unsafe { crate::jni::jni_AllocObject(env, class) as u64 },
        Op::GetMethodId {
            class,
            name,
            sig,
            is_static,
        } => {
            let n = cstring(&name);
            let s = cstring(&sig);
            unsafe {
                if is_static {
                    jni_GetStaticMethodID(env, class, n.as_ptr(), s.as_ptr()) as u64
                } else {
                    jni_GetMethodID(env, class, n.as_ptr(), s.as_ptr()) as u64
                }
            }
        }
        Op::GetFieldId {
            class,
            name,
            sig,
            is_static,
        } => {
            let n = cstring(&name);
            let s = cstring(&sig);
            unsafe {
                if is_static {
                    jni_GetStaticFieldID(env, class, n.as_ptr(), s.as_ptr()) as u64
                } else {
                    jni_GetFieldID(env, class, n.as_ptr(), s.as_ptr()) as u64
                }
            }
        }
        Op::CallMethod {
            target,
            mid,
            mode,
            words,
        } => {
            let ptr = if words.is_empty() {
                std::ptr::null()
            } else {
                words.as_ptr()
            };
            let r = unsafe { acos_jni_va_invoke(env, target, mid, mode, ptr, words.len() as i32) };
            // `NewObject` (mode 2) already creates a local; plain calls must
            // root the returned object in the worker's frame or GC stress can
            // collect it before the worker uses it.
            if mode == 2 {
                r.bits
            } else {
                root_ref_return(vm, rt, worker, mid, r.bits)
            }
        }
        Op::CallNonvirtual {
            obj,
            cls,
            mid,
            words,
        } => {
            let ptr = if words.is_empty() {
                std::ptr::null()
            } else {
                words.as_ptr()
            };
            let r = unsafe {
                acos_jni_va_invoke_nonvirtual(env, obj, cls, mid, 0, ptr, words.len() as i32)
            };
            root_ref_return(vm, rt, worker, mid, r.bits)
        }
        Op::GetField { obj, fid } => broker_get_field(obj, fid),
        Op::SetField { obj, fid, bits } => {
            broker_set_field(obj, fid, bits);
            0
        }
        Op::GetStringLength { s } => unsafe { jni_GetStringLength(env, s) as u64 },
        Op::GetStringUtfChars { s } => unsafe {
            jni_GetStringUTFChars(env, s, std::ptr::null_mut()) as u64
        },
        Op::ReleaseStringUtfChars { s, chars } => {
            unsafe { jni_ReleaseStringUTFChars(env, s, chars as *const std::os::raw::c_char) };
            0
        }
        Op::ExceptionCheck => unsafe { jni_ExceptionCheck(env) as u64 },
        Op::ExceptionOccurred => unsafe { jni_ExceptionOccurred(env) as u64 },
        Op::ExceptionClear => {
            unsafe { jni_ExceptionClear(env) };
            0
        }
        Op::PushLocalFrame { cap } => unsafe { jni_PushLocalFrame(env, cap) as u64 },
        Op::PopLocalFrame { result } => unsafe { jni_PopLocalFrame(env, result) as u64 },
        Op::NewGlobalRef { obj } => unsafe { jni_NewGlobalRef(env, obj) as u64 },
        Op::DeleteGlobalRef { obj } => {
            unsafe { jni_DeleteGlobalRef(env, obj) };
            0
        }
        Op::DeleteLocalRef { obj } => {
            unsafe { jni_DeleteLocalRef(env, obj) };
            0
        }
        Op::GetObjectClass { obj } => unsafe { jni_GetObjectClass(env, obj) as u64 },
        Op::IsSameObject { a, b } => unsafe { jni_IsSameObject(env, a, b) as u64 },
        Op::Throw { obj } => unsafe { jni_Throw(env, obj) as u64 },
        Op::ThrowNew { class, message } => {
            let m = cstring(&message);
            unsafe { jni_ThrowNew(env, class, m.as_ptr()) as u64 }
        }
        Op::GetObjectRefType { obj } => unsafe { jni_GetObjectRefType(env, obj) as u64 },
        Op::Detach => {
            detach_state(vm, rt, worker);
            0
        }
    }
}

/// If `mid` returns a reference, root `bits` as a local in `worker`'s frame.
fn root_ref_return(
    vm: &mut Vm,
    rt: &std::rc::Rc<RefCell<State>>,
    worker: usize,
    mid: usize,
    bits: u64,
) -> u64 {
    let ret = {
        let st = rt.borrow();
        st.methods
            .iter()
            .find(|m| &***m as *const crate::state::MethodInfo as usize == mid)
            .map(|m| m.ret_desc.clone())
    };
    if let Some(ret) = ret {
        if matches!(ret.as_bytes().first().copied(), Some(b'L') | Some(b'[')) && bits != 0 {
            // SAFETY: handle came from the owner-side method call.
            let h = crate::refs::local(vm, rt, worker, bits as u32);
            return h as u64;
        }
    }
    bits
}

/// Release every root owned by a worker's JNI state (locals + pending).
fn detach_state(vm: &mut Vm, rt: &std::rc::Rc<RefCell<State>>, worker: usize) {
    let removed = rt.borrow_mut().threads.remove(&worker);
    if let Some(th) = removed {
        for frame in th.frames {
            for h in frame.refs {
                let _ = vm.release_bridge_root(h);
            }
        }
        for h in th.return_roots {
            let _ = vm.release_bridge_root(h);
        }
        if let Some(p) = th.pending {
            if p.rooted {
                let _ = vm.release_bridge_root(p.handle);
            }
        }
    }
}

/// Detach the calling worker: service a `Detach` so the owner frees the
/// worker's reference frames, then drop the local attachment. Returns whether
/// the thread was attached.
pub fn detach_worker() -> bool {
    let tid = std::thread::current().id();
    let Some((worker, owner)) = worker_lookup(tid) else {
        return false;
    };
    // A worker that never issued a VM request owns no reference frames on the
    // owner, so it can drop its own registry entry without a round trip. This
    // keeps a bare attach/detach from waiting on an owner that is itself
    // blocked in a native join.
    let used = {
        let mut b = broker().lock().unwrap_or_else(|e| e.into_inner());
        b.used_workers.remove(&worker)
    };
    if used {
        // A `Detach` request is only useful if this is still a live worker and
        // the owner still exists. Even if it times out, drop the local
        // attachment so the OS thread does not leak it.
        let _ = submit(Op::Detach, "DetachCurrentThread");
    }
    let mut b = broker().lock().unwrap_or_else(|e| e.into_inner());
    b.workers.remove(&tid);
    if b.attached_workers > 0 {
        b.attached_workers -= 1;
    }
    let _ = worker;
    drop(b);
    WORKER.with(|c| c.set(None));
    let _ = owner;
    true
}

/// Install the broker's safe-point hook into `acos-vm`. Idempotent.
pub(crate) fn install_safe_point() {
    safe_point::register(Some(safe_point_hook));
}

/// Remove the safe-point hook.
pub(crate) fn uninstall_safe_point() {
    safe_point::register(None);
}

// ---------------------------------------------------------------------------
// Worker-side routing for individual slots
// ---------------------------------------------------------------------------

/// Generic worker routing for a slot returning a machine word. On a broker
/// error the error is recorded on the calling worker and `0` is returned (the
/// JNI contract for a contract violation).
pub(crate) fn worker_slot(op: Op, function: &str) -> Option<u64> {
    match worker_op(op, function) {
        None => None,
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            set_worker_error(e);
            Some(0)
        }
    }
}

/// Route a `GetMethodID`/`GetStaticMethodID` through the broker.
pub(crate) fn worker_get_method_id(
    class: usize,
    name: &[u8],
    sig: &[u8],
    is_static: bool,
) -> Option<u64> {
    worker_slot(
        Op::GetMethodId {
            class,
            name: name.to_vec(),
            sig: sig.to_vec(),
            is_static,
        },
        "GetMethodID",
    )
}

/// Route a `GetFieldID`/`GetStaticFieldID` through the broker.
pub(crate) fn worker_get_field_id(
    class: usize,
    name: &[u8],
    sig: &[u8],
    is_static: bool,
) -> Option<u64> {
    worker_slot(
        Op::GetFieldId {
            class,
            name: name.to_vec(),
            sig: sig.to_vec(),
            is_static,
        },
        "GetFieldID",
    )
}

/// Route a class lookup through the broker (worker threads only).
pub(crate) fn worker_find_class(name: &[u8]) -> Option<u64> {
    match worker_op(
        Op::FindClass {
            name: name.to_vec(),
        },
        "FindClass",
    ) {
        None => None,
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            set_worker_error(e);
            Some(0)
        }
    }
}

/// Route `NewStringUTF` through the broker.
pub(crate) fn worker_new_string_utf(bytes: &[u8]) -> Option<u64> {
    match worker_op(
        Op::NewStringUtf {
            bytes: bytes.to_vec(),
        },
        "NewStringUTF",
    ) {
        None => None,
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            set_worker_error(e);
            Some(0)
        }
    }
}

/// Route `NewString` through the broker.
pub(crate) fn worker_new_string(units: &[u16]) -> Option<u64> {
    match worker_op(
        Op::NewString {
            units: units.to_vec(),
        },
        "NewString",
    ) {
        None => None,
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            set_worker_error(e);
            Some(0)
        }
    }
}
