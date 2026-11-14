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

/// Block SIGTERM/SIGINT and run `teardown` on the first one, then exit.
///
/// Must be called from the main thread **before** other threads are created, so
/// the mask is inherited (the confined child never calls it: it has no sessions).
pub fn install_teardown<F>(teardown: F) -> io::Result<()>
where
    F: Fn() + Send + Sync + 'static,
{
    block_termination()?;
    std::thread::Builder::new()
        .name("acos-mcp-signals".to_string())
        .spawn(move || {
            let mut sig = 0;
            // A spurious failure (`EINTR`) just waits again; a real signal hands
            // us its number and we stop.
            loop {
                // SAFETY: `sigset_t` is a plain struct; both calls take a pointer
                // to a value this thread owns and pass no other pointers.
                unsafe {
                    let mut set: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut set);
                    libc::sigaddset(&mut set, libc::SIGTERM);
                    libc::sigaddset(&mut set, libc::SIGINT);
                    if libc::sigwait(&set, &mut sig) == 0 {
                        break;
                    }
                }
            }
            eprintln!("acos-mcp: signal {sig} received; running the stdin-EOF teardown");
            teardown();
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
}
