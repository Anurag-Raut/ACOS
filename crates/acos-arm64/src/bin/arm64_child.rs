//! `arm64_child` — the confined qemu-aarch64 launcher (W191-A).
//!
//! It is deliberately minimal and runs as its own process so that [`confine`]
//! happens in a fresh, single-threaded image. It applies every confinement
//! layer except seccomp (`SeccompMode::DeferredPreload`), then `execve`s
//! `qemu-aarch64` with `LD_PRELOAD=<guard>` and
//! `ACOS_GUARD_STATUS_FD=<n>`; the preloaded guard installs the audited filter
//! after the exec and reports over the status pipe.
//!
//! ```text
//! arm64_child --guard PATH --sysroot DIR --rw DIR [--ro DIR]... \
//!             --status-fd N -- QEMU [QEMU-ARGS...] GUEST [GUEST-ARGS...]
//! ```
//!
//! Exit codes: `0` only if the exec failed is impossible (exec replaces the
//! image); any other value means confinement or exec failed, and the parent
//! sees the status pipe close without an `OK` line — it never treats such a
//! child as confined.
#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use acos_sandbox::{confine, ConfineProfile, SeccompMode};

const EXIT_USAGE: i32 = 2;
const EXIT_CONFINE_FAILED: i32 = 20;
const EXIT_EXEC_FAILED: i32 = 21;

fn main() {
    std::process::exit(run());
}

struct Args {
    guard: PathBuf,
    sysroot: PathBuf,
    rw: PathBuf,
    ro: Vec<PathBuf>,
    status_fd: i32,
    command: Vec<OsString>,
}

fn parse(argv: Vec<OsString>) -> Result<Args, String> {
    let mut guard = None;
    let mut sysroot = None;
    let mut rw = None;
    let mut ro = Vec::new();
    let mut status_fd = 3;
    let mut i = 1usize;
    while i < argv.len() {
        let arg = argv[i].to_string_lossy();
        match arg.as_ref() {
            "--" => {
                let command = argv[i + 1..].to_vec();
                if command.is_empty() {
                    return Err("expected QEMU after --".to_string());
                }
                return Ok(Args {
                    guard: guard.ok_or("--guard is required")?,
                    sysroot: sysroot.ok_or("--sysroot is required")?,
                    rw: rw.ok_or("--rw is required")?,
                    ro,
                    status_fd,
                    command,
                });
            }
            "--guard" | "--sysroot" | "--rw" | "--ro" | "--status-fd" => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| format!("{arg} needs a value"))?;
                i += 2;
                match arg.as_ref() {
                    "--guard" => guard = Some(PathBuf::from(value)),
                    "--sysroot" => sysroot = Some(PathBuf::from(value)),
                    "--rw" => rw = Some(PathBuf::from(value)),
                    "--ro" => ro.push(PathBuf::from(value)),
                    "--status-fd" => {
                        status_fd = value
                            .to_string_lossy()
                            .parse()
                            .map_err(|_| "--status-fd must be an integer".to_string())?
                    }
                    _ => unreachable!(),
                }
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Err("expected -- QEMU [ARGS...]".to_string())
}

fn run() -> i32 {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let args = match parse(argv) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("arm64_child: {message}");
            return EXIT_USAGE;
        }
    };

    let mut read_only_paths = vec![args.sysroot.clone()];
    read_only_paths.extend(args.ro.iter().cloned());
    // The guest's bionic allocator (scudo) reserves a large virtual region at
    // startup, and qemu-user TCG maps its own translation buffers; the default
    // 4 GiB RLIMIT_AS is too small for a real dynamic bionic process. Keep a
    // bounded but generous 32 GiB address space (the wrapper's cgroup
    // MemoryMax bounds resident memory independently).
    let rlimits = acos_sandbox::RlimitProfile {
        address_space_bytes: Some(32 * 1024 * 1024 * 1024),
        ..acos_sandbox::RlimitProfile::default()
    };
    let profile = ConfineProfile {
        read_only_paths,
        read_write_paths: vec![args.rw.clone()],
        // Seccomp is installed after the exec, by the preloaded guard.
        seccomp: SeccompMode::DeferredPreload(args.guard.clone()),
        keep_fds: vec![0, 1, 2, args.status_fd],
        rlimits,
        ..ConfineProfile::default()
    };

    if let Err(e) = confine(&profile) {
        eprintln!("arm64_child: confinement refused: {e}");
        return EXIT_CONFINE_FAILED;
    }

    let (qemu, qemu_args) = args.command.split_first().expect("parse ensured non-empty");
    let mut command = Command::new(qemu);
    command.args(qemu_args);
    // These are set *after* the sanitising env step of `confine`, so they
    // survive into qemu. `LD_PRELOAD` names the guard; the guard reads the
    // status fd to prove it installed the filter.
    command.env("LD_PRELOAD", &args.guard);
    command.env("ACOS_GUARD_STATUS_FD", args.status_fd.to_string());

    // `exec` only returns on failure.
    let err = command.exec();
    eprintln!(
        "arm64_child: exec {} failed: {err}",
        qemu.to_string_lossy()
    );
    EXIT_EXEC_FAILED
}
