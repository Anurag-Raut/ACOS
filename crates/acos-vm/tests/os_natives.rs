//! Java-level acceptance for the `core.os` natives (T-25).
//!
//! Compiles `tests/java/OsNativesMain.java` with the host `javac`/`d8` (build
//! tools only, never the execution path), runs it on the real Android 14 core
//! library + framework DEX, and checks:
//!
//! * `android.os.Build` reads (`SDK_INT`, `CPU_ABI`, `SUPPORTED_ABIS`,
//!   `FINGERPRINT`, `TYPE`, `TAGS`, …) match the extracted `build.prop` /
//!   the ART reference capture;
//! * an unset property returns the caller's default through each accessor;
//! * `SystemClock`, `Process`, `Trace` and `Log` answer real values;
//! * `Log.println_native` produced a line in the exact `logcat -v threadtime`
//!   layout of `fixtures/reference/hello-views/art-api34-x86_64/logcat_initial.txt`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_vm::natives::{os_log, os_props};
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

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
    jar: PathBuf,
    android_jar: PathBuf,
}

fn tools() -> Option<Tools> {
    let javac = which("javac")?;
    let jar = which("jar")?;
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
        jar,
        android_jar,
    })
}

/// Compile the Java case and its `@hide` stub into a DEX that references the
/// real `android.os.SystemProperties` (the stub class itself is *not* in the
/// DEX; it is only a `--lib` for d8).
fn compile_case(name: &str) -> Option<PathBuf> {
    let tools = tools()?;
    let root = repo_root();
    let out = root.join("target/tmp/acos-os-natives").join(name);
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let src = root
        .join("crates/acos-vm/tests/java")
        .join(format!("{name}.java"));
    let stub_dir = root.join("crates/acos-vm/tests/java/stubs/android/os");
    if !src.is_file() {
        return None;
    }
    let mut stubs_src: Vec<PathBuf> = std::fs::read_dir(&stub_dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "java").unwrap_or(false))
        .collect();
    stubs_src.sort();
    if stubs_src.is_empty() {
        return None;
    }
    let stubs = out.join("stubs");
    let classes = out.join("classes");
    std::fs::create_dir_all(&stubs).ok()?;
    std::fs::create_dir_all(&classes).ok()?;

    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&tools.android_jar)
        .args(["-d"])
        .arg(&stubs)
        .args(&stubs_src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let stub_jar = out.join("systemproperties-stub.jar");
    let ok = Command::new(&tools.jar)
        .arg("cf")
        .arg(&stub_jar)
        .args(["-C"])
        .arg(&stubs)
        .arg("android")
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(format!(
            "{}:{}",
            stubs.display(),
            tools.android_jar.display()
        ))
        .args(["-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    // Only the test class goes into the DEX — never the stub.
    let class_file = classes.join(format!("{name}.class"));
    if !class_file.is_file() {
        return None;
    }
    let ok = Command::new(&tools.d8)
        .arg("--lib")
        .arg(&tools.android_jar)
        .arg("--lib")
        .arg(&stub_jar)
        .args(["--min-api", "26", "--output"])
        .arg(&out)
        .arg(&class_file)
        .status()
        .ok()?
        .success();
    if !ok || !dex.is_file() {
        return None;
    }
    Some(dex)
}

fn reference_logcat() -> Option<String> {
    std::fs::read_to_string(
        repo_root().join("fixtures/reference/hello-views/art-api34-x86_64/logcat_initial.txt"),
    )
    .ok()
}

fn parse_stdout(stdout: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

/// `MM-DD HH:MM:SS.mmm` — the fixed 18-byte logcat prefix.
fn timestamp_shape(ts: &str) -> bool {
    let b = ts.as_bytes();
    b.len() == 18
        && b[2] == b'-'
        && b[5] == b' '
        && b[8] == b':'
        && b[11] == b':'
        && b[14] == b'.'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 2 | 5 | 8 | 11 | 14) || c.is_ascii_digit())
}

#[test]
fn core_os_natives_on_the_real_framework() {
    let Some(dex) = compile_case("OsNativesMain") else {
        eprintln!("SKIP core_os_natives_on_the_real_framework: javac/d8/android.jar unavailable");
        return;
    };
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => {
            eprintln!("SKIP core_os_natives_on_the_real_framework: {m}");
            return;
        }
        Err(e) => panic!("Vm::new failed: {e}"),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());

    // Start from an empty sink so only this run's lines are inspected.
    let _ = os_log::drain();
    let outcome = vm.run("OsNativesMain", &[]);
    assert_eq!(
        outcome.exit_code, 0,
        "run failed: vm_error={:?} stderr={}",
        outcome.vm_error, outcome.stderr
    );
    assert!(outcome.vm_error.is_none(), "{:?}", outcome.vm_error);
    let out = parse_stdout(&outcome.stdout);
    let get = |k: &str| -> &str {
        out.get(k)
            .unwrap_or_else(|| panic!("missing `{k}` in stdout:\n{}", outcome.stdout))
            .as_str()
    };

    // ---- Build.*: served by the seeded property table ----
    let prop =
        |k: &str| os_props::get(k).unwrap_or_else(|| panic!("property {k} is unset in the seed"));
    assert_eq!(get("sdk_prop"), "34");
    assert_eq!(get("sdk_prop_int"), "34", "getInt parses the same property");
    assert_eq!(get("sdk"), "34", "Build.VERSION.SDK_INT");
    assert_eq!(get("sdk_string"), "34", "Build.VERSION.SDK");
    assert_eq!(get("release"), "14");
    assert_eq!(get("abi"), "x86_64", "Build.CPU_ABI");
    assert_eq!(get("abi"), prop("ro.product.cpu.abi"));
    assert_eq!(
        get("abis"),
        prop("ro.product.cpu.abilist"),
        "Build.SUPPORTED_ABIS joins ro.product.cpu.abilist"
    );
    assert_eq!(get("abi32_count"), "0", "abilist32 is empty on x86_64");
    assert_eq!(get("abi64_count"), "2", "abilist64 is x86_64,arm64-v8a");
    assert_eq!(
        get("fingerprint"),
        "google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys",
        "the ART reference capture's fingerprint"
    );
    assert_eq!(get("fingerprint"), prop("ro.build.fingerprint"));
    assert_eq!(get("brand"), prop("ro.product.brand"));
    assert_eq!(get("device"), prop("ro.product.device"));
    assert_eq!(get("model"), prop("ro.product.model"));
    assert_eq!(get("product"), prop("ro.product.name"));
    assert_eq!(get("id"), "UE1A.230829.050");
    assert_eq!(get("type"), "userdebug");
    assert_eq!(get("tags"), "dev-keys");

    // ---- unset property → the caller's default ----
    assert_eq!(get("get_empty"), "[]", "get(key) → empty string");
    assert_eq!(get("get_default"), "fallback");
    assert_eq!(get("get_int"), "42");
    assert_eq!(get("get_long"), "-7");
    assert_eq!(get("get_boolean_true"), "true");
    assert_eq!(get("get_boolean_false"), "false");
    assert_eq!(
        get("type_prop_int"),
        "-1",
        "a non-numeric value falls back to the caller's default"
    );

    // ---- SystemClock ----
    assert_eq!(get("uptime_nonneg"), "true");
    assert_eq!(get("elapsed_ns_pos"), "true");
    assert_eq!(get("uptime_ns_pos"), "true");
    assert_eq!(get("elapsed_ge_uptime"), "true");
    assert_eq!(get("thread_time_nonneg"), "true");
    assert_eq!(get("micro_close"), "true", "currentTimeMicro vs wall clock");

    // ---- Process / Trace ----
    assert_eq!(get("pid_pos"), "true");
    assert_eq!(get("tid_pos"), "true");
    assert_eq!(get("uid"), "10192", "the ACOS app uid");
    assert_eq!(get("uid_for_name_system"), "1000");
    assert_eq!(get("uid_for_name_unknown"), "-1");
    assert_eq!(
        get("priority"),
        "10",
        "THREAD_PRIORITY_BACKGROUND round-trips through the VM table"
    );
    assert_eq!(get("trace_enabled"), "false", "systrace is not implemented");
    assert_eq!(get("loggable_info"), "true");
    assert_eq!(get("loggable_verbose"), "false");
    assert_eq!(get("logged"), "true");

    // ---- Guest environment (HONESTY_AUDIT.md #5): the VM-owned table, never
    // the host process environment.
    assert_eq!(
        get("env_path_null"),
        "true",
        "host PATH must not leak to guest"
    );
    assert_eq!(
        get("env_android_root"),
        "/system",
        "fixed Android-like seed"
    );
    assert_eq!(get("env_setenv"), "round-trip", "Os.setenv round-trips");
    assert_eq!(get("env_unset"), "true", "Os.unsetenv removes the key");
    assert_eq!(
        get("env_environ_nonempty"),
        "true",
        "Os.environ reflects the seeded table"
    );

    // ---- LockSupport park/unpark (HONESTY_AUDIT.md #7).
    assert_eq!(
        get("park_nanos_ok"),
        "true",
        "parkNanos must block then wake"
    );
    assert_eq!(
        get("park_permit_ok"),
        "true",
        "an unpark permit must make the next park return"
    );
    assert!(outcome.stdout.ends_with("done\n"), "main did not finish");

    // ---- Log output: the exact ART `logcat -v threadtime` layout ----
    let records = os_log::drain();
    let info = records
        .iter()
        .find(|r| r.tag == "AcosFixture" && r.msg == "onCreate")
        .unwrap_or_else(|| panic!("no AcosFixture/onCreate record in {records:?}"));
    assert_eq!(info.priority, os_log::PRIORITY_INFO);
    assert_eq!(info.pid, std::process::id());
    let line = os_log::threadtime_line(info);
    let Some(reference_file) = reference_logcat() else {
        eprintln!("SKIP logcat comparison: reference capture missing");
        return;
    };
    let reference = reference_file
        .lines()
        .find(|l| l.contains("AcosFixture: onCreate"))
        .expect("reference capture has the onCreate line");
    assert_eq!(
        reference,
        "11-10 13:51:14.009  7361  7361 I AcosFixture: onCreate"
    );
    // The pid/tid fields widen past 5 digits on a host `cargo test` process,
    // exactly like logcat's `%5d`; the fixed-width layout itself is pinned by
    // the `os_log` unit test against the same reference line.
    assert!(timestamp_shape(&line[..18]), "bad timestamp: {line}");
    let ours: Vec<&str> = line[18..].split_whitespace().collect();
    let theirs: Vec<&str> = reference[18..].split_whitespace().collect();
    assert_eq!(ours.len(), theirs.len());
    assert_eq!(
        &ours[2..],
        &theirs[2..],
        "priority letter, tag and message must match the ART line"
    );
    assert_eq!(ours[0], std::process::id().to_string());
    let warn = records
        .iter()
        .find(|r| r.tag == "AcosFixture" && r.msg == "warn")
        .expect("Log.w record");
    assert!(os_log::threadtime_line(warn).ends_with(" W AcosFixture: warn"));
}
