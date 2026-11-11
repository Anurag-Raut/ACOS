//! Best-effort build of the bionic `LD_PRELOAD` driver with the NDK.
//!
//! The peer itself is a normal host binary and needs no NDK.  The driver is
//! compiled only when an NDK clang is available (`ANDROID_NDK_HOME` or the
//! standard SDK path); otherwise `cargo test -p acos-binder-peer` still works
//! and the integration script reports that the driver is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn find_ndk() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ANDROID_NDK_HOME") {
        let p = PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let base = Path::new(&home).join("Android/Sdk/ndk");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    versions.sort();
    versions.pop()
}

fn main() {
    println!("cargo:rerun-if-changed=native/binder_driver.c");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let dst = out.join("libacos_binder_driver.so");
    let Some(ndk) = find_ndk() else {
        println!("cargo:warning=NDK not found; skipping libacos_binder_driver.so (peer tests do not need it)");
        return;
    };
    let clang = ndk.join("toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android34-clang");
    if !clang.is_file() {
        println!("cargo:warning={} missing; skipping driver build", clang.display());
        return;
    }
    let status = Command::new(&clang)
        .args([
            "-O2",
            "-fPIC",
            "-shared",
            "-Wall",
            "-Wno-unused-parameter",
            "-Wno-unused-function",
            "-o",
        ])
        .arg(&dst)
        .arg("native/binder_driver.c")
        .args(["-llog", "-ldl"])
        .status();
    match status {
        Ok(s) if s.success() => println!("cargo:warning=built {}", dst.display()),
        Ok(s) => println!("cargo:warning=driver build failed ({s})"),
        Err(e) => println!("cargo:warning=driver build failed: {e}"),
    }
}
