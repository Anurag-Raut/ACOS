// The workspace build fingerprint, shared by every crate whose binaries must
// be able to tell each other apart.
//
// One implementation, `include!`d by the build scripts (`crates/acos-mcp` and
// `crates/acos-webview`), because the two daemons answer the same question —
// "was this process built from the code I am running?" — and two copies of the
// digest would drift into two answers.
//
// It emits three `cargo:rustc-env` constants, identical for **every binary of
// one build** and different for any other code:
//
// * `ACOS_BUILD_GIT_SHA` — `git rev-parse --short=12 HEAD` of the checkout
//   (`unknown` without git, e.g. an installed tarball);
// * `ACOS_BUILD_SOURCE_HASH` — FNV-1a over every workspace source file, the
//   manifests, `Cargo.lock` and this file, so a commit, a dirty tree, a rename
//   or a changed build script all change it;
// * `ACOS_BUILD_DIRTY` — `"dirty"`/`"clean"` for the checkout at build time.
//
// Cargo reruns the script when a tracked path or the git HEAD changes, so a
// stale constant cannot silently claim a build it did not make.


use std::path::{Path, PathBuf};

fn acos_fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Every regular file under `dir`, sorted so the digest never depends on
/// directory order.
fn acos_collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => acos_collect_files(&path, out),
            Ok(meta) if meta.is_file() => out.push(path),
            _ => {}
        }
    }
}

fn acos_git(root: &Path, args: &[&str]) -> Option<String> {
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

/// The paths whose content the digest covers.
fn acos_fingerprint_paths(root: &Path) -> Vec<PathBuf> {
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
    // This file changes the constants every binary ships: it is part of the
    // build, so it is part of the fingerprint.
    tracked.push(root.join("build-support"));
    for krate in &crates {
        tracked.push(krate.join("Cargo.toml"));
        tracked.push(krate.join("src"));
        // A build script can change the shipped constants; keep it in the digest.
        tracked.push(krate.join("build.rs"));
    }
    tracked
}

/// Compute the fingerprint of the workspace containing `manifest_dir` and emit
/// it to cargo.
fn emit_build_fingerprint(manifest_dir: &Path) {
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.to_path_buf());
    let tracked = acos_fingerprint_paths(&root);

    let mut files: Vec<PathBuf> = Vec::new();
    for path in &tracked {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => files.push(path.clone()),
            Ok(meta) if meta.is_dir() => acos_collect_files(path, &mut files),
            _ => {}
        }
    }
    files.sort();

    // The digest covers path, size and content of every workspace source, so
    // renaming or moving code changes it too.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for file in &files {
        let rel = file.strip_prefix(&root).unwrap_or(file);
        hash = acos_fnv1a(rel.to_string_lossy().as_bytes(), hash);
        match std::fs::read(file) {
            Ok(content) => {
                hash = acos_fnv1a(&(content.len() as u64).to_le_bytes(), hash);
                hash = acos_fnv1a(&content, hash);
            }
            Err(_) => hash = acos_fnv1a(b"<unreadable>", hash),
        }
    }

    let sha = acos_git(&root, &["rev-parse", "--short=12", "HEAD"])
        .unwrap_or_else(|| "unknown".into());
    let dirty = acos_git(&root, &["status", "--porcelain"])
        .map(|status| !status.is_empty())
        .unwrap_or(false);
    println!("cargo:rustc-env=ACOS_BUILD_GIT_SHA={sha}");
    println!("cargo:rustc-env=ACOS_BUILD_SOURCE_HASH={hash:016x}");
    println!(
        "cargo:rustc-env=ACOS_BUILD_DIRTY={}",
        if dirty { "dirty" } else { "clean" }
    );
    if let Some(git_dir) = acos_git(&root, &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from) {
        // A commit or index change must rerun this script (installed binaries
        // ship without git; then the sha is `unknown` and only the source hash
        // discriminates).
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!("cargo:rerun-if-changed={}", git_dir.join("index").display());
        println!("cargo:rerun-if-changed={}", git_dir.join("refs").display());
    }
    for path in &tracked {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
}
