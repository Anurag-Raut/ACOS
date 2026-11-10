//! Main-looper lifecycle driver (milestone M2).
//!
//! After the attach handshake, real Android delivers
//! `IApplicationThread.bindApplication` and then the `ClientTransaction`
//! stream. Both enqueue `ActivityThread.H` messages on the app's main looper;
//! the lifecycle callbacks (`Application.onCreate`, `Activity.onCreate`,
//! `onStart`, `onResume`) run when the main looper dispatches them.
//!
//! ACOS has no remote process and no zygote, so this module drives the real
//! framework bytecode directly:
//!
//! 1. push the real `android.os.Looper.loop()` frame (it blocks in
//!    `MessageQueue.nativePollOnce`, so it cannot go through the synchronous
//!    nested-call bridge — see [`Vm::push_static_call`]);
//! 2. deliver `bindApplication` with real Java parcelables;
//! 3. run the interpreter until the main thread parks in the looper (message
//!    queue drained);
//! 4. deliver the `ClientTransaction` stream;
//! 5. run the looper again, then `quitSafely()` so the bounded run terminates.
//!
//! Every step executes the real Android 14 boot-classpath bytecode. Nothing is
//! faked: a missing native or a framework failure surfaces as a structured
//! error and the partial log is still reported.

use std::path::Path;

use acos_apk::Apk;
use acos_services::pms::{AppInfo, FIXTURE_UID};
use acos_vm::heap::{Handle, NULL};
use acos_vm::javabridge::{BridgeError, JavaValue};
use acos_vm::natives::os_log;
use acos_vm::vm::ThreadStatus;
use acos_vm::{Vm, VmError};

use crate::bind_app::{self, BindAppError};
use crate::client_transaction::ClientTransactionError;
use crate::boot::BootError;
use crate::Runtime;

/// A lifecycle callback observed in the `AcosFixture` log.
pub type LifecycleLog = Vec<String>;

/// The result of driving the app process lifecycle.
#[derive(Debug, Clone)]
pub struct LifecycleReport {
    /// `AcosFixture` log messages, in order (`onCreate`, `onStart`, ...).
    pub events: LifecycleLog,
    /// Wall clock for the whole boot + lifecycle run.
    pub elapsed: std::time::Duration,
    /// VM classes loaded at the end.
    pub classes_loaded: usize,
    /// `bindApplication` was delivered to the real `ApplicationThread`.
    pub bind_delivered: bool,
    /// `ActivityThread.mInitialApplication` was non-null after the bind phase
    /// (i.e. `Application.attach` + `Application.onCreate` ran).
    pub app_created: bool,
    /// `IApplicationThread.scheduleTransaction` accepted the launch/resume
    /// `ClientTransaction` (the real app thread posted `H.EXECUTE_TRANSACTION`).
    pub transaction_scheduled: bool,
    /// The transaction ran far enough that `ActivityThread.mActivities` holds a
    /// record with a live `Activity` (i.e. `Activity.onCreate` returned).
    pub transaction_delivered: bool,
    /// The main looper was asked to `quitSafely()` and returned.
    pub looper_quit: bool,
    /// The first structured failure, if any. Populated even when the lifecycle
    /// log was partially produced.
    pub error: Option<String>,
}

impl LifecycleReport {
    /// The first observed callback, if any.
    #[must_use]
    pub fn first(&self) -> Option<&str> {
        self.events.first().map(String::as_str)
    }
}

/// A structured lifecycle-driver failure.
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    /// The initial boot (through `attachApplication`) failed.
    #[error("boot: {0}")]
    Boot(#[from] BootError),
    /// `bindApplication` delivery failed.
    #[error("bindApplication: {0}")]
    Bind(#[from] BindAppError),
    /// Building/delivering the launch `ClientTransaction` failed.
    #[error("client transaction: {0}")]
    ClientTransaction(#[from] ClientTransactionError),
    /// A bridge call failed.
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    /// A VM-level failure.
    #[error("vm: {0}")]
    Vm(#[from] VmError),
    /// Configuring the in-process services for the launched APK failed.
    #[error("service setup: {0}")]
    Service(#[from] acos_services::ServiceError),
    /// A step is not wired yet. Returned instead of pretending the work was
    /// done; the message names the exact missing piece.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The real framework threw an uncaught Java exception. Carries the fully
    /// rendered `Class: message` plus the recorded stack frames, not just a
    /// handle, so the blocker is diagnosable from the report alone.
    #[error("java exception: {0}")]
    JavaException(String),
}

impl Runtime {
    /// Boot the real framework, deliver `bindApplication` + the launch/resume
    /// transaction, run the main looper, and return the observed lifecycle log.
    ///
    /// The `error` field of the report is populated on a structured failure;
    /// the partial log is always returned so callers can see how far the real
    /// framework got.
    pub fn boot_and_run_lifecycle(&mut self, apk_path: impl AsRef<Path>) -> LifecycleReport {
        let started = std::time::Instant::now();
        // The log sink is process-wide; drop anything from earlier runs.
        let _ = os_log::drain();
        let mut report = LifecycleReport {
            events: Vec::new(),
            elapsed: std::time::Duration::ZERO,
            classes_loaded: 0,
            bind_delivered: false,
            app_created: false,
            transaction_scheduled: false,
            transaction_delivered: false,
            looper_quit: false,
            error: None,
        };
        match self.run_lifecycle_inner(apk_path.as_ref(), &mut report) {
            Ok(app_thread) => {
                let _ = app_thread;
            }
            Err(e) => {
                report.error = Some(format!(
                    "{e}\n  java-tail: {}\n  native-tail: {}",
                    java_tail(self.vm()),
                    native_tail(self.vm())
                ));
            }
        }
        report.events = fixture_log();
        report.classes_loaded = self.vm().classes.len();
        report.elapsed = started.elapsed();
        report
    }

    /// [`Runtime::boot_and_run_lifecycle`] for a `Vm` restored from a
    /// framework-init snapshot (T-43b).
    ///
    /// The app DEX is parsed and added first, exactly as `boot_app_process`
    /// would have done, then the same real framework lifecycle runs. Any
    /// failure is reported in the returned [`LifecycleReport`], never as a
    /// panic, so the caller can fall back or report the honest blocker.
    pub fn boot_and_run_lifecycle_from_boot(
        &mut self,
        apk_path: impl AsRef<Path>,
        activity_thread: Handle,
    ) -> LifecycleReport {
        let started = std::time::Instant::now();
        let _ = os_log::drain();
        let mut report = LifecycleReport {
            events: Vec::new(),
            elapsed: std::time::Duration::ZERO,
            classes_loaded: 0,
            bind_delivered: false,
            app_created: false,
            transaction_scheduled: false,
            transaction_delivered: false,
            looper_quit: false,
            error: None,
        };
        let inner = (|| -> Result<Handle, LifecycleError> {
            let apk_path = apk_path.as_ref();
            let __t = std::time::Instant::now();
            self.host()
                .borrow_mut()
                .install_package_from_apk(apk_path)?;
            acos_vm::profile::add_stage(
                "install_package",
                __t.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            );
            let __t = std::time::Instant::now();
            self.load_app_dex(apk_path)?;
            acos_vm::profile::add_stage(
                "load_app_dex",
                __t.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            );
            self.run_lifecycle_after_boot(apk_path, activity_thread, &mut report)
        })();
        if let Err(e) = inner {
            report.error = Some(format!(
                "{e}\n  java-tail: {}\n  native-tail: {}",
                java_tail(self.vm()),
                native_tail(self.vm())
            ));
        }
        report.events = fixture_log();
        report.classes_loaded = self.vm().classes.len();
        report.elapsed = started.elapsed();
        report
    }

    /// Parse and add the APK's first `classes.dex`, as `boot_app_process` does.
    pub fn load_app_dex(&mut self, apk_path: &Path) -> Result<(), LifecycleError> {
        let apk = Apk::open(apk_path).map_err(|e| LifecycleError::Boot(BootError::Apk(e)))?;
        let dex = apk
            .dex_files
            .first()
            .ok_or(LifecycleError::Boot(BootError::NoDex))?;
        let parsed = acos_dex::DexFile::parse(dex.bytes.clone())
            .map_err(|e| LifecycleError::Boot(BootError::Dex(e)))?;
        self.vm_mut().add_dex(parsed);
        Ok(())
    }

    fn run_lifecycle_inner(
        &mut self,
        apk_path: &Path,
        report: &mut LifecycleReport,
    ) -> Result<Handle, LifecycleError> {
        // The in-process PMS is seeded with the `hello-views` fixture; point it
        // at the APK actually being launched before the framework asks for
        // `getApplicationInfo`, or `handleBindApplication` raises
        // `unknown service name <package>`.
        self.host()
            .borrow_mut()
            .install_package_from_apk(apk_path)?;
        let boot = self.boot_app_process(apk_path)?;
        self.run_lifecycle_after_boot(apk_path, boot.activity_thread, report)
    }

    /// Drive `bindApplication` + the launch transaction on a `Runtime` whose
    /// framework `Vm` was already booted (or restored from an ACOSSNAP boot
    /// image).
    fn run_lifecycle_after_boot(
        &mut self,
        apk_path: &Path,
        thread: Handle,
        report: &mut LifecycleReport,
    ) -> Result<Handle, LifecycleError> {
        let mut app = app_info_for(apk_path)?;
        // The framework's `LoadedApk`/`PathClassLoader`/`AssetManager` must see
        // a guest path, not the host fixture path. Stage it before the bind.
        crate::apk_loader::stage(self.vm(), apk_path, &mut app)
            .map_err(|e| LifecycleError::Bind(BindAppError::Materialize(e)))?;

        // Enter the real `Looper.loop()` under the interpreter scheduler: it
        // parks the main thread in `MessageQueue.next()` and returns when the
        // queue is quit.
        self.vm_mut()
            .push_static_call("android.os.Looper", "loop", "()V", &[])?;

        // --- bindApplication -------------------------------------------------
        let __t = std::time::Instant::now();
        let providers = provider_specs(apk_path)?;
        let app_thread =
            bind_app::deliver_bind_application(self.vm_mut(), thread, &app, &providers)?;
        report.bind_delivered = true;
        run_phase(self.vm_mut())?;
        report.app_created = initial_application_non_null(self.vm_mut(), thread);
        acos_vm::profile::add_stage(
            "bind_application",
            __t.elapsed().as_nanos().min(u64::MAX as u128) as u64,
        );

        // --- ClientTransaction (launch + resume) -----------------------------
        let __t = std::time::Instant::now();
        let tx = deliver_launch_transaction(self.vm_mut(), app_thread, &app, thread, apk_path)?;
        report.transaction_scheduled = true;
        run_phase(self.vm_mut())?;
        report.transaction_delivered = activity_created(self.vm_mut(), thread, tx.token);
        acos_vm::profile::add_stage(
            "client_transaction",
            __t.elapsed().as_nanos().min(u64::MAX as u128) as u64,
        );

        // --- bounded shutdown ------------------------------------------------
        let __t = std::time::Instant::now();
        quit_main_looper(self.vm_mut())?;
        run_phase(self.vm_mut())?;
        report.looper_quit = true;
        acos_vm::profile::add_stage(
            "shutdown",
            __t.elapsed().as_nanos().min(u64::MAX as u128) as u64,
        );
        Ok(app_thread)
    }
}

/// The `AcosFixture` messages currently in the process log sink.
#[must_use]
pub fn fixture_log() -> LifecycleLog {
    os_log::drain()
        .into_iter()
        .filter(|r| r.tag == "AcosFixture")
        .map(|r| r.msg)
        .collect()
}

/// Materialise the fixture `ApplicationInfo` description from the APK manifest.
fn app_info_for(apk_path: &Path) -> Result<AppInfo, LifecycleError> {
    let apk = Apk::open(apk_path).map_err(|e| LifecycleError::Boot(BootError::Apk(e)))?;
    let manifest = apk
        .manifest()
        .map_err(|e| LifecycleError::Bind(BindAppError::Materialize(format!("manifest: {e}"))))?;
    Ok(AppInfo::from_manifest(&manifest, apk_path, FIXTURE_UID))
}

/// The `(class, authority)` pairs of the app's manifest-declared providers.
///
/// `bindApplication`'s `ProviderInfoList` is what makes the real
/// `ActivityThread.installContentProviders` instantiate them; without it no
/// provider ever exists in the app process.
fn provider_specs(apk_path: &Path) -> Result<Vec<bind_app::ProviderSpec>, LifecycleError> {
    let apk = Apk::open(apk_path).map_err(|e| LifecycleError::Boot(BootError::Apk(e)))?;
    let manifest = apk
        .manifest()
        .map_err(|e| LifecycleError::Bind(BindAppError::Materialize(format!("manifest: {e}"))))?;
    Ok(manifest
        .providers
        .iter()
        .flat_map(|c| {
            // The manifest's `android:permission` is the default for both read
            // and write (the APK model has no separate read/write attributes).
            let shared = c.permission.clone().unwrap_or_default();
            let class = acos_apk::manifest::fq_name(&manifest.package, &c.name);
            let exported = c.exported.unwrap_or(false);
            c.authorities.iter().map(move |authority| bind_app::ProviderSpec {
                class: class.clone(),
                authority: authority.clone(),
                read_permission: shared.clone(),
                write_permission: shared.clone(),
                exported,
            })
        })
        .collect())
}

/// Run the interpreter until no green thread is runnable (the main looper has
/// drained its queue and parked) or a structured error surfaces.
fn run_phase(vm: &mut Vm) -> Result<(), LifecycleError> {
    acos_vm::interp::run(vm)?;
    if vm.pending_exception != NULL {
        let h = vm.pending_exception;
        vm.pending_exception = NULL;
        return Err(LifecycleError::JavaException(describe_throwable(vm, h)));
    }
    Ok(())
}

/// Render an uncaught `Throwable` as `Class: message` plus its recorded
/// frames. Mirrors [`Vm::describe_uncaught`] without needing the private
/// helper: `class_of_object` / `class_desc_of_id` / `throwable_message` and
/// `throwable_stacks` are all public.
fn describe_throwable(vm: &Vm, h: Handle) -> String {
    let mut out = String::new();
    // Walk the cause chain so a wrapper such as `ExceptionInInitializerError`
    // cannot hide the failed `<clinit>`. Bounded so a cyclic/self-referential
    // chain (or a corrupted handle) cannot loop forever.
    let mut current = Some(h);
    let mut depth = 0;
    while let Some(cur) = current {
        if depth > 0 {
            out.push_str("\n  caused by: ");
        }
        let class = vm
            .class_of_object(cur)
            .map(|c| vm.class_desc_of_id(c))
            .unwrap_or_else(|| "java.lang.Throwable".to_string());
        match vm.throwable_message(cur) {
            Some(m) => out.push_str(&format!("{class}: {m}")),
            None => out.push_str(&class),
        }
        if let Some(entries) = vm.throwable_stacks.get(&cur) {
            for e in entries {
                out.push_str(&format!("\n    at {}.{}({})", e.class, e.method, e.line));
            }
        }
        current = if depth >= MAX_CAUSE_DEPTH {
            out.push_str("\n  ... cause chain truncated");
            None
        } else {
            vm.throwable_cause(cur)
        };
        depth += 1;
    }
    out
}

/// Bound on how deep [`describe_throwable`] follows the cause chain.
const MAX_CAUSE_DEPTH: usize = 8;

/// The last 15 natives invoked for the first time, in invocation order. The
/// tail localises the framework call that precedes an otherwise shallow
/// exception stack.
fn native_tail(vm: &Vm) -> String {
    let Some(tr) = vm.native_trace.as_ref() else {
        return "(trace disabled)".to_string();
    };
    let start = 0; // DEBUG: full native trace
    let lines: Vec<String> = tr.hits[start..]
        .iter()
        .map(|hit| format!("{}->{}{}", hit.class, hit.method, hit.signature))
        .collect();
    format!("\n    {}", lines.join("\n    "))
}

/// The last 20 Java frames on the main thread, innermost first.
fn java_tail(vm: &Vm) -> String {
    let Some(thread) = vm.threads.first() else {
        return "(no thread)".to_string();
    };
    let start = thread.frames.len().saturating_sub(20);
    let lines: Vec<String> = thread.frames[start..]
        .iter()
        .rev()
        .map(|f| {
            let m = &vm.methods[f.method as usize];
            format!("{}.{}{}", vm.class_desc(m.class), m.name, m.sig)
        })
        .collect();
    format!("\n    {}", lines.join("\n    "))
}

fn initial_application_non_null(vm: &mut Vm, thread: Handle) -> bool {
    matches!(
        vm.get_field(thread, "mInitialApplication"),
        Ok(JavaValue::Object(h)) if h != NULL
    )
}

fn main_thread_parked(vm: &Vm) -> bool {
    vm.threads
        .first()
        .is_some_and(|t| t.status == ThreadStatus::Parked)
}

/// Ask the current main looper to quit so the bounded run terminates.
fn quit_main_looper(vm: &mut Vm) -> Result<(), LifecycleError> {
    if !main_thread_parked(vm) {
        return Ok(());
    }
    let looper = match vm.call_static(
        "android.os.Looper",
        "myLooper",
        "()Landroid/os/Looper;",
        &[],
    )? {
        JavaValue::Object(h) if h != NULL => h,
        _ => return Ok(()),
    };
    // The process main looper's `MessageQueue` is constructed with
    // `quitAllowed=false`, so the framework's own `Looper.quitSafely()` ->
    // `MessageQueue.quit(true)` correctly throws
    // `IllegalStateException: Main thread not allowed to quit.` (the real
    // `ActivityThread.main` never quits its looper; the OS kills the process).
    // ACOS must terminate its bounded run, so it performs exactly what
    // `MessageQueue.quit()` does *after* its guard: set `mQuitting`, then wake
    // the parked `nativePollOnce`. `MessageQueue.next()` observes the flag and
    // returns null, so `Looper.loop()` returns normally instead of throwing.
    let queue = match vm.get_field(looper, "mQueue") {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        _ => return Ok(()),
    };
    vm.set_instance_field_by_name(queue, "mQuitting", 1);
    let ptr = match vm.get_field(queue, "mPtr") {
        Ok(JavaValue::Long(p)) => p,
        _ => 0,
    };
    let _ = vm.call_method(queue, "nativeWake", "(J)V", &[JavaValue::Long(ptr)]);
    Ok(())
}

/// Deliver the launch + resume `ClientTransaction` for the fixture activity.
///
/// This is the real T-23 path: a `ClientTransaction` carrying a
/// `LaunchActivityItem` callback and a `ResumeActivityItem` lifecycle request is
/// built as real framework objects and handed to the real
/// `ApplicationThread.scheduleTransaction` bytecode. See
/// [`crate::client_transaction`].
fn deliver_launch_transaction(
    vm: &mut Vm,
    app_thread: Handle,
    app: &AppInfo,
    _thread: Handle,
    apk_path: &Path,
) -> Result<crate::client_transaction::LaunchTransaction, LifecycleError> {
    Ok(crate::client_transaction::deliver_launch(
        vm, app_thread, app, apk_path,
    )?)
}

/// True when `ActivityThread.mActivities` has a record for `token` whose
/// `activity` field was set (i.e. `Activity.onCreate` completed).
fn activity_created(vm: &mut Vm, thread: Handle, token: Handle) -> bool {
    let record = match vm.call_method(
        thread,
        "getActivityClient",
        "(Landroid/os/IBinder;)Landroid/app/ActivityThread$ActivityClientRecord;",
        &[JavaValue::Object(token)],
    ) {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        _ => return false,
    };
    matches!(
        vm.get_field(record, "activity"),
        Ok(JavaValue::Object(h)) if h != NULL
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_vm::{CoreLib, VmOptions};

    /// Regression (W40-A): `describe_throwable` must follow the
    /// `ExceptionInInitializerError.exception` / `Throwable.cause` chain, or a
    /// failed `<clinit>` is reported as a frame-less wrapper with no clue.
    #[test]
    fn describe_throwable_unwraps_exception_in_initializer_error() {
        let mut rt = match Runtime::standard_with(VmOptions {
            corelib: CoreLib::Real,
            ..VmOptions::default()
        }) {
            Ok(rt) => rt,
            Err(VmError::BootClasspathMissing(msg)) => {
                eprintln!("SKIP describe_throwable_unwraps_exception_in_initializer_error: {msg}");
                return;
            }
            Err(e) => panic!("Runtime::standard_with failed: {e}"),
        };
        let vm = rt.vm_mut();
        let cause = vm
            .new_throwable("Ljava/util/MissingResourceException;", Some("bundle"))
            .expect("cause");
        vm.native_roots.push(cause);
        let wrapper = vm
            .new_throwable("Ljava/lang/ExceptionInInitializerError;", None)
            .expect("wrapper");
        vm.native_roots.push(wrapper);
        vm.set_instance_field_by_name(wrapper, "exception", cause);
        vm.native_roots.pop();
        vm.native_roots.pop();

        let text = describe_throwable(vm, wrapper);
        assert!(text.contains("ExceptionInInitializerError"), "{text}");
        assert!(text.contains("caused by:"), "{text}");
        assert!(text.contains("MissingResourceException: bundle"), "{text}");
    }
}
