//! Host-libc surface used by the bionic shims.
//!
//! On the x86_64 glibc host this module is a plain re-export of the `libc`
//! crate, so every `libc::` path in the shim modules is unchanged.
//!
//! On `aarch64-linux-android` (W194-A: the cross-built runtime under
//! `qemu-aarch64`) the `libc` crate's Android module omits a handful of
//! symbols that bionic *does* export but does not declare: the
//! `__errno`/`environ` accessors, `pthread_once`/`pthread_getname_np`, and
//! several libc entry points whose names libc only exposes for glibc
//! (`abs`, `rand`, `mremap`, `dup3`, …). Declaring them here keeps the shim
//! source identical on both targets and leaves the x86_64 path byte-for-byte
//! unchanged.

#[cfg(not(target_os = "android"))]
pub use ::libc::*;

#[cfg(target_os = "android")]
mod android {
    use core::ffi::{c_char, c_int, c_long, c_void};

    pub use ::libc::*;

    extern "C" {
        fn __errno() -> *mut c_int;

        /// bionic's process environment (`char**`).
        pub static mut environ: *mut *mut c_char;

        pub fn pthread_once(once_control: *mut c_int, init: Option<extern "C" fn()>) -> c_int;
        pub fn pthread_getname_np(thread: ::libc::pthread_t, buf: *mut c_char, len: usize)
            -> c_int;

        pub fn abs(value: c_int) -> c_int;
        pub fn labs(value: c_long) -> c_long;
        pub fn rand() -> c_int;
        pub fn srand(seed: u32);
        pub fn drand48() -> f64;
        pub fn lrand48() -> c_long;
        pub fn mrand48() -> c_long;

        /// `mremap` is variadic in bionic (`..., void *new_address`).
        pub fn mremap(
            old_address: *mut c_void,
            old_size: usize,
            new_size: usize,
            flags: c_int,
            ...
        ) -> *mut c_void;

        pub fn dup3(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int;
    }

    /// bionic spells the errno accessor `__errno`; glibc's name is
    /// `__errno_location`. The shims use the glibc name on every target.
    ///
    /// # Safety
    /// Always safe; bionic returns a valid thread-local pointer.
    pub unsafe fn __errno_location() -> *mut c_int {
        // SAFETY: `__errno` has no preconditions.
        unsafe { __errno() }
    }

    /// bionic's `pthread_once_t` is a plain 32-bit flag (`int`).
    #[allow(non_camel_case_types)]
    pub type pthread_once_t = c_int;
}

#[cfg(target_os = "android")]
pub use android::*;
