//! Translating bionic shims.
//!
//! These are the symbols whose ABI differs from glibc and therefore cannot be
//! forwarded directly. Each shim documents *why* it exists.

use std::collections::HashMap;
use std::os::raw::{c_char, c_int, c_void};
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

/// Bionic `FILE` is opaque to us; we only compare stream pointers by address.
const SF_STREAM_BYTES: usize = 256;

#[repr(C, align(16))]
struct SfBuffer([u8; SF_STREAM_BYTES * 3]);

static SF_BUF: SfBuffer = SfBuffer([0u8; SF_STREAM_BYTES * 3]);

/// Address of the fake bionic `__sF` array.
#[allow(non_snake_case)]
pub fn __sF_addr() -> usize {
    &SF_BUF as *const SfBuffer as usize
}

/// Translate a guest stream pointer to a real host `FILE*`.
///
/// # Safety
/// Called from C with any pointer value; only compares addresses.
#[no_mangle]
pub unsafe extern "C" fn acos_map_stream(stream: usize) -> usize {
    let base = __sF_addr();
    if stream == base {
        stdin as usize
    } else if stream == base + SF_STREAM_BYTES {
        stdout as usize
    } else if stream == base + SF_STREAM_BYTES * 2 {
        stderr as usize
    } else {
        stream
    }
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
