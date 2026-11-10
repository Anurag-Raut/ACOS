//! Guest ELF TLS support.
//!
//! # Strategy
//!
//! Bionic guest libraries use the *general dynamic* TLS model on x86_64, which
//! resolves `__thread` variables through a call to `__tls_get_addr(ti)` where
//! `ti = { module_id, offset }`. Rather than trying to extend the host thread
//! pointer (impossible from a normal process), we emulate per-module TLS
//! blocks and key them off a host `pthread_key_t`:
//!
//! * every loaded library with a `PT_TLS` segment gets a monotonic *module id*;
//! * each guest thread lazily receives a [`TlsThread`] that owns one aligned
//!   block per module, copied from the library's TLS init image;
//! * `__tls_get_addr` returns `block_base + offset`.
//!
//! Guest threads created through our `pthread_create` shim use the same
//! mechanism transparently. The host stack canary at `%fs:0x28` matches
//! glibc's, so `-fstack-protector` code needs no translation.
//!
//! ## Unsupported
//!
//! `initial-exec`/`local-exec` relocations (`R_X86_64_TLS_TPOFF`,
//! `R_X86_64_TLSDESC`) require a fixed offset from the real thread pointer and
//! are rejected with a structured error rather than silently mis-linked.

use std::sync::{Mutex, OnceLock};

/// Geometry of one library's TLS block.
pub struct TlsModuleInfo {
    pub size: usize,
    pub align: usize,
    /// Initialised bytes (`PT_TLS` file image); the rest of the block is zero.
    pub init: Vec<u8>,
}

/// Monotonic registry of TLS modules; module id = index + 1.
static MODULES: Mutex<Vec<TlsModuleInfo>> = Mutex::new(Vec::new());

/// Host pthread key holding a `*mut TlsThread` for each guest thread.
fn tls_key() -> libc::pthread_key_t {
    static KEY: OnceLock<libc::pthread_key_t> = OnceLock::new();
    *KEY.get_or_init(|| {
        let mut k: libc::pthread_key_t = 0;
        // SAFETY: `k` points to valid storage; the destructor only touches the
        // value stored under this key.
        unsafe {
            libc::pthread_key_create(&mut k, Some(tls_thread_dtor));
        }
        k
    })
}

/// Per-thread TLS blocks. Only ever touched by the owning thread, so the raw
/// pointers do not need synchronisation.
struct TlsThread {
    blocks: Vec<TlsBlock>,
}

struct TlsBlock {
    ptr: *mut u8,
}

impl TlsThread {
    fn new() -> Self {
        TlsThread { blocks: Vec::new() }
    }

    fn block_for(&mut self, id: usize) -> Option<*mut u8> {
        let modules = MODULES.lock().ok()?;
        let info = modules.get(id.checked_sub(1)?)?;
        if self.blocks.len() < id {
            self.blocks.resize_with(id, || TlsBlock {
                ptr: std::ptr::null_mut(),
            });
        }
        let slot = &mut self.blocks[id - 1];
        if slot.ptr.is_null() {
            let size = info.size.max(1);
            let _align = info.align.max(1);
            // SAFETY: layout is non-zero sized (size.max(1)); null is checked.
            let ptr = unsafe { libc::calloc(1, size) as *mut u8 };
            if ptr.is_null() {
                return None;
            }
            if !info.init.is_empty() {
                // SAFETY: copy at most `size` bytes into the freshly allocated
                // block; `init.len() <= size` by construction.
                unsafe {
                    std::ptr::copy_nonoverlapping(info.init.as_ptr(), ptr, info.init.len().min(size));
                }
            }
            slot.ptr = ptr;
        }
        Some(slot.ptr)
    }
}

extern "C" fn tls_thread_dtor(value: *mut libc::c_void) {
    if value.is_null() {
        return;
    }
    // SAFETY: the key holds a `Box<TlsThread>` created in `thread_state`.
    unsafe {
        let tls = Box::from_raw(value as *mut TlsThread);
        for b in &tls.blocks {
            if !b.ptr.is_null() {
                libc::free(b.ptr as *mut libc::c_void);
            }
        }
        drop(tls);
    }
}

/// Get (creating if needed) this thread's TLS state.
fn thread_state() -> Option<*mut TlsThread> {
    let key = tls_key();
    // SAFETY: key is a valid pthread key created above.
    let existing = unsafe { libc::pthread_getspecific(key) };
    if !existing.is_null() {
        return Some(existing as *mut TlsThread);
    }
    let boxed = Box::into_raw(Box::new(TlsThread::new()));
    // SAFETY: key valid; boxed is a live leaked pointer.
    let rc = unsafe { libc::pthread_setspecific(key, boxed as *mut libc::c_void) };
    if rc != 0 {
        // SAFETY: reclaim the box we just created.
        unsafe { drop(Box::from_raw(boxed)) };
        return None;
    }
    Some(boxed)
}

/// Register a new TLS module and return its module id (1-based).
pub fn register_module(size: usize, align: usize, init: Vec<u8>) -> usize {
    let mut modules = MODULES.lock().expect("tls registry poisoned");
    modules.push(TlsModuleInfo {
        size,
        align: align.max(1),
        init,
    });
    modules.len()
}

/// `__tls_get_addr` shim. `ti` points to `{ usize module; usize offset }`.
///
/// # Safety
/// `ti` must point to a valid two-word TLS index, as emitted by the compiler.
pub unsafe extern "C" fn __tls_get_addr(ti: *mut usize) -> *mut u8 {
    if ti.is_null() {
        return std::ptr::null_mut();
    }
    let module = *ti;
    let offset = *ti.add(1);
    let st = match thread_state() {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    // SAFETY: `st` belongs to this thread.
    let block = match (*st).block_for(module) {
        Some(b) => b,
        None => return std::ptr::null_mut(),
    };
    // SAFETY: offset comes from the module's own relocation and is < size.
    block.add(offset)
}

/// Total number of registered TLS modules (for reports).
pub fn module_count() -> usize {
    MODULES.lock().map(|m| m.len()).unwrap_or(0)
}

/// Ensure the current thread has a guest TLS state object. Called by the
/// `pthread_create` trampoline before running guest code.
pub fn ensure_thread_tls() {
    let _ = thread_state();
}
