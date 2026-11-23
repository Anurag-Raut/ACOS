//! End-to-end JNI: the fixture APK's real `libhellojni.so` runs through the VM.
//!
//! The driver DEX (`tests/data/hellojni-driver.dex`) is compiled from
//! `tests/java/.../MainActivity.java`, which declares the same native methods as
//! the fixture app. The test loads the library **from the APK** via
//! `System.loadLibrary("hellojni")`, so the native-loader, `JNI_OnLoad`,
//! `RegisterNatives`, Java→native dispatch and native→Java callbacks all run.
//!
//! Expected lines are read from the ART reference capture
//! (`fixtures/reference/hello-jni/art-api34-x86_64/observed-texts-initial.txt`).

use std::path::{Path, PathBuf};

use acos_dex::DexFile;
use acos_jni::{JniConfig, JniRuntime};
use acos_vm::{CoreLib, Vm, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn boot_available() -> bool {
    // Hard-fail under ACOS_REQUIRE_FIXTURES; otherwise a loud skip.
    acos_test_support::have_fixture(".cache/boot/android-34/BOOTCLASSPATH")
}

fn reference_lines() -> Vec<String> {
    let text = std::fs::read_to_string(
        repo_root()
            .join("fixtures/reference/hello-jni/art-api34-x86_64/observed-texts-initial.txt"),
    )
    .expect("ART reference capture");
    // UI captures prefix the text view content (`android.widget.TextView | ...`),
    // so locate the fixture marker inside each line rather than at column 0.
    let markers = [
        "stringFromJNI=",
        "add(20,22)=",
        "sumArray(",
        "javaCallbackResult=",
        "voidCallback=",
        "registeredEcho=",
        "throwFromJNI ",
    ];
    text.lines()
        .filter_map(|line| {
            markers
                .iter()
                .find_map(|m| line.find(m).map(|i| line[i..].to_string()))
        })
        .collect()
}

#[test]
fn hello_jni_runs_through_the_vm() {
    if !boot_available() {
        acos_test_support::require_prereq(false, "hello_jni_runs_through_the_vm", "hello_jni_runs_through_the_vm: .cache/boot/android-34 is absent (run tools/fetch/extract-bootclasspath.sh)");
        return;
    }

    let dex_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/hellojni-driver.dex");
    let dex = DexFile::parse(std::fs::read(&dex_path).expect("driver dex")).expect("parse dex");

    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: 512 * 1024 * 1024,
        gc_stress: std::env::var_os("ACOS_GC_STRESS").is_some(),
        corelib: CoreLib::Real,
        boot_dir: None,
        trace_natives: false,
    })
    .expect("real-corelib VM");
    vm.add_dex(dex);

    if !acos_test_support::have_fixture("fixtures/build/apps/hello-jni.apk") {
        acos_test_support::require_prereq(false, "hello_jni_runs_through_the_vm", "hello_jni_runs_through_the_vm: fixtures/build/apps/hello-jni.apk is absent");
        return;
    }
    let apk = repo_root().join("fixtures/build/apps/hello-jni.apk");
    // The APK's `lib/<abi>/libhellojni.so` is a trusted, checked-in fixture, and
    // this test process is not confined; opt out of the ADR-0022 confinement
    // gate explicitly, exactly as `jni_native_fixture` does.
    let jni = JniRuntime::new(
        JniConfig::default()
            .with_apk(&apk)
            .for_tests_without_confinement(),
    );
    jni.attach(&mut vm).expect("attach JNI");

    let outcome = vm.run("dev/acos/fixtures/hellojni/MainActivity", &[]);
    if let Some(acos_vm::VmError::JavaException(h)) = outcome.vm_error {
        let mut cur = Some(h);
        let mut depth = 0;
        while let Some(c) = cur {
            if depth > 8 {
                break;
            }
            let class = vm
                .class_of_object(c)
                .map(|id| vm.class_desc_of_id(id))
                .unwrap_or_else(|| "<dead>".into());
            eprintln!(
                "{}cause[{}] handle {c}: {class}: {:?}",
                if depth == 0 { "" } else { "  " },
                depth,
                vm.throwable_message(c)
            );
            if let Some(entries) = vm.throwable_stacks.get(&c) {
                for e in entries {
                    eprintln!("    at {}.{}({})", e.class, e.method, e.line);
                }
            }
            cur = vm.throwable_cause(c);
            depth += 1;
        }
    }
    assert!(
        outcome.vm_error.is_none(),
        "VM error: {:?} uncaught: {:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error,
        outcome.uncaught,
        outcome.stdout,
        outcome.stderr
    );
    assert_eq!(outcome.exit_code, 0, "stdout:\n{}", outcome.stdout);

    let expected = reference_lines();
    assert!(!expected.is_empty(), "reference capture has JNI lines");
    let got: Vec<String> = outcome.stdout.lines().map(str::to_string).collect();
    assert_eq!(
        got, expected,
        "VM output must match the ART reference lines"
    );

    // The APK's own DEX declares the same natives the guest library exports.
    // (The driver DEX is used to execute the JNI path without the framework UI.)
    {
        use std::io::Read;
        let raw = std::fs::read(&apk).expect("apk bytes");
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(raw)).expect("zip");
        let mut entry = archive.by_name("classes.dex").expect("classes.dex");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read dex");
        let apk_dex = DexFile::parse(bytes).expect("parse apk dex");
        let def = apk_dex
            .find_class("Ldev/acos/fixtures/hellojni/MainActivity;")
            .expect("MainActivity in APK");
        let data = apk_dex
            .class_data(def)
            .expect("class data")
            .expect("class data");
        let mut apk_natives: Vec<String> = data
            .direct_methods
            .iter()
            .chain(data.virtual_methods.iter())
            .filter(|m| m.access_flags & acos_dex::access::NATIVE != 0)
            .map(|m| apk_dex.method_ref(m.method_idx).unwrap().name.to_string())
            .collect();
        apk_natives.sort();
        assert_eq!(
            apk_natives,
            vec![
                "add",
                "callBack",
                "registeredEcho",
                "stringFromJNI",
                "sumArray",
                "throwFromJNI"
            ],
            "APK native declarations"
        );
    }

    // The library really came out of the APK.
    let libs = jni.libraries();
    assert_eq!(libs.len(), 1, "loaded libraries: {libs:?}");
    assert_eq!(libs[0].0, "libhellojni.so");
    assert!(
        libs[0].1.as_deref().unwrap_or("").contains("hello-jni.apk"),
        "library path: {:?}",
        libs[0].1
    );

    // `RegisterNatives` installed the fourth native.
    let registered = jni.registered_natives();
    assert!(
        registered.iter().any(|(c, n, s, p)| {
            c == "Ldev/acos/fixtures/hellojni/MainActivity;"
                && n == "registeredEcho"
                && s == "(Ljava/lang/String;)Ljava/lang/String;"
                && *p != 0
        }),
        "RegisterNatives entries: {registered:?}"
    );

    assert_eq!(jni.take_error(), None, "no structured JNI error");
}

/// Regression guard for the ADR-0022 gate: `JniConfig` must keep requiring an
/// active confinement by default, and the trusted-fixture opt-out used above
/// must clear exactly that flag. The end-to-end test exercises the plumbing
/// through `JniRuntime::new` (it fails to load `libhellojni.so` if the flag is
/// ignored).
#[test]
fn jni_config_keeps_confinement_by_default() {
    assert!(
        JniConfig::default().require_confinement,
        "product default must require process confinement"
    );
    assert!(
        !JniConfig::default()
            .for_tests_without_confinement()
            .require_confinement,
        "the trusted in-process fixture opt-out must clear the confinement flag"
    );
}
