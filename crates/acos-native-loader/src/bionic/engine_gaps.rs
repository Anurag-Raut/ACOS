//! Bionic shims for the imports `libflutter.so` needs that no earlier
//! workstream provided.
//!
//! Every symbol here was a *missing* entry in
//! `results/flutter/native-inventory.json` (48 libc/other + 3 OpenSSL; the
//! EGL/GLES imports are served by `acos-gles`, see
//! [`crate::bionic::gles`]).  The Glibc ABI for these names on x86_64 is
//! identical to bionic's, so most are *forwarded*; the few that differ
//! (fortified `openat`, `FILE*` streams, the bionic 8-byte `sigset_t`) get a
//! translating shim, and the two dangerous control-flow entries
//! (`setjmp`/`longjmp`) remain fail-closed structured traps: binding succeeds
//! so the engine can be loaded, but *calling* them reports the exact symbol
//! and aborts instead of handing a bionic-sized `jmp_buf` to glibc (which
//! would smash the guest stack).
//!
//! Nothing here silently fakes success. `fork`/`execvp` are fail-closed
//! *returning* shims (the Android contract for a restricted process is an
//! error), and the OpenSSL hooks are the trivial `malloc`/`free`/
//! `malloc_usable_size` allocator they name.

use std::os::raw::{c_char, c_int, c_void};

use super::shims::acos_map_stream;

// --- forwarded entries (bionic == glibc ABI on x86_64) ---------------------
//
// Declared without signatures: only their addresses are placed in the table.
// The loader calls them with the guest's ABI, exactly like `bionic/mod.rs`
// does for its other forwarded libc entries.
extern "C" {
    fn __cmsg_nxthdr();
    fn __poll_chk();
    fn __pread_chk();
    fn __read_chk();
    fn __readlink_chk();
    fn close_range();
    fn epoll_create1();
    fn epoll_ctl();
    fn epoll_wait();
    fn fma();
    fn freeaddrinfo();
    fn freeifaddrs();
    fn ftruncate64();
    fn gai_strerror();
    fn getaddrinfo();
    fn getifaddrs();
    fn getnameinfo();
    fn if_nametoindex();
    fn inet_aton();
    fn inet_ntop();
    fn inet_pton();
    fn inotify_add_watch();
    fn inotify_init1();
    fn inotify_rm_watch();
    fn llround();
    fn llroundf();
    fn mkdirat();
    fn posix_madvise();
    fn pthread_condattr_setclock();
    fn readlinkat();
    fn renameat();
    fn rewinddir();
    fn scalbnf();
    fn sendfile64();
    fn symlinkat();
    fn timerfd_create();
    fn timerfd_settime();
    fn wait();
    fn setbuf(stream: *mut libc::FILE, buf: *mut c_char);
    fn malloc_usable_size(ptr: *mut c_void) -> usize;
}

/// Address table for the forwarded entries.
pub fn forwarded_table() -> Vec<(&'static str, *const c_void)> {
    macro_rules! addrs {
        ($($name:ident),+ $(,)?) => {
            vec![$( (stringify!($name), $name as *const () as *const c_void) ),+]
        };
    }
    addrs!(
        __cmsg_nxthdr, __poll_chk, __pread_chk, __read_chk, __readlink_chk, close_range,
        epoll_create1, epoll_ctl, epoll_wait, fma, freeaddrinfo, freeifaddrs, ftruncate64,
        gai_strerror, getaddrinfo, getifaddrs, getnameinfo, if_nametoindex, inet_aton,
        inet_ntop, inet_pton, inotify_add_watch, inotify_init1, inotify_rm_watch, llround,
        llroundf, mkdirat, posix_madvise, pthread_condattr_setclock, readlinkat, renameat,
        rewinddir, scalbnf, sendfile64, symlinkat, timerfd_create, timerfd_settime, wait,
    )
}

/// `environ` is a *variable*, not a function: the guest GOT entry must hold the
/// address of glibc's `environ` slot, which itself holds the `char**`.
pub fn environ_addr() -> *const c_void {
    std::ptr::addr_of!(libc::environ) as *const c_void
}

// --- translating shims -----------------------------------------------------

/// `__openat_2(dirfd, path, flags)` — bionic's `_FORTIFY_SOURCE` `openat`
/// entry. The three-argument spelling always has no `O_CREAT`, so glibc's
/// variadic `openat` sees the same effective mode of `0`.
///
/// # Safety
/// `path` must be a NUL-terminated guest string.
pub unsafe extern "C" fn __openat_2(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
) -> c_int {
    libc::openat(dirfd, path, flags)
}

/// `feof(stream)` — translates a bionic `__sF` slot (or a host `FILE*` our
/// stdio shims handed out) before calling glibc.
///
/// # Safety
/// `stream` is a bionic `FILE*` or a NULL/unknown pointer; unknown streams are
/// rejected with `EBADF` instead of reaching glibc.
pub unsafe extern "C" fn feof(stream: *mut c_void) -> c_int {
    let real = acos_map_stream(stream as usize) as *mut libc::FILE;
    if real.is_null() {
        *libc::__errno_location() = libc::EBADF;
        return 0;
    }
    libc::feof(real)
}

/// `setbuf(stream, buf)` — same stream translation as [`feof`].
///
/// # Safety
/// `stream` is a bionic `FILE*` or a NULL/unknown pointer; unknown streams are
/// ignored.
pub unsafe extern "C" fn setbuf_shim(stream: *mut c_void, buf: *mut c_char) {
    let real = acos_map_stream(stream as usize) as *mut libc::FILE;
    if real.is_null() {
        return;
    }
    setbuf(real, buf);
}

// --- bionic 8-byte sigset_t ------------------------------------------------

/// `sigprocmask(how, set, oldset)`.
///
/// Bionic `sigset_t` is an 8-byte bitmask; glibc's is 128 bytes. Translate in
/// both directions (the low word carries the same signal bits).
///
/// # Safety
/// `set`/`old` must point to 8-byte bionic `sigset_t` objects (or be NULL).
pub unsafe extern "C" fn sigprocmask(
    how: c_int,
    set: *const u64,
    old: *mut u64,
) -> c_int {
    let mut gset: libc::sigset_t = std::mem::zeroed();
    let mut gold: libc::sigset_t = std::mem::zeroed();
    let set_ptr = if set.is_null() {
        std::ptr::null()
    } else {
        let words: *mut u64 = &mut gset as *mut libc::sigset_t as *mut u64;
        *words = *set;
        &gset
    };
    let old_ptr = if old.is_null() {
        std::ptr::null_mut()
    } else {
        &mut gold
    };
    let rc = libc::sigprocmask(how, set_ptr, old_ptr);
    if rc == 0 && !old.is_null() {
        let words: *const u64 = &gold as *const libc::sigset_t as *const u64;
        *old = *words;
    }
    rc
}

// --- fail-closed dangerous calls -------------------------------------------

/// Structured report for a call we deliberately refuse to perform.
unsafe fn refuse(symbol: &str, errno: c_int) -> c_int {
    eprintln!(
        "ACOS_UNSUPPORTED_NATIVE_SYMBOL {}",
        serde_json::json!({
            "code": "FAIL_CLOSED",
            "symbol": symbol,
            "reason": "guest called an operation the confined runtime refuses",
        })
    );
    *libc::__errno_location() = errno;
    -1
}

/// `fork()` — a confined ACOS process never spawns a child.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn fork() -> libc::pid_t {
    refuse("fork", libc::ENOSYS)
}

/// `execvp(file, argv)` — never replaces the confined image.
///
/// # Safety
/// Arguments are unused.
pub unsafe extern "C" fn execvp(_file: *const c_char, _argv: *const *const c_char) -> c_int {
    refuse("execvp", libc::EACCES)
}

// --- bionic jmp_buf: real setjmp/longjmp -----------------------------------
//
// Bionic's x86_64 `jmp_buf` is 11 words (`_JBLEN = 11`, 88 bytes) and its
// `setjmp` saves the callee-saved GPRs plus `rsp`/`rip`. Glibc's `jmp_buf` is
// a different, larger, pointer-mangled layout, so forwarding smashes the guest
// stack. These save/restore only the classic registers into the first 64 bytes,
// which is self-consistent and leaves the guest's 88-byte buffer untouched past
// that point. Dart's `LongJump` saves the buffer and later `longjmp`s back to
// it; it never inspects the layout, so this is the exact contract it needs.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".globl acos_setjmp",
    ".hidden acos_setjmp",
    ".type acos_setjmp, @function",
    "acos_setjmp:",
    "  mov [rdi + 0], rbx",
    "  mov [rdi + 8], rbp",
    "  mov [rdi + 16], r12",
    "  mov [rdi + 24], r13",
    "  mov [rdi + 32], r14",
    "  mov [rdi + 40], r15",
    "  lea rax, [rsp + 8]",
    "  mov [rdi + 48], rax",
    "  mov rax, [rsp]",
    "  mov [rdi + 56], rax",
    "  xor eax, eax",
    "  ret",
    ".size acos_setjmp, .-acos_setjmp",
    ".globl acos_longjmp",
    ".hidden acos_longjmp",
    ".type acos_longjmp, @function",
    "acos_longjmp:",
    "  mov rbx, [rdi + 0]",
    "  mov rbp, [rdi + 8]",
    "  mov r12, [rdi + 16]",
    "  mov r13, [rdi + 24]",
    "  mov r14, [rdi + 32]",
    "  mov r15, [rdi + 40]",
    "  mov rsp, [rdi + 48]",
    "  mov rdx, [rdi + 56]",
    "  mov eax, esi",
    "  test eax, eax",
    "  jne 1f",
    "  mov eax, 1",
    "1:",
    "  jmp rdx",
    ".size acos_longjmp, .-acos_longjmp",
);

#[cfg(target_arch = "x86_64")]
extern "C" {
    fn acos_setjmp(env: *mut c_void) -> c_int;
    fn acos_longjmp(env: *mut c_void, val: c_int) -> !;
}

/// Fail-closed fallback for architectures without the bionic asm above.
#[cfg(not(target_arch = "x86_64"))]
unsafe fn trap(symbol: &str) -> ! {
    eprintln!(
        "ACOS UNSUPPORTED_NATIVE_SYMBOL: libflutter.so called unimplemented symbol {symbol:?} \
         (the bionic jmp_buf ABI differs from the host's; forwarding would smash the guest stack)"
    );
    std::process::abort()
}

/// `setjmp(env)` — structured trap (non-x86_64 fallback).
///
/// # Safety
/// Never returns; report only.
#[cfg(not(target_arch = "x86_64"))]
pub unsafe extern "C" fn setjmp_trap(_env: *mut c_void) -> c_int {
    trap("setjmp")
}

/// `longjmp(env, val)` — structured trap (non-x86_64 fallback).
///
/// # Safety
/// Never returns; report only.
#[cfg(not(target_arch = "x86_64"))]
pub unsafe extern "C" fn longjmp_trap(_env: *mut c_void, _val: c_int) -> ! {
    trap("longjmp")
}

// --- OpenSSL allocator hooks ----------------------------------------------
//
// `libflutter.so` imports these three as **weak** symbols (`w` in `nm -D`).
// BoringSSL exposes them so an embedder can install its own allocator; when
// none is installed the intended default is the process allocator. That is
// exactly what these provide, so they are trivially correct rather than stubs.

/// `OPENSSL_memory_alloc(size)` → the process allocator.
///
/// # Safety
/// `size` is passed straight to `malloc`.
pub unsafe extern "C" fn openssl_memory_alloc(size: usize) -> *mut c_void {
    libc::malloc(size)
}

/// `OPENSSL_memory_free(ptr)`.
///
/// # Safety
/// `ptr` must come from [`openssl_memory_alloc`] or be NULL.
pub unsafe extern "C" fn openssl_memory_free(ptr: *mut c_void) {
    libc::free(ptr);
}

/// `OPENSSL_memory_get_size(ptr)` → the allocator's usable size, matching
/// BoringSSL's `CRYPTO_get_alloc_size` contract.
///
/// # Safety
/// `ptr` must be a live allocation or NULL.
pub unsafe extern "C" fn openssl_memory_get_size(ptr: *mut c_void) -> usize {
    if ptr.is_null() {
        return 0;
    }
    malloc_usable_size(ptr)
}

/// The complete shim table for this module.
pub fn table() -> std::collections::HashMap<&'static str, super::Shim> {
    let mut m = std::collections::HashMap::new();
    for (name, ptr) in forwarded_table() {
        m.insert(name, super::Shim::forwarded(name, ptr));
    }
    m.insert(
        "environ",
        super::Shim::forwarded("environ", environ_addr()),
    );
    m.insert(
        "malloc_usable_size",
        super::Shim::forwarded("malloc_usable_size", malloc_usable_size as *const () as *const c_void),
    );
    let mut shim = |name: &'static str, ptr: *const c_void| {
        m.insert(name, super::Shim::shimmed(name, ptr));
    };
    shim("__openat_2", __openat_2 as *const () as *const c_void);
    shim("feof", feof as *const () as *const c_void);
    shim("setbuf", setbuf_shim as *const () as *const c_void);
    shim("sigprocmask", sigprocmask as *const () as *const c_void);
    shim("fork", fork as *const () as *const c_void);
    shim("execvp", execvp as *const () as *const c_void);
    #[cfg(target_arch = "x86_64")]
    {
        shim("setjmp", acos_setjmp as *const () as *const c_void);
        shim("longjmp", acos_longjmp as *const () as *const c_void);
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        shim("setjmp", setjmp_trap as *const () as *const c_void);
        shim("longjmp", longjmp_trap as *const () as *const c_void);
    }
    shim(
        "OPENSSL_memory_alloc",
        openssl_memory_alloc as *const () as *const c_void,
    );
    shim(
        "OPENSSL_memory_free",
        openssl_memory_free as *const () as *const c_void,
    );
    shim(
        "OPENSSL_memory_get_size",
        openssl_memory_get_size as *const () as *const c_void,
    );
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_symbol_is_present() {
        let t = table();
        for name in [
            "__cmsg_nxthdr",
            "__openat_2",
            "__poll_chk",
            "__pread_chk",
            "__read_chk",
            "__readlink_chk",
            "close_range",
            "environ",
            "epoll_create1",
            "epoll_ctl",
            "epoll_wait",
            "execvp",
            "feof",
            "fma",
            "fork",
            "freeaddrinfo",
            "freeifaddrs",
            "ftruncate64",
            "gai_strerror",
            "getaddrinfo",
            "getifaddrs",
            "getnameinfo",
            "if_nametoindex",
            "inet_aton",
            "inet_ntop",
            "inet_pton",
            "inotify_add_watch",
            "inotify_init1",
            "inotify_rm_watch",
            "llround",
            "llroundf",
            "longjmp",
            "malloc_usable_size",
            "mkdirat",
            "posix_madvise",
            "pthread_condattr_setclock",
            "readlinkat",
            "renameat",
            "rewinddir",
            "scalbnf",
            "sendfile64",
            "setbuf",
            "setjmp",
            "sigprocmask",
            "symlinkat",
            "timerfd_create",
            "timerfd_settime",
            "wait",
            "OPENSSL_memory_alloc",
            "OPENSSL_memory_free",
            "OPENSSL_memory_get_size",
        ] {
            assert!(t.contains_key(name), "missing shim for {name}");
        }
    }

    #[test]
    fn openssl_hooks_roundtrip_through_the_process_allocator() {
        // SAFETY: the hooks take/return plain allocations.
        unsafe {
            let p = openssl_memory_alloc(64);
            assert!(!p.is_null());
            assert!(openssl_memory_get_size(p) >= 64);
            openssl_memory_free(p);
        }
    }
}
