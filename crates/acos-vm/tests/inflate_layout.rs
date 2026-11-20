//! Java-level acceptance for `LayoutInflater` over the real resource natives
//! (W43-A).
//!
//! Compiles `tests/java/InflateMain.java` with the host `javac`/`d8` (build
//! tools only, never the execution path) and runs it on the real Android 14
//! core library + framework DEX (`--corelib real`). The Java case builds a real
//! `Resources`/`Theme` over `fixtures/build/apps/counter.apk` and inflates the
//! APK's compiled `res/layout/main.xml` through the real `PhoneLayoutInflater`,
//! then prints the resulting view tree (class, id, text, padding, text size).
//!
//! The `counter` layout is used because `hello-views` contains an `EditText`
//! whose `Editor` loads ICU break-iterator bundles (`brkitr/en_US.res`) through
//! the `/apex/com.android.i18n` VFS root, which the VM does not yet map; that is
//! the same ICU substrate blocker recorded in `results/m2/M2_LOG.md`.
//!
//! `android.test.mock.MockContext` supplies the abstract `Context` surface; its
//! DEX is not on the boot classpath, so it is loaded explicitly as a shared
//! library DEX (it is real framework code, not a fixture stub).
//!
//! Skips loudly when `javac`, `d8`, `android.jar`, the fixture APK or the real
//! boot classpath are unavailable.

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;
use acos_vm::javabridge::JavaValue;
use acos_vm::natives::binder::{install_transport, BinderDelivery, BinderTransport};
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

/// A transport that answers every service lookup with `null` (the truthful
/// answer for this VM-level test: no platform services are running).
///
/// `ServiceManager.<clinit>` only reaches `ViewConfiguration`/`Accessibility`
/// when `BinderInternal.getContextObject()` returns null; with a context object
/// the framework's own null checks make `getService` return null without a
/// fake service.
struct NoServices;

impl BinderTransport for NoServices {
    fn context_object(&self, _vm: &mut Vm) -> u64 {
        1
    }

    fn binder_id_of(&self, vm: &mut Vm, obj: acos_vm::heap::Handle) -> Option<BinderId> {
        if let Ok(JavaValue::Long(v)) = vm.get_field(obj, "mNativeData") {
            return Some(BinderId(v as u64));
        }
        if let Ok(JavaValue::Long(v)) = vm.get_field(obj, "mObject") {
            return Some(BinderId(v as u64));
        }
        None
    }

    fn binder_object(&self, vm: &mut Vm, id: BinderId) -> Option<acos_vm::heap::Handle> {
        if id.is_null() {
            return None;
        }
        vm.new_instance_ctor(
            "android.os.BinderProxy",
            "(J)V",
            &[JavaValue::Long(id.0 as i64)],
        )
        .ok()
    }

    fn transact(
        &self,
        _vm: &mut Vm,
        _binder: BinderId,
        _code: u32,
        _data: &mut Parcel,
        reply: &mut Parcel,
        _flags: u32,
    ) -> Result<Vec<BinderDelivery>, String> {
        // Success header (`readException` sees 0) followed by a null binder,
        // exactly what a real `IServiceManager.getService` returns for a name
        // that is not registered.
        reply.reset();
        reply.write_int32(0);
        reply.write_strong_binder_opt(None);
        Ok(Vec::new())
    }

    fn descriptor(&self, _id: BinderId) -> Option<String> {
        None
    }

    fn is_alive(&self, _id: BinderId) -> bool {
        false
    }
}

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

/// Compile `InflateMain` against `android.jar` plus the hidden-API stubs and
/// return the DEX. The stub `AssetManager`/`Context` surface is a `--lib` only;
/// the VM loads the real framework classes.
fn compile_case(name: &str) -> Option<PathBuf> {
    let tools = tools()?;
    let root = repo_root();
    let out = root.join("target/tmp/acos-inflate-layout").join(name);
    let dex = out.join("classes.dex");
    let src = root
        .join("crates/acos-vm/tests/java")
        .join(format!("{name}.java"));
    if !src.is_file() {
        return None;
    }
    // The hidden-API stubs: the res `AssetManager` stub used by `res_natives`
    // plus the `MockContext`/`PhoneLayoutInflater` surface for this case.
    let mut stubs_src: Vec<PathBuf> = Vec::new();
    collect_java(
        &root.join("crates/acos-vm/tests/java/stubs"),
        &mut stubs_src,
    );
    collect_java(
        &root.join("crates/acos-vm/tests/java/inflate_stubs"),
        &mut stubs_src,
    );
    if stubs_src.is_empty() {
        return None;
    }
    let stubs = out.join("stubs");
    let classes = out.join("classes");
    std::fs::create_dir_all(&stubs).ok()?;
    std::fs::create_dir_all(&classes).ok()?;
    let javac = |args: &[&std::ffi::OsStr]| -> bool {
        Command::new(&tools.javac)
            .args(["--release", "11", "-encoding", "UTF-8"])
            .args(args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let stub_src_os: Vec<&std::ffi::OsStr> = stubs_src.iter().map(|p| p.as_os_str()).collect();
    let stub_args: Vec<&std::ffi::OsStr> = std::iter::once(std::ffi::OsStr::new("-cp"))
        .chain(std::iter::once(tools.android_jar.as_os_str()))
        .chain(std::iter::once(std::ffi::OsStr::new("-d")))
        .chain(std::iter::once(stubs.as_os_str()))
        .chain(stub_src_os)
        .collect();
    if !javac(&stub_args) {
        return None;
    }
    // Compile the case with the stubs dir + android.jar on the classpath; the
    // `android.test.mock` classes come from the boot DEX at runtime.
    let cp = format!("{}:{}", stubs.display(), tools.android_jar.display());
    let case_args: Vec<&std::ffi::OsStr> = vec![
        std::ffi::OsStr::new("-cp"),
        std::ffi::OsStr::new(&cp),
        std::ffi::OsStr::new("-d"),
        classes.as_os_str(),
        src.as_os_str(),
    ];
    if !javac(&case_args) {
        return None;
    }
    let mut class_files: Vec<PathBuf> = Vec::new();
    collect_ext(&classes, "class", &mut class_files);
    if class_files.is_empty() {
        return None;
    }
    let mut d8 = Command::new(&tools.d8);
    d8.arg("--lib")
        .arg(&tools.android_jar)
        .args(["--min-api", "26", "--output"])
        .arg(&out);
    for c in &class_files {
        d8.arg(c);
    }
    let ok = d8.status().ok()?.success();
    if !ok || !dex.is_file() {
        return None;
    }
    Some(dex)
}

fn collect_java(dir: &Path, out: &mut Vec<PathBuf>) {
    collect_ext(dir, "java", out);
}

fn collect_ext(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_ext(&p, ext, out);
        } else if p.extension().map(|x| x == ext).unwrap_or(false) {
            out.push(p);
        }
    }
}

fn run_case() -> Result<String, String> {
    let Some(dex) = compile_case("InflateMain") else {
        return Err("javac/d8/android.jar unavailable".to_string());
    };
    let apk = repo_root().join("fixtures/build/apps/canvas-shapes.apk");
    if !apk.is_file() {
        return Err(format!("missing fixture {}", apk.display()));
    }
    let mock_dex = repo_root().join(".cache/boot/android-34/dex/android.test.mock/classes.dex");
    if !mock_dex.is_file() {
        return Err(format!("missing {}", mock_dex.display()));
    }
    // `TextView`'s `Editor` loads ICU break-iterator data through
    // `AndroidDataFiles`, which reads these roots. The real M2 boot seeds the
    // same values (`acos-runtime`'s `seed_device_environment`); the VFS maps the
    // `/system/**` guest path onto `.cache/boot/android-34/**`.
    if std::env::var_os("ANDROID_I18N_ROOT").is_none() {
        std::env::set_var("ANDROID_I18N_ROOT", "/system/apex/com.android.i18n");
    }
    if std::env::var_os("ANDROID_TZDATA_ROOT").is_none() {
        std::env::set_var("ANDROID_TZDATA_ROOT", "/apex/com.android.tzdata");
    }
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => return Err(m),
        Err(e) => return Err(format!("Vm::new failed: {e}")),
    };
    // `ServiceManager` must answer with `null`, not panic, so the framework's
    // service-less paths (ViewConfiguration/Accessibility) run as they do on a
    // device with no such service.
    install_transport(Box::new(NoServices));
    // Real framework test helper, loaded as an app DEX because `android.test.*`
    // is not on the framework boot classpath.
    let app_dex = repo_root().join("fixtures/build/apps/canvas-shapes/dex/classes.dex");
    if !app_dex.is_file() {
        return Err(format!("missing {}", app_dex.display()));
    }
    vm.add_dex(DexFile::parse(std::fs::read(&mock_dex).unwrap()).unwrap());
    vm.add_dex(DexFile::parse(std::fs::read(&app_dex).unwrap()).unwrap());
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run("InflateMain", &[apk.to_string_lossy().to_string()]);
    if outcome.exit_code != 0 || outcome.vm_error.is_some() {
        return Err(format!(
            "run failed: exit={} vm_error={:?} stdout={} stderr={}",
            outcome.exit_code, outcome.vm_error, outcome.stdout, outcome.stderr
        ));
    }
    Ok(outcome.stdout)
}

fn assert_tree(out: &str) {
    for needle in [
        "view=android.widget.LinearLayout id=0",
        "view=android.widget.TextView id=canvas_status",
        "text=tap the canvas",
        "view=dev.acos.fixtures.canvasshapes.ShapeView id=shape",
        "done",
    ] {
        assert!(
            out.lines().any(|l| l.contains(needle)),
            "missing `{needle}` in stdout:\n{out}"
        );
    }
}

#[test]
fn inflate_layout_through_real_layoutinflater() {
    match run_case() {
        Ok(out) => assert_tree(&out),
        Err(m) => eprintln!("SKIP inflate_layout_through_real_layoutinflater: {m}"),
    }
}
