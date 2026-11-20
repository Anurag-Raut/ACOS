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
use crate::boot::BootError;
use crate::client_transaction::ClientTransactionError;
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
    /// The launch `Activity` is resumed (`Activity.mResumed`), read from the
    /// framework rather than inferred from the app's logcat. Fixtures such as
    /// hello-jni never log `onResume`, so the log heuristic alone would
    /// false-block the `on_resume` launch rung.
    pub resumed: bool,
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
    /// An internal invariant failed (a framework object came back null, a
    /// configuration field was missing, ...). Not an app bug.
    #[error("internal: {0}")]
    Internal(String),
    /// A framework API the step needs is absent from the boot image.
    #[error("unsupported api: {api} (at {at})")]
    UnsupportedApi {
        /// The API that is missing.
        api: String,
        /// Where it was called from.
        at: String,
    },
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
            resumed: false,
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
            resumed: false,
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
        // A framework exception here (for example the API-34 traversal's
        // `KeyCharacterMap.load(-1)` on a device with no input service) must
        // not prevent the explicit frame drive below, so keep the error and
        // return it after the frame has been attempted.
        let transaction_error = run_phase(self.vm_mut()).err();
        report.transaction_delivered = activity_created(self.vm_mut(), thread, tx.token);
        report.resumed = activity_resumed(self.vm_mut(), thread, tx.token);
        acos_vm::profile::add_stage(
            "client_transaction",
            __t.elapsed().as_nanos().min(u64::MAX as u128) as u64,
        );

        // --- first frame -----------------------------------------------------
        // ACOS has no RenderThread and the framework's vsync-driven
        // `performDraw` does not reach `ThreadedRenderer.draw` in this build
        // (the traversal stops after `relayoutWindow`; see M2_LOG §W133-A).
        // Drive the *real* `ThreadedRenderer.draw(view, attachInfo, vr)` once
        // synchronously: it records the DecorView's display list through the
        // `gfx_*` RenderNode natives and submits the rasterised frame through
        // `nSyncAndDrawFrame` into the `FrameStore`. This is the same call the
        // framework makes from `ViewRootImpl.draw`, only invoked by ACOS
        // instead of the (absent) render thread.
        let __t = std::time::Instant::now();
        match self.render_frame_now() {
            Ok(()) => {}
            Err(e) => {
                eprintln!("render_frame_now: {e}");
            }
        }
        acos_vm::profile::add_stage(
            "first_frame",
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
        if let Some(e) = transaction_error {
            return Err(e);
        }
        Ok(app_thread)
    }

    /// Re-open the main looper, let pending input/vsync work run, and publish a
    /// fresh frame. This is the interaction pump: after `acos launch`, the
    /// bounded run quits the looper; an injected `MotionEvent` sits in the
    /// window `InputChannel` until a `nativePollOnce` on the main looper
    /// dispatches it. `pump` clears the quit flag, re-enters `Looper.loop()`
    /// when the main thread is not already looping, runs the scheduler, and
    /// draws the resulting tree.
    pub fn pump(&mut self) -> Result<(), LifecycleError> {
        set_main_looper_quitting(self.vm_mut(), 0)?;
        let in_loop = main_thread_in_looper(self.vm());
        if !in_loop {
            self.vm_mut()
                .push_static_call("android.os.Looper", "loop", "()V", &[])?;
            // The bounded launch run let `Looper.loop()` return, which leaves
            // the main green thread finished; re-arm it so the scheduler runs
            // the re-pushed loop (and its `nativePollOnce` input drain).
            if let Some(t) = self.vm_mut().threads.first_mut() {
                t.status = ThreadStatus::Runnable;
            }
        }
        // Interactions only need the turns that drain the injected event and
        // its immediate repaint. A repeating timer (the `EditText` cursor blink
        // posts every 500 ms of guest time) keeps the virtual-time pump busy
        // forever, so this is bounded instead of using the launch scheduler
        // budget.
        const INTERACTION_PUMP_TURNS: u32 = 2_000;
        // The injected event is delivered by an immediate looper wake, so the
        // pump services already-due work only and does **not** advance the
        // virtual clock to the next vsync or the `EditText` cursor blink (500
        // ms).
        //
        // The framework's response to a click (button press-state animation,
        // ripple, relayout) keeps the `Choreographer` scheduling a vsync per
        // frame; in the interpreter each frame is a full traversal (~1 s), and
        // W180-A measured 118 frames / 113 s for one tap after typing. The
        // visible result — the click handler's text change and a repaint — does
        // not need that animation to finish, so the pump arms a small vsync
        // budget and draws the resulting tree synchronously below. This bounds
        // the work; it does not skip the input dispatch or the render.
        const INTERACTION_FRAME_BUDGET: u32 = 3;
        acos_vm::natives::fwk_vsync::set_frame_budget(Some(INTERACTION_FRAME_BUDGET));
        let pumped = self.vm_mut().run_scheduler_due(INTERACTION_PUMP_TURNS);
        acos_vm::natives::fwk_vsync::set_frame_budget(None);
        pumped?;
        if self.vm().pending_exception != NULL {
            let h = self.vm_mut().pending_exception;
            self.vm_mut().pending_exception = NULL;
            return Err(LifecycleError::JavaException(describe_throwable(
                self.vm(),
                h,
            )));
        }
        self.emit_platform_events();
        self.render_frame_now()?;
        Ok(())
    }

    /// Turn the platform-service state that the pump advanced into structured
    /// evidence on the log/event stream.
    ///
    /// Today that is the toast: `Toast.show()` reaches
    /// `INotificationManager.enqueueTextToast`, whose text would be rendered by
    /// `NotificationManagerService` in the system process. ACOS records the real
    /// request; this emits it as one `AcosToast` record carrying
    /// `{"event":"toast","text":…,"duration_ms":…}` so an agent can observe the
    /// toast without screen-scraping, and exactly once per request.
    pub fn emit_platform_events(&mut self) {
        let Some(state) = self.host().borrow().notification_state() else {
            return;
        };
        let toasts = {
            let st = state.lock().unwrap();
            st.shown_toasts.clone()
        };
        let mut emitted = self.emitted_toasts;
        while emitted < toasts.len() {
            let toast = &toasts[emitted];
            // `{"event":"toast","text":…}`: a structured record, so an agent
            // reads the toast text from the event stream instead of scraping.
            let payload = format!(
                "{{\"event\":\"toast\",\"text\":{},\"package\":{},\"duration_ms\":{}}}",
                Self::json_quote(&toast.text),
                Self::json_quote(&toast.package),
                toast.duration_ms
            );
            os_log::push_record(4, "AcosToast", &payload, 0);
            emitted += 1;
        }
        self.emitted_toasts = emitted;
    }

    /// Quote and escape a string for the structured `AcosToast` payload.
    fn json_quote(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// Drive a real configuration-change relaunch of the launched activity.
    ///
    /// This is the framework's own path, not a re-launch shortcut:
    /// `ActivityThread.prepareRelaunchActivity` builds the relaunch
    /// `ActivityClientRecord` from a `MergedConfiguration`, and
    /// `ActivityThread.handleRelaunchActivity` then runs
    /// `handleRelaunchActivityInner`, which pauses, stops (calling the app's
    /// `onSaveInstanceState`), destroys, and launches the activity again with
    /// the saved `Bundle` — the exact sequence AMS triggers with a
    /// `ActivityRelaunchItem`.
    ///
    /// `orientation` is `1` (portrait) or `2` (landscape); the display override
    /// is updated first so the relaunched activity measures against the new
    /// screen.
    pub fn relaunch_activity(&mut self, orientation: i32) -> Result<(), LifecycleError> {
        use acos_vm::javabridge::JavaValue;

        // The display the app will measure against after the relaunch.
        if let Some(mut display) = acos_services::display::display_override() {
            let landscape = orientation == 2;
            let (w, h) = (display.width, display.height);
            if landscape != (w > h) {
                display.width = h;
                display.height = w;
            }
            acos_services::display::set_display_override(Some(display));
        }

        let thread = match self
            .vm_mut()
            .call_static(
                "android.app.ActivityThread",
                "currentActivityThread",
                "()Landroid/app/ActivityThread;",
                &[],
            )
            .ok()
            .and_then(|v| match v {
                JavaValue::Object(h) if h != NULL => Some(h),
                _ => None,
            }) {
            Some(t) => t,
            None => {
                return Err(LifecycleError::UnsupportedApi {
                    api: "ActivityThread.currentActivityThread()".into(),
                    at: "acos_runtime::lifecycle::relaunch_activity".into(),
                })
            }
        };
        // The new configuration: same locale/density, flipped geometry.
        let config = crate::bind_app::build_config(self.vm_mut())
            .map_err(|e| LifecycleError::Internal(e.to_string()))?;
        let (w_dp, h_dp) = {
            let vm = self.vm_mut();
            (
                match vm.get_field(config, "screenWidthDp") {
                    Ok(JavaValue::Int(v)) => v,
                    _ => 411,
                },
                match vm.get_field(config, "screenHeightDp") {
                    Ok(JavaValue::Int(v)) => v,
                    _ => 842,
                },
            )
        };
        {
            let vm = self.vm_mut();
            vm.set_field(config, "orientation", JavaValue::Int(orientation))?;
            if orientation == 2 {
                vm.set_field(config, "screenWidthDp", JavaValue::Int(h_dp.max(w_dp)))?;
                vm.set_field(config, "screenHeightDp", JavaValue::Int(w_dp.min(h_dp)))?;
            } else {
                vm.set_field(config, "screenWidthDp", JavaValue::Int(w_dp.min(h_dp)))?;
                vm.set_field(config, "screenHeightDp", JavaValue::Int(h_dp.max(w_dp)))?;
            }
        }
        let merged = self
            .vm_mut()
            .new_instance_ctor(
                "android.util.MergedConfiguration",
                "(Landroid/content/res/Configuration;)V",
                &[JavaValue::Object(config)],
            )
            .map_err(|e| LifecycleError::Internal(e.to_string()))?;
        let actions = self
            .vm_mut()
            .new_instance_ctor(
                "android.app.servertransaction.PendingTransactionActions",
                "()V",
                &[],
            )
            .map_err(|e| LifecycleError::Internal(e.to_string()))?;

        // The live activity's token: `ActivityThread.mActivities` is the
        // ArrayMap<IBinder, ActivityClientRecord> the launch registered. Using
        // the real token (not null) is what makes the framework tear the
        // existing activity down instead of creating a second one.
        let activity_token = {
            let vm = self.vm_mut();
            let map = match vm.get_field(thread, "mActivities") {
                Ok(JavaValue::Object(h)) if h != NULL => h,
                _ => {
                    return Err(LifecycleError::Internal(
                        "ActivityThread.mActivities is not readable".into(),
                    ))
                }
            };
            // Several records can be present (a restore replays the launch),
            // so pick the one whose `activity` field is live rather than
            // `keyAt(0)`: a stale record would make `handleRelaunchActivity`
            // look up an activity that no longer exists and silently do
            // nothing.
            let size = match vm.call_method(map, "size", "()I", &[]) {
                Ok(JavaValue::Int(n)) if n > 0 => n,
                _ => {
                    return Err(LifecycleError::Internal(
                        "ActivityThread.mActivities is empty (no launched activity)".into(),
                    ))
                }
            };
            let mut live = None;
            for i in (0..size).rev() {
                let Ok(JavaValue::Object(key)) =
                    vm.call_method(map, "keyAt", "(I)Ljava/lang/Object;", &[JavaValue::Int(i)])
                else {
                    continue;
                };
                if key == NULL {
                    continue;
                }
                let Ok(JavaValue::Object(rec)) = vm.call_method(
                    thread,
                    "getActivityClient",
                    "(Landroid/os/IBinder;)Landroid/app/ActivityThread$ActivityClientRecord;",
                    &[JavaValue::Object(key)],
                ) else {
                    continue;
                };
                if rec == NULL {
                    continue;
                }
                if matches!(vm.get_field(rec, "activity"), Ok(JavaValue::Object(a)) if a != NULL) {
                    live = Some(key);
                    break;
                }
            }
            match live {
                Some(k) => k,
                None => {
                    return Err(LifecycleError::Internal(
                        "no ActivityClientRecord has a live activity".into(),
                    ))
                }
            }
        };

        // `prepareRelaunchActivity(token, pendingResults, pendingNewIntents,
        // configChanges, mergedConfig, preserveWindow)` -> ActivityClientRecord.
        // Every config bit is "changed" (0xFFFF) because the fixture declares no
        // `android:configChanges`, so the framework must recreate the activity.
        const ALL_CONFIG_CHANGES: i32 = 0xFFFF;
        let record = match self.vm_mut().call_method(
            thread,
            "prepareRelaunchActivity",
            "(Landroid/os/IBinder;Ljava/util/List;Ljava/util/List;ILandroid/util/MergedConfiguration;Z)Landroid/app/ActivityThread$ActivityClientRecord;",
            &[
                JavaValue::Object(activity_token),
                JavaValue::Null,
                JavaValue::Null,
                JavaValue::Int(ALL_CONFIG_CHANGES),
                JavaValue::Object(merged),
                JavaValue::Boolean(false),
            ],
        ) {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            Ok(_) => {
                return Err(LifecycleError::Internal(
                    "prepareRelaunchActivity returned null (no live ActivityClientRecord)".into(),
                ))
            }
            Err(e) => return Err(e.into()),
        };

        let vm_reason = self
            .vm_mut()
            .new_string_rooted("relaunch")
            .map_err(|e| LifecycleError::Internal(e.to_string()))?;

        // The real destroy -> onCreate(savedInstanceState) sequence.
        self.vm_mut().call_method(
            thread,
            "handleRelaunchActivity",
            "(Landroid/app/ActivityThread$ActivityClientRecord;Landroid/app/servertransaction/PendingTransactionActions;)V",
            &[JavaValue::Object(record), JavaValue::Object(actions)],
        )?;
        if self.vm().pending_exception != NULL {
            let h = self.vm_mut().pending_exception;
            self.vm_mut().pending_exception = NULL;
            return Err(LifecycleError::JavaException(describe_throwable(
                self.vm(),
                h,
            )));
        }

        // `handleRelaunchActivity` ends at `handleLaunchActivity`, which runs
        // `onCreate`/`onStart` and stops; the resume is the transaction's
        // `ResumeActivityItem` (`handleResumeActivity`), which is what re-adds
        // the window (`makeVisible` -> `WindowManager.addView`). Without it the
        // recreated activity has no `ViewRootImpl` and nothing to draw.
        // `prepareRelaunchActivity` returns a *temporary* record (no
        // `activity`); the live record the destroy/launch updated is the one
        // registered under the token.
        let live_record = match self.vm_mut().call_method(
            thread,
            "getActivityClient",
            "(Landroid/os/IBinder;)Landroid/app/ActivityThread$ActivityClientRecord;",
            &[JavaValue::Object(activity_token)],
        ) {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            _ => {
                return Err(LifecycleError::Internal(
                    "the relaunched activity has no ActivityClientRecord".into(),
                ))
            }
        };
        // `TransactionExecutor` cycles ON_START before ON_RESUME; calling
        // `handleResumeActivity` alone resumes an activity that never started.
        self.vm_mut().call_method(
            thread,
            "handleStartActivity",
            "(Landroid/app/ActivityThread$ActivityClientRecord;Landroid/app/servertransaction/PendingTransactionActions;Landroid/app/ActivityOptions;)V",
            &[
                JavaValue::Object(live_record),
                JavaValue::Object(actions),
                JavaValue::Null,
            ],
        )?;
        if self.vm().pending_exception != NULL {
            let h = self.vm_mut().pending_exception;
            self.vm_mut().pending_exception = NULL;
            return Err(LifecycleError::JavaException(describe_throwable(
                self.vm(),
                h,
            )));
        }
        self.vm_mut().call_method(
            thread,
            "handleResumeActivity",
            "(Landroid/app/ActivityThread$ActivityClientRecord;ZZZLjava/lang/String;)V",
            &[
                JavaValue::Object(live_record),
                JavaValue::Boolean(false), // finalStateRequest
                JavaValue::Boolean(false), // isForward
                JavaValue::Boolean(false), // onlyLocalRequest
                JavaValue::Object(vm_reason),
            ],
        )?;
        if self.vm().pending_exception != NULL {
            let h = self.vm_mut().pending_exception;
            self.vm_mut().pending_exception = NULL;
            return Err(LifecycleError::JavaException(describe_throwable(
                self.vm(),
                h,
            )));
        }
        // A recreate needs a **real traversal**: the recreated window has no
        // measured bounds until `ViewRootImpl` runs `performTraversals` on the
        // next vsync, which the due-work pump (no clock advance) never reaches.
        // Advance the guest clock to the next vsyncs instead, bounded by a
        // frame budget so the press-state/animation frames cannot run away.
        const RELAUNCH_FRAME_BUDGET: u32 = 4;
        set_main_looper_quitting(self.vm_mut(), 0)?;
        if !main_thread_in_looper(self.vm()) {
            self.vm_mut()
                .push_static_call("android.os.Looper", "loop", "()V", &[])?;
            if let Some(t) = self.vm_mut().threads.first_mut() {
                t.status = ThreadStatus::Runnable;
            }
        }
        acos_vm::natives::fwk_vsync::set_frame_budget(Some(RELAUNCH_FRAME_BUDGET));
        let pumped = self.vm_mut().run_scheduler(true);
        acos_vm::natives::fwk_vsync::set_frame_budget(None);
        pumped?;
        if self.vm().pending_exception != NULL {
            let h = self.vm_mut().pending_exception;
            self.vm_mut().pending_exception = NULL;
            return Err(LifecycleError::JavaException(describe_throwable(
                self.vm(),
                h,
            )));
        }
        self.render_frame_now()?;
        Ok(())
    }

    /// Advance the guest clock through at most `frames` vsync frames, then draw.
    ///
    /// The interaction pump services only already-due work (a click's text
    /// change lands immediately), but some effects are *animated*: a
    /// `smoothScrollToPosition` or a dialog window transition only completes
    /// after a few `Choreographer` frames. This advances to those frames, with
    /// the frame budget as the hard stop, so a caller can ask for "one more
    /// frame" until the state change is visible without ever letting the
    /// animation storm run unbounded.
    pub fn pump_frames(&mut self, frames: u32) -> Result<(), LifecycleError> {
        set_main_looper_quitting(self.vm_mut(), 0)?;
        if !main_thread_in_looper(self.vm()) {
            self.vm_mut()
                .push_static_call("android.os.Looper", "loop", "()V", &[])?;
            if let Some(t) = self.vm_mut().threads.first_mut() {
                t.status = ThreadStatus::Runnable;
            }
        }
        acos_vm::natives::fwk_vsync::set_frame_budget(Some(frames));
        let pumped = self.vm_mut().run_scheduler(true);
        acos_vm::natives::fwk_vsync::set_frame_budget(None);
        pumped?;
        if self.vm().pending_exception != NULL {
            let h = self.vm_mut().pending_exception;
            self.vm_mut().pending_exception = NULL;
            return Err(LifecycleError::JavaException(describe_throwable(
                self.vm(),
                h,
            )));
        }
        self.render_frame_now()?;
        Ok(())
    }

    /// Drive the real `ThreadedRenderer.draw` for every live `ViewRootImpl`.
    ///
    /// This is the synchronous replacement for the framework's
    /// RenderThread/vsync draw: it runs `View.updateDisplayListIfDirty` ->
    /// `RenderNode.beginRecording`/`endRecording` -> `nSyncAndDrawFrame` through
    /// the real framework bytecode, so a `FrameStore` frame is published from
    /// the app's own `View.draw` and not from synthetic geometry.
    ///
    /// Returns `Ok(())` after attempting every root; a root without a live
    /// hardware renderer is skipped.
    pub fn render_frame_now(&mut self) -> Result<(), LifecycleError> {
        let vm = self.vm_mut();
        let global = match vm.call_static(
            "android.view.WindowManagerGlobal",
            "getInstance",
            "()Landroid/view/WindowManagerGlobal;",
            &[],
        ) {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            _ => return Ok(()),
        };
        vm.pending_exception = NULL;
        let roots = match vm.call_method(global, "getRootViews", "()Ljava/util/ArrayList;", &[]) {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            _ => {
                vm.pending_exception = NULL;
                match vm.get_field(global, "mRoots") {
                    Ok(JavaValue::Object(h)) if h != NULL => h,
                    _ => return Ok(()),
                }
            }
        };
        let count = match vm.call_method(roots, "size", "()I", &[]) {
            Ok(JavaValue::Int(n)) if n > 0 => n as usize,
            _ => {
                vm.pending_exception = NULL;
                return Ok(());
            }
        };
        for i in 0..count {
            let root = match vm.call_method(
                roots,
                "get",
                "(I)Ljava/lang/Object;",
                &[JavaValue::Int(i as i32)],
            ) {
                Ok(JavaValue::Object(h)) if h != NULL => h,
                _ => continue,
            };
            let view = match vm.get_field(root, "mView") {
                Ok(JavaValue::Object(h)) if h != NULL => h,
                _ => continue,
            };
            let attach = match vm.get_field(root, "mAttachInfo") {
                Ok(JavaValue::Object(h)) if h != NULL => h,
                _ => continue,
            };
            let renderer = match vm.get_field(attach, "mThreadedRenderer") {
                Ok(JavaValue::Object(h)) if h != NULL => h,
                _ => continue,
            };
            // Same call `ViewRootImpl.draw` makes: record the DecorView display
            // list and synchronously sync+draw it through the frame seam.
            let r = vm.call_method(
                renderer,
                "draw",
                "(Landroid/view/View;Landroid/view/View$AttachInfo;Landroid/view/ThreadedRenderer$DrawCallbacks;)V",
                &[
                    JavaValue::Object(view),
                    JavaValue::Object(attach),
                    JavaValue::Object(root),
                ],
            );
            if let Err(e) = r {
                vm.pending_exception = NULL;
                eprintln!("render_frame_now: ThreadedRenderer.draw failed: {e}");
            }
        }
        vm.pending_exception = NULL;
        Ok(())
    }
}

/// The `AcosFixture` messages currently in the process log sink.
///
/// This peeks the sink instead of draining it: a fixture that logs under its
/// own tag (e.g. `AcosCounter`) still has its `onResume` line visible to the
/// session's `acos logs` view and to the stage probe. The sink is cleared at
/// the start of each lifecycle run.
#[must_use]
pub fn fixture_log() -> LifecycleLog {
    os_log::records()
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
            c.authorities
                .iter()
                .map(move |authority| bind_app::ProviderSpec {
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
///
/// The looper is pumped with **virtual time**: the framework's main looper
/// parks on future-dated `Handler` messages (the `Choreographer` `doFrame`
/// posted at the next vsync, in particular), and `run_scheduler` advances the
/// guest monotonic clock to the earliest such deadline when every thread is
/// idle, so `performTraversals` -> `IWindowSession.relayout` actually runs.
/// This mirrors a device that simply waits for the vsync, without the real
/// 16.6 ms sleep per frame and with a deterministic timeline.
fn run_phase(vm: &mut Vm) -> Result<(), LifecycleError> {
    vm.run_scheduler(true)?;
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

/// True when the main green thread currently has an `android.os.Looper.loop`
/// frame on its stack (i.e. it is parked inside the loop rather than returned).
fn main_thread_in_looper(vm: &Vm) -> bool {
    vm.threads.first().is_some_and(|t| {
        t.frames.iter().any(|f| {
            let m = &vm.methods[f.method as usize];
            vm.class_desc(m.class) == "Landroid/os/Looper;" && m.name == "loop"
        })
    })
}

/// Set or clear the main `MessageQueue.mQuitting` flag and wake the looper.
fn set_main_looper_quitting(vm: &mut Vm, quitting: u32) -> Result<(), LifecycleError> {
    let looper = match vm.call_static(
        "android.os.Looper",
        "myLooper",
        "()Landroid/os/Looper;",
        &[],
    )? {
        JavaValue::Object(h) if h != NULL => h,
        _ => return Ok(()),
    };
    let queue = match vm.get_field(looper, "mQueue") {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        _ => return Ok(()),
    };
    vm.set_instance_field_by_name(queue, "mQuitting", quitting);
    let ptr = match vm.get_field(queue, "mPtr") {
        Ok(JavaValue::Long(p)) => p,
        _ => 0,
    };
    let _ = vm.call_method(queue, "nativeWake", "(J)V", &[JavaValue::Long(ptr)]);
    Ok(())
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

/// True when the launch `Activity` has run `onResume` (`Activity.mResumed` is
/// set by `Activity.performResume`). Read from the framework so fixtures that
/// do not override `onResume` are still detected.
fn activity_resumed(vm: &mut Vm, thread: Handle, token: Handle) -> bool {
    let record = match vm.call_method(
        thread,
        "getActivityClient",
        "(Landroid/os/IBinder;)Landroid/app/ActivityThread$ActivityClientRecord;",
        &[JavaValue::Object(token)],
    ) {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        _ => return false,
    };
    let activity = match vm.get_field(record, "activity") {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        _ => return false,
    };
    matches!(
        vm.get_field(activity, "mResumed"),
        Ok(JavaValue::Boolean(true))
    )
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
                acos_test_support::require_prereq(false, "describe_throwable_unwraps_exception_in_initializer_error", &format!("describe_throwable_unwraps_exception_in_initializer_error: {msg}"));
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
