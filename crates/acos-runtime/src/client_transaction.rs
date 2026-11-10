//! Real `ClientTransaction` construction and delivery (task T-23).
//!
//! After `bindApplication` the real `ActivityThread` is waiting in
//! `Looper.loop()`. AMS/ATMS then sends a `ClientTransaction` through
//! `IApplicationThread.scheduleTransaction(ClientTransaction)`; the app's
//! `ActivityThread.scheduleTransaction` posts `H.EXECUTE_TRANSACTION`, and
//! `TransactionExecutor` runs:
//!
//! ```text
//! LaunchActivityItem.execute  -> ActivityThread.handleLaunchActivity
//!                              -> performLaunchActivity -> Activity.onCreate
//! ResumeActivityItem.execute  -> ActivityThread.handleResumeActivity
//!                              -> Activity.onResume
//! ```
//!
//! ACOS has no remote process and no binder driver between AMS and the app, so
//! the same way [`crate::bind_app`] delivers `bindApplication`, this module
//! builds the *real* framework objects (`ClientTransaction`,
//! `LaunchActivityItem`, `ResumeActivityItem`, `Intent`, `ActivityInfo`) through
//! the Java bridge and calls the real `ApplicationThread.scheduleTransaction`
//! bytecode. Only the `Parcel` serialisation boundary between two in-process
//! objects is skipped; every object and every method that consumes it is the
//! unmodified Android 14 framework.
//!
//! The transaction token is a real `android.os.Binder` (the object ATMS uses as
//! `ActivityRecord.token`). The same instance is the `ClientTransaction`
//! activity token, the `ActivityClientRecord.token`, and the Activity's
//! `mToken`, so `TransactionExecutor` can find the record that
//! `performLaunchActivity` registered in `ActivityThread.mActivities`.

use std::path::Path;

use acos_apk::Apk;
use acos_services::pms::{ActivityInfo as PmsActivityInfo, AppInfo};
use acos_vm::heap::Handle;
use acos_vm::javabridge::{BridgeError, JavaValue};
use acos_vm::Vm;

use crate::{bind_app, pms_java};

/// The exact API 34 `LaunchActivityItem.obtain` signature.
const LAUNCH_OBTAIN_SIG: &str = "(Landroid/content/Intent;ILandroid/content/pm/ActivityInfo;Landroid/content/res/Configuration;Landroid/content/res/Configuration;ILjava/lang/String;Lcom/android/internal/app/IVoiceInteractor;ILandroid/os/Bundle;Landroid/os/PersistableBundle;Ljava/util/List;Ljava/util/List;Landroid/app/ActivityOptions;ZLandroid/app/ProfilerInfo;Landroid/os/IBinder;Landroid/app/IActivityClientController;Landroid/os/IBinder;ZLandroid/os/IBinder;)Landroid/app/servertransaction/LaunchActivityItem;";

/// A structured failure while building or delivering the transaction.
#[derive(Debug, thiserror::Error)]
pub enum ClientTransactionError {
    /// The APK/manifest could not be read.
    #[error("apk: {0}")]
    Apk(#[from] acos_apk::ApkError),
    /// A bridge/VM call failed.
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    /// A VM-level failure.
    #[error("vm: {0}")]
    Vm(#[from] acos_vm::VmError),
    /// Materialising a framework object failed.
    #[error("materialize: {0}")]
    Materialize(String),
    /// The manifest declares no activity to launch.
    #[error("manifest declares no activity")]
    NoActivity,
}

fn obj(v: JavaValue) -> Result<Handle, ClientTransactionError> {
    match v {
        JavaValue::Object(h) if h != acos_vm::heap::NULL => Ok(h),
        _ => Err(ClientTransactionError::Materialize(
            "expected a non-null framework object".to_string(),
        )),
    }
}

/// Pick the launcher activity from the manifest (`MAIN`/`LAUNCHER`), falling
/// back to the first declared activity.
fn launcher_activity(apk_path: &Path) -> Result<PmsActivityInfo, ClientTransactionError> {
    let apk = Apk::open(apk_path)?;
    let manifest = apk.manifest()?;
    let component = manifest
        .activities
        .iter()
        .find(|a| {
            a.intent_filters.iter().any(|f| {
                f.actions.iter().any(|x| x == "android.intent.action.MAIN")
                    && f.categories
                        .iter()
                        .any(|x| x == "android.intent.category.LAUNCHER")
            })
        })
        .or_else(|| manifest.activities.first())
        .ok_or(ClientTransactionError::NoActivity)?;
    let launch_mode = match component.launch_mode.as_deref() {
        Some("singleTop") => 1,
        Some("singleTask") => 2,
        Some("singleInstance") => 3,
        _ => 0,
    };
    Ok(PmsActivityInfo {
        name: component.name.clone(),
        package_name: manifest.package.clone(),
        launch_mode,
        flags: 0,
        theme: manifest.application.theme.unwrap_or(0) as i32,
        exported: component.exported.unwrap_or(true),
        parent_activity_name: String::new(),
    })
}

/// The handles materialised for one launch transaction (diagnostics/tests).
#[derive(Debug, Clone, Copy)]
pub struct LaunchTransaction {
    /// The `ClientTransaction` object handed to `scheduleTransaction`.
    pub transaction: Handle,
    /// The `android.os.Binder` activity token shared by launch and resume.
    pub token: Handle,
    /// The `LaunchActivityItem`.
    pub launch_item: Handle,
    /// The `ResumeActivityItem`.
    pub resume_item: Handle,
    /// The `ActivityInfo` the item carries.
    pub activity_info: Handle,
    /// The `Intent` the item carries.
    pub intent: Handle,
}

/// Build a real `ClientTransaction{LaunchActivityItem, ResumeActivityItem}`
/// for `app` and deliver it to `app_thread.scheduleTransaction`.
///
/// `app_thread` is the real `ActivityThread$ApplicationThread` object (the
/// local binder `ActivityThread.mAppThread`); calling its bytecode is the same
/// hand-off AMS performs, minus the parcel write/read for an in-process object.
pub fn deliver_launch(
    vm: &mut Vm,
    app_thread: Handle,
    app: &AppInfo,
    apk_path: &Path,
) -> Result<LaunchTransaction, ClientTransactionError> {
    let activity = launcher_activity(apk_path)?;
    let info = pms_java::activity_info(vm, app, &activity)
        .map_err(ClientTransactionError::Materialize)?;

    let package = vm.new_string_rooted(&activity.package_name)?;
    let intent = vm.new_instance_ctor("android.content.Intent", "()V", &[])?;
    let name = vm.new_string_rooted(&activity.name)?;
    vm.call_method(
        intent,
        "setClassName",
        "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
        &[JavaValue::Object(package), JavaValue::Object(name)],
    )?;

    let config = bind_app::build_config(vm)
        .map_err(|e| ClientTransactionError::Materialize(e.to_string()))?;
    // The launch item carries two configurations: the process configuration and
    // an optional override. The fixture has no override, so both point at the
    // real process configuration (the item's `mCurConfig` field is written by
    // `setValues` unconditionally).
    let launch_item = obj(vm.call_static(
        "android.app.servertransaction.LaunchActivityItem",
        "obtain",
        LAUNCH_OBTAIN_SIG,
        &[
            JavaValue::Object(intent),           // intent
            JavaValue::Int(0),                   // ident
            JavaValue::Object(info),             // ActivityInfo
            JavaValue::Object(config),           // curConfig
            JavaValue::Object(config),           // overrideConfig
            JavaValue::Int(0),                   // deviceId (DEVICE_ID_DEFAULT)
            JavaValue::Null,                     // referrer
            JavaValue::Null,                     // voiceInteractor
            JavaValue::Int(2),                   // procState (PROCESS_STATE_TOP)
            JavaValue::Null,                     // state
            JavaValue::Null,                     // persistentState
            JavaValue::Null,                     // pendingResults
            JavaValue::Null,                     // pendingNewIntents
            JavaValue::Null,                     // activityOptions
            JavaValue::Boolean(false),           // isForward
            JavaValue::Null,                     // profilerInfo
            JavaValue::Null,                     // assistToken
            JavaValue::Null,                     // activityClientController
            JavaValue::Null,                     // shareableActivityToken
            JavaValue::Boolean(false),           // launchedFromBubble
            JavaValue::Null,                     // taskFragmentToken
        ],
    )?)?;

    let resume_item = obj(vm.call_static(
        "android.app.servertransaction.ResumeActivityItem",
        "obtain",
        "(ZZ)Landroid/app/servertransaction/ResumeActivityItem;",
        &[JavaValue::Boolean(false), JavaValue::Boolean(false)],
    )?)?;

    // The activity token: a real local Binder, exactly the object type ATMS
    // uses for `ActivityRecord.token`.
    let token = vm.new_instance_ctor("android.os.Binder", "()V", &[])?;

    let transaction = obj(vm.call_static(
        "android.app.servertransaction.ClientTransaction",
        "obtain",
        "(Landroid/app/IApplicationThread;Landroid/os/IBinder;)Landroid/app/servertransaction/ClientTransaction;",
        &[JavaValue::Object(app_thread), JavaValue::Object(token)],
    )?)?;
    vm.call_method(
        transaction,
        "addCallback",
        "(Landroid/app/servertransaction/ClientTransactionItem;)V",
        &[JavaValue::Object(launch_item)],
    )?;
    vm.call_method(
        transaction,
        "setLifecycleStateRequest",
        "(Landroid/app/servertransaction/ActivityLifecycleItem;)V",
        &[JavaValue::Object(resume_item)],
    )?;

    // Real receiver: `ActivityThread$ApplicationThread.scheduleTransaction`
    // -> `ActivityThread.scheduleTransaction` -> `H.EXECUTE_TRANSACTION`.
    vm.call_method(
        app_thread,
        "scheduleTransaction",
        "(Landroid/app/servertransaction/ClientTransaction;)V",
        &[JavaValue::Object(transaction)],
    )?;

    Ok(LaunchTransaction {
        transaction,
        token,
        launch_item,
        resume_item,
        activity_info: info,
        intent,
    })
}
