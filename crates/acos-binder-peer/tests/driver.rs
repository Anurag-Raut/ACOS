//! Driver-level acceptance test for the userspace binder driver: no ART, no
//! libbinder, no guest process.
//!
//! `native/driver_test.c` is one translation unit with `native/binder_driver.c`
//! and drives it through the same entries libbinder uses (`open("/dev/binder")`,
//! `ioctl(BINDER_WRITE_READ)` with `BC_*`/`BR_*` commands) against a scripted
//! peer on a unix socket. This test compiles it for the host and asserts its
//! TAP-style output, so the delivery rules are covered by
//! `cargo test -p acos-binder-peer` without an ART run.
//!
//! The checks (see the C file for the full list) are behaviours, not internals:
//! a node exported by the guest can be reached after the parcel that exposed it
//! is freed, many callbacks in one turn all arrive, and a transaction to a node
//! that is gone answers `DEAD_OBJECT` instead of being dropped silently.

use std::path::{Path, PathBuf};
use std::process::Command;

fn native_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("native")
}

/// A host C compiler: `$CC`, then `cc`, then `gcc`/`clang`.
fn compiler() -> Option<String> {
    if let Ok(explicit) = std::env::var("CC") {
        if !explicit.trim().is_empty() {
            return Some(explicit);
        }
    }
    for candidate in ["cc", "gcc", "clang"] {
        if Command::new(candidate)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(candidate.to_string());
        }
    }
    None
}

#[test]
fn driver_delivers_callbacks_and_answers_dead_nodes() {
    let Some(cc) = compiler() else {
        // A missing host C compiler is a harness limitation, never a pass: this
        // test exists to prove behaviour, so it fails and says what to install.
        panic!("driver test needs a host C compiler (set CC or install cc/gcc/clang)");
    };
    let dir = native_dir();
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("acos_binder_driver_test");
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("create tmp dir");
    }
    let build = Command::new(&cc)
        .args(["-O1", "-g", "-pthread", "-DACOS_DRIVER_HOST_TEST", "-o"])
        .arg(&out)
        .arg(dir.join("driver_test.c"))
        .output()
        .expect("run the C compiler");
    assert!(
        build.status.success(),
        "compiling native/driver_test.c failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("run the driver test binary");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    let failures: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("not ok"))
        .collect();
    assert!(
        failures.is_empty() && run.status.success(),
        "driver test failed ({} check(s)):\n{}\n--- driver diagnostics ---\n{}",
        failures.len(),
        stdout,
        stderr
    );
    // Guard against a harness that reports success without running its checks.
    let checks = stdout.lines().filter(|line| line.starts_with("ok ")).count();
    assert!(
        checks >= 8,
        "driver test ran {checks} checks, expected at least 8:\n{stdout}"
    );
    assert!(
        stdout.lines().any(|line| line == "RESULT failures=0"),
        "driver test did not report RESULT failures=0:\n{stdout}"
    );
}
