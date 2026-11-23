//! W158-A: fixture-gated product e2e for the React Native session.
//!
//! The parent spawns a confined child (the same isolation the product
//! `acos serve` child uses), which runs [`RnSession`]: mount the RN bridge,
//! read the agent tree, tap the Button by `testID`, and read the tree again.
//!
//! Assertions (in the parent, from the child's structured output):
//!
//! * the mounted tree contains `Hello ACOS` and `TAP ME`;
//! * the tap resolves `hello-button` and the refreshed tree says `count: 1`;
//! * `frame()` reports `UNSUPPORTED_RN_NO_RENDERER` honestly.
//!
//! Evidence: `results/rn/w158a-product.json`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use acos_runtime::{RnSession, RnSessionOptions};
use acos_vm::CoreLib;

const CHILD_ENV: &str = "ACOS_RN_W158A_CHILD";
const LIBDIR_ENV: &str = "ACOS_RN_W158A_LIBDIR";
const OUT_ENV: &str = "ACOS_RN_W158A_OUT";
const APK_ENV: &str = "ACOS_RN_W158A_APK";
const DEX_ENV: &str = "ACOS_RN_W158A_DEX";

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

#[test]
fn rn_product_child_worker() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let libdir = PathBuf::from(std::env::var(LIBDIR_ENV).expect("libdir"));
    let scratch = PathBuf::from(std::env::var(OUT_ENV).expect("scratch"));
    let apk = PathBuf::from(std::env::var(APK_ENV).expect("apk"));
    let dex = PathBuf::from(std::env::var(DEX_ENV).expect("driver dex"));
    std::fs::create_dir_all(&scratch).expect("scratch dir");
    let boot_dir = repo_root().join(".cache/boot/android-34");
    let data_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");

    let mut profile = acos_sandbox::ConfineProfile::default()
        .with_read_write(&scratch)
        .with_read_only(&libdir)
        .with_read_only(data_dir.clone());
    if let Some(parent) = dex.parent() {
        profile = profile.with_read_only(parent);
    }
    let apk_parent = apk.parent().map(Path::to_path_buf);
    for base in [
        Some(libdir.clone()),
        Some(data_dir.clone()),
        apk_parent.clone(),
        dex.parent().map(Path::to_path_buf),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(real) = std::fs::canonicalize(&base) {
            profile = profile.with_read_only(&real);
        }
        let mut cur = base.parent();
        while let Some(p) = cur {
            if p.exists() {
                profile = profile.with_read_only(p);
            }
            cur = p.parent();
        }
    }
    if boot_dir.join("BOOTCLASSPATH").is_file() {
        profile = profile.with_read_only(&boot_dir);
    }
    for dev in ["/dev/urandom", "/dev/null"] {
        if Path::new(dev).exists() {
            profile = profile.with_read_only(dev);
        }
    }
    match acos_sandbox::confine(&profile) {
        Ok(report) => println!(
            "W158A_STAGE {}",
            serde_json::json!({"confined": true, "report": report})
        ),
        Err(e) => {
            println!(
                "W158A_SUMMARY {}",
                serde_json::json!({"confined": false, "error": e.to_string()})
            );
            return;
        }
    }

    let boot_ok = boot_dir.join("BOOTCLASSPATH").is_file();
    let options = RnSessionOptions {
        lib_dir: libdir,
        driver_dex: dex,
        scratch: scratch.join("session-scratch"),
        heap_max_bytes: 1024 * 1024 * 1024,
        boot_dir: if boot_ok { Some(boot_dir) } else { None },
        corelib: if boot_ok {
            CoreLib::Real
        } else {
            CoreLib::Bootstrap
        },
        ..RnSessionOptions::default()
    };

    let mut session = match RnSession::launch(&apk, options) {
        Ok(session) => session,
        Err(e) => {
            println!(
                "W158A_SUMMARY {}",
                serde_json::json!({"confined": true, "launched": false, "code": e.code(), "error": e.to_string()})
            );
            return;
        }
    };
    let initial = session.tree().clone();
    println!(
        "W158A_STAGE {}",
        serde_json::json!({
            "phase": "mount",
            "nodes": initial.node_count(),
            "ops": initial.op_count,
            "hello": initial.find(|n| n.text.as_deref() == Some("Hello ACOS")).map(|n| n.tag),
            "tap_me": initial.find(|n| n.text.as_deref() == Some("TAP ME")).map(|n| n.tag),
            "tree": initial.to_json(),
        })
    );

    let tap_tag = initial.tag_for_test_id("hello-button");
    let tap_err = session
        .tap_test_id("hello-button")
        .err()
        .map(|e| (e.code().to_string(), e.to_string()));
    let after = session.tree().clone();
    let count = after
        .find(|n| n.text.as_deref() == Some("count: 1"))
        .map(|n| n.tag);
    println!(
        "W158A_STAGE {}",
        serde_json::json!({
            "phase": "tap",
            "tap_tag": tap_tag,
            "tap_error": tap_err,
            "count_tag": count,
            "tree": after.to_json(),
        })
    );

    let frame_code = session.frame().err().map(|e| e.code().to_string());
    let frame_err = session.frame().err().map(|e| e.to_string());
    println!(
        "W158A_SUMMARY {}",
        serde_json::json!({
            "confined": true,
            "launched": true,
            "nodes": initial.node_count(),
            "hello": initial.find(|n| n.text.as_deref() == Some("Hello ACOS")).is_some(),
            "tap_me": initial.find(|n| n.text.as_deref() == Some("TAP ME")).is_some(),
            "tap_tag": tap_tag,
            "count_one": count.is_some(),
            "frame_code": frame_code,
            "frame_error": frame_err,
            "registered_natives": session.registered_natives(),
            "real_boot": session.real_boot(),
        })
    );
}

#[test]
fn rn_product_launches_observes_and_taps() {
    if !acos_test_support::have_fixture("fixtures/build/apps/rn-hello.apk") {
        acos_test_support::require_prereq(false, "rn_product_launches_observes_and_taps", "rn_product_launches_observes_and_taps: fixtures/build/apps/rn-hello.apk is absent (build with tools/build/build-rn-hello.sh)");
        return;
    }
    let apk = repo_root().join("fixtures/build/apps/rn-hello.apk");
    let libdir = default_lib_dir();
    if let Err(e) = ensure_libs(&apk, &libdir) {
        panic!("could not extract fixture x86_64 libs: {e}");
    }
    let dex = repo_root().join("crates/acos-jni/tests/data/rn-bridge-probe.dex");

    let scratch = std::env::temp_dir().join(format!("acos-rn-w158a-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch dir");

    let output = Command::new(std::env::current_exe().expect("current exe"))
        .args(["--exact", "rn_product_child_worker", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env(LIBDIR_ENV, &libdir)
        .env(OUT_ENV, &scratch)
        .env(APK_ENV, &apk)
        .env(DEX_ENV, &dex)
        .output()
        .expect("spawn confined child");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("child exit status: {:?}", output.status.code());
    for line in stdout
        .lines()
        .filter(|l| l.starts_with("W158A_") || l.starts_with("ACOS_"))
    {
        eprintln!("child> {line}");
    }

    let summary_text = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("W158A_SUMMARY "))
        .map(str::to_string);
    let summary: serde_json::Value = match summary_text {
        Some(s) => serde_json::from_str(&s)
            .unwrap_or_else(|e| panic!("bad summary ({e}): {s}\nstderr:\n{stderr}")),
        None => serde_json::json!({"crashed": true, "exit": output.status.code()}),
    };

    let out_dir = acos_test_support::test_artifacts_dir("rn");
    let _ = std::fs::create_dir_all(&out_dir);
    let _ = std::fs::write(
        out_dir.join("w158a-product.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "summary": summary,
            "stages": stdout.lines().filter(|l| l.starts_with("W158A_STAGE ")).collect::<Vec<_>>(),
        }))
        .unwrap_or_default(),
    );
    let _ = std::fs::remove_dir_all(&scratch);

    assert_eq!(
        summary["confined"], true,
        "child did not report active confinement:\n{summary}\nstderr:\n{stderr}"
    );
    assert_eq!(
        summary["launched"], true,
        "RnSession::launch failed:\n{summary}\nstderr:\n{stderr}"
    );
    assert_eq!(
        summary["hello"], true,
        "tree missing 'Hello ACOS':\n{summary}"
    );
    assert_eq!(summary["tap_me"], true, "tree missing 'TAP ME':\n{summary}");
    assert!(
        summary["tap_tag"].as_i64().unwrap_or(-1) > 0,
        "hello-button did not resolve to a tag:\n{summary}"
    );
    assert_eq!(
        summary["count_one"], true,
        "tap did not move the tree to 'count: 1':\n{summary}\nstderr:\n{stderr}"
    );
    assert_eq!(
        summary["frame_code"], "UNSUPPORTED_RN_NO_RENDERER",
        "observe --frame must be honestly unsupported:\n{summary}"
    );
}
