//! W86-A: a real NDK fixture clears and draws a triangle through EGL **under
//! the ADR-0022 confinement**.
//!
//! `fixtures/jni/acos_gles_surface.c` is compiled with the NDK and imports
//! `libEGL.so`/`libGLESv2.so` like a third-party Flutter/React Native library.
//! The parent test spawns a child copy of this binary; the child
//! (`PR_SET_NO_NEW_PRIVS` + Landlock + seccomp) then:
//!
//! 1. installs the native NDK host *and* the software GLES substrate
//!    (`acos_jni::ndk_host::install_forced`, i.e. the `--native` wiring),
//! 2. loads `libacosgles.so` with `require_confinement = true` (fail closed),
//! 3. calls `acos_gles_test(ANativeWindow*)`, which does the full EGL dance and
//!    returns a failure bitmask,
//! 4. reports the frame the compositor sink received.
//!
//! The parent fails if confinement did not actually engage, if the fixture
//! reported any failing step, or if no frame was published.

use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use acos_native_loader::{Loader, LoaderConfig};
use acos_vm::natives::gfx_hwui::{install_sink, CapturedFrame, FrameSink};

/// A recording frame sink: the child asserts the swap reached a sink at all,
/// which is the same interface the WMS compositor implements.
#[derive(Default)]
struct RecordingSink {
    last: Arc<Mutex<Option<CapturedFrame>>>,
    seq: u64,
}

impl FrameSink for RecordingSink {
    fn submit_frame(
        &mut self,
        window_id: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
        cause_id: i64,
    ) -> u64 {
        self.seq += 1;
        if let Ok(mut g) = self.last.lock() {
            *g = Some(CapturedFrame {
                window_id,
                width,
                height,
                rgba: rgba.to_vec(),
                cause_id,
                seq: self.seq,
            });
        }
        self.seq
    }
}

const CHILD_ENV: &str = "ACOS_GLE_W86A_CHILD";
const WIDTH: u32 = 32;
const HEIGHT: u32 = 32;
const LAYER: u64 = 0x77_0042;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/libacosgles.so")
}

/// Workspace-relative path of the NDK fixture (for the fixture gate).
const FIXTURE_REL: &str = "crates/acos-jni/tests/data/libacosgles.so";

/// The child: confine, wire the substrate, load the fixture, run it.
#[test]
fn gles_child_worker() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let data_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let scratch = repo_root().join(".cache/w86a-gles-fixture");
    let _ = std::fs::create_dir_all(&scratch);
    let profile = acos_sandbox::ConfineProfile::default()
        .with_read_only(&data_dir)
        .with_read_write(&scratch);
    if let Err(e) = acos_sandbox::confine(&profile) {
        println!("W86A_SUMMARY {}", serde_json::json!({"confined": false, "error": e.to_string()}));
        return;
    }
    let confined = acos_sandbox::confinement_active();

    // Observe the frame the swap publishes, then run the `--native` wiring
    // (NDK host + software GLES entry-point table).
    let recorded = Arc::new(Mutex::new(None));
    install_sink(Box::new(RecordingSink {
        last: Arc::clone(&recorded),
        seq: 0,
    }));
    acos_jni::ndk_host::install_forced();
    let window = acos_jni::ndk_host::register_native_window(
        WIDTH as i32,
        HEIGHT as i32,
        LAYER,
        0x5AA2,
    );

    let bytes = match std::fs::read(fixture()) {
        Ok(b) => b,
        Err(e) => {
            println!(
                "W86A_SUMMARY {}",
                serde_json::json!({"confined": confined, "error": format!("read fixture: {e}")})
            );
            return;
        }
    };
    let loader = Loader::new(LoaderConfig {
        require_confinement: true,
        missing_policy: acos_native_loader::MissingPolicy::Strict,
        ..LoaderConfig::default()
    });
    let handle = match loader.load_bytes(&bytes, "libacosgles.so") {
        Ok(h) => h,
        Err(e) => {
            println!(
                "W86A_SUMMARY {}",
                serde_json::json!({"confined": confined, "error": format!("load: {e}")})
            );
            return;
        }
    };
    let Some(entry) = handle.symbol("acos_gles_test") else {
        println!(
            "W86A_SUMMARY {}",
            serde_json::json!({"confined": confined, "error": "acos_gles_test is not exported"})
        );
        return;
    };
    let test: extern "C" fn(*mut std::ffi::c_void) -> c_int =
        unsafe { std::mem::transmute(entry) };
    let failures = test(window as usize as *mut std::ffi::c_void);

    let frame = recorded.lock().ok().and_then(|g| g.clone());
    println!(
        "W86A_SUMMARY {}",
        serde_json::json!({
            "confined": confined,
            "failures": failures,
            "frame": frame.as_ref().map(|f| serde_json::json!({
                "width": f.width,
                "height": f.height,
                "rgba_len": f.rgba.len(),
                "window_id": f.window_id,
            })),
        })
    );
}

/// Parent: run the child, require confinement, a clean fixture and a frame.
#[test]
fn gles_fixture_draws_a_triangle_under_confinement() {
    if !acos_test_support::have_fixture(FIXTURE_REL) {
        eprintln!(
            "SKIP gles_fixture_draws_a_triangle_under_confinement: {} is absent; run \
             fixtures/jni/build.sh",
            fixture().display()
        );
        return;
    }
    let exe = std::env::current_exe().expect("test binary");
    let output = Command::new(exe)
        .args(["gles_child_worker", "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("ACOS_W86A_CHILD", "1")
        .output()
        .expect("spawn confined child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let summary = stdout
        .lines()
        .find(|l| l.starts_with("W86A_SUMMARY "))
        .unwrap_or_else(|| panic!("no W86A_SUMMARY; stdout:\n{stdout}\nstderr:\n{stderr}"));
    let json: serde_json::Value =
        serde_json::from_str(summary.trim_start_matches("W86A_SUMMARY ")).expect("summary json");
    assert_eq!(
        json["confined"], true,
        "the fixture must run inside the ADR-0022 confinement: {json}"
    );
    assert_eq!(json["failures"], 0, "fixture steps failed: {json}");
    let frame = &json["frame"];
    assert!(!frame.is_null(), "a frame must reach the sink: {json}");
    assert_eq!(frame["width"], WIDTH as u64);
    assert_eq!(frame["height"], HEIGHT as u64);
    assert_eq!(
        frame["rgba_len"],
        (WIDTH * HEIGHT * 4) as u64,
        "full RGBA8888 frame: {json}"
    );
}
