//! W157-A: run the RN legacy (Paper) bridge inside an `acos-runtime` session.
//!
//! W143-A/W153-A ran the same `BridgeProbe` on a raw `Vm` and had to override
//! `ChoreographerProvider` (virtual clock) and `PlatformConstants`
//! (`uiMode="normal"` shim) because the raw VM installs no service registry.
//! `acos-runtime` installs the real `ServiceHost` + Binder transport
//! (`display`, ...), so the *real* `android.view.Choreographer`,
//! `AndroidChoreographerProvider`, `ReactChoreographer` and the real
//! `AndroidInfoModule` (whose `uiMode()` reads the `uimode` service) run here.
//!
//! The probe prints `ACOS_RN_BRIDGE {...}` stages and `ACOS_RN_OP {...}` ops.
//! After the first mount batch it dispatches a synthetic tap through RN's real
//! event path (`UIManagerModule.getEventDispatcher()` + `TouchEvent`), marked by
//! `ACOS_RN_TAP_START {...}`, and the parent records the second batch.
//!
//! Evidence: `results/rn/w157a-ops.json`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_jni::{JniConfig, JniRuntime};
use acos_runtime::Runtime;
use acos_vm::{CoreLib, VmOptions};

const CHILD_ENV: &str = "ACOS_RN_W157A_CHILD";
const LIBDIR_ENV: &str = "ACOS_RN_W157A_LIBDIR";
const OUT_ENV: &str = "ACOS_RN_W157A_OUT";
const APK_ENV: &str = "ACOS_RN_W157A_APK";

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

fn ensure_libs(apk: &Path, libdir: &Path) -> Result<(), String> {
    if ORDER.iter().all(|name| libdir.join(name).is_file()) {
        return Ok(());
    }
    std::fs::create_dir_all(libdir).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(apk).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
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

fn extract_asset(apk: &Path, entry: &str, dest: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(apk).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    archive
        .by_name(entry)
        .map_err(|e| format!("{entry}: {e}"))?
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    std::fs::write(dest, &data).map_err(|e| e.to_string())?;
    Ok(data.len())
}

#[test]
fn rn_runtime_child_worker() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let libdir = PathBuf::from(std::env::var(LIBDIR_ENV).expect("libdir"));
    let scratch = PathBuf::from(std::env::var(OUT_ENV).expect("scratch"));
    let apk = PathBuf::from(std::env::var(APK_ENV).expect("apk"));
    std::fs::create_dir_all(&scratch).expect("scratch dir");
    let libdir = {
        let dst = scratch.join("lib");
        std::fs::create_dir_all(&dst).expect("scratch lib dir");
        for entry in std::fs::read_dir(&libdir).expect("read fixture lib dir") {
            let entry = entry.expect("lib entry");
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                std::fs::copy(entry.path(), dst.join(entry.file_name())).expect("copy lib");
            }
        }
        dst
    };
    let apk = {
        let dst = scratch.join("rn-hello.apk");
        std::fs::copy(&apk, &dst).expect("copy apk");
        dst
    };

    // Stage the guest-visible tree inside the VM sandbox VFS root (see
    // `rn_bridge.rs` for the canonical explanation).
    let session_root = scratch.join("session");
    let guest_app_dir = session_root.join("data/user/0/dev.acos.fixtures.rnhello");
    let guest_lib_dir = guest_app_dir.join("lib");
    std::fs::create_dir_all(&guest_lib_dir).expect("guest lib dir");
    for entry in std::fs::read_dir(&libdir).expect("read scratch lib dir") {
        let entry = entry.expect("lib entry");
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            std::fs::copy(entry.path(), guest_lib_dir.join(entry.file_name()))
                .expect("stage guest lib");
        }
    }
    let guest_apk = guest_app_dir.join("base.apk");
    std::fs::copy(&apk, &guest_apk).expect("stage guest apk");
    let guest_lib_dir_str = "/data/user/0/dev.acos.fixtures.rnhello/lib".to_string();
    let guest_apk_str = "/data/user/0/dev.acos.fixtures.rnhello/base.apk".to_string();

    let apk_dir = apk.parent().unwrap_or_else(|| Path::new("."));
    let data_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let boot_dir = repo_root().join(".cache/boot/android-34");
    let libdir_real = std::fs::canonicalize(&libdir).unwrap_or_else(|_| libdir.clone());
    let apk_dir_real = std::fs::canonicalize(apk_dir).unwrap_or_else(|_| apk_dir.to_path_buf());
    let mut profile = acos_sandbox::ConfineProfile::default()
        .with_read_only(&libdir)
        .with_read_only(&libdir_real)
        .with_read_only(apk_dir)
        .with_read_only(&apk_dir_real)
        .with_read_only(&data_dir);
    for base in [&libdir, apk_dir, &libdir_real, &apk_dir_real] {
        let mut cur = base.parent();
        while let Some(p) = cur {
            if p.exists() {
                profile = profile.with_read_only(p);
            }
            cur = p.parent();
        }
    }
    for dev in ["/dev/urandom", "/dev/null"] {
        if Path::new(dev).exists() {
            profile = profile.with_read_only(dev);
        }
    }
    if let Some(parent) = scratch.parent() {
        if parent.exists() {
            profile = profile.with_read_only(parent);
        }
    }
    profile = profile.with_read_write(&scratch);
    if boot_dir.join("BOOTCLASSPATH").is_file() {
        profile = profile.with_read_only(&boot_dir);
    }
    let _ = std::fs::create_dir_all(session_root.join("data/user/0"));
    profile = profile.with_read_write(&session_root);
    profile.env.push((
        "ACOS_SESSION_ROOT".to_string(),
        session_root.display().to_string(),
    ));
    match acos_sandbox::confine(&profile) {
        Ok(report) => println!(
            "W157A_STAGE {}",
            serde_json::json!({"confined": true, "report": report})
        ),
        Err(e) => {
            println!(
                "W157A_SUMMARY {}",
                serde_json::json!({"confined": false, "error": e.to_string()})
            );
            return;
        }
    }

    let boot_ok = boot_dir.join("BOOTCLASSPATH").is_file();
    let mut rt = match Runtime::standard_with(VmOptions {
        heap_max_bytes: 1024 * 1024 * 1024,
        corelib: if boot_ok {
            CoreLib::Real
        } else {
            CoreLib::Bootstrap
        },
        boot_dir: if boot_ok {
            Some(boot_dir.clone())
        } else {
            None
        },
        ..VmOptions::default()
    }) {
        Ok(rt) => rt,
        Err(e) => {
            println!(
                "W157A_SUMMARY {}",
                serde_json::json!({"confined": true, "error": format!("Runtime::standard_with: {e:?}")})
            );
            return;
        }
    };

    if boot_ok {
        if let Err(e) = rt.vm_mut().load_class("Landroid/content/res/AssetManager;") {
            println!(
                "W157A_STAGE {}",
                serde_json::json!({"phase": "assetmanager-preload-error", "detail": format!("{e:?}")})
            );
        }
    }

    // The APK's DEX provides the RN classes; the probe DEX provides the entry.
    let apk_bytes = std::fs::read(&apk).expect("read apk");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(apk_bytes)).expect("zip apk");
    let mut names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| n.starts_with("classes") && n.ends_with(".dex"))
        .collect();
    names.sort();
    for name in &names {
        let mut data = Vec::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        let dex = DexFile::parse(data).expect("parse apk dex");
        rt.vm_mut().add_dex(dex);
    }
    println!(
        "W157A_STAGE {}",
        serde_json::json!({"phase": "apk-dex", "classes_added": names.len()})
    );

    let probe_dex = repo_root().join("crates/acos-jni/tests/data/rn-bridge-probe.dex");
    match std::fs::read(&probe_dex)
        .map_err(|e| e.to_string())
        .and_then(|b| DexFile::parse(b).map_err(|e| format!("{e:?}")))
    {
        Ok(dex) => {
            rt.vm_mut().add_dex(dex);
            println!(
                "W157A_STAGE {}",
                serde_json::json!({"phase": "probe-dex", "path": probe_dex.display().to_string()})
            );
        }
        Err(e) => {
            println!(
                "W157A_SUMMARY {}",
                serde_json::json!({"confined": true, "error": format!("probe dex: {e}")})
            );
            return;
        }
    }

    let cfg = JniConfig::default()
        .with_search_path(&libdir)
        .with_apk(&apk);
    let jni = JniRuntime::new(cfg);
    if let Err(e) = jni.attach(rt.vm_mut()) {
        println!(
            "W157A_SUMMARY {}",
            serde_json::json!({"confined": true, "error": format!("attach: {e}")})
        );
        return;
    }
    jni.bind(rt.vm_mut());

    let bundle_path = scratch.join("index.android.bundle");
    match extract_asset(&apk, "assets/index.android.bundle", &bundle_path) {
        Ok(n) => println!(
            "W157A_STAGE {}",
            serde_json::json!({"phase": "bundle", "bytes": n, "path": bundle_path.display().to_string()})
        ),
        Err(e) => println!(
            "W157A_STAGE {}",
            serde_json::json!({"phase": "bundle-error", "detail": e})
        ),
    }

    let args = vec![
        bundle_path.display().to_string(),
        guest_lib_dir_str.clone(),
        guest_apk_str.clone(),
        apk.display().to_string(),
        "real".to_string(),
        "tap".to_string(),
    ];
    let outcome = rt
        .vm_mut()
        .run_virtual("dev/acos/fixtures/rnhello/BridgeProbe", &args);
    for line in outcome.stdout.lines() {
        println!("W157A_PROBE {line}");
    }
    for line in outcome.stderr.lines() {
        println!("W157A_PROBE_ERR {line}");
    }
    if let Some(acos_vm::VmError::JavaException(h)) = outcome.vm_error {
        let class = rt
            .vm()
            .class_of_object(h)
            .map(|id| rt.vm().class_desc_of_id(id))
            .unwrap_or_else(|| "<dead>".into());
        println!(
            "W157A_STAGE {}",
            serde_json::json!({"phase": "uncaught", "class": class, "message": rt.vm().throwable_message(h)})
        );
    } else if let Some(e) = &outcome.vm_error {
        println!(
            "W157A_STAGE {}",
            serde_json::json!({"phase": "vm-error", "error": format!("{e:?}")})
        );
    }
    println!(
        "W157A_SUMMARY {}",
        serde_json::json!({
            "confined": true,
            "exit_code": outcome.exit_code,
            "registered_natives": jni.registered_natives().len(),
            "error": jni.take_error().map(|e| e.to_string()),
        })
    );
}

#[test]
fn rn_bridge_runs_on_a_real_runtime_session_and_taps_the_button() {
    if !acos_test_support::have_fixture("fixtures/build/apps/rn-hello.apk") {
        eprintln!(
            "SKIP rn_bridge_runs_on_a_real_runtime_session_and_taps_the_button: \
             fixtures/build/apps/rn-hello.apk is absent (build with tools/build-rn-hello.sh)"
        );
        return;
    }
    let apk = repo_root().join("fixtures/build/apps/rn-hello.apk");
    let libdir = default_lib_dir();
    if let Err(e) = ensure_libs(&apk, &libdir) {
        panic!(
            "could not extract the fixture x86_64 libs to {}: {e}",
            libdir.display()
        );
    }

    let scratch = std::env::temp_dir().join(format!("acos-rn-w157a-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch dir");

    let output = Command::new(std::env::current_exe().expect("current exe"))
        .args(["--exact", "rn_runtime_child_worker", "--nocapture"])
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
        .filter(|l| l.starts_with("W157A_") || l.starts_with("ACOS_"))
    {
        eprintln!("child> {line}");
    }

    let stage_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| {
            l.starts_with("W157A_STAGE ")
                || l.starts_with("W157A_PROBE ")
                || l.starts_with("ACOS_RN_")
        })
        .collect();

    // Split the captured `ACOS_RN_OP` lines at the `ACOS_RN_TAP_START` marker:
    // everything before it is the initial mount batch, everything after is the
    // tap-driven re-render batch.
    let mut first_batch: Vec<String> = Vec::new();
    let mut tap_batch: Vec<String> = Vec::new();
    let mut tap_marker: Option<String> = None;
    let mut seen_tap = false;
    for line in stdout.lines() {
        if let Some((_, rest)) = line.split_once("ACOS_RN_TAP_START ") {
            seen_tap = true;
            tap_marker = Some(rest.trim().to_string());
        }
        if let Some((_, rest)) = line.split_once("ACOS_RN_OP ") {
            if seen_tap {
                tap_batch.push(rest.trim().to_string());
            } else {
                first_batch.push(rest.trim().to_string());
            }
        }
    }

    let summary_text = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("W157A_SUMMARY "))
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

    let out_dir = acos_test_support::test_artifacts_dir("rn");
    let _ = std::fs::create_dir_all(&out_dir);
    let _ = std::fs::write(
        out_dir.join("w157a-ops.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "summary": summary,
            "stages": stage_lines,
            "first_batch": first_batch,
            "tap_marker": tap_marker,
            "tap_batch": tap_batch,
        }))
        .unwrap_or_default(),
    );
    let _ = std::fs::remove_dir_all(&scratch);

    assert_eq!(
        summary["confined"], true,
        "child did not report active confinement (fail closed): {summary}\nstderr:\n{stderr}"
    );
    assert!(
        first_batch.len() >= 19,
        "expected the initial UIManager mount batch (>=19 ops), got {}:\n{first_batch:#?}\nstderr:\n{stderr}",
        first_batch.len()
    );
    // The real `AndroidChoreographerProvider` must be the one in use.
    assert!(
        stage_lines.iter().any(|l| l.contains("ok-real-provider")),
        "the real Choreographer provider was not installed:\n{stage_lines:#?}"
    );
    // The real `AndroidInfoModule` must have resolved the `uimode` service.
    assert!(
        stage_lines
            .iter()
            .any(|l| l.contains("\"phase\":\"uimode-service\"") && l.contains("binder")),
        "the real UiModeManager service was not reached:\n{stage_lines:#?}"
    );
    // W157-A goal: the tap-driven batch shows the count text moving to 1.
    assert!(
        tap_batch
            .iter()
            .any(|op| op.contains("count: 1")),
        "the tap batch did not show `count: 1`:\nmarker={tap_marker:?}\ntap_batch={tap_batch:#?}\nstderr:\n{stderr}"
    );
}
