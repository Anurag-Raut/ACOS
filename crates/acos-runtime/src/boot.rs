//! Real framework boot: bring `android.app.ActivityThread` up inside the Rust
//! VM and drive the application-process attach handshake (milestone **M2**).
//!
//! # What "boot" means here
//!
//! Android's app process is forked from `zygote` and enters through
//! `ActivityThread.main([Ljava/lang/String;)V`, which runs
//! `Looper.prepareMainLooper()`, constructs the `ActivityThread`, calls
//! `attach(false, startSeq)` and then blocks in `Looper.loop()`.
//!
//! ACOS has no zygote, so [`Runtime::boot_app_process`] runs the same real
//! framework bytecode (from `framework.jar` on the boot classpath) but stops at
//! the end of `attach`: the attach handshake is the M2 evidence, and
//! `Looper.loop()` would block the host. Concretely it executes
//!
//! ```text
//! android.os.Looper.prepareMainLooper()          // real framework bytecode
//! new android.app.ActivityThread()               // real <init>
//! activityThread.attach(false, startSeq)         // real attach
//!   -> android.app.ActivityManager.getService()  // ServiceManager + AIDL proxy
//!   -> IActivityManager.attachApplication(mAppThread, startSeq)
//! ```
//!
//! The last call crosses the single ACOS Binder native
//! (`BinderProxy.transactNative`), is dispatched by [`crate::transport`] into
//! the in-process [`acos_services::ServiceHost`], and is recorded by
//! [`crate::atms_boot`] with the real `IApplicationThread` binder.
//!
//! # Never fake
//!
//! Every missing native or class surfaces as a structured
//! [`BootError::Stage`] naming the exact `Lclass;->method(sig)` — the boot does
//! not substitute a stub for an unimplemented API.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use acos_apk::Apk;
use acos_dex::DexFile;
use acos_vm::heap::Handle;
use acos_vm::javabridge::{BridgeError, JavaValue};
use acos_vm::VmError;

use crate::atms_boot::{first_attach, install_recording_atms, is_valid_app_thread, AttachRecord};
use crate::Runtime;

/// Boot stages, in execution order. A failure names the stage it happened in.
pub const STAGES: &[&str] = &[
    "open-apk",
    "load-app-dex",
    "prepare-main-looper",
    "instantiate-activity-thread",
    "attach-application",
];

/// A successful application-process boot.
#[derive(Debug, Clone)]
pub struct BootReport {
    /// The stages that ran, in order.
    pub stages: Vec<&'static str>,
    /// The recorded `attachApplication` handshake.
    pub attach: AttachRecord,
    /// The live `android.app.ActivityThread` instance `attach` ran on.
    pub activity_thread: Handle,
    /// Number of VM classes loaded when `attach` returned.
    pub classes_loaded: usize,
    /// Number of VM classes loaded after the boot-classpath load and before the
    /// first framework class (the "framework-boot delta" for `BOOT_LOG.md`).
    pub boot_classpath_classes: usize,
    /// Native methods invoked while booting (class, method, signature), in first
    /// invocation order. Empty unless the VM was built with native tracing.
    pub natives_invoked: Vec<(String, String, String)>,
    /// Wall-clock time for the whole boot.
    pub elapsed: Duration,
}

/// A failed application-process boot.
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    /// The APK could not be opened.
    #[error("apk: {0}")]
    Apk(#[from] acos_apk::ApkError),
    /// The app DEX could not be parsed.
    #[error("dex: {0}")]
    Dex(#[from] acos_dex::DexError),
    /// The APK contains no DEX.
    #[error("apk contains no classes.dex")]
    NoDex,
    /// A boot stage failed with a VM/bridge error.
    #[error("boot stage `{stage}`: {source}")]
    Stage {
        /// Which stage was running.
        stage: &'static str,
        /// The underlying error (boxed to keep `BootError` small).
        #[source]
        source: Box<BridgeError>,
    },
    /// `attach` returned but the ATMS never saw `attachApplication`.
    #[error("attach returned but no IActivityManager.attachApplication was received")]
    NoAttach,
    /// `attachApplication` was received with a null `IApplicationThread`.
    #[error("attachApplication received without a valid IApplicationThread binder")]
    NullAppThread,
}

impl Runtime {
    /// Boot the real framework application process for `apk_path` and stop when
    /// `IActivityManager.attachApplication` has been received.
    ///
    /// See the module docs for the exact framework path this runs.
    pub fn boot_app_process(
        &mut self,
        apk_path: impl AsRef<Path>,
    ) -> Result<BootReport, BootError> {
        seed_device_environment();
        let started = Instant::now();
        let mut stages: Vec<&'static str> = Vec::new();
        let boot_classpath_classes = self.vm().classes.len();

        // --- open-apk -----------------------------------------------------
        stages.push(STAGES[0]);
        let apk = Apk::open(apk_path)?;

        // --- load-app-dex -------------------------------------------------
        stages.push(STAGES[1]);
        let dex = apk.dex_files.first().ok_or(BootError::NoDex)?;
        let dex = DexFile::parse(dex.bytes.clone())?;
        self.vm_mut().add_dex(dex);

        // Record the attach handshake without replacing the real ATMS logic.
        let log = {
            let host = self.host();
            let mut host = host.borrow_mut();
            install_recording_atms(&mut host)
        };

        // --- prepare-main-looper -----------------------------------------
        stages.push(STAGES[2]);
        self.stage("prepare-main-looper", |vm| {
            vm.call_static("android.os.Looper", "prepareMainLooper", "()V", &[])?;
            Ok(())
        })?;

        // --- instantiate-activity-thread ---------------------------------
        stages.push(STAGES[3]);
        let thread = self.stage("instantiate-activity-thread", |vm| {
            vm.new_instance_ctor("android.app.ActivityThread", "()V", &[])
        })?;

        // --- attach-application ------------------------------------------
        stages.push(STAGES[4]);
        let start_seq: i64 = NEXT_START_SEQ.fetch_add(1, Ordering::Relaxed) as i64;
        self.stage("attach-application", |vm| {
            vm.call_method_direct(
                thread,
                "attach",
                "(ZJ)V",
                &[JavaValue::Boolean(false), JavaValue::Long(start_seq)],
            )?;
            Ok(())
        })?;

        let attach = first_attach(&log).ok_or(BootError::NoAttach)?;
        if !is_valid_app_thread(&attach) {
            return Err(BootError::NullAppThread);
        }

        let natives_invoked = self
            .vm()
            .native_trace
            .as_ref()
            .map(|t| t.classes())
            .unwrap_or_default();

        Ok(BootReport {
            stages,
            attach,
            activity_thread: thread,
            classes_loaded: self.vm().classes.len(),
            boot_classpath_classes,
            natives_invoked,
            elapsed: started.elapsed(),
        })
    }

    fn stage<T>(
        &mut self,
        name: &'static str,
        run: impl FnOnce(&mut acos_vm::Vm) -> Result<T, BridgeError>,
    ) -> Result<T, BootError> {
        let trace = std::env::var_os("ACOS_BOOT_TRACE").is_some();
        if trace {
            eprintln!("[boot] enter {name} (classes={})", self.vm().classes.len());
        }
        let started = Instant::now();
        let result = run(self.vm_mut()).map_err(|source| BootError::Stage {
            stage: name,
            source: Box::new(source),
        });
        if trace {
            eprintln!(
                "[boot] leave {name} in {:?} (classes={})",
                started.elapsed(),
                self.vm().classes.len()
            );
        }
        result
    }
}

/// A fresh, process-unique `startSeq` per boot so the ATMS `attachApplication`
/// / `finishAttachApplication` sequence check stays meaningful.
static NEXT_START_SEQ: AtomicU64 = AtomicU64::new(1);

/// Flatten a [`VmError`] the way the CLI reports it (for evidence logs).
#[must_use]
pub fn describe_vm_error(e: &VmError) -> String {
    format!("{}: {e}", e.code())
}

/// Seed the device environment the framework reads through
/// `android.system.Os.getenv` / `java.lang.System.getenv`.
///
/// `android.icu.platform.AndroidDataFiles.generateIcuDataPath()` builds the ICU
/// data search path from `ANDROID_I18N_ROOT` and `ANDROID_TZDATA_ROOT`; the
/// real device sets them to the i18n/tzdata apex mounts. ACOS extracts the ICU
/// `.dat` into the boot cache (`<boot>/apex/com.android.i18n/etc/icu/`) and
/// exposes it read-only through the already-mapped `/system/**` VFS subtree, so
/// the guest root is `/system/apex/com.android.i18n`. The tzdata path keeps the
/// real `/apex/com.android.tzdata` spelling because the VFS maps that prefix
/// directly. Existing host values are preserved (a real shell override wins).
fn seed_device_environment() {
    // `std::env::set_var` is process-global; only set the device default when
    // the host has not provided one.
    if std::env::var_os("ANDROID_I18N_ROOT").is_none() {
        std::env::set_var("ANDROID_I18N_ROOT", "/system/apex/com.android.i18n");
    }
    if std::env::var_os("ANDROID_TZDATA_ROOT").is_none() {
        std::env::set_var("ANDROID_TZDATA_ROOT", "/apex/com.android.tzdata");
    }
}
