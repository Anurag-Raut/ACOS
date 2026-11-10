//! T-19 / T-21 acceptance: the Binder transport and the service registry.
//!
//! Every case runs the **real Android 14 core library + framework DEX** through
//! the real `$Stub$Proxy` bytecode and the single ACOS native
//! `BinderProxy.transactNative`. The services behind it are the in-process Rust
//! `ServiceHost`. The harness compiles the Java cases with the host
//! `javac`/`d8` (build tools only, never the execution path) and skips loudly
//! when the SDK or the boot-classpath cache is absent.

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_aidl::MethodInfo;
use acos_dex::DexFile;
use acos_parcel::{Parcel, ParcelFd};
use acos_runtime::Runtime;
use acos_services::host::{CallResult, PendingDelivery, ServiceHost, ServiceObject};
use acos_services::value::{Parcelable, RawFdValue, Value};
use acos_services::ServiceError;
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

/// Build the signature-only `@hide` stub jar once per test process.
fn stub_jar(tools: &Tools) -> Option<PathBuf> {
    let root = repo_root();
    let out = root.join("target/tmp/acos-runtime/stubs");
    let jar = out.join("hidden-stubs.jar");
    if jar.is_file() {
        return Some(jar);
    }
    let stubs = out.join("classes");
    std::fs::create_dir_all(&stubs).ok()?;
    let mut sources = Vec::new();
    collect_java(&root.join("crates/acos-runtime/tests/java/stubs"), &mut sources);
    sources.sort();
    if sources.is_empty() {
        return None;
    }
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&tools.android_jar)
        .arg("-d")
        .arg(&stubs)
        .args(&sources)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let ok = Command::new("jar")
        .arg("cf")
        .arg(&jar)
        .args(["-C"])
        .arg(&stubs)
        .arg(".")
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    Some(jar)
}

/// Compile `tests/java/<name>.java` (and any sibling it names) into one DEX.
/// The real framework classes (and the `@hide` stubs) are only `--lib`s for
/// d8, never bundled into the DEX.
fn compile_case(names: &[&str]) -> Option<PathBuf> {
    let tools = tools()?;
    let root = repo_root();
    let out = root.join("target/tmp/acos-runtime").join(names.join("-"));
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let stubs = stub_jar(&tools)?;
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    let sources: Vec<PathBuf> = names
        .iter()
        .map(|n| root.join("crates/acos-runtime/tests/java").join(format!("{n}.java")))
        .collect();
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(format!("{}:{}", stubs.display(), tools.android_jar.display()))
        .arg("-d")
        .arg(&classes)
        .args(&sources)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let mut class_files: Vec<PathBuf> = Vec::new();
    collect_class_files(&classes, &mut class_files);
    class_files.sort();
    if class_files.is_empty() {
        return None;
    }
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
    if !ok || !dex.is_file() {
        return None;
    }
    Some(dex)
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

fn new_runtime() -> Option<Runtime> {
    match Runtime::standard_with(VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    }) {
        Ok(rt) => Some(rt),
        Err(VmError::BootClasspathMissing(msg)) => {
            eprintln!("SKIP real-corelib runtime test: {msg}");
            None
        }
        Err(e) => panic!("Runtime::standard_with failed: {e}"),
    }
}

fn run_case(dex: &Path, main: &str) -> (Runtime, acos_vm::RunOutcome) {
    let mut rt = new_runtime().expect("runtime");
    rt.vm_mut()
        .add_dex(DexFile::parse(std::fs::read(dex).unwrap()).unwrap());
    let outcome = rt.run(main, &[]);
    (rt, outcome)
}

#[test]
fn service_manager_get_service_returns_a_working_proxy() {
    let Some(dex) = compile_case(&["RegistryMain"]) else {
        eprintln!("SKIP service_manager_get_service_returns_a_working_proxy: javac/d8/android.jar unavailable");
        return;
    };
    let (_rt, outcome) = run_case(&dex, "RegistryMain");
    assert!(
        outcome.vm_error.is_none(),
        "run failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error,
        outcome.stdout,
        outcome.stderr
    );
    assert_eq!(outcome.exit_code, 0, "exit {}:\n{}", outcome.exit_code, outcome.stderr);
    assert!(
        outcome.stdout.contains("registry=ok dev.acos.fixtures.helloviews"),
        "missing registry evidence:\n{}",
        outcome.stdout
    );
}

// ---------------------------------------------------------------------------
// T-19: outgoing transact, binder/fd round-trip, oneway, re-entrant delivery
// ---------------------------------------------------------------------------

/// A fake `IGraphicsStats` service: returns a memfd-backed
/// `ParcelFileDescriptor` (string + binder args, fd reply) and re-enters the
/// caller's `IGraphicsStatsCallback` binder through a `system -> app` delivery.
struct FakeGraphicsStats;

impl ServiceObject for FakeGraphicsStats {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        args: &[Value],
    ) -> Result<CallResult, ServiceError> {
        if method.name != "requestBufferForProcess" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let package = args.first().and_then(Value::as_str).unwrap_or_default();
        assert_eq!(package, "dev.acos.fixtures.helloviews", "string arg");
        let callback = match args.get(1) {
            Some(Value::Binder(id)) => *id,
            other => panic!("expected a binder callback arg, got {other:?}"),
        };
        let fd = ParcelFd::memfd("acos-test-gfxstats")
            .map_err(|e| ServiceError::Internal(format!("memfd: {e}")))?;
        let reply = vec![Value::TypedObject(Parcelable::ordered(
            "android.os.ParcelFileDescriptor",
            vec![Value::Int(0), Value::RawFd(Some(RawFdValue(fd)))],
        ))];
        let mut deliveries = Vec::new();
        if let Some(callback) = callback {
            let mut data = Parcel::new();
            data.write_interface_token("android.view.IGraphicsStatsCallback")
                .expect("write callback token");
            deliveries.push(PendingDelivery {
                target: callback,
                interface: "android.view.IGraphicsStatsCallback".to_string(),
                code: 1,
                method: "onRotateGraphicsStatsBuffer".to_string(),
                data,
            });
        }
        Ok(CallResult { reply, deliveries })
    }
}

/// A service that always fails; used to prove a oneway call swallows the error.
struct AlwaysFails;

impl ServiceObject for AlwaysFails {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[Value],
    ) -> Result<CallResult, ServiceError> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }
}

fn fake_host() -> ServiceHost {
    let mut host = ServiceHost::standard();
    host.register("test.svc", "android.view.IGraphicsStats", |_w| {
        Box::new(FakeGraphicsStats) as Box<dyn ServiceObject>
    });
    host.register(
        "test.oneway",
        "android.app.IActivityClientController",
        |_w| Box::new(AlwaysFails) as Box<dyn ServiceObject>,
    );
    host
}

fn run_fake_case(dex: &Path, main: &str) -> acos_vm::RunOutcome {
    let mut rt = Runtime::with_host(
        VmOptions {
            corelib: CoreLib::Real,
            ..VmOptions::default()
        },
        fake_host(),
    )
    .expect("runtime");
    rt.vm_mut()
        .add_dex(DexFile::parse(std::fs::read(dex).unwrap()).unwrap());
    rt.run(main, &[])
}

#[test]
fn fake_service_round_trips_binder_and_fd_and_reenters_the_looper() {
    let Some(dex) = compile_case(&["BinderMain"]) else {
        eprintln!("SKIP fake_service_round_trips_binder_and_fd_and_reenters_the_looper: javac/d8/android.jar unavailable");
        return;
    };
    let outcome = run_fake_case(&dex, "BinderMain");
    assert!(
        outcome.vm_error.is_none(),
        "run failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error,
        outcome.stdout,
        outcome.stderr
    );
    assert_eq!(outcome.exit_code, 0, "exit {}:\n{}", outcome.exit_code, outcome.stderr);
    assert!(
        outcome.stdout.contains("binder=ok") && outcome.stdout.contains("cbThread="),
        "missing binder evidence:\n{}\nstderr:\n{}",
        outcome.stdout,
        outcome.stderr
    );
}
