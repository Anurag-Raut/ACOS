//! W21: the guest-side priority adapter (`setpriority` clamp in libartshim.so).
//!
//! Stock `init.rc` sets `RLIMIT_NICE=40`, so an app thread may ask for a
//! negative nice (`Process.setThreadPriority(THREAD_PRIORITY_DISPLAY)` = -4).
//! An unprivileged ACOS host session has a hard limit of 0 and a user-namespace
//! CAP_SYS_RESOURCE cannot raise it, so `androidSetThreadPriority` would turn
//! EACCES into `SecurityException` and kill React Native's
//! `create_react_context` thread before any JS exists.
//!
//! The adapter lives in the LD_PRELOAD shim (`crates/acos-art/shim/artshim.c`),
//! i.e. it only exists inside the sandboxed guest. This test pins its
//! behaviour on the host: interposition happens, a negative nice succeeds and
//! is reported once, and a non-negative priority is still forwarded to the
//! kernel untouched.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/w21-test-shim");
    std::fs::create_dir_all(&dir).expect("test shim dir");
    dir
}

/// Build the shim into a private directory (same code path as a real launch).
fn shim(dir: &Path) -> PathBuf {
    acos_art::build_shim(dir).expect("build libartshim.so")
}

/// Compile a throwaway program: `setpriority(PRIO_PROCESS, 0, <nice>)` on
/// itself, exit 0 on success and 3 on failure.
fn probe(dir: &Path) -> PathBuf {
    let src = dir.join("prio_probe.c");
    let exe = dir.join("prio_probe");
    std::fs::write(
        &src,
        "#include <sys/resource.h>\n\
         int main(int argc, char **argv) {\n\
           if (argc < 2) return 2;\n\
           const char *a = argv[1];\n\
           int neg = 0, p = 0;\n\
           if (*a == '-') { neg = 1; a++; }\n\
           while (*a) { p = p * 10 + (*a - '0'); a++; }\n\
           if (neg) p = -p;\n\
           return setpriority(PRIO_PROCESS, 0, p) == 0 ? 0 : 3;\n\
         }\n",
    )
    .expect("write probe");
    let cc = std::env::var("CC").unwrap_or_else(|_| "clang".into());
    let st = Command::new(&cc)
        .args(["-x", "c", "-O0", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .expect("run cc");
    assert!(st.success(), "{cc} failed to build the probe");
    exe
}

#[test]
fn the_preloaded_shim_clamps_a_negative_nice_only_inside_the_process() {
    let dir = scratch();
    let shim = shim(&dir);
    let probe = probe(&dir);

    // Baseline without the adapter. On this host the hard RLIMIT_NICE is below
    // stock's 40, so -4 is refused; that is exactly the wall W20 recorded. On a
    // host that can do it the interposition half below still has to hold.
    let base = Command::new(&probe)
        .arg("-4")
        .output()
        .expect("baseline probe");
    let baseline_refused = !base.status.success();
    if baseline_refused {
        assert_eq!(
            base.status.code(),
            Some(3),
            "probe reports failure as exit 3"
        );
    }

    let out = Command::new(&probe)
        .arg("-4")
        .env("LD_PRELOAD", &shim)
        .output()
        .expect("preloaded probe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "setpriority(-4) must succeed with the adapter preloaded (exit {:?}); stderr: {stderr}",
        out.status.code()
    );
    assert!(
        stderr.contains("clamped to 0"),
        "the adapter must report the clamp (it is a fidelity compromise, not a silent no-op); \
         stderr: {stderr}"
    );
    assert!(
        stderr.matches("clamped to 0").count() <= 1,
        "the clamp note is printed once per process; stderr: {stderr}"
    );

    // Teeth: the very same binary without the preload is refused on a
    // hard-limit-0 host, so the pass above is the adapter, not the environment.
    if baseline_refused {
        assert_eq!(base.status.code(), Some(3));
    }

    // A non-negative priority must still reach the kernel untouched: nice 5
    // always succeeds and must not print the clamp note.
    let out5 = Command::new(&probe)
        .arg("5")
        .env("LD_PRELOAD", &shim)
        .output()
        .expect("preloaded probe (positive)");
    assert!(out5.status.success(), "setpriority(5) must succeed");
    assert!(
        !String::from_utf8_lossy(&out5.stderr).contains("clamped"),
        "a non-negative priority must not be clamped"
    );
}
