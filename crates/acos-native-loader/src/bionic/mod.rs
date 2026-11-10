//! The bionic compatibility table.
//!
//! Every guest import is classified:
//!
//! * [`Category::Forwarded`] — the bionic and glibc ABIs are identical, so the
//!   symbol resolves directly to the host libc implementation.
//! * [`Category::Shimmed`] — a translating shim in this crate implements the
//!   symbol because the ABI differs (pthread handle indirection, `struct
//!   sigaction`, stdio `__sF`, `errno`, android logging, properties, ...).
//!
//! Anything absent from the table is *never* silently stubbed: binding either
//! fails with `UNSUPPORTED_NATIVE_SYMBOL`, or (opt-in `LazyTrap`) binds to a
//! per-symbol aborting trap.

use std::collections::HashMap;
use std::os::raw::c_void;
use std::sync::OnceLock;

pub mod dl;
pub mod gles;
pub mod ndk;
pub mod pthread;
pub mod shims;
pub mod trap;

/// How a guest symbol is provided.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    /// Directly forwarded to the host libc.
    Forwarded,
    /// Implemented by a translating shim in this crate.
    Shimmed,
}

/// One table entry.
#[derive(Clone, Copy)]
pub struct Shim {
    pub name: &'static str,
    pub ptr: *const c_void,
    pub category: Category,
}

// SAFETY: the table is built once and never mutated; the pointers are stable
// host function/data addresses.
unsafe impl Send for Shim {}
unsafe impl Sync for Shim {}

impl Shim {
    pub const fn forwarded(name: &'static str, ptr: *const c_void) -> Self {
        Shim {
            name,
            ptr,
            category: Category::Forwarded,
        }
    }
    pub const fn shimmed(name: &'static str, ptr: *const c_void) -> Self {
        Shim {
            name,
            ptr,
            category: Category::Shimmed,
        }
    }
}

/// Whether the category is forwarded (used by the report).
pub fn is_forwarded(s: &Shim) -> bool {
    s.category == Category::Forwarded
}

fn build_table() -> HashMap<&'static str, Shim> {
    let mut m: HashMap<&'static str, Shim> = HashMap::new();

    // Forth-fortify entry points and C++ runtime entries that glibc exports.
    extern "C" {
        fn __memcpy_chk();
        fn __memmove_chk();
        fn __memset_chk();
        fn __strcpy_chk();
        fn __strncpy_chk();
        fn __strcat_chk();
        fn __strncat_chk();
        fn __sprintf_chk();
        fn __snprintf_chk();
        fn __vsnprintf_chk();
        fn __printf_chk();
        fn __fprintf_chk();
        fn __vprintf_chk();
        fn __vfprintf_chk();
        fn __stack_chk_fail();
        fn __cxa_atexit();
        fn __cxa_finalize();
        fn __cxa_thread_atexit_impl();
        fn __register_atfork();
        fn feclearexcept();
        fn fetestexcept();
    }

    macro_rules! fwd_extern {
        ($($name:ident),+ $(,)?) => {
            $( m.insert(stringify!($name), Shim::forwarded(stringify!($name), $name as *const () as *const c_void)); )+
        };
    }
    fwd_extern!(
        __memcpy_chk, __memmove_chk, __memset_chk, __strcpy_chk, __strncpy_chk,
        __strcat_chk, __strncat_chk, __sprintf_chk, __snprintf_chk, __vsnprintf_chk,
        __printf_chk, __fprintf_chk, __vprintf_chk, __vfprintf_chk, __stack_chk_fail,
        __cxa_atexit, __cxa_finalize, __cxa_thread_atexit_impl, __register_atfork,
        feclearexcept, fetestexcept,
    );

    // glibc math/stdlib/stdio entries that the `libc` crate does not expose.
    extern "C" {
        fn strtold();
        fn llabs();
        fn random();
        fn srandom();
        fn div();
        fn ldiv();
        fn lldiv();
        fn clock();
        fn getpagesize();
        fn psignal();
        fn vprintf();
        fn vsnprintf();
        fn vsprintf();
        fn vsscanf();
        fn pow();
        fn powf();
        fn sqrt();
        fn sqrtf();
        fn sin();
        fn sinf();
        fn cos();
        fn cosf();
        fn tan();
        fn tanf();
        fn asin();
        fn acos();
        fn atan();
        fn atan2();
        fn atan2f();
        fn exp();
        fn expf();
        fn exp2();
        fn expm1();
        fn log();
        fn logf();
        fn log2();
        fn log2f();
        fn log10();
        fn log10f();
        fn log1p();
        fn fmod();
        fn fmodf();
        fn floor();
        fn floorf();
        fn ceil();
        fn ceilf();
        fn round();
        fn roundf();
        fn trunc();
        fn truncf();
        fn fabs();
        fn fabsf();
        fn fdim();
        fn fmin();
        fn fmax();
        fn copysign();
        fn copysignf();
        fn hypot();
        fn hypotf();
        fn cbrt();
        fn cbrtf();
        fn ldexp();
        fn frexp();
        fn modf();
        fn nearbyint();
        fn rint();
        fn remainder();
        fn nextafter();
        fn nextafterf();
        fn nan();
        fn nanf();
        // wide-character + misc entries used by libc++.
        fn wcslen();
        fn wcstod();
        fn wcstof();
        fn wcstol();
        fn wcstold();
        fn wcstoll();
        fn wcstoul();
        fn wcstoull();
        fn wmemchr();
        fn wmemcmp();
        fn swprintf();
        fn vasprintf();
        fn posix_memalign();
    }
    fwd_extern!(
        strtold, llabs, random, srandom, div, ldiv, lldiv, clock, getpagesize, psignal,
        vprintf, vsnprintf, vsprintf, vsscanf,
        pow, powf, sqrt, sqrtf, sin, sinf, cos, cosf, tan, tanf, asin, acos, atan,
        atan2, atan2f, exp, expf, exp2, expm1, log, logf, log2, log2f, log10, log10f,
        log1p, fmod, fmodf, floor, floorf, ceil, ceilf, round, roundf, trunc, truncf,
        fabs, fabsf, fdim, fmin, fmax, copysign, copysignf, hypot, hypotf, cbrt,
        cbrtf, ldexp, frexp, modf, nearbyint, rint, remainder, nextafter, nextafterf,
        nan, nanf,
        wcslen, wcstod, wcstof, wcstol, wcstold, wcstoll, wcstoul, wcstoull,
        wmemchr, wmemcmp, swprintf, vasprintf, posix_memalign,
    );

    // --- libc: ABI-identical symbols forwarded to glibc ---------------------
    macro_rules! fwd {
        ($($name:ident),+ $(,)?) => {
            $( m.insert(stringify!($name), Shim::forwarded(stringify!($name), libc::$name as *const () as *const c_void)); )+
        };
    }

    fwd!(
        // memory
        malloc, calloc, realloc, free, memcpy, memmove, memset, memcmp, memchr, memrchr,
        // strings
        strlen, strnlen, strcmp, strncmp, strcasecmp, strncasecmp, strcpy, strncpy, strcat,
        strncat, strchr, strrchr, strstr, strcasestr, strdup, strndup, strtok, strtok_r,
        strerror, strerror_r, strspn, strcspn, strpbrk, strcoll, strxfrm,
        // stdlib (subset exported by the `libc` crate)
        atoi, atol, atoll, strtol, strtoul, strtoll, strtoull, strtof, strtod,
        abs, labs, qsort, bsearch, getenv, setenv, unsetenv, putenv, rand, srand,
        drand48, lrand48, mrand48, atexit,
        // time
        clock_gettime, clock_getres, clock_settime, gettimeofday, settimeofday, time,
        localtime, localtime_r, gmtime, gmtime_r, mktime, timegm, strftime, difftime,
        // process / sys
        getpid, getppid, gettid, getuid, geteuid, getgid, getegid, setsid, sysconf,
        sched_yield, sched_getcpu, sleep, usleep, nanosleep, pause, abort, raise, kill,
        syscall,
        // memory mapping
        mmap, munmap, mprotect, madvise, msync, mremap, mlock, munlock,
        // file descriptors (common open flags are identical on x86_64; see docs)
        close, read, write, pread, pwrite, lseek, ftruncate, fsync, fdatasync, unlink,
        rmdir, mkdir, chdir, getcwd, access, faccessat, dup, dup2, dup3, pipe, pipe2,
        ioctl, fcntl, isatty, umask, chmod, fchmod, chown, fchown, rename, link, symlink,
        readlink, truncate, sync, open, openat, creat, posix_fadvise, posix_fallocate,
        // scheduler / limits
        sched_getaffinity, sched_setaffinity, getrlimit, setrlimit,
        // network (socket-level ABI matches; getaddrinfo is absent, see docs)
        socket, socketpair, bind, listen, accept, accept4, connect, shutdown, getsockname,
        getpeername, setsockopt, getsockopt, send, sendto, recv, recvfrom, sendmsg,
        recvmsg,
        // misc
        getauxval, __errno_location, strsignal,
        // stdio that does not take a FILE* stream
        printf, puts, putchar, snprintf, sprintf, sscanf, perror,
    );


    // Additional glibc libc/libm entries needed by real Android libraries.
    extern "C" {
        // process / libc surface required by the RN/Hermes/ONNX corpus
        fn exit();
        fn _exit();
        fn prctl();
        fn uname();
        fn pthread_sigmask();
        fn pthread_kill();
        fn pthread_getschedparam();
        fn pthread_setschedparam();
        fn pthread_attr_getstack();
        fn pthread_attr_getschedparam();
        fn pthread_attr_setschedparam();
        fn pthread_attr_setschedpolicy();
        fn pthread_attr_setscope();
        fn sched_get_priority_max();
        fn sigaltstack();
        fn popen();
        fn pclose();
        fn fileno();
        fn dirname();
        fn asprintf();
        fn mkstemp();
        fn gethostname();
        fn getrusage();
        fn utimes();
        fn nice();
        fn select();
        fn flock();
        fn ferror();
        fn ftell();
        fn rewind();
        fn tmpfile();
        fn fscanf();
        fn nftw();
        fn lseek64();
        fn __open_2();
        fn __xpg_strerror_r();
        // libm hyperbolics used by Hermes/ONNX
        fn acosh();
        fn asinh();
        fn atanh();
        fn cosh();
        fn sinh();
        fn tanh();
        fn sincos();
        fn lround();
        fn llroundl();
        fn __ctype_get_mb_cur_max();
        fn __vsprintf_chk();
        fn acosf();
        fn acoshf();
        fn arc4random();
        fn asinf();
        fn asinhf();
        fn atanf();
        fn atanhf();
        fn btowc();
        fn closedir();
        fn coshf();
        fn erfcf();
        fn erff();
        fn exp2f();
        fn expm1f();
        fn fchmodat();
        fn fdimf();
        fn fdopendir();
        fn fgets();
        fn fmaf();
        fn fputwc();
        fn freelocale();
        fn frexpf();
        fn fseek();
        fn fseeko();
        fn ftello();
        fn getc();
        fn getwc();
        fn ilogbf();
        fn islower();
        fn isnanf();
        fn isupper();
        fn iswalpha_l();
        fn iswblank_l();
        fn iswcntrl_l();
        fn iswdigit_l();
        fn iswlower();
        fn iswlower_l();
        fn iswprint_l();
        fn iswpunct_l();
        fn iswspace_l();
        fn iswupper_l();
        fn iswxdigit_l();
        fn isxdigit();
        fn ldexpf();
        fn lgammaf();
        fn lgammaf_r();
        fn localeconv();
        fn log1pf();
        fn logbf();
        fn mbrlen();
        fn mbrtowc();
        fn mbsnrtowcs();
        fn mbsrtowcs();
        fn mbtowc();
        fn memalign();
        fn modff();
        fn newlocale();
        fn opendir();
        fn pathconf();
        fn poll();
        fn readdir();
        fn realpath();
        fn remainderf();
        fn remove();
        fn remquof();
        fn sendfile();
        fn setlocale();
        fn setpriority();
        fn sincosf();
        fn sinhf();
        fn statvfs();
        fn strcoll_l();
        fn strftime_l();
        fn strlcpy();
        fn strtold_l();
        fn strtoll_l();
        fn strtoull_l();
        fn strxfrm_l();
        fn tanhf();
        fn tgamma();
        fn towlower_l();
        fn towupper_l();
        fn tzset();
        fn ungetc();
        fn ungetwc();
        fn unlinkat();
        fn uselocale();
        fn utimensat();
        fn wcrtomb();
        fn wcscoll_l();
        fn wcsnrtombs();
        fn wcsxfrm_l();
        fn wctob();
        fn wmemcpy();
        fn wmemmove();
        fn wmemset();
    }
    fwd_extern!(
        exit, _exit, prctl, uname, pthread_sigmask, pthread_kill, pthread_getschedparam,
        pthread_setschedparam, pthread_attr_getstack, pthread_attr_getschedparam,
        pthread_attr_setschedparam, pthread_attr_setschedpolicy, pthread_attr_setscope,
        sched_get_priority_max, sigaltstack, popen, pclose, fileno, dirname, asprintf,
        mkstemp, gethostname, getrusage, utimes, nice, select, flock, ferror, ftell,
        rewind, tmpfile, fscanf, nftw, lseek64, __open_2,
        __xpg_strerror_r,
        acosh, asinh, atanh, cosh, sinh, tanh, sincos, lround, llroundl,
        __ctype_get_mb_cur_max, __vsprintf_chk, acosf, acoshf, arc4random, asinf, asinhf, atanf, atanhf, btowc, closedir, coshf, erfcf, erff, exp2f, expm1f, fchmodat, fdimf, fdopendir, fgets, fmaf, fputwc, freelocale, frexpf, fseek, fseeko, ftello, getc, getwc, ilogbf, islower, isnanf, isupper, iswalpha_l, iswblank_l, iswcntrl_l, iswdigit_l, iswlower, iswlower_l, iswprint_l, iswpunct_l, iswspace_l, iswupper_l, iswxdigit_l, isxdigit, ldexpf, lgammaf, lgammaf_r, localeconv, log1pf, logbf, mbrlen, mbrtowc, mbsnrtowcs, mbsrtowcs, mbtowc, memalign, modff, newlocale, opendir, pathconf, poll, readdir, realpath, remainderf, remove, remquof, sendfile, setlocale, setpriority, sincosf, sinhf, statvfs, strcoll_l, strftime_l, strlcpy, strtold_l, strtoll_l, strtoull_l, strxfrm_l, tanhf, tgamma, towlower_l, towupper_l, tzset, ungetc, ungetwc, unlinkat, uselocale, utimensat, wcrtomb, wcscoll_l, wcsnrtombs, wcsxfrm_l, wctob, wmemcpy, wmemmove, wmemset,
    );

    // --- shimmed symbols ---------------------------------------------------
    macro_rules! shim {
        ($name:literal, $ptr:expr) => {
            m.insert($name, Shim::shimmed($name, $ptr as *const c_void));
        };
    }

    shim!("__errno", shims::__errno as *const c_void);
    shim!("__sF", shims::__sF_addr() as *const c_void);
    shim!("__android_log_print", shims::c_android_log_print_addr());
    shim!("__android_log_vprint", shims::c_android_log_vprint_addr());
    shim!("__android_log_write", shims::c_android_log_write_addr());
    shim!("__android_log_assert", shims::c_android_log_assert_addr());
    shim!("__android_log_buf_print", shims::c_android_log_print_addr());
    shim!("__system_property_get", shims::__system_property_get as *const c_void);
    shim!("__system_property_find", shims::__system_property_find as *const c_void);
    shim!("__system_property_read_callback", shims::__system_property_read_callback as *const c_void);
    shim!("__system_property_serial", shims::__system_property_serial as *const c_void);
    shim!("android_set_abort_message", shims::android_set_abort_message as *const c_void);
    shim!("__android_get_application_target_sdk_version", shims::__android_get_application_target_sdk_version as *const c_void);
    shim!("__android_log_is_loggable", shims::__android_log_is_loggable as *const c_void);
    shim!("__android_log_is_loggable_len", shims::__android_log_is_loggable_len as *const c_void);
    shim!("android_get_application_target_sdk_version", shims::__android_get_application_target_sdk_version as *const c_void);
    shim!("__tls_get_addr", crate::tls::__tls_get_addr as *const c_void);
    shim!("__tls_get_addr", crate::tls::__tls_get_addr as *const c_void);

    // bionic spells the GNU strerror_r entry `__gnu_strerror_r`; glibc's
    // exported `__xpg_strerror_r` has the same (int, char*, size_t) -> int ABI.
    if let Some(xpg) = m.get("__xpg_strerror_r").copied() {
        m.insert("__gnu_strerror_r", Shim::forwarded("__gnu_strerror_r", xpg.ptr));
    }

    // NDK platform surface (libandroid / libjnigraphics / liblog / EGL-GLES).
    // Every entry is backed by the installed `NdkHost` or reports a structured
    // `UNSUPPORTED_NATIVE_SYMBOL`; see `ndk.rs`.
    for (name, ptr) in ndk::ndk_asset::table()
        .into_iter()
        .chain(ndk::ndk_bitmap::table())
        .chain(ndk::ndk_window::table())
        .chain(ndk::ndk_looper::table())
        .chain(ndk::ndk_misc::table())
        .chain(ndk::ndk_gl::full_table())
    {
        m.insert(name, Shim::shimmed(name, ptr));
    }

    m.extend(shims::log_extra_table());
    m.extend(shims::fortify_table());
    m.extend(shims::android_extra_table());
    m.extend(shims::stat_table());
    m.extend(shims::signal_table());
    m.extend(shims::stdio_table());
    pthread::install(&mut m);
    dl::install(&mut m);

    m
}

/// The process-wide bionic symbol table.
pub fn table() -> &'static HashMap<&'static str, Shim> {
    static TABLE: OnceLock<HashMap<&'static str, Shim>> = OnceLock::new();
    TABLE.get_or_init(|| {
        shims::init_stream_cells();
        build_table()
    })
}

/// Look up a bionic-provided symbol.
pub fn resolve(name: &str) -> Option<Shim> {
    // A installed GL substrate (the `--native` path installs one) wins over the
    // static EGL/GLES traps: the guest then reaches the real software renderer
    // instead of a reported `GL_INVALID_OPERATION`. Without an install the traps
    // in the table below remain, so the loader still fails closed.
    if let Some(shim) = gles::shim(name) {
        return Some(shim);
    }
    table().get(name).copied()
}

/// `(forwarded, shimmed)` entry counts, for reporting.
pub fn counts() -> (usize, usize) {
    let mut forwarded = 0;
    let mut shimmed = 0;
    for s in table().values() {
        match s.category {
            Category::Forwarded => forwarded += 1,
            Category::Shimmed => shimmed += 1,
        }
    }
    (forwarded, shimmed)
}
