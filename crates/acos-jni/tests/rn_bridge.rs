//! W88-A: drive the real React Native legacy (Paper) bridge inside the CR_VM
//! confined child, from a Java harness, and capture the first JS-originated
//! UIManager op batch.
//!
//! W84-A (`rn_hermes.rs`) ran `System.loadLibrary` + the merged SoLoader
//! onloads and executed the bundled HBC on a bare Hermes runtime, stopping at
//! `__fbBatchedBridgeConfig is not set`. This test goes one step further: the
//! `BridgeProbe` entry constructs real `CatalystInstanceImpl` (whose private
//! ctor calls native `initializeBridge`, installing the JSI bridge globals),
//! runs the bundle, and mounts `rnhello` via `AppRegistry.runApplication`.
//!
//! Everything the probe prints is `ACOS_RN_BRIDGE {...}`; JS-originated
//! UIManager commands are `ACOS_RN_OP {...}`. The parent records both in
//! `results/rn/w88a-bridge.json`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_jni::{JniConfig, JniRuntime};
use acos_vm::{CoreLib, Vm, VmOptions};

const CHILD_ENV: &str = "ACOS_RN_W88A_CHILD";
const LIBDIR_ENV: &str = "ACOS_RN_W88A_LIBDIR";
const OUT_ENV: &str = "ACOS_RN_W88A_OUT";
const APK_ENV: &str = "ACOS_RN_W88A_APK";

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

#[test]
fn rn_bridge_child_worker() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let libdir = PathBuf::from(std::env::var(LIBDIR_ENV).expect("libdir"));
    let scratch = PathBuf::from(std::env::var(OUT_ENV).expect("scratch"));
    let apk = PathBuf::from(std::env::var(APK_ENV).expect("apk"));
    std::fs::create_dir_all(&scratch).expect("scratch dir");
    // Copy the fixture libs and APK into the writable scratch dir. The real
    // paths live behind symlinks in a git worktree, and SoLoader's
    // `File.canonicalize0` cannot walk a symlinked ancestor that Landlock has
    // not granted; the scratch dir is already read-write and confined.
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

    // Stage the guest-visible tree inside the VM sandbox VFS root. SoLoader
    // resolves `ApplicationInfo.sourceDir` and `nativeLibraryDir` through
    // `java.io.File`, i.e. as *guest* paths in the VM's VFS; host paths are not
    // mapped (W106-A mis-attributed the resulting `EACCES` to Landlock `realpath`
    // ancestry — the real cause was the VFS session root not being writable).
    // The Rust native loader still loads by SONAME from the host lib dir below.
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
    let probe_lib_dir = repo_root().join("fixtures/build/rn-bridge/x86_64");
    let hermes_lib_dir = repo_root().join("fixtures/build/rn-hermes/x86_64");
    let boot_dir = repo_root().join(".cache/boot/android-34");
    // Resolve symlinks first: a git worktree points `.cache/` and
    // `fixtures/build/` at the main tree, and SoLoader's `File.canonicalize0`
    // walks the *real* ancestor chain. Granting the symlink's own path is not
    // enough for `realpath`.
    let libdir_real = std::fs::canonicalize(&libdir).unwrap_or_else(|_| libdir.clone());
    let apk_dir_real = std::fs::canonicalize(apk_dir).unwrap_or_else(|_| apk_dir.to_path_buf());
    let mut profile = acos_sandbox::ConfineProfile::default()
        .with_read_only(&libdir)
        .with_read_only(&libdir_real)
        .with_read_only(apk_dir)
        .with_read_only(&apk_dir_real)
        .with_read_only(&data_dir);
    // SoLoader's `File.canonicalize0` walks the parent chain of the APK and the
    // native lib dir; those ancestors are symlinked in a git worktree, so grant
    // them read-only too (never the whole repo, only the ancestors).
    for base in [&libdir, apk_dir, &libdir_real, &apk_dir_real] {
        let mut cur = base.parent();
        while let Some(p) = cur {
            if p.exists() {
                profile = profile.with_read_only(p);
            }
            cur = p.parent();
        }
    }
    for d in [&probe_lib_dir, &hermes_lib_dir] {
        if d.is_dir() {
            profile = profile.with_read_only(d);
        }
    }
    // Hermes' `std::random_device` opens `/dev/urandom`. Grant only the device
    // nodes that are needed (W94-A #8) instead of all of `/dev`; this harness is
    // test-only and the production profile grants no `/dev` path. Landlock
    // masks directory-only rights for these non-directory paths (W106-A).
    for dev in ["/dev/urandom", "/dev/null"] {
        if Path::new(dev).exists() {
            profile = profile.with_read_only(dev);
        }
    }
    // SoLoader canonicalizes the scratch lib dir; its parent temp dir must be
    // readable but stays read-only (the scratch subtree itself is read-write).
    if let Some(parent) = scratch.parent() {
        if parent.exists() {
            profile = profile.with_read_only(parent);
        }
    }
    profile = profile.with_read_write(&scratch);
    if boot_dir.join("BOOTCLASSPATH").is_file() {
        profile = profile.with_read_only(&boot_dir);
    }
    // The VM's sandbox VFS creates its session root lazily *after* confinement
    // (`VfsConfig::from_env`). By default that is `.cache/sessions/<id>/data`,
    // which is only read-only under this profile, so `Vfs::new` is poisoned
    // with `EACCES` and every `File.canonicalize0` fails. Pin the VFS root into
    // the writable scratch (`session_root`, already populated above) and keep
    // it visible across `clearenv`, exactly like the product launch path
    // (`acos-cli`'s `session_root`).
    let _ = std::fs::create_dir_all(session_root.join("data/user/0"));
    profile = profile.with_read_write(&session_root);
    profile.env.push((
        "ACOS_SESSION_ROOT".to_string(),
        session_root.display().to_string(),
    ));
    match acos_sandbox::confine(&profile) {
        Ok(report) => println!(
            "W88A_STAGE {}",
            serde_json::json!({"confined": true, "report": report})
        ),
        Err(e) => {
            println!(
                "W88A_SUMMARY {}",
                serde_json::json!({"confined": false, "error": e.to_string()})
            );
            return;
        }
    }

    let boot_ok = boot_dir.join("BOOTCLASSPATH").is_file();
    let corelib = if boot_ok { CoreLib::Real } else { CoreLib::Bootstrap };
    let mut vm = match Vm::new(VmOptions {
        heap_max_bytes: 1024 * 1024 * 1024,
        corelib,
        boot_dir: if boot_ok { Some(boot_dir.clone()) } else { None },
        ..VmOptions::default()
    }) {
        Ok(vm) => vm,
        Err(e) => {
            println!(
                "W88A_SUMMARY {}",
                serde_json::json!({"confined": true, "error": format!("Vm::new: {e:?}")})
            );
            return;
        }
    };
    // The probe DEX carries a compile-time `AssetManager` stub (it only needs
    // the hidden ctor/addAssetPath for javac). Loading the real framework jar
    // eagerly makes the lazy boot provider win before the probe DEX is added,
    // so `ResourcesImpl.<clinit>` resolves the real `getThemeFreeFunction`.
    if boot_ok {
        // The probe DEX carries a compile-time `AssetManager` stub (it only
        // needs the hidden ctor/addAssetPath for javac). Force the real
        // framework class in first so the lazy boot provider wins over that
        // stub; `ResourcesImpl.<clinit>` then resolves the real Java
        // `AssetManager.getThemeFreeFunction()`.
        if let Err(e) = vm.load_class("Landroid/content/res/AssetManager;") {
            println!(
                "W88A_STAGE {}",
                serde_json::json!({"phase": "assetmanager-preload-error", "detail": format!("{e:?}")})
            );
        }
    }
    match load_apk_dex(&apk, &mut vm) {
        Ok(n) => println!(
            "W88A_STAGE {}",
            serde_json::json!({"phase": "apk-dex", "classes_added": n})
        ),
        Err(e) => println!(
            "W88A_STAGE {}",
            serde_json::json!({"phase": "apk-dex-error", "detail": e})
        ),
    }
    for desc in [
        "Ljavax/inject/Provider;",
        "Lcom/facebook/react/shell/MainReactPackage;",
    ] {
        match vm.load_class(desc) {
            Ok(_) => println!(
                "W88A_STAGE {}",
                serde_json::json!({"phase": "class-ok", "desc": desc})
            ),
            Err(e) => println!(
                "W88A_STAGE {}",
                serde_json::json!({"phase": "class-err", "desc": desc, "error": format!("{e:?}")})
            ),
        }
    }

    let bundle_path = scratch.join("index.android.bundle");
    match extract_asset(&apk, "assets/index.android.bundle", &bundle_path) {
        Ok(n) => println!(
            "W88A_STAGE {}",
            serde_json::json!({"phase": "bundle", "bytes": n, "path": bundle_path.display().to_string()})
        ),
        Err(e) => println!(
            "W88A_STAGE {}",
            serde_json::json!({"phase": "bundle-error", "detail": e})
        ),
    }

    let probe_dex = data_dir.join("rn-bridge-probe.dex");
    match std::fs::read(&probe_dex)
        .map_err(|e| e.to_string())
        .and_then(|b| DexFile::parse(b).map_err(|e| format!("{e:?}")))
    {
        Ok(dex) => {
            vm.add_dex(dex);
            println!(
                "W88A_STAGE {}",
                serde_json::json!({"phase": "probe-dex", "path": probe_dex.display().to_string()})
            );
        }
        Err(e) => {
            println!(
                "W88A_SUMMARY {}",
                serde_json::json!({"confined": true, "error": format!("probe dex: {e}")})
            );
            return;
        }
    }

    let mut cfg = JniConfig::default()
        .with_search_path(&libdir)
        .with_apk(&apk);
    if probe_lib_dir.join("libbridgeprobe.so").is_file() {
        cfg = cfg.with_search_path(&probe_lib_dir);
    }
    let rt = JniRuntime::new(cfg);
    if let Err(e) = rt.attach(&mut vm) {
        println!(
            "W88A_SUMMARY {}",
            serde_json::json!({"confined": true, "error": format!("attach: {e}")})
        );
        return;
    }
    rt.bind(&mut vm);

    let args = vec![
        bundle_path.display().to_string(),
        guest_lib_dir_str.clone(),
        guest_apk_str.clone(),
        apk.display().to_string(),
    ];
    let outcome = vm.run_virtual("dev/acos/fixtures/rnhello/BridgeProbe", &args);
    for line in outcome.stdout.lines() {
        println!("W88A_PROBE {line}");
    }
    for line in outcome.stderr.lines() {
        println!("W88A_PROBE_ERR {line}");
    }
    if let Some(acos_vm::VmError::JavaException(h)) = outcome.vm_error {
        let class = vm
            .class_of_object(h)
            .map(|id| vm.class_desc_of_id(id))
            .unwrap_or_else(|| "<dead>".into());
        println!(
            "W88A_STAGE {}",
            serde_json::json!({"phase": "uncaught", "class": class, "message": vm.throwable_message(h)})
        );
    } else if let Some(e) = &outcome.vm_error {
        println!(
            "W88A_STAGE {}",
            serde_json::json!({"phase": "vm-error", "error": format!("{e:?}")})
        );
    }
    let natives = rt.registered_natives().len();
    let libs: Vec<String> = rt.libraries().into_iter().map(|(n, _)| n).collect();
    println!(
        "W88A_SUMMARY {}",
        serde_json::json!({
            "confined": true,
            "exit_code": outcome.exit_code,
            "libraries": libs,
            "registered_natives": natives,
            "error": rt.take_error().map(|e| e.to_string()),
        })
    );
}

#[test]
fn rn_bridge_reaches_uimanager_under_confinement() {
    if !acos_test_support::have_fixture("fixtures/build/apps/rn-hello.apk") {
        eprintln!(
            "SKIP rn_bridge_reaches_uimanager_under_confinement: \
             fixtures/build/apps/rn-hello.apk is absent (build with tools/build-rn-hello.sh)"
        );
        return;
    }
    let apk = repo_root().join("fixtures/build/apps/rn-hello.apk");
    let libdir = default_lib_dir();
    if let Err(e) = ensure_libs(&apk, &libdir) {
        panic!("could not extract the fixture x86_64 libs to {}: {e}", libdir.display());
    }

    let scratch = std::env::temp_dir().join(format!("acos-rn-w88a-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch dir");

    let output = Command::new(std::env::current_exe().expect("current exe"))
        .args(["--exact", "rn_bridge_child_worker", "--nocapture"])
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
        .filter(|l| l.starts_with("W88A_") || l.starts_with("ACOS_"))
    {
        eprintln!("child> {line}");
    }

    let stage_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| {
            l.starts_with("W88A_STAGE ")
                || l.starts_with("W88A_PROBE ")
                || l.starts_with("ACOS_RN_")
        })
        .collect();
    let ops: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("ACOS_RN_OP "))
        .collect();
    let summary_text = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("W88A_SUMMARY "))
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
    // W140-A: the bridge artifact is `w140a-ops.json`. It carries the captured
    // `ACOS_RN_OP` batch under `ops` plus the full `ACOS_RN_BRIDGE` stage trail
    // (main-looper preparation, `ReactChoreographer`, `initializeBridge`,
    // `runJSBundle`, `AppRegistry.runApplication`). With the generic
    // `invoke-polymorphic` `VarHandle`/`MethodHandle` support (W140-A) the
    // probe now runs `runJSBundle` through real core-library
    // `ConcurrentLinkedQueue`/`ConcurrentHashMap`/`Striped64` code; the next
    // blocker is the JS-level `__fbBatchedBridgeConfig` bootstrap.
    let _ = std::fs::write(
        out_dir.join("w140a-ops.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "summary": summary,
            "stages": stage_lines,
            "ops": ops,
        }))
        .unwrap_or_default(),
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
