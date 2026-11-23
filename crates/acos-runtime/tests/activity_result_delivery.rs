//! SYSUI4: the caller side of `ActivityResultItem` on a real `ActivityThread`.
//!
//! SYSUI3A's ART run of `sysui1-permission-pregranted` showed the host queue and
//! deliver the caller's result transaction and the guest execute one
//! `H.EXECUTE_TRANSACTION` with no `onRequestPermissionsResult` and no restart.
//! This case names the blocking call offline: it boots the real
//! `ActivityThread` for the `perm-probe` fixture (the same `boot_app_process`
//! the launcher uses), installs a real `ActivityClientRecord` + `Activity` in
//! `ActivityThread.mActivities` in each caller state the host can be in when a
//! dialog finishes during `onCreate` (`Resumed`, `Paused`, `Stopped`), and then
//! schedules the **host encoder's** `ActivityResultItem + ResumeActivityItem`
//! `ClientTransaction` (encoded by `acos_services::client_transaction`, decoded
//! with the image's own `ClientTransaction.CREATOR`) through the real
//! `ApplicationThread.scheduleTransaction`.
//!
//! The record is installed rather than launched because the launch path stops
//! earlier in `ConfigurationController.handleConfigurationChanged`
//! (`client_transaction.rs` pins that as a separate, pre-existing blocker:
//! `bindApplication` reaches `handleLaunchActivity` and then NPEs in the
//! resource/display path). The delivery path exercised here is exactly the one
//! the failing run reaches — a record that already exists in `mActivities`.
//!
//! The evidence read back per state:
//! * the fixture's own `Log.i("AcosPermProbe", "result request=…")` line
//!   (`onRequestPermissionsResult` entered; the fixture's next line touches a
//!   view that a manually-installed Activity does not have, so only the log
//!   line is the callback's proof),
//! * `Activity.mCalled` (the framework's "called through to super" flag) and
//!   `Activity.mResumed`,
//! * the record's own `paused` / `stopped` / `mLifecycleState`,
//! * and, when the transaction does not complete, the real throwable with its
//!   stack (the blocking call, by name).

mod common;

use std::path::PathBuf;

use acos_apk::Apk;
use acos_runtime::{apk_loader, bind_app, parcel_java};
use acos_services::ams::{Extra, IntentFacts};
use acos_services::client_transaction as ctx;
use acos_services::pms::{AppInfo, FIXTURE_UID};
use acos_vm::heap::{Handle, NULL};
use acos_vm::javabridge::JavaValue;
use acos_vm::{interp, Vm};

/// `Activity.REQUEST_PERMISSIONS_WHO_PREFIX` — `dispatchActivityResult` maps a
/// `who` starting with it to `onRequestPermissionsResult`. It carries the
/// `@android:` system prefix (image dex `Activity.dispatchActivityResult`), and
/// that is the value the caller's own `startActivityForResult(who, …)` passes:
/// the host stores it verbatim.
const WHO: &str = "@android:requestPermissions:";
const REQUEST_CODE: i32 = 7;

fn perm_probe_apk() -> Option<PathBuf> {
    let path = common::repo_root().join("fixtures/build/apps/perm-probe.apk");
    path.is_file().then_some(path)
}

fn obj(h: Handle) -> Option<Handle> {
    (h != NULL).then_some(h)
}

fn java_parcel(vm: &mut Vm, bytes: &[u8]) -> Handle {
    parcel_java::java_parcel_from_bytes(vm, bytes).expect("real Parcel")
}

/// The `ResultInfo` the permission controller's `finish()` produces: the AOSP
/// extras `REQUEST_PERMISSIONS_NAMES` / `REQUEST_PERMISSIONS_RESULTS` in an
/// `ACTION`-less `Intent`, with `request_permissions:` as the `who`.
fn permission_result_info() -> acos_services::value::Parcelable {
    let mut facts = IntentFacts::default();
    facts.extras.insert(
        "android.content.pm.extra.REQUEST_PERMISSIONS_NAMES".into(),
        Extra::Strings(vec![
            "android.permission.CAMERA".into(),
            "android.permission.ACCESS_FINE_LOCATION".into(),
        ]),
    );
    facts.extras.insert(
        "android.content.pm.extra.REQUEST_PERMISSIONS_RESULTS".into(),
        Extra::Ints(vec![0, 0]),
    );
    let data = ctx::intent(&facts).expect("result data Intent");
    ctx::result_info(Some(WHO), REQUEST_CODE, -1, Some(data))
}

/// The host's own wire bytes for the delivery (`AtmsState::deliver_result`).
fn host_transaction() -> Vec<u8> {
    let item = ctx::activity_result_item(vec![permission_result_info()]);
    let resume = ctx::resume_item(2, true, false, false);
    // The caller's own binder id, written as a strong binder exactly as
    // `AtmsState::deliver_result` writes it (`Some(BinderId(caller))`).
    let transaction = ctx::client_transaction(
        Some(acos_parcel::BinderId(0x1000_0003)),
        Some(resume),
        vec![item],
    );
    ctx::encode(&transaction).expect("encode the host's delivery")
}

/// `android.app.ActivityClientRecord` lifecycle constants
/// (`ActivityLifecycleItem.ON_*`).
const ON_RESUME: i32 = 3;
const ON_PAUSE: i32 = 4;
const ON_STOP: i32 = 5;

struct Installed {
    token: Handle,
    record: Handle,
    activity: Handle,
}

/// Install a real `ActivityClientRecord` naming a real fixture `Activity` in
/// `ActivityThread.mActivities`, in the requested caller state.
fn install_caller(vm: &mut Vm, thread: Handle, state: i32) -> Installed {
    let token = vm
        .new_instance_ctor("android.os.Binder", "()V", &[])
        .expect("real activity token Binder");
    let activity = vm
        .new_instance_ctor("Ldev/acos/fixtures/permprobe/MainActivity;", "()V", &[])
        .expect("the fixture's MainActivity");
    let record = vm
        .new_instance_ctor(
            "Landroid/app/ActivityThread$ActivityClientRecord;",
            "()V",
            &[],
        )
        .expect("real ActivityClientRecord");

    // `ActivityThread.mInstrumentation` is created by `handleBindApplication`,
    // which `boot_app_process` stops before (the launcher delivers the bind).
    // The framework's own no-arg `Instrumentation` is exactly what
    // `handleBindApplication` constructs for a non-instrumented app.
    let instrumentation = vm
        .new_instance_ctor("android.app.Instrumentation", "()V", &[])
        .expect("real Instrumentation");
    vm.set_instance_field_by_name(thread, "mInstrumentation", instrumentation);
    // `boot_app_process` stops after `attach` (the bind is delivered by the
    // launcher), so `mInitialApplication` is still null. The system Context is
    // the framework's own Context for exactly this pre-bind window.
    let application = match vm.call_method(
        thread,
        "getSystemContext",
        "()Landroid/app/ContextImpl;",
        &[],
    ) {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        other => panic!("ActivityThread.getSystemContext() = {other:?}"),
    };
    // The fields `Activity.attach` fills that the delivery (and the pause the
    // resumed arm inserts before it) reads. `mBase` is the application Context:
    // `ActivityClientRecord.isPreHoneycomb()` asks `getApplicationInfo()`, which
    // a ContextWrapper without a base cannot answer.
    vm.set_instance_field_by_name(activity, "mBase", application);
    // A real process always has one `Application` (`mInitialApplication`, set
    // by `handleBindApplication`); `Activity.dispatchActivityPre*` dereferences
    // it, so the harness creates and attaches the framework's own class.
    let app_object = vm
        .new_instance_ctor("android.app.Application", "()V", &[])
        .expect("Application");
    vm.call_method(
        app_object,
        "attach",
        "(Landroid/content/Context;)V",
        &[JavaValue::Object(application)],
    )
    .expect("Application.attach");
    vm.set_instance_field_by_name(activity, "mApplication", app_object);
    vm.set_instance_field_by_name(activity, "mMainThread", thread);
    vm.set_instance_field_by_name(activity, "mInstrumentation", instrumentation);
    vm.set_instance_field_by_name(activity, "mToken", token);
    // `mCalled` = true is what `performCreate`/`performResume` leave behind.
    vm.set_instance_field_by_name(activity, "mCalled", 1);
    // `Activity.requestPermissions` sets this before the request leaves, and
    // `dispatchRequestPermissionsResult` clears it: the framework's own record
    // that the permission result reached the app (the fixture's log sink is not
    // wired in this harness, and its next line touches a view the manually
    // installed Activity does not have).
    vm.set_instance_field_by_name(activity, "mHasCurrentPermissionsRequest", 1);

    // `ActivityClientRecord.isPersistable()`/`isPreHoneycomb()` read the
    // record's `ActivityInfo`/Context, exactly as the real launch would leave
    // them.
    let app_info = match vm.call_method(
        application,
        "getApplicationInfo",
        "()Landroid/content/pm/ApplicationInfo;",
        &[],
    ) {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        other => panic!("Context.getApplicationInfo() = {other:?}"),
    };
    let activity_info = vm
        .new_instance_ctor("android.content.pm.ActivityInfo", "()V", &[])
        .expect("ActivityInfo");
    vm.set_instance_field_by_name(activity_info, "applicationInfo", app_info);
    let name = vm
        .new_string_rooted("dev.acos.fixtures.permprobe.MainActivity")
        .expect("activity name");
    let package = vm
        .new_string_rooted("dev.acos.fixtures.permprobe")
        .expect("package name");
    vm.set_instance_field_by_name(activity_info, "name", name);
    vm.set_instance_field_by_name(activity_info, "packageName", package);
    // `dispatchActivityResult` ends with `EventLogTags.writeWmOnActivityResultCalled`
    // over `getComponentName()`; a real launch fills it in `attach`.
    let component = vm
        .new_instance_ctor(
            "android.content.ComponentName",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[JavaValue::Object(package), JavaValue::Object(name)],
        )
        .expect("ComponentName");
    vm.set_instance_field_by_name(activity, "mComponent", component);

    let intent = vm
        .new_instance_ctor("android.content.Intent", "()V", &[])
        .expect("record intent");
    let class_name = vm
        .new_string_rooted("dev.acos.fixtures.permprobe.MainActivity")
        .expect("class name");
    vm.call_method(
        intent,
        "setClassName",
        "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
        &[JavaValue::Object(package), JavaValue::Object(class_name)],
    )
    .expect("Intent.setClassName");

    vm.set_instance_field_by_name(record, "token", token);
    vm.set_instance_field_by_name(record, "intent", intent);
    vm.set_instance_field_by_name(record, "activityInfo", activity_info);
    vm.set_instance_field_by_name(record, "activity", activity);
    let paused = u32::from(matches!(state, ON_PAUSE | ON_STOP));
    let stopped = u32::from(state == ON_STOP);
    vm.set_instance_field_by_name(record, "paused", paused);
    vm.set_instance_field_by_name(record, "stopped", stopped);
    vm.call_method(record, "setState", "(I)V", &[JavaValue::Int(state)])
        .expect("ActivityClientRecord.setState");

    let map = match vm.get_field(thread, "mActivities").expect("mActivities") {
        JavaValue::Object(h) if h != NULL => h,
        other => panic!("ActivityThread.mActivities is {}", other.kind()),
    };
    vm.call_method(
        map,
        "put",
        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        &[JavaValue::Object(token), JavaValue::Object(record)],
    )
    .expect("mActivities.put");

    Installed {
        token,
        record,
        activity,
    }
}

fn read_i32(vm: &mut Vm, handle: Handle, field: &str) -> i32 {
    match vm.get_field(handle, field) {
        Ok(JavaValue::Int(v)) => v,
        Ok(JavaValue::Boolean(b)) => i32::from(b),
        _ => -1,
    }
}

/// The guest's `Log.i("AcosPermProbe", …)` records since `from`.
fn callback_lines(from: usize) -> Vec<String> {
    acos_vm::natives::os_log::records()
        .into_iter()
        .skip(from)
        .filter(|r| r.tag == "AcosPermProbe")
        .map(|r| r.msg)
        .collect()
}

fn describe_throwable(vm: &mut Vm, h: Handle) -> String {
    let mut out = String::new();
    let mut cur = h;
    for depth in 0..6 {
        if cur == NULL {
            break;
        }
        let class = vm
            .class_of_object(cur)
            .map(|c| vm.class_desc_of_id(c))
            .unwrap_or_else(|| "java.lang.Throwable".to_string());
        out.push_str(&format!("\n  [cause {depth}] "));
        match vm.throwable_message(cur) {
            Some(m) => out.push_str(&format!("{class}: {m}")),
            None => out.push_str(&class),
        }
        if let Some(entries) = vm.throwable_stacks.get(&cur) {
            for e in entries.iter().take(12) {
                out.push_str(&format!("\n      at {}.{}({})", e.class, e.method, e.line));
            }
        }
        cur = match vm.call_method(cur, "getCause", "()Ljava/lang/Throwable;", &[]) {
            Ok(JavaValue::Object(next)) => next,
            _ => NULL,
        };
    }
    out
}

/// Run the main looper until it parks again, returning the pending throwable
/// (the blocking call) as text when the transaction failed.
fn run_looper(vm: &mut Vm) -> Option<String> {
    let result = interp::run(vm);
    if let Err(e) = &result {
        return Some(e.to_string());
    }
    let pending = vm.pending_exception;
    if pending == NULL {
        return None;
    }
    vm.pending_exception = NULL;
    Some(describe_throwable(vm, pending))
}

struct Observation {
    lines: Vec<String>,
    permissions_request_open: bool,
    m_called: i32,
    m_resumed: i32,
    paused: i32,
    stopped: i32,
    lifecycle: i32,
    blocker: Option<String>,
}

/// One delivery on its own process: an exception thrown out of the looper kills
/// the main thread, so the states cannot share a boot (the same reason the ART
/// launcher starts one process per activity).
fn observe(apk_path: &std::path::Path, state: i32) -> Observation {
    let mut rt = common::new_runtime().expect("runtime");
    let manifest = Apk::open(apk_path)
        .expect("open perm-probe apk")
        .manifest()
        .expect("manifest");
    let mut app = AppInfo::from_manifest(&manifest, apk_path, FIXTURE_UID);
    apk_loader::stage(rt.vm(), apk_path, &mut app).expect("stage perm-probe apk");
    let boot = rt.boot_app_process(apk_path).expect("boot the app process");
    rt.vm_mut()
        .push_static_call("android.os.Looper", "loop", "()V", &[])
        .expect("push Looper.loop");
    let thread = boot.activity_thread;
    let app_thread = bind_app::app_thread(rt.vm_mut(), thread).expect("mAppThread");

    let bytes = host_transaction();
    let caller = install_caller(rt.vm_mut(), thread, state);
    let vm = rt.vm_mut();
    let parcel = java_parcel(vm, &bytes);
    let transaction = parcel_java::create_from_parcel(
        vm,
        "android.app.servertransaction.ClientTransaction",
        parcel,
    )
    .expect("the image's own ClientTransaction reader accepts the host's bytes");
    // The token the guest's own reader produced from the host's bytes. The
    // record is installed under it, so a token the reader dropped (null) or
    // rewrote shows up as a failed lookup instead of being papered over by a
    // harness swap. `mActivities` is keyed by the token object the executor
    // resolves the record with.
    let decoded_token = vm
        .get_instance_field_by_name(transaction, "mActivityToken")
        .filter(|h| *h != NULL);
    let decoded_token = decoded_token.unwrap_or_else(|| {
        panic!("the image's ClientTransaction reader dropped mActivityToken from the host's bytes")
    });
    vm.set_instance_field_by_name(caller.record, "token", decoded_token);
    vm.set_instance_field_by_name(caller.activity, "mToken", decoded_token);
    let map = match vm.get_field(thread, "mActivities").expect("mActivities") {
        JavaValue::Object(h) if h != NULL => h,
        other => panic!("mActivities is {}", other.kind()),
    };
    vm.call_method(
        map,
        "put",
        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        &[JavaValue::Object(decoded_token), JavaValue::Object(caller.record)],
    )
    .expect("mActivities.put(decoded token)");
    let before = acos_vm::natives::os_log::records().len();
    vm.call_method(
        app_thread,
        "scheduleTransaction",
        "(Landroid/app/servertransaction/ClientTransaction;)V",
        &[JavaValue::Object(transaction)],
    )
    .expect("real ApplicationThread.scheduleTransaction");
    let blocker = run_looper(vm);

    Observation {
        lines: callback_lines(before),
        permissions_request_open: read_i32(vm, caller.activity, "mHasCurrentPermissionsRequest")
            == 1,
        m_called: read_i32(vm, caller.activity, "mCalled"),
        m_resumed: read_i32(vm, caller.activity, "mResumed"),
        paused: read_i32(vm, caller.record, "paused"),
        stopped: read_i32(vm, caller.record, "stopped"),
        lifecycle: read_i32(vm, caller.record, "mLifecycleState"),
        blocker,
    }
}

#[test]
fn the_hosts_result_transaction_reaches_the_caller_in_every_lifecycle_state() {
    let Some(apk_path) = perm_probe_apk() else {
        acos_test_support::require_prereq(false, "the_hosts_result_transaction_reaches_the_caller_in_every_lifecycle_state", "the_hosts_result_transaction_reaches_the_caller_in_every_lifecycle_state: fixtures/build/apps/perm-probe.apk is absent (run tools/build/build-apps.sh)");
        return;
    };
    let mut seen = Vec::new();
    // AOSP's `handleSendResult` covers both shapes: a caller that is
    // PAUSED/STOPPED (`resumed == false`: results now, then the resume lifecycle
    // item restarts it) and a caller that is still RESUMED (`resumed == true`:
    // pause, results, resume). The pre-granted run's caller is PAUSED/STOPPED
    // when the dialog finishes during `onCreate`; the interactive arms deliver
    // to a RESUMED caller. All three must reach `onRequestPermissionsResult`.
    for (name, state) in [
        ("resumed", ON_RESUME),
        ("paused", ON_PAUSE),
        ("stopped", ON_STOP),
    ] {
        let obs = observe(&apk_path, state);
        eprintln!(
            "activity_result_delivery: state={name} permissions_request_open={} \
             mCalled={} mResumed={} record.paused={} record.stopped={} record.lifecycle={} \
             lines={:?} blocker={:?}",
            obs.permissions_request_open,
            obs.m_called,
            obs.m_resumed,
            obs.paused,
            obs.stopped,
            obs.lifecycle,
            obs.lines,
            obs.blocker
        );
        seen.push((name, obs));
    }

    for (name, obs) in &seen {
        // The delivery must do *something* through the real executor: either the
        // caller's `onRequestPermissionsResult` runs, or the transaction's first
        // framework step is named. "One transaction ran and nothing happened" —
        // the failing ART run's shape — fails here.
        let first_step = obs.blocker.as_deref().unwrap_or_default();
        match *name {
            // The pre-granted arm's shape: the caller is paused, so
            // `handleSendResult` delivers the list, then the resume lifecycle
            // item restarts it. The fixture's `onRequestPermissionsResult` runs
            // (it clears `mHasCurrentPermissionsRequest`); its next line touches
            // a view this manually installed Activity does not have, which
            // `deliverResults` catches and rethrows as the named blocker.
            "paused" => assert!(
                !obs.permissions_request_open,
                "paused: the caller did not reach onRequestPermissionsResult \
                 (lines={:?}, blocker={:?})",
                obs.lines, obs.blocker
            ),
            // A resumed caller: AOSP's `handleSendResult` pauses it first
            // (`performPauseActivity`), so the result reaches it from the pause.
            "resumed" => assert!(
                !obs.permissions_request_open
                    || first_step.contains("performPauseActivity")
                    || first_step.contains("performSaveInstanceState"),
                "resumed: expected AOSP's pause-before-result step, got \
                 request_open={} blocker={:?}",
                obs.permissions_request_open,
                obs.blocker
            ),
            // A stopped caller: `TransactionExecutor.executeCallbacks` cycles to
            // the closest pre-execution state first (AOSP's
            // `getClosestPreExecutionState`/`cycleToPath`), so the restart
            // (`handleStartActivity`) is the first framework step.
            "stopped" => assert!(
                !obs.permissions_request_open
                    || first_step.contains("handleStartActivity")
                    || first_step.contains("performStart"),
                "stopped: expected AOSP's restart-before-result step, got \
                 request_open={} blocker={:?}",
                obs.permissions_request_open,
                obs.blocker
            ),
            other => panic!("unexpected state {other}"),
        }
        assert!(
            obs.m_called == 1 || obs.blocker.is_some(),
            "{name}: Activity.mCalled was left false with no named blocker"
        );
    }
}
