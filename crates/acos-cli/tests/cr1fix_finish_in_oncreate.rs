//! CR1FIX (#11): an activity that calls `finish()` in `onCreate()` must report
//! a successful launch (`activity-resumed ... finish_during_create=true`), never
//! `HOST_TRANSACTION_TIMEOUT`, when `installTopFinishHook()` is active.

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

#[test]
fn host_scheduled_finish_in_oncreate_reports_launch_not_timeout() {
    let root = repo_root();
    if acos_test_support::require_art(&root, "CR1FIX #11 finish-in-onCreate").is_none() {
        return;
    }
    let perm_probe = root.join("fixtures/build/apps/perm-probe.apk");
    let perm_ctrl = root.join("fixtures/build/apps/permissioncontroller.apk");
    assert!(
        perm_probe.is_file() && perm_ctrl.is_file(),
        "perm-probe.apk and permissioncontroller.apk must be built"
    );

    // The controller does a little work between `super.onCreate()` and
    // `finish()` (fixture sleep), so the host-scheduled watcher can select the
    // Activity before it has finished. Launch repeatedly: any one launch that
    // ends in a timeout or never reports the final outcome fails the test.
    const LAUNCHES: usize = 10;
    for i in 0..LAUNCHES {
        let artifacts = root
            .join(".cache")
            .join(format!("cr1fix-11-test-{}-{i}", std::process::id()));
        let _ = std::fs::remove_dir_all(&artifacts);

        let out = Command::new(acos_bin())
            .current_dir(&root)
            .env("ACOS_ART_BIN", art_bin())
            .env("ACOSLAUNCH_RESUME_MS", "1000")
            .args([
                "verify",
                "scenarios/sysui1-permission-pregranted.yaml",
                "--engine",
                "art",
                "--keep-artifacts",
                "all",
                "--artifacts",
            ])
            .arg(&artifacts)
            .args(["--overwrite", "--json"])
            .output()
            .expect("run acos verify");

        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "launch {i}: sysui1-permission-pregranted failed: rc={:?}\nstdout={stdout}\nstderr={stderr}",
            out.status.code()
        );

        let proc1_stdout = artifacts.join("art-frames/process-1/stdout.log");
        let log = std::fs::read_to_string(&proc1_stdout)
            .unwrap_or_else(|e| panic!("read {}: {e}", proc1_stdout.display()));

        let _ = std::fs::remove_dir_all(&artifacts);

        assert!(
            !log.contains("HOST_TRANSACTION_TIMEOUT")
                && !log.contains("host-transaction-timeout")
                && !log.contains("watchdog: activity not resumed"),
            "launch {i}: secondary process hit a launch timeout:\n{log}"
        );
        // The FINAL successful outcome, not the intermediate
        // `host-transaction-resumed` marker.
        assert!(
            log.contains("ACOSLAUNCH MARK activity-resumed")
                && log.contains("finish_during_create=true"),
            "launch {i}: secondary process must report finish_during_create=true:\n{log}"
        );
    }
}
