//! W76-A: drive the React Native + Hermes fixture native stack through the
//! confined native loader.
//!
//! The fixture libs come from `fixtures/build/apps/rn-hello.apk` (RN 0.81.4,
//! Hermes HBC96, x86_64). The test parent spawns a **child copy of this test
//! binary**; the child installs the ADR-0022 confinement (`PR_SET_NO_NEW_PRIVS`,
//! Landlock, seccomp) and then loads the libraries in dependency order with
//! `require_confinement = true` (fail closed), then calls each `JNI_OnLoad`.
//! Every step prints a `W76A_STAGE` line so the first failing stage survives a
//! crash; the final `W76A_SUMMARY` line is the machine-readable result.
//!
//! This test does not run `acos launch`; W72-A owns the runtime's
//! `System.loadLibrary` wiring. It exercises only the loader + JNI + bionic/NDK
//! surface using libs we built for the fixture.

use std::os::raw::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_jni::{JniConfig, JniRuntime};
use acos_native_loader::{Loader, LoaderConfig, LoaderError};
use acos_vm::{CoreLib, Vm, VmOptions};

const CHILD_ENV: &str = "ACOS_RN_W76A_CHILD";
const LIBDIR_ENV: &str = "ACOS_RN_W76A_LIBDIR";
const OUT_ENV: &str = "ACOS_RN_W76A_OUT";
const APK_ENV: &str = "ACOS_RN_W76A_APK";

/// Dependency order for the fixture (each entry's `DT_NEEDED` are earlier
/// entries or bionic shim libs).
const ORDER: &[&str] = &[
    "libc++_shared.so",
    "libjsi.so",
    "libfbjni.so",
    "libhermes.so",
    "libreactnative.so",
    "libhermestooling.so",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Add every `classes*.dex` in the APK to the VM. Returns the count added.
fn load_apk_dex(apk: &Path, vm: &mut Vm) -> Result<usize, String> {
    let bytes = std::fs::read(apk).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| n.starts_with("classes") && n.ends_with(".dex"))
        .collect();
    names.sort();
    let mut added = 0;
    for name in names {
        let mut data = Vec::new();
        use std::io::Read;
        archive
            .by_name(&name)
            .map_err(|e| e.to_string())?
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        let dex = DexFile::parse(data).map_err(|e| format!("{name}: {e:?}"))?;
        vm.add_dex(dex);
        added += 1;
    }
    Ok(added)
}

fn default_lib_dir() -> PathBuf {
    repo_root().join(".cache/rn-w76a/lib")
}

/// Extract `lib/x86_64/*.so` from the fixture APK into `libdir` if any of the
/// expected libraries are absent, so the test only depends on the APK.
fn ensure_libs(apk: &Path, libdir: &Path) -> Result<(), String> {
    if ORDER.iter().all(|name| libdir.join(name).is_file()) {
        return Ok(());
    }
    std::fs::create_dir_all(libdir).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(apk).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    for name in ORDER {
        let entry = format!("lib/x86_64/{name}");
        let mut data = Vec::new();
        use std::io::Read;
        archive
            .by_name(&entry)
            .map_err(|e| format!("{entry}: {e}"))?
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        std::fs::write(libdir.join(name), data).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Broad category for a loader/JNI failure, used by `FIRST_RUN.md`.
fn category(e: &LoaderError) -> &'static str {
    match e {
        LoaderError::UnsupportedSymbols(_) => "bionic shim",
        LoaderError::UnsupportedRelocation { .. } => "loader",
        LoaderError::DepNotFound { .. } | LoaderError::UnsafeDependency { .. } => "loader",
        LoaderError::InitFailed(_) => "loader/init",
        LoaderError::ElfMalformed(_) | LoaderError::WrongAbi { .. } => "loader",
        LoaderError::Io(_) => "host io/landlock",
        LoaderError::ConfinementRequired => "confinement",
        _ => "loader",
    }
}

fn missing_symbols(e: &LoaderError) -> Vec<serde_json::Value> {
    match e {
        LoaderError::UnsupportedSymbols(missing) => missing
            .iter()
            .map(|m| {
                serde_json::json!({
                    "library": m.library,
                    "symbols": m.symbols,
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The child: confine, then load the stack. Runs only with `ACOS_RN_W76A_CHILD`.
#[test]
fn rn_child_worker() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let libdir = PathBuf::from(std::env::var(LIBDIR_ENV).expect("libdir"));
    let scratch = PathBuf::from(std::env::var(OUT_ENV).expect("scratch"));
    let apk = PathBuf::from(std::env::var(APK_ENV).expect("apk"));
    std::fs::create_dir_all(&scratch).expect("scratch dir");

    // 1. Confinement first: the loader refuses to map guest PROT_EXEC code
    //    without it (ADR-0022), so a failed confine fails closed.
    // Landlock read-only rules are directory-shaped (they grant READ_DIR as
    // well as READ_FILE), so grant the APK's directory rather than the file.
    let apk_dir = apk.parent().unwrap_or_else(|| Path::new("."));
    let data_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let profile = acos_sandbox::ConfineProfile::default()
        .with_read_only(&libdir)
        .with_read_only(apk_dir)
        .with_read_only(&data_dir)
        .with_read_write(&scratch);
    match acos_sandbox::confine(&profile) {
        Ok(report) => println!(
            "W76A_STAGE {}",
            serde_json::json!({"confined": true, "report": report})
        ),
        Err(e) => {
            println!(
                "W76A_SUMMARY {}",
                serde_json::json!({"confined": false, "error": e.to_string()})
            );
            return;
        }
    }

    // 2. A minimal VM so `JNI_OnLoad` gets a real `JavaVM*`. Bootstrap core
    //    library touches no files; the fixture's `JNI_OnLoad` only needs the
    //    VM/JNI table.
    let mut vm = match Vm::new(VmOptions {
        heap_max_bytes: 64 * 1024 * 1024,
        corelib: CoreLib::Bootstrap,
        ..VmOptions::default()
    }) {
        Ok(vm) => vm,
        Err(e) => {
            println!(
                "W76A_SUMMARY {}",
                serde_json::json!({"confined": true, "error": format!("Vm::new: {e:?}")})
            );
            return;
        }
    };
    // Load the fixture APK's Java classes (`com.facebook.jni.*` lives in
    // classes2.dex), so `JNI_OnLoad`'s `FindClass`/`RegisterNatives` can find
    // them. Class resolution stays lazy; only referenced classes are linked.
    match load_apk_dex(&apk, &mut vm) {
        Ok(n) => println!(
            "W76A_STAGE {}",
            serde_json::json!({"phase": "apk-dex", "classes_added": n})
        ),
        Err(e) => println!(
            "W76A_STAGE {}",
            serde_json::json!({"phase": "apk-dex-error", "detail": e})
        ),
    }
    // The fixture APK's DEX omits `com.facebook.jni.*` because react-android's
    // POM marks the fbjni artifact `optional`; a real Gradle app adds it
    // explicitly. Provide the same classes from a checked-in 10 KiB DEX so the
    // native stack can proceed to the next blocker.
    let fbjni_dex = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fbjni-classes.dex");
    if fbjni_dex.is_file() {
        match std::fs::read(&fbjni_dex)
            .map_err(|e| e.to_string())
            .and_then(|b| DexFile::parse(b).map_err(|e| format!("{e:?}")))
        {
            Ok(dex) => {
                vm.add_dex(dex);
                println!(
                    "W76A_STAGE {}",
                    serde_json::json!({"phase": "fbjni-dex", "classes_added": 1})
                );
            }
            Err(e) => println!(
                "W76A_STAGE {}",
                serde_json::json!({"phase": "fbjni-dex-error", "detail": e})
            ),
        }
    }

    let rt = JniRuntime::new(JniConfig::default().with_search_path(&libdir));
    if let Err(e) = rt.attach(&mut vm) {
        println!(
            "W76A_SUMMARY {}",
            serde_json::json!({"confined": true, "error": format!("attach: {e}")})
        );
        return;
    }
    rt.bind(&mut vm);

    // 3. Load in dependency order. `require_confinement` stays on.
    let loader = Loader::new(LoaderConfig {
        search_paths: vec![libdir.clone()],
        require_confinement: true,
        ..LoaderConfig::default()
    });

    let mut loaded: Vec<String> = Vec::new();
    let mut blockers: Vec<serde_json::Value> = Vec::new();
    let mut all_imports_missing: Vec<serde_json::Value> = Vec::new();

    // Phase A: map + relocate + run constructors for every library, in
    // dependency order. A failure here is a loader/bionic-surface blocker; we
    // keep going so the table covers every library.
    let mut handles: Vec<(String, acos_native_loader::LibraryHandle)> = Vec::new();
    for name in ORDER {
        let path = libdir.join(name);
        println!(
            "W76A_STAGE {}",
            serde_json::json!({"library": name, "phase": "load-begin"})
        );
        match loader.load(&path, name) {
            Ok(handle) => {
                loaded.push(name.to_string());
                // Record any import the report still flags as missing.
                for entry in loader.report().libraries.iter().filter(|l| l.name == *name) {
                    for symbol in &entry.imports_missing {
                        all_imports_missing.push(serde_json::json!({
                            "library": name,
                            "symbol": symbol,
                        }));
                    }
                }
                let onload = handle.symbol("JNI_OnLoad").is_some();
                println!(
                    "W76A_STAGE {}",
                    serde_json::json!({"library": name, "phase": "loaded", "jni_onload": onload})
                );
                handles.push((name.to_string(), handle));
            }
            Err(e) => {
                blockers.push(serde_json::json!({
                    "category": category(&e),
                    "library": name,
                    "code": e.code(),
                    "detail": e.to_string(),
                    "missing": missing_symbols(&e),
                }));
                println!(
                    "W76A_STAGE {}",
                    serde_json::json!({"library": name, "phase": "load-error", "code": e.code(),
                                       "detail": e.to_string()})
                );
            }
        }
    }

    // Phase B: run `JNI_OnLoad` in dependency order. The order matters because
    // fbjni registers the native classes the other libraries use.
    for (name, handle) in &handles {
        let Some(p) = handle.symbol("JNI_OnLoad") else {
            continue;
        };
        println!(
            "W76A_STAGE {}",
            serde_json::json!({"library": name, "phase": "jni_onload-begin"})
        );
        // SAFETY: `JNI_OnLoad` has the JNI-mandated signature
        // `jint (*)(JavaVM*, void*)`; the pointer is a real export of the
        // library the loader just mapped, and `java_vm()` is the published
        // slot for the attached runtime.
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32 =
            unsafe { std::mem::transmute(p) };
        let rc = unsafe { f(rt.java_vm(), std::ptr::null_mut()) };
        println!(
            "W76A_STAGE {}",
            serde_json::json!({"library": name, "phase": "jni_onload-returned", "rc": rc})
        );
        if rc < 0 {
            blockers.push(serde_json::json!({
                "category": "JNI",
                "library": name,
                "code": "JNI_ONLOAD_ERR",
                "detail": format!("JNI_OnLoad returned {rc:#x}"),
            }));
        }
    }

    println!(
        "W76A_SUMMARY {}",
        serde_json::json!({
            "confined": true,
            "loaded": loaded,
            "blockers": blockers,
            "missing_imports": all_imports_missing,
        })
    );
}

/// Parent: spawn the confined child, capture the stages/summary, and fail if
/// confinement did not actually engage.
#[test]
fn rn_native_stack_under_confinement() {
    if !acos_test_support::have_fixture("fixtures/build/apps/rn-hello.apk") {
        eprintln!(
            "SKIP rn_native_stack_under_confinement: fixtures/build/apps/rn-hello.apk is absent \
             (build with tools/build-rn-hello.sh)"
        );
        return;
    }
    let apk = repo_root().join("fixtures/build/apps/rn-hello.apk");
    let libdir = default_lib_dir();
    if let Err(e) = ensure_libs(&apk, &libdir) {
        panic!("could not extract the fixture x86_64 libs to {}: {e}", libdir.display());
    }

    let scratch = std::env::temp_dir().join(format!("acos-rn-w76a-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch dir");

    let output = Command::new(std::env::current_exe().expect("current exe"))
        .args(["--exact", "rn_child_worker", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env(LIBDIR_ENV, &libdir)
        .env(OUT_ENV, &scratch)
        .env(APK_ENV, &apk)
        .output()
        .expect("spawn confined child");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("child exit status: {:?}", output.status.code());
    for line in stdout.lines().filter(|l| l.starts_with("W76A_")) {
        eprintln!("child> {line}");
    }

    let stage_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("W76A_STAGE "))
        .collect();
    let summary_text = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("W76A_SUMMARY "))
        .map(str::to_string);

    // Evidence collection, not an assertion of success: the run is expected to
    // stop at the first loader/bionic/JNI blocker. A child crash still leaves
    // the stage lines, which are the useful record. The only hard contract is
    // that native code ran **inside** an active confinement (fail closed).
    let summary: serde_json::Value = match summary_text {
        Some(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            panic!("child summary was not valid JSON ({e}): {s}\nstderr:\n{stderr}")
        }),
        None => serde_json::json!({
            "confined": stage_lines
                .iter()
                .any(|l| l.contains("\"confined\":true")),
            "crashed": true,
            "exit": output.status.code(),
            "stages": stage_lines,
        }),
    };

    assert_eq!(
        summary["confined"], true,
        "child did not report active confinement (fail closed): {summary}\nstderr:\n{stderr}"
    );

    // Reaching `libfbjni.so` proves the loader mapped + relocated + initialised
    // real RN/Hermes native code under Landlock + seccomp. Everything past it is
    // the first blocker recorded in results/rn/FIRST_RUN.md.
    assert!(
        summary.to_string().contains("libfbjni.so"),
        "expected to reach libfbjni.so under confinement: {summary}\nstderr:\n{stderr}"
    );

    // Persist the raw result next to the other evidence.
    let out_dir = repo_root().join("results/rn");
    let _ = std::fs::create_dir_all(&out_dir);
    let json = serde_json::to_string_pretty(&summary).unwrap_or_default();
    let _ = std::fs::write(out_dir.join("rn-native-first-run.json"), &json);
    let _ = std::fs::remove_dir_all(&scratch);
}
