//! ZPROD3b: the demand-probe channel of a zygote child.
//!
//! The exec path's `PROBE` wake goes to the guest's own stdin. A zygote child
//! cannot use that fd: the keeper is spawned with `Stdio::null()`
//! (`zygote_prod::PersistentZygote::start`), so the forked app's fd 0 is
//! `/dev/null` while the session launcher's stub owns the pipe. On real ART
//! that produced a post-tap capture that never settled
//! (`capture: strict settle timed out (probe-floor)`, the newest event-driven
//! sample staying 1.6 s old) even though the tap itself passed.
//!
//! These tests prove the replacement channel and its contract with the guest:
//!
//! * every `PROBE` written through `GuestProbeChannel::Fifo` is readable from
//!   the child's side, in order, with no loss;
//! * the FIFO is private (`0600`) and opened read-write by the host, so a
//!   reader that goes away cannot turn the next wake into `EPIPE`/EOF;
//! * the guest source opens `<sessions>/<tag>/probe.fifo` and sets it as
//!   `System.in` **before** `spawnSession`, i.e. before any namespace,
//!   seccomp or Landlock rule can deny the path, and the name matches the
//!   host's `GUEST_PROBE_FIFO`.

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;
use std::time::Duration;

/// The guest source that is compiled into the zygote build
/// (`research/zygote-proto/z3/build.sh`); read as text so the two halves of
/// the contract cannot drift silently.
const ZYGOTE_GUEST_SRC: &str =
    include_str!("../../../research/zygote-proto/z3/java/dev/acos/art/ZygoteApp.java");

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acos-zprod3b-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The host writer can be created before any reader exists, and every wake is
/// delivered to the reader the guest would open — in order, nothing dropped.
#[test]
fn every_probe_wake_reaches_the_zygote_child_side_of_the_fifo() {
    let dir = scratch_dir("deliver");
    let mut channel = Some(GuestProbeChannel::fifo(
        open_guest_probe_fifo(&dir).expect("create the session probe FIFO"),
    ));
    let path = dir.join(GUEST_PROBE_FIFO);
    assert_eq!(GUEST_PROBE_FIFO, "probe.fifo");
    let mode = std::fs::metadata(&path)
        .expect("fifo metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "the probe FIFO stays private: {mode:o}"
    );

    let (tx, rx) = mpsc::channel();
    let reader_path = path.clone();
    let reader = std::thread::spawn(move || {
        // This is the child's open: blocking is fine (the host holds the FIFO
        // read-write), and it must yield exactly the wake lines.
        let file = std::fs::File::open(&reader_path).expect("child-side open");
        let mut lines = Vec::new();
        for line in BufReader::new(file).lines() {
            match line {
                Ok(line) => lines.push(line),
                Err(_) => break,
            }
            if lines.len() == 3 {
                break;
            }
        }
        let _ = tx.send(lines);
    });

    // Two wakes at settle entry and one during the wait: all three must arrive.
    let state = std::sync::Arc::new(std::sync::Mutex::new(ProbeState::default()));
    request_guest_probe(&mut channel, &state);
    request_guest_probe(&mut channel, &state);
    std::thread::sleep(Duration::from_millis(20));
    request_guest_probe(&mut channel, &state);
    assert_eq!(
        state.lock().unwrap().wakes_sent,
        3,
        "every wake that reached the FIFO is counted"
    );

    let seen = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the child side received the wakes");
    assert_eq!(seen, vec!["PROBE", "PROBE", "PROBE"]);
    reader.join().expect("reader thread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The host side is read-write: when the child's reader is gone (a reset, or a
/// child that died mid-settle) a wake is still written without error, so a
/// demand probe can never poison the launcher's command loop.
#[test]
fn a_vanished_reader_cannot_break_the_next_wake() {
    let dir = scratch_dir("reader-gone");
    let mut channel = Some(GuestProbeChannel::fifo(
        open_guest_probe_fifo(&dir).expect("create the session probe FIFO"),
    ));
    let path = dir.join(GUEST_PROBE_FIFO);
    {
        let file = std::fs::File::open(&path).expect("reader opens");
        drop(file);
    }
    // Writes must still succeed (this process is also a reader, so no EPIPE).
    let writer = channel.as_mut().expect("expected the FIFO channel");
    assert!(writeln!(writer, "PROBE").is_ok());
    assert!(writer.flush().is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The guest half of the contract, checked on the source that is compiled into
/// the zygote build: the FIFO is opened and installed as stdin in the child's
/// pre-sandbox window (before `spawnSession`), so it needs no new Landlock
/// grant, and the name is the one the host creates.
#[test]
fn the_forked_child_installs_the_probe_fifo_as_stdin_before_its_sandbox() {
    assert!(
        ZYGOTE_GUEST_SRC.contains("\"/\" + tag + \"/probe.fifo\""),
        "the guest opens the session's probe.fifo"
    );
    assert!(
        ZYGOTE_GUEST_SRC.contains("System.setIn(in)"),
        "the guest makes the FIFO its stdin, which the launch guest's PROBE reader uses"
    );
    let fork_body = ZYGOTE_GUEST_SRC
        .split("private static int z4ForkOne")
        .nth(1)
        .expect("z4ForkOne exists");
    let probe_at = fork_body
        .find("z4ProbeStdin(sessions, tag);")
        .expect("the persistent fork path installs the probe channel");
    let sandbox_at = fork_body
        .find("spawnSession(tag)")
        .expect("the persistent fork path installs its sandbox");
    assert!(
        probe_at < sandbox_at,
        "the probe FIFO is opened before the mount/seccomp chain"
    );
    // The non-persistent Z4 cohort takes the same channel.
    let cohort = ZYGOTE_GUEST_SRC
        .split("private static void z4Main")
        .nth(1)
        .expect("z4Main exists");
    assert!(
        cohort.contains("z4ProbeStdin(sessions, k);"),
        "the Z4 cohort path installs the probe channel too"
    );
}
