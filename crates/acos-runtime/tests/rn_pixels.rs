//! W164-A probe: launch the real RN fixture APK through the platform
//! lifecycle path (ActivityThread -> Application.onCreate -> MainActivity)
//! with native code enabled, and record how far the real framework gets.
//!
//! The RN session (`RnSession`) mounts the Paper renderer through a driver
//! harness with no Activity window. This probe asks the other half of the
//! question: can the *platform* path run the APK's own `ReactActivity`?
//!
//! The child is confined (ADR-0022) and prints structured stage lines; the
//! parent test only reports them (the evidence, not an assertion of success).

use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_ENV: &str = "ACOS_RN_W164A_CHILD";
const APK_ENV: &str = "ACOS_RN_W164A_APK";
const OUT_ENV: &str = "ACOS_RN_W164A_OUT";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn rn_pixels_child_worker() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let apk = PathBuf::from(std::env::var(APK_ENV).expect("apk"));
    let scratch = PathBuf::from(std::env::var(OUT_ENV).expect("scratch"));
    let boot_dir = repo_root().join(".cache/boot/android-34");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch dir");

    let mut profile = acos_sandbox::ConfineProfile::default().with_read_write(&scratch);
    // The session VFS root (native-lib extraction target) lives under
    // `.cache/sessions`; the confined child must be able to write there.
    let sessions = repo_root().join(".cache/sessions");
    let _ = std::fs::create_dir_all(&sessions);
    profile = profile.with_read_write(&sessions);
    for base in [Some(apk.clone()), Some(boot_dir.clone())]
        .into_iter()
        .flatten()
    {
        if let Ok(real) = std::fs::canonicalize(&base) {
            profile = profile.with_read_only(&real);
        }
        let mut cur = base.parent();
        while let Some(p) = cur {
            if p.exists() {
                profile = profile.with_read_only(p);
            }
            cur = p.parent();
        }
    }
    for dev in ["/dev/urandom", "/dev/null"] {
        if Path::new(dev).exists() {
            profile = profile.with_read_only(dev);
        }
    }
    match acos_sandbox::confine(&profile) {
        Ok(report) => println!("W164A_CONFINE {}", serde_json::json!(report)),
        Err(e) => {
            println!(
                "W164A_SUMMARY {}",
                serde_json::json!({"confined": false, "error": e.to_string()})
            );
            return;
        }
    }

    let session =
        acos_runtime::session::AgentSession::start(acos_runtime::session::SessionOptions {
            trace_natives: true,
            native: true,
            wall_secs: 240,
            ..acos_runtime::session::SessionOptions::default()
        });
    match session.launch(&apk) {
        Ok(report) => {
            println!(
                "W164A_LAUNCH {}",
                serde_json::json!({
                    "reached_stage": report.reached_stage,
                    "root_blocker": report.root_blocker.as_ref().map(|b| (b.code.clone(), b.message.clone(), b.details.clone())),
                    "stages": report.stages.iter().map(|s| (s.name, format!("{:?}", s.status))).collect::<Vec<_>>(),
                    "activity": report.activity,
                    "wall_ms": report.wall_ms,
                    "classes_loaded": report.classes_loaded,
                })
            );
            let events: Vec<&String> = report
                .events
                .iter()
                .filter(|e| {
                    let e = e.to_lowercase();
                    e.contains("acos")
                        || e.contains("react")
                        || e.contains("frame")
                        || e.contains("error")
                        || e.contains("exception")
                })
                .take(40)
                .collect();
            println!("W164A_EVENTS {}", serde_json::json!(events));
            match session.observe(None) {
                Ok(o) => println!(
                    "W164A_FRAME {}",
                    serde_json::json!({
                        "frame": o.frame.as_ref().map(|f| (f.width, f.height)),
                        "non_black": o.frame.as_ref().map(|f| f.pixels.chunks_exact(4).any(|p| p[0] | p[1] | p[2] != 0)),
                    })
                ),
                Err(e) => println!(
                    "W164A_FRAME {}",
                    serde_json::json!({"code": e.code, "error": e.message})
                ),
            }
        }
        Err(e) => println!(
            "W164A_SUMMARY {}",
            serde_json::json!({"launched": false, "code": e.code, "error": e.message})
        ),
    }
    session.close();
}

#[test]
fn rn_platform_launch_probe() {
    const APK_FIXTURE: &str = "fixtures/build/apps/rn-hello.apk";
    // Routed through the global gate: `ACOS_REQUIRE_FIXTURES=1` must fail, not
    // skip, when the React Native fixture is absent (W170-A C4/H5).
    if !acos_test_support::have_fixture(APK_FIXTURE) {
        eprintln!("SKIP rn_platform_launch_probe: no fixture apk");
        return;
    }
    let apk = repo_root().join(APK_FIXTURE);
    let exe = std::env::current_exe().expect("test exe");
    let scratch = repo_root().join("target/tmp/w164a-platform");
    let out = Command::new(exe)
        .env(CHILD_ENV, "1")
        .env(APK_ENV, &apk)
        .env(OUT_ENV, &scratch)
        .args([
            "--exact",
            "rn_pixels_child_worker",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .expect("spawn child");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut launch: Option<serde_json::Value> = None;
    let mut events: Option<serde_json::Value> = None;
    for line in text.lines().filter(|l| l.starts_with("W164A_")) {
        println!("{line}");
        if let Some(rest) = line.strip_prefix("W164A_LAUNCH ") {
            launch = serde_json::from_str(rest).ok();
        } else if let Some(rest) = line.strip_prefix("W164A_EVENTS ") {
            events = serde_json::from_str(rest).ok();
        }
    }
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));

    // W178-A guard: assert the *current* rung, not a legacy one.
    //
    // Three rungs are locked here:
    //   * the class loader (`FLAG_HAS_CODE == 0x4`) — no `ClassNotFoundException`;
    //   * `ComponentActivity.<init>` — the fixture now packages
    //     `lifecycle-viewmodel-savedstate`, so `activity_on_create` completes;
    //   * the native/`SoLoader` rung — `nativeLibraryDir` is the guest
    //     `/data/user/0/<pkg>/lib/x86_64` (not a host build path), the merged-so
    //     mapping plus a `System.loadLibrary`-backed `SoFileLoader` load
    //     `libreactnative.so` through `acos-native-loader` (its `jni_lib_merge`
    //     `JNI_OnLoad` runs), so no `UnsatisfiedLinkError` is raised.
    //
    // The next blocker is `ReactDelegate.loadApp` taking the bridgeless branch
    // (`enableBridgelessArchitecture()` is true) while the fixture is Paper, so
    // `ReactActivityDelegate.loadApp` passes a null root view to
    // `setContentView`. That is the next rung (see W164A_PIXELS.md §W178-A).
    let launch = launch.expect("child printed W164A_LAUNCH");
    assert_ne!(launch["reached_stage"], "apk_parsed", "launch: {launch}");
    assert!(
        launch["classes_loaded"].as_u64().unwrap_or(0) > 1000,
        "framework booted few classes: {launch}"
    );
    let blocker = launch["root_blocker"].to_string();
    assert!(
        !blocker.contains("ClassNotFoundException"),
        "the app class loader is empty again (FLAG_HAS_CODE regression): {blocker}"
    );
    let stages = launch["stages"].as_array().cloned().unwrap_or_default();
    let stage_status = |name: &str| -> Option<String> {
        stages
            .iter()
            .find(|s| s.get(0).and_then(|v| v.as_str()) == Some(name))
            .and_then(|s| s.get(1).and_then(|v| v.as_str()).map(str::to_string))
    };
    assert_eq!(
        stage_status("bind_application").as_deref(),
        Some("Ok"),
        "bindApplication did not complete: {launch}"
    );
    assert_eq!(
        stage_status("application_on_create").as_deref(),
        Some("Ok"),
        "Application.onCreate did not run (class loader rung regressed): {launch}"
    );
    assert_eq!(
        stage_status("activity_on_create").as_deref(),
        Some("Ok"),
        "Activity.onCreate did not complete (SavedStateHandle/native rung regressed): {launch}"
    );
    assert!(
        !blocker.contains("UnsatisfiedLinkError"),
        "an app native library failed to load again (SoLoader/native-load rung regressed): {blocker}"
    );
    let events = events.expect("child printed W164A_EVENTS");
    let events = events
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        events.contains("Preparing SO source")
            && events.contains("ApplicationSoSource")
            && events.contains("/lib/x86_64"),
        "SoLoader did not prepare the guest native-library SO source; events:\n{events}"
    );
}
