//! Bionic shims for the imports `libflutter.so` needs that no earlier
//! workstream provided.
//!
//! Every symbol here was a *missing* entry in
//! `results/flutter/native-inventory.json` (48 libc/other + 3 OpenSSL; the
//! EGL/GLES imports are served by `acos-gles`, see
//! [`crate::bionic::gles`]).  The Glibc ABI for these names on x86_64 is
//! identical to bionic's, so most are *forwarded*; the few that differ
//! (fortified `openat`, `FILE*` streams, the bionic 8-byte `sigset_t`) get a
//! translating shim, and the non-local-jump entries (`setjmp`, `_setjmp`,
//! `sigsetjmp`, `longjmp`, `_longjmp`, `siglongjmp`) are real bionic-ABI
//! save/restore code: a bionic-sized `jmp_buf` is never handed to the host's
//! different, pointer-mangled glibc `setjmp`.
//!
//! Nothing here silently fakes success. `fork`/`execvp` are fail-closed
//! *returning* shims (the Android contract for a restricted process is an
//! error), and the OpenSSL hooks are the trivial `malloc`/`free`/
//! `malloc_usable_size` allocator they name.

use std::os::raw::{c_char, c_int, c_void};

use super::shims::acos_map_stream;
// W194-A: host-libc surface (see `bionic::host`).
use crate::bionic::host as libc;

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
        rewinddir, scalbnf, sendfile64, symlinkat, wait,
    )
}

/// `environ` is a *variable*, not a function: the guest GOT entry must hold the
/// address of glibc's `environ` slot, which itself holds the `char**`.
pub fn environ_addr() -> *const c_void {
    std::ptr::addr_of!(libc::environ) as *const c_void
}

// --- W113-A timerfd trace (compiled, inert by default) ---------------------

/// Flip to `true` to trace the engine's `timerfd_create`/`timerfd_settime`
/// (deadline/it_value) with the calling thread id. Off in the committed
/// configuration for the same timing reason as the looper trace; see
/// `results/flutter/FIRST_FRAME.md` (W113-A §4).
const TIMER_TRACE: bool = false;

fn gap_trace(args: std::fmt::Arguments<'_>) {
    if !TIMER_TRACE {
        return;
    }
    use std::sync::atomic::AtomicUsize;
    static N: AtomicUsize = AtomicUsize::new(0);
    if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 200 {
        eprintln!(
            "ACOS W113A timer tid={:?} {args}",
            std::thread::current().id()
        );
    }
}

/// `timerfd_create(clockid, flags)` with an inert-by-default trace.
///
/// # Safety
/// Same contract as the libc entry.
pub unsafe extern "C" fn acos_timerfd_create(clockid: c_int, flags: c_int) -> c_int {
    let fd = libc::timerfd_create(clockid, flags);
    gap_trace(format_args!("create clock={clockid} flags={flags} -> {fd}"));
    fd
}

/// `timerfd_settime(fd, flags, new_value, old_value)` with an inert trace.
///
/// # Safety
/// Same contract as the libc entry.
pub unsafe extern "C" fn acos_timerfd_settime(
    fd: c_int,
    flags: c_int,
    new_value: *const libc::itimerspec,
    old_value: *mut libc::itimerspec,
) -> c_int {
    let r = libc::timerfd_settime(fd, flags, new_value, old_value);
    // Only dereference `new_value` when the trace is enabled: the pointer is
    // guest-supplied and may be invalid, in which case libc returns EFAULT but
    // an unconditional `*new_value` here would fault the host (W121-A).
    if TIMER_TRACE && !new_value.is_null() {
        let v = (*new_value).it_value;
        let i = (*new_value).it_interval;
        gap_trace(format_args!(
            "settime fd={fd} flags={flags} value={}.{:09} interval={}.{:09} -> {r}",
            v.tv_sec, v.tv_nsec, i.tv_sec, i.tv_nsec
        ));
    }
    r
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
// Bionic's x86_64 `jmp_buf` is 11 words (`_JBLEN = 11`, 88 bytes) and
// `sigjmp_buf` is 12. Bionic's `setjmp` *also* saves the signal mask
// (`<setjmp.h>`: "setjmp is equivalent to sigsetjmp(env, 1)"), `_setjmp` does
// not. Glibc's `jmp_buf` is a different, larger, pointer-mangled layout, so
// forwarding either one smashes the guest stack.
//
// These entry points are real save/restore code, not traps, and they never
// route through the host `setjmp` family (whose buffer layout differs). The
// buffer layout used here is private to this file and self-consistent, which
// is all the guest can depend on (bionic documents the buffer as opaque and
// nothing in the corpus inspects it):
//
//   words 0..7  rbx, rbp, r12, r13, r14, r15, rsp, rip
//   word  8     flags: 0x534f4341 ("ACOS") | 1 when the mask word is valid
//   word  9     bionic 8-byte `sigset_t` saved with `rt_sigprocmask`
//   word  10    unused (within `jmp_buf`'s 11 words)
//
// The save runs at the *shim entry* (there is no Rust wrapper frame between the
// guest's call and the saved `rsp`/`rip`), so `longjmp` resumes the caller of
// the shim, i.e. it genuinely returns twice at the guest call site; the
// intermediate shim frame is dead either way. `sigsetjmp`/`siglongjmp` are
// tail-jumps, never calls, for the same reason.
//
// Signal masks use the kernel's `rt_sigprocmask` with an 8-byte sigset, which
// is exactly bionic's `sigset_t`; the host glibc 128-byte `sigset_t` is never
// involved, so no translation layer is needed either.

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
    "  mov qword ptr [rdi + 64], 0",
    "  xor eax, eax",
    "  ret",
    ".size acos_setjmp, .-acos_setjmp",
    ".globl acos_setjmp_mask",
    ".hidden acos_setjmp_mask",
    ".type acos_setjmp_mask, @function",
    "acos_setjmp_mask:",
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
    "  mov qword ptr [rdi + 64], 0",
    // rt_sigprocmask(SIG_SETMASK, NULL, &env[9], 8) -> saves the mask.
    "  push rdi",
    "  lea rdx, [rdi + 72]",
    "  mov eax, 14",
    "  mov edi, 2",
    "  xor esi, esi",
    "  mov r10d, 8",
    "  syscall",
    "  pop rdi",
    "  mov eax, 0x534f4341",
    "  or rax, 1",
    "  mov [rdi + 64], rax",
    "  xor eax, eax",
    "  ret",
    ".size acos_setjmp_mask, .-acos_setjmp_mask",
    // sigsetjmp(env, savemask): tail-jump, no extra frame.
    ".globl acos_sigsetjmp",
    ".hidden acos_sigsetjmp",
    ".type acos_sigsetjmp, @function",
    "acos_sigsetjmp:",
    "  test esi, esi",
    "  jne 1f",
    "  jmp acos_setjmp",
    "1:",
    "  jmp acos_setjmp_mask",
    ".size acos_sigsetjmp, .-acos_sigsetjmp",
    ".globl acos_longjmp",
    ".hidden acos_longjmp",
    ".type acos_longjmp, @function",
    "acos_longjmp:",
    // Restore the saved mask first (if this buffer has one), while rdi/rsi are
    // still live and the current stack is still valid.
    "  mov rcx, [rdi + 64]",
    "  mov rdx, 0x534f4341",
    "  and rcx, rdx",
    "  cmp rcx, rdx",
    "  jne 2f",
    "  test byte ptr [rdi + 64], 1",
    "  jz 2f",
    "  push rsi",
    "  push rdi",
    "  lea rsi, [rdi + 72]",
    "  mov eax, 14",
    "  mov edi, 2",
    "  xor edx, edx",
    "  mov r10d, 8",
    "  syscall",
    "  pop rdi",
    "  pop rsi",
    "2:",
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
    "  jne 3f",
    "  mov eax, 1",
    "3:",
    "  jmp rdx",
    ".size acos_longjmp, .-acos_longjmp",
);

#[cfg(target_arch = "x86_64")]
extern "C" {
    fn acos_setjmp(env: *mut c_void) -> c_int;
    fn acos_setjmp_mask(env: *mut c_void) -> c_int;
    fn acos_sigsetjmp(env: *mut c_void, save: c_int) -> c_int;
    fn acos_longjmp(env: *mut c_void, val: c_int) -> !;
}

/// Fail-closed fallback for architectures without the bionic asm above.
#[cfg(not(target_arch = "x86_64"))]
unsafe fn trap(symbol: &str) -> ! {
    eprintln!(
        "ACOS_UNSUPPORTED_NATIVE_SYMBOL: libflutter.so called unimplemented symbol {symbol:?} \
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
    shim(
        "timerfd_create",
        acos_timerfd_create as *const () as *const c_void,
    );
    shim(
        "timerfd_settime",
        acos_timerfd_settime as *const () as *const c_void,
    );
    shim("fork", fork as *const () as *const c_void);
    shim("execvp", execvp as *const () as *const c_void);
    #[cfg(target_arch = "x86_64")]
    {
        // bionic: setjmp == sigsetjmp(env, 1), _setjmp saves registers only.
        // `libhermesvm.so` imports exactly the underscore pair.
        shim("setjmp", acos_setjmp_mask as *const () as *const c_void);
        shim("_setjmp", acos_setjmp as *const () as *const c_void);
        shim("sigsetjmp", acos_sigsetjmp as *const () as *const c_void);
        shim("longjmp", acos_longjmp as *const () as *const c_void);
        shim("_longjmp", acos_longjmp as *const () as *const c_void);
        shim("siglongjmp", acos_longjmp as *const () as *const c_void);
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        shim("setjmp", setjmp_trap as *const () as *const c_void);
        shim("_setjmp", setjmp_trap as *const () as *const c_void);
        shim("sigsetjmp", setjmp_trap as *const () as *const c_void);
        shim("longjmp", longjmp_trap as *const () as *const c_void);
        shim("_longjmp", longjmp_trap as *const () as *const c_void);
        shim("siglongjmp", longjmp_trap as *const () as *const c_void);
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

// ---------------------------------------------------------------------------
// Guest `/system` path translation (W99-A)
// ---------------------------------------------------------------------------

/// Translate a guest *system* path to the extracted boot tree.
///
/// Android libraries open absolute paths (`/system/etc/fonts.xml`,
/// `/system/fonts/Roboto-Regular.ttf`) that exist in the APK/boot extraction,
/// not on the host root. `ACOS_BOOT_DIR` names that extraction (the harness /
/// runtime sets it); the mapping mirrors the VM's VFS table
/// (`crates/acos-vm/src/vfs.rs`): `/system/<rest>` -> `<boot>/<rest>`, with the
/// framework's `fonts/` placement for `etc/fonts*.xml`.
///
/// Returns `None` when the path is not a translated `/system` path or when no
/// candidate exists, so callers keep the original path (and the normal
/// `ENOENT` contract) instead of inventing a file.
#[cfg(unix)]
pub fn translate_guest_system_path(path: &str) -> Option<String> {
    let boot = std::env::var_os("ACOS_BOOT_DIR")?;
    let boot = std::path::PathBuf::from(boot);
    let rest = path.strip_prefix("/system/")?;
    // The suffix is guest-controlled: reject any `..`, root or prefix
    // component so `/system/../../etc/passwd` cannot escape the boot tree
    // through `Path::join` (W121-A).
    if has_escaping_component(rest) {
        return None;
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(font) = rest.strip_prefix("fonts/") {
        candidates.push(boot.join("fonts").join(font));
    } else if let Some(etc) = rest.strip_prefix("etc/") {
        // The boot extraction keeps `fonts.xml`/`fallback_fonts.xml` under
        // `fonts/`, while the guest spells them under `etc/`.
        candidates.push(boot.join("etc").join(etc));
        candidates.push(boot.join("fonts").join(etc));
    } else {
        candidates.push(boot.join(rest));
    }
    for candidate in candidates {
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// Whether a guest-supplied `/system/` suffix contains a component that would
/// escape the boot tree when joined (`..`, an absolute root, or a Windows-style
/// prefix). Pure so it is unit-testable without mutating the environment.
fn has_escaping_component(rest: &str) -> bool {
    std::path::Path::new(rest)
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
}

/// C ABI used by the `fopen` shim: write the translated path into `out`.
///
/// Returns the number of bytes written (excluding the NUL) or 0 when the path
/// is not translated. `out` is only written when a translation exists, so the
/// caller can fall back to the original pointer.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn acos_translate_guest_path(
    path: *const std::os::raw::c_char,
    out: *mut std::os::raw::c_char,
    out_len: usize,
) -> usize {
    if path.is_null() || out.is_null() || out_len == 0 {
        return 0;
    }
    // SAFETY: the caller (C stdio shim) passes a NUL-terminated guest string
    // and a writable buffer of `out_len` bytes.
    let Some(text) = unsafe { std::ffi::CStr::from_ptr(path) }.to_str().ok() else {
        return 0;
    };
    let Some(translated) = translate_guest_system_path(text) else {
        return 0;
    };
    let bytes = translated.as_bytes();
    if bytes.len() + 1 > out_len {
        return 0;
    }
    // SAFETY: bounds checked above; the destination is a valid writable buffer.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, bytes.len());
        *out.add(bytes.len()) = 0;
    }
    bytes.len()
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
            "wait",
            "OPENSSL_memory_alloc",
            "OPENSSL_memory_free",
            "OPENSSL_memory_get_size",
        ] {
            assert!(t.contains_key(name), "missing shim for {name}");
        }
    }

    #[test]
    fn system_path_suffix_cannot_escape_the_boot_tree() {
        for rest in ["../etc/passwd", "fonts/../../etc/passwd", "/etc/passwd", "a/../../b"] {
            assert!(has_escaping_component(rest), "must reject {rest:?}");
        }
        for rest in ["fonts/Roboto-Regular.ttf", "etc/fonts.xml", "a/b/c"] {
            assert!(!has_escaping_component(rest), "must accept {rest:?}");
        }
    }

    #[test]
    fn timerfd_settime_tolerates_an_invalid_pointer() {
        // With the trace inert the guest pointer is never dereferenced; a
        // nonsense `new_value` must not fault the host (W121-A).
        // SAFETY: the shim only forwards the pointer to the kernel.
        let rc = unsafe {
            acos_timerfd_settime(-1, 0, 0xdead_beef_usize as *const libc::itimerspec, std::ptr::null_mut())
        };
        assert_eq!(rc, -1);
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

