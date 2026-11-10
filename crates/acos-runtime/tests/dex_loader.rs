//! Acceptance for the real app class loader (M2, W24b).
//!
//! The counter APK is staged into the guest VFS exactly as `bindApplication`
//! does, then a real `dalvik.system.PathClassLoader` resolves
//! `dev.acos.fixtures.counter.MainActivity` through `DexPathList` ->
//! `DexFile.defineClassNative`. The DEX is deliberately **not** registered with
//! the VM beforehand, so a passing test proves the native path did the work.
//! A static method declared by the app class is then invoked and must return
//! the value it just wrote.

mod common;

use acos_apk::Apk;
use acos_runtime::apk_loader;
use acos_services::pms::{AppInfo, FIXTURE_UID};
use acos_vm::CoreLib;
use common::{compile_case, new_runtime, repo_root};

fn counter_apk() -> Option<std::path::PathBuf> {
    [
        repo_root().join("fixtures/build/apps/counter.apk"),
        repo_root().join("fixtures/build/apps/counter/base.apk"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

#[test]
fn path_class_loader_loads_app_class_from_counter_apk_dex() {
    let Some(apk) = counter_apk() else {
        eprintln!("SKIP dex_loader: fixtures/build/apps/counter.apk is absent");
        return;
    };
    let Some(java_dex) = compile_case(&["DexLoaderMain"]) else {
        eprintln!("SKIP dex_loader: javac/d8/android.jar unavailable");
        return;
    };
    let Some(mut rt) = new_runtime() else {
        return;
    };

    let manifest = Apk::open(&apk).expect("open counter apk").manifest().expect("manifest");
    let mut app = AppInfo::from_manifest(&manifest, &apk, FIXTURE_UID);
    let staged = apk_loader::stage(rt.vm(), &apk, &mut app).expect("stage counter apk");
    assert_eq!(staged.guest_path, app.source_dir);
    assert_eq!(staged.guest_path, "/data/user/0/dev.acos.fixtures.counter/base.apk");

    // Honesty: the app class must not be resolvable through the VM's providers
    // yet, otherwise the test could pass without the class loader doing work.
    let descriptor = "Ldev/acos/fixtures/counter/MainActivity;";
    assert!(
        rt.vm().class_by_desc.get(descriptor).is_none(),
        "counter class was preloaded before the class loader ran"
    );

    rt.vm_mut().add_dex(
        acos_dex::DexFile::parse(std::fs::read(&java_dex).expect("read test dex"))
            .expect("parse test dex"),
    );
    let outcome = rt.run("DexLoaderMain", &[]);
    assert!(
        outcome.vm_error.is_none(),
        "DexLoaderMain failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error,
        outcome.stdout,
        outcome.stderr
    );
    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    let out = outcome.stdout;
    assert!(out.contains("loaded=dev.acos.fixtures.counter.MainActivity"), "stdout:\n{out}");
    assert!(out.contains("probe=4242"), "stdout:\n{out}");
    assert!(
        rt.vm().class_by_desc.contains_key(descriptor),
        "the app class was never defined by DexFile.defineClassNative"
    );
    // The loader must have added exactly the APK's provider to the VM.
    assert!(
        rt.vm()
            .dexs
            .iter()
            .any(|d| d.file.find_class_index(descriptor).is_some()),
        "no DEX provider came from the app APK"
    );
    let _ = CoreLib::Real;
}
