//! `IApplicationThread.bindApplication` delivery (task T-24).
//!
//! Real Android's AMS, once `attachApplication` has been received, calls
//! `IApplicationThread.bindApplication(...)` with a bag of process parameters.
//! `ActivityThread$ApplicationThread.bindApplication` builds an
//! `ActivityThread$AppBindData` from them and posts `H.BIND_APPLICATION` to the
//! app's main looper, where `ActivityThread.handleBindApplication` creates the
//! `Application`, runs `Application.onCreate`, and (at its end) calls
//! `IActivityManager.finishAttachApplication`.
//!
//! # Why this module drives the bridge, not a byte codec
//!
//! The arguments are real framework parcelables (`ApplicationInfo`,
//! `ProviderInfoList`, `Configuration`, `CompatibilityInfo`, `Bundle`, ...).
//! `research/service-layer/REPORT.md` §3.6 records the invariant that the only
//! code that may write a parcelable's bytes is that parcelable's own
//! `writeToParcel`; the earlier hand-written prefix codec assigned
//! `ApplicationInfo` fields to the wrong slots. Rather than reproduce a
//! 26-argument wire layout by hand, ACOS materialises the real Java objects
//! through [`crate::pms_java`] and invokes the real, unmodified
//! `ApplicationThread.bindApplication` bytecode.
//!
//! This is **not** a stub: every Java object created here is the real
//! framework class, and the method that consumes them is the real framework
//! method. What is bypassed is only the `Parcel` serialization boundary between
//! two in-process objects (there is no remote process). The same applies to the
//! `ClientTransaction` stream; see `lifecycle.rs`.

use acos_services::pms::AppInfo;
use acos_vm::heap::{Handle, NULL};
use acos_vm::javabridge::{BridgeError, JavaValue};
use acos_vm::Vm;

use crate::pms_java;

/// The exact API 34 signature of `IApplicationThread.bindApplication`.
pub const BIND_APPLICATION_SIG: &str = "(Ljava/lang/String;Landroid/content/pm/ApplicationInfo;Ljava/lang/String;Ljava/lang/String;Landroid/content/pm/ProviderInfoList;Landroid/content/ComponentName;Landroid/app/ProfilerInfo;Landroid/os/Bundle;Landroid/app/IInstrumentationWatcher;Landroid/app/IUiAutomationConnection;IZZZZLandroid/content/res/Configuration;Landroid/content/res/CompatibilityInfo;Ljava/util/Map;Landroid/os/Bundle;Ljava/lang/String;Landroid/content/AutofillOptions;Landroid/content/ContentCaptureOptions;[JLandroid/os/SharedMemory;JJ)V";

/// A hard failure while building or delivering `bindApplication`.
#[derive(Debug, thiserror::Error)]
pub enum BindAppError {
    /// A bridge/VM call failed.
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    /// A VM-level failure.
    #[error("vm: {0}")]
    Vm(#[from] acos_vm::VmError),
    /// Materialising a framework parcelable failed.
    #[error("materialize: {0}")]
    Materialize(String),
    /// The `ActivityThread` did not expose a live `mAppThread`.
    #[error("ActivityThread.mAppThread is missing or not an IApplicationThread")]
    NoAppThread,
}

fn obj(v: JavaValue) -> Result<Handle, BindAppError> {
    match v {
        JavaValue::Object(h) if h != NULL => Ok(h),
        _ => Err(BindAppError::NoAppThread),
    }
}

/// Read `ActivityThread.mAppThread` (the real `ApplicationThread` local binder).
pub fn app_thread(vm: &mut Vm, activity_thread: Handle) -> Result<Handle, BindAppError> {
    match vm.get_field(activity_thread, "mAppThread") {
        Ok(JavaValue::Object(h)) if h != NULL => Ok(h),
        _ => Err(BindAppError::NoAppThread),
    }
}

fn infostr(vm: &mut Vm, s: &str) -> Result<Handle, BindAppError> {
    Ok(vm.new_string_rooted(s)?)
}

/// Build and deliver one `bindApplication` call to `activity_thread`.
///
/// The arguments follow the real framework path for the hello-views fixture:
/// no instrumentation, no providers, no splits, default compatibility info and
/// a default `Configuration`. `services` and `coreSettings` are real empty
/// containers so the framework's `ServiceManager.initServiceCache` /
/// `setCoreSettings` paths run.
pub fn deliver_bind_application(
    vm: &mut Vm,
    activity_thread: Handle,
    app: &AppInfo,
    providers: &[(String, String)],
) -> Result<Handle, BindAppError> {
    let thread = app_thread(vm, activity_thread)?;

    // ApplicationInfo is materialised by the same code the PMS reply path uses.
    let info = pms_java::application_info(vm, app).map_err(BindAppError::Materialize)?;

    // `ProviderInfoList.fromList(list)` — `handleBindApplication` calls
    // `providerList.getList()` unconditionally, so this must not be null. Each
    // entry is a real `ProviderInfo` for a provider the app declares; the real
    // `ActivityThread.installContentProviders` then instantiates the provider
    // class and reports it through `AMS.publishContentProviders`.
    let providers_list = vm.new_instance_ctor("java.util.ArrayList", "()V", &[])?;
    for (class, authority) in providers {
        let provider = crate::ams_java::app_provider_info(vm, &app.package_name, class, authority)
            .map_err(BindAppError::Materialize)?;
        vm.call_method(
            providers_list,
            "add",
            "(Ljava/lang/Object;)Z",
            &[JavaValue::Object(provider)],
        )?;
    }
    let provider_list = obj(vm.call_static(
        "android.content.pm.ProviderInfoList",
        "fromList",
        "(Ljava/util/List;)Landroid/content/pm/ProviderInfoList;",
        &[JavaValue::Object(providers_list)],
    )?)?;

    // Default CompatibilityInfo (its <clinit> must have run).
    let ci_cid = vm.load_class("Landroid/content/res/CompatibilityInfo;")?;
    vm.initialize_class(ci_cid)?;
    let compat = obj(vm.get_static_field(
        "android.content.res.CompatibilityInfo",
        "DEFAULT_COMPATIBILITY_INFO",
    )?)?;

    let config = build_config(vm)?;
    let services = vm.new_instance_ctor("java.util.HashMap", "()V", &[])?;
    let instrumentation_args = vm.new_instance_ctor("android.os.Bundle", "()V", &[])?;
    let core_settings = vm.new_instance_ctor("android.os.Bundle", "()V", &[])?;
    let disabled = vm.new_array_empty("J", 0)?;

    let args = vec![
        JavaValue::Object(infostr(vm, &app.package_name)?), // packageName
        JavaValue::Object(info),                            // appInfo
        JavaValue::Object(infostr(vm, &app.process_name)?), // processName
        JavaValue::Null,                                    // sdkSandboxClientAppVolumeUuid
        JavaValue::Object(provider_list),                   // providerList
        JavaValue::Null,                                    // instrumentationName
        JavaValue::Null,                                    // profilerInfo
        JavaValue::Object(instrumentation_args),            // instrumentationArgs
        JavaValue::Null,                                    // instrumentationWatcher
        JavaValue::Null,                                    // instrumentationUiConnection
        JavaValue::Int(0),                                  // debugMode
        JavaValue::Boolean(false),                          // enableBinderTracking
        JavaValue::Boolean(false),                          // trackAllocation
        JavaValue::Boolean(false),                          // isRestrictedBackupMode
        JavaValue::Boolean(false),                          // persistent
        JavaValue::Object(config),                          // config
        JavaValue::Object(compat),                          // compatInfo
        JavaValue::Object(services),                        // services
        JavaValue::Object(core_settings),                   // coreSettings
        JavaValue::Object(infostr(vm, "acos")?),            // buildSerial
        JavaValue::Null,                                    // autofillOptions
        JavaValue::Null,                                    // contentCaptureOptions
        JavaValue::Object(disabled),                        // disabledCompatChanges
        JavaValue::Null,                                    // serializedSystemFontMap
        JavaValue::Long(0),                                 // startRequestedElapsedTime
        JavaValue::Long(0),                                 // startRequestedUptime
    ];
    vm.call_method(thread, "bindApplication", BIND_APPLICATION_SIG, &args)?;
    Ok(thread)
}

/// Build the process `Configuration` delivered to `bindApplication`.
///
/// AMS sends the system configuration; ACOS has no system server, so it
/// materialises the values captured from the API 34 oracle
/// (`fixtures/reference/hello-views/art-api34-x86_64/device-config.json` +
/// `display.txt`): locale `en-US`, density 420, 411x842 dp portrait. The locale
/// list must be non-empty — `handleBindApplication` calls
/// `LocaleList.setDefault(config.getLocales())`, which rejects an empty list.
pub(crate) fn build_config(vm: &mut Vm) -> Result<Handle, BindAppError> {
    let config = vm.new_instance_ctor("android.content.res.Configuration", "()V", &[])?;
    let tag = infostr(vm, "en-US")?;
    let locales = obj(vm.call_static(
        "android.os.LocaleList",
        "forLanguageTags",
        "(Ljava/lang/String;)Landroid/os/LocaleList;",
        &[JavaValue::Object(tag)],
    )?)?;
    vm.call_method(
        config,
        "setLocales",
        "(Landroid/os/LocaleList;)V",
        &[JavaValue::Object(locales)],
    )?;
    // Density and dp bounds from the oracle's `overrideConfig`.
    vm.set_field(config, "densityDpi", JavaValue::Int(420))?;
    vm.set_field(config, "screenWidthDp", JavaValue::Int(411))?;
    vm.set_field(config, "screenHeightDp", JavaValue::Int(842))?;
    vm.set_field(config, "smallestScreenWidthDp", JavaValue::Int(411))?;
    vm.set_field(config, "fontScale", JavaValue::Float(1.0))?;
    // `Configuration.ORIENTATION_PORTRAIT`.
    vm.set_field(config, "orientation", JavaValue::Int(1))?;
    Ok(config)
}
