//! Env-gated parse + verify over the Android boot classpath.
//!
//! Set `ACOS_BOOT_JARS` to a directory of platform jars (e.g.
//! `/tmp/acos-framework-dex/sys/framework`). The test extracts every
//! `classes*.dex` entry in-memory (writing temporary copies under the system
//! temp dir, never into the repository), parses and structurally verifies each
//! one, and prints totals. When the env var is unset the tests skip loudly.
//!
//! A second test runs the `dexdump` differential for `framework.jar`
//! `classes.dex` and `classes3.dex` plus `core-oj.jar` `classes.dex`.

use acos_dex::DexFile;
use std::io::Read;
use std::path::{Path, PathBuf};

fn boot_jars_dir() -> Option<PathBuf> {
    std::env::var("ACOS_BOOT_JARS").ok().map(PathBuf::from)
}

/// Locate the built `dexdump` example next to the current test binary
/// (`target/<profile>/deps/<test>` -> `target/<profile>/examples/dexdump`).
fn example_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?;
    let candidate = profile_dir.join("examples").join("dexdump");
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

fn extract_dex(jar: &Path) -> Vec<(String, Vec<u8>)> {
    let data = match std::fs::read(jar) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skip {jar:?}: {e}");
            return Vec::new();
        }
    };
    let cursor = std::io::Cursor::new(data);
    let mut archive = match zip::ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("skip {jar:?}: not a zip ({e})");
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
        if name.starts_with("classes") && name.ends_with(".dex") {
            let mut bytes = Vec::new();
            if entry.read_to_end(&mut bytes).is_ok() {
                out.push((name, bytes));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn jars_in(dir: &Path) -> Vec<PathBuf> {
    let mut jars: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "jar").unwrap_or(false))
        .collect();
    jars.sort();
    jars
}

#[test]
fn boot_classpath_parse_and_verify() {
    let Some(dir) = boot_jars_dir() else {
        eprintln!(
            "ACOS_BOOT_JARS not set; skipping boot classpath test \
             (set ACOS_BOOT_JARS=/tmp/acos-framework-dex/sys/framework to run)"
        );
        return;
    };
    assert!(dir.is_dir(), "ACOS_BOOT_JARS is not a directory: {dir:?}");
    let jars = jars_in(&dir);
    assert!(!jars.is_empty(), "no .jar files in {dir:?}");

    let tmp = std::env::temp_dir().join(format!("acos-boot-dex-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("create temp dir");

    let mut files = 0u64;
    let mut classes = 0u64;
    let mut methods = 0u64;
    let mut instructions = 0u64;
    let mut signature_mismatches = 0u64;
    let mut hiddenapi_files = 0u64;
    let mut failures = Vec::new();

    for jar in &jars {
        for (name, bytes) in extract_dex(jar) {
            let tmpf = tmp.join(format!(
                "{}-{}",
                jar.file_stem().unwrap_or_default().to_string_lossy(),
                name
            ));
            std::fs::write(&tmpf, &bytes).expect("write temp dex");

            match DexFile::parse(bytes.as_slice()) {
                Ok(dex) => {
                    let sig = dex.signature_valid();
                    if !sig {
                        signature_mismatches += 1;
                    }
                    let has_hiddenapi = dex.hiddenapi_class_data().is_some();
                    if has_hiddenapi {
                        hiddenapi_files += 1;
                    }
                    match dex.verify() {
                        Ok(r) => {
                            files += 1;
                            classes += r.classes;
                            methods += r.methods_with_code;
                            instructions += r.instructions;
                            eprintln!(
                                "ok {:<28} {:<12} {:>9} bytes classes={:<5} methods={:<6} instructions={:<7} sig_valid={} hiddenapi={}",
                                jar.file_name().unwrap_or_default().to_string_lossy(),
                                name,
                                bytes.len(),
                                r.classes,
                                r.methods_with_code,
                                r.instructions,
                                sig,
                                has_hiddenapi,
                            );
                        }
                        Err(e) => failures.push(format!(
                            "verify {}!{name}: {} {e}",
                            jar.display(),
                            e.code()
                        )),
                    }
                }
                Err(e) => failures.push(format!(
                    "parse {}!{name}: {} {e}",
                    jar.display(),
                    e.code()
                )),
            }
            let _ = std::fs::remove_file(&tmpf);
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);

    eprintln!(
        "boot totals: files={files} classes={classes} methods={methods} instructions={instructions} \
         signature_mismatches_accepted={signature_mismatches} hiddenapi_files={hiddenapi_files}"
    );
    assert!(files > 0, "no boot dex files were parsed");
    assert!(
        failures.is_empty(),
        "boot classpath failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn boot_classpath_dexdump_differential() {
    let Some(dir) = boot_jars_dir() else {
        eprintln!("ACOS_BOOT_JARS not set; skipping boot dexdump differential");
        return;
    };
    let Some(bin) = example_binary() else {
        eprintln!("dexdump example binary not found; skipping boot differential");
        return;
    };

    let tmp = std::env::temp_dir().join(format!("acos-boot-diff-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("create temp dir");

    let targets = [
        ("framework.jar", "classes.dex"),
        ("framework.jar", "classes3.dex"),
        ("core-oj.jar", "classes.dex"),
    ];
    let mut paths = Vec::new();
    for (jar_name, dex_name) in targets {
        let jar = dir.join(jar_name);
        for (name, bytes) in extract_dex(&jar) {
            if name == dex_name {
                let p = tmp.join(format!("{jar_name}.{name}"));
                std::fs::write(&p, &bytes).expect("write temp dex");
                paths.push(p);
            }
        }
    }
    assert_eq!(paths.len(), 3, "expected the three named boot dex files");

    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/diff_dexdump.sh");
    let output = std::process::Command::new("bash")
        .arg(&script)
        .args(&paths)
        .env("DEXDUMP_BIN", &bin)
        .output()
        .expect("run diff_dexdump.sh");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("{stdout}");
    if !stderr.is_empty() {
        eprintln!("[diff stderr] {stderr}");
    }
    let _ = std::fs::remove_dir_all(&tmp);

    let mismatches: u64 = stdout
        .lines()
        .filter_map(|l| l.split("mismatches=").nth(1))
        .filter_map(|s| s.trim().parse().ok())
        .next()
        .unwrap_or(u64::MAX);
    assert_eq!(mismatches, 0, "boot dexdump differential mismatches: {mismatches}");
}
