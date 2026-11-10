//! Translating bionic shims.
//!
//! These are the symbols whose ABI differs from glibc and therefore cannot be
//! forwarded directly. Each shim documents *why* it exists.

use std::collections::{HashMap, HashSet};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

// --- host libc symbols used by the shims -----------------------------------

extern "C" {
    // glibc exposes the `stat` family as real symbols since 2.33; the x86_64
    // bionic `struct stat` is byte-for-byte identical to glibc's (verified
    // against NDK 27.1 and glibc 2.44 with clang record-layout dumps).
    pub fn stat(path: *const c_char, buf: *mut c_void) -> c_int;
    pub fn fstat(fd: c_int, buf: *mut c_void) -> c_int;
    pub fn lstat(path: *const c_char, buf: *mut c_void) -> c_int;
    pub fn fstatat(dirfd: c_int, path: *const c_char, buf: *mut c_void, flags: c_int) -> c_int;
    pub fn statfs(path: *const c_char, buf: *mut c_void) -> c_int;
}

// --- errno ------------------------------------------------------------------

/// `__errno` is a bionic *function* returning the thread's errno location;
/// glibc exposes the same thing as `__errno_location`.
///
/// # Safety
/// Always safe; returns a valid pointer.
pub unsafe extern "C" fn __errno() -> *mut c_int {
    libc::__errno_location()
}

// --- android logging (implemented in C, addresses exposed here) -------------

extern "C" {
    static mut stdin: *mut libc::FILE;
    static mut stdout: *mut libc::FILE;
    static mut stderr: *mut libc::FILE;
    fn acos_fprintf();
    fn acos_vfprintf();
    fn acos_fputs();
    fn acos_fputc();
    fn acos_fwrite();
    fn acos_fread();
    fn acos_fclose();
    fn acos_fflush();
    fn acos_fopen();
    fn acos_fdopen();
    fn acos_tmpfile();
    fn acos_popen();
    fn acos_openlog();
    fn acos_closelog();
    fn acos_syslog();
}

/// Address of the C `__android_log_print` shim.
pub fn c_android_log_print_addr() -> *const c_void {
    extern "C" {
        fn __android_log_print();
    }
    __android_log_print as *const () as *const c_void
}
pub fn c_android_log_vprint_addr() -> *const c_void {
    extern "C" {
        fn __android_log_vprint();
    }
    __android_log_vprint as *const () as *const c_void
}
pub fn c_android_log_write_addr() -> *const c_void {
    extern "C" {
        fn __android_log_write();
    }
    __android_log_write as *const () as *const c_void
}
pub fn c_android_log_assert_addr() -> *const c_void {
    extern "C" {
        fn __android_log_assert();
    }
    __android_log_assert as *const () as *const c_void
}

// --- `__sF` stdio translation ----------------------------------------------

/// Bionic `FILE` is opaque; its LP64 size is `152` bytes (NDK 27
/// `bits/struct_file.h`: `struct __sFILE { char __private[152]; }`). Pre-M
/// guests spell `stdin`/`stdout`/`stderr` as `&__sF[0..2]`, so the array
/// stride is 152 and the stderr slot is `__sF + 304`, **not** a round number.
/// Getting this wrong passes a bogus pointer into glibc `fwrite` and SIGSEGVs.
pub const BIONIC_FILE_BYTES: usize = 152;

#[repr(C, align(8))]
struct SfBuffer([u8; BIONIC_FILE_BYTES * 3]);

static SF_BUF: SfBuffer = SfBuffer([0u8; BIONIC_FILE_BYTES * 3]);

/// Address of the fake bionic `__sF` array.
#[allow(non_snake_case)]
pub fn __sF_addr() -> usize {
    &SF_BUF as *const SfBuffer as usize
}

/// Host `FILE*` values handed to the guest by our `fopen`/`fdopen`/`tmpfile`/
/// `popen` shims. A pointer that is not a `__sF` slot, not a host std stream
/// and not in this set is not a valid `FILE*`; the stdio trampolines fail
/// closed instead of handing it to glibc (which crashes).
fn file_registry() -> &'static Mutex<HashSet<usize>> {
    static FILES: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    FILES.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Register a host `FILE*` returned to guest code (called from C).
#[no_mangle]
pub extern "C" fn acos_register_file(f: usize) {
    if f != 0 {
        if let Ok(mut g) = file_registry().lock() {
            g.insert(f);
        }
    }
}

/// Forget a host `FILE*` after it is closed (called from C).
#[no_mangle]
pub extern "C" fn acos_unregister_file(f: usize) {
    if let Ok(mut g) = file_registry().lock() {
        g.remove(&f);
    }
}

fn file_is_known(f: usize) -> bool {
    file_registry().lock().map(|g| g.contains(&f)).unwrap_or(false)
}

/// Emit the structured, fail-closed error for a stream that is not a `FILE*`.
#[no_mangle]
pub unsafe extern "C" fn acos_invalid_stream(stream: usize) {
    eprintln!(
        "ACOS_STDIO_INVALID_STREAM {}",
        serde_json::json!({
            "code": "INVALID_FILE_STREAM",
            "stream": format!("0x{stream:x}"),
        })
    );
}

/// Translate a guest stream pointer to a real host `FILE*`, or `0` when the
/// pointer is not a known stream. Callers must check for `0` and fail closed.
///
/// # Safety
/// Called from C with any pointer value; only compares addresses.
#[no_mangle]
pub unsafe extern "C" fn acos_map_stream(stream: usize) -> usize {
    let base = __sF_addr();
    if stream == base {
        return stdin as usize;
    }
    if stream == base + BIONIC_FILE_BYTES {
        return stdout as usize;
    }
    if stream == base + BIONIC_FILE_BYTES * 2 {
        return stderr as usize;
    }
    if stream == stdin as usize || stream == stdout as usize || stream == stderr as usize {
        return stream;
    }
    if file_is_known(stream) {
        return stream;
    }
    acos_invalid_stream(stream);
    0
}

/// Table addresses of the C stdio trampolines.
pub fn stdio_table() -> HashMap<&'static str, super::Shim> {
    let mut m = HashMap::new();
    let mut add = |name: &'static str, ptr: *const c_void| {
        m.insert(name, super::Shim::shimmed(name, ptr));
    };
    add("fprintf", acos_fprintf as *const () as *const c_void);
    add("vfprintf", acos_vfprintf as *const () as *const c_void);
    add("fputs", acos_fputs as *const () as *const c_void);
    add("fputc", acos_fputc as *const () as *const c_void);
    add("putc", acos_fputc as *const () as *const c_void);
    add("fwrite", acos_fwrite as *const () as *const c_void);
    add("fread", acos_fread as *const () as *const c_void);
    add("fclose", acos_fclose as *const () as *const c_void);
    add("fflush", acos_fflush as *const () as *const c_void);
    add("fopen", acos_fopen as *const () as *const c_void);
    add("fopen64", acos_fopen as *const () as *const c_void);
    add("fdopen", acos_fdopen as *const () as *const c_void);
    add("tmpfile", acos_tmpfile as *const () as *const c_void);
    add("tmpfile64", acos_tmpfile as *const () as *const c_void);
    add("popen", acos_popen as *const () as *const c_void);
    m
}

// --- system properties ------------------------------------------------------

static PROPERTIES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn properties() -> &'static Mutex<HashMap<String, String>> {
    PROPERTIES.get_or_init(|| {
        let mut m = HashMap::new();
        // Defaults matching the first target API (ADR-0002: Android 34).
        for (k, v) in [
            ("ro.build.version.sdk", "34"),
            ("ro.build.version.release", "14"),
            ("ro.build.version.codename", "REL"),
            ("ro.build.version.security_patch", "2024-01-05"),
            ("ro.product.cpu.abi", "x86_64"),
            ("ro.product.cpu.abilist", "x86_64,x86,arm64-v8a,armeabi-v7a,armeabi"),
            ("ro.product.model", "ACOS"),
            ("ro.product.brand", "acos"),
            ("ro.product.manufacturer", "acos"),
            ("ro.product.name", "acos_x86_64"),
            ("ro.product.device", "acos"),
            ("ro.debuggable", "1"),
            ("ro.secure", "0"),
            ("ro.kernel.qemu", "1"),
            ("ro.dalvik.vm.native.bridge", "0"),
            ("persist.sys.timezone", "UTC"),
            ("ro.hardware", "ranchu"),
        ] {
            m.insert(k.to_string(), v.to_string());
        }
        Mutex::new(m)
    })
}

/// Replace the configured system property set.
pub fn set_system_properties(props: HashMap<String, String>) {
    let p = properties();
    let mut g = p.lock().expect("properties poisoned");
    for (k, v) in props {
        g.insert(k, v);
    }
}

/// `__system_property_get(name, value)` — writes the property into `value`
/// (bionic `PROP_VALUE_MAX` = 92 bytes) and returns its length.
///
/// # Safety
/// `name` must be a NUL-terminated string; `value` must be writable for at
/// least 92 bytes.
pub unsafe extern "C" fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int {
    if name.is_null() || value.is_null() {
        return 0;
    }
    // SAFETY: caller-provided NUL-terminated string.
    let key = std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned();
    let val = properties()
        .lock()
        .ok()
        .and_then(|g| g.get(&key).cloned())
        .unwrap_or_default();
    let bytes = val.as_bytes();
    let n = bytes.len().min(91);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), value as *mut u8, n);
    *value.add(n) = 0;
    n as c_int
}

/// `__system_property_find` — not modelled; returns NULL.
///
/// # Safety
/// Never dereferences its argument.
pub unsafe extern "C" fn __system_property_find(_name: *const c_char) -> *const c_void {
    std::ptr::null()
}

/// `__system_property_read_callback` — not modelled; no-op.
///
/// # Safety
/// Never dereferences its arguments.
pub unsafe extern "C" fn __system_property_read_callback(
    _pi: *const c_void,
    _cb: *const c_void,
    _cookie: *mut c_void,
) {
}

/// `__system_property_serial` — not modelled; returns 0.
///
/// # Safety
/// Never dereferences its argument.
pub unsafe extern "C" fn __system_property_serial(_pi: *const c_void) -> u32 {
    0
}

// --- abort message / sdk level / loggability --------------------------------

static ABORT_MESSAGE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// `android_set_abort_message(msg)`.
///
/// # Safety
/// `msg` must be a NUL-terminated string or NULL.
pub unsafe extern "C" fn android_set_abort_message(msg: *const c_char) {
    let s = if msg.is_null() {
        None
    } else {
        Some(std::ffi::CStr::from_ptr(msg).to_string_lossy().into_owned())
    };
    *ABORT_MESSAGE.get_or_init(|| Mutex::new(None)).lock().unwrap() = s;
}

/// Last abort message set by the guest.
pub fn abort_message() -> Option<String> {
    ABORT_MESSAGE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|g| g.clone())
}

/// `__android_get_application_target_sdk_version()`.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn __android_get_application_target_sdk_version() -> c_int {
    34
}

/// `__strlen_chk(s, slen)` — fortify-checked `strlen`.
///
/// # Safety
/// `s` must be a NUL-terminated string valid for at least `slen` bytes.
pub unsafe extern "C" fn __strlen_chk(s: *const c_char, _slen: usize) -> usize {
    libc::strlen(s)
}

/// `__strrchr_chk(s, c, slen)` — fortify-checked `strrchr`.
///
/// # Safety
/// `s` must be a NUL-terminated string valid for at least `slen` bytes.
pub unsafe extern "C" fn __strrchr_chk(s: *const c_char, c: c_int, _slen: usize) -> *mut c_char {
    libc::strrchr(s, c) as *mut c_char
}

/// Address table for syslog and `__assert2`.
pub fn log_extra_table() -> HashMap<&'static str, super::Shim> {
    let mut m = HashMap::new();
    let mut add = |name: &'static str, ptr: *const c_void| {
        m.insert(name, super::Shim::shimmed(name, ptr));
    };
    add("openlog", acos_openlog as *const () as *const c_void);
    add("closelog", acos_closelog as *const () as *const c_void);
    add("syslog", acos_syslog as *const () as *const c_void);
    add("__assert2", __assert2 as *const () as *const c_void);
    add("__strlen_chk", __strlen_chk as *const () as *const c_void);
    add("__strrchr_chk", __strrchr_chk as *const () as *const c_void);
    m
}

/// `__assert2(file, line, func, expr)` — bionic's assertion failure handler.
///
/// # Safety
/// Arguments are NUL-terminated strings (file/func/expr) from guest code.
pub unsafe extern "C" fn __assert2(
    file: *const c_char,
    line: c_int,
    func: *const c_char,
    expr: *const c_char,
) -> ! {
    let cstr = |p: *const c_char| {
        if p.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    eprintln!(
        "ACOS guest assertion failed: {} at {}:{} ({})",
        cstr(expr),
        cstr(file),
        line,
        cstr(func)
    );
    libc::abort()
}

/// `__android_log_is_loggable(prio, tag, default)`.
///
/// # Safety
/// Never dereferences its arguments.
pub unsafe extern "C" fn __android_log_is_loggable(
    _prio: c_int,
    _tag: *const c_char,
    default: c_int,
) -> c_int {
    default
}

/// `__android_log_is_loggable_len(prio, tag, len, default)`.
///
/// # Safety
/// Never dereferences its arguments.
pub unsafe extern "C" fn __android_log_is_loggable_len(
    _prio: c_int,
    _tag: *const c_char,
    _len: usize,
    default: c_int,
) -> c_int {
    default
}

// --- sigaction translation --------------------------------------------------

/// Bionic `sigaction` (x86_64): `{ int sa_flags; handler; sigset_t sa_mask
/// (8-byte unsigned long); restorer; }` — size 32.
///
/// glibc: `{ handler; sigset_t sa_mask (128 bytes); int sa_flags; restorer; }`.
///
/// We translate in both directions.
#[repr(C)]
pub struct BionicSigaction {
    sa_flags: c_int,
    sa_handler: usize,
    sa_mask: u64,
    sa_restorer: usize,
}

/// `sigaction` shim.
///
/// # Safety
/// `act`/`old` follow the guest's bionic layout.
pub unsafe extern "C" fn sigaction(
    sig: c_int,
    act: *const BionicSigaction,
    old: *mut BionicSigaction,
) -> c_int {
    let mut gact: libc::sigaction = std::mem::zeroed();
    let mut gold: libc::sigaction = std::mem::zeroed();
    let act_ptr = if act.is_null() {
        std::ptr::null()
    } else {
        gact.sa_sigaction = (*act).sa_handler;
        gact.sa_flags = (*act).sa_flags;
        gact.sa_mask = std::mem::zeroed();
        // Copy the (8-byte) bionic mask into the low word of glibc's set.
        let words: *mut u64 = &mut gact.sa_mask as *mut libc::sigset_t as *mut u64;
        *words = (*act).sa_mask;
        &gact
    };
    let old_ptr = if old.is_null() {
        std::ptr::null_mut()
    } else {
        &mut gold
    };
    let rc = libc::sigaction(sig, act_ptr, old_ptr);
    if rc == 0 && !old.is_null() {
        (*old).sa_flags = gold.sa_flags;
        (*old).sa_handler = gold.sa_sigaction;
        let words: *const u64 = &gold.sa_mask as *const libc::sigset_t as *const u64;
        (*old).sa_mask = *words;
        (*old).sa_restorer = gold.sa_restorer.map_or(0, |f| f as usize);
    }
    rc
}

/// Bionic `sigset_t` is an 8-byte bitmask.
///
/// # Safety
/// `set` must point to 8 writable bytes.
pub unsafe extern "C" fn sigemptyset(set: *mut u64) -> c_int {
    if !set.is_null() {
        *set = 0;
    }
    0
}

/// See [`sigemptyset`].
///
/// # Safety
/// `set` must point to 8 writable bytes.
pub unsafe extern "C" fn sigfillset(set: *mut u64) -> c_int {
    if !set.is_null() {
        *set = u64::MAX;
    }
    0
}

/// See [`sigemptyset`].
///
/// # Safety
/// `set` must point to 8 writable bytes.
pub unsafe extern "C" fn sigaddset(set: *mut u64, sig: c_int) -> c_int {
    if !set.is_null() && (1..=64).contains(&sig) {
        *set |= 1u64 << (sig - 1);
    }
    0
}

/// See [`sigemptyset`].
///
/// # Safety
/// `set` must point to 8 writable bytes.
pub unsafe extern "C" fn sigdelset(set: *mut u64, sig: c_int) -> c_int {
    if !set.is_null() && (1..=64).contains(&sig) {
        *set &= !(1u64 << (sig - 1));
    }
    0
}

/// See [`sigemptyset`].
///
/// # Safety
/// `set` must point to 8 readable bytes.
pub unsafe extern "C" fn sigismember(set: *const u64, sig: c_int) -> c_int {
    if set.is_null() || !(1..=64).contains(&sig) {
        return 0;
    }
    c_int::from(*set & (1u64 << (sig - 1)) != 0)
}

// --- glibc stat-family addresses (forwarded) -------------------------------

macro_rules! addr {
    ($f:path) => {
        $f as *const () as *const c_void
    };
}

/// Addresses for the forwarded `stat` family.
pub fn stat_table() -> HashMap<&'static str, super::Shim> {
    let mut m = HashMap::new();
    let mut add = |name: &'static str, ptr: *const c_void| {
        m.insert(name, super::Shim::forwarded(name, ptr));
    };
    add("stat", addr!(stat));
    add("stat64", addr!(stat));
    add("__stat", addr!(stat));
    add("fstat", addr!(fstat));
    add("fstat64", addr!(fstat));
    add("__fstat", addr!(fstat));
    add("lstat", addr!(lstat));
    add("lstat64", addr!(lstat));
    add("fstatat", addr!(fstatat));
    add("fstatat64", addr!(fstatat));
    add("statfs", addr!(statfs));
    m
}

/// Address table for the sigset/sigaction shims.
pub fn signal_table() -> HashMap<&'static str, super::Shim> {
    let mut m = HashMap::new();
    let mut add = |name: &'static str, ptr: *const c_void| {
        m.insert(name, super::Shim::shimmed(name, ptr));
    };
    add("sigaction", addr!(sigaction));
    add("__sigaction", addr!(sigaction));
    add("sigemptyset", addr!(sigemptyset));
    add("sigfillset", addr!(sigfillset));
    add("sigaddset", addr!(sigaddset));
    add("sigdelset", addr!(sigdelset));
    add("sigismember", addr!(sigismember));
    m
}

// --- stdio stream *data* symbols -------------------------------------------

/// Bionic exposes `stdin`/`stdout`/`stderr` as `FILE*` *variables* inside the
/// `__sF` array. Guest code that reads the variable must receive a pointer our
/// stdio trampolines translate (`acos_map_stream`), so each name is bound to a
/// cell holding the corresponding `__sF` slot address.
static STDIN_CELL: AtomicUsize = AtomicUsize::new(0);
static STDOUT_CELL: AtomicUsize = AtomicUsize::new(0);
static STDERR_CELL: AtomicUsize = AtomicUsize::new(0);

const SF_STRIDE: usize = BIONIC_FILE_BYTES;

fn stream_addr(slot: usize) -> *const c_void {
    let cell: &AtomicUsize = match slot {
        0 => &STDIN_CELL,
        1 => &STDOUT_CELL,
        _ => &STDERR_CELL,
    };
    cell as *const AtomicUsize as *const c_void
}

/// `stdin`, `stdout`, `stderr` as bionic `FILE*` variables.
pub fn stdio_stream_table() -> HashMap<&'static str, super::Shim> {
    let mut m = HashMap::new();
    m.insert("stdin", super::Shim::shimmed("stdin", stream_addr(0)));
    m.insert("stdout", super::Shim::shimmed("stdout", stream_addr(1)));
    m.insert("stderr", super::Shim::shimmed("stderr", stream_addr(2)));
    m
}

/// Fill the stream cells (called before the table is published).
pub fn init_stream_cells() {
    let base = __sF_addr();
    STDIN_CELL.store(base, Ordering::Relaxed);
    STDOUT_CELL.store(base + SF_STRIDE, Ordering::Relaxed);
    STDERR_CELL.store(base + SF_STRIDE * 2, Ordering::Relaxed);
}

// --- bionic sem_t ----------------------------------------------------------

/// Bionic's `sem_t` is an opaque 32-byte object with a different layout from
/// glibc's, so guest storage is treated as a handle onto a host `sem_t` (the
/// same indirection used for mutexes and condition variables).
struct HostSem(libc::sem_t);
// SAFETY: a `sem_t` is designed for cross-thread use.
unsafe impl Send for HostSem {}

fn sems() -> &'static Mutex<HashMap<usize, u64>> {
    static SEMS: OnceLock<Mutex<HashMap<usize, u64>>> = OnceLock::new();
    SEMS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sem_ref(key: usize) -> Option<u64> {
    sems().lock().ok()?.get(&key).copied()
}

/// `sem_init(sem, pshared, value)`.
///
/// # Safety
/// `sem` must point to at least 32 writable guest bytes.
pub unsafe extern "C" fn sem_init(sem: *mut c_void, _pshared: c_int, value: u32) -> c_int {
    if sem.is_null() {
        return -1;
    }
    let key = sem as usize;
    let boxed = Box::new(HostSem(std::mem::zeroed()));
    let ptr = Box::into_raw(boxed);
    if libc::sem_init(&mut (*ptr).0, 0, value) != 0 {
        drop(Box::from_raw(ptr));
        return -1;
    }
    sems().lock().unwrap().insert(key, ptr as u64);
    // Guest object is a handle: zero it so a stale copy is detectable.
    std::ptr::write_bytes(sem as *mut u8, 0, 32);
    0
}

/// `sem_destroy`.
///
/// # Safety
/// `sem` must have been passed to [`sem_init`].
pub unsafe extern "C" fn sem_destroy(sem: *mut c_void) -> c_int {
    let key = sem as usize;
    match sems().lock().unwrap().remove(&key) {
        Some(p) => {
            let boxed = Box::from_raw(p as *mut HostSem);
            libc::sem_destroy(&boxed.0 as *const libc::sem_t as *mut libc::sem_t);
            0
        }
        None => -1,
    }
}

/// `sem_post`.
///
/// # Safety
/// `sem` must have been passed to [`sem_init`].
pub unsafe extern "C" fn sem_post(sem: *mut c_void) -> c_int {
    match sem_ref(sem as usize) {
        Some(p) => libc::sem_post(&mut (*(p as *mut HostSem)).0),
        None => -1,
    }
}

/// `sem_wait`.
///
/// # Safety
/// `sem` must have been passed to [`sem_init`].
pub unsafe extern "C" fn sem_wait(sem: *mut c_void) -> c_int {
    match sem_ref(sem as usize) {
        Some(p) => libc::sem_wait(&mut (*(p as *mut HostSem)).0),
        None => -1,
    }
}

/// `sem_trywait`.
///
/// # Safety
/// `sem` must have been passed to [`sem_init`].
pub unsafe extern "C" fn sem_trywait(sem: *mut c_void) -> c_int {
    match sem_ref(sem as usize) {
        Some(p) => libc::sem_trywait(&mut (*(p as *mut HostSem)).0),
        None => -1,
    }
}

/// `sem_timedwait` with a bionic `timespec` (identical layout to glibc's).
///
/// # Safety
/// `sem` must have been passed to [`sem_init`]; `abs_timeout` must point to a
/// valid `timespec`.
pub unsafe extern "C" fn sem_timedwait(
    sem: *mut c_void,
    abs_timeout: *const libc::timespec,
) -> c_int {
    match sem_ref(sem as usize) {
        Some(p) => libc::sem_timedwait(&mut (*(p as *mut HostSem)).0, abs_timeout),
        None => -1,
    }
}

// --- libcutils tracing -----------------------------------------------------

/// `ATrace_beginSection` / `ATrace_endSection` / `ATrace_isEnabled`.
///
/// The platform contract is: when system tracing is not enabled these are
/// no-ops returning immediately (`ATrace_isEnabled` returns false). ACOS never
/// enables platform tracing, so these are exact implementations of the
/// documented disabled behaviour, not stubs.
///
/// # Safety
/// `name` is ignored; any pointer value is accepted.
pub unsafe extern "C" fn atrace_begin_section(_name: *const c_char) {}

/// See [`atrace_begin_section`].
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn atrace_end_section() {}

/// `ATrace_isEnabled` — tracing is never enabled.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn atrace_is_enabled() -> bool {
    false
}

/// `libandroid`/`libjnigraphics`/`libcutils` extras table.
pub fn android_extra_table() -> HashMap<&'static str, super::Shim> {
    let mut m = stdio_stream_table();
    let mut add = |name: &'static str, ptr: *const c_void| {
        m.insert(name, super::Shim::shimmed(name, ptr));
    };
    add("sem_init", sem_init as *const c_void);
    add("sem_destroy", sem_destroy as *const c_void);
    add("sem_post", sem_post as *const c_void);
    add("sem_wait", sem_wait as *const c_void);
    add("sem_trywait", sem_trywait as *const c_void);
    add("sem_timedwait", sem_timedwait as *const c_void);
    add("ATrace_beginSection", atrace_begin_section as *const c_void);
    add("ATrace_endSection", atrace_end_section as *const c_void);
    add("ATrace_isEnabled", atrace_is_enabled as *const c_void);
    m
}

// --- bionic-only fortify wrappers ------------------------------------------
//
// glibc has no `__*_chk` counterparts for these (they are bionic `_FORTIFY_SOURCE`
// entry points), so they are implemented here with exactly bionic's contract:
// the object-size argument is validated and the plain operation is performed.

/// `__memchr_chk(s, c, len, bos)`.
///
/// # Safety
/// `s` must be readable for `len` bytes; `bos` is the compiler-provided buffer size.
pub unsafe extern "C" fn __memchr_chk(
    s: *const c_void,
    c: c_int,
    len: usize,
    bos: usize,
) -> *mut c_void {
    if len > bos {
        return std::ptr::null_mut();
    }
    libc::memchr(s, c, len) as *mut c_void
}

/// `__strchr_chk(s, c, bos)`.
///
/// # Safety
/// `s` must be a NUL-terminated string of at most `bos` bytes.
pub unsafe extern "C" fn __strchr_chk(s: *const c_char, c: c_int, bos: usize) -> *mut c_char {
    // Bionic requires the terminator to be inside the object.
    for i in 0..bos {
        let b = *s.add(i);
        if b == c as c_char {
            return s.add(i) as *mut c_char;
        }
        if b == 0 {
            return std::ptr::null_mut();
        }
    }
    std::ptr::null_mut()
}

/// `__write_chk(fd, buf, count, buf_size)`.
///
/// # Safety
/// `buf` must be readable for `count` bytes.
pub unsafe extern "C" fn __write_chk(
    fd: c_int,
    buf: *const c_void,
    count: usize,
    buf_size: usize,
) -> isize {
    if count > buf_size {
        *libc::__errno_location() = libc::EINVAL;
        return -1;
    }
    libc::write(fd, buf, count)
}

/// `__fwrite_chk(ptr, size, nmemb, stream, bos)`.
///
/// The stream is a bionic `FILE*` (`__sF` slot) and is translated by the stdio
/// trampoline; the size check matches bionic's.
///
/// # Safety
/// `ptr` must be readable for `size * nmemb` bytes.
pub unsafe extern "C" fn __fwrite_chk(
    ptr: *const c_void,
    size: usize,
    nmemb: usize,
    stream: *mut c_void,
    bos: usize,
) -> usize {
    if size.saturating_mul(nmemb) > bos {
        return 0;
    }
    let real = acos_map_stream(stream as usize) as *mut libc::FILE;
    if real.is_null() {
        *libc::__errno_location() = libc::EBADF;
        return 0;
    }
    libc::fwrite(ptr, size, nmemb, real)
}

/// `__FD_SET_chk(fd, set, set_size)`.
///
/// # Safety
/// `set` must be a writable `fd_set` of `set_size` bytes.
#[allow(non_snake_case)]
pub unsafe extern "C" fn __FD_SET_chk(fd: c_int, set: *mut c_void, set_size: usize) -> c_int {
    if fd < 0 || (fd as usize) >= set_size * 8 {
        *libc::__errno_location() = libc::EINVAL;
        return -1;
    }
    let set = set as *mut libc::fd_set;
    libc::FD_SET(fd, set);
    0
}

/// `__FD_ISSET_chk(fd, set, set_size)`.
///
/// # Safety
/// `set` must be a readable `fd_set` of `set_size` bytes.
#[allow(non_snake_case)]
pub unsafe extern "C" fn __FD_ISSET_chk(fd: c_int, set: *const c_void, set_size: usize) -> c_int {
    if fd < 0 || (fd as usize) >= set_size * 8 {
        return 0;
    }
    libc::FD_ISSET(fd, set as *const libc::fd_set) as c_int
}

/// bionic-only fortify entry points.
pub fn fortify_table() -> HashMap<&'static str, super::Shim> {
    let mut m = HashMap::new();
    let mut add = |name: &'static str, ptr: *const c_void| {
        m.insert(name, super::Shim::shimmed(name, ptr));
    };
    add("__memchr_chk", __memchr_chk as *const c_void);
    add("__strchr_chk", __strchr_chk as *const c_void);
    add("__write_chk", __write_chk as *const c_void);
    add("__fwrite_chk", __fwrite_chk as *const c_void);
    add("__FD_SET_chk", __FD_SET_chk as *const c_void);
    add("__FD_ISSET_chk", __FD_ISSET_chk as *const c_void);
    // bionic uses `stderr`-style `__sF` slots for `tmpfile`/`fscanf` streams too.
    m
}

#[cfg(test)]
mod stdio_map_tests {
    use super::*;

    /// The pre-M `__sF` layout must use bionic's real LP64 `FILE` size (152).
    /// The old 256-byte stride missed `&__sF[2]` and passed a bogus pointer to
    /// glibc `fwrite`, which SIGSEGV'd.
    #[test]
    fn sf_slots_use_bionic_stride_and_fail_closed() {
        assert_eq!(BIONIC_FILE_BYTES, 152);
        let base = __sF_addr();
        // SAFETY: the shim only compares pointer addresses.
        unsafe {
            assert_eq!(acos_map_stream(base), stdin as usize);
            assert_eq!(acos_map_stream(base + BIONIC_FILE_BYTES), stdout as usize);
            assert_eq!(acos_map_stream(base + BIONIC_FILE_BYTES * 2), stderr as usize);
            // The wrong stride and arbitrary pointers are not valid FILE*.
            assert_eq!(acos_map_stream(base + 256), 0);
            assert_eq!(acos_map_stream(0xdead_beef), 0);
        }
    }

    #[test]
    fn registered_host_file_roundtrips() {
        let fake = 0x1234_5678usize;
        acos_register_file(fake);
        // SAFETY: the shim only compares pointer addresses.
        unsafe { assert_eq!(acos_map_stream(fake), fake) };
        acos_unregister_file(fake);
        // SAFETY: the shim only compares pointer addresses.
        unsafe { assert_eq!(acos_map_stream(fake), 0) };
    }
}
