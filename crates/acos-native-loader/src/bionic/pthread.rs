//! pthread shims with handle indirection.
//!
//! # Why not forward?
//!
//! Bionic's `pthread_mutex_t` (40 B), `pthread_cond_t` (48 B) and
//! `pthread_attr_t` (56 B) are *not* layout-compatible with glibc's; passing
//! guest storage straight to glibc would corrupt it. We therefore treat the
//! guest's opaque storage as a handle:
//!
//! * the first word(s) of the guest object are irrelevant to us — we key a
//!   host object off the object's *address*;
//! * a zeroed guest `pthread_mutex_t`/`pthread_cond_t` (the bionic static
//!   initializers) is created lazily as a default host object;
//! * guest threads created through `pthread_create` run a Rust trampoline that
//!   initialises the guest TLS state before invoking the guest start routine.

use std::collections::HashMap;
use std::os::raw::{c_int, c_void};
use std::sync::{Mutex, OnceLock};

use super::Shim;

// Set by the TLS module when a guest thread starts.
use crate::tls::ensure_thread_tls;

struct ThreadStart {
    start: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    arg: usize,
}

extern "C" {
    #[link_name = "pthread_attr_getdetachstate"]
    fn host_pthread_attr_getdetachstate(attr: *const libc::pthread_attr_t, state: *mut c_int) -> c_int;
}

extern "C" fn thread_trampoline(ptr: *mut c_void) -> *mut c_void {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // Establish the guest TLS block for this thread before any guest code runs.
    ensure_thread_tls();
    // SAFETY: the pointer is the leaked `ThreadStart` from `pthread_create`.
    let ts = unsafe { Box::from_raw(ptr as *mut ThreadStart) };
    let start = ts.start;
    let arg = ts.arg as *mut c_void;
    drop(ts);
    // SAFETY: guest-provided function pointer with the pthread start ABI.
    unsafe { start(arg) }
}

// --- handle tables ----------------------------------------------------------

type MutexMap = Mutex<HashMap<usize, Box<libc::pthread_mutex_t>>>;
type CondMap = Mutex<HashMap<usize, Box<libc::pthread_cond_t>>>;
type RwlockMap = Mutex<HashMap<usize, Box<libc::pthread_rwlock_t>>>;
type AttrMap = Mutex<HashMap<usize, Box<libc::pthread_attr_t>>>;
type MutexAttrMap = Mutex<HashMap<usize, Box<libc::pthread_mutexattr_t>>>;
type CondAttrMap = Mutex<HashMap<usize, Box<libc::pthread_condattr_t>>>;

macro_rules! table {
    ($fn_name:ident, $map_ty:ty, $ty:ty) => {
        fn $fn_name() -> &'static $map_ty {
            static T: OnceLock<$map_ty> = OnceLock::new();
            T.get_or_init(|| Mutex::new(HashMap::new()))
        }
    };
}

table!(mutexes, MutexMap, libc::pthread_mutex_t);
table!(conds, CondMap, libc::pthread_cond_t);
table!(rwlocks, RwlockMap, libc::pthread_rwlock_t);
table!(attrs, AttrMap, libc::pthread_attr_t);
table!(mutex_attrs, MutexAttrMap, libc::pthread_mutexattr_t);
table!(cond_attrs, CondAttrMap, libc::pthread_condattr_t);

fn mutex_ptr(guest: usize) -> *mut libc::pthread_mutex_t {
    let mut g = mutexes().lock().unwrap();
    g.entry(guest)
        .or_insert_with(|| {
            // SAFETY: a zeroed glibc mutex is a valid default mutex (this is
            // exactly glibc's PTHREAD_MUTEX_INITIALIZER).
            Box::new(unsafe { std::mem::zeroed() })
        })
        .as_mut()
}

fn cond_ptr(guest: usize) -> *mut libc::pthread_cond_t {
    let mut g = conds().lock().unwrap();
    g.entry(guest)
        .or_insert_with(|| {
            // SAFETY: zeroed glibc cond = PTHREAD_COND_INITIALIZER.
            Box::new(unsafe { std::mem::zeroed() })
        })
        .as_mut()
}

fn rwlock_ptr(guest: usize) -> *mut libc::pthread_rwlock_t {
    let mut g = rwlocks().lock().unwrap();
    g.entry(guest)
        .or_insert_with(|| Box::new(unsafe { std::mem::zeroed() }))
        .as_mut()
}

fn attr_ptr(guest: usize) -> *mut libc::pthread_attr_t {
    let mut g = attrs().lock().unwrap();
    g.entry(guest)
        .or_insert_with(|| Box::new(unsafe { std::mem::zeroed() }))
        .as_mut()
}

// --- mutex ------------------------------------------------------------------

pub unsafe extern "C" fn pthread_mutex_init(m: usize, _attr: *const c_void) -> c_int {
    let mut g = mutexes().lock().unwrap();
    let mut v: libc::pthread_mutex_t = std::mem::zeroed();
    libc::pthread_mutex_init(&mut v, std::ptr::null());
    g.insert(m, Box::new(v));
    libc::pthread_mutex_lock(&mut v);
    libc::pthread_mutex_unlock(&mut v);
    0
}

pub unsafe extern "C" fn pthread_mutex_destroy(m: usize) -> c_int {
    mutexes().lock().unwrap().remove(&m);
    0
}

pub unsafe extern "C" fn pthread_mutex_lock(m: usize) -> c_int {
    libc::pthread_mutex_lock(mutex_ptr(m))
}

pub unsafe extern "C" fn pthread_mutex_unlock(m: usize) -> c_int {
    libc::pthread_mutex_unlock(mutex_ptr(m))
}

pub unsafe extern "C" fn pthread_mutex_trylock(m: usize) -> c_int {
    libc::pthread_mutex_trylock(mutex_ptr(m))
}

// --- condition variables ----------------------------------------------------

pub unsafe extern "C" fn pthread_cond_init(c: usize, _attr: *const c_void) -> c_int {
    let mut g = conds().lock().unwrap();
    let mut v: libc::pthread_cond_t = std::mem::zeroed();
    libc::pthread_cond_init(&mut v, std::ptr::null());
    g.insert(c, Box::new(v));
    0
}

pub unsafe extern "C" fn pthread_cond_destroy(c: usize) -> c_int {
    conds().lock().unwrap().remove(&c);
    0
}

pub unsafe extern "C" fn pthread_cond_signal(c: usize) -> c_int {
    libc::pthread_cond_signal(cond_ptr(c))
}

pub unsafe extern "C" fn pthread_cond_broadcast(c: usize) -> c_int {
    libc::pthread_cond_broadcast(cond_ptr(c))
}

pub unsafe extern "C" fn pthread_cond_wait(c: usize, m: usize) -> c_int {
    libc::pthread_cond_wait(cond_ptr(c), mutex_ptr(m))
}

pub unsafe extern "C" fn pthread_cond_timedwait(
    c: usize,
    m: usize,
    abstime: *const libc::timespec,
) -> c_int {
    libc::pthread_cond_timedwait(cond_ptr(c), mutex_ptr(m), abstime)
}

// --- rwlocks ----------------------------------------------------------------

pub unsafe extern "C" fn pthread_rwlock_init(l: usize, _attr: *const c_void) -> c_int {
    let mut g = rwlocks().lock().unwrap();
    g.insert(l, Box::new(std::mem::zeroed()));
    0
}
pub unsafe extern "C" fn pthread_rwlock_destroy(l: usize) -> c_int {
    rwlocks().lock().unwrap().remove(&l);
    0
}
pub unsafe extern "C" fn pthread_rwlock_rdlock(l: usize) -> c_int {
    libc::pthread_rwlock_rdlock(rwlock_ptr(l))
}
pub unsafe extern "C" fn pthread_rwlock_wrlock(l: usize) -> c_int {
    libc::pthread_rwlock_wrlock(rwlock_ptr(l))
}
pub unsafe extern "C" fn pthread_rwlock_unlock(l: usize) -> c_int {
    libc::pthread_rwlock_unlock(rwlock_ptr(l))
}

// --- attributes -------------------------------------------------------------

pub unsafe extern "C" fn pthread_attr_init(a: usize) -> c_int {
    let p = attr_ptr(a);
    libc::pthread_attr_init(p)
}
pub unsafe extern "C" fn pthread_attr_destroy(a: usize) -> c_int {
    attrs().lock().unwrap().remove(&a);
    0
}
pub unsafe extern "C" fn pthread_attr_setdetachstate(a: usize, state: c_int) -> c_int {
    libc::pthread_attr_setdetachstate(attr_ptr(a), state)
}
pub unsafe extern "C" fn pthread_attr_getdetachstate(a: usize, out: *mut c_int) -> c_int {
    host_pthread_attr_getdetachstate(attr_ptr(a), out)
}
pub unsafe extern "C" fn pthread_attr_setstacksize(a: usize, size: usize) -> c_int {
    libc::pthread_attr_setstacksize(attr_ptr(a), size)
}
pub unsafe extern "C" fn pthread_attr_getstacksize(a: usize, out: *mut usize) -> c_int {
    libc::pthread_attr_getstacksize(attr_ptr(a), out)
}
pub unsafe extern "C" fn pthread_attr_setguardsize(a: usize, size: usize) -> c_int {
    libc::pthread_attr_setguardsize(attr_ptr(a), size)
}
pub unsafe extern "C" fn pthread_attr_getguardsize(a: usize, out: *mut usize) -> c_int {
    libc::pthread_attr_getguardsize(attr_ptr(a), out)
}

// --- mutex/cond attributes (opaque handles; best-effort) --------------------

pub unsafe extern "C" fn pthread_mutexattr_init(a: usize) -> c_int {
    let mut g = mutex_attrs().lock().unwrap();
    let mut v: libc::pthread_mutexattr_t = std::mem::zeroed();
    libc::pthread_mutexattr_init(&mut v);
    g.insert(a, Box::new(v));
    0
}
pub unsafe extern "C" fn pthread_mutexattr_destroy(a: usize) -> c_int {
    mutex_attrs().lock().unwrap().remove(&a);
    0
}
pub unsafe extern "C" fn pthread_mutexattr_settype(a: usize, t: c_int) -> c_int {
    let mut g = mutex_attrs().lock().unwrap();
    match g.get_mut(&a) {
        Some(v) => libc::pthread_mutexattr_settype(v.as_mut(), t),
        None => -1,
    }
}

pub unsafe extern "C" fn pthread_condattr_init(a: usize) -> c_int {
    let mut g = cond_attrs().lock().unwrap();
    let mut v: libc::pthread_condattr_t = std::mem::zeroed();
    libc::pthread_condattr_init(&mut v);
    g.insert(a, Box::new(v));
    0
}
pub unsafe extern "C" fn pthread_condattr_destroy(a: usize) -> c_int {
    cond_attrs().lock().unwrap().remove(&a);
    0
}

// --- threads ----------------------------------------------------------------

pub unsafe extern "C" fn pthread_create(
    thread: *mut libc::pthread_t,
    attr: usize,
    start: usize,
    arg: usize,
) -> c_int {
    let start_fn: unsafe extern "C" fn(*mut c_void) -> *mut c_void =
        std::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void) -> *mut c_void>(start);
    let ts = Box::into_raw(Box::new(ThreadStart {
        start: start_fn,
        arg,
    }));
    let host_attr: *const libc::pthread_attr_t = if attr == 0 {
        std::ptr::null()
    } else {
        attr_ptr(attr)
    };
    let mut tid: libc::pthread_t = 0;
    let rc = libc::pthread_create(&mut tid, host_attr, thread_trampoline, ts as *mut c_void);
    if rc != 0 {
        // SAFETY: reclaim the box on failure.
        drop(Box::from_raw(ts));
        return rc;
    }
    if !thread.is_null() {
        *thread = tid;
    }
    0
}

pub unsafe extern "C" fn pthread_join(thread: libc::pthread_t, retval: *mut *mut c_void) -> c_int {
    libc::pthread_join(thread, retval)
}

pub unsafe extern "C" fn pthread_detach(thread: libc::pthread_t) -> c_int {
    libc::pthread_detach(thread)
}

pub unsafe extern "C" fn pthread_self() -> libc::pthread_t {
    libc::pthread_self()
}

pub unsafe extern "C" fn pthread_equal(a: libc::pthread_t, b: libc::pthread_t) -> c_int {
    c_int::from(a == b)
}

pub unsafe extern "C" fn pthread_exit(retval: *mut c_void) -> ! {
    libc::pthread_exit(retval);
}

pub unsafe extern "C" fn pthread_key_create(
    key: *mut libc::pthread_key_t,
    dtor: Option<unsafe extern "C" fn(*mut c_void)>,
) -> c_int {
    libc::pthread_key_create(key, dtor)
}
pub unsafe extern "C" fn pthread_key_delete(key: libc::pthread_key_t) -> c_int {
    libc::pthread_key_delete(key)
}
pub unsafe extern "C" fn pthread_getspecific(key: libc::pthread_key_t) -> *mut c_void {
    libc::pthread_getspecific(key)
}
pub unsafe extern "C" fn pthread_setspecific(
    key: libc::pthread_key_t,
    value: *const c_void,
) -> c_int {
    libc::pthread_setspecific(key, value)
}
pub unsafe extern "C" fn pthread_once(
    once: *mut libc::pthread_once_t,
    init: Option<unsafe extern "C" fn()>,
) -> c_int {
    match init {
        Some(f) => {
            let safe: extern "C" fn() = std::mem::transmute::<unsafe extern "C" fn(), extern "C" fn()>(f);
            libc::pthread_once(once, safe)
        }
        None => -1,
    }
}

pub unsafe extern "C" fn pthread_setname_np(
    thread: libc::pthread_t,
    name: *const std::os::raw::c_char,
) -> c_int {
    libc::pthread_setname_np(thread, name)
}
pub unsafe extern "C" fn pthread_getname_np(
    thread: libc::pthread_t,
    buf: *mut std::os::raw::c_char,
    len: usize,
) -> c_int {
    libc::pthread_getname_np(thread, buf, len)
}

// --- table installation -----------------------------------------------------

macro_rules! shim {
    ($m:expr, $name:literal, $f:path) => {
        $m.insert(
            $name,
            Shim::shimmed($name, $f as *const () as *const c_void),
        );
    };
}

/// Add all pthread shims to the table.
pub fn install(m: &mut HashMap<&'static str, Shim>) {
    shim!(m, "pthread_mutex_init", pthread_mutex_init);
    shim!(m, "pthread_mutex_destroy", pthread_mutex_destroy);
    shim!(m, "pthread_mutex_lock", pthread_mutex_lock);
    shim!(m, "pthread_mutex_unlock", pthread_mutex_unlock);
    shim!(m, "pthread_mutex_trylock", pthread_mutex_trylock);
    shim!(m, "pthread_cond_init", pthread_cond_init);
    shim!(m, "pthread_cond_destroy", pthread_cond_destroy);
    shim!(m, "pthread_cond_signal", pthread_cond_signal);
    shim!(m, "pthread_cond_broadcast", pthread_cond_broadcast);
    shim!(m, "pthread_cond_wait", pthread_cond_wait);
    shim!(m, "pthread_cond_timedwait", pthread_cond_timedwait);
    shim!(m, "pthread_rwlock_init", pthread_rwlock_init);
    shim!(m, "pthread_rwlock_destroy", pthread_rwlock_destroy);
    shim!(m, "pthread_rwlock_rdlock", pthread_rwlock_rdlock);
    shim!(m, "pthread_rwlock_wrlock", pthread_rwlock_wrlock);
    shim!(m, "pthread_rwlock_unlock", pthread_rwlock_unlock);
    shim!(m, "pthread_attr_init", pthread_attr_init);
    shim!(m, "pthread_attr_destroy", pthread_attr_destroy);
    shim!(m, "pthread_attr_setdetachstate", pthread_attr_setdetachstate);
    shim!(m, "pthread_attr_getdetachstate", pthread_attr_getdetachstate);
    shim!(m, "pthread_attr_setstacksize", pthread_attr_setstacksize);
    shim!(m, "pthread_attr_getstacksize", pthread_attr_getstacksize);
    shim!(m, "pthread_attr_setguardsize", pthread_attr_setguardsize);
    shim!(m, "pthread_attr_getguardsize", pthread_attr_getguardsize);
    shim!(m, "pthread_mutexattr_init", pthread_mutexattr_init);
    shim!(m, "pthread_mutexattr_destroy", pthread_mutexattr_destroy);
    shim!(m, "pthread_mutexattr_settype", pthread_mutexattr_settype);
    shim!(m, "pthread_condattr_init", pthread_condattr_init);
    shim!(m, "pthread_condattr_destroy", pthread_condattr_destroy);
    shim!(m, "pthread_create", pthread_create);
    shim!(m, "pthread_join", pthread_join);
    shim!(m, "pthread_detach", pthread_detach);
    shim!(m, "pthread_self", pthread_self);
    shim!(m, "pthread_equal", pthread_equal);
    shim!(m, "pthread_exit", pthread_exit);
    shim!(m, "pthread_key_create", pthread_key_create);
    shim!(m, "pthread_key_delete", pthread_key_delete);
    shim!(m, "pthread_getspecific", pthread_getspecific);
    shim!(m, "pthread_setspecific", pthread_setspecific);
    shim!(m, "pthread_once", pthread_once);
    shim!(m, "pthread_setname_np", pthread_setname_np);
    shim!(m, "pthread_getname_np", pthread_getname_np);
}
