//! Build fingerprint for the daemon/client identity (REL3D).
//!
//! A `acos serve` daemon must never answer a client built from different code
//! (MODA11Y: an older daemon answered `code 5 unsupported` for a service the
//! newer client implements). The runtime identity is
//!
//!   crate version + git sha + a hash of the workspace sources + a hash of the
//!   binary itself (computed once at process start, in `identity.rs`).
//!
//! The two build-time constants live here:
//!
//! * `ACOS_BUILD_GIT_SHA` — `git rev-parse --short=12 HEAD` of the checkout the
//!   binary was built in (`unknown` when there is no git, e.g. a tarball);
//! * `ACOS_BUILD_SOURCE_HASH` — FNV-1a over every workspace source file plus the
//!   manifests and `Cargo.lock`. It is **identical for every binary of one
//!   build** (so the MCP server's `acos-mcp` and the daemon's `acos` agree) and
//!   changes whenever the sources do (a different commit, a dirty tree) — which
//!   is exactly the "was this binary built from that code?" question the daemon
//!   handshake asks.
//!
//! Cargo reruns this script when a workspace source, manifest or the git HEAD
//! changes, so a stale constant cannot silently claim a build it did not make.

use std::path::{Path, PathBuf};

fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Every regular file under `dir`, sorted so the digest never depends on
/// directory order.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => collect(&path, out),
            Ok(meta) if meta.is_file() => out.push(path),
            _ => {}
        }
    }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("workspace root");
    // The crate directories that make up the workspace (the sources whose
    // change must change the digest).
    let mut crates: Vec<PathBuf> = std::fs::read_dir(root.join("crates"))
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    crates.sort();

    let mut tracked: Vec<PathBuf> = Vec::new();
    tracked.push(root.join("Cargo.toml"));
    tracked.push(root.join("Cargo.lock"));
    for krate in &crates {
        tracked.push(krate.join("Cargo.toml"));
        tracked.push(krate.join("src"));
        // A build script can change the shipped constants; keep it in the digest.
        tracked.push(krate.join("build.rs"));
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for path in &tracked {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => files.push(path.clone()),
            Ok(meta) if meta.is_dir() => collect(path, &mut files),
            _ => {}
        }
    }
    files.sort();

    // The digest covers path, size and content of every workspace source, so
    // renaming or moving code changes it too.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for file in &files {
        let rel = file.strip_prefix(&root).unwrap_or(file);
        hash = fnv1a(rel.to_string_lossy().as_bytes(), hash);
        match std::fs::read(file) {
            Ok(content) => {
                hash = fnv1a(&(content.len() as u64).to_le_bytes(), hash);
                hash = fnv1a(&content, hash);
            }
            Err(_) => hash = fnv1a(b"<unreadable>", hash),
        }
    }

    let sha = git(&root, &["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&root, &["status", "--porcelain"])
        .map(|status| !status.is_empty())
        .unwrap_or(false);
    println!("cargo:rustc-env=ACOS_BUILD_GIT_SHA={sha}");
    println!("cargo:rustc-env=ACOS_BUILD_SOURCE_HASH={hash:016x}");
    println!(
        "cargo:rustc-env=ACOS_BUILD_DIRTY={}",
        if dirty { "dirty" } else { "clean" }
    );
    if let Ok(git_dir) = git(&root, &["rev-parse", "--absolute-git-dir"])
        .map(PathBuf::from)
        .ok_or(())
    {
        // A commit or index change must rerun this script (installed binaries
        // ship without git; then the sha is `unknown` and only the source hash
        // and the binary hash discriminate).
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!("cargo:rerun-if-changed={}", git_dir.join("index").display());
        println!("cargo:rerun-if-changed={}", git_dir.join("refs").display());
    }
    for path in &tracked {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
}
