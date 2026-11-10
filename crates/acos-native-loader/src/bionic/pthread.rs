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
    #[link_name = "pthread_getattr_np"]
    fn host_pthread_getattr_np(thread: libc::pthread_t, attr: *mut libc::pthread_attr_t) -> c_int;
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
/// Guest `pthread_mutexattr_t` state.
///
/// bionic's attribute objects are a bare `int` (4 B) and `pthread_mutexattr_*`
/// operate directly on that word, so a forwarded glibc attribute would be
/// silently discarded by [`pthread_mutex_init`]. Keep the type in a side table
/// keyed by the guest address (and mirror bionic's encoding into the guest word
/// so code that reads the attribute directly still agrees).
#[derive(Clone, Copy)]
struct MutexAttrState {
    type_: c_int,
}

/// Guest `pthread_condattr_t` state: the clock id, exactly as bionic stores it.
#[derive(Clone, Copy)]
struct CondAttrState {
    clock: c_int,
}

type MutexAttrMap = Mutex<HashMap<usize, MutexAttrState>>;
type CondAttrMap = Mutex<HashMap<usize, CondAttrState>>;

/// W117-A diagnostic trace for the attribute/clock path (inert by default).
const TRACE: bool = false;

fn trace(args: std::fmt::Arguments<'_>) {
    if !TRACE {
        return;
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    if N.fetch_add(1, Ordering::Relaxed) < 120 {
        eprintln!(
            "ACOS W117A pthread tid={:?} {args}",
            std::thread::current().id()
        );
    }
}

/// `PTHREAD_MUTEX_NORMAL` / `RECURSIVE` / `ERRORCHECK` — bionic and glibc use
/// the same numbering, so the value can be passed to both.
const MUTEX_NORMAL: c_int = 0;
const MUTEX_RECURSIVE: c_int = 1;
const MUTEX_ERRORCHECK: c_int = 2;

/// bionic packs the *mutex* type into bits 14-15 of the guest object, which is
/// what the static initialisers use (`PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP`
/// is `0x4000`, errorcheck `0x8000`). All-zero is the normal static initialiser.
const BIONIC_MUTEX_TYPE_SHIFT: u32 = 14;
const BIONIC_MUTEX_TYPE_MASK: u32 = 0x3 << BIONIC_MUTEX_TYPE_SHIFT;

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

/// Allocate a host mutex of the requested flavour.
///
/// A zeroed glibc mutex is exactly `PTHREAD_MUTEX_INITIALIZER` (normal), so the
/// common case needs no explicit init; recursive/errorcheck must be initialised
/// with an attribute object or the guest self-deadlocks on re-lock.
fn host_mutex(type_: c_int) -> Box<libc::pthread_mutex_t> {
    // SAFETY: zeroed pthread types are the documented static initialisers.
    let mut v: libc::pthread_mutex_t = unsafe { std::mem::zeroed() };
    if type_ == MUTEX_NORMAL {
        return Box::new(v);
    }
    let mut a: libc::pthread_mutexattr_t = unsafe { std::mem::zeroed() };
    // SAFETY: all three calls take initialised host objects.
    unsafe {
        libc::pthread_mutexattr_init(&mut a);
        libc::pthread_mutexattr_settype(&mut a, type_);
        libc::pthread_mutex_init(&mut v, &a);
        libc::pthread_mutexattr_destroy(&mut a);
    }
    Box::new(v)
}

/// Read the bionic static-initialiser encoding out of the guest mutex object.
fn bionic_static_mutex_type(guest: usize) -> c_int {
    if guest == 0 {
        return MUTEX_NORMAL;
    }
    let mut raw = 0u32;
    // SAFETY: the guest object is bionic's 40-byte `pthread_mutex_t`; copy so an
    // unaligned guest address is fine. Any value outside the documented type
    // bits (stack garbage on a never-initialised mutex) falls back to normal.
    unsafe {
        std::ptr::copy_nonoverlapping(guest as *const u8, &mut raw as *mut u32 as *mut u8, 4);
    }
    match (raw & BIONIC_MUTEX_TYPE_MASK) >> BIONIC_MUTEX_TYPE_SHIFT {
        1 => MUTEX_RECURSIVE,
        2 => MUTEX_ERRORCHECK,
        _ => MUTEX_NORMAL,
    }
}

fn mutex_ptr(guest: usize) -> *mut libc::pthread_mutex_t {
    let mut g = mutexes().lock().unwrap_or_else(|e| e.into_inner());
    g.entry(guest)
        .or_insert_with(|| host_mutex(bionic_static_mutex_type(guest)))
        .as_mut()
}

fn cond_ptr(guest: usize) -> *mut libc::pthread_cond_t {
    let mut g = conds().lock().unwrap_or_else(|e| e.into_inner());
    g.entry(guest)
        .or_insert_with(|| {
            // SAFETY: zeroed glibc cond = PTHREAD_COND_INITIALIZER (realtime
            // clock), which is also bionic's all-zero static initialiser.
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

pub unsafe extern "C" fn pthread_mutex_init(m: usize, attr: *const c_void) -> c_int {
    // Honouring the attribute type is not optional: libflutter initialises
    // recursive mutexes through `pthread_mutexattr_settype`, and a normal host
    // mutex would self-deadlock on the second lock.
    let type_ = mutex_attr_type(attr);
    trace(format_args!("pthread_mutex_init type={type_}"));
    let v = host_mutex(type_);
    mutexes()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(m, v);
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

pub unsafe extern "C" fn pthread_cond_init(c: usize, attr: *const c_void) -> c_int {
    let clock = cond_attr_clock(attr);
    trace(format_args!("pthread_cond_init clock={clock}"));
    // SAFETY: zeroed pthread types are the documented static initialisers.
    let mut v: libc::pthread_cond_t = unsafe { std::mem::zeroed() };
    if clock == libc::CLOCK_REALTIME {
        libc::pthread_cond_init(&mut v, std::ptr::null());
    } else {
        // bionic's `pthread_condattr_setclock` is a real feature: the engine
        // arms monotonic deadlines, and a condvar left on CLOCK_REALTIME would
        // return ETIMEDOUT immediately for every `pthread_cond_timedwait`.
        let mut a: libc::pthread_condattr_t = unsafe { std::mem::zeroed() };
        libc::pthread_condattr_init(&mut a);
        libc::pthread_condattr_setclock(&mut a, clock);
        libc::pthread_cond_init(&mut v, &a);
        libc::pthread_condattr_destroy(&mut a);
    }
    conds()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(c, Box::new(v));
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
    let rc = libc::pthread_cond_timedwait(cond_ptr(c), mutex_ptr(m), abstime);
    if rc == libc::ETIMEDOUT {
        trace(format_args!("pthread_cond_timedwait timeout"));
    }
    rc
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

/// `pthread_getattr_np(thread, attr)`.
///
/// The guest `pthread_attr_t` is opaque bionic storage (8 B on LP64); glibc's
/// is 56 B, so it must not be written directly. Fill the host attribute in the
/// same address-keyed table the other `pthread_attr_*` shims use. For the FFI
/// model the guest runs on the host thread, so the host stack it reports is the
/// stack the guest is actually using.
pub unsafe extern "C" fn pthread_getattr_np(thread: libc::pthread_t, attr: usize) -> c_int {
    host_pthread_getattr_np(thread, attr_ptr(attr))
}

/// `pthread_attr_getstack(attr, addr, size)` against the host attribute box.
pub unsafe extern "C" fn pthread_attr_getstack(
    attr: usize,
    stackaddr: *mut *mut c_void,
    stacksize: *mut usize,
) -> c_int {
    libc::pthread_attr_getstack(attr_ptr(attr), stackaddr, stacksize)
}

// --- mutex/cond attributes (opaque handles; best-effort) --------------------

/// Write a 4-byte bionic attribute word (bionic's attrs are a bare `int`).
fn write_guest_attr(a: usize, value: c_int) {
    if a == 0 {
        return;
    }
    // SAFETY: guest storage for these attribute types is one `int`.
    unsafe { std::ptr::write(a as *mut c_int, value) }
}

/// Read a 4-byte bionic attribute word (fallback when the side table has no
/// entry, e.g. an attribute the guest built without calling `*_attr_init`).
fn read_guest_attr(a: usize) -> c_int {
    if a == 0 {
        return 0;
    }
    let mut raw: c_int = 0;
    // SAFETY: see [`write_guest_attr`].
    unsafe { std::ptr::copy_nonoverlapping(a as *const u8, &mut raw as *mut c_int as *mut u8, 4) };
    raw
}

fn mutex_attr_type(attr: *const c_void) -> c_int {
    if attr.is_null() {
        return MUTEX_NORMAL;
    }
    let a = attr as usize;
    let g = mutex_attrs().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.get(&a) {
        return s.type_;
    }
    drop(g);
    // bionic stores the type directly (`*attr = type`).
    match read_guest_attr(a) {
        MUTEX_RECURSIVE => MUTEX_RECURSIVE,
        MUTEX_ERRORCHECK => MUTEX_ERRORCHECK,
        _ => MUTEX_NORMAL,
    }
}

fn cond_attr_clock(attr: *const c_void) -> c_int {
    if attr.is_null() {
        return libc::CLOCK_REALTIME;
    }
    let a = attr as usize;
    let g = cond_attrs().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.get(&a) {
        return s.clock;
    }
    drop(g);
    // bionic stores the clock id directly (`*attr = clock`).
    match read_guest_attr(a) {
        libc::CLOCK_MONOTONIC => libc::CLOCK_MONOTONIC,
        _ => libc::CLOCK_REALTIME,
    }
}

pub unsafe extern "C" fn pthread_mutexattr_init(a: usize) -> c_int {
    mutex_attrs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            a,
            MutexAttrState {
                type_: MUTEX_NORMAL,
            },
        );
    write_guest_attr(a, MUTEX_NORMAL);
    0
}
pub unsafe extern "C" fn pthread_mutexattr_destroy(a: usize) -> c_int {
    mutex_attrs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&a);
    0
}
pub unsafe extern "C" fn pthread_mutexattr_settype(a: usize, t: c_int) -> c_int {
    if !(MUTEX_NORMAL..=MUTEX_ERRORCHECK).contains(&t) {
        return libc::EINVAL;
    }
    mutex_attrs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(a, MutexAttrState { type_: t });
    write_guest_attr(a, t);
    0
}
pub unsafe extern "C" fn pthread_mutexattr_gettype(a: usize, out: *mut c_int) -> c_int {
    if out.is_null() {
        return libc::EINVAL;
    }
    // SAFETY: caller-provided writable `int`.
    unsafe { out.write(mutex_attr_type(a as *const c_void)) };
    0
}

pub unsafe extern "C" fn pthread_condattr_init(a: usize) -> c_int {
    cond_attrs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            a,
            CondAttrState {
                clock: libc::CLOCK_REALTIME,
            },
        );
    write_guest_attr(a, libc::CLOCK_REALTIME);
    0
}
pub unsafe extern "C" fn pthread_condattr_destroy(a: usize) -> c_int {
    cond_attrs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&a);
    0
}

/// `pthread_condattr_setclock` — previously forwarded straight to glibc on
/// bionic guest storage, which lost the attribute entirely: [`pthread_cond_init`]
/// always built a CLOCK_REALTIME condvar and every monotonic timed wait expired
/// immediately.
pub unsafe extern "C" fn pthread_condattr_setclock(a: usize, clock: c_int) -> c_int {
    if clock != libc::CLOCK_REALTIME && clock != libc::CLOCK_MONOTONIC {
        return libc::EINVAL;
    }
    trace(format_args!("pthread_condattr_setclock clock={clock}"));
    cond_attrs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(a, CondAttrState { clock });
    write_guest_attr(a, clock);
    0
}
pub unsafe extern "C" fn pthread_condattr_getclock(a: usize, out: *mut c_int) -> c_int {
    if out.is_null() {
        return libc::EINVAL;
    }
    // SAFETY: caller-provided writable `int`.
    unsafe { out.write(cond_attr_clock(a as *const c_void)) };
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
    shim!(m, "pthread_getattr_np", pthread_getattr_np);
    shim!(m, "pthread_attr_getstack", pthread_attr_getstack);
    shim!(m, "pthread_mutexattr_init", pthread_mutexattr_init);
    shim!(m, "pthread_mutexattr_destroy", pthread_mutexattr_destroy);
    shim!(m, "pthread_mutexattr_settype", pthread_mutexattr_settype);
    shim!(m, "pthread_mutexattr_gettype", pthread_mutexattr_gettype);
    shim!(m, "pthread_condattr_init", pthread_condattr_init);
    shim!(m, "pthread_condattr_destroy", pthread_condattr_destroy);
    shim!(m, "pthread_condattr_setclock", pthread_condattr_setclock);
    shim!(m, "pthread_condattr_getclock", pthread_condattr_getclock);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Guest storage big enough for bionic's LP64 pthread types
    /// (`pthread_mutex_t` 40 B, `pthread_cond_t` 48 B); 64 B is generous.
    fn guest() -> Box<[u8; 64]> {
        Box::new([0u8; 64])
    }

    fn addr(b: &[u8; 64]) -> usize {
        b.as_ptr() as usize
    }

    #[test]
    fn mutexattr_type_reaches_the_host_mutex() {
        // SAFETY: shim calls with valid guest storage.
        unsafe {
            let m = guest();
            let a = 0i32;
            let ap = &a as *const i32 as usize;
            assert_eq!(pthread_mutexattr_init(ap), 0);
            assert_eq!(pthread_mutexattr_settype(ap, MUTEX_RECURSIVE), 0);
            // bionic mirrors the type in the guest word so a direct reader sees it.
            assert_eq!(a, MUTEX_RECURSIVE);
            assert_eq!(pthread_mutex_init(addr(&m), ap as *const c_void), 0);

            // A recursive mutex may be re-locked by the same thread.
            assert_eq!(pthread_mutex_lock(addr(&m)), 0);
            assert_eq!(pthread_mutex_lock(addr(&m)), 0);
            assert_eq!(pthread_mutex_unlock(addr(&m)), 0);
            assert_eq!(pthread_mutex_unlock(addr(&m)), 0);
            assert_eq!(pthread_mutex_destroy(addr(&m)), 0);
            assert_eq!(pthread_mutexattr_destroy(ap), 0);
        }
    }

    #[test]
    fn errorcheck_mutex_reports_self_deadlock_instead_of_hanging() {
        // SAFETY: shim calls with valid guest storage.
        unsafe {
            let m = guest();
            let a = 0i32;
            let ap = &a as *const i32 as usize;
            pthread_mutexattr_init(ap);
            pthread_mutexattr_settype(ap, MUTEX_ERRORCHECK);
            assert_eq!(pthread_mutex_init(addr(&m), ap as *const c_void), 0);
            assert_eq!(pthread_mutex_lock(addr(&m)), 0);
            assert_eq!(pthread_mutex_lock(addr(&m)), libc::EDEADLK);
            assert_eq!(pthread_mutex_unlock(addr(&m)), 0);
            pthread_mutex_destroy(addr(&m));
        }
    }

    #[test]
    fn bionic_static_initialisers_are_decoded() {
        // SAFETY: `guest` is a writable 64-byte object standing in for the guest
        // pthread object; the shims only read its first word for the type.
        unsafe {
            // PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP: 0x4000.
            let mut m = guest();
            m[0] = 0x00;
            m[1] = 0x40;
            assert_eq!(bionic_static_mutex_type(addr(&m)), MUTEX_RECURSIVE);
            assert_eq!(pthread_mutex_lock(addr(&m)), 0);
            assert_eq!(pthread_mutex_lock(addr(&m)), 0, "static recursive re-lock");
            assert_eq!(pthread_mutex_unlock(addr(&m)), 0);
            assert_eq!(pthread_mutex_unlock(addr(&m)), 0);

            // PTHREAD_MUTEX_INITIALIZER (all zero) stays a normal mutex.
            let z = guest();
            assert_eq!(bionic_static_mutex_type(addr(&z)), MUTEX_NORMAL);
        }
    }

    #[test]
    fn condattr_monotonic_clock_is_honoured() {
        // SAFETY: shim calls with valid guest storage.
        unsafe {
            let m = guest();
            let c = guest();
            let a = 0i32;
            let ap = &a as *const i32 as usize;
            pthread_mutex_init(addr(&m), std::ptr::null());
            assert_eq!(pthread_condattr_init(ap), 0);
            assert_eq!(pthread_condattr_setclock(ap, libc::CLOCK_MONOTONIC), 0);
            assert_eq!(a, libc::CLOCK_MONOTONIC);

            let mut got = 0i32;
            assert_eq!(pthread_condattr_getclock(ap, &mut got), 0);
            assert_eq!(got, libc::CLOCK_MONOTONIC);

            assert_eq!(pthread_cond_init(addr(&c), ap as *const c_void), 0);
            pthread_mutex_lock(addr(&m));

            // A monotonic 50 ms deadline must actually sleep ~50 ms. Before the
            // fix the condvar was CLOCK_REALTIME and this returned immediately.
            let mut ts: libc::timespec = std::mem::zeroed();
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
            ts.tv_nsec += 50_000_000;
            if ts.tv_nsec >= 1_000_000_000 {
                ts.tv_nsec -= 1_000_000_000;
                ts.tv_sec += 1;
            }
            let start = std::time::Instant::now();
            let rc = pthread_cond_timedwait(addr(&c), addr(&m), &ts);
            let waited = start.elapsed();
            assert_eq!(rc, libc::ETIMEDOUT);
            assert!(
                waited >= std::time::Duration::from_millis(30),
                "monotonic condvar woke early: {waited:?}"
            );
            assert!(waited < std::time::Duration::from_secs(2), "{waited:?}");
            pthread_mutex_unlock(addr(&m));
            pthread_cond_destroy(addr(&c));
            pthread_mutex_destroy(addr(&m));
            pthread_condattr_destroy(ap);
        }
    }

    #[test]
    fn condattr_rejects_unknown_clock() {
        // SAFETY: shim call on plain storage.
        unsafe {
            let a = 0i32;
            let ap = &a as *const i32 as usize;
            assert_eq!(pthread_condattr_setclock(ap, 99), libc::EINVAL);
        }
    }

    #[test]
    fn tls_keys_are_per_thread_and_once_runs_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static ONCE_RUNS: AtomicUsize = AtomicUsize::new(0);
        // `pthread_once` writes the guest word; keep it out of .rodata.
        static mut ONCE: libc::pthread_once_t = 0;

        unsafe extern "C" fn init() {
            ONCE_RUNS.fetch_add(1, Ordering::SeqCst);
        }

        // SAFETY: raw pthread shim calls; the key/value lifetime is the tests'.
        unsafe {
            let once = std::ptr::addr_of_mut!(ONCE);
            assert_eq!(pthread_once(once, Some(init)), 0);
            assert_eq!(pthread_once(once, Some(init)), 0);
            assert_eq!(ONCE_RUNS.load(Ordering::SeqCst), 1);

            let mut key: libc::pthread_key_t = 0;
            assert_eq!(pthread_key_create(&mut key, None), 0);
            let ours = 0x1234usize;
            assert_eq!(pthread_setspecific(key, ours as *const c_void), 0);
            assert_eq!(pthread_getspecific(key) as usize, ours);

            let seen = std::thread::spawn(move || {
                // SAFETY: key is process-global; each thread has its own slot.
                pthread_getspecific(key) as usize
            })
            .join()
            .unwrap();
            assert_eq!(seen, 0, "TLS value leaked across threads");
            assert_eq!(pthread_getspecific(key) as usize, ours);
            pthread_key_delete(key);
        }
    }
}
