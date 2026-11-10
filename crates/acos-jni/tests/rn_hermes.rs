//! W84-A: drive the React Native + Hermes fixture through the **runtime**
//! `System.loadLibrary` path and the SoLoader-style merged-native initialisers.
//!
//! `rn_native_stack.rs` proves the raw loader maps and relocates the six
//! `rn-hello` `.so`s. This test goes one step further: it runs real Java (the
//! `HermesProbe` driver dex, `crates/acos-jni/tests/data/rn-hermes-probe.dex`)
//! inside the VM and lets it call
//!
//! 1. `System.loadLibrary("hermes" | "reactnative" | ...)` — the same
//!    `Runtime.nativeLoad` seam `SoLoader` uses — so `JNI_OnLoad` runs from
//!    Java, and
//! 2. `OpenSourceMergedSoMapping.INSTANCE.lib*_so()` — the merged per-library
//!    initialisers that `SoLoader.invokeJniOnload` calls, which turn on the
//!    original `JNI_OnLoad`s folded into `libreactnative`/`libhermestooling`.
//!
//! The parent spawns a confined child copy of the test binary; the child
//! installs the ADR-0022 confinement before any guest code, then runs the
//! probe. Every stage is printed as `ACOS_RN_PROBE {...}` so the first failing
//! stage survives a crash, and the Rust side records blockers in
//! `results/rn/W84A_HERMES.md`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_jni::{JniConfig, JniRuntime};
use acos_vm::{CoreLib, Vm, VmOptions};

const CHILD_ENV: &str = "ACOS_RN_W84A_CHILD";
const LIBDIR_ENV: &str = "ACOS_RN_W84A_LIBDIR";
const OUT_ENV: &str = "ACOS_RN_W84A_OUT";
const APK_ENV: &str = "ACOS_RN_W84A_APK";

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

fn default_lib_dir() -> PathBuf {
    repo_root().join(".cache/rn-w76a/lib")
}

/// Extract `lib/x86_64/*.so` from the fixture APK into `libdir` if absent.
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
        archive
            .by_name(&entry)
            .map_err(|e| format!("{entry}: {e}"))?
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        std::fs::write(libdir.join(name), data).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Extract one zip entry from the APK to `dest`; returns the byte count.
fn extract_asset(apk: &Path, entry: &str, dest: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(apk).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    archive
        .by_name(entry)
        .map_err(|e| format!("{entry}: {e}"))?
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    std::fs::write(dest, &data).map_err(|e| e.to_string())?;
    Ok(data.len())
}

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

/// The child: confine, then run the Java probe through the real VM.
#[test]
fn rn_hermes_child_worker() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let libdir = PathBuf::from(std::env::var(LIBDIR_ENV).expect("libdir"));
    let scratch = PathBuf::from(std::env::var(OUT_ENV).expect("scratch"));
    let apk = PathBuf::from(std::env::var(APK_ENV).expect("apk"));
    std::fs::create_dir_all(&scratch).expect("scratch dir");

    let apk_dir = apk.parent().unwrap_or_else(|| Path::new("."));
    let data_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let probe_lib_dir = repo_root().join("fixtures/build/rn-hermes/x86_64");
    let boot_dir = repo_root().join(".cache/boot/android-34");
    let mut profile = acos_sandbox::ConfineProfile::default()
        .with_read_only(&libdir)
        .with_read_only(apk_dir)
        .with_read_only(&data_dir);
    if probe_lib_dir.is_dir() {
        profile = profile.with_read_only(&probe_lib_dir);
    }
    // Hermes' `std::random_device` opens `/dev/urandom` while constructing the
    // runtime (via libc++); without a read rule Landlock fails the open and
    // `makeHermesRuntime` throws `random_device failed to open /dev/urandom`.
    // Grant **only** the two device nodes that are needed (W94-A #8) — a
    // directory rule on all of `/dev` would also expose `/dev/kmsg`,
    // `/dev/input/*`, `/dev/dri/*`, ... to the guest wherever the uid permits.
    // This is test-harness-only; the production `native_launch` profile does not
    // grant `/dev` at all.
    for dev in ["/dev/urandom", "/dev/null"] {
        if Path::new(dev).exists() {
            profile = profile.with_read_only(dev);
        }
    }
    profile = profile.with_read_write(&scratch);
    if boot_dir.join("BOOTCLASSPATH").is_file() {
        profile = profile.with_read_only(&boot_dir);
    }
    match acos_sandbox::confine(&profile) {
        Ok(report) => println!(
            "W84A_STAGE {}",
            serde_json::json!({"confined": true, "report": report})
        ),
        Err(e) => {
            println!(
                "W84A_SUMMARY {}",
                serde_json::json!({"confined": false, "error": e.to_string()})
            );
            return;
        }
    }

    let boot_ok = boot_dir.join("BOOTCLASSPATH").is_file();
    let corelib = if boot_ok { CoreLib::Real } else { CoreLib::Bootstrap };
    let mut vm = match Vm::new(VmOptions {
        heap_max_bytes: 512 * 1024 * 1024,
        corelib,
        boot_dir: if boot_ok { Some(boot_dir.clone()) } else { None },
        ..VmOptions::default()
    }) {
        Ok(vm) => vm,
        Err(e) => {
            println!(
                "W84A_SUMMARY {}",
                serde_json::json!({"confined": true, "error": format!("Vm::new: {e:?}")})
            );
            return;
        }
    };
    match load_apk_dex(&apk, &mut vm) {
        Ok(n) => println!(
            "W84A_STAGE {}",
            serde_json::json!({"phase": "apk-dex", "classes_added": n})
        ),
        Err(e) => println!(
            "W84A_STAGE {}",
            serde_json::json!({"phase": "apk-dex-error", "detail": e})
        ),
    }

    // The fixture's Hermes bytecode, extracted into the writable scratch dir.
    let bundle_path = scratch.join("index.android.bundle");
    match extract_asset(&apk, "assets/index.android.bundle", &bundle_path) {
        Ok(n) => println!(
            "W84A_STAGE {}",
            serde_json::json!({"phase": "bundle", "bytes": n, "path": bundle_path.display().to_string()})
        ),
        Err(e) => println!(
            "W84A_STAGE {}",
            serde_json::json!({"phase": "bundle-error", "detail": e})
        ),
    }

    let probe_dex = data_dir.join("rn-hermes-probe.dex");
    match std::fs::read(&probe_dex)
        .map_err(|e| e.to_string())
        .and_then(|b| DexFile::parse(b).map_err(|e| format!("{e:?}")))
    {
        Ok(dex) => {
            vm.add_dex(dex);
            println!(
                "W84A_STAGE {}",
                serde_json::json!({"phase": "probe-dex", "path": probe_dex.display().to_string()})
            );
        }
        Err(e) => {
            println!(
                "W84A_SUMMARY {}",
                serde_json::json!({"confined": true, "error": format!("probe dex: {e}")})
            );
            return;
        }
    }

    let mut cfg = JniConfig::default()
        .with_search_path(&libdir)
        .with_apk(&apk);
    if probe_lib_dir.join("libhermesprobe.so").is_file() {
        cfg = cfg.with_search_path(&probe_lib_dir);
    }
    let rt = JniRuntime::new(cfg);
    if let Err(e) = rt.attach(&mut vm) {
        println!(
            "W84A_SUMMARY {}",
            serde_json::json!({"confined": true, "error": format!("attach: {e}")})
        );
        return;
    }
    rt.bind(&mut vm);

    let args = vec![bundle_path.display().to_string()];
    let outcome = vm.run("dev/acos/fixtures/rnhello/HermesProbe", &args);
    for line in outcome.stdout.lines() {
        println!("W84A_PROBE {line}");
    }
    for line in outcome.stderr.lines() {
        println!("W84A_PROBE_ERR {line}");
    }
    if let Some(acos_vm::VmError::JavaException(h)) = outcome.vm_error {
        let class = vm
            .class_of_object(h)
            .map(|id| vm.class_desc_of_id(id))
            .unwrap_or_else(|| "<dead>".into());
        println!(
            "W84A_STAGE {}",
            serde_json::json!({"phase": "uncaught", "class": class, "message": vm.throwable_message(h)})
        );
    } else if let Some(e) = &outcome.vm_error {
        println!(
            "W84A_STAGE {}",
            serde_json::json!({"phase": "vm-error", "error": format!("{e:?}")})
        );
    }
    let natives = rt.registered_natives().len();
    let libs: Vec<String> = rt.libraries().into_iter().map(|(n, _)| n).collect();
    println!(
        "W84A_SUMMARY {}",
        serde_json::json!({
            "confined": true,
            "exit_code": outcome.exit_code,
            "libraries": libs,
            "registered_natives": natives,
            "error": rt.take_error().map(|e| e.to_string()),
        })
    );
}

/// Parent: spawn the confined child, capture the stages/summary.
#[test]
fn rn_hermes_merged_onloads_under_confinement() {
    if !acos_test_support::have_fixture("fixtures/build/apps/rn-hello.apk") {
        eprintln!(
            "SKIP rn_hermes_merged_onloads_under_confinement: \
             fixtures/build/apps/rn-hello.apk is absent (build with tools/build-rn-hello.sh)"
        );
        return;
    }
    let apk = repo_root().join("fixtures/build/apps/rn-hello.apk");
    let libdir = default_lib_dir();
    if let Err(e) = ensure_libs(&apk, &libdir) {
        panic!("could not extract the fixture x86_64 libs to {}: {e}", libdir.display());
    }

    let scratch = std::env::temp_dir().join(format!("acos-rn-w84a-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch dir");

    let output = Command::new(std::env::current_exe().expect("current exe"))
        .args(["--exact", "rn_hermes_child_worker", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env(LIBDIR_ENV, &libdir)
        .env(OUT_ENV, &scratch)
        .env(APK_ENV, &apk)
        .output()
        .expect("spawn confined child");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("child exit status: {:?}", output.status.code());
    for line in stdout
        .lines()
        .filter(|l| l.starts_with("W84A_") || l.starts_with("ACOS_HERMES"))
    {
        eprintln!("child> {line}");
    }

    let stage_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| {
            l.starts_with("W84A_STAGE ")
                || l.starts_with("W84A_PROBE ")
                || l.starts_with("ACOS_HERMES")
        })
        .collect();
    let summary_text = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("W84A_SUMMARY "))
        .map(str::to_string);

    let summary: serde_json::Value = match summary_text {
        Some(s) => serde_json::from_str(&s)
            .unwrap_or_else(|e| panic!("bad summary ({e}): {s}\nstderr:\n{stderr}")),
        None => serde_json::json!({
            "confined": stage_lines.iter().any(|l| l.contains("\"confined\":true")),
            "crashed": true,
            "exit": output.status.code(),
            "stages": stage_lines,
        }),
    };

    assert_eq!(
        summary["confined"], true,
        "child did not report active confinement (fail closed): {summary}\nstderr:\n{stderr}"
    );

    let out_dir = repo_root().join("results/rn");
    let _ = std::fs::create_dir_all(&out_dir);
    let _ = std::fs::write(
        out_dir.join("w84a-hermes-probe.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "summary": summary,
            "stages": stage_lines,
        }))
        .unwrap_or_default(),
    );
    let _ = std::fs::remove_dir_all(&scratch);

    // Evidence collection, not a success assertion: the run is expected to stop
    // at the first Hermes/JSI blocker. Record it in results/rn/W84A_HERMES.md.
}
