//! Process-wide binder identity, service-manager table and calling identity.
//!
//! `BinderRegistry` issues the stable `u64` handles that go into a parcel's
//! 24-byte flat objects. The per-thread calling-identity stack and strict-mode
//! policy live in `thread_local!` storage so two VM threads never see each
//! other's caller.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use crate::binder::{BinderId, BinderKind};
use crate::error::{ParcelError, Result};

// ----------------------------------------------------------------------
// calling identity (thread-local)
// ----------------------------------------------------------------------

/// A `{uid, pid}` calling identity as seen by `Binder.getCallingUid/Pid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallingIdentity {
    /// Linux uid.
    pub uid: u32,
    /// Linux pid.
    pub pid: i32,
}

impl CallingIdentity {
    /// Construct an identity.
    #[must_use]
    pub const fn new(uid: u32, pid: i32) -> Self {
        Self { uid, pid }
    }
}

/// Opaque token returned by [`clear_calling_identity`].
///
/// The token is handed back to [`restore_calling_identity`], which enforces
/// LIFO ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityToken {
    saved: CallingIdentity,
    seq: u64,
    was_empty: bool,
}

thread_local! {
    static IDENTITY_STACK: RefCell<Vec<CallingIdentity>> = const { RefCell::new(Vec::new()) };
    static CLEAR_STACK: RefCell<Vec<IdentityToken>> = const { RefCell::new(Vec::new()) };
    static NEXT_TOKEN: Cell<u64> = const { Cell::new(1) };
    static PROCESS_IDENTITY: Cell<CallingIdentity> = const { Cell::new(CallingIdentity { uid: 0, pid: 0 }) };
    static STRICT_MODE: Cell<i32> = const { Cell::new(0) };
    static WORK_SOURCE: Cell<i32> = const { Cell::new(-1) };
    static PROPAGATE_WORK_SOURCE: Cell<bool> = const { Cell::new(false) };
}

/// Set the identity reported when no caller is on the stack (this process).
pub fn set_process_identity(uid: u32, pid: i32) {
    PROCESS_IDENTITY.with(|c| c.set(CallingIdentity::new(uid, pid)));
}

/// The identity reported for "this process".
#[must_use]
pub fn process_identity() -> CallingIdentity {
    PROCESS_IDENTITY.with(Cell::get)
}

/// Push a dispatcher-supplied caller identity.
pub fn push_calling_identity(uid: u32, pid: i32) {
    IDENTITY_STACK.with(|s| s.borrow_mut().push(CallingIdentity::new(uid, pid)));
}

/// Pop the most recent caller identity.
pub fn pop_calling_identity() -> Option<CallingIdentity> {
    IDENTITY_STACK.with(|s| s.borrow_mut().pop())
}

/// Effective caller identity: the top of the stack, else the process identity.
#[must_use]
pub fn calling_identity() -> CallingIdentity {
    IDENTITY_STACK.with(|s| {
        s.borrow()
            .last()
            .copied()
            .unwrap_or_else(process_identity)
    })
}

/// `Binder.getCallingUid()`.
#[must_use]
pub fn calling_uid() -> u32 {
    calling_identity().uid
}

/// `Binder.getCallingPid()`.
#[must_use]
pub fn calling_pid() -> i32 {
    calling_identity().pid
}

/// `Binder.clearCallingIdentity()`: temporarily report this process's identity.
///
/// # Panics
///
/// Never panics; the internal stacks are only borrowed for the duration of a
/// call and cannot be held across a re-entrant call.
pub fn clear_calling_identity() -> IdentityToken {
    let process = process_identity();
    let (saved, was_empty) = IDENTITY_STACK.with(|s| {
        let mut stack = s.borrow_mut();
        let was_empty = stack.is_empty();
        let saved = *stack.last().unwrap_or(&process);
        if was_empty {
            stack.push(saved);
        }
        if let Some(top) = stack.last_mut() {
            *top = process;
        }
        (saved, was_empty)
    });
    let seq = NEXT_TOKEN.with(|c| {
        let v = c.get();
        c.set(v.wrapping_add(1));
        v
    });
    let token = IdentityToken {
        saved,
        seq,
        was_empty,
    };
    CLEAR_STACK.with(|s| s.borrow_mut().push(token));
    token
}

/// `Binder.restoreCallingIdentity(token)`: undo a [`clear_calling_identity`].
///
/// Returns [`ParcelError::IdentityLifo`] when tokens are restored out of order.
pub fn restore_calling_identity(token: IdentityToken) -> Result<()> {
    let top = CLEAR_STACK.with(|s| s.borrow().last().copied());
    if top != Some(token) {
        return Err(ParcelError::IdentityLifo);
    }
    CLEAR_STACK.with(|s| {
        s.borrow_mut().pop();
    });
    IDENTITY_STACK.with(|s| {
        let mut stack = s.borrow_mut();
        if token.was_empty {
            stack.pop();
        } else if let Some(top) = stack.last_mut() {
            *top = token.saved;
        }
    });
    Ok(())
}

/// Clear, run `f`, restore even on panic.
pub fn with_cleared_identity<R>(f: impl FnOnce() -> R) -> R {
    let token = clear_calling_identity();
    let result = f();
    let _ = restore_calling_identity(token);
    result
}

/// This thread's strict-mode policy word.
#[must_use]
pub fn thread_strict_mode_policy() -> i32 {
    STRICT_MODE.with(Cell::get)
}

/// Set this thread's strict-mode policy word.
pub fn set_thread_strict_mode_policy(policy: i32) {
    STRICT_MODE.with(|c| c.set(policy));
}

/// This thread's work-source uid (`-1` when none).
#[must_use]
pub fn thread_work_source_uid() -> i32 {
    WORK_SOURCE.with(Cell::get)
}

/// Set this thread's work-source uid.
pub fn set_thread_work_source_uid(uid: i32) {
    WORK_SOURCE.with(|c| c.set(uid));
}

/// Whether this thread propagates its work source into interface tokens.
#[must_use]
pub fn should_propagate_work_source() -> bool {
    PROPAGATE_WORK_SOURCE.with(Cell::get)
}

/// Set whether this thread propagates its work source.
pub fn set_propagate_work_source(propagate: bool) {
    PROPAGATE_WORK_SOURCE.with(|c| c.set(propagate));
}

// ----------------------------------------------------------------------
// BinderRegistry
// ----------------------------------------------------------------------

struct BinderRecord {
    descriptor: String,
    kind: BinderKind,
    death_recipients: HashSet<u64>,
}

struct RegistryInner {
    next_id: u64,
    objects: HashMap<u64, BinderRecord>,
    services: HashMap<String, u64>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Stable `u64` handles for local binder objects and proxies plus the ACOS
/// `IServiceManager` name table.
pub struct BinderRegistry {
    inner: Mutex<RegistryInner>,
}

impl Default for BinderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BinderRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RegistryInner {
                next_id: 1,
                objects: HashMap::new(),
                services: HashMap::new(),
            }),
        }
    }

    fn register(&self, descriptor: impl Into<String>, kind: BinderKind) -> BinderId {
        let mut inner = lock(&self.inner);
        let id = inner.next_id;
        inner.next_id = inner.next_id.wrapping_add(1).max(1);
        inner.objects.insert(
            id,
            BinderRecord {
                descriptor: descriptor.into(),
                kind,
                death_recipients: HashSet::new(),
            },
        );
        BinderId(id)
    }

    /// Register a local (`android.os.Binder`) object.
    pub fn register_local(&self, descriptor: impl Into<String>) -> BinderId {
        self.register(descriptor, BinderKind::Local)
    }

    /// Register a proxy (`android.os.BinderProxy`).
    pub fn register_proxy(&self, descriptor: impl Into<String>) -> BinderId {
        self.register(descriptor, BinderKind::Proxy)
    }

    /// The descriptor of a registered binder.
    #[must_use]
    pub fn descriptor(&self, id: BinderId) -> Option<String> {
        lock(&self.inner)
            .objects
            .get(&id.0)
            .map(|r| r.descriptor.clone())
    }

    /// The kind of a registered binder.
    #[must_use]
    pub fn kind(&self, id: BinderId) -> Option<BinderKind> {
        lock(&self.inner).objects.get(&id.0).map(|r| r.kind)
    }

    /// `true` if the handle is registered.
    #[must_use]
    pub fn contains(&self, id: BinderId) -> bool {
        lock(&self.inner).objects.contains_key(&id.0)
    }

    /// `isBinderAlive`: always `true` for a registered handle.
    #[must_use]
    pub fn is_binder_alive(&self, id: BinderId) -> bool {
        self.contains(id)
    }

    /// `pingBinder`: always `true` for a registered handle.
    #[must_use]
    pub fn ping_binder(&self, id: BinderId) -> bool {
        self.contains(id)
    }

    /// Forget a handle (used when a scenario removes a service).
    pub fn remove(&self, id: BinderId) -> bool {
        lock(&self.inner).objects.remove(&id.0).is_some()
    }

    /// `IServiceManager.addService`.
    ///
    /// The binder must already be registered; an unknown id is an error, never
    /// a silent success (ADR-0008).
    pub fn add_service(&self, name: impl Into<String>, id: BinderId) -> Result<()> {
        let mut inner = lock(&self.inner);
        if !inner.objects.contains_key(&id.0) {
            return Err(ParcelError::UnknownBinder(id.0));
        }
        inner.services.insert(name.into(), id.0);
        Ok(())
    }

    /// `IServiceManager.getService`: the binder for `name`, or `None`.
    #[must_use]
    pub fn get_service(&self, name: &str) -> Option<BinderId> {
        let inner = lock(&self.inner);
        inner
            .services
            .get(name)
            .copied()
            .filter(|id| inner.objects.contains_key(id))
            .map(BinderId)
    }

    /// `IServiceManager.checkService`: identical to [`Self::get_service`].
    #[must_use]
    pub fn check_service(&self, name: &str) -> Option<BinderId> {
        self.get_service(name)
    }

    /// `IServiceManager.listServices`.
    #[must_use]
    pub fn list_services(&self) -> Vec<String> {
        let mut v: Vec<String> = lock(&self.inner).services.keys().cloned().collect();
        v.sort();
        v
    }

    /// `linkToDeath`: record `recipient` and return `true`.
    ///
    /// ACOS binders never die, so `binderDied` is never fired. Unknown handles
    /// return `false`, matching a real link to a dead binder.
    pub fn link_to_death(&self, id: BinderId, recipient: u64) -> bool {
        let mut inner = lock(&self.inner);
        match inner.objects.get_mut(&id.0) {
            Some(record) => {
                record.death_recipients.insert(recipient);
                true
            }
            None => false,
        }
    }

    /// `unlinkToDeath`: `true` when the recipient was registered.
    pub fn unlink_to_death(&self, id: BinderId, recipient: u64) -> bool {
        let mut inner = lock(&self.inner);
        match inner.objects.get_mut(&id.0) {
            Some(record) => record.death_recipients.remove(&recipient),
            None => false,
        }
    }

    /// Registered death recipients for diagnostics/tests.
    #[must_use]
    pub fn death_recipients(&self, id: BinderId) -> Vec<u64> {
        let mut v: Vec<u64> = lock(&self.inner)
            .objects
            .get(&id.0)
            .map(|r| r.death_recipients.iter().copied().collect())
            .unwrap_or_default();
        v.sort_unstable();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_stack_is_lifo_and_thread_local() {
        // Main thread has no caller.
        assert_eq!(calling_identity(), process_identity());
        push_calling_identity(10192, 1234);
        assert_eq!(calling_uid(), 10192);
        assert_eq!(calling_pid(), 1234);
        push_calling_identity(1000, 999);
        assert_eq!(calling_uid(), 1000);
        assert_eq!(pop_calling_identity(), Some(CallingIdentity::new(1000, 999)));
        assert_eq!(calling_uid(), 10192);
        assert_eq!(pop_calling_identity(), Some(CallingIdentity::new(10192, 1234)));
        assert_eq!(calling_identity(), process_identity());
    }

    #[test]
    fn clear_restore_is_lifo() {
        set_process_identity(2000, 42);
        push_calling_identity(10192, 777);
        assert_eq!(calling_uid(), 10192);

        let t1 = clear_calling_identity();
        assert_eq!(calling_uid(), 2000);
        let t2 = clear_calling_identity();
        assert_eq!(calling_uid(), 2000);

        // Out-of-order restore must fail and change nothing.
        assert_eq!(restore_calling_identity(t1), Err(ParcelError::IdentityLifo));
        assert_eq!(calling_uid(), 2000);

        restore_calling_identity(t2).unwrap();
        assert_eq!(calling_uid(), 2000);
        restore_calling_identity(t1).unwrap();
        assert_eq!(calling_uid(), 10192);

        pop_calling_identity();
        set_process_identity(0, 0);
    }

    #[test]
    fn service_table_and_registry() {
        let reg = BinderRegistry::new();
        let id = reg.register_local("android.app.IActivityManager");
        assert_eq!(reg.descriptor(id).as_deref(), Some("android.app.IActivityManager"));
        assert!(reg.contains(id));
        assert!(reg.add_service("activity", id).is_ok());

        let found = reg.check_service("activity").unwrap();
        assert_eq!(reg.descriptor(found).as_deref(), Some("android.app.IActivityManager"));
        assert_eq!(reg.check_service("nope"), None);
        assert_eq!(reg.list_services(), vec!["activity".to_string()]);

        assert!(reg.add_service("ghost", BinderId(9999)).is_err());
    }

    #[test]
    fn death_recipients_never_fire_but_unlink_tracks() {
        let reg = BinderRegistry::new();
        let id = reg.register_proxy("android.view.IWindowSession");
        assert!(reg.link_to_death(id, 0xaaaa));
        assert!(reg.link_to_death(id, 0xbbbb));
        assert_eq!(reg.death_recipients(id), vec![0xaaaa, 0xbbbb]);
        assert!(reg.unlink_to_death(id, 0xaaaa));
        assert!(!reg.unlink_to_death(id, 0xaaaa));
        assert_eq!(reg.death_recipients(id), vec![0xbbbb]);
        assert!(reg.is_binder_alive(id));
        assert!(reg.ping_binder(id));
        assert!(!reg.link_to_death(BinderId(12345), 1));
    }

    #[test]
    fn two_threads_have_independent_identity_stacks() {
        use std::sync::mpsc;

        set_process_identity(2000, 42);
        push_calling_identity(10192, 1);
        assert_eq!(calling_uid(), 10192);

        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            // The fresh thread starts empty, not with the parent's frame.
            tx.send((calling_uid(), calling_pid())).unwrap();
            push_calling_identity(3000, 7);
            tx.send((calling_uid(), calling_pid())).unwrap();
        });
        handle.join().unwrap();
        // Parent is unaffected by the child.
        assert_eq!(calling_uid(), 10192);
        assert_eq!(rx.recv().unwrap(), (0, 0));
        assert_eq!(rx.recv().unwrap(), (3000, 7));
        pop_calling_identity();
        set_process_identity(0, 0);
    }
}
