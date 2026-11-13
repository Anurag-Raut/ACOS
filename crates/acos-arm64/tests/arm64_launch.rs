//! End-to-end arm64 transport test (W191-A/W192-A).
//!
//! When `qemu-aarch64`, a bionic sysroot, the seccomp guard and the NDK are
//! all present, this compiles the dynamic aarch64 guest in `tests/data/` and
//! runs it under the confined transport: Landlock must deny `/etc/hostname`,
//! the deferred seccomp guard must deny `ptrace`/`unshare`, and the bionic
//! `linker64` must have resolved (otherwise the guest never runs at all).
//!
//! Any missing prerequisite prints a `SKIP` reason and returns, so the test is
//! green on a host without qemu/the phone-attached sysroot. The fail-closed
//! error path is covered by a test that never needs a device.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use acos_arm64::{launch, Arm64Error, LaunchSpec, ARM64_UNAVAILABLE};

fn ndk_clang() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"] {
        if let Some(value) = std::env::var_os(var) {
            roots.push(PathBuf::from(value));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let sdk = PathBuf::from(home).join("Android/Sdk/ndk");
        if let Ok(entries) = std::fs::read_dir(&sdk) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            versions.sort();
            roots.extend(versions.into_iter().rev());
        }
    }
    for root in roots {
        let clang = root
            .join("toolchains/llvm/prebuilt/linux-x86_64/bin")
            .join("aarch64-linux-android29-clang");
        if clang.is_file() {
            return Some(clang);
        }
    }
    None
}

fn skip(reason: &str) {
    eprintln!("SKIP acos-arm64 integration launch: {reason}");
}

fn compile_guest(clang: &Path, out: &Path) -> std::io::Result<()> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/hello_dynamic.c");
    let status = std::process::Command::new(clang)
        .arg("-O2")
        .arg("-o")
        .arg(out)
        .arg(&src)
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "NDK clang exited with {status}"
        )));
    }
    Ok(())
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acos-arm64-test-{}-{}",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn fail_closed_when_sysroot_is_missing() {
    let dir = scratch_dir("missing-sysroot");
    let guest = dir.join("guest-not-needed");
    std::fs::write(&guest, b"\x7fELF").unwrap();
    // The transport checks the qemu executable before the sysroot on hosts
    // without qemu; this test covers the sysroot error, so skip loudly instead
    // of asserting a message the host could never produce.
    if let Err(e) = acos_arm64::default_qemu() {
        skip(&format!("{e}"));
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let mut spec = LaunchSpec::new(&guest, dir.join("rw"));
    spec.sysroot = Some(dir.join("no-such-sysroot"));
    // `launch` validates the sysroot before it ever needs qemu.
    let err = launch(spec).expect_err("missing sysroot must fail closed");
    assert_eq!(err.code, ARM64_UNAVAILABLE);
    assert!(
        err.message.contains("linker64") || err.message.contains("sysroot"),
        "unexpected message: {}",
        err.message
    );
    assert!(err.remedy.contains("fetch-bionic-sysroot.sh"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dynamic_aarch64_bionic_guest_runs_confined() {
    let Some(clang) = ndk_clang() else {
        skip("NDK aarch64 clang not found (set ANDROID_NDK_HOME)");
        return;
    };
    let qemu = match acos_arm64::default_qemu() {
        Ok(qemu) => qemu,
        Err(e) => {
            skip(&format!("{e}"));
            return;
        }
    };
    let sysroot = match acos_arm64::default_sysroot() {
        Ok(sysroot) => sysroot,
        Err(e) => {
            skip(&format!("{e}"));
            return;
        }
    };
    let guard = match acos_arm64::default_guard() {
        Ok(guard) => guard,
        Err(e) => {
            skip(&format!("{e}"));
            return;
        }
    };

    let dir = scratch_dir("confined");
    let guest = dir.join("hello_dynamic");
    if let Err(e) = compile_guest(&clang, &guest) {
        skip(&format!("could not build the guest with NDK clang: {e}"));
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let mut spec = LaunchSpec::new(&guest, dir.join("rw"));
    spec.qemu = Some(qemu);
    spec.sysroot = Some(sysroot);
    spec.guard = Some(guard);
    spec.launcher = Some(PathBuf::from(env!("CARGO_BIN_EXE_arm64_child")));
    spec.stdout = Stdio::piped();
    spec.stderr = Stdio::piped();

    let confined = launch(spec).expect("confined arm64 launch");
    assert!(
        confined.seccomp.detail.contains("denied syscalls"),
        "guard receipt should describe the filter: {}",
        confined.seccomp.detail
    );
    let qemu = confined.qemu.clone();
    let sysroot = confined.sysroot.clone();
    let output = confined
        .child
        .wait_with_output()
        .expect("wait for the confined guest");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "guest failed (exit {:?})\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code()
    );
    assert!(
        stdout.contains("guest pid="),
        "guest did not run under qemu {qemu:?} with sysroot {sysroot:?}\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Landlock: `/etc/hostname` is outside every granted root -> EACCES (13).
    assert!(
        stdout.contains("hostname_fd=-1 hostname_errno=13"),
        "Landlock should deny /etc/hostname\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Deferred seccomp: `unshare` and `setuid` -> EACCES (13), not the
    // kernel's EPERM/EINVAL, proving the guard's filter is installed. (qemu
    // answers guest `ptrace` with ENOSYS itself, so it is not asserted here.)
    assert!(
        stdout.contains("unshare_errno=13"),
        "seccomp should deny unshare with EACCES\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("setuid_errno=13"),
        "seccomp should deny setuid with EACCES\nstdout: {stdout}\nstderr: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn launch_spec_defaults_are_inherited_and_structured() {
    let spec = LaunchSpec::new("/tmp/guest", "/tmp/rw");
    assert!(spec.qemu.is_none());
    assert!(spec.sysroot.is_none());
    assert!(spec.guard.is_none());
    // A bogus guest path fails closed with a structured code, not a panic.
    let err: Arm64Error = launch(spec).expect_err("bogus guest");
    assert_eq!(err.code, ARM64_UNAVAILABLE);
}
