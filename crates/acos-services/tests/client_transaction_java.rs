//! MPROC6 real-Java differential for the host-built launch transactions.
//!
//! The same harness as MODNOTIF4's
//! `real_java_write_to_parcel_matches_the_rust_codec`: the Java case
//! (`tests/java/ClientTransactionCompareMain.java`) runs on the real Android 14
//! framework through the ACOS VM, builds the *same* logical values the host
//! encodes, calls the real `writeToParcel` into an ACOS-native `Parcel` and
//! prints the marshalled bytes. This test:
//!
//! 1. **forward**: `acos-services::client_transaction` must produce byte-for-byte
//!    the framework's own bytes, for every item and for the transactions
//!    (`ClientTransaction` with a launch callback + a lifecycle-only one);
//! 2. **reverse**: the framework's `CREATOR.createFromParcel` must read those
//!    Rust bytes back (the case asserts the reconstructed fields, including the
//!    intent extras and a pending `ResultInfo`), and re-emitting the decoded
//!    object must reproduce the Rust bytes.
//!
//! The Java case skips loudly when the `javac`/`d8`/`android.jar` toolchain or
//! the boot classpath is absent; it never silently passes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_runtime::Runtime;
use acos_services::client_transaction as ctx;
use acos_services::pms::{ActivityInfo as PmsActivityInfo, PmsState};
use acos_services::value::{Bundle, Parcelable, Value};
use acos_vm::{CoreLib, VmError, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

struct Tools {
    javac: PathBuf,
    d8: PathBuf,
    android_jar: PathBuf,
}

fn tools() -> Option<Tools> {
    let javac = which("javac")?;
    let sdk = std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    Some(Tools {
        javac,
        d8,
        android_jar,
    })
}

fn collect_java(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_java(&path, out);
        } else if path.extension().map(|x| x == "java").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn collect_class_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_class_files(&path, out);
        } else if path.extension().map(|x| x == "class").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn is_fresh(artifact: &Path, deps: &[PathBuf]) -> bool {
    let Ok(a) = std::fs::metadata(artifact) else {
        return false;
    };
    let Ok(at) = a.modified() else { return false };
    deps.iter().all(|d| {
        std::fs::metadata(d)
            .and_then(|m| m.modified())
            .map(|dt| dt <= at)
            .unwrap_or(false)
    })
}

/// Signature-only `@hide` stubs (`tests/java/stubs`), compiled once.
fn stub_jar(tools: &Tools) -> Option<PathBuf> {
    let root = repo_root();
    let out = root.join("target/tmp/acos-services/stubs");
    let jar = out.join("hidden-stubs.jar");
    let mut sources = Vec::new();
    collect_java(&root.join("crates/acos-services/tests/java/stubs"), &mut sources);
    sources.sort();
    if is_fresh(&jar, &sources) {
        return Some(jar);
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    if sources.is_empty() {
        return None;
    }
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&tools.android_jar)
        .arg("-d")
        .arg(&classes)
        .args(&sources)
        .status()
        .ok()?
        .success();
    assert!(ok, "the @hide stub sources do not compile");
    let ok = Command::new("jar")
        .arg("cf")
        .arg(&jar)
        .arg("-C")
        .arg(&classes)
        .arg(".")
        .status()
        .ok()?
        .success();
    assert!(ok, "the @hide stub jar cannot be written");
    Some(jar)
}

fn compile_case(name: &str) -> Option<PathBuf> {
    let tools = tools()?;
    let root = repo_root();
    let out = root.join("target/tmp/acos-services").join(name);
    let dex = out.join("classes.dex");
    let source = root
        .join("crates/acos-services/tests/java")
        .join(format!("{name}.java"));
    let stubs = stub_jar(&tools)?;
    if is_fresh(&dex, &[source.clone(), stubs.clone()]) {
        return Some(dex);
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(format!("{}:{}", stubs.display(), tools.android_jar.display()))
        .arg("-d")
        .arg(&classes)
        .arg(&source)
        .status()
        .ok()?
        .success();
    assert!(ok, "the Java differential case does not compile");
    let mut class_files: Vec<PathBuf> = Vec::new();
    collect_class_files(&classes, &mut class_files);
    class_files.sort();
    let ok = Command::new(&tools.d8)
        .arg("--lib")
        .arg(&tools.android_jar)
        .arg("--lib")
        .arg(&stubs)
        .args(["--min-api", "26", "--output"])
        .arg(&out)
        .args(&class_files)
        .status()
        .ok()?
        .success();
    assert!(ok && dex.is_file(), "d8 did not produce the case DEX");
    Some(dex)
}

fn run_case(dex: &Path, main: &str, args: &[String]) -> (Runtime, acos_vm::RunOutcome) {
    let mut rt = match Runtime::standard_with(VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    }) {
        Ok(rt) => rt,
        // `None` for a missing boot classpath is handled by the caller's skip
        // path; a runtime built from an explicit path must never silently pass.
        Err(VmError::BootClasspathMissing(msg)) => {
            panic!("SKIP real_java_client_transaction_matches_the_host_encoder: boot classpath absent: {msg}")
        }
        Err(e) => panic!("Runtime::standard_with failed: {e}"),
    };
    rt.vm_mut()
        .add_dex(DexFile::parse(std::fs::read(dex).unwrap()).unwrap());
    let outcome = rt.run(main, args);
    (rt, outcome)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(out, "{b:02x}").unwrap();
    }
    out
}

fn parse_prefixed(stdout: &str, prefix: &str) -> HashMap<String, String> {
    stdout
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix(prefix)?;
            let (name, hex) = rest.split_once('=')?;
            Some((name.to_string(), hex.to_string()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// the logical values the Rust encoder produces for the same case
// ---------------------------------------------------------------------------

const PKG: &str = "dev.acos.fixtures.multiactivity";
const SECOND: &str = "dev.acos.fixtures.multiactivity.SecondActivity";

/// The fixture APK's own `ApplicationInfo` + `ActivityInfo`: the launch item's
/// `mInfo` is the PMS codec's value (its own differential is MODNOTIF4's), so
/// the Java case reads *our* bytes for it and the item's field order is what
/// this test compares.
fn installed() -> (Parcelable, PmsActivityInfo) {
    let path = acos_services::workspace_path("fixtures/build/apps/multi-activity.apk")
        .expect("multi-activity fixture APK");
    let state = PmsState::from_apk(path).expect("PMS state");
    let record = state.lookup(Some(PKG)).expect("installed package");
    let activity = record
        .package
        .activities
        .iter()
        .find(|a| a.name == SECOND)
        .expect("SecondActivity")
        .clone();
    (activity.to_parcelable(&record.app), activity)
}

/// The intent the Java case builds: MAIN + explicit component + the LAUNCHER
/// category + a string, an int, a boolean and a parcelable (`Rect`) extra.
fn case_intent() -> Parcelable {
    let mut extras = Bundle::new();
    extras.put("greeting", Value::Str(Some("hello".into())));
    extras.put("count", Value::Int(7));
    extras.put("flag", Value::Bool(true));
    extras.put(
        "bounds",
        Value::Parcelable(Parcelable::ordered(
            "android.graphics.Rect",
            vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Int(4),
            ],
        )),
    );
    ctx::intent_payload(
        Some("android.intent.action.MAIN"),
        None,
        0x1000_0000,
        None,
        Some((PKG, SECOND)),
        &["android.intent.category.LAUNCHER".to_string()],
        Some(extras),
    )
}

fn case_result_info() -> Parcelable {
    ctx::result_info(
        Some("dev.acos.fixtures.multiactivity.MainActivity"),
        5,
        -1, // Activity.RESULT_OK
        Some(case_intent()),
    )
}

fn case_launch_item(info: &Parcelable) -> Parcelable {
    let config = ctx::process_configuration();
    ctx::LaunchActivity::plain(
        case_intent(),
        info.clone(),
        config.clone(),
        config,
        Some(PKG.to_string()),
    )
    .to_parcelable()
}

#[test]
fn real_java_client_transaction_matches_the_host_encoder() {
    let Some(dex) = compile_case("ClientTransactionCompareMain") else {
        eprintln!("SKIP real_java_client_transaction_matches_the_host_encoder: toolchain unavailable");
        return;
    };

    let (activity_info, _) = installed();
    // The Java case needs the fixture APK; a checkout without built fixtures
    // skips loudly instead of inventing values.
    if !repo_root()
        .join("fixtures/build/apps/multi-activity.apk")
        .is_file()
    {
        eprintln!("SKIP real_java_client_transaction_matches_the_host_encoder: fixtures/build absent");
        return;
    }

    let launch = case_launch_item(&activity_info);
    let lifecycle = ctx::client_transaction(None, Some(ctx::pause_item(false, true, 0, false, false)), Vec::new());
    let values: Vec<(&str, Parcelable)> = vec![
        ("intent", case_intent()),
        ("configuration", ctx::process_configuration()),
        ("activity_info", activity_info.clone()),
        ("result_info", case_result_info()),
        (
            "referrer_intent",
            ctx::referrer_intent(case_intent(), Some(PKG)),
        ),
        ("launch_activity_item", launch.clone()),
        (
            "launch_activity_item_with_result",
            {
                let mut item = ctx::LaunchActivity::plain(
                    case_intent(),
                    activity_info.clone(),
                    ctx::process_configuration(),
                    ctx::process_configuration(),
                    Some(PKG.to_string()),
                );
                item.pending_results = vec![case_result_info()];
                item.to_parcelable()
            },
        ),
        ("resume_activity_item", ctx::resume_item(2, true, false, false)),
        (
            "pause_activity_item",
            ctx::pause_item(false, true, 0, false, false),
        ),
        ("stop_activity_item", ctx::stop_item(0x400)),
        ("destroy_activity_item", ctx::destroy_item(true, 0)),
        (
            "activity_result_item",
            ctx::activity_result_item(vec![case_result_info()]),
        ),
        (
            "new_intent_item",
            ctx::new_intent_item(vec![ctx::referrer_intent(case_intent(), Some(PKG))], true),
        ),
        (
            "client_transaction",
            // Binder tokens are null here: a live binder's id is minted by the
            // transport, not by the codec (the encoded id path is covered by
            // the ATMS tests).
            ctx::client_transaction(
                None,
                Some(ctx::resume_item(2, true, false, false)),
                vec![launch],
            ),
        ),
        // The answer to MPROC5's open question: a lifecycle-only transaction
        // writes the callbacks-presence `false` and no list at all.
        ("client_transaction_lifecycle_only", lifecycle),
    ];

    // Forward: the real framework's bytes vs. the host encoder's bytes. The
    // Java case reads the `ActivityInfo` from the same PMS bytes the host uses,
    // because that value's own codec has its own differential.
    let args = vec![format!("activity_info={}", hex(&ctx::encode(&activity_info).expect("activity_info")))];
    let (_rt, outcome) = run_case(&dex, "ClientTransactionCompareMain", &args);
    assert!(
        outcome.vm_error.is_none() && outcome.exit_code == 0,
        "ClientTransactionCompareMain failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error,
        outcome.stdout,
        outcome.stderr
    );
    let writes = parse_prefixed(&outcome.stdout, "write.");
    for (name, value) in &values {
        let rust = ctx::encode(value).unwrap_or_else(|e| panic!("{name}: encode: {e}"));
        assert_eq!(
            writes.get(*name).map(String::as_str),
            Some(hex(&rust).as_str()),
            "{name}: real-Java bytes differ from the host encoder\njava={:?}\nrust={}",
            writes.get(*name),
            hex(&rust)
        );
    }

    // Reverse: the real `createFromParcel` must read the host's bytes and
    // re-emit them identically (the case also asserts the fields, including the
    // intent extras and the pending result).
    let args: Vec<String> = values
        .iter()
        .map(|(name, value)| {
            format!(
                "{name}={}",
                hex(&ctx::encode(value).unwrap_or_else(|e| panic!("{name}: {e}")))
            )
        })
        .collect();
    let (_rt, outcome) = run_case(&dex, "ClientTransactionCompareMain", &args);
    assert!(
        outcome.vm_error.is_none() && outcome.exit_code == 0,
        "reverse createFromParcel failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error,
        outcome.stdout,
        outcome.stderr
    );
    let roundtrips = parse_prefixed(&outcome.stdout, "roundtrip.");
    for (name, value) in &values {
        assert!(
            outcome.stdout.contains(&format!("read.{name}=ok")),
            "the real {name} reader rejected the host's bytes:\n{}",
            outcome.stdout
        );
        let rust = ctx::encode(value).expect("encode");
        assert_eq!(
            roundtrips.get(*name).map(String::as_str),
            Some(hex(&rust).as_str()),
            "{name}: the real reader's re-emission differs from the host's bytes"
        );
    }
}
