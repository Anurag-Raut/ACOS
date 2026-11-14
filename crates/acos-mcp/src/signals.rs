//! SIGTERM/SIGINT: the same teardown as stdin EOF (FB5).
//!
//! A stdio MCP client's normal exit closes stdin, and [`crate::Server`] runs the
//! backend teardown there (SERVELEAK). A client that is *killed*, or a supervisor
//! that stops the server with `SIGTERM`, never closes a pipe: without a handler
//! the process dies and leaves its ART guests (and its private daemon) behind
//! until the daemon's own owner check notices.
//!
//! The handler is a dedicated `sigwait` thread, not a signal handler:
//!
//! * `SIGTERM` and `SIGINT` are **blocked** in `main` before any other thread
//!   exists, so every thread inherits the mask and only `sigwait` consumes the
//!   signals (no async-signal-safety limits on the teardown code);
//! * the children that must receive their own `SIGTERM` (the private `acos
//!   serve`) unblock it in `pre_exec` (see `private_daemon`), because a blocked
//!   mask survives `exec`.
//!
//! The teardown is the backend's [`crate::Backend::shutdown`], identical to the
//! stdin-EOF path, so a signalled server destroys exactly the sessions it
//! launched and nothing else.

use std::io;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a *second* SIGTERM/SIGINT waits for the running teardown before the
/// process exits anyway (FB5b/L3).
pub const SECOND_SIGNAL_GRACE: Duration = Duration::from_secs(3);
/// How long `main` waits for a signal-started teardown after stdin EOF.
pub const MAIN_JOIN_BOUND: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Idle,
    Running,
    Done,
}

/// Shared teardown progress (FB5b/L3): the signal thread starts it and finishes
/// it; `main` waits on it so a SIGTERM that races stdin EOF is never cut short
/// by `main` returning (which would end the process mid-teardown).
#[derive(Debug)]
struct TeardownState {
    phase: Mutex<Phase>,
    changed: Condvar,
}

impl TeardownState {
    fn new() -> Self {
        TeardownState {
            phase: Mutex::new(Phase::Idle),
            changed: Condvar::new(),
        }
    }

    fn set(&self, phase: Phase) {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner()) = phase;
        self.changed.notify_all();
    }

    /// Wait until no teardown is running; `true` when it finished (or never
    /// started), `false` when `bound` elapsed first.
    fn wait_finished(&self, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        let mut phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        while *phase == Phase::Running {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (next, _) = self
                .changed
                .wait_timeout(phase, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            phase = next;
        }
        true
    }
}

fn state() -> &'static Arc<TeardownState> {
    static STATE: OnceLock<Arc<TeardownState>> = OnceLock::new();
    STATE.get_or_init(|| Arc::new(TeardownState::new()))
}

/// Called by `main` after the stdin-EOF path: if a signal started the teardown,
/// wait (bounded) for it to finish before the process returns.
pub fn join_teardown() {
    if !state().wait_finished(MAIN_JOIN_BOUND) {
        eprintln!("acos-mcp: signal teardown still running after {MAIN_JOIN_BOUND:?}; exiting");
    }
}

/// Wait for one of SIGTERM/SIGINT (blocked in this process).
fn wait_signal() -> i32 {
    let mut sig = 0;
    loop {
        // SAFETY: `sigset_t` is a plain struct; both calls take a pointer to a
        // value this thread owns and pass no other pointers.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGINT);
            if libc::sigwait(&set, &mut sig) == 0 {
                return sig;
            }
        }
    }
}

/// Block SIGTERM/SIGINT and run `teardown` on the first one, then exit.
///
/// Must be called from the main thread **before** other threads are created, so
/// the mask is inherited (the confined child never calls it: it has no sessions).
///
/// FB5b/L3: a second signal while the teardown runs does not queue silently — a
/// watcher thread waits [`SECOND_SIGNAL_GRACE`] for the teardown and then exits
/// the process (status 1); `main` joins a running teardown via [`join_teardown`].
pub fn install_teardown<F>(teardown: F) -> io::Result<()>
where
    F: Fn() + Send + Sync + 'static,
{
    block_termination()?;
    let state = Arc::clone(state());
    std::thread::Builder::new()
        .name("acos-mcp-signals".to_string())
        .spawn(move || {
            let sig = wait_signal();
            state.set(Phase::Running);
            eprintln!("acos-mcp: signal {sig} received; running the stdin-EOF teardown");
            let watcher = Arc::clone(&state);
            let _ = std::thread::Builder::new()
                .name("acos-mcp-signals-2".to_string())
                .spawn(move || {
                    let again = wait_signal();
                    eprintln!(
                        "acos-mcp: signal {again} during teardown; waiting up to {:?} for it",
                        SECOND_SIGNAL_GRACE
                    );
                    watcher.wait_finished(SECOND_SIGNAL_GRACE);
                    std::process::exit(1);
                });
            teardown();
            state.set(Phase::Done);
            std::process::exit(0);
        })?;
    Ok(())
}

/// Block SIGTERM/SIGINT for this process (inherited by every later thread).
fn block_termination() -> io::Result<()> {
    // SAFETY: as above; the mask is a local value and `null_mut()` means "do not
    // report the previous mask".
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mask is process/thread wide, so this test asserts the *blocking*
    /// helper, not the handler thread (a unit test must not exit the runner).
    #[test]
    fn termination_signals_are_blocked_after_install_block() {
        block_termination().expect("block");
        // SAFETY: `pthread_sigmask` with a local set; `SIG_BLOCK` returns the
        // previous mask, which is what we inspect.
        unsafe {
            let mut current: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut current);
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGINT);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut current),
                0
            );
            assert_eq!(libc::sigismember(&current, libc::SIGTERM), 1);
            assert_eq!(libc::sigismember(&current, libc::SIGINT), 1);
        }
    }

    #[test]
    fn main_waits_for_a_running_teardown_and_not_for_an_idle_one() {
        let state = Arc::new(TeardownState::new());
        // Never started: nothing to wait for.
        assert!(state.wait_finished(Duration::from_millis(10)));

        state.set(Phase::Running);
        let worker = Arc::clone(&state);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            worker.set(Phase::Done);
        });
        let started = Instant::now();
        assert!(state.wait_finished(Duration::from_secs(5)), "joined the teardown");
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "waited for the teardown, not just returned"
        );
        handle.join().expect("worker");
    }

    #[test]
    fn a_second_signal_wait_is_bounded_when_the_teardown_hangs() {
        let state = TeardownState::new();
        state.set(Phase::Running);
        let started = Instant::now();
        assert!(!state.wait_finished(Duration::from_millis(120)), "bound elapsed");
        assert!(started.elapsed() < Duration::from_secs(2), "exits promptly");
    }
}
