//! CR1FIX #17 (and #15): native tests for the camera frame sink in
//! `shim/render_shim.c`.
//!
//! The shim is built for bionic and cannot run on the host, but its sink section
//! (everything after the "(4) Camera Option A frame sink" banner) only talks to
//! Android through `g_cam_api`. This test slices that section out of the real
//! source, compiles it with `tests/native/cam_sink_test.c` (a fake BufferQueue
//! behind `g_cam_api`) using the host C compiler, and runs each case. A wedged
//! sink trips the C test's `alarm()`.
//!
//! Set `CAM_SINK_SHIM_SOURCE=<path>` to run the same harness against another
//! revision of `render_shim.c` (used to show a case failing before its fix).

use std::path::PathBuf;
use std::process::Command;

const BANNER: &str = "(4) Camera Option A frame sink";

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn jni_include_dirs() -> Option<Vec<PathBuf>> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(h) = std::env::var_os("JAVA_HOME") {
        roots.push(PathBuf::from(h));
    }
    if let Ok(entries) = std::fs::read_dir("/usr/lib/jvm") {
        for e in entries.flatten() {
            roots.push(e.path());
        }
    }
    for r in roots {
        let inc = r.join("include");
        if inc.join("jni.h").is_file() {
            return Some(vec![inc.clone(), inc.join("linux")]);
        }
    }
    None
}

/// Compile the harness once per process; returns the binary.
fn build_harness(tag: &str) -> Option<PathBuf> {
    let Some(inc) = jni_include_dirs() else {
        eprintln!("cam_sink_native: no JDK jni.h on this host; skipping");
        return None;
    };
    let src_path = std::env::var_os("CAM_SINK_SHIM_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest().join("shim/render_shim.c"));
    let shim = std::fs::read_to_string(&src_path).expect("read render_shim.c");
    let marker = shim.find(BANNER).expect("sink banner present in render_shim.c");
    let start = shim[..marker].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let slice = &shim[start..];

    let dir = manifest()
        .join("../../.cache/cam-sink-native")
        .join(format!("{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cam_sink_slice.inc"), slice).unwrap();
    let bin = dir.join("cam_sink_test");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let mut cmd = Command::new(&cc);
    cmd.arg("-O1")
        .arg("-g")
        .arg("-pthread")
        .arg("-Wno-unused-parameter")
        .arg("-I")
        .arg(&dir)
        .arg("-I")
        .arg(manifest().join("tests/native"));
    for i in inc {
        cmd.arg("-I").arg(i);
    }
    cmd.arg("-o")
        .arg(&bin)
        .arg(manifest().join("tests/native/cam_sink_test.c"))
        .arg("-ldl");
    let out = cmd.output().unwrap_or_else(|e| panic!("run {cc}: {e}"));
    assert!(
        out.status.success(),
        "harness build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(bin)
}

fn run_case(case: &str) {
    let Some(bin) = build_harness(case) else {
        return;
    };
    let out = Command::new(&bin).arg(case).output().expect("run harness");
    let _ = std::fs::remove_dir_all(bin.parent().unwrap());
    assert!(
        out.status.success(),
        "case {case} failed (status {:?}):\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_parked_dequeue_does_not_wedge_other_streams_or_release() {
    run_case("blocked_dequeue_does_not_wedge");
}

#[test]
fn a_full_queue_drops_the_frame_instead_of_blocking() {
    run_case("full_queue_drops_frame");
}

#[test]
fn closing_a_stream_leaks_no_window_reference() {
    run_case("replace_waits_for_inflight");
}
