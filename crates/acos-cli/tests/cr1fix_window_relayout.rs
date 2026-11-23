//! CR1FIX2 (round-2 #1): the guest must not acknowledge a `WINDOW` command as
//! done when no relayout ran. With `ACOSLAUNCH_TEST_WINDOW_NO_ROOT` the decor
//! reports no attached root, so the relayout cannot run: the host must report
//! the command as failed (`GUEST_WINDOW_FAILED`), never a false success.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

fn acos_bin() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

fn art_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("ACOS_ART_BIN") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return p;
        }
    }
    Path::new(acos_bin())
        .parent()
        .expect("acos binary dir")
        .join("acos-art")
}

fn run(root: &Path, no_root: bool, tag: &str) -> (bool, String) {
    let artifacts = root
        .join(".cache")
        .join(format!("cr1fix2-window-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&artifacts);
    let mut cmd = Command::new(acos_bin());
    cmd.current_dir(root)
        .env("ACOS_ART_BIN", art_bin())
        .env("ACOSLAUNCH_FRAME_WAIT_SECS", "8")
        .args([
            "verify",
            "scenarios/multiwindow-split.yaml",
            "--engine",
            "art",
            "--keep-artifacts",
            "none-on-pass",
            "--artifacts",
        ])
        .arg(&artifacts)
        .args(["--overwrite", "--json"]);
    if no_root {
        cmd.env("ACOSLAUNCH_TEST_WINDOW_NO_ROOT", "1");
    } else {
        cmd.env_remove("ACOSLAUNCH_TEST_WINDOW_NO_ROOT");
    }
    let out = cmd.output().expect("run acos verify");
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&artifacts);
    (out.status.success(), text)
}

#[test]
fn a_window_command_whose_relayout_cannot_run_is_reported_failed() {
    let root = repo_root();
    if acos_test_support::require_art(&root, "CR1FIX2 #1 window relayout").is_none() {
        return;
    }
    let (ok, text) = run(&root, true, "noroot");
    assert!(
        !ok,
        "WINDOW with no attached root was acknowledged as success:\n{text}"
    );
    assert!(
        text.contains("GUEST_WINDOW_FAILED"),
        "the host must report GUEST_WINDOW_FAILED:\n{text}"
    );
}

#[test]
fn a_window_command_with_a_root_still_succeeds() {
    let root = repo_root();
    if acos_test_support::require_art(&root, "CR1FIX2 #1 window relayout control").is_none() {
        return;
    }
    let (ok, text) = run(&root, false, "control");
    assert!(ok, "multiwindow-split must pass with a root:\n{text}");
}
