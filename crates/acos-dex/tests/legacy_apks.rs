//! Read-only parse + verify of real-world `classes*.dex` extracted from APKs.
//!
//! This test never writes into the repository: extracted entries are written to
//! a temporary directory under the system temp dir (typically `/tmp`) and
//! cleaned up afterwards.
//!
//! Sources:
//! * `ACOS_LEGACY_APK_DIR` (default `/home/Qui/Work/android-container/helper/build`)
//! * `ACOS_EXTRA_APKS` — colon-separated extra apk/jar paths
//! * when `ACOS_SDK_APKS=1`, APKs/JARs under `~/Android/Sdk` that contain a
//!   `classes*.dex` entry are also exercised.

use acos_dex::DexFile;
use std::io::Read;
use std::path::{Path, PathBuf};

fn extract_dex(apk: &Path) -> Vec<(String, Vec<u8>)> {
    let data = match std::fs::read(apk) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skip {apk:?}: {e}");
            return Vec::new();
        }
    };
    let cursor = std::io::Cursor::new(data);
    let mut archive = match zip::ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("skip {apk:?}: not a zip ({e})");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        let is_dex = name.starts_with("classes") && name.ends_with(".dex");
        if !is_dex {
            continue;
        }
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_ok() {
            out.push((name, bytes));
        }
    }
    out
}

fn collect_apks() -> Vec<PathBuf> {
    let mut apks = Vec::new();

    let dir = std::env::var("ACOS_LEGACY_APK_DIR")
        .unwrap_or_else(|_| "/home/Qui/Work/android-container/helper/build".to_string());
    let dir = PathBuf::from(dir);
    if dir.is_dir() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for e in read.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "apk").unwrap_or(false) {
                    apks.push(p);
                }
            }
        }
    }
    apks.sort();

    if let Ok(extra) = std::env::var("ACOS_EXTRA_APKS") {
        for p in extra.split(':').filter(|s| !s.is_empty()) {
            apks.push(PathBuf::from(p));
        }
    }

    if std::env::var("ACOS_SDK_APKS").as_deref() == Ok("1") {
        let home = std::env::var("HOME").unwrap_or_default();
        let sdk = PathBuf::from(home).join("Android/Sdk");
        collect_sdk_apks(&sdk, &mut apks, 0);
    }
    apks
}

fn collect_sdk_apks(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 || out.len() > 20 {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for e in read.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_sdk_apks(&p, out, depth + 1);
        } else if matches!(
            p.extension().and_then(|x| x.to_str()),
            Some("apk") | Some("jar")
        ) {
            out.push(p);
        }
    }
}

#[test]
fn legacy_apk_dex_parse_and_verify() {
    let apks = collect_apks();
    if apks.is_empty() {
        eprintln!("no legacy APKs found; skipping");
        return;
    }

    let tmp_dir = std::env::temp_dir().join(format!("acos-dex-legacy-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);

    let mut parsed = 0u64;
    let mut classes_total = 0u64;
    let mut instructions_total = 0u64;

    for apk in &apks {
        for (name, bytes) in extract_dex(apk) {
            // Copy to /tmp (never into the repo) and parse from disk.
            let tmp = tmp_dir.join(format!(
                "{}-{}",
                apk.file_stem().and_then(|s| s.to_str()).unwrap_or("apk"),
                name
            ));
            std::fs::write(&tmp, &bytes).expect("write temp dex");
            let dex = DexFile::parse(bytes.as_slice())
                .unwrap_or_else(|e| panic!("parse {apk:?}!{name}: {} {e}", e.code()));
            let report = dex
                .verify()
                .unwrap_or_else(|e| panic!("verify {apk:?}!{name}: {} {e}", e.code()));
            parsed += 1;
            classes_total += report.classes;
            instructions_total += report.instructions;
            let _ = std::fs::remove_file(&tmp);
            eprintln!(
                "ok {apk:?}!{name}: {} bytes, {} classes, {} instructions",
                bytes.len(),
                report.classes,
                report.instructions
            );
        }
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
    assert!(parsed > 0, "no classes.dex extracted");
    eprintln!(
        "legacy totals: {parsed} dex files, {classes_total} classes, {instructions_total} instructions"
    );
}
