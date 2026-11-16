//! Which network this session has — the one fact the framework and the
//! launcher both need.
//!
//! The `network` module ([`crate::modules::network`]) owns the real network
//! (ADR-0030). Three places have to agree with the module's attached profile and
//! none of them can see the module registry:
//!
//! * `ConnectivityManager`'s answer (`crate::connectivity`: capabilities,
//!   `LinkProperties`, the DHCP/DNS facts), which is a **free** parcelable
//!   builder with no host access;
//! * the launcher's sandbox (`acos-art`): whether `--unshare-net` is dropped and
//!   the guest is spawned inside the relay's session netns;
//! * the guest's seccomp program: whether `AF_INET`/`AF_INET6` are allowed.
//!
//! So the module publishes its profile here, at attach, before the guest's
//! command is built, and detach clears it. This is the seam
//! `crate::connectivity`'s old comment promised ("the product will ask the
//! module registry instead"); the env flag it used before
//! (`ACOS_SPIKE_NET`) is now only an override for the standalone `acos-art`
//! binary's own spikes and tests.
//!
//! # One process, one session
//!
//! A launcher process hosts exactly one guest, so one slot is enough and it is
//! what makes the seam usable from the parcelable builders that take no
//! arguments. `acos-services` is otherwise state-in-`ServiceHost`; a second
//! session in one process would have to move this into the host, and that is
//! noted in `docs/handoff/NET5` rather than pretended away.

use std::sync::atomic::{AtomicU8, Ordering};

use crate::module::Profile;

/// Environment override kept for the standalone `acos-art` binary (its own
/// boundary tests and the NET1–NET4 spike scripts). The product path is the
/// module: a session that attaches `network` needs no variable set.
pub const SPIKE_FLAG: &str = "ACOS_SPIKE_NET";

static PROFILE: AtomicU8 = AtomicU8::new(0);

fn encode(profile: Profile) -> u8 {
    match profile {
        Profile::Off => 0,
        Profile::Stub => 1,
        Profile::Scripted => 2,
        Profile::Real => 3,
    }
}

fn decode(value: u8) -> Profile {
    match value {
        1 => Profile::Stub,
        2 => Profile::Scripted,
        _ => Profile::Real,
    }
}

/// Publish the attached profile (called by the module's `attach`).
pub fn set_profile(profile: Profile) {
    PROFILE.store(encode(profile), Ordering::SeqCst);
}

/// Back to no network (called by the module's `detach`).
pub fn clear() {
    PROFILE.store(0, Ordering::SeqCst);
}

/// The profile the module attached, `Off` when it is not attached.
#[must_use]
pub fn profile() -> Profile {
    match PROFILE.load(Ordering::SeqCst) {
        0 => Profile::Off,
        other => decode(other),
    }
}

/// True when the session really has a network interface and traffic.
///
/// `real` is the only profile that does: `stub`/`scripted` are not implemented
/// for this module (there is no scripted network: the real arm already has
/// deterministic online/offline/latency/loss controls, and a fake interface
/// would be a lie about the app's actual connectivity).
#[must_use]
pub fn networked() -> bool {
    profile() == Profile::Real
}

/// The spike override (`ACOS_SPIKE_NET=1`): only the standalone launcher honours
/// it, and only when no module published a profile.
#[must_use]
pub fn spike_override() -> bool {
    matches!(std::env::var(SPIKE_FLAG).as_deref(), Ok("1") | Ok("true"))
}

/// The one gate every consumer asks: does this session have a real network?
#[must_use]
pub fn session_has_network() -> bool {
    networked() || spike_override()
}

/// The relay session name for a session id: `acos-<id>`.
///
/// One rule for every path (NET4): the launcher names the relay session with it,
/// and the module's control ops resolve the same name from the id the caller
/// holds.
#[must_use]
pub fn relay_session_name(session_id: &str) -> String {
    format!("acos-{session_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slot is process-wide, so the tests that touch it run in one thread and
    /// restore `off` afterwards. `ACOS_SPIKE_NET` is not set by the test harness.
    #[test]
    fn the_module_profile_is_the_gate_and_off_is_the_default() {
        clear();
        assert_eq!(profile(), Profile::Off);
        assert!(!networked());
        assert!(!session_has_network());

        set_profile(Profile::Real);
        assert_eq!(profile(), Profile::Real);
        assert!(networked());
        assert!(session_has_network());
        assert_eq!(relay_session_name("verify-7"), "acos-verify-7");

        // A profile the module does not implement is still *not* a network: the
        // gate asks for `real`, so a stray `stub` can never fake one.
        set_profile(Profile::Stub);
        assert!(!networked());

        clear();
        assert_eq!(profile(), Profile::Off);
    }
}
