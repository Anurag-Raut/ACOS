//! WVKILL (guard for the fix): the engine must still die with the *broker process*.
//!
//! The WVKILL fix forks every engine on one permanent thread
//! (`cdp.rs`, `spawner`) so a session's connection thread ending cannot send
//! the engine bwrap's `--die-with-parent` signal. That must not cost the other
//! half of `--die-with-parent`: a broker *process* that dies (here: a child
//! process standing in for it, exiting) still takes its sandboxed engine down.
//! Needs only `bwrap` (no Chromium). The behaviour the fix is *for* — a second
//! session surviving the first one's end — is `engine_session_end.rs`.

use std::process::Command;
use std::time::Duration;

use acos_webview::sandbox::bwrap_argv;
use acos_webview::CdpClient;

fn bwrap_usable() -> bool {
    Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev", "--", "/bin/true"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn sandboxed_sleep() -> Command {
    let sleep = std::path::PathBuf::from("/usr/bin/sleep");
    let dir = std::env::temp_dir().join(format!("acos-wvkill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("profile dir");
    // The very argv the broker uses, with `sleep` standing in for the engine.
    let mut argv = bwrap_argv(&sleep, &dir, &["300".to_string()]);
    assert!(
        argv.iter().any(|a| a == "--die-with-parent"),
        "the sandbox argv must keep --die-with-parent (the broker's own death must still take the engine down)"
    );
    // bwrap_argv ends with `<engine> <args…>`; keep it as is.
    let mut c = Command::new("bwrap");
    c.args(argv.drain(..));
    c
}

#[test]
fn the_engine_still_dies_with_the_broker_process() {
    // The other half of --die-with-parent must survive the fix: a broker that
    // exits (here: a child process standing in for it) takes its engine down.
    if acos_test_support::require_prereq(
        bwrap_usable(),
        "the_engine_still_dies_with_the_broker_process",
        "bwrap is not usable here (bwrap --ro-bind / / -- /bin/true fails)",
    )
    .is_none()
    {
        return;
    }
    if std::env::var("ACOS_WVKILL_CHILD").as_deref() == Ok("1") {
        let client = CdpClient::spawn(sandboxed_sleep()).expect("spawn");
        println!("PID {}", client.pid());
        std::thread::sleep(Duration::from_millis(400));
        std::process::exit(0);
    }
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args(["the_engine_still_dies_with_the_broker_process", "--exact", "--nocapture", "--test-threads=1"])
        .env("ACOS_WVKILL_CHILD", "1")
        .output()
        .expect("child");
    let text = String::from_utf8_lossy(&out.stdout);
    let pid: u32 = text
        .lines()
        .find_map(|l| l.split("PID ").nth(1))
        .and_then(|p| p.trim().parse().ok())
        .expect("child printed the engine pid");
    // Give the kernel a moment to deliver PDEATHSIG and reap.
    let mut alive = true;
    for _ in 0..40 {
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            alive = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive, "bwrap pid {pid} outlived the broker process that spawned it");
}
