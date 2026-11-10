//! End-to-end confinement harness.
//!
//! Runs the `sandbox_child` bin as a separate process. With confinement on, a
//! set of hostile actions must fail while normal runtime actions must succeed;
//! with confinement off, the same hostile actions must succeed, proving the
//! sandbox (not the environment) is what denies them.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const OK: i32 = 0;
const DENIED: i32 = 10;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn child_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sandbox_child"))
}

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "acos-sandbox-test-{}-{}",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("ro")).expect("create ro");
        std::fs::create_dir_all(root.join("rw")).expect("create rw");
        std::fs::create_dir_all(root.join("outside")).expect("create outside");
        std::fs::write(root.join("ro/hello.txt"), b"hello\n").expect("seed ro");
        std::fs::write(root.join("outside/secret.txt"), b"secret\n").expect("seed outside");
        Sandbox { root }
    }

    fn run(&self, action: &str, confined: bool) -> i32 {
        let mut cmd = Command::new(child_bin());
        cmd.arg("--action")
            .arg(action)
            .arg("--ro")
            .arg(self.root.join("ro"))
            .arg("--rw")
            .arg(self.root.join("rw"))
            .arg("--outside")
            .arg(self.root.join("outside"));
        if !confined {
            cmd.arg("--no-confinement");
        }
        cmd.status()
            .expect("spawn sandbox_child")
            .code()
            .unwrap_or(-1)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn confined_process_denies_dangerous_actions_and_allows_normal_ones() {
    let sandbox = Sandbox::new();

    // Permitted: normal runtime needs.
    for action in ["open_ro", "write_data", "threads", "mmap_exec", "futex"] {
        assert_eq!(
            sandbox.run(action, true),
            OK,
            "{action} should succeed inside confinement"
        );
    }

    // Denied: filesystem escape.
    for action in ["open_outside", "write_ro", "write_outside"] {
        assert_eq!(
            sandbox.run(action, true),
            DENIED,
            "{action} should be denied by Landlock"
        );
    }

    // Denied: syscall escape.
    for action in ["exec", "ptrace", "socket_inet", "process_vm"] {
        assert_eq!(
            sandbox.run(action, true),
            DENIED,
            "{action} should be denied by seccomp"
        );
    }
}

#[test]
fn baseline_without_confinement_allows_what_confinement_denies() {
    let sandbox = Sandbox::new();
    for action in ["open_outside", "write_outside", "socket_inet", "ptrace"] {
        assert_eq!(
            sandbox.run(action, false),
            OK,
            "{action} should succeed without confinement"
        );
    }
}

#[test]
fn probe_reports_which_layers_are_active() {
    let sandbox = Sandbox::new();
    let output = Command::new(child_bin())
        .arg("--action")
        .arg("probe")
        .arg("--ro")
        .arg(sandbox.root.join("ro"))
        .arg("--rw")
        .arg(sandbox.root.join("rw"))
        .output()
        .expect("spawn sandbox_child probe");
    assert!(
        output.status.success(),
        "probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("probe emitted JSON");

    for layer in ["no_new_privs", "close_fds", "sanitize_env", "rlimits", "landlock", "seccomp"] {
        let status = report["layers"]
            .as_array()
            .expect("layers array")
            .iter()
            .find(|entry| entry["layer"] == layer)
            .and_then(|entry| entry["status"].as_str())
            .unwrap_or("missing");
        assert_eq!(status, "active", "layer {layer} should be active: {report}");
    }
    assert_eq!(report["active"], serde_json::Value::Bool(true));
    assert!(
        report["landlock_abi"].as_u64().is_some(),
        "landlock ABI should be reported: {report}"
    );
}
