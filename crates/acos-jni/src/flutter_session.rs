//! `FlutterSession` (W141-A): the product form of the W93-A/W130-A Flutter
//! driver.
//!
//! W130-A proved the whole chain (raw ELF loader → `JNI_OnLoad` → real Java
//! `FlutterJNI` → engine platform `ALooper` → software GLES → presented frame →
//! pointer packet → counter increment) inside the `tests/flutter_hello.rs`
//! harness. This module moves that driver into a library type so the confined
//! child used by `acos launch --native` / the MCP server can own it:
//!
//! ```text
//! FlutterSession::launch(apk, opts)  -> session
//! session.pump() / pump_until_frames(n)
//! session.frame()                    -> RGBA (and PNG)
//! session.semantics()                -> Vec<SemanticsNode>
//! session.tap(x, y) / tap_text(label)
//! session.key(...) / text(...)       -> structured UNSUPPORTED_* today
//! session.shutdown()
//! ```
//!
//! The session assumes the caller has already entered `acos-sandbox`
//! confinement (ADR-0022): the loader is created with `require_confinement =
//! true` and fails closed otherwise. The caller owns the writable scratch
//! directory (the only place the confined child may write).
//!
//! Pumping is deterministic and bounded: every `pump()` drains the platform
//! `ALooper` once and delivers at most one synthetic vsync, and the
//! `pump_until_frames`/`pump_until_semantics` helpers stop at a round or wall
//! budget instead of spinning forever.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use acos_dex::DexFile;
use acos_native_loader::{LibraryHandle, Loader, LoaderConfig, LoaderError};
use acos_vm::{CoreLib, JavaValue, Vm, VmOptions};

use crate::flutter::{decode_semantics_update, detect, SemanticsNode};
use crate::{JniConfig, JniRuntime};

/// The Java driver class compiled into `flutter-probe.dex`.
pub const DRIVER_CLASS: &str = "dev/acos/fixtures/flutterhello/FlutterProbe";
/// The descriptor form of [`DRIVER_CLASS`].
pub const DRIVER_DESCRIPTOR: &str = "Ldev/acos/fixtures/flutterhello/FlutterProbe;";

/// Why a Flutter session could not be created or driven.
#[derive(Debug, Clone, PartialEq)]
pub enum FlutterSessionError {
    /// The APK is not structurally a Flutter APK.
    NotFlutter {
        /// The APK path.
        apk: String,
    },
    /// The APK ships no engine for the requested ABI.
    UnsupportedAbi {
        /// The APK path.
        apk: String,
        /// The engine ABIs the APK does ship.
        abis: Vec<String>,
    },
    /// A setup step failed before the engine was mapped.
    Setup {
        /// The stage that failed.
        stage: &'static str,
        /// Human-readable detail.
        detail: String,
    },
    /// The raw loader refused the engine (missing imports, relocation, …).
    Engine {
        /// The loader stage.
        stage: &'static str,
        /// The loader error code (`UNSUPPORTED_NATIVE_SYMBOL`, …).
        code: String,
        /// Human-readable detail.
        detail: String,
    },
    /// The Java driver failed to run.
    Driver {
        /// The stage that failed.
        stage: &'static str,
        /// Human-readable detail.
        detail: String,
    },
    /// A bounded pump exhausted its round/time budget.
    Budget {
        /// `rounds` or `time`.
        kind: &'static str,
        /// Human-readable detail.
        detail: String,
    },
    /// The engine cannot do the requested thing.
    Unsupported {
        /// The capability (`key`, `text`, …).
        feature: &'static str,
        /// Human-readable detail.
        detail: String,
    },
}

impl FlutterSessionError {
    /// The structured ACOS error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            FlutterSessionError::NotFlutter { .. } => "UNSUPPORTED_FLUTTER",
            FlutterSessionError::UnsupportedAbi { .. } => "WRONG_ABI",
            FlutterSessionError::Setup { .. } => "UNSUPPORTED_BACKEND",
            FlutterSessionError::Engine { code, .. } => match code.as_str() {
                "UNSUPPORTED_NATIVE_SYMBOL" => "UNSUPPORTED_NATIVE_SYMBOL",
                "UNSUPPORTED_RELOCATION" => "UNSUPPORTED_RELOCATION",
                _ => "ENGINE_LOAD",
            },
            FlutterSessionError::Driver { .. } => "ENGINE_DRIVER",
            FlutterSessionError::Budget { .. } => "TIMEOUT",
            FlutterSessionError::Unsupported { .. } => "UNSUPPORTED_BACKEND",
        }
    }

    /// Whether retrying the whole launch could plausibly help.
    #[must_use]
    pub fn retryable(&self) -> bool {
        matches!(self, FlutterSessionError::Budget { .. })
    }
}

impl std::fmt::Display for FlutterSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlutterSessionError::NotFlutter { apk } => {
                write!(f, "{} is not a Flutter APK", apk)
            }
            FlutterSessionError::UnsupportedAbi { apk, abis } => write!(
                f,
                "{} ships no engine for the host ABI (engine abis: {})",
                apk,
                abis.join(", ")
            ),
            FlutterSessionError::Setup { stage, detail } => {
                write!(f, "flutter setup failed at {stage}: {detail}")
            }
            FlutterSessionError::Engine {
                stage,
                code,
                detail,
            } => write!(f, "flutter engine {stage} failed ({code}): {detail}"),
            FlutterSessionError::Driver { stage, detail } => {
                write!(f, "flutter driver failed at {stage}: {detail}")
            }
            FlutterSessionError::Budget { kind, detail } => {
                write!(f, "flutter pump exceeded its {kind} budget: {detail}")
            }
            FlutterSessionError::Unsupported { feature, detail } => {
                write!(f, "flutter {feature} unsupported: {detail}")
            }
        }
    }
}

impl std::error::Error for FlutterSessionError {}

/// One presented frame, straight from `ANativeWindow_unlockAndPost`.
#[derive(Clone, Debug)]
pub struct FlutterFrame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA8888, `width * height * 4` bytes (premultiplied, like the
    /// compositor).
    pub rgba: Vec<u8>,
    /// Sink frame sequence.
    pub seq: u64,
}

impl FlutterFrame {
    /// Encode the frame as an RGBA PNG.
    pub fn png(&self) -> Result<Vec<u8>, FlutterSessionError> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|e| FlutterSessionError::Setup {
                    stage: "frame-png",
                    detail: e.to_string(),
                })?;
            writer
                .write_image_data(&self.rgba)
                .map_err(|e| FlutterSessionError::Setup {
                    stage: "frame-png",
                    detail: e.to_string(),
                })?;
        }
        Ok(out)
    }

    /// Write the frame as a PNG.
    pub fn write_png(&self, path: &Path) -> Result<(), FlutterSessionError> {
        let bytes = self.png()?;
        std::fs::write(path, bytes).map_err(|e| FlutterSessionError::Setup {
            stage: "frame-png-write",
            detail: format!("{}: {e}", path.display()),
        })
    }
}

/// Geometry and budgets for one [`FlutterSession`].
#[derive(Debug, Clone)]
pub struct FlutterSessionOptions {
    /// Surface width in physical pixels.
    pub width: u32,
    /// Surface height in physical pixels.
    pub height: u32,
    /// Device pixel ratio (`400/160 = 2.5` on the reference device).
    pub dpr: f64,
    /// Physical status-bar padding (reference: 108 px).
    pub padding_top: u32,
    /// Directory holding the extracted `libflutter.so`/`libapp.so`.
    pub lib_dir: PathBuf,
    /// Writable scratch directory (stages the snapshot and assets).
    pub scratch: PathBuf,
    /// The Java driver dex. `None` uses the in-tree `flutter-probe.dex`.
    pub driver_dex: Option<PathBuf>,
    /// Prefer `libapp_probe.so` (the instrumented AOT snapshot) when present.
    pub prefer_probe_snapshot: bool,
    /// Java heap ceiling for the guest VM.
    pub ram_budget_bytes: usize,
    /// Wall-clock ceiling for the whole session.
    pub time_budget: Duration,
    /// Enable the semantics stream (`setSemanticsEnabled(true)`).
    pub semantics: bool,
    /// Hard ceiling on pump rounds for a single wait.
    pub max_pump_rounds: usize,
    /// Synthetic vsync rate (reference device: 60 Hz).
    pub vsync_hz: f64,
}

impl Default for FlutterSessionOptions {
    fn default() -> Self {
        FlutterSessionOptions {
            width: 1080,
            height: 2400,
            dpr: 2.5,
            padding_top: 108,
            lib_dir: PathBuf::from(".cache/flutter-1/lib-x86_64"),
            scratch: std::env::temp_dir().join("acos-flutter-session"),
            driver_dex: None,
            prefer_probe_snapshot: false,
            ram_budget_bytes: 512 * 1024 * 1024,
            time_budget: Duration::from_secs(120),
            semantics: true,
            max_pump_rounds: 3000,
            vsync_hz: 60.0,
        }
    }
}

/// A live Flutter engine, driven through the real Java embedding.
pub struct FlutterSession {
    /// Boxed so its address is stable: `JniRuntime::bind` stores a raw `Vm`
    /// pointer in the thread-local JNI slot, and moving an inline `Vm` into this
    /// struct (or moving the struct) would leave that pointer dangling. The
    /// engine's JNI callbacks then dereference a stale `Heap` header.
    vm: Box<Vm>,
    rt: JniRuntime,
    _loader: Loader,
    _handle: LibraryHandle,
    looper: *mut std::os::raw::c_void,
    opts: FlutterSessionOptions,
    apk: PathBuf,
    started: Instant,
    round: usize,
    // Diagnostics from the launch.
    engine_imports_total: usize,
    engine_imports_missing: usize,
    engine_relocations: usize,
    driver_stdout: String,
    driver_stderr: String,
    driver_error: Option<String>,
    // Frames observed since launch.
    frames: Vec<FlutterFrame>,
    last_seq: u64,
    // Vsync bookkeeping.
    vsync_due: Instant,
    vsync_delivered: usize,
    // Semantics bookkeeping.
    semantics_seq: i32,
    semantics: Vec<SemanticsNode>,
    /// Whether `enableSemantics` has been sent to the engine. W156-A: it is
    /// deferred until the first frame is presented, because the Dart isolate
    /// starts asynchronously and `onSemanticsEnabledChanged` may not be
    /// installed when `launch` returns (the engine's
    /// `_updateSemanticsEnabled(true)` would then be delivered before the app
    /// can react).
    semantics_requested: bool,
}

// The session holds a raw `ALooper*` owned by the platform thread; it is never
// moved across threads by the library, but the type is safe to move between
// struct fields on one thread.
unsafe impl Send for FlutterSession {}

impl std::fmt::Debug for FlutterSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlutterSession")
            .field("apk", &self.apk)
            .field("frames", &self.frames.len())
            .field("semantics", &self.semantics.len())
            .finish_non_exhaustive()
    }
}

fn setup(stage: &'static str, detail: impl Into<String>) -> FlutterSessionError {
    FlutterSessionError::Setup {
        stage,
        detail: detail.into(),
    }
}

fn category(e: &LoaderError) -> &'static str {
    match e {
        LoaderError::UnsupportedSymbols(_) => "engine-load",
        LoaderError::UnsupportedRelocation { .. } => "engine-relocate",
        LoaderError::DepNotFound { .. } | LoaderError::UnsafeDependency { .. } => "engine-deps",
        LoaderError::InitFailed(_) => "engine-onload",
        LoaderError::ElfMalformed(_) | LoaderError::WrongAbi { .. } => "engine-elf",
        LoaderError::ConfinementRequired => "engine-confinement",
        _ => "engine-load",
    }
}

/// Extract `lib/<abi>/{libflutter,libapp}.so` from the APK when absent.
fn ensure_libs(apk: &Path, libdir: &Path, abi: &str) -> Result<(), FlutterSessionError> {
    // The engine dir is usually a pre-populated, read-only `.cache` mount under
    // the ADR-0022 sandbox; `mkdir` on it is denied (EACCES) even though the
    // directory exists, so only create it when it is genuinely absent.
    if !libdir.is_dir() {
        std::fs::create_dir_all(libdir)
            .map_err(|e| setup("ensure-libs", format!("{}: {e}", libdir.display())))?;
    }
    let bytes = std::fs::read(apk)
        .map_err(|e| setup("read-apk", format!("{}: {e}", apk.display())))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| setup("open-apk", e.to_string()))?;
    for name in ["libflutter.so", "libapp.so"] {
        let dest = libdir.join(name);
        if dest.is_file() {
            continue;
        }
        let entry = format!("lib/{abi}/{name}");
        let mut data = Vec::new();
        archive
            .by_name(&entry)
            .map_err(|e| setup("apk-entry", format!("{entry}: {e}")))?
            .read_to_end(&mut data)
            .map_err(|e| setup("apk-entry", format!("{entry}: {e}")))?;
        std::fs::write(&dest, data)
            .map_err(|e| setup("write-lib", format!("{}: {e}", dest.display())))?;
    }
    Ok(())
}

/// Extract one zip entry to `dest`.
fn extract_asset(apk: &Path, entry: &str, dest: &Path) -> Result<usize, FlutterSessionError> {
    let bytes = std::fs::read(apk)
        .map_err(|e| setup("read-apk", format!("{}: {e}", apk.display())))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| setup("open-apk", e.to_string()))?;
    let mut data = Vec::new();
    archive
        .by_name(entry)
        .map_err(|e| setup("apk-entry", format!("{entry}: {e}")))?
        .read_to_end(&mut data)
        .map_err(|e| setup("apk-entry", format!("{entry}: {e}")))?;
    std::fs::write(dest, &data).map_err(|e| setup("write-asset", e.to_string()))?;
    Ok(data.len())
}

/// Add every `classes*.dex` in the APK to the VM.
fn load_apk_dex(apk: &Path, vm: &mut Vm) -> Result<usize, FlutterSessionError> {
    let bytes = std::fs::read(apk)
        .map_err(|e| setup("read-apk", format!("{}: {e}", apk.display())))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| setup("open-apk", e.to_string()))?;
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
            .map_err(|e| setup("apk-entry", format!("{name}: {e}")))?
            .read_to_end(&mut data)
            .map_err(|e| setup("apk-entry", format!("{name}: {e}")))?;
        let dex = DexFile::parse(data)
            .map_err(|e| setup("apk-dex", format!("{name}: {e:?}")))?;
        vm.add_dex(dex);
        added += 1;
    }
    Ok(added)
}

impl FlutterSession {
    /// Launch the engine for `apk`. The caller must already be confined.
    pub fn launch(
        apk: &Path,
        opts: FlutterSessionOptions,
    ) -> Result<Self, FlutterSessionError> {
        let apk = apk.to_path_buf();
        let apk_str = apk.display().to_string();
        let info = detect(&apk).map_err(|e| setup("detect", e.to_string()))?;
        let info = info.ok_or(FlutterSessionError::NotFlutter { apk: apk_str.clone() })?;
        let abi = info
            .host_engine_abi()
            .ok_or_else(|| FlutterSessionError::UnsupportedAbi {
                apk: apk_str.clone(),
                abis: info.engine_abis.clone(),
            })?
            .to_string();

        std::fs::create_dir_all(&opts.scratch)
            .map_err(|e| setup("scratch", format!("{}: {e}", opts.scratch.display())))?;
        ensure_libs(&apk, &opts.lib_dir, &abi)?;

        // Stage the AOT snapshot.
        let app_path = opts.scratch.join("libapp.so");
        let probe_src = opts.lib_dir.join("libapp_probe.so");
        let app_src = if opts.prefer_probe_snapshot && probe_src.is_file() {
            probe_src
        } else {
            opts.lib_dir.join("libapp.so")
        };
        std::fs::copy(&app_src, &app_path).map_err(|e| {
            setup(
                "stage-snapshot",
                format!("{}: {e}", app_src.display()),
            )
        })?;

        // Stage the flutter_assets manifests the engine reads.
        let assets_dir = opts.scratch.join("flutter_assets");
        std::fs::create_dir_all(&assets_dir)
            .map_err(|e| setup("assets-dir", e.to_string()))?;
        for entry in [
            "assets/flutter_assets/AssetManifest.bin",
            "assets/flutter_assets/FontManifest.json",
            "assets/flutter_assets/NOTICES",
        ] {
            let name = Path::new(entry)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();
            let _ = extract_asset(&apk, entry, &assets_dir.join(&name));
        }

        // Software GLES + NDK host so the engine's egl/gl/window imports answer.
        crate::ndk_host::install_forced();

        // Vision: build the VM. Resolve the boot-classpath cache exactly like
        // the framework runtime (`$ACOS_BOOT_DIR`, then a `.cache/boot/android-34`
        // walk-up): under ADR-0022 confinement the environment is cleared, so
        // the env var alone is not reliable. Fall back to `CoreLib::Bootstrap`
        // when no cache is present.
        let boot_dir = acos_vm::vm::default_boot_dir(None);
        let boot_ok = boot_dir
            .as_ref()
            .map(|d| d.join("BOOTCLASSPATH").is_file())
            .unwrap_or(false);
        let corelib = if boot_ok { CoreLib::Real } else { CoreLib::Bootstrap };
        let mut vm = Box::new(
            Vm::new(VmOptions {
                heap_max_bytes: opts.ram_budget_bytes,
                corelib,
                boot_dir: if boot_ok { boot_dir } else { None },
                ..VmOptions::default()
            })
            .map_err(|e| setup("vm", format!("{e:?}")))?,
        );
        load_apk_dex(&apk, &mut vm)?;

        let rt = JniRuntime::new(
            JniConfig::default()
                .with_search_path(&opts.lib_dir)
                .with_search_path(&opts.scratch)
                .with_apk(&apk),
        );
        rt.attach(&mut vm)
            .map_err(|e| setup("jni-attach", e.to_string()))?;
        rt.bind(&mut vm);

        // Map the engine with the raw loader (fail closed without confinement).
        let loader = Loader::new(LoaderConfig {
            search_paths: vec![opts.lib_dir.clone(), opts.scratch.clone()],
            require_confinement: true,
            ..LoaderConfig::default()
        });
        let handle = loader.load(&opts.lib_dir.join("libflutter.so"), "libflutter.so").map_err(
            |e| FlutterSessionError::Engine {
                stage: category(&e),
                code: e.code().to_string(),
                detail: e.to_string(),
            },
        )?;
        let (engine_imports_total, engine_imports_missing, engine_relocations) = loader
            .report()
            .libraries
            .iter()
            .find(|l| l.name == "libflutter.so")
            .map(|l| (l.imports_total, l.imports_missing.len(), l.relocations))
            .unwrap_or((0, 0, 0));

        // Run `JNI_OnLoad` explicitly (the Java `System.loadLibrary` path also
        // does it; doing it first makes a registration failure visible before
        // the driver runs).
        if let Some(p) = handle.symbol("JNI_OnLoad") {
            // SAFETY: JNI_OnLoad's mandated `jint (*)(JavaVM*, void*)` ABI; `p`
            // is an export of the engine just mapped.
            let f: unsafe extern "C" fn(
                *mut std::os::raw::c_void,
                *mut std::os::raw::c_void,
            ) -> i32 = unsafe { std::mem::transmute(p) };
            // SAFETY: the runtime publishes a real JavaVM*.
            let _ = unsafe { f(rt.java_vm(), std::ptr::null_mut()) };
        }

        // The Java driver dex.
        let driver_dex = opts
            .driver_dex
            .clone()
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/flutter-probe.dex"));
        let driver_bytes = std::fs::read(&driver_dex).map_err(|e| {
            setup("driver-dex", format!("{}: {e}", driver_dex.display()))
        })?;
        let driver = DexFile::parse(driver_bytes)
            .map_err(|e| setup("driver-dex", format!("{}: {e:?}", driver_dex.display())))?;
        vm.add_dex(driver);

        // Drive the real embedding from Java.
        let args = vec![
            app_path.display().to_string(),
            "main".to_string(),
            assets_dir.display().to_string(),
            apk.display().to_string(),
        ];
        let outcome = vm.run(DRIVER_CLASS, &args);
        let driver_error = outcome.vm_error.as_ref().map(|e| format!("{e:?}"));
        let driver_stdout = outcome.stdout.clone();
        let driver_stderr = outcome.stderr.clone();
        if !driver_stdout.contains("phase=runBundle ok") && driver_error.is_none() {
            return Err(FlutterSessionError::Driver {
                stage: "runBundle",
                detail: first_error_line(&driver_stdout)
                    .unwrap_or_else(|| "driver did not reach runBundle".to_string()),
            });
        }

        // Prepare the platform thread's ALooper (idempotent).
        // SAFETY: `ALooper_prepare` has no preconditions.
        let looper = unsafe {
            acos_native_loader::bionic::ndk::ndk_looper::alooper_prepare(0)
        };

        let session = FlutterSession {
            vm,
            rt,
            _loader: loader,
            _handle: handle,
            looper,
            opts,
            apk,
            started: Instant::now(),
            round: 0,
            engine_imports_total,
            engine_imports_missing,
            engine_relocations,
            driver_stdout,
            driver_stderr,
            driver_error,
            frames: Vec::new(),
            last_seq: 0,
            vsync_due: Instant::now(),
            vsync_delivered: 0,
            semantics_seq: -1,
            semantics: Vec::new(),
            semantics_requested: false,
        };
        Ok(session)
    }

    fn call_driver(
        &mut self,
        method: &str,
        sig: &str,
        args: &[JavaValue],
    ) -> Result<JavaValue, FlutterSessionError> {
        self.vm
            .call_static(DRIVER_DESCRIPTOR, method, sig, args)
            .map_err(|e| FlutterSessionError::Driver {
                stage: "call",
                detail: format!("{method}: {e:?}"),
            })
    }

    /// Drain the platform `ALooper` once and deliver at most one synthetic
    /// vsync. Returns the number of frames observed so far.
    pub fn pump(&mut self) -> Result<usize, FlutterSessionError> {
        let mut fd: std::os::raw::c_int = 0;
        let mut events: std::os::raw::c_int = 0;
        let mut data: *mut std::os::raw::c_void = std::ptr::null_mut();
        debug_assert!(!self.looper.is_null(), "ALooper was not prepared");
        // SAFETY: out-pointers are writable locals; the looper is this thread's.
        unsafe {
            acos_native_loader::bionic::ndk::ndk_looper::alooper_pollonce(
                8,
                &mut fd,
                &mut events,
                &mut data,
            );
        }
        self.round += 1;

        // Synthetic vsync on the VM-owner (platform) thread.
        let now = Instant::now();
        if now >= self.vsync_due {
            let period = Duration::from_nanos(
                (1_000_000_000.0 / self.opts.vsync_hz.max(1.0)) as u64,
            );
            self.vsync_due = now + period;
            let delay = now.duration_since(self.vsync_due).as_nanos() as i64;
            let _ = self.call_driver(
                "deliverVsync",
                "(J)J",
                &[JavaValue::Long(delay.max(0))],
            );
            self.vsync_delivered += 1;
        }

        // The probe's single `scheduleFrame()` is coalesced into the first
        // frame; ask again until the rasterizer has presented one.
        if self.frames.is_empty() && self.round % 40 == 39 {
            let _ = self.call_driver("requestFrame", "()V", &[]);
        }

        self.collect_frames();
        // W156-A: request semantics only once the Dart isolate has drawn a
        // frame (proof that `main` ran and installed
        // `onSemanticsEnabledChanged`). Doing it in `launch` raced the isolate
        // start and the engine's callback fired before the app could observe
        // it, leaving `sSemanticsSeq` at 0.
        if self.opts.semantics && !self.semantics_requested && !self.frames.is_empty() {
            self.semantics_requested = true;
            let _ = self.call_driver("enableSemantics", "()V", &[]);
        }
        Ok(self.frames.len())
    }

    /// Pump until at least `target` frames have been observed, or the round /
    /// wall budget is exhausted. Returns the frame count.
    pub fn pump_until_frames(
        &mut self,
        target: usize,
        max_rounds: usize,
    ) -> Result<usize, FlutterSessionError> {
        let deadline = self.started + self.opts.time_budget;
        for _ in 0..max_rounds {
            if self.frames.len() >= target {
                return Ok(self.frames.len());
            }
            if Instant::now() >= deadline {
                return Err(FlutterSessionError::Budget {
                    kind: "time",
                    detail: format!(
                        "awaiting frame {target}, have {}",
                        self.frames.len()
                    ),
                });
            }
            self.pump()?;
        }
        if self.frames.len() >= target {
            Ok(self.frames.len())
        } else {
            Err(FlutterSessionError::Budget {
                kind: "rounds",
                detail: format!("awaiting frame {target}, have {}", self.frames.len()),
            })
        }
    }

    fn collect_frames(&mut self) {
        for frame in crate::ndk_host::presented_frames() {
            if frame.seq == 0 || frame.seq <= self.last_seq {
                continue;
            }
            self.last_seq = frame.seq;
            self.frames.push(FlutterFrame {
                width: frame.width,
                height: frame.height,
                rgba: frame.rgba,
                seq: frame.seq,
            });
            // Bound memory: keep the newest 8 frames.
            if self.frames.len() > 8 {
                self.frames.remove(0);
            }
        }
    }

    /// Every frame observed so far, oldest first.
    #[must_use]
    pub fn frames(&self) -> &[FlutterFrame] {
        &self.frames
    }

    /// The most recently presented frame.
    #[must_use]
    pub fn frame(&self) -> Option<&FlutterFrame> {
        self.frames.last()
    }

    /// The number of frames observed.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Refresh and return the semantic tree from the engine's accessibility
    /// stream. Empty until the app calls `PlatformDispatcher.updateSemantics`.
    pub fn semantics(&mut self) -> Result<Vec<SemanticsNode>, FlutterSessionError> {
        let seq = match self
            .vm
            .get_static_field(DRIVER_DESCRIPTOR, "sSemanticsSeq")
        {
            Ok(JavaValue::Int(v)) => v,
            Ok(_) => 0,
            Err(e) => {
                return Err(FlutterSessionError::Driver {
                    stage: "semantics",
                    detail: format!("sSemanticsSeq: {e:?}"),
                })
            }
        };
        if seq == self.semantics_seq {
            return Ok(self.semantics.clone());
        }
        self.semantics_seq = seq;

        let buffer = match self.vm.get_static_field(DRIVER_DESCRIPTOR, "sSemanticsBuffer") {
            Ok(JavaValue::Object(h)) => self.read_byte_array(h)?,
            Ok(JavaValue::Null) | Err(_) => Vec::new(),
            Ok(_) => Vec::new(),
        };
        let strings = match self.vm.get_static_field(DRIVER_DESCRIPTOR, "sSemanticsStrings") {
            Ok(JavaValue::Object(h)) => self.read_string_array(h)?,
            Ok(JavaValue::Null) | Err(_) => Vec::new(),
            Ok(_) => Vec::new(),
        };
        if buffer.is_empty() {
            self.semantics = Vec::new();
            return Ok(self.semantics.clone());
        }
        let nodes = decode_semantics_update(&buffer, &strings, &[])
            .map_err(|e| FlutterSessionError::Driver {
                stage: "semantics",
                detail: e.to_string(),
            })?;
        self.semantics = nodes;
        Ok(self.semantics.clone())
    }

    fn read_byte_array(&mut self, h: acos_vm::Handle) -> Result<Vec<u8>, FlutterSessionError> {
        let len = self.vm.array_length(h).map_err(|e| FlutterSessionError::Driver {
            stage: "semantics",
            detail: format!("{e:?}"),
        })?;
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            if let JavaValue::Byte(b) = self.vm.get_array_element(h, i).map_err(|e| {
                FlutterSessionError::Driver {
                    stage: "semantics",
                    detail: format!("{e:?}"),
                }
            })? {
                out.push(b as u8);
            }
        }
        Ok(out)
    }

    fn read_string_array(&mut self, h: acos_vm::Handle) -> Result<Vec<String>, FlutterSessionError> {
        let len = self.vm.array_length(h).map_err(|e| FlutterSessionError::Driver {
            stage: "semantics",
            detail: format!("{e:?}"),
        })?;
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            match self.vm.get_array_element(h, i).map_err(|e| {
                FlutterSessionError::Driver {
                    stage: "semantics",
                    detail: format!("{e:?}"),
                }
            })? {
                JavaValue::Object(sh) => {
                    let s = self.vm.java_string_to_rust(sh).map_err(|e| {
                        FlutterSessionError::Driver {
                            stage: "semantics",
                            detail: format!("{e:?}"),
                        }
                    })?;
                    out.push(s);
                }
                JavaValue::Null => out.push(String::new()),
                _ => out.push(String::new()),
            }
        }
        Ok(out)
    }

    /// Dispatch a tap at physical `(x, y)` and pump until the next frame.
    pub fn tap(&mut self, x: f64, y: f64) -> Result<(), FlutterSessionError> {
        let before = self.frames.len();
        self.call_driver(
            "tapAt",
            "(DD)V",
            &[JavaValue::Double(x), JavaValue::Double(y)],
        )?;
        // Bound the wait: a tap that produces no new frame is reported, not
        // hidden. The caller can inspect `frame_count`.
        let _ = self.pump_until_frames(before + 1, self.opts.max_pump_rounds);
        Ok(())
    }

    /// Tap the centre of the semantic node whose `text()` equals `label`.
    /// Returns the tapped physical point.
    pub fn tap_text(&mut self, label: &str) -> Result<[f64; 2], FlutterSessionError> {
        let nodes = self.semantics()?;
        let node = nodes
            .iter()
            .find(|n| n.text() == Some(label))
            .ok_or_else(|| FlutterSessionError::Unsupported {
                feature: "tap_text",
                detail: format!(
                    "no semantics node with label {label:?} ({} nodes; semantics stream may \
                     be empty because the app declares none)",
                    nodes.len()
                ),
            })?;
        let b = node.bounds();
        // Semantics rects are logical pixels; the driver's `tapAt` packet holds
        // physical pixels (the Dart handler divides by `devicePixelRatio`).
        let dpr = self.opts.dpr;
        let x = ((b[0] + b[2]) as f64 / 2.0) * dpr;
        let y = ((b[1] + b[3]) as f64 / 2.0) * dpr;
        let (node_id, clickable) = (node.id, node.clickable());
        // W156-A: prefer the accessibility action channel
        // (`nativeDispatchSemanticsAction`, TAP = 1). If the app does not act on
        // it (a raw `dart:ui` probe without an action handler), fall back to the
        // proven W130-A pointer packet at the node centre so the tap still has
        // an effect.
        if clickable {
            let before = self.frames.len();
            let _ = self.call_driver(
                "dispatchSemanticsTap",
                "(I)V",
                &[JavaValue::Int(node_id)],
            );
            let _ =
                self.pump_until_frames(before + 1, self.opts.max_pump_rounds.min(120));
            if self.frames.len() > before {
                return Ok([x, y]);
            }
        }
        self.tap(x, y)?;
        Ok([x, y])
    }

    /// Send a key. Flutter has no `KeyEvent` embedding entry point in this
    /// driver; reported as unsupported rather than silently dropped.
    pub fn key(&mut self, code: &str) -> Result<(), FlutterSessionError> {
        Err(FlutterSessionError::Unsupported {
            feature: "key",
            detail: format!(
                "no Flutter embedding key path wired for {code:?}; use tap/tap_text or the \
                 semantics action channel"
            ),
        })
    }

    /// Type text. Flutter text input goes through the platform text-input
    /// channel; not wired in this driver.
    pub fn text(&mut self, _text: &str) -> Result<(), FlutterSessionError> {
        Err(FlutterSessionError::Unsupported {
            feature: "text",
            detail: "Flutter text input requires the text-input channel; not wired".to_string(),
        })
    }

    /// The number of frames the VM reports having presented (diagnostic).
    #[must_use]
    pub fn vsync_delivered(&self) -> usize {
        self.vsync_delivered
    }

    /// The geometry/budget options this session was created with.
    #[must_use]
    pub fn opts(&self) -> &FlutterSessionOptions {
        &self.opts
    }

    /// Engine import/relocation diagnostics from the raw loader.
    #[must_use]
    pub fn engine_stats(&self) -> (usize, usize, usize) {
        (
            self.engine_imports_total,
            self.engine_imports_missing,
            self.engine_relocations,
        )
    }

    /// The Java driver's captured stdout.
    #[must_use]
    pub fn driver_stdout(&self) -> &str {
        &self.driver_stdout
    }

    /// The Java driver's captured stderr.
    #[must_use]
    pub fn driver_stderr(&self) -> &str {
        &self.driver_stderr
    }

    /// A VM-level driver error, when there was one.
    #[must_use]
    pub fn driver_error(&self) -> Option<&str> {
        self.driver_error.as_deref()
    }

    /// The APK this session launched.
    #[must_use]
    pub fn apk(&self) -> &Path {
        &self.apk
    }

    /// The JNI runtime (diagnostics: registered natives, libraries).
    #[must_use]
    pub fn jni(&self) -> &JniRuntime {
        &self.rt
    }

    /// Wall-clock since launch.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Tear the session down. The VM and the mapped engine are dropped.
    pub fn shutdown(self) {
        drop(self);
    }
}

/// The first `phase=error …` line the Java driver printed, if any.
fn first_error_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find(|l| l.contains("phase=error"))
        .map(str::to_string)
}
