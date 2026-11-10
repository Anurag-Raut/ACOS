//! End-to-end test for the `acos serve` session daemon (F-08).
//!
//! Starts the daemon through `acos launch --keep` (which auto-starts `acos
//! serve`), then drives the *same* live session from separate `acos session …`
//! processes, and finally asserts the daemon exits on its idle timeout once the
//! last session is destroyed.
//!
//! The fixture is `native-hello` with `--native`: it is the app that "works
//! today" (it reaches `first_frame`), so `observe` reads a real frame and the
//! native confined-child path is exercised end to end. The test skips loudly if
//! the fixture APK or the boot-classpath cache is absent.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn acos() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

fn fixture() -> Option<PathBuf> {
    let apk = repo_root().join("fixtures/build/apps/native-hello.apk");
    apk.is_file().then_some(apk)
}

fn boot_available() -> bool {
    repo_root()
        .join(".cache/boot/android-34/BOOTCLASSPATH")
        .is_file()
}

fn runtime_dir() -> PathBuf {
    std::env::temp_dir().join(format!("acos-w91a-serve-{}", std::process::id()))
}

fn run(args: &[&str]) -> Output {
    let mut command = Command::new(acos());
    command
        .current_dir(repo_root())
        .env("ACOS_RUNTIME_DIR", runtime_dir())
        .env("ACOS_SESSION_IDLE_SECS", "2")
        .env("ACOS_MCP_QUIET", "1")
        .args(args);
    command.output().expect("run acos")
}

fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| {
        panic!(
            "stdout should be valid JSON: {e}\n{}",
            String::from_utf8_lossy(bytes)
        )
    })
}

#[test]
fn daemon_holds_a_native_session_across_cli_invocations_and_idles_out() {
    if !boot_available() {
        eprintln!("SKIP daemon_holds_a_native_session...: no boot cache");
        return;
    }
    let Some(apk) = fixture() else {
        eprintln!("SKIP daemon_holds_a_native_session...: native-hello.apk missing");
        return;
    };
    let dir = runtime_dir();
    let _ = std::fs::remove_dir_all(&dir);
    let socket = dir.join("session.sock");

    // 1. Launch through the daemon (`--keep` auto-starts `acos serve`).
    let launch = run(&[
        "launch",
        apk.to_str().unwrap(),
        "--native",
        "--keep",
        "--json",
        "--timeout",
        "240",
    ]);
    let launch_json = parse_json(&launch.stdout);
    if launch_json["error"].is_object()
        && launch_json["error"]["code"] == "CONFINEMENT_UNAVAILABLE"
    {
        eprintln!("SKIP: confinement unavailable on this host: {launch_json:#?}");
        return;
    }
    let first_frame_ok = launch_json["result"]["stages"]
        .as_array()
        .and_then(|stages| stages.iter().find(|s| s["stage"] == "first_frame"))
        .map(|s| s["status"] == "ok")
        .unwrap_or(false);
    assert!(
        first_frame_ok,
        "native-hello must publish a frame through the daemon: {launch_json:#?}"
    );
    assert!(
        socket.exists(),
        "the daemon socket {} must exist after --keep",
        socket.display()
    );
    // The runtime dir is private.
    let mode = std::fs::metadata(&dir).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(mode.mode() & 0o777, 0o700, "runtime dir must be 0700");

    // 2. A separate process lists the daemon's sessions.
    let list = run(&["session", "list", "--json"]);
    assert!(list.status.success(), "session list: {list:?}");
    let list_json = parse_json(&list.stdout);
    assert_eq!(list_json["status"], "pass", "{list_json:#?}");
    let sessions = list_json["result"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "one session expected: {list_json:#?}");
    assert_eq!(sessions[0]["session_id"], "s_cli");
    assert_eq!(sessions[0]["native"], true);
    assert_eq!(sessions[0]["launched"], true);

    // 3. A separate process observes the same live frame.
    let observe = run(&["session", "observe", "--json"]);
    let observe_json = parse_json(&observe.stdout);
    assert_eq!(observe_json["status"], "pass", "{observe_json:#?}");
    assert_eq!(observe_json["result"]["published_frames"], 1);
    assert!(
        observe_json["result"]["frame"]["width"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "the daemon must serve the child's frame: {observe_json:#?}"
    );

    // 4. Reset, then destroy the session from separate processes.
    let reset = run(&["session", "reset", "--json"]);
    let reset_json = parse_json(&reset.stdout);
    assert_eq!(reset_json["status"], "pass", "{reset_json:#?}");
    assert_eq!(reset_json["result"]["state_reset"], true);

    let destroy = run(&["session", "destroy", "--json"]);
    let destroy_json = parse_json(&destroy.stdout);
    assert_eq!(destroy_json["status"], "pass", "{destroy_json:#?}");
    assert_eq!(destroy_json["result"]["destroyed"], true);

    // 5. With no sessions left, the daemon exits on its 2s idle timeout.
    let deadline = Instant::now() + Duration::from_secs(30);
    while socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        !socket.exists(),
        "daemon must exit on idle and remove {}",
        socket.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
