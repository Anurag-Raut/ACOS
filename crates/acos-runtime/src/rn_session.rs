//! `RnSession` (W158-A): the product form of the W157-A React Native bridge
//! harness.
//!
//! W143-A/W153-A/W157-A drove the RN legacy (Paper) bridge from a Java harness
//! (`BridgeProbe`) and captured the `UIManager` operation stream. This module
//! owns the *host loop* half — VM boot, service registry, JNI attach, APK probe
//! DEX loading, VFS staging, the bundle mount and the tap pump — and exposes it
//! as a library so the confined child used by `acos launch --native` and the
//! MCP server can hold a live session:
//!
//! ```text
//! RnSession::launch(apk, opts)  -> session
//! session.tree()                -> &RnTree (tag/class/testID/text; no bounds)
//! session.tap_tag(tag)          -> refreshed tree
//! session.tap_text("TAP ME")    -> refreshed tree (resolves the touchable)
//! session.tap_test_id("hello-button")
//! session.frame()               -> UNSUPPORTED_RN_NO_RENDERER (no native views)
//! ```
//!
//! The host loop is the real one W157-A proved: the probe's `Looper.loop()` over
//! `ReactChoreographer` (real `Choreographer`, real `display` service) drains the
//! JS/JS-queue work, and the tap travels the real
//! `UIManagerModule.getEventDispatcher()`/`TouchEvent` path. The library
//! re-enters that loop for a later `tap` instead of re-running the harness.
//!
//! The session assumes the caller is already confined (ADR-0022) when it is used
//! from the product path; it only writes under its `scratch`.

use std::io::Read;
use std::path::{Path, PathBuf};

use acos_dex::DexFile;
use acos_jni::react_native::{build_tree_from_json, ReactNativeApk};
use acos_vm::vm::ThreadStatus;
pub use acos_vm::CoreLib;
use acos_vm::{JavaValue, VmOptions};

use acos_jni::{JniConfig, JniRuntime};

use crate::Runtime;

/// The Java driver class compiled into `rn-bridge-probe.dex`.
pub const DRIVER_CLASS: &str = "dev/acos/fixtures/rnhello/BridgeProbe";
/// The descriptor form of [`DRIVER_CLASS`].
pub const DRIVER_DESCRIPTOR: &str = "Ldev/acos/fixtures/rnhello/BridgeProbe;";

/// Why a React Native session could not be created or driven.
#[derive(Debug, Clone, PartialEq)]
pub enum RnSessionError {
    /// The APK is not structurally a React Native APK.
    NotReactNative {
        /// The APK path.
        apk: String,
    },
    /// The APK ships no RN/Hermes runtime for the host ABI.
    ///
    /// This is the APK-level ABI gate: the APK provides native code, but no
    /// `lib/<host-abi>/` directory. It is `UNSUPPORTED_ABI` (ADR-0018 W59-A),
    /// the same contract as the CLI gate in `acos launch`. `WRONG_ABI` stays
    /// reserved for an individual ELF whose `e_machine` the loader cannot map
    /// (`acos-native-loader::LoaderError::WrongAbi`).
    UnsupportedAbi {
        /// The APK path.
        apk: String,
        /// The RN ABIs the APK does ship.
        abis: Vec<String>,
    },
    /// A setup step failed before the bundle mounted.
    Setup {
        /// The stage that failed.
        stage: &'static str,
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
    /// A capability is not supported by the RN bridge.
    Unsupported {
        /// The capability (`renderer`, `key`, `text`, …).
        feature: &'static str,
        /// Human-readable detail.
        detail: String,
    },
    /// The requested tap target was not in the tree.
    NoTarget {
        /// What was searched for.
        query: String,
    },
}

impl RnSessionError {
    /// The structured ACOS error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            RnSessionError::NotReactNative { .. } => "UNSUPPORTED_RN",
            RnSessionError::UnsupportedAbi { .. } => "UNSUPPORTED_ABI",
            RnSessionError::Setup { .. } => "UNSUPPORTED_RN_SETUP",
            RnSessionError::Driver { .. } => "UNSUPPORTED_RN_DRIVER",
            RnSessionError::Unsupported { feature, .. } => match *feature {
                "renderer" => "UNSUPPORTED_RN_NO_RENDERER",
                "key" => "UNSUPPORTED_RN_KEY",
                "text" => "UNSUPPORTED_RN_TEXT",
                _ => "UNSUPPORTED_RN",
            },
            RnSessionError::NoTarget { .. } => "UNSUPPORTED_RN_NO_TARGET",
        }
    }
}

impl std::fmt::Display for RnSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RnSessionError::NotReactNative { apk } => {
                write!(f, "{apk} is not a React Native APK")
            }
            RnSessionError::UnsupportedAbi { apk, abis } => write!(
                f,
                "{apk} ships no RN runtime for the host ABI (abis: {})",
                abis.join(", ")
            ),
            RnSessionError::Setup { stage, detail } => {
                write!(f, "rn setup failed at {stage}: {detail}")
            }
            RnSessionError::Driver { stage, detail } => {
                write!(f, "rn driver failed at {stage}: {detail}")
            }
            RnSessionError::Unsupported { feature, detail } => {
                write!(f, "rn {feature} unsupported: {detail}")
            }
            RnSessionError::NoTarget { query } => write!(f, "no RN target for {query}"),
        }
    }
}

impl std::error::Error for RnSessionError {}

/// Session options.
#[derive(Debug, Clone)]
pub struct RnSessionOptions {
    /// Host directory holding the extracted `lib/<abi>/*.so`.
    pub lib_dir: PathBuf,
    /// Host path of the driver DEX (`rn-bridge-probe.dex`).
    pub driver_dex: PathBuf,
    /// Writable scratch directory (the only place the session may write).
    pub scratch: PathBuf,
    /// Guest application id (paths under `/data/user/0/<package>`).
    pub package: String,
    /// Heap ceiling.
    pub heap_max_bytes: u64,
    /// Boot classpath directory (`.cache/boot/android-34`), when present.
    pub boot_dir: Option<PathBuf>,
    /// Real boot classpath vs the Rust bootstrap subset.
    pub corelib: CoreLib,
    /// Virtual-time budget for one pump, in ms.
    pub quit_ms: i64,
}

impl Default for RnSessionOptions {
    fn default() -> Self {
        Self {
            lib_dir: PathBuf::from(".cache/rn-w76a/lib"),
            driver_dex: PathBuf::from("crates/acos-jni/tests/data/rn-bridge-probe.dex"),
            scratch: std::env::temp_dir().join("acos-rn-session"),
            package: "dev.acos.fixtures.rnhello".to_string(),
            heap_max_bytes: 1024 * 1024 * 1024,
            boot_dir: None,
            corelib: CoreLib::Bootstrap,
            quit_ms: 1500,
        }
    }
}

/// A live React Native session.
pub struct RnSession {
    runtime: Box<Runtime>,
    jni: JniRuntime,
    tree: acos_jni::react_native::RnTree,
    /// Host path of the extracted JS bundle, for `tap` diagnostics.
    bundle: PathBuf,
    guest_lib_dir: String,
    guest_apk: String,
    host_apk: PathBuf,
    quit_ms: i64,
    real_boot: bool,
}

impl std::fmt::Debug for RnSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RnSession")
            .field("bundle", &self.bundle)
            .field("nodes", &self.tree.node_count())
            .finish_non_exhaustive()
    }
}

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

impl RnSession {
    /// Detect a React Native APK (structural: RN/Hermes library + bundle).
    pub fn detect(apk: &Path) -> Result<Option<ReactNativeApk>, RnSessionError> {
        acos_jni::react_native::detect(apk).map_err(|e| RnSessionError::Setup {
            stage: "detect",
            detail: e.to_string(),
        })
    }

    /// Extract every `lib/<abi>/` entry from `apk` into `dest` (flat).
    ///
    /// The product path stages the RN runtime libraries from the APK into its
    /// writable session root instead of requiring a pre-extracted `.cache`
    /// directory. Returns the extracted file names.
    pub fn extract_native_libs(
        apk: &Path,
        abi: &str,
        dest: &Path,
    ) -> Result<Vec<String>, RnSessionError> {
        std::fs::create_dir_all(dest).map_err(|e| setup("stage-lib", e))?;
        let bytes = std::fs::read(apk).map_err(|e| setup("read-apk", e))?;
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| setup("zip", e))?;
        let prefix = format!("lib/{abi}/");
        let mut extracted = Vec::new();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| setup("apk-lib", e))?;
            if !entry.name().starts_with(&prefix) || entry.is_dir() {
                continue;
            }
            let Some(name) = entry.name().rsplit('/').next().map(str::to_string) else {
                continue;
            };
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut data).map_err(|e| setup("stage-lib", e))?;
            std::fs::write(dest.join(&name), &data).map_err(|e| setup("stage-lib", e))?;
            extracted.push(name);
        }
        extracted.sort();
        Ok(extracted)
    }

    /// Boot the runtime + JNI, stage the VFS, mount the JS bundle, and capture
    /// the first UIManager batch as a tree.
    pub fn launch(apk: &Path, options: RnSessionOptions) -> Result<Self, RnSessionError> {
        let apk = std::fs::canonicalize(apk).unwrap_or_else(|_| apk.to_path_buf());
        let detected = Self::detect(&apk)?.ok_or_else(|| RnSessionError::NotReactNative {
            apk: apk.display().to_string(),
        })?;
        if detected.native_abis.is_empty() {
            return Err(RnSessionError::UnsupportedAbi {
                apk: apk.display().to_string(),
                abis: detected.native_abis,
            });
        }

        // Stage the guest-visible tree inside the session VFS root.
        let scratch = &options.scratch;
        let session_root = scratch.join("session");
        let guest_app_dir = session_root.join(format!("data/user/0/{}", options.package));
        let guest_lib_dir = guest_app_dir.join("lib");
        std::fs::create_dir_all(&guest_lib_dir).map_err(|e| setup("scratch", e))?;
        for entry in std::fs::read_dir(&options.lib_dir).map_err(|e| setup("lib-dir", e))? {
            let entry = entry.map_err(|e| setup("lib-dir", e))?;
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                std::fs::copy(entry.path(), guest_lib_dir.join(entry.file_name()))
                    .map_err(|e| setup("stage-lib", e))?;
            }
        }
        let guest_apk = guest_app_dir.join("base.apk");
        std::fs::copy(&apk, &guest_apk).map_err(|e| setup("stage-apk", e))?;

        // The VM pins its VFS root from this variable at construction.
        std::env::set_var("ACOS_SESSION_ROOT", &session_root);

        let boot_ok = options
            .boot_dir
            .as_ref()
            .is_some_and(|d| d.join("BOOTCLASSPATH").is_file());
        // Boxed before `jni.bind`: `JniRuntime::bind` stores a raw `*mut Vm`, so
        // the `Runtime` must not move afterwards (W149-A). `Box` gives it a
        // stable heap address for the life of the session.
        let mut rt = Box::new(
            Runtime::standard_with(VmOptions {
                heap_max_bytes: options.heap_max_bytes as usize,
                corelib: if boot_ok {
                    CoreLib::Real
                } else {
                    options.corelib
                },
                boot_dir: if boot_ok {
                    options.boot_dir.clone()
                } else {
                    None
                },
                ..VmOptions::default()
            })
            .map_err(|e| setup("runtime", e))?,
        );

        if boot_ok {
            if let Err(e) = rt.vm_mut().load_class("Landroid/content/res/AssetManager;") {
                eprintln!("acos-rn-session: AssetManager preload: {e:?}");
            }
        }

        // The APK's DEX provides the RN classes; the driver DEX the entry point.
        let apk_bytes = std::fs::read(&apk).map_err(|e| setup("read-apk", e))?;
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(apk_bytes)).map_err(|e| setup("zip", e))?;
        let mut names: Vec<String> = (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
            .filter(|n| n.starts_with("classes") && n.ends_with(".dex"))
            .collect();
        names.sort();
        for name in &names {
            let mut data = Vec::new();
            archive
                .by_name(name)
                .map_err(|e| setup("apk-dex", e))?
                .read_to_end(&mut data)
                .map_err(|e| setup("apk-dex", e))?;
            let dex = DexFile::parse(data).map_err(|e| setup("apk-dex", e.to_string()))?;
            rt.vm_mut().add_dex(dex);
        }

        let driver_bytes =
            std::fs::read(&options.driver_dex).map_err(|e| setup("driver-dex", e))?;
        let driver =
            DexFile::parse(driver_bytes).map_err(|e| setup("driver-dex", e.to_string()))?;
        rt.vm_mut().add_dex(driver);

        let jni = JniRuntime::new(
            JniConfig::default()
                .with_search_path(&options.lib_dir)
                .with_apk(&apk),
        );
        jni.attach(rt.vm_mut())
            .map_err(|e| setup("jni-attach", e))?;
        jni.bind(rt.vm_mut());

        let bundle = scratch.join("index.android.bundle");
        extract_asset(&apk, "assets/index.android.bundle", &bundle)
            .map_err(|e| setup("bundle", e))?;

        let guest_lib_dir_str = format!("/data/user/0/{}/lib", options.package);
        let guest_apk_str = format!("/data/user/0/{}/base.apk", options.package);
        let args = vec![
            bundle.display().to_string(),
            guest_lib_dir_str.clone(),
            guest_apk_str.clone(),
            apk.display().to_string(),
            "real".to_string(),
            "mount".to_string(),
        ];
        let outcome = rt.vm_mut().run_virtual(DRIVER_CLASS, &args);

        // The driver mounts and drains; any uncaught framework failure is a
        // structured setup error, not silently ignored. The driver prints its
        // stage markers to stdout, so the tail is the only diagnostic that
        // survives a failure that never returns a tree.
        let stdout_tail = || {
            let lines: Vec<&str> = outcome.stdout.lines().rev().take(8).collect();
            lines.into_iter().rev().collect::<Vec<_>>().join(" | ")
        };
        if let Some(err) = &outcome.vm_error {
            return Err(RnSessionError::Driver {
                stage: "mount",
                detail: format!("{err:?} | {}", stdout_tail()),
            });
        }
        if let Some(uncaught) = &outcome.uncaught {
            return Err(RnSessionError::Driver {
                stage: "mount",
                detail: format!(
                    "uncaught {}: {:?} | {}",
                    uncaught.class,
                    uncaught.message,
                    stdout_tail()
                ),
            });
        }

        let mut session = Self {
            runtime: rt,
            jni,
            tree: acos_jni::react_native::RnTree {
                roots: Vec::new(),
                op_count: 0,
            },
            bundle,
            guest_lib_dir: guest_lib_dir_str,
            guest_apk: guest_apk_str,
            host_apk: apk,
            quit_ms: options.quit_ms,
            real_boot: boot_ok,
        };
        session.refresh_tree()?;
        Ok(session)
    }

    fn ops_json(&mut self) -> Result<String, RnSessionError> {
        let vm = self.runtime.vm_mut();
        let value = vm
            .call_static(DRIVER_CLASS, "opsJson", "()Ljava/lang/String;", &[])
            .map_err(|e| RnSessionError::Driver {
                stage: "ops",
                detail: format!("{e:?}"),
            })?;
        let handle = match value {
            JavaValue::Object(h) => h,
            _ => {
                return Err(RnSessionError::Driver {
                    stage: "ops",
                    detail: "opsJson did not return a String".to_string(),
                })
            }
        };
        vm.java_string_to_rust(handle)
            .map_err(|e| RnSessionError::Driver {
                stage: "ops",
                detail: format!("{e:?}"),
            })
    }

    fn refresh_tree(&mut self) -> Result<(), RnSessionError> {
        let json = self.ops_json()?;
        self.tree = build_tree_from_json(&json).map_err(|e| RnSessionError::Driver {
            stage: "tree",
            detail: format!("{e}"),
        })?;
        Ok(())
    }

    /// The current agent UI tree.
    #[must_use]
    pub fn tree(&self) -> &acos_jni::react_native::RnTree {
        &self.tree
    }

    /// The extracted JS bundle path.
    #[must_use]
    pub fn bundle(&self) -> &Path {
        &self.bundle
    }

    /// Dispatch a synthetic touch on `tag` through the real RN event path and
    /// drain the resulting JS work, then refresh the tree.
    pub fn tap_tag(&mut self, tag: i32) -> Result<(), RnSessionError> {
        let quit = self.quit_ms;
        {
            let vm = self.runtime.vm_mut();
            vm.push_static_call(
                DRIVER_CLASS,
                "tapAndPump",
                "(IJ)V",
                &[JavaValue::Int(tag), JavaValue::Long(quit)],
            )
            .map_err(|e| RnSessionError::Driver {
                stage: "tap",
                detail: format!("{e:?}"),
            })?;
            if let Some(t) = vm.threads.first_mut() {
                t.status = ThreadStatus::Runnable;
            }
            vm.run_scheduler(true).map_err(|e| RnSessionError::Driver {
                stage: "tap-pump",
                detail: format!("{e:?}"),
            })?;
            // `Looper.quit()` on the main looper throws by design and is caught
            // inside the driver; clear any residual pending exception.
            vm.pending_exception = acos_vm::heap::NULL;
        }
        self.refresh_tree()
    }

    /// Tap by `testID`.
    pub fn tap_test_id(&mut self, id: &str) -> Result<(), RnSessionError> {
        let tag = self
            .tree
            .tag_for_test_id(id)
            .ok_or_else(|| RnSessionError::NoTarget {
                query: format!("testID={id}"),
            })?;
        self.tap_tag(tag)
    }

    /// Tap the touchable whose text is `text` (`--text`).
    pub fn tap_text(&mut self, text: &str) -> Result<(), RnSessionError> {
        let tag = self
            .tree
            .tag_for_text(text)
            .ok_or_else(|| RnSessionError::NoTarget {
                query: format!("text={text}"),
            })?;
        self.tap_tag(tag)
    }

    /// The RN bridge registers no native view managers, so no pixel frame is
    /// produced. Reported honestly rather than as a blank image.
    pub fn frame(&self) -> Result<(), RnSessionError> {
        Err(RnSessionError::Unsupported {
            feature: "renderer",
            detail: "the RN bridge registers no native view managers, so UIImplementation never \
                     lays out or draws; there is no pixel frame yet"
                .to_string(),
        })
    }

    /// Whether the session was built against the real boot classpath.
    #[must_use]
    pub fn real_boot(&self) -> bool {
        self.real_boot
    }

    /// The guest library directory (diagnostic).
    #[must_use]
    pub fn guest_lib_dir(&self) -> &str {
        &self.guest_lib_dir
    }

    /// The guest APK path (diagnostic).
    #[must_use]
    pub fn guest_apk(&self) -> &str {
        &self.guest_apk
    }

    /// The host APK path.
    #[must_use]
    pub fn host_apk(&self) -> &Path {
        &self.host_apk
    }

    /// The number of registered JNI natives (diagnostic).
    #[must_use]
    pub fn registered_natives(&self) -> usize {
        self.jni.registered_natives().len()
    }

    /// The guest `System.out`/`System.err` captured so far (mount + taps).
    ///
    /// The driver dex prints one `ACOS_RN_OP` line per UIManager op; the product
    /// `logs` tool surfaces the same lines so an agent can see the bridge's own
    /// trace without a separate stdout channel.
    #[must_use]
    pub fn logs(&self) -> Vec<String> {
        let vm = self.runtime.vm();
        let mut out = Vec::new();
        for line in String::from_utf8_lossy(&vm.out_buf).lines() {
            if !line.is_empty() {
                out.push(format!("stdout: {line}"));
            }
        }
        for line in String::from_utf8_lossy(&vm.err_buf).lines() {
            if !line.is_empty() {
                out.push(format!("stderr: {line}"));
            }
        }
        out
    }

    /// Number of nodes in the current tree (diagnostic convenience).
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.tree.node_count()
    }

    /// Number of UIManager ops replayed into the current tree.
    #[must_use]
    pub fn op_count(&self) -> usize {
        self.tree.op_count
    }

    /// The JNI runtime error, if any (diagnostic).
    pub fn jni_error(&self) -> Option<String> {
        self.jni.take_error().map(|e| e.to_string())
    }
}

fn setup(stage: &'static str, detail: impl std::fmt::Display) -> RnSessionError {
    RnSessionError::Setup {
        stage,
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0018 W59-A/E1: an APK-level ABI miss is `UNSUPPORTED_ABI` on this
    /// route too, not the loader's per-ELF `WRONG_ABI`.
    #[test]
    fn abi_miss_is_unsupported_abi() {
        let err = RnSessionError::UnsupportedAbi {
            apk: "rn-hello-arm64.apk".to_string(),
            abis: vec!["arm64-v8a".to_string()],
        };
        assert_eq!(err.code(), "UNSUPPORTED_ABI");
        assert!(err.to_string().contains("host ABI"));
    }
}
