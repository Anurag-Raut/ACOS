//! B-LOADER PATCH: the bionic/NDK surface real ART needs.
//!
//! This module exists only in the research vendored copy of
//! `acos-native-loader`. It is the empirical gap list from `gapscan` over the
//! Android 14 x86_64 `libart.so` closure, split three ways:
//!
//! * **forwarded** — the bionic and host ABIs agree, so the symbol resolves to
//!   the host libc / libgcc_s / zlib implementation (resolved at runtime with
//!   `dlsym`, so a missing provider degrades to "symbol still missing" instead
//!   of a link failure);
//! * **shimmed** — bionic-only entry points (`*64` signal calls,
//!   `android_fdsan_*`, `android_get_device_api_level`, liblog's event API,
//!   fortify `__*_chk` variants) implemented here;
//! * **denied-by-sandbox** — `mount`, `unshare`, `ptrace`, `process_vm_readv`,
//!   `execve`: forwarded to the host and expected to fail under the ACOS
//!   seccomp filter; ART only reaches them on paths that need root/debugger.

use std::collections::HashMap;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

use super::{Category, Shim};

/// Resolve a host symbol from a shared object at runtime (cached).
fn host_sym(lib: &str, name: &str) -> *const c_void {
    static CACHE: OnceLock<Mutex<HashMap<(String, String), usize>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (lib.to_string(), name.to_string());
    if let Some(p) = cache.lock().unwrap().get(&key) {
        return *p as *const c_void;
    }
    let so = std::ffi::CString::new(lib).unwrap();
    let nm = std::ffi::CString::new(name).unwrap();
    // SAFETY: dlopen/dlsym with valid C strings; the handle is intentionally
    // kept open for the process lifetime (RTLD_NODELETE semantics).
    let ptr = unsafe {
        let h = libc::dlopen(so.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if h.is_null() {
            std::ptr::null()
        } else {
            libc::dlsym(h, nm.as_ptr())
        }
    };
    cache.lock().unwrap().insert(key, ptr as usize);
    ptr
}

macro_rules! fwd_host {
    ($m:expr, $lib:literal, $($name:literal),+ $(,)?) => {
        $(
            let p = host_sym($lib, $name);
            if !p.is_null() {
                $m.insert($name, Shim {
                    name: $name,
                    ptr: p,
                    category: Category::Forwarded,
                });
            }
        )+
    };
}

macro_rules! shim {
    ($m:expr, $name:literal, $f:expr) => {
        $m.insert(
            $name,
            Shim {
                name: $name,
                ptr: $f as *const () as *const c_void,
                category: Category::Shimmed,
            },
        );
    };
}

/// The ART gap table.
pub fn table() -> HashMap<&'static str, Shim> {
    let mut m: HashMap<&'static str, Shim> = HashMap::new();

    // --- host libc: ABI-identical, just absent from the bionic table --------
    fwd_host!(
        m,
        "libc.so.6",
        "writev",
        "mincore",
        "fallocate",
        "mallinfo",
        "getpriority",
        "setpgid",
        "getpwnam",
        "basename",
        "fgetc",
        "getline",
        "mkdtemp",
        "mkostemps",
        "wcscoll",
        "wcsxfrm",
        "iswalpha",
        "iswblank",
        "iswcntrl",
        "iswdigit",
        "iswprint",
        "iswpunct",
        "iswspace",
        "iswupper",
        "iswxdigit",
        "towlower",
        "towupper",
        "strtod_l",
        "strtof_l",
        "fstatfs",
        "tgkill",
        "process_vm_readv",
        "ptrace",
        "pidfd_open",
        "waitid",
        "sched_getscheduler",
        "sched_getparam",
        "pthread_getcpuclockid",
        "sigwait",
        "pthread_sigmask",
        "__libc_current_sigrtmin",
        "__libc_current_sigrtmax",
        // Sandbox-denied syscalls: forwarded so the *filter* is the enforcement
        // point, not a link-time gap. ART only calls these on root/debugger
        // paths that ACOS never enables.
        "mount",
        "umount2",
        "unshare",
        "setns",
        "execv",
        "execve",
        // libselinux / libcutils plumbing reached through libartpalette-system
        "stpcpy",
        "fnmatch",
        "getpwuid",
        "fts_open",
        "fts_read",
        "fts_set",
        "fts_close",
        "getpgid",
        "lchown",
        "fgetxattr",
        "getxattr",
        "setxattr",
        "fsetxattr",
        "lgetxattr",
        "lsetxattr",
        "__fsetlocking",
        "memfd_create",
        "strtoimax",
        "in6addr_any",
    );

    // --- libgcc_s: bionic's libc.so exports the unwinder ---------------------
    fwd_host!(
        m,
        "libgcc_s.so.1",
        "_Unwind_RaiseException",
        "_Unwind_DeleteException",
        "_Unwind_Resume",
        "_Unwind_GetLanguageSpecificData",
        "_Unwind_GetIP",
        "_Unwind_GetIPInfo",
        "_Unwind_GetRegionStart",
        "_Unwind_GetTextRelBase",
        "_Unwind_GetDataRelBase",
        "_Unwind_SetGR",
        "_Unwind_SetIP",
        "_Unwind_Backtrace",
    );

    // --- zlib: libz.so is a shim system lib; ART + libprofile use raw zlib ---
    fwd_host!(
        m,
        "libz.so.1",
        "adler32",
        "adler32_combine",
        "inflate",
        "inflateEnd",
        "inflateInit_",
        "inflateInit2_",
        "deflate",
        "deflateEnd",
        "deflateInit_",
        "deflateBound",
    );

    // --- bionic-only signal surface ----------------------------------------
    // On x86_64 bionic `sigset_t` and `sigset64_t` are both one 64-bit word, so
    // the `*64` spellings are the same operations.
    shim!(m, "sigaction64", super::shims::sigaction);
    shim!(m, "sigemptyset64", super::shims::sigemptyset);
    shim!(m, "sigfillset64", super::shims::sigfillset);
    shim!(m, "sigaddset64", super::shims::sigaddset);
    shim!(m, "sigdelset64", super::shims::sigdelset);
    shim!(m, "sigismember64", super::shims::sigismember);
    shim!(m, "sigwait64", sigwait64);
    shim!(m, "sigprocmask64", sigprocmask64);
    shim!(m, "sigprocmask", sigprocmask64);
    shim!(m, "pthread_sigmask64", pthread_sigmask64);
    shim!(m, "pthread_gettid_np", pthread_gettid_np);
    shim!(m, "android_handle_signal", android_handle_signal);

    // --- bionic / libbase / liblog -----------------------------------------
    shim!(m, "android_get_device_api_level", android_get_device_api_level);
    shim!(m, "android_fdsan_create_owner_tag", fsan_create_owner_tag);
    shim!(m, "android_fdsan_exchange_owner_tag", fsan_exchange_owner_tag);
    shim!(m, "android_fdsan_close_with_tag", fsan_close_with_tag);
    shim!(m, "__system_property_set", property_set);
    shim!(m, "__system_property_wait", property_wait);
    shim!(m, "__system_property_area_serial", property_area_serial);
    shim!(m, "__android_log_error_write", log_error_write);
    shim!(m, "__android_log_bswrite", log_bswrite);
    shim!(m, "__system_properties_init", system_properties_init);
    shim!(m, "atrace_get_enabled_tags", atrace_get_enabled_tags);
    shim!(m, "atrace_begin_body", atrace_noop1);
    shim!(m, "atrace_end_body", atrace_end_body);
    shim!(m, "atrace_int_body", atrace_int_body);
    shim!(m, "ashmem_create_region", ashmem_create_region);
    shim!(m, "ashmem_set_prot_region", ashmem_set_prot_region);
    // `_ctype_` is an OBJECT (data) import: the GOT entry must receive the
    // table address, not a function address.
    m.insert(
        "_ctype_",
        Shim {
            name: "_ctype_",
            ptr: CTYPE_TABLE.as_ptr() as *const c_void,
            category: Category::Shimmed,
        },
    );
    shim!(m, "create_android_logger", create_android_logger);
    shim!(m, "android_log_destroy", android_log_destroy);
    shim!(m, "android_log_write_int32", android_log_write_int32);
    shim!(m, "android_log_write_string8", android_log_write_string8);
    shim!(m, "android_log_write_string8_len", android_log_write_string8_len);
    shim!(m, "android_log_write_list", android_log_noop3);
    shim!(m, "__android_log_set_logger", log_set_logger);
    shim!(m, "__android_log_logd_logger", log_logd_logger);
    shim!(m, "__android_log_set_aborter", log_set_aborter);
    shim!(m, "__android_log_call_aborter", log_call_aborter);
    shim!(m, "__android_log_get_minimum_priority", log_get_min_priority);
    shim!(m, "__android_log_set_minimum_priority", log_set_min_priority);
    shim!(m, "__android_log_set_default_tag", log_set_default_tag);
    shim!(m, "__android_log_write_log_message", log_write_message);
    shim!(m, "getprogname", getprogname);
    shim!(m, "sysconf", art_sysconf);
    shim!(m, "android_fdsan_get_owner_tag", fsan_get_owner_tag);
    shim!(m, "__system_property_foreach", property_foreach);
    shim!(m, "__hwasan_tag_pointer", hwasan_tag_pointer);
    shim!(m, "__assert", bionic_assert);
    shim!(m, "__strlcpy_chk", strlcpy_chk);
    shim!(m, "__strncpy_chk2", strncpy_chk2);
    shim!(m, "__pwrite_chk", pwrite_chk);
    shim!(m, "__pwrite64_chk", pwrite_chk);
    shim!(m, "__pread64_chk", pread_chk);

    m
}

/// B-LOADER PATCH: bionic and glibc number the `sysconf` names differently
/// (bionic `_SC_PAGE_SIZE` = 0x28, glibc `_SC_SEM_NSEMS_MAX` = 40; glibc's
/// `_SC_PAGE_SIZE` = 30). ART compares `sysconf(_SC_PAGE_SIZE)` to 4096 at
/// startup, so a raw forward aborts with "Check failed ... =255". The table
/// below is generated from NDK 27 `bits/sysconf.h` and glibc
/// `bits/confname.h` : bionic value -> glibc value.
unsafe extern "C" fn art_sysconf(name: c_int) -> libc::c_long {
    let host = match name {
        0x0001 => 36, // _SC_BC_BASE_MAX
        0x0002 => 37, // _SC_BC_DIM_MAX
        0x0003 => 38, // _SC_BC_SCALE_MAX
        0x0004 => 39, // _SC_BC_STRING_MAX
        0x0005 => 1, // _SC_CHILD_MAX
        0x0006 => 2, // _SC_CLK_TCK
        0x0007 => 40, // _SC_COLL_WEIGHTS_MAX
        0x0008 => 42, // _SC_EXPR_NEST_MAX
        0x0009 => 43, // _SC_LINE_MAX
        0x000a => 3, // _SC_NGROUPS_MAX
        0x000b => 4, // _SC_OPEN_MAX
        0x000c => 88, // _SC_PASS_MAX
        0x000d => 47, // _SC_2_C_BIND
        0x000e => 48, // _SC_2_C_DEV
        0x000f => 96, // _SC_2_C_VERSION
        0x0010 => 95, // _SC_2_CHAR_TERM
        0x0011 => 49, // _SC_2_FORT_DEV
        0x0012 => 50, // _SC_2_FORT_RUN
        0x0013 => 52, // _SC_2_LOCALEDEF
        0x0014 => 51, // _SC_2_SW_DEV
        0x0015 => 97, // _SC_2_UPE
        0x0016 => 46, // _SC_2_VERSION
        0x0017 => 7, // _SC_JOB_CONTROL
        0x0018 => 8, // _SC_SAVED_IDS
        0x0019 => 29, // _SC_VERSION
        0x001a => 44, // _SC_RE_DUP_MAX
        0x001b => 5, // _SC_STREAM_MAX
        0x001c => 6, // _SC_TZNAME_MAX
        0x001d => 92, // _SC_XOPEN_CRYPT
        0x001e => 93, // _SC_XOPEN_ENH_I18N
        0x001f => 94, // _SC_XOPEN_SHM
        0x0020 => 89, // _SC_XOPEN_VERSION
        0x0021 => 90, // _SC_XOPEN_XCU_VERSION
        0x0022 => 130, // _SC_XOPEN_REALTIME
        0x0023 => 131, // _SC_XOPEN_REALTIME_THREADS
        0x0024 => 129, // _SC_XOPEN_LEGACY
        0x0025 => 87, // _SC_ATEXIT_MAX
        0x0026 => 60, // _SC_IOV_MAX
        0x0027 => 30, // _SC_PAGESIZE
        0x0028 => 30, // _SC_PAGE_SIZE
        0x0029 => 91, // _SC_XOPEN_UNIX
        0x002a => 125, // _SC_XBS5_ILP32_OFF32
        0x002b => 126, // _SC_XBS5_ILP32_OFFBIG
        0x002c => 127, // _SC_XBS5_LP64_OFF64
        0x002d => 128, // _SC_XBS5_LPBIG_OFFBIG
        0x002e => 23, // _SC_AIO_LISTIO_MAX
        0x002f => 24, // _SC_AIO_MAX
        0x0030 => 25, // _SC_AIO_PRIO_DELTA_MAX
        0x0031 => 26, // _SC_DELAYTIMER_MAX
        0x0032 => 27, // _SC_MQ_OPEN_MAX
        0x0033 => 28, // _SC_MQ_PRIO_MAX
        0x0034 => 31, // _SC_RTSIG_MAX
        0x0035 => 32, // _SC_SEM_NSEMS_MAX
        0x0036 => 33, // _SC_SEM_VALUE_MAX
        0x0037 => 34, // _SC_SIGQUEUE_MAX
        0x0038 => 35, // _SC_TIMER_MAX
        0x0039 => 12, // _SC_ASYNCHRONOUS_IO
        0x003a => 15, // _SC_FSYNC
        0x003b => 16, // _SC_MAPPED_FILES
        0x003c => 17, // _SC_MEMLOCK
        0x003d => 18, // _SC_MEMLOCK_RANGE
        0x003e => 19, // _SC_MEMORY_PROTECTION
        0x003f => 20, // _SC_MESSAGE_PASSING
        0x0040 => 13, // _SC_PRIORITIZED_IO
        0x0041 => 10, // _SC_PRIORITY_SCHEDULING
        0x0042 => 9, // _SC_REALTIME_SIGNALS
        0x0043 => 21, // _SC_SEMAPHORES
        0x0044 => 22, // _SC_SHARED_MEMORY_OBJECTS
        0x0045 => 14, // _SC_SYNCHRONIZED_IO
        0x0046 => 11, // _SC_TIMERS
        0x0047 => 69, // _SC_GETGR_R_SIZE_MAX
        0x0048 => 70, // _SC_GETPW_R_SIZE_MAX
        0x0049 => 71, // _SC_LOGIN_NAME_MAX
        0x004a => 73, // _SC_THREAD_DESTRUCTOR_ITERATIONS
        0x004b => 74, // _SC_THREAD_KEYS_MAX
        0x004c => 75, // _SC_THREAD_STACK_MIN
        0x004d => 76, // _SC_THREAD_THREADS_MAX
        0x004e => 72, // _SC_TTY_NAME_MAX
        0x004f => 67, // _SC_THREADS
        0x0050 => 77, // _SC_THREAD_ATTR_STACKADDR
        0x0051 => 78, // _SC_THREAD_ATTR_STACKSIZE
        0x0052 => 79, // _SC_THREAD_PRIORITY_SCHEDULING
        0x0053 => 80, // _SC_THREAD_PRIO_INHERIT
        0x0054 => 81, // _SC_THREAD_PRIO_PROTECT
        0x0055 => 68, // _SC_THREAD_SAFE_FUNCTIONS
        0x0060 => 83, // _SC_NPROCESSORS_CONF
        0x0061 => 84, // _SC_NPROCESSORS_ONLN
        0x0062 => 85, // _SC_PHYS_PAGES
        0x0063 => 86, // _SC_AVPHYS_PAGES
        0x0064 => 149, // _SC_MONOTONIC_CLOCK
        0x0065 => 168, // _SC_2_PBS
        0x0066 => 169, // _SC_2_PBS_ACCOUNTING
        0x0067 => 175, // _SC_2_PBS_CHECKPOINT
        0x0068 => 170, // _SC_2_PBS_LOCATE
        0x0069 => 171, // _SC_2_PBS_MESSAGE
        0x006a => 172, // _SC_2_PBS_TRACK
        0x006b => 132, // _SC_ADVISORY_INFO
        0x006c => 133, // _SC_BARRIERS
        0x006d => 137, // _SC_CLOCK_SELECTION
        0x006e => 138, // _SC_CPUTIME
        0x006f => 180, // _SC_HOST_NAME_MAX
        0x0070 => 235, // _SC_IPV6
        0x0071 => 236, // _SC_RAW_SOCKETS
        0x0072 => 153, // _SC_READER_WRITER_LOCKS
        0x0073 => 155, // _SC_REGEXP
        0x0074 => 157, // _SC_SHELL
        0x0075 => 159, // _SC_SPAWN
        0x0076 => 154, // _SC_SPIN_LOCKS
        0x0077 => 160, // _SC_SPORADIC_SERVER
        0x0078 => 241, // _SC_SS_REPL_MAX
        0x0079 => 173, // _SC_SYMLOOP_MAX
        0x007a => 139, // _SC_THREAD_CPUTIME
        0x007b => 82, // _SC_THREAD_PROCESS_SHARED
        0x007c => 247, // _SC_THREAD_ROBUST_PRIO_INHERIT
        0x007d => 248, // _SC_THREAD_ROBUST_PRIO_PROTECT
        0x007e => 161, // _SC_THREAD_SPORADIC_SERVER
        0x007f => 164, // _SC_TIMEOUTS
        0x0080 => 181, // _SC_TRACE
        0x0081 => 182, // _SC_TRACE_EVENT_FILTER
        0x0082 => 242, // _SC_TRACE_EVENT_NAME_MAX
        0x0083 => 183, // _SC_TRACE_INHERIT
        0x0084 => 184, // _SC_TRACE_LOG
        0x0085 => 243, // _SC_TRACE_NAME_MAX
        0x0086 => 244, // _SC_TRACE_SYS_MAX
        0x0087 => 245, // _SC_TRACE_USER_EVENT_MAX
        0x0088 => 165, // _SC_TYPED_MEMORY_OBJECTS
        0x0089 => 237, // _SC_V7_ILP32_OFF32
        0x008a => 238, // _SC_V7_ILP32_OFFBIG
        0x008b => 239, // _SC_V7_LP64_OFF64
        0x008c => 240, // _SC_V7_LPBIG_OFFBIG
        0x008d => 246, // _SC_XOPEN_STREAMS
        0x008f => 185, // _SC_LEVEL1_ICACHE_SIZE
        0x0090 => 186, // _SC_LEVEL1_ICACHE_ASSOC
        0x0091 => 187, // _SC_LEVEL1_ICACHE_LINESIZE
        0x0092 => 188, // _SC_LEVEL1_DCACHE_SIZE
        0x0093 => 189, // _SC_LEVEL1_DCACHE_ASSOC
        0x0094 => 190, // _SC_LEVEL1_DCACHE_LINESIZE
        0x0095 => 191, // _SC_LEVEL2_CACHE_SIZE
        0x0096 => 192, // _SC_LEVEL2_CACHE_ASSOC
        0x0097 => 193, // _SC_LEVEL2_CACHE_LINESIZE
        0x0098 => 194, // _SC_LEVEL3_CACHE_SIZE
        0x0099 => 195, // _SC_LEVEL3_CACHE_ASSOC
        0x009a => 196, // _SC_LEVEL3_CACHE_LINESIZE
        0x009b => 197, // _SC_LEVEL4_CACHE_SIZE
        0x009c => 198, // _SC_LEVEL4_CACHE_ASSOC
        0x009d => 199, // _SC_LEVEL4_CACHE_LINESIZE
        _ => {
            // Honest failure for the two names glibc does not have.
            *libc::__errno_location() = libc::EINVAL;
            return -1;
        }
    };
    unsafe { libc::sysconf(host) }
}

// --- signal ----------------------------------------------------------------

/// bionic `sigwait64(const sigset64_t*, int*)`.
unsafe extern "C" fn sigwait64(set: *const u64, sig: *mut c_int) -> c_int {
    let mut g: libc::sigset_t = std::mem::zeroed();
    if set.is_null() || sig.is_null() {
        return libc::EINVAL;
    }
    // SAFETY: bionic's 8-byte mask fills the low word of glibc's set.
    unsafe {
        let w = &mut g as *mut libc::sigset_t as *mut u64;
        *w = *set;
        libc::sigwait(&g, sig)
    }
}

/// bionic `pthread_sigmask64(int, const sigset64_t*, sigset64_t*)`.
unsafe extern "C" fn pthread_sigmask64(how: c_int, set: *const u64, old: *mut u64) -> c_int {
    let mut gset: libc::sigset_t = std::mem::zeroed();
    let mut gold: libc::sigset_t = std::mem::zeroed();
    let sp = if set.is_null() {
        std::ptr::null()
    } else {
        // SAFETY: caller-provided 8-byte mask.
        unsafe {
            let w = &mut gset as *mut libc::sigset_t as *mut u64;
            *w = *set;
            &gset
        }
    };
    let op = if old.is_null() { std::ptr::null_mut() } else { &mut gold };
    // SAFETY: valid pointers, glibc sigset sized correctly.
    let rc = unsafe { libc::pthread_sigmask(how, sp, op) };
    if rc == 0 && !old.is_null() {
        // SAFETY: low word of the returned mask is bionic's whole mask.
        unsafe {
            let w = &gold as *const libc::sigset_t as *const u64;
            *old = *w;
        }
    }
    rc
}

/// bionic `sigprocmask64(int, const sigset64_t*, sigset64_t*)`.
unsafe extern "C" fn sigprocmask64(how: c_int, set: *const u64, old: *mut u64) -> c_int {
    unsafe { pthread_sigmask64(how, set, old) }
}

extern "C" fn pthread_gettid_np() -> c_int {
    // SAFETY: gettid has no preconditions.
    unsafe { libc::syscall(libc::SYS_gettid) as c_int }
}

/// libsigchain asks the runtime whether it handled a chained signal. ACOS has
/// no ART-specific chain handler, so report "not handled" and let the previous
/// handler run.
extern "C" fn android_handle_signal(_signal: c_int, _info: *mut c_void, _ctx: *mut c_void) {}

// --- properties / fdsan ----------------------------------------------------

extern "C" fn android_get_device_api_level() -> c_int {
    34
}

extern "C" fn fsan_get_owner_tag(_fd: c_int) -> u64 {
    0
}

extern "C" fn fsan_create_owner_tag(tag_type: u64, owner: u64) -> u64 {
    // bionic: (tag_type << 56) | (owner & 0x00ff_ffff_ffff_ffff)
    (tag_type << 56) | (owner & 0x00ff_ffff_ffff_ffff)
}

extern "C" fn fsan_exchange_owner_tag(_fd: c_int, expected: u64, _new: u64) -> u64 {
    expected
}

unsafe extern "C" fn fsan_close_with_tag(fd: c_int, _tag: u64) -> c_int {
    unsafe { libc::close(fd) }
}

unsafe extern "C" fn property_set(_name: *const c_char, _value: *const c_char) -> c_int {
    // The ACOS property store is read-only for guests; report success so
    // libbase's best-effort writes do not take an error path.
    0
}

unsafe extern "C" fn property_wait(
    _pi: *const c_void,
    old_serial: u32,
    new_serial: *mut u32,
    _timeout_ns: i64,
) -> bool {
    if !new_serial.is_null() {
        unsafe { *new_serial = old_serial };
    }
    false
}

unsafe extern "C" fn property_foreach(_cb: *const c_void, _cookie: *mut c_void) -> c_int {
    0
}

extern "C" fn property_area_serial() -> u32 {
    1
}

// --- libcutils / liblog / bionic data surface ------------------------------

unsafe extern "C" fn log_bswrite(_tag: *const c_char, _buf: *const c_char) -> c_int {
    0
}

extern "C" fn system_properties_init() -> c_int {
    0
}

extern "C" fn atrace_get_enabled_tags() -> u64 {
    0
}

unsafe extern "C" fn atrace_noop1(_name: *const c_char) {}

extern "C" fn atrace_end_body() {}

unsafe extern "C" fn atrace_int_body(_name: *const c_char, _value: i32) {}

/// bionic `ashmem_create_region(name, size)`: a memfd is the modern ashmem.
unsafe extern "C" fn ashmem_create_region(name: *const c_char, size: usize) -> c_int {
    let fd = unsafe { libc::memfd_create(name, 0) };
    if fd < 0 {
        return -1;
    }
    if unsafe { libc::ftruncate(fd, size as i64) } != 0 {
        unsafe { libc::close(fd) };
        return -1;
    }
    fd
}

extern "C" fn ashmem_set_prot_region(_fd: c_int, _prot: c_int) -> c_int {
    0
}

/// bionic `_ctype_`: 257 bytes, indexed by `c + 1`, with EOF at index 0.
static CTYPE_TABLE: [u8; 257] = build_ctype();

const fn build_ctype() -> [u8; 257] {
    // bionic <ctype.h> flags
    const U: u8 = 0x01;
    const L: u8 = 0x02;
    const D: u8 = 0x04;
    const S: u8 = 0x08;
    const P: u8 = 0x10;
    const C: u8 = 0x20;
    const X: u8 = 0x40;
    const B: u8 = 0x80;
    let mut t = [0u8; 257];
    let mut c = 0usize;
    while c < 256 {
        let ch = c as u8;
        let mut f = 0u8;
        if ch >= b'A' && ch <= b'Z' {
            f |= U;
        }
        if ch >= b'a' && ch <= b'z' {
            f |= L;
        }
        if ch >= b'0' && ch <= b'9' {
            f |= D;
        }
        if ch == b' ' || ch == 0x09 || ch == 0x0a || ch == 0x0b || ch == 0x0c || ch == 0x0d {
            f |= S;
            if ch == b' ' || ch == 0x09 {
                f |= B;
            }
        }
        if ch >= 0x21 && ch <= 0x7e && !(f & (U | L | D) != 0) {
            f |= P;
        }
        if ch < 0x20 || ch == 0x7f {
            f |= C;
        }
        if (ch >= b'0' && ch <= b'9') || (ch >= b'a' && ch <= b'f') || (ch >= b'A' && ch <= b'F') {
            f |= X;
        }
        t[c + 1] = f;
        c += 1;
    }
    t
}

// --- liblog -----------------------------------------------------------------

#[repr(C)]
struct AndroidLogMessage {
    size: u32,
    buffer_id: i32,
    priority: i32,
    tag: *const c_char,
    file: *const c_char,
    line: u32,
    message: *const c_char,
}

unsafe fn cstr(p: *const c_char) -> &'static str {
    if p.is_null() {
        ""
    } else {
        // SAFETY: NUL-terminated string owned by the caller for this call.
        unsafe { std::ffi::CStr::from_ptr(p) }.to_str().unwrap_or("<non-utf8>")
    }
}

unsafe extern "C" fn log_error_write(
    _buf_id: c_int,
    _tag: *const c_char,
    _msg: *const c_char,
    _error: c_int,
) -> c_int {
    0
}

unsafe extern "C" fn create_android_logger(_name: *const c_void) -> c_int {
    // A negative id is "no logger"; ART's writers ignore the failure.
    0
}

unsafe extern "C" fn android_log_destroy(_id: *mut c_int) {}

unsafe extern "C" fn android_log_write_int32(_id: c_int, _value: i32) {}

unsafe extern "C" fn android_log_write_string8(_id: c_int, _value: *const c_char) {}

unsafe extern "C" fn android_log_write_string8_len(
    _id: c_int,
    _value: *const c_char,
    _len: usize,
) {
}

unsafe extern "C" fn android_log_noop3(_a: *mut c_void, _b: *mut c_void, _c: *mut c_void) {}

unsafe extern "C" fn log_set_logger(_logger: *const c_void) {}

unsafe extern "C" fn log_logd_logger(_msg: *const AndroidLogMessage) {}

unsafe extern "C" fn log_set_aborter(_aborter: *const c_void) {}

unsafe extern "C" fn log_call_aborter(_msg: *const c_char) {
    // No aborter installed by ACOS: keep libbase's contract (never returns).
    std::process::abort()
}

unsafe extern "C" fn log_get_min_priority() -> c_int {
    0
}

unsafe extern "C" fn log_set_min_priority(_prio: c_int) -> c_int {
    0
}

unsafe extern "C" fn log_set_default_tag(_tag: *const c_char) {}

unsafe extern "C" fn log_write_message(msg: *const AndroidLogMessage) {
    if msg.is_null() {
        return;
    }
    // SAFETY: pointer validated above; libbase owns the struct for the call.
    let m = unsafe { &*msg };
    eprintln!("[art/log] {}: {}", unsafe { cstr(m.tag) }, unsafe {
        cstr(m.message)
    });
}

extern "C" fn getprogname() -> *const c_char {
    c"art".as_ptr()
}

extern "C" fn hwasan_tag_pointer(ptr: *const c_void, _tag: u64) -> *const c_void {
    ptr
}

// --- assert / fortify ------------------------------------------------------

/// bionic `__assert(file, line, function, failed_expression)`.
unsafe extern "C" fn bionic_assert(
    file: *const c_char,
    line: c_int,
    function: *const c_char,
    expr: *const c_char,
) -> ! {
    eprintln!(
        "ASSERT: {}:{}: {}: \"{}\"",
        unsafe { cstr(file) },
        line,
        unsafe { cstr(function) },
        unsafe { cstr(expr) }
    );
    std::process::abort()
}

unsafe extern "C" fn strlcpy_chk(
    dst: *mut c_char,
    src: *const c_char,
    siz: usize,
    dstlen: usize,
) -> usize {
    if siz > dstlen || siz == 0 {
        eprintln!("__strlcpy_chk: size {siz} exceeds destination {dstlen}");
        std::process::abort()
    }
    // SAFETY: caller guarantees `dst` is `siz` bytes and `src` is a C string.
    unsafe {
        let mut n = 0usize;
        while n + 1 < siz && *src.add(n) != 0 {
            *dst.add(n) = *src.add(n);
            n += 1;
        }
        *dst.add(n) = 0;
        let mut len = n;
        while *src.add(len) != 0 {
            len += 1;
        }
        len
    }
}

unsafe extern "C" fn strncpy_chk2(
    dst: *mut c_char,
    src: *const c_char,
    len: usize,
    dstlen: usize,
    srclen: usize,
) -> *mut c_char {
    if len > dstlen || len > srclen {
        eprintln!("__strncpy_chk2: len {len} dst {dstlen} src {srclen}");
        std::process::abort()
    }
    unsafe { libc::strncpy(dst, src, len) }
}

unsafe extern "C" fn pwrite_chk(
    fd: c_int,
    buf: *const c_void,
    count: usize,
    offset: i64,
    buflen: usize,
) -> isize {
    if count > buflen {
        eprintln!("__pwrite_chk: count {count} > buffer {buflen}");
        std::process::abort()
    }
    unsafe { libc::pwrite(fd, buf, count, offset) }
}

unsafe extern "C" fn pread_chk(
    fd: c_int,
    buf: *mut c_void,
    count: usize,
    offset: i64,
    buflen: usize,
) -> isize {
    if count > buflen {
        eprintln!("__pread_chk: count {count} > buffer {buflen}");
        std::process::abort()
    }
    unsafe { libc::pread(fd, buf, count, offset) }
}

// --- Android absolute-path remap -------------------------------------------
//
// B-LOADER PATCH: ART and libnativeloader open hard-coded Android paths
// (`/system/etc/public.libraries.txt`, `/apex/com.android.art/...`,
// `/system/lib64/...`). On the host those roots do not exist and cannot be
// created without root. The ACOS product will mount/expose a sysroot; for this
// spike a path-rewriting shim maps the Android roots onto the extracted
// sysroot. Only these three roots are rewritten; everything else passes
// through unchanged (so `/proc`, `/dev/null`, the dex files and the host
// library search keep working).

use std::path::PathBuf;

static SYSROOT: OnceLock<PathBuf> = OnceLock::new();
static OVERRIDES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Replace one Android absolute path with a host path (spike-local config such
/// as an emptied `public.libraries.txt`).
pub fn set_path_override(from: impl Into<String>, to: impl Into<String>) {
    OVERRIDES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(from.into(), to.into());
}

/// Install the sysroot that `/system`, `/apex`, `/data` and `/product` map onto.
pub fn set_sysroot(root: impl Into<PathBuf>) {
    let _ = SYSROOT.set(root.into());
}

fn android_roots() -> &'static [&'static str] {
    &["/system", "/apex", "/product", "/system_ext", "/odm", "/vendor"]
}

struct MappedPath {
    c: Option<std::ffi::CString>,
    raw: *const c_char,
}

impl MappedPath {
    fn new(path: *const c_char) -> Self {
        if path.is_null() {
            return MappedPath { c: None, raw: path };
        }
        // SAFETY: caller-provided NUL-terminated string.
        let s = unsafe { std::ffi::CStr::from_ptr(path) }.to_string_lossy().into_owned();
        if let Some(map) = OVERRIDES.get() {
            if let Some(to) = map.lock().unwrap().get(&s) {
                if let Ok(c) = std::ffi::CString::new(to.clone()) {
                    let raw = c.as_ptr();
                    return MappedPath { c: Some(c), raw };
                }
            }
        }
        let Some(root) = SYSROOT.get() else {
            return MappedPath { c: None, raw: path };
        };
        let mapped = android_roots().iter().find_map(|r| {
            s.strip_prefix(r)
                .filter(|rest| rest.is_empty() || rest.starts_with('/'))
                .map(|rest| root.join(&s[1..]))
        });
        match mapped {
            Some(p) => {
                let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).ok();
                match c {
                    Some(c) => {
                        let raw = c.as_ptr();
                        MappedPath { c: Some(c), raw }
                    }
                    None => MappedPath { c: None, raw: path },
                }
            }
            None => MappedPath { c: None, raw: path },
        }
    }
}

unsafe extern "C" fn open_mapped(path: *const c_char, flags: c_int, mode: c_int) -> c_int {
    let m = MappedPath::new(path);
    unsafe { libc::open(m.raw as *const c_char, flags, mode) }
}

unsafe extern "C" fn openat_mapped(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mode: c_int,
) -> c_int {
    let m = MappedPath::new(path);
    unsafe { libc::openat(dirfd, m.raw as *const c_char, flags, mode) }
}

unsafe extern "C" fn fopen_mapped(path: *const c_char, mode: *const c_char) -> *mut c_void {
    let m = MappedPath::new(path);
    unsafe { libc::fopen(m.raw as *const c_char, mode) as *mut c_void }
}

unsafe extern "C" fn access_mapped(path: *const c_char, mode: c_int) -> c_int {
    let m = MappedPath::new(path);
    unsafe { libc::access(m.raw as *const c_char, mode) }
}

unsafe extern "C" fn stat_mapped(path: *const c_char, buf: *mut c_void) -> c_int {
    let m = MappedPath::new(path);
    unsafe { super::shims::stat(m.raw as *const c_char, buf) }
}

unsafe extern "C" fn realpath_mapped(
    path: *const c_char,
    resolved: *mut c_char,
) -> *mut c_char {
    let m = MappedPath::new(path);
    unsafe { libc::realpath(m.raw as *const c_char, resolved) }
}

unsafe extern "C" fn opendir_mapped(path: *const c_char) -> *mut c_void {
    let m = MappedPath::new(path);
    unsafe { libc::opendir(m.raw as *const c_char) as *mut c_void }
}

/// Table entries for the remap layer (installed last, so they override the
/// plain forwards).
pub fn path_table() -> HashMap<&'static str, Shim> {
    let mut m = HashMap::new();
    shim!(m, "open", open_mapped);
    shim!(m, "openat", openat_mapped);
    shim!(m, "fopen", fopen_mapped);
    shim!(m, "fopen64", fopen_mapped);
    shim!(m, "access", access_mapped);
    shim!(m, "stat", stat_mapped);
    shim!(m, "realpath", realpath_mapped);
    shim!(m, "opendir", opendir_mapped);
    shim!(m, "__open_2", open_mapped);
    m
}

// --- linker-namespace API (`__loader_*`) ------------------------------------
//
// B-LOADER PATCH: on Android 14 x86_64 `ld-android.so` is a stub whose whole
// `.text` is two bytes (`ud2`); every `__loader_*` symbol points at it. The real
// implementations live in the dynamic linker binary (`/system/bin/linker64`),
// which also reads `ld.config.txt`. `libnativebridge.so` (the NDK namespace
// API surface used by ART's `libnativeloader`) imports them directly, so
// calling any namespace function jumps into the `ud2` trap and ART dies with
// SIGILL right after `InitDefaultPublicLibraries`.
//
// ACOS has no linker, so the spike provides a minimal but honest namespace
// model: namespaces are opaque handles; linking always succeeds; every library
// resolves through the loader's single global scope (which is exactly what the
// flat search-path model already does).

static NEXT_NAMESPACE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

unsafe extern "C" fn loader_create_namespace(
    _name: *const c_char,
    _ld_library_path: *const c_char,
    _default_library_path: *const c_char,
    _type_: u64,
    _permitted_writable_paths: *const c_char,
    _params: *mut c_void,
) -> *mut c_void {
    // Non-null handle; never dereferenced by our shims.
    let id = NEXT_NAMESPACE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    (0x4e53_0000usize | id) as *mut c_void
}

unsafe extern "C" fn loader_link_namespaces(
    _from: *mut c_void,
    _to: *mut c_void,
    _shared_libs: *const c_char,
) -> bool {
    true
}

unsafe extern "C" fn loader_get_exported_namespace(_name: *const c_char) -> *mut c_void {
    // A synthetic handle here is NOT a real linker namespace: libnativeloader
    // dereferences it when it wants the namespace's exported sonames, so a fake
    // non-null pointer crashes. Report "no such namespace"; nativeloader then
    // creates one through `__loader_android_create_namespace`, whose handle we
    // never dereference.
    std::ptr::null_mut()
}

unsafe extern "C" fn loader_init_anonymous_namespace(
    _shared_libs: *const c_char,
    _params: *mut c_void,
) -> bool {
    true
}

unsafe extern "C" fn loader_get_ld_library_path(buf: *mut c_char, buf_len: usize) -> usize {
    let path = b"/system/lib64\0";
    if buf.is_null() || buf_len == 0 {
        return 0;
    }
    let n = path.len().min(buf_len) - 1;
    unsafe {
        std::ptr::copy_nonoverlapping(path.as_ptr() as *const c_char, buf, n);
        *buf.add(n) = 0;
    }
    n
}

unsafe extern "C" fn loader_set_target_sdk(_v: i32) {}
unsafe extern "C" fn loader_get_target_sdk() -> i32 {
    34
}
unsafe extern "C" fn loader_dlwarning(_a: *const c_char, _b: *const c_char) {}
unsafe extern "C" fn loader_handle_signal() -> bool {
    false
}
unsafe extern "C" fn loader_remove_tls_dtor(_a: *mut c_void) {}
unsafe extern "C" fn loader_cfi_fail() -> ! {
    eprintln!("__loader_cfi_fail");
    std::process::abort()
}

/// bionic's `__loader_shared_globals` is a small POD block owned by the linker.
static LOADER_SHARED_GLOBALS: [usize; 8] = [0; 8];

pub fn namespace_table() -> HashMap<&'static str, Shim> {
    let mut m = HashMap::new();
    shim!(m, "__loader_android_create_namespace", loader_create_namespace);
    shim!(m, "__loader_android_link_namespaces", loader_link_namespaces);
    shim!(
        m,
        "__loader_android_get_exported_namespace",
        loader_get_exported_namespace
    );
    shim!(
        m,
        "__loader_android_init_anonymous_namespace",
        loader_init_anonymous_namespace
    );
    shim!(
        m,
        "__loader_android_get_LD_LIBRARY_PATH",
        loader_get_ld_library_path
    );
    shim!(
        m,
        "__loader_android_set_application_target_sdk_version",
        loader_set_target_sdk
    );
    shim!(
        m,
        "__loader_android_get_application_target_sdk_version",
        loader_get_target_sdk
    );
    shim!(m, "__loader_android_dlwarning", loader_dlwarning);
    shim!(m, "__loader_android_handle_signal", loader_handle_signal);
    shim!(m, "__loader_remove_thread_local_dtor", loader_remove_tls_dtor);
    shim!(m, "__loader_cfi_fail", loader_cfi_fail);
    shim!(m, "__loader_dlsym", super::dl::dlsym);
    shim!(m, "__loader_dlvsym", super::dl::dlsym);
    shim!(m, "__loader_dladdr", super::dl::dladdr);
    shim!(m, "__loader_dlclose", super::dl::dlclose);
    shim!(m, "__loader_dl_iterate_phdr", super::dl::dl_iterate_phdr);
    shim!(m, "__loader_android_dlopen_ext", super::dl::android_dlopen_ext);
    m.insert(
        "__loader_shared_globals",
        Shim {
            name: "__loader_shared_globals",
            ptr: LOADER_SHARED_GLOBALS.as_ptr() as *const c_void,
            category: Category::Shimmed,
        },
    );
    m
}
