//! Local/global reference management.
//!
//! JNI references are ACOS heap handles. A local reference is a GC root for the
//! lifetime of its local frame (or until `DeleteLocalRef`); a global reference
//! is rooted until `DeleteGlobalRef`/`DeleteWeakGlobalRef`. Rooting uses the
//! VM's precise `native_roots` set, so `ACOS_GC_STRESS=1` cannot move or free a
//! live JNI reference.

use std::cell::RefCell;
use std::rc::Rc;

use acos_vm::heap::NULL;
use acos_vm::{Handle, Vm};

use crate::state::{PendingException, State};

type Shared = Rc<RefCell<State>>;

/// Push a local frame.
pub(crate) fn push_frame(rt: &Shared, t: usize) {
    let mut st = rt.borrow_mut();
    st.threads
        .entry(t)
        .or_default()
        .frames
        .push(crate::state::LocalFrame::default());
}

/// Pop the top local frame, releasing every reference it owns.
///
/// `result` (if non-null) becomes a local reference in the caller's frame, as
/// `PopLocalFrame` specifies.
pub(crate) fn pop_frame(vm: &mut Vm, rt: &Shared, t: usize, result: Handle) -> Handle {
    let refs = {
        let mut st = rt.borrow_mut();
        let th = st.threads.entry(t).or_default();
        if th.frames.len() <= 1 {
            drop(st);
            return if result != NULL {
                local(vm, rt, t, result)
            } else {
                NULL
            };
        }
        th.frames.pop().map(|f| f.refs).unwrap_or_default()
    };
    for h in refs {
        let _ = vm.release_bridge_root(h);
    }
    if result != NULL {
        local(vm, rt, t, result)
    } else {
        NULL
    }
}

/// Register `h` as a local reference in the current frame.
pub(crate) fn local(vm: &mut Vm, rt: &Shared, t: usize, h: Handle) -> Handle {
    if h == NULL {
        return NULL;
    }
    vm.native_roots.push(h);
    let mut st = rt.borrow_mut();
    let th = st.threads.entry(t).or_default();
    if th.frames.is_empty() {
        th.frames.push(crate::state::LocalFrame::default());
    }
    if let Some(f) = th.frames.last_mut() {
        f.refs.push(h);
    }
    h
}

/// Delete a local reference (no-op if it is not tracked).
pub(crate) fn delete_local(vm: &mut Vm, rt: &Shared, t: usize, h: Handle) {
    if h == NULL {
        return;
    }
    let found = {
        let mut st = rt.borrow_mut();
        let th = st.threads.entry(t).or_default();
        let mut found = false;
        for f in th.frames.iter_mut().rev() {
            if let Some(i) = f.refs.iter().rposition(|x| *x == h) {
                f.refs.remove(i);
                found = true;
                break;
            }
        }
        found
    };
    if found {
        let _ = vm.release_bridge_root(h);
    }
}

/// Create a global reference.
pub(crate) fn global(vm: &mut Vm, rt: &Shared, h: Handle) -> Handle {
    if h == NULL {
        return NULL;
    }
    vm.native_roots.push(h);
    rt.borrow_mut().globals.push(h);
    h
}

/// Delete a global reference.
pub(crate) fn delete_global(vm: &mut Vm, rt: &Shared, h: Handle) {
    if h == NULL {
        return;
    }
    let found = {
        let mut st = rt.borrow_mut();
        if let Some(i) = st.globals.iter().rposition(|x| *x == h) {
            st.globals.remove(i);
            true
        } else {
            false
        }
    };
    if found {
        let _ = vm.release_bridge_root(h);
    }
}

/// True when `h` is a registered global reference.
pub(crate) fn is_global(rt: &Shared, h: Handle) -> bool {
    rt.borrow().globals.contains(&h)
}

/// True when `h` is a local reference in any frame of thread `t`.
pub(crate) fn is_local(rt: &Shared, t: usize, h: Handle) -> bool {
    rt.borrow()
        .threads
        .get(&t)
        .map(|th| th.frames.iter().any(|f| f.refs.contains(&h)))
        .unwrap_or(false)
}

/// Install a pending exception, rooting it until it is cleared or propagated.
pub(crate) fn set_pending(vm: &mut Vm, rt: &Shared, t: usize, h: Handle) {
    if h == NULL {
        return;
    }
    let replaced = {
        let mut st = rt.borrow_mut();
        let th = st.threads.entry(t).or_default();
        th.pending.replace(PendingException {
            handle: h,
            rooted: true,
        })
    };
    vm.native_roots.push(h);
    if let Some(old) = replaced {
        if old.rooted {
            let _ = vm.release_bridge_root(old.handle);
        }
    }
}

/// Take the pending exception, releasing this module's root. The caller must
/// keep the handle alive (the interpreter roots a propagating exception).
pub(crate) fn take_pending(vm: &mut Vm, rt: &Shared, t: usize) -> Option<Handle> {
    let pending = {
        let mut st = rt.borrow_mut();
        st.threads.get_mut(&t).and_then(|th| th.pending.take())
    };
    pending.map(|p| {
        if p.rooted {
            let _ = vm.release_bridge_root(p.handle);
        }
        p.handle
    })
}

/// Drop the pending exception (ExceptionClear).
pub(crate) fn clear_pending(vm: &mut Vm, rt: &Shared, t: usize) {
    let _ = take_pending(vm, rt, t);
}

/// Inspect the pending exception without clearing it.
pub(crate) fn pending(rt: &Shared, t: usize) -> Option<Handle> {
    rt.borrow()
        .threads
        .get(&t)
        .and_then(|th| th.pending.as_ref().map(|p| p.handle))
}

/// Record a structured error for the current thread.
pub(crate) fn set_error(rt: &Shared, t: usize, e: crate::error::JniError) {
    rt.borrow_mut().threads.entry(t).or_default().error = Some(e);
}

/// Take the structured error recorded on thread `t`.
pub(crate) fn take_error(rt: &Shared, t: usize) -> Option<crate::error::JniError> {
    rt.borrow_mut()
        .threads
        .get_mut(&t)
        .and_then(|s| s.error.take())
}
