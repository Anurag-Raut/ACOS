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

    // Permitted: normal runtime needs. `tgkill_self` proves the signal
    // argument filter still allows the own thread group, and `personality_ok`
    // proves PER_LINUX is still installable, even though foreign signals and
    // namespace-bearing clones are denied.
    for action in [
        "open_ro",
        "write_data",
        "threads",
        "mmap_exec",
        "futex",
        "tgkill_self",
        "personality_ok",
        // W120-A: absolute-path resolution of a granted path must succeed; it
        // needs search on the ancestor chain, which the profile now grants.
        "canonicalize_ro",
        // Resolving an *outside* path is pure traversal (no content read), so
        // it also succeeds: the ancestor grant is `EXECUTE`-only. The content
        // stays unreachable — `open_outside`/`list_outside` below prove that.
        "canonicalize_outside",
    ] {
        assert_eq!(
            sandbox.run(action, true),
            OK,
            "{action} should succeed inside confinement"
        );
    }

    // Denied: filesystem escape.
    for action in [
        "open_outside",
        "write_ro",
        "write_outside",
        // `EXECUTE` (search) does not imply `READ_DIR` (listing): the ancestor
        // grant must not leak sibling directory names.
        "list_outside",
    ] {
        assert_eq!(
            sandbox.run(action, true),
            DENIED,
            "{action} should be denied by Landlock"
        );
    }

    // Denied: syscall escape. The W67A hardened set must return `EACCES`
    // (filtered), not `EPERM` (reached the kernel).
    for action in [
        "exec",
        "ptrace",
        "socket_inet",
        "process_vm",
        "io_uring_setup",
        "userfaultfd",
        "open_by_handle_at",
        "name_to_handle_at",
        "fsopen",
        "move_mount",
        "pidfd_open",
        "kcmp",
        "kill_other",
        "setuid",
        "clone_userns",
    ] {
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
    let abi = report["landlock_abi"].as_u64().expect("landlock ABI reported");
    assert!(abi >= 1, "landlock ABI should be present: {report}");

    // On ABI >= 6 the landlock layer must also scope signals and abstract UNIX
    // sockets to the sandbox domain (W67A #5); report it in the layer detail.
    if abi >= 6 {
        let detail = report["layers"]
            .as_array()
            .expect("layers array")
            .iter()
            .find(|entry| entry["layer"] == "landlock")
            .and_then(|entry| entry["detail"].as_str())
            .unwrap_or_default();
        assert!(
            detail.contains("scoped abstract_unix_socket+signal"),
            "landlock ABI {abi} should report the signal/abstract-socket scope: {detail}"
        );
    }
}

fn layer_status(report: &serde_json::Value, layer: &str) -> String {
    report["layers"]
        .as_array()
        .expect("layers array")
        .iter()
        .find(|entry| entry["layer"] == layer)
        .and_then(|entry| entry["status"].as_str())
        .unwrap_or("missing")
        .to_string()
}

/// W191-A: a deferred seccomp layer is recorded as `deferred`, satisfies the
/// fail-closed required-layer check, and leaves the process *not* confined,
/// because the filter is only installed after `execve` by the preload guard.
#[test]
fn deferred_seccomp_is_reported_but_not_active() {
    let sandbox = Sandbox::new();
    let guard = sandbox.root.join("libacos_seccomp_guard.so");
    std::fs::write(&guard, b"placeholder").expect("write guard placeholder");
    let output = Command::new(child_bin())
        .arg("--action")
        .arg("probe")
        .arg("--rw")
        .arg(sandbox.root.join("rw"))
        .arg("--seccomp-deferred")
        .arg(&guard)
        .output()
        .expect("spawn sandbox_child probe");
    assert!(
        output.status.success(),
        "probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("probe emitted JSON");
    assert_eq!(layer_status(&report, "seccomp"), "deferred");
    assert_eq!(layer_status(&report, "landlock"), "active");
    assert_eq!(
        report["active"],
        serde_json::Value::Bool(false),
        "a deferred seccomp layer must not mark the process confined: {report}"
    );
}

/// W191-A: a missing deferred guard is recorded as `failed`, so the caller
/// can fail closed with an exact remedy instead of pretending seccomp exists.
#[test]
fn deferred_seccomp_missing_guard_is_a_failure() {
    let sandbox = Sandbox::new();
    let missing = sandbox.root.join("no-such-guard.so");
    let output = Command::new(child_bin())
        .arg("--action")
        .arg("probe")
        .arg("--rw")
        .arg(sandbox.root.join("rw"))
        .arg("--seccomp-deferred")
        .arg(&missing)
        .output()
        .expect("spawn sandbox_child probe");
    assert!(output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("probe emitted JSON");
    assert_eq!(layer_status(&report, "seccomp"), "failed");
    let detail = report["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["layer"] == "seccomp")
        .and_then(|entry| entry["detail"].as_str())
        .unwrap_or_default();
    assert!(
        detail.contains("acos-seccomp-guard") && detail.contains("ACOS_SECCOMP_GUARD"),
        "failure detail must name the remedy: {detail}"
    );
}
