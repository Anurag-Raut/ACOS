//! W93-A: first run of the Flutter hello engine (`libflutter.so` 166 MB
//! x86_64 + AOT `libapp.so`) through the confined native stack.
//!
//! The parent spawns a **child copy of this test binary**. The child installs
//! the ADR-0022 confinement, installs the `acos-gles` software EGL/GLES
//! substrate and the production NDK host, then
//!
//! 1. maps + relocates + `JNI_OnLoad`s `libflutter.so` with the raw loader
//!    (`require_confinement = true`, fail closed) so a missing import or
//!    relocation is recorded exactly, and
//! 2. runs the real Java harness `FlutterProbe` (compiled to
//!    `tests/data/flutter-probe.dex`) inside the VM, which calls
//!    `System.loadLibrary("flutter")`, `FlutterJNI.init` with
//!    `--enable-software-rendering`, `attachToNative` and
//!    `runBundleAndSnapshotFromLibrary` exactly the way the embedding does.
//!
//! Every step prints a `W93A_STAGE` line so the first failing stage survives a
//! crash; the last `W93A_SUMMARY` line is the machine-readable result. The
//! blockers are recorded in `results/flutter/FIRST_RUN.md`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_jni::{JniConfig, JniRuntime};
use acos_native_loader::{Loader, LoaderConfig, LoaderError};
use acos_vm::{CoreLib, Vm, VmOptions};

const CHILD_ENV: &str = "ACOS_FLUTTER_W93A_CHILD";
const LIBDIR_ENV: &str = "ACOS_FLUTTER_W93A_LIBDIR";
const OUT_ENV: &str = "ACOS_FLUTTER_W93A_OUT";
const APK_ENV: &str = "ACOS_FLUTTER_W93A_APK";

/// The libraries the loader maps directly. `libapp.so` is mapped later by the
/// engine's own `dlopen`, so only the engine is prefixed here.
const ENGINE_LIB: &str = "libflutter.so";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn default_lib_dir() -> PathBuf {
    repo_root().join(".cache/flutter-1/lib-x86_64")
}

/// Extract `lib/x86_64/{libflutter,libapp}.so` and the `flutter_assets`
/// manifest entries from the fixture APK when absent.
fn ensure_libs(apk: &Path, libdir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(libdir).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(apk).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    for name in ["libflutter.so", "libapp.so"] {
        let dest = libdir.join(name);
        if dest.is_file() {
            continue;
        }
        let entry = format!("lib/x86_64/{name}");
        let mut data = Vec::new();
        archive
            .by_name(&entry)
            .map_err(|e| format!("{entry}: {e}"))?
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        std::fs::write(&dest, data).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Extract one zip entry from the APK to `dest`.
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

/// Add every `classes*.dex` in the APK to the VM. Returns the count added.
fn load_apk_dex(apk: &Path, vm: &mut Vm) -> Result<usize, String> {
    let bytes = std::fs::read(apk).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
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

/// Broad category for a loader failure, used by `FIRST_RUN.md`.
fn category(e: &LoaderError) -> &'static str {
    match e {
        LoaderError::UnsupportedSymbols(_) => "bionic shim",
        LoaderError::UnsupportedRelocation { .. } => "loader/relocation",
        LoaderError::DepNotFound { .. } | LoaderError::UnsafeDependency { .. } => "loader/dep",
        LoaderError::InitFailed(_) => "loader/init",
        LoaderError::ElfMalformed(_) | LoaderError::WrongAbi { .. } => "loader/elf",
        LoaderError::Io(_) => "host io/landlock",
        LoaderError::ConfinementRequired => "confinement",
        _ => "loader",
    }
}

fn missing_symbols(e: &LoaderError) -> Vec<serde_json::Value> {
    match e {
        LoaderError::UnsupportedSymbols(missing) => missing
            .iter()
            .map(|m| {
                serde_json::json!({
                    "library": m.library,
                    "symbols": m.symbols,
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Write an RGBA8888 buffer as a PNG (8-bit, non-premultiplied channel order).
fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        width,
        height,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    Ok(())
}

/// W103-A crash diagnosis: an async-signal-safe-ish handler that prints the
/// fault address, instruction pointer and the loader's module+symbol+offset
/// (`dladdr`) for both. The engine is mapped by the raw loader, not the host
/// dynamic loader, so the host `dladdr` cannot see it; the loader's own
/// `bionic::dl::dladdr` uses `OBJECTS` (populated at load) and `nearest_symbol`.
///
/// Prints to `stderr` and `_exit(134)` so the parent records the last lines.
fn install_crash_handler() {
    extern "C" fn handler(sig: i32, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
        let addr = if info.is_null() {
            0usize
        } else {
            // SAFETY: a real signal frame with a valid `siginfo_t`.
            unsafe { (*info).si_addr() as usize }
        };
        let pc = if ctx.is_null() {
            0usize
        } else {
            // SAFETY: x86_64 Linux `ucontext_t` with `REG_RIP`.
            unsafe {
                (*(ctx as *const libc::ucontext_t)).uc_mcontext.gregs[libc::REG_RIP as usize]
                    as usize
            }
        };
        let describe = |value: usize| -> (String, usize) {
            let mut info = acos_native_loader::bionic::dl::DlInfo {
                dli_fname: std::ptr::null(),
                dli_fbase: std::ptr::null_mut(),
                dli_sname: std::ptr::null(),
                dli_saddr: std::ptr::null_mut(),
            };
            // SAFETY: `info` is a writable local; a failed lookup leaves it zeroed.
            let ok = unsafe {
                acos_native_loader::bionic::dl::dladdr(value as *const std::os::raw::c_void, &mut info)
            };
            if ok == 0 {
                return ("<host>".to_string(), 0);
            }
            let name = if info.dli_fname.is_null() {
                "<unknown>".to_string()
            } else {
                // SAFETY: loader-owned NUL-terminated object name.
                unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }
                    .to_string_lossy()
                    .into_owned()
            };
            let sym = if info.dli_sname.is_null() {
                String::new()
            } else {
                // SAFETY: loader-owned NUL-terminated symbol name.
                unsafe { std::ffi::CStr::from_ptr(info.dli_sname) }
                    .to_string_lossy()
                    .into_owned()
            };
            let base = info.dli_fbase as usize;
            let offset = value.wrapping_sub(base);
            let text = if sym.is_empty() {
                format!("{name}+0x{offset:x}")
            } else {
                format!("{name}+0x{offset:x} ({sym}+0x{:x})", value.wrapping_sub(info.dli_saddr as usize))
            };
            (text, base)
        };
        let (pc_text, _) = describe(pc);
        let (addr_text, _) = describe(addr);
        eprintln!(
            "W103A_CRASH signal={sig} pc=0x{pc:x} [{pc_text}] fault=0x{addr:x} [{addr_text}]"
        );
        unsafe { libc::_exit(134) };
    }

    // SAFETY: installing handlers for the fatal synchronous signals; the
    // handler only reads its arguments and prints.
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = handler as usize;
        act.sa_flags = libc::SA_SIGINFO | libc::SA_NODEFER | libc::SA_RESETHAND;
        libc::sigemptyset(&mut act.sa_mask);
        libc::sigaction(libc::SIGSEGV, &act, std::ptr::null_mut());
        libc::sigaction(libc::SIGBUS, &act, std::ptr::null_mut());
        libc::sigaction(libc::SIGABRT, &act, std::ptr::null_mut());
        libc::sigaction(libc::SIGILL, &act, std::ptr::null_mut());
        libc::sigaction(libc::SIGFPE, &act, std::ptr::null_mut());
    }
}

/// The child: confine, then load/drive the engine. Runs only with the env set.
#[test]
fn flutter_child_worker() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    install_crash_handler();
    let libdir = PathBuf::from(std::env::var(LIBDIR_ENV).expect("libdir"));
    let scratch = PathBuf::from(std::env::var(OUT_ENV).expect("scratch"));
    let apk = PathBuf::from(std::env::var(APK_ENV).expect("apk"));
    std::fs::create_dir_all(&scratch).expect("scratch dir");

    // 1. Confinement first (ADR-0022). The engine needs entropy (`/dev`) and
    //    the APK/lib directories read-only; the writable scratch is where the
    //    assets and the AOT snapshot are staged for the engine.
    let apk_dir = apk.parent().unwrap_or_else(|| Path::new("."));
    let data_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let boot_dir = repo_root().join(".cache/boot/android-34");
    let mut profile = acos_sandbox::ConfineProfile::default()
        .with_read_only(&libdir)
        .with_read_only(apk_dir)
        .with_read_only(&data_dir)
        .with_read_write(&scratch);
    // Dart's `random_device` / Dart VM seed opens `/dev/urandom`; Landlock
    // rejects character devices, so grant `/dev` read-only (READ_FILE only).
    if Path::new("/dev").is_dir() {
        profile = profile.with_read_only("/dev");
    }
    if boot_dir.join("BOOTCLASSPATH").is_file() {
        profile = profile.with_read_only(&boot_dir);
        // The engine's Skia font manager opens `/system/etc/fonts.xml` and
        // `/system/fonts/*` by absolute path; the native-loader's path
        // translation maps those onto the extracted boot tree named here.
        profile
            .env
            .push(("ACOS_BOOT_DIR".to_string(), boot_dir.display().to_string()));
    }
    // Dart AOT reserves a large (4 GiB, lightly committed) compressed-pointer
    // heap region at VM startup: `virtual_memory_compressed.cc` mmaps it with
    // MAP_NORESERVE|MAP_ANONYMOUS and fails with ENOMEM if RLIMIT_AS is too
    // small. The product default is exactly 4 GiB, which the 166 MiB engine
    // mapping plus loader tables already exhaust. Raise the *bounded* guard to
    // 12 GiB for this harness; the risk note is in results/flutter/FIRST_RUN.md
    // and ADR-0022.
    profile.rlimits.address_space_bytes = Some(12 * 1024 * 1024 * 1024);
    match acos_sandbox::confine(&profile) {
        Ok(report) => println!(
            "W93A_STAGE {}",
            serde_json::json!({"confined": true, "report": report})
        ),
        Err(e) => {
            println!(
                "W93A_SUMMARY {}",
                serde_json::json!({"confined": false, "error": e.to_string()})
            );
            return;
        }
    }

    // 2. Stage the AOT snapshot and the (empty) `flutter_assets` tree in the
    //    writable scratch so the engine can `dlopen` the snapshot and read the
    //    manifests via the extract path. Prefer the W93-A instrumented snapshot
    //    (`libapp_probe.so`, prints `ACOS_DART_MAIN`) when it has been built by
    //    `tools/flutter-hello-snapshot.sh`; otherwise use the shipped fixture.
    let app_path = scratch.join("libapp.so");
    let probe_src = libdir.join("libapp_probe.so");
    let (app_src, app_source) = if probe_src.is_file() {
        (probe_src, "probe")
    } else {
        (libdir.join("libapp.so"), "fixture")
    };
    match std::fs::copy(&app_src, &app_path) {
        Ok(n) => println!(
            "W93A_STAGE {}",
            serde_json::json!({"phase": "stage-app", "source": app_source, "bytes": n, "path": app_path.display().to_string()})
        ),
        Err(e) => println!(
            "W93A_STAGE {}",
            serde_json::json!({"phase": "stage-app-error", "detail": e.to_string()})
        ),
    }
    let assets_dir = scratch.join("flutter_assets");
    let _ = std::fs::create_dir_all(&assets_dir);
    for entry in [
        "assets/flutter_assets/AssetManifest.bin",
        "assets/flutter_assets/FontManifest.json",
        "assets/flutter_assets/NOTICES",
    ] {
        let name = Path::new(entry).file_name().unwrap().to_string_lossy().to_string();
        match extract_asset(&apk, entry, &assets_dir.join(&name)) {
            Ok(n) => println!(
                "W93A_STAGE {}",
                serde_json::json!({"phase": "asset", "entry": entry, "bytes": n})
            ),
            Err(e) => println!(
                "W93A_STAGE {}",
                serde_json::json!({"phase": "asset-error", "entry": entry, "detail": e})
            ),
        }
    }

    // 3. Install the software GLES/EGL substrate and the production NDK host so
    //    the engine's `egl*`/`gl*`/`ANativeWindow_*`/`AAssetManager_*` imports
    //    answer from real ACOS state instead of the fail-closed traps.
    acos_jni::ndk_host::install_forced();
    println!(
        "W93A_STAGE {}",
        serde_json::json!({"phase": "gles-installed", "installed": acos_gles::loader_table_installed()})
    );

    // 4. A VM so `JNI_OnLoad` gets a real `JavaVM*`, and the APK DEX so the
    //    engine's `RegisterNatives` can resolve `FlutterJNI`.
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
                "W93A_SUMMARY {}",
                serde_json::json!({"confined": true, "error": format!("Vm::new: {e:?}")})
            );
            return;
        }
    };
    match load_apk_dex(&apk, &mut vm) {
        Ok(n) => println!(
            "W93A_STAGE {}",
            serde_json::json!({"phase": "apk-dex", "classes_added": n})
        ),
        Err(e) => println!(
            "W93A_STAGE {}",
            serde_json::json!({"phase": "apk-dex-error", "detail": e})
        ),
    }

    // 5. Attach the JNI runtime before any `JNI_OnLoad` so it receives a real
    //    `JavaVM*`. A null VM would make the engine's registration path fault.
    let rt = JniRuntime::new(
        JniConfig::default()
            .with_search_path(&libdir)
            .with_search_path(&scratch)
            .with_apk(&apk),
    );
    if let Err(e) = rt.attach(&mut vm) {
        println!(
            "W93A_SUMMARY {}",
            serde_json::json!({"confined": true, "error": format!("attach: {e}")})
        );
        return;
    }
    rt.bind(&mut vm);

    // 6. Phase A: raw loader map + relocate + `JNI_OnLoad` for the engine.
    let loader = Loader::new(LoaderConfig {
        search_paths: vec![libdir.clone(), scratch.clone()],
        require_confinement: true,
        ..LoaderConfig::default()
    });
    let mut raw: Vec<serde_json::Value> = Vec::new();
    let path = libdir.join(ENGINE_LIB);
    let engine_handle = match loader.load(&path, ENGINE_LIB) {
        Ok(handle) => {
            let mut missing = Vec::new();
            if let Some(entry) = loader
                .report()
                .libraries
                .iter()
                .find(|l| l.name == ENGINE_LIB)
            {
                for symbol in &entry.imports_missing {
                    missing.push(symbol.clone());
                }
            }
            println!(
                "W93A_STAGE {}",
                serde_json::json!({
                    "phase": "engine-loaded",
                    "report": loader.report().libraries.iter()
                        .find(|l| l.name == ENGINE_LIB)
                        .map(|l| serde_json::json!({
                            "imports": l.imports_total,
                            "missing": l.imports_missing.len(),
                            "relocations": l.relocations,
                        })),
                    "missing": missing,
                })
            );
            Some(handle)
        }
        Err(e) => {
            let blocker = serde_json::json!({
                "phase": "engine-load",
                "category": category(&e),
                "code": e.code(),
                "detail": e.to_string(),
                "missing": missing_symbols(&e),
            });
            println!("W93A_STAGE {blocker}");
            raw.push(blocker);
            None
        }
    };

    // Phase A2: run `JNI_OnLoad` from the raw handle (exercises the same
    // registration path as `System.loadLibrary`, without the VM yet).
    if let Some(handle) = &engine_handle {
        if let Some(p) = handle.symbol("JNI_OnLoad") {
            println!("W93A_STAGE {}", serde_json::json!({"phase": "jni-onload-begin"}));
            // SAFETY: JNI_OnLoad has the JNI-mandated `jint (*)(JavaVM*, void*)`
            // signature; `p` is an export of the library just mapped.
            let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void) -> i32 =
                unsafe { std::mem::transmute(p) };
            // SAFETY: `rt.java_vm()` is the published JavaVM slot for the
            // attached runtime; `JNI_OnLoad` registers its natives on it.
            let rc = unsafe { f(rt.java_vm(), std::ptr::null_mut()) };
            println!(
                "W93A_STAGE {}",
                serde_json::json!({"phase": "jni-onload-returned", "rc": rc})
            );
        }
    }

    // 7. Phase B: drive the engine from real Java through the VM/JNI harness.
    let data_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let probe_dex = data_dir.join("flutter-probe.dex");
    match std::fs::read(&probe_dex)
        .map_err(|e| e.to_string())
        .and_then(|b| DexFile::parse(b).map_err(|e| format!("{e:?}")))
    {
        Ok(dex) => {
            vm.add_dex(dex);
            println!(
                "W93A_STAGE {}",
                serde_json::json!({"phase": "probe-dex", "path": probe_dex.display().to_string()})
            );
        }
        Err(e) => println!(
            "W93A_STAGE {}",
            serde_json::json!({"phase": "probe-dex-error", "detail": e})
        ),
    }

    let args = vec![
        app_path.display().to_string(),
        "main".to_string(),
        assets_dir.display().to_string(),
        apk.display().to_string(),
    ];
    let outcome = vm.run("dev/acos/fixtures/flutterhello/FlutterProbe", &args);
    // `runBundleAndSnapshotFromLibrary` is asynchronous: `main()`, surface
    // creation, viewport metrics, vsync and the pointer packet are all posted
    // to the engine's platform task runner. In the real embedding the Android
    // main `Looper` drains those tasks; here the harness pumps the same
    // per-thread `ALooper` the engine prepared.
    //
    // The software renderer draws on the engine's **raster** thread, so the
    // pump is interleaved with short sleeps: the platform thread must keep
    // draining tasks (vsync callbacks, frame scheduling) while the raster
    // thread runs, and the presented-frame capture is written from that thread.
    // The engine's fml platform runner is the calling thread's `ALooper`
    // (`fml::MessageLoopAndroid`); `alooper_pollonce` is a no-op returning
    // POLL_ERROR unless this thread has a prepared looper. Preparing is
    // idempotent: it returns the looper `ALooper_prepare` already installed for
    // this thread, or creates the platform-thread one if the engine has not
    // been attached yet.
    // SAFETY: `ALooper_prepare` has no preconditions.
    let looper = unsafe {
        acos_native_loader::bionic::ndk::ndk_looper::alooper_prepare(0)
    };
    println!(
        "W93A_STAGE {}",
        serde_json::json!({"phase": "looper-prepared", "looper": format!("{looper:p}")})
    );
    let mut seen_frames = 0usize;
    let mut delivered = 0usize;
    let mut vsync_due = std::time::Instant::now();
    for _round in 0..1500 {
        let mut fd: std::os::raw::c_int = 0;
        let mut events: std::os::raw::c_int = 0;
        let mut data: *mut std::os::raw::c_void = std::ptr::null_mut();
        // SAFETY: the out-pointers are writable locals; the looper is the one
        // `ALooper_prepare` installed for this thread. A short timeout keeps
        // the 60 Hz delivery cadence tight while still draining the engine's
        // platform runner (which is where `asyncWaitForVsync` is requested).
        unsafe {
            acos_native_loader::bionic::ndk::ndk_looper::alooper_pollonce(
                8,
                &mut fd,
                &mut events,
                &mut data,
            );
        }
        // Synthetic 60 Hz vsync: deliver `FlutterJNI.onVsync` on this (the VM
        // owner / platform) thread only when the engine has an outstanding
        // request. `frameDelayNanos` is the time since the synthetic vsync
        // instant, exactly as `VsyncWaiter$FrameCallback.doFrame` computes it.
        let now = std::time::Instant::now();
        if now >= vsync_due {
            vsync_due += std::time::Duration::from_nanos(16_666_667);
            let delay = now.duration_since(vsync_due).as_nanos() as i64;
            match vm.call_static(
                "Ldev/acos/fixtures/flutterhello/FlutterProbe;",
                "deliverVsync",
                "(J)J",
                &[acos_vm::JavaValue::Long(delay.max(0))],
            ) {
                Ok(acos_vm::JavaValue::Long(cookie)) if cookie >= 0 => {
                    delivered += 1;
                    println!(
                        "W93A_STAGE {}",
                        serde_json::json!({"phase": "vsync-delivered", "cookie": cookie, "count": delivered})
                    );
                }
                Ok(_) => {}
                Err(e) => println!(
                    "W93A_STAGE {}",
                    serde_json::json!({"phase": "vsync-deliver-error", "detail": format!("{e:?}")})
                ),
            }
        }
        let frames = acos_jni::ndk_host::presented_frames();
        if frames.len() > seen_frames {
            seen_frames = frames.len();
            println!(
                "W93A_STAGE {}",
                serde_json::json!({
                    "phase": "frame-presented",
                    "count": seen_frames,
                    "width": frames.last().map(|f| f.width),
                    "height": frames.last().map(|f| f.height),
                    "seq": frames.last().map(|f| f.seq),
                    "thread": frames.last().map(|f| f.thread.clone()),
                })
            );
        }
        if seen_frames >= 3 {
            // The tap's second frame is present; stop once it has settled.
            break;
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    for line in outcome.stdout.lines() {
        println!("W93A_PROBE {line}");
    }
    for line in outcome.stderr.lines() {
        println!("W93A_PROBE_ERR {line}");
    }

    // Persist every presented frame so `acos frame-diff` can compare the real
    // engine output with the phone reference. Frames are RGBA8888 straight
    // from `ANativeWindow_unlockAndPost` (premultiplied, like the compositor).
    let frames = acos_jni::ndk_host::presented_frames();
    let mut written: Vec<serde_json::Value> = Vec::new();
    let out_frames = repo_root().join("results/flutter");
    let _ = std::fs::create_dir_all(&out_frames);
    let mut dedup: Vec<u64> = Vec::new();
    for (i, frame) in frames.iter().enumerate() {
        // Skip byte-identical consecutive frames (same pixels, new seq).
        let mut hash: u64 = 0xcbf29ce484222325;
        for &b in frame.rgba.iter().step_by(7) {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        let path = out_frames.join(format!("w108a-frame-{i}.png"));
        let ok = write_png(&path, frame.width, frame.height, &frame.rgba).is_ok();
        written.push(serde_json::json!({
            "index": i,
            "path": path.display().to_string(),
            "width": frame.width,
            "height": frame.height,
            "seq": frame.seq,
            "hash": format!("{hash:016x}"),
            "duplicate": dedup.contains(&hash),
            "written": ok,
        }));
        dedup.push(hash);
    }
    if let Some(last) = frames.last() {
        println!(
            "W93A_STAGE {}",
            serde_json::json!({
                "phase": "frames-written",
                "count": frames.len(),
                "frames": written,
                "last": {
                    "width": last.width,
                    "height": last.height,
                    "seq": last.seq,
                    "thread": last.thread,
                },
            })
        );
    }
    if let Some(acos_vm::VmError::JavaException(h)) = outcome.vm_error {
        let class = vm
            .class_of_object(h)
            .map(|id| vm.class_desc_of_id(id))
            .unwrap_or_else(|| "<dead>".into());
        println!(
            "W93A_STAGE {}",
            serde_json::json!({"phase": "uncaught", "class": class, "message": vm.throwable_message(h)})
        );
    } else if let Some(e) = &outcome.vm_error {
        println!(
            "W93A_STAGE {}",
            serde_json::json!({"phase": "vm-error", "error": format!("{e:?}")})
        );
    }

    let natives: Vec<serde_json::Value> = rt
        .registered_natives()
        .into_iter()
        .filter(|(c, _, _, _)| c.contains("FlutterJNI"))
        .map(|(c, n, s, p)| serde_json::json!({"class": c, "name": n, "sig": s, "ptr": p}))
        .collect();
    println!(
        "W93A_STAGE {}",
        serde_json::json!({"phase": "vsync-counts", "delivered": delivered})
    );
    let libs: Vec<String> = rt.libraries().into_iter().map(|(n, _)| n).collect();
    println!(
        "W93A_SUMMARY {}",
        serde_json::json!({
            "confined": true,
            "exit_code": outcome.exit_code,
            "libraries": libs,
            "flushjni_natives": natives.len(),
            "raw_blockers": raw,
            "error": rt.take_error().map(|e| e.to_string()),
        })
    );
}

/// Parent: spawn the confined child, capture the stages/summary, persist the
/// evidence, and fail only if confinement did not engage (fail closed).
#[test]
fn flutter_hello_under_confinement() {
    if !acos_test_support::have_fixture("fixtures/build/apps/flutter-hello.apk") {
        eprintln!(
            "SKIP flutter_hello_under_confinement: fixtures/build/apps/flutter-hello.apk is \
             absent (build with tools/build-flutter-hello.sh x86_64)"
        );
        return;
    }
    let apk = repo_root().join("fixtures/build/apps/flutter-hello.apk");
    let libdir = default_lib_dir();
    if let Err(e) = ensure_libs(&apk, &libdir) {
        panic!("could not extract the fixture libs to {}: {e}", libdir.display());
    }

    let scratch = std::env::temp_dir().join(format!("acos-flutter-w93a-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch dir");

    let output = Command::new(std::env::current_exe().expect("current exe"))
        .args(["--exact", "flutter_child_worker", "--nocapture"])
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
        .filter(|l| l.starts_with("W93A_") || l.starts_with("ACOS_"))
    {
        eprintln!("child> {line}");
    }
    // Engine diagnostics: `__android_log_print` lines and structured refusals
    // land on the child's stderr. They are the only view into asynchronous
    // engine failures, so surface them (bounded) in the test report.
    let diag: Vec<&str> = stderr
        .lines()
        .filter(|l| {
            l.contains("android-log")
                || l.starts_with("ACOS ")
                || l.contains("FATAL")
                || l.contains("flutter:")
        })
        .collect();
    for line in diag.iter().take(120) {
        eprintln!("child-err> {line}");
    }

    let stage_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("W93A_STAGE ") || l.starts_with("W93A_PROBE"))
        .collect();
    let summary_text = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("W93A_SUMMARY "))
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

    // W93-A success boundary: the raw loader maps the 166 MiB engine with zero
    // missing imports, and the real Java `FlutterJNI` sequence reaches
    // `runBundleAndSnapshotFromLibrary` without a loader/bionic/JNI blocker.
    assert!(
        stdout.contains("\"phase\":\"engine-loaded\"") && stdout.contains("\"missing\":0"),
        "expected libflutter.so to load with zero missing imports: {stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("ACOS_FLUTTER_PROBE phase=runBundle ok"),
        "expected FlutterJNI.runBundleAndSnapshotFromLibrary to be reached: {stdout}\nstderr:\n{stderr}"
    );
    // When the instrumented AOT snapshot is present the Dart `main()` print is
    // the hard proof the engine executed the app snapshot (not just started the
    // shell): `flutter: ACOS_DART_MAIN entered` arrives through the engine's
    // `__android_log_print` sink.
    if stdout.contains("\"source\":\"probe\"") {
        assert!(
            stderr.contains("ACOS_DART_MAIN entered"),
            "instrumented snapshot was used but Dart main() did not run: {stdout}\nstderr:\n{stderr}"
        );
    }

    let out_dir = repo_root().join("results/flutter");
    let _ = std::fs::create_dir_all(&out_dir);
    let _ = std::fs::write(
        out_dir.join("w93a-first-run.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "summary": summary,
            "stages": stage_lines,
            "exit": output.status.code(),
        }))
        .unwrap_or_default(),
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
