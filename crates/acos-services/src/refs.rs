//! Host-held strong references on guest binders (the registration ledger).
//!
//! On Android, storing a callback binder in a system service *is* taking a
//! strong reference on it: `Parcel::readStrongBinder`/`writeStrongBinder`
//! hand the receiver an `sp<IBinder>`, `Binder` refcounting keeps the
//! `JavaBBinder` (and through its global reference the Java object) alive, and
//! the reference is dropped when the service removes the registration.
//! `BpBinder`'s destructor then sends `BC_RELEASE`/`BC_DECREFS`, so the driver
//! unroots and frees the node.
//!
//! ACOS's guest and the host are two processes with the binder driver in
//! between; the driver only unroots a node when the *host* says so
//! (`F_REF` + `REF_RELEASE`, `results/handoff/BINDERREF`). So every module that
//! stores a callback binder has to report two things:
//!
//! * "I now hold this binder" — the host keeps the guest's node alive;
//! * "I dropped it" — the host releases the node (`BR_RELEASE` + `BR_DECREFS`).
//!
//! [`GuestRefs`] is that bookkeeping, once, for every module. It is a *set*,
//! not a counter: a registration is a reference, and re-registering the same
//! binder (which Android's services do — `listen()` twice with the same
//! listener) does not take a second one. That keeps the release path correct
//! no matter in which order a handler reports, and it is what
//! [`GuestRefs::release_all`] needs to be able to unroot everything a module
//! owns when the module detaches.
//!
//! Usage, in a `ServiceObject` that stores a callback binder:
//!
//! ```ignore
//! state.refs.hold(listener);          // registerLocationListener
//! state.refs.release(listener);       // unregisterLocationListener
//! ...
//! fn drain_retained(&mut self) -> Vec<BinderId> { self.state.lock().unwrap().refs.drain_retained() }
//! fn drain_released(&mut self) -> Vec<BinderId> { self.state.lock().unwrap().refs.drain_released() }
//! fn release_all_refs(&mut self) -> Vec<BinderId> { self.state.lock().unwrap().refs.release_all() }
//! ```
//!
//! The host applies `drain_retained` before `drain_released` in the same turn,
//! so a registration that replaces the same binder in one transaction
//! (`listen(events)` after `listen(NONE)`) nets out correctly.

use acos_parcel::BinderId;

/// The set of guest binders one service holds strong references on.
#[derive(Debug, Default, Clone)]
pub struct GuestRefs {
    /// Currently held, in the order they were first taken.
    held: Vec<BinderId>,
    /// References taken since the last drain (what the host must retain).
    retained: Vec<BinderId>,
    /// References dropped since the last drain (what the host must release).
    released: Vec<BinderId>,
}

impl GuestRefs {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a strong reference on `id`. Returns `false` when it was already
    /// held (a re-registration is not a second reference).
    pub fn hold(&mut self, id: BinderId) -> bool {
        if self.held.contains(&id) {
            return false;
        }
        self.held.push(id);
        self.retained.push(id);
        true
    }

    /// Drop the reference on `id`. Returns `false` when this ledger did not hold
    /// one (an unregister for a registration that was never made: never a
    /// release the driver would have to undo).
    pub fn release(&mut self, id: BinderId) -> bool {
        let Some(index) = self.held.iter().position(|held| *held == id) else {
            return false;
        };
        self.held.remove(index);
        self.released.push(id);
        true
    }

    /// `true` when a registration on `id` is currently held.
    #[must_use]
    pub fn holds(&self, id: BinderId) -> bool {
        self.held.contains(&id)
    }

    /// The binders currently held.
    #[must_use]
    pub fn held(&self) -> &[BinderId] {
        &self.held
    }

    /// How many references are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// `true` when nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Drop every reference (module detach, peer disconnect, process death) and
    /// return what was dropped, so the caller can release it on the host.
    pub fn release_all(&mut self) -> Vec<BinderId> {
        let dropped = std::mem::take(&mut self.held);
        for id in &dropped {
            if !self.released.contains(id) {
                self.released.push(*id);
            }
        }
        dropped
    }

    /// References taken since the last drain.
    pub fn drain_retained(&mut self) -> Vec<BinderId> {
        std::mem::take(&mut self.retained)
    }

    /// References dropped since the last drain.
    pub fn drain_released(&mut self) -> Vec<BinderId> {
        std::mem::take(&mut self.released)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> BinderId {
        BinderId(n)
    }

    #[test]
    fn a_registration_is_one_reference_and_a_repeat_registration_is_not_two() {
        let mut refs = GuestRefs::new();
        assert!(refs.hold(id(7)));
        assert!(!refs.hold(id(7)), "the same binder registered twice");
        assert_eq!(refs.drain_retained(), vec![id(7)]);
        assert!(refs.hold(id(8)));
        assert_eq!(refs.drain_retained(), vec![id(8)], "only the new one");
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn releasing_a_registration_that_was_never_made_is_not_reported() {
        let mut refs = GuestRefs::new();
        refs.hold(id(1));
        let _ = refs.drain_retained();
        assert!(!refs.release(id(9)), "not held");
        assert!(
            refs.drain_released().is_empty(),
            "the driver must never be told to release a node it did not hand out"
        );
        assert!(refs.release(id(1)));
        assert_eq!(refs.drain_released(), vec![id(1)]);
        assert!(refs.is_empty());
    }

    #[test]
    fn detach_releases_everything_the_handler_still_held() {
        let mut refs = GuestRefs::new();
        refs.hold(id(1));
        refs.hold(id(2));
        let _ = refs.drain_retained();
        assert_eq!(refs.release_all(), vec![id(1), id(2)]);
        let released = refs.drain_released();
        assert_eq!(released, vec![id(1), id(2)]);
        assert!(refs.is_empty(), "a second release_all is a no-op");
        assert!(refs.release_all().is_empty());
    }

    #[test]
    fn release_all_does_not_report_a_reference_twice() {
        let mut refs = GuestRefs::new();
        refs.hold(id(3));
        let _ = refs.drain_retained();
        assert!(refs.release(id(3)));
        assert_eq!(refs.release_all(), Vec::new());
        assert_eq!(
            refs.drain_released(),
            vec![id(3)],
            "released once, not twice"
        );
    }
}
