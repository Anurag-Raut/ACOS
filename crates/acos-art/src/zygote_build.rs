//! The production zygote's guest build (ZDEFAULT1).
//!
//! The persistent zygote (`zygote_prod`) needs three artefacts next to the normal
//! launch guest: the zygote guest dex (`LaunchMain` + `RenderNat` + `Nat` +
//! `SoftwareKeyStore` compiled together with `ZygoteApp`, `dev.acos.z1.Nat` and
//! the `ZygoteHooks` shim), the freestanding raw-syscall fork shim
//! (`libz2fork.so`) and a copy of the seccomp guard that `System.load` accepts
//! from a classloader namespace (no `__stack_chk_fail` reference).
//!
//! Until ZDEFAULT1 these came from `research/zygote-proto/z3/build.sh` and the
//! operator had to point `ACOS_ART_ZYGOTE_BUILD` at the result, which made the
//! zygote impossible to turn on by default. This module builds the same three
//! files **on demand into the shared build cache** (same toolchain, same flags,
//! same layout as `build.sh`): `<cache>/zygote-guest/<key>/{dex/classes.dex,
//! lib/libz2fork.so, lib/libacos_seccomp_guest.so}`. The key hashes every source
//! byte and the toolchain identity, so a changed source or compiler is a new
//! entry and a stale artefact is never reused. Concurrent first launches take
//! the per-key `flock` and build once.
//!
//! The zygote-only sources live under `crates/acos-art/zygote/` and are embedded
//! in the binary (like the preload shim), so an installed tree needs no research
//! checkout; the shared guest sources (`LaunchMain` ...) are read from the
//! asset path exactly as the exec path reads them.

use crate::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Zygote-only sources, embedded so an installed tree carries them.
pub const ZYGOTE_APP_JAVA: &str = include_str!("../zygote/java/dev/acos/art/ZygoteApp.java");
pub const Z1_NAT_JAVA: &str = include_str!("../zygote/java/dev/acos/z1/Nat.java");
pub const ZYGOTE_HOOKS_JAVA: &str = include_str!("../zygote/java/dalvik/system/ZygoteHooks.java");
pub const Z2FORK_C: &str = include_str!("../zygote/shim/z2fork.c");

/// Bumped when this build recipe changes (a different recipe is a different key).
const RECIPE: &str = "acos-art/zygote-guest/v1";

/// The shared guest sources the zygote dex compiles in (same list the exec
/// path's `build_launch_guest` compiles).
const SHARED_GUEST_SOURCES: [&str; 4] = [
    "dev/acos/art/LaunchMain.java",
    "dev/acos/art/RenderNat.java",
    "dev/acos/art/Nat.java",
    "dev/acos/art/SoftwareKeyStore.java",
];

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A finished zygote guest build.
#[derive(Debug, Clone)]
pub struct ZygoteGuestBuild {
    /// The directory `ACOS_ZYGOTE_APP` names (`dex/`, `lib/`).
    pub dir: PathBuf,
    /// The content-addressed key (hex).
    pub key: String,
    /// True when an existing verified entry was reused (no compiler ran).
    pub reused: bool,
}

fn complete(dir: &Path) -> bool {
    dir.join(".complete").is_file()
        && dir.join("dex/classes.dex").is_file()
        && dir.join("lib/libz2fork.so").is_file()
        && dir.join("lib/libacos_seccomp_guest.so").is_file()
}

fn host_cc() -> PathBuf {
    let name = std::env::var("CC").unwrap_or_else(|_| "clang".into());
    let p = PathBuf::from(&name);
    if p.components().count() > 1 {
        return p;
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(&name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    p
}

fn walk_classes(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk_classes(&path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("class") {
            out.push(path);
        }
    }
    Ok(())
}

fn run(what: &str, command: &mut Command) -> Result<()> {
    let out = command.output()?;
    if !out.status.success() {
        return Err(Error::Tool(format!(
            "{what} failed for the zygote guest:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// Build (or reuse) the zygote guest and return its directory.
///
/// Fails closed with [`Error::Tool`] naming the missing tool or the compiler
/// output; it never returns a partial directory.
pub fn ensure_guest_build() -> Result<ZygoteGuestBuild> {
    let guest_dir = crate::app::guest_source_dir();
    let shared: Vec<PathBuf> = SHARED_GUEST_SOURCES
        .iter()
        .map(|rel| guest_dir.join(rel))
        .collect();
    let sdk = crate::app::android_sdk()?;
    let android_jar = sdk.join("platforms/android-34/android.jar");
    let d8 = crate::app::find_tool(&sdk, "d8")?;
    let javac = PathBuf::from(std::env::var("JAVAC").unwrap_or_else(|_| "javac".into()));
    let cc = host_cc();
    for tool in [&android_jar, &d8] {
        if !tool.is_file() {
            return Err(Error::Tool(format!(
                "zygote guest build needs {} (set ANDROID_SDK_ROOT)",
                tool.display()
            )));
        }
    }

    // The key: every input byte and the identity of every tool.
    let mut parts: Vec<Vec<u8>> = vec![RECIPE.as_bytes().to_vec()];
    for path in &shared {
        parts.push(path.to_string_lossy().as_bytes().to_vec());
        parts.push(fs::read(path)?);
    }
    for (name, text) in [
        ("ZygoteApp.java", ZYGOTE_APP_JAVA),
        ("z1/Nat.java", Z1_NAT_JAVA),
        ("ZygoteHooks.java", ZYGOTE_HOOKS_JAVA),
        ("z2fork.c", Z2FORK_C),
        ("seccomp_guest.c", crate::seccomp::GUEST_SOURCE),
    ] {
        parts.push(name.as_bytes().to_vec());
        parts.push(text.as_bytes().to_vec());
    }
    for tool in [&android_jar, &d8, &javac, &cc] {
        parts.push(crate::build_cache::file_id(tool).into_bytes());
    }
    parts.push(b"javac -source 11 -target 11; d8 --min-api 34; cc -nostdlib -ffreestanding -fno-builtin -fno-stack-protector -fPIC -shared -O2".to_vec());
    let key = crate::build_cache::digest_owned(&parts);

    let root = crate::build_cache::cache_root().join("zygote-guest");
    let entry = root.join(&key);
    if complete(&entry) {
        return Ok(ZygoteGuestBuild {
            dir: entry,
            key,
            reused: true,
        });
    }
    fs::create_dir_all(&root)?;
    // One builder per key; a cold cohort compiles once and the rest wait and hit.
    let _lock = crate::build_cache::lock(&format!("zygote-guest-{key}"))?;
    if complete(&entry) {
        return Ok(ZygoteGuestBuild {
            dir: entry,
            key,
            reused: true,
        });
    }
    let stage = root.join(format!(
        ".tmp-{}-{}-{}",
        &key[..12],
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&stage);
    let src = stage.join("src");
    let classes = stage.join("classes");
    fs::create_dir_all(src.join("dev/acos/art"))?;
    fs::create_dir_all(src.join("dev/acos/z1"))?;
    fs::create_dir_all(src.join("dalvik/system"))?;
    fs::create_dir_all(&classes)?;
    fs::create_dir_all(stage.join("dex"))?;
    fs::create_dir_all(stage.join("lib"))?;
    let app = src.join("dev/acos/art/ZygoteApp.java");
    let z1 = src.join("dev/acos/z1/Nat.java");
    let hooks = src.join("dalvik/system/ZygoteHooks.java");
    let fork_c = src.join("z2fork.c");
    let guard_c = src.join("seccomp_guest.c");
    fs::write(&app, ZYGOTE_APP_JAVA)?;
    fs::write(&z1, Z1_NAT_JAVA)?;
    fs::write(&hooks, ZYGOTE_HOOKS_JAVA)?;
    fs::write(&fork_c, Z2FORK_C)?;
    fs::write(&guard_c, crate::seccomp::GUEST_SOURCE)?;

    let mut sources: Vec<&Path> = shared.iter().map(PathBuf::as_path).collect();
    sources.extend([app.as_path(), z1.as_path(), hooks.as_path()]);
    run(
        "javac",
        Command::new(&javac)
            .args(crate::jvm::dump_args())
            .args(["-nowarn", "-source", "11", "-target", "11", "-cp"])
            .arg(&android_jar)
            .arg("-d")
            .arg(&classes)
            .args(&sources),
    )?;
    let mut class_files = Vec::new();
    walk_classes(&classes, &mut class_files)?;
    class_files.sort();
    run(
        "d8",
        Command::new(&d8)
            .args(crate::jvm::d8_dump_args())
            .arg("--lib")
            .arg(&android_jar)
            .args(["--min-api", "34", "--output"])
            .arg(stage.join("dex"))
            .args(&class_files),
    )?;
    for (source, output) in [
        (&fork_c, "lib/libz2fork.so"),
        (&guard_c, "lib/libacos_seccomp_guest.so"),
    ] {
        run(
            "cc",
            Command::new(&cc)
                .args([
                    "-nostdlib",
                    "-ffreestanding",
                    "-fno-builtin",
                    "-fno-stack-protector",
                    "-fPIC",
                    "-shared",
                    "-O2",
                    "-w",
                    "-o",
                ])
                .arg(stage.join(output))
                .arg(source),
        )?;
    }
    let _ = fs::remove_dir_all(&classes);
    let _ = fs::remove_dir_all(&src);
    fs::write(stage.join(".complete"), format!("{RECIPE} {key}\n"))?;
    if !complete(&stage) {
        let _ = fs::remove_dir_all(&stage);
        return Err(Error::Tool(
            "the zygote guest build produced no dex or no shim".into(),
        ));
    }
    // Publish with one rename; a half-written entry is never visible.
    let _ = fs::remove_dir_all(&entry);
    fs::rename(&stage, &entry)?;
    eprintln!(
        "acos-art: zygote guest build {} -> {}",
        &key[..12],
        entry.display()
    );
    Ok(ZygoteGuestBuild {
        dir: entry,
        key,
        reused: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_sources_are_the_zygote_entry_points() {
        assert!(ZYGOTE_APP_JAVA.contains("class ZygoteApp"));
        assert!(ZYGOTE_APP_JAVA.contains("resetLogSink"));
        assert!(Z1_NAT_JAVA.contains("package dev.acos.z1;"));
        assert!(ZYGOTE_HOOKS_JAVA.contains("postForkChild"));
        assert!(Z2FORK_C.contains("spawnSession"));
    }

    #[test]
    fn an_incomplete_entry_is_not_a_build() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache")
            .join(format!("zgb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("dex")).unwrap();
        fs::write(dir.join("dex/classes.dex"), b"x").unwrap();
        assert!(!complete(&dir), "no .complete stamp and no libs");
        let _ = fs::remove_dir_all(&dir);
    }
}
