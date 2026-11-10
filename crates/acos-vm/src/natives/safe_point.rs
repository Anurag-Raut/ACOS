//! VM safe points for host-side brokers.
//!
//! `acos-vm` runs one VM on one host thread and is `#![forbid(unsafe_code)]`.
//! A host integration (the JNI crate) sometimes needs to run code on that owner
//! thread while it is *not* in the middle of a Rust call into the VM: this is
//! how a broker services the request queue of native OS threads without ever
//! touching the VM from two threads at once.
//!
//! A *safe-point hook* is an ordinary `extern "C"` function pointer taking the
//! owner pointer to the VM. It is registered once at process start and invoked
//! from the VM's own yield points (the interpreter scheduler and the looper
//! pump). The pointer is only dereferenced by the crate that registered it, and
//! only from the owner thread, so the VM is never aliased from two threads.
//!
//! This module is safe Rust: registering and calling a function pointer is
//! safe; the callee owns the (documented) `unsafe` dereference.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::vm::Vm;

/// Runs at a VM safe point on the owner thread. `vm` is the owner pointer; the
/// function returns after it has finished touching the VM.
pub type SafePointHook = extern "C" fn(*mut Vm);

static HOOK: OnceLock<Mutex<Option<SafePointHook>>> = OnceLock::new();
static ENTERED: AtomicBool = AtomicBool::new(false);

fn hook_slot() -> &'static Mutex<Option<SafePointHook>> {
    HOOK.get_or_init(|| Mutex::new(None))
}

/// Register (or clear, with `None`) the safe-point hook.
pub fn register(hook: Option<SafePointHook>) {
    let mut slot = hook_slot().lock().unwrap_or_else(|e| e.into_inner());
    *slot = hook;
}

/// Invoke the safe-point hook, if any.
///
/// Called from the VM's own code, which only ever runs on the owner thread.
/// Re-entrant calls are ignored: the broker's own nested pump is driven from
/// the hook itself, not by re-entering here.
pub(crate) fn pump(vm: &mut Vm) {
    let hook = {
        let slot = hook_slot().lock().unwrap_or_else(|e| e.into_inner());
        *slot
    };
    let Some(hook) = hook else {
        return;
    };
    if ENTERED.swap(true, Ordering::AcqRel) {
        return;
    }
    hook(vm as *mut Vm);
    ENTERED.store(false, Ordering::Release);
}
