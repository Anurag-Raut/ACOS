//! Child-process execution with wall-time, output-size and cancellation limits.
//!
//! The backend never invokes a shell: it always spawns a program with an explicit
//! argv. This module owns the bounded, cancellable run used for every `acos`
//! subprocess. A cancelled or timed-out child is killed, never leaked.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A cloneable cancellation flag shared between the dispatcher and a call.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, uncancelled token.
    pub fn new() -> Self {
        CancelToken(Arc::new(AtomicBool::new(false)))
    }

    /// Request cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Everything needed to run one bounded child process.
pub struct ChildSpec<'a> {
    /// Program to execute (no shell).
    pub program: &'a Path,
    /// Explicit argv (after argv[0]).
    pub args: &'a [String],
    /// Extra environment variables.
    pub env: &'a [(String, String)],
    /// Hard wall-clock deadline.
    pub deadline: Instant,
    /// Cancellation flag polled while the child runs.
    pub cancel: &'a CancelToken,
    /// Maximum captured bytes for stdout and for stderr separately.
    pub max_output_bytes: usize,
}

/// Captured output of a completed child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// Exit status code, when the process exited normally.
    pub status: Option<i32>,
    /// Captured stdout bytes (possibly truncated only on error).
    pub stdout: Vec<u8>,
    /// Captured stderr bytes (possibly truncated only on error).
    pub stderr: Vec<u8>,
    /// Wall-clock duration in milliseconds.
    pub wall_ms: u64,
}

/// Why a bounded child run did not produce output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The program could not be spawned.
    Spawn(String),
    /// An I/O error occurred while waiting.
    Io(String),
    /// The wall-clock deadline passed; the child was killed.
    Timeout,
    /// The caller cancelled; the child was killed.
    Cancelled,
    /// stdout/stderr exceeded the configured cap; the child was killed.
    OutputLimit,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn(m) => write!(f, "failed to spawn child: {m}"),
            RunError::Io(m) => write!(f, "child I/O error: {m}"),
            RunError::Timeout => write!(f, "child exceeded its wall-clock budget"),
            RunError::Cancelled => write!(f, "child was cancelled"),
            RunError::OutputLimit => write!(f, "child output exceeded the configured limit"),
        }
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(2);
const READ_CHUNK: usize = 64 * 1024;

/// Run a child with a hard deadline, output cap and cancellation.
pub fn run(spec: &ChildSpec<'_>) -> Result<Captured, RunError> {
    let started = Instant::now();
    let mut command = Command::new(spec.program);
    command
        .args(spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Put the child in its own process group so a timeout/cancel can kill the
    // whole tree, including any grandchildren it spawns.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|e| RunError::Spawn(format!("{}: {e}", spec.program.display())))?;

    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RunError::Io("missing stdout pipe".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| RunError::Io("missing stderr pipe".to_string()))?;
    let out_handle = spawn_reader(stdout, spec.max_output_bytes, Arc::clone(&overflow));
    let err_handle = spawn_reader(stderr, spec.max_output_bytes, Arc::clone(&overflow));

    let mut failure: Option<RunError> = None;
    let status = loop {
        if spec.cancel.is_cancelled() {
            failure = Some(RunError::Cancelled);
            break None;
        }
        if overflow.load(Ordering::SeqCst) {
            failure = Some(RunError::OutputLimit);
            break None;
        }
        if Instant::now() >= spec.deadline {
            failure = Some(RunError::Timeout);
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(e) => {
                failure = Some(RunError::Io(e.to_string()));
                break None;
            }
        }
    };

    if failure.is_some() {
        kill_tree(&mut child);
    }
    let _ = child.wait();
    let stdout = join_reader(out_handle);
    let stderr = join_reader(err_handle);

    if let Some(err) = failure {
        return Err(err);
    }

    Ok(Captured {
        status,
        stdout,
        stderr,
        wall_ms: started.elapsed().as_millis() as u64,
    })
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    cap: usize,
    overflow: Arc<AtomicBool>,
) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut stored: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if stored.len() + n > cap {
                        let room = cap.saturating_sub(stored.len());
                        stored.extend_from_slice(&buf[..room]);
                        overflow.store(true, Ordering::SeqCst);
                        // Keep draining so a killed child does not block on a
                        // full pipe, but stop storing.
                        let mut sink = vec![0u8; READ_CHUNK];
                        while let Ok(n) = reader.read(&mut sink) {
                            if n == 0 {
                                break;
                            }
                        }
                        break;
                    }
                    stored.extend_from_slice(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        stored
    })
}

fn join_reader(handle: JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

/// Kill the child and everything in its process group.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        if pid > 0 {
            // SAFETY: `kill(2)` with a negative pid signals the process group
            // created by `process_group(0)`. It touches no Rust memory and its
            // arguments are a valid pid and signal number.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout() {
        let cancel = CancelToken::new();
        let spec = ChildSpec {
            program: Path::new("/bin/echo"),
            args: &["hello".to_string()],
            env: &[],
            deadline: Instant::now() + Duration::from_secs(5),
            cancel: &cancel,
            max_output_bytes: 4096,
        };
        let out = run(&spec).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }

    #[test]
    fn timeout_kills_child() {
        let cancel = CancelToken::new();
        let spec = ChildSpec {
            program: Path::new("/bin/sleep"),
            args: &["30".to_string()],
            env: &[],
            deadline: Instant::now() + Duration::from_millis(50),
            cancel: &cancel,
            max_output_bytes: 4096,
        };
        assert_eq!(run(&spec), Err(RunError::Timeout));
    }

    #[test]
    fn cancellation_kills_child() {
        let cancel = CancelToken::new();
        let trigger = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            trigger.cancel();
        });
        let spec = ChildSpec {
            program: Path::new("/bin/sleep"),
            args: &["30".to_string()],
            env: &[],
            deadline: Instant::now() + Duration::from_secs(30),
            cancel: &cancel,
            max_output_bytes: 4096,
        };
        assert_eq!(run(&spec), Err(RunError::Cancelled));
    }

    #[test]
    fn output_limit_is_enforced() {
        let cancel = CancelToken::new();
        let spec = ChildSpec {
            program: Path::new("/bin/echo"),
            args: &["a-long-enough-line-to-exceed-one-byte".to_string()],
            env: &[],
            deadline: Instant::now() + Duration::from_secs(5),
            cancel: &cancel,
            max_output_bytes: 1,
        };
        assert_eq!(run(&spec), Err(RunError::OutputLimit));
    }
}
