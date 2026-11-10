//! The runtime-side AMS bridge: real `android.content.Intent` in, real
//! framework objects out (W52-A).
//!
//! `acos-services::ams` is deliberately VM-free: it consumes typed
//! [`IntentFacts`] and returns typed [`PendingLaunch`]s, [`PendingBind`]s and
//! [`PendingReceiver`]s. This module is the only place that turns the real
//! framework objects into those facts and back:
//!
//! * **request direction** — an AIDL `Intent` argument is read with the
//!   framework's own `Intent.CREATOR` (through
//!   [`crate::parcel_java::read_typed_object`]) and reduced to [`IntentFacts`]
//!   by calling the real `getAction`/`getDataString`/`getComponent`/
//!   `getCategories`/`getExtras` accessors. `IntentFilter` is read the same
//!   way (`countActions`/`getAction`, `countCategories`/`getCategory`).
//! * **reply direction** — `startService` returns a real `ComponentName`
//!   written by its own `writeToParcel`; `getContentProvider` returns a real
//!   `ContentProviderHolder` carrying a real `ProviderInfo`.
//! * **deliveries** — [`BinderDelivery`]s whose request parcels are written by
//!   the real interfaces: `IServiceConnection.connected`,
//!   `IIntentReceiver.performReceive`. The `Intent` written into a broadcast is
//!   a real `Intent` re-materialised from the typed facts.
//!
//! # The invariant
//!
//! No parcelable's wire bytes are hand-written. `Intent`/`ComponentName`/
//! `ProviderInfo`/`ContentProviderHolder` are real Java objects whose own
//! `writeToParcel` emits the payload (`research/service-layer/REPORT.md`
//! §3.6).
//!
//! # Never fake
//!
//! An extra whose runtime type is not `String`/`int`/`boolean` is a structured
//! `UNSUPPORTED_ANDROID_API`, not a silently dropped value. An activity whose
//! launch cannot be materialised is a structured error. `IContentProvider`
//! query/insert/update/delete without a provider implementation is a structured
//! `UNSUPPORTED_ANDROID_API` (see [`transact_content_provider`]).
//!
//! # State
//!
//! The `AmsState` is process-wide (thread-local, matching the single-threaded
//! VM) so it survives across the guest's transactions. It is seeded from the
//! launched APK with [`install_manifest_from_apk`].

use std::cell::RefCell;

use acos_parcel::{binder::BinderId, Parcel};
use acos_services::ams::{
    AmsState, Extra, IntentFacts, PendingBind, PendingLaunch, PendingReceiver,
};
use acos_services::pms::{ActivityInfo as PmsActivityInfo, AppInfo};
use acos_vm::heap::{Handle, NULL};
use acos_vm::javabridge::{class_descriptor, JavaValue};
use acos_vm::natives::binder::BinderDelivery;
use acos_vm::Vm;

type JResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("ams_java: {e}")
}

thread_local! {
    static STATE: RefCell<AmsState> = RefCell::new(AmsState::new());
}

/// Forget all AMS state (tests / a fresh launch).
pub fn reset() {
    STATE.with(|s| *s.borrow_mut() = AmsState::new());
}

/// Seed the AMS manifest table from an installed APK.
pub fn install_manifest_from_apk(path: impl AsRef<std::path::Path>, uid: i32) -> JResult<()> {
    STATE.with(|s| s.borrow_mut().manifests.add_apk(path, uid).map_err(err))
}

/// Run `f` against the process-wide AMS state.
pub fn with_state<R>(f: impl FnOnce(&mut AmsState) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// Drain the queued `IServiceConnection.connected` deliveries.
#[must_use]
pub fn take_pending_binds() -> Vec<PendingBind> {
    STATE.with(|s| s.borrow_mut().take_pending_binds())
}

/// Drain the queued `IIntentReceiver.performReceive` deliveries.
#[must_use]
pub fn take_pending_receivers() -> Vec<PendingReceiver> {
    STATE.with(|s| s.borrow_mut().take_pending_receivers())
}

/// Drain the queued launches.
#[must_use]
pub fn take_pending_launches() -> Vec<PendingLaunch> {
    STATE.with(|s| s.borrow_mut().take_pending_launches())
}

/// True when `descriptor` is routed to this module.
#[must_use]
pub fn owns(descriptor: Option<&str>) -> bool {
    matches!(
        descriptor,
        Some("android.app.IActivityManager") | Some("android.app.IActivityTaskManager")
    )
}

/// True when this module must marshal the request and/or reply for `method`.
#[must_use]
pub fn owns_method(method: &str) -> bool {
    matches!(
        method,
        "startActivity"
            | "startActivityWithFeature"
            | "startActivityAsUser"
            | "startService"
            | "stopService"
            | "bindService"
            | "bindServiceInstance"
            | "unbindService"
            | "registerReceiver"
            | "registerReceiverWithFeature"
            | "unregisterReceiver"
            | "broadcastIntent"
            | "broadcastIntentWithFeature"
            | "getContentProvider"
            | "publishContentProviders"
    ) || content_provider_method(method)
}

/// True for an `IContentProvider` method this module owns.
#[must_use]
pub fn content_provider_method(method: &str) -> bool {
    matches!(method, "query" | "insert" | "update" | "delete" | "call")
}

// ---------------------------------------------------------------------------
// Request decoding
// ---------------------------------------------------------------------------

/// One decoded AIDL argument.
enum Arg {
    Binder(Option<BinderId>),
    Str(Option<String>),
    Int(i32),
    Long(i64),
    Bool(bool),
    Intent(Option<IntentFacts>),
    Filter(Vec<String>, Vec<String>),
    Strings(#[allow(dead_code)] Vec<Option<String>>),
    /// A decoded argument this module consumes but does not interpret.
    Other,
}

impl Arg {
    fn binder(&self) -> u64 {
        match self {
            Arg::Binder(Some(b)) => b.0,
            _ => 0,
        }
    }
    fn int(&self) -> i32 {
        match self {
            Arg::Int(v) => *v,
            Arg::Bool(v) => i32::from(*v),
            _ => 0,
        }
    }
    fn long(&self) -> i64 {
        match self {
            Arg::Long(v) => *v,
            _ => 0,
        }
    }
    fn string(&self) -> Option<&str> {
        match self {
            Arg::Str(s) => s.as_deref(),
            _ => None,
        }
    }
}

fn decode_args(vm: &mut Vm, method: &acos_aidl::MethodInfo, data: &mut Parcel) -> JResult<Vec<Arg>> {
    let mut out = Vec::with_capacity(method.args.len());
    for op in &method.args {
        let arg = match op {
            acos_aidl::WireOp::Binder { .. } => {
                Arg::Binder(data.read_strong_binder().map_err(err)?)
            }
            acos_aidl::WireOp::Str16 => Arg::Str(data.read_string16().map_err(err)?),
            acos_aidl::WireOp::I32 => Arg::Int(data.read_int32().map_err(err)?),
            acos_aidl::WireOp::I64 => Arg::Long(data.read_int64().map_err(err)?),
            acos_aidl::WireOp::Bool => Arg::Bool(data.read_int32().map_err(err)? != 0),
            acos_aidl::WireOp::StringArray => {
                Arg::Strings(data.read_string16_array().map_err(err)?.unwrap_or_default())
            }
            acos_aidl::WireOp::TypedObject { class, .. } => {
                if class == "android.content.Intent" {
                    Arg::Intent(read_intent_facts(vm, data)?)
                } else if class == "android.content.IntentFilter" {
                    let h = crate::parcel_java::read_typed_object(vm, data, class)?;
                    let (actions, categories) = match h {
                        Some(h) if h != NULL => intent_filter(vm, h)?,
                        _ => (Vec::new(), Vec::new()),
                    };
                    Arg::Filter(actions, categories)
                } else {
                    // A parcelable ACOS does not interpret: consume it with its
                    // own CREATOR so the stream stays aligned.
                    let _ = crate::parcel_java::read_typed_object(vm, data, class)?;
                    Arg::Other
                }
            }
            acos_aidl::WireOp::Parcelable { class } => {
                let _ = crate::parcel_java::read_typed_object(vm, data, class)?;
                Arg::Other
            }
            acos_aidl::WireOp::Bundle => {
                let _ = crate::parcel_java::read_typed_object(vm, data, "android.os.Bundle")?;
                Arg::Other
            }
            other => {
                return Err(err(format!(
                    "{}: argument type {} is not decoded here",
                    method.name,
                    other.render()
                )))
            }
        };
        out.push(arg);
    }
    Ok(out)
}

/// Read one AIDL `writeTypedObject` `Intent` argument and reduce it to the
/// typed facts. `None` means the presence marker was `null`.
pub fn read_intent_facts(vm: &mut Vm, data: &mut Parcel) -> JResult<Option<IntentFacts>> {
    let h = crate::parcel_java::read_typed_object(vm, data, "android.content.Intent")?;
    match h {
        Some(h) if h != NULL => Ok(Some(intent_facts(vm, h)?)),
        _ => Ok(None),
    }
}

/// Reduce a real `android.content.Intent` to the typed facts.
fn intent_facts(vm: &mut Vm, intent: Handle) -> JResult<IntentFacts> {
    let action = opt_string(vm, intent, "getAction", "()Ljava/lang/String;")?;
    let data = opt_string(vm, intent, "getDataString", "()Ljava/lang/String;")?;
    let package = opt_string(vm, intent, "getPackage", "()Ljava/lang/String;")?;

    let component = match vm
        .call_method(
            intent,
            "getComponent",
            "()Landroid/content/ComponentName;",
            &[],
        )
        .map_err(err)?
    {
        JavaValue::Object(h) if h != NULL => {
            let pkg = opt_string(vm, h, "getPackageName", "()Ljava/lang/String;")?
                .ok_or_else(|| err("ComponentName.getPackageName() returned null"))?;
            let class = opt_string(vm, h, "getClassName", "()Ljava/lang/String;")?
                .ok_or_else(|| err("ComponentName.getClassName() returned null"))?;
            Some((pkg, class))
        }
        _ => None,
    };

    let categories = match vm
        .call_method(
            intent,
            "getCategories",
            "()Ljava/util/Set;",
            &[],
        )
        .map_err(err)?
    {
        JavaValue::Object(h) if h != NULL => string_set(vm, h)?,
        _ => Vec::new(),
    };

    let extras = match vm
        .call_method(intent, "getExtras", "()Landroid/os/Bundle;", &[])
        .map_err(err)?
    {
        JavaValue::Object(h) if h != NULL => bundle_extras(vm, h)?,
        _ => std::collections::BTreeMap::new(),
    };

    Ok(IntentFacts {
        action,
        data,
        package,
        component,
        categories,
        extras,
    })
}

fn opt_string(vm: &mut Vm, obj: Handle, method: &str, sig: &str) -> JResult<Option<String>> {
    match vm.call_method(obj, method, sig, &[]).map_err(err)? {
        JavaValue::Object(h) if h != NULL => Ok(Some(vm.java_string_to_rust(h).map_err(err)?)),
        JavaValue::Null => Ok(None),
        JavaValue::Object(_) => Ok(None),
        other => Err(err(format!("{method} returned {}", other.kind()))),
    }
}

/// Read every element of a `Set<String>` through `toArray()`.
fn string_set(vm: &mut Vm, set: Handle) -> JResult<Vec<String>> {
    let array = match vm
        .call_method(set, "toArray", "()[Ljava/lang/Object;", &[])
        .map_err(err)?
    {
        JavaValue::Object(h) if h != NULL => h,
        _ => return Ok(Vec::new()),
    };
    let len = vm.array_length(array).map_err(err)?;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        match vm.get_array_element(array, i).map_err(err)? {
            JavaValue::Object(h) if h != NULL => out.push(vm.java_string_to_rust(h).map_err(err)?),
            _ => {}
        }
    }
    Ok(out)
}

/// Reduce a real `Bundle` to the `IntentFacts` extra map. A value type other
/// than `String`/`int`/`boolean` is a structured error (never a silent drop).
fn bundle_extras(vm: &mut Vm, bundle: Handle) -> JResult<std::collections::BTreeMap<String, Extra>> {
    let key_set = match vm
        .call_method(bundle, "keySet", "()Ljava/util/Set;", &[])
        .map_err(err)?
    {
        JavaValue::Object(h) if h != NULL => h,
        _ => return Ok(std::collections::BTreeMap::new()),
    };
    let keys = string_set(vm, key_set)?;
    let mut out = std::collections::BTreeMap::new();
    for key in keys {
        let key_h = vm.new_string_rooted(&key).map_err(err)?;
        let value = vm
            .call_method(
                bundle,
                "get",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JavaValue::Object(key_h)],
            )
            .map_err(err)?;
        let extra = match value {
            JavaValue::Object(h) if h != NULL => {
                let class = vm
                    .class_of_object(h)
                    .map(|c| vm.class_desc_of_id(c))
                    .unwrap_or_default();
                let class = class
                    .trim_start_matches('L')
                    .trim_end_matches(';')
                    .replace('/', ".");
                match class.as_str() {
                    "java.lang.String" => Extra::String(vm.java_string_to_rust(h).map_err(err)?),
                    "java.lang.Integer" => match vm
                        .call_method(h, "intValue", "()I", &[])
                        .map_err(err)?
                    {
                        JavaValue::Int(v) => Extra::Int(v),
                        other => return Err(err(format!("Integer.intValue returned {}", other.kind()))),
                    },
                    "java.lang.Boolean" => match vm
                        .call_method(h, "booleanValue", "()Z", &[])
                        .map_err(err)?
                    {
                        JavaValue::Boolean(v) => Extra::Bool(v),
                        JavaValue::Int(v) => Extra::Bool(v != 0),
                        other => {
                            return Err(err(format!(
                                "Boolean.booleanValue returned {}",
                                other.kind()
                            )))
                        }
                    },
                    other => {
                        return Err(err(format!(
                            "extra `{key}` has unsupported type {other}; AMS the bridge \
                             only carries String/int/boolean extras"
                        )))
                    }
                }
            }
            _ => Extra::Present,
        };
        out.insert(key, extra);
    }
    Ok(out)
}

/// Read a real `IntentFilter`'s actions and categories.
fn intent_filter(vm: &mut Vm, filter: Handle) -> JResult<(Vec<String>, Vec<String>)> {
    let actions = counted_strings(vm, filter, "countActions", "getAction")?;
    let categories = counted_strings(vm, filter, "countCategories", "getCategory")?;
    Ok((actions, categories))
}

fn counted_strings(
    vm: &mut Vm,
    obj: Handle,
    count: &str,
    get: &str,
) -> JResult<Vec<String>> {
    let n = match vm.call_method(obj, count, "()I", &[]).map_err(err)? {
        JavaValue::Int(v) if v > 0 => v as usize,
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        match vm
            .call_method(obj, get, "(I)Ljava/lang/String;", &[JavaValue::Int(i as i32)])
            .map_err(err)?
        {
            JavaValue::Object(h) if h != NULL => out.push(vm.java_string_to_rust(h).map_err(err)?),
            _ => {}
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Reply materialisation
// ---------------------------------------------------------------------------

fn as_obj(v: JavaValue) -> JResult<Handle> {
    match v {
        JavaValue::Object(h) if h != NULL => Ok(h),
        _ => Err(err("expected a non-null framework object")),
    }
}

/// Build a real `android.content.ComponentName`.
pub fn component_name(vm: &mut Vm, package: &str, class: &str) -> JResult<Handle> {
    let p = vm.new_string_rooted(package).map_err(err)?;
    let c = vm.new_string_rooted(class).map_err(err)?;
    vm.new_instance_ctor(
        "android.content.ComponentName",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[JavaValue::Object(p), JavaValue::Object(c)],
    )
    .map_err(err)
}

/// A minimal real `android.content.pm.ApplicationInfo` for the provider's
/// owning package.
///
/// `ComponentInfo.writeToParcel` dereferences `applicationInfo`
/// unconditionally, so the `ProviderInfo` must carry a non-null one even when
/// the app never reads it.
fn provider_application_info(
    vm: &mut Vm,
    package: &str,
    process_name: &str,
) -> JResult<Handle> {
    let app = AppInfo {
        package_name: package.to_string(),
        name: String::new(),
        class_name: String::new(),
        source_dir: String::new(),
        public_source_dir: String::new(),
        data_dir: format!("/data/user/0/{package}"),
        device_protected_data_dir: format!("/data/user_de/0/{package}"),
        credential_protected_data_dir: format!("/data/user/0/{package}"),
        native_library_dir: String::new(),
        uid: 1000,
        flags: 0,
        private_flags: 0,
        target_sdk: 34,
        min_sdk: 26,
        process_name: process_name.to_string(),
        theme: 0,
        label_res: 0,
        target_sandbox_version: 2,
    };
    crate::pms_java::application_info(vm, &app)
}

/// Materialise a real `ProviderInfo` (`getContentProvider`).
///
/// The field set is the API-34 `PackageItemInfo` -> `ComponentInfo` ->
/// `ProviderInfo` chain; the framework's own `writeToParcel` writes them in
/// order once they are set.
fn provider_info(
    vm: &mut Vm,
    package: &str,
    class: &str,
    authority: &str,
) -> JResult<Handle> {
    let h = vm
        .new_instance_ctor("android.content.pm.ProviderInfo", "()V", &[])
        .map_err(err)?;
    // PackageItemInfo
    set_str(vm, h, "name", class)?;
    set_str(vm, h, "packageName", package)?;
    set_int(vm, h, "labelRes", 0)?;
    vm.set_field(h, "nonLocalizedLabel", JavaValue::Null)
        .map_err(err)?;
    set_int(vm, h, "icon", 0)?;
    set_int(vm, h, "logo", 0)?;
    vm.set_field(h, "metaData", JavaValue::Null).map_err(err)?;
    set_int(vm, h, "banner", 0)?;
    set_int(vm, h, "showUserIcon", 0)?;
    // ComponentInfo
    let app = provider_application_info(vm, package, package)?;
    vm.set_field(h, "applicationInfo", JavaValue::Object(app))
        .map_err(err)?;
    set_str(vm, h, "processName", package)?;
    vm.set_field(h, "splitName", JavaValue::Null).map_err(err)?;
    vm.set_field(h, "attributionTags", JavaValue::Null)
        .map_err(err)?;
    set_int(vm, h, "descriptionRes", 0)?;
    set_bool(vm, h, "enabled", true)?;
    vm.set_field(h, "exported", JavaValue::Boolean(false))
        .map_err(err)?;
    set_bool(vm, h, "directBootAware", false)?;
    // ProviderInfo
    set_str(vm, h, "authority", authority)?;
    set_str(vm, h, "readPermission", "android.permission.READ_SETTINGS")?;
    set_str(vm, h, "writePermission", "android.permission.WRITE_SETTINGS")?;
    set_bool(vm, h, "grantUriPermissions", false)?;
    set_bool(vm, h, "forceUriPermissions", false)?;
    vm.set_field(h, "uriPermissionPatterns", JavaValue::Null)
        .map_err(err)?;
    vm.set_field(h, "pathPermissions", JavaValue::Null)
        .map_err(err)?;
    set_bool(vm, h, "multiprocess", false)?;
    set_int(vm, h, "initOrder", 0)?;
    set_int(vm, h, "flags", 0)?;
    set_bool(vm, h, "isSyncable", true)?;
    Ok(h)
}

/// Materialise a real `ContentProviderHolder` carrying a real `ProviderInfo`
/// and (when known) a real `android.content.ContentProviderProxy` for the
/// provider.
///
/// The proxy wraps a `BinderProxy` whose `mNativeData` is the binder id, so
/// `IContentProvider.asBinder()` (called by `writeToParcel`) returns a binder
/// the ACOS transport routes to the `settings` handler.
fn content_provider_holder(
    vm: &mut Vm,
    package: &str,
    class: &str,
    authority: &str,
    binder: u64,
) -> JResult<Handle> {
    let info = provider_info(vm, package, class, authority)?;
    let holder = vm
        .new_instance("android.app.ContentProviderHolder")
        .map_err(err)?;
    vm.set_field(holder, "info", JavaValue::Object(info))
        .map_err(err)?;
    if binder != 0 {
        let remote = vm
            .new_instance_ctor("android.os.BinderProxy", "(J)V", &[JavaValue::Long(binder as i64)])
            .map_err(err)?;
        let provider = vm
            .new_instance_ctor(
                "android.content.ContentProviderProxy",
                "(Landroid/os/IBinder;)V",
                &[JavaValue::Object(remote)],
            )
            .map_err(err)?;
        vm.set_field(holder, "provider", JavaValue::Object(provider))
            .map_err(err)?;
    } else {
        vm.set_field(holder, "provider", JavaValue::Null)
            .map_err(err)?;
    }
    vm.set_field(holder, "connection", JavaValue::Null)
        .map_err(err)?;
    vm.set_field(holder, "noReleaseNeeded", JavaValue::Boolean(true))
        .map_err(err)?;
    vm.set_field(holder, "mLocal", JavaValue::Boolean(false))
        .map_err(err)?;
    Ok(holder)
}

fn set_str(vm: &mut Vm, obj: Handle, field: &str, value: &str) -> JResult<()> {
    let h = vm.new_string_rooted(value).map_err(err)?;
    vm.set_field(obj, field, JavaValue::Object(h)).map_err(err)
}

fn set_int(vm: &mut Vm, obj: Handle, field: &str, value: i32) -> JResult<()> {
    vm.set_field(obj, field, JavaValue::Int(value)).map_err(err)
}

fn set_bool(vm: &mut Vm, obj: Handle, field: &str, value: bool) -> JResult<()> {
    vm.set_field(obj, field, JavaValue::Boolean(value)).map_err(err)
}

/// Re-build a real `android.content.Intent` from typed facts.
pub fn materialize_intent(vm: &mut Vm, facts: &IntentFacts) -> JResult<Handle> {
    let intent = vm
        .new_instance_ctor("android.content.Intent", "()V", &[])
        .map_err(err)?;
    if let Some(action) = &facts.action {
        let h = vm.new_string_rooted(action).map_err(err)?;
        vm.call_method(
            intent,
            "setAction",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JavaValue::Object(h)],
        )
        .map_err(err)?;
    }
    if let Some(data) = &facts.data {
        let data_h = vm.new_string_rooted(data).map_err(err)?;
        let uri = as_obj(
            vm.call_static(
                "android.net.Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JavaValue::Object(data_h)],
            )
            .map_err(err)?,
        )?;
        vm.call_method(
            intent,
            "setData",
            "(Landroid/net/Uri;)Landroid/content/Intent;",
            &[JavaValue::Object(uri)],
        )
        .map_err(err)?;
    }
    if let Some(package) = &facts.package {
        let h = vm.new_string_rooted(package).map_err(err)?;
        vm.call_method(
            intent,
            "setPackage",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JavaValue::Object(h)],
        )
        .map_err(err)?;
    }
    if let Some((package, class)) = &facts.component {
        let cn = component_name(vm, package, class)?;
        vm.call_method(
            intent,
            "setComponent",
            "(Landroid/content/ComponentName;)Landroid/content/Intent;",
            &[JavaValue::Object(cn)],
        )
        .map_err(err)?;
    }
    for category in &facts.categories {
        let h = vm.new_string_rooted(category).map_err(err)?;
        vm.call_method(
            intent,
            "addCategory",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JavaValue::Object(h)],
        )
        .map_err(err)?;
    }
    for (key, value) in &facts.extras {
        let k = vm.new_string_rooted(key).map_err(err)?;
        match value {
            Extra::String(s) => {
                let v = vm.new_string_rooted(s).map_err(err)?;
                vm.call_method(
                    intent,
                    "putExtra",
                    "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
                    &[JavaValue::Object(k), JavaValue::Object(v)],
                )
                .map_err(err)?;
            }
            Extra::Int(v) => {
                vm.call_method(
                    intent,
                    "putExtra",
                    "(Ljava/lang/String;I)Landroid/content/Intent;",
                    &[JavaValue::Object(k), JavaValue::Int(*v)],
                )
                .map_err(err)?;
            }
            Extra::Bool(v) => {
                vm.call_method(
                    intent,
                    "putExtra",
                    "(Ljava/lang/String;Z)Landroid/content/Intent;",
                    &[JavaValue::Object(k), JavaValue::Boolean(*v)],
                )
                .map_err(err)?;
            }
            Extra::Present => {
                return Err(err(format!(
                    "cannot re-materialise extra `{key}`: its type was not captured"
                )))
            }
        }
    }
    Ok(intent)
}

// ---------------------------------------------------------------------------
// IActivityManager / IActivityTaskManager
// ---------------------------------------------------------------------------

/// Handle one `IActivityManager`/`IActivityTaskManager` transaction.
pub fn transact(
    vm: &mut Vm,
    _binder: BinderId,
    code: u32,
    data: &mut Parcel,
    reply: &mut Parcel,
    _flags: u32,
) -> JResult<Vec<BinderDelivery>> {
    let descriptor = data
        .peek_interface_token()
        .map_err(err)?
        .ok_or_else(|| err("missing interface token"))?;
    let method = acos_aidl::lookup(&descriptor, code)
        .ok_or_else(|| err(format!("unknown method code {code} for {descriptor}")))?;

    if descriptor == "android.content.IContentProvider" {
        return transact_content_provider(vm, method, data, reply);
    }
    if !owns_method(&method.name) {
        return Err(err(format!("method {} is not owned here", method.name)));
    }
    data.enforce_interface(&descriptor).map_err(err)?;
    let args = decode_args(vm, method, data)?;

    reply.reset();
    acos_services::exception::write_no_exception(reply);

    with_state(|state| match method.name.as_str() {
        "startActivity" | "startActivityWithFeature" | "startActivityAsUser" => {
            let (intent_i, result_i, request_i, flags_i) = match method.name.as_str() {
                "startActivity" => (2, 4, 6, 7),
                "startActivityWithFeature" => (3, 5, 7, 8),
                _ => (2, 4, 6, 7),
            };
            let intent = intent_at(&args, intent_i)?;
            let flags = args.get(flags_i).map(Arg::int).unwrap_or(0);
            let result_to = args.get(result_i).map(Arg::binder).unwrap_or(0);
            let request_code = args.get(request_i).map(Arg::int).unwrap_or(0);
            let code = match state.start_activity(&intent, flags, result_to, request_code) {
                Ok(code) => code,
                Err(acos_services::ServiceError::IllegalState { .. }) => {
                    acos_services::ams::START_NOT_FOUND
                }
                Err(e) => return Err(err(e)),
            };
            reply.write_int32(code);
            Ok(Vec::new())
        }
        "startService" => {
            let intent = intent_at(&args, 1)?;
            let (package, class) = state.start_service(&intent).map_err(err)?;
            let cn = component_name(vm, &package, &class)?;
            crate::pms_java::typed_object(vm, Some(cn), reply)?;
            Ok(Vec::new())
        }
        "stopService" => {
            let intent = intent_at(&args, 1)?;
            let stopped = state.stop_service(&intent).map_err(err)?;
            reply.write_int32(i32::from(stopped));
            Ok(Vec::new())
        }
        "bindService" | "bindServiceInstance" => {
            let connection = args.get(4).map(Arg::binder).unwrap_or(0);
            let intent = intent_at(&args, 2)?;
            let flags = args.get(5).map(Arg::long).unwrap_or(0);
            let binder = state.bind_service(connection, &intent, flags).map_err(err)?;
            reply
                .write_int32(i32::try_from(binder & 0x7fff_ffff).unwrap_or(0));
            Ok(Vec::new())
        }
        "unbindService" => {
            let connection = args.first().map(Arg::binder).unwrap_or(0);
            let released = state.unbind_service(connection);
            reply.write_int32(i32::from(released));
            Ok(Vec::new())
        }
        "registerReceiver" | "registerReceiverWithFeature" => {
            let (receiver_i, filter_i, flags_i) = match method.name.as_str() {
                "registerReceiver" => (2, 3, 5),
                _ => (4, 5, 7),
            };
            let receiver = args.get(receiver_i).map(Arg::binder).unwrap_or(0);
            let (actions, categories) = match args.get(filter_i) {
                Some(Arg::Filter(a, c)) => (a.clone(), c.clone()),
                _ => (Vec::new(), Vec::new()),
            };
            let flags = args.get(flags_i).map(Arg::int).unwrap_or(0);
            state.register_receiver(receiver, "", actions, categories, flags);
            crate::pms_java::typed_object(vm, None, reply)?;
            Ok(Vec::new())
        }
        "unregisterReceiver" => {
            let receiver = args.first().map(Arg::binder).unwrap_or(0);
            state.unregister_receiver(receiver);
            Ok(Vec::new())
        }
        "broadcastIntent" | "broadcastIntentWithFeature" => {
            let (intent_i, result_i, code_i, data_i) = match method.name.as_str() {
                "broadcastIntent" => (1, 3, 4, 5),
                _ => (2, 4, 5, 6),
            };
            let intent = intent_at(&args, intent_i)?;
            let result_to = args.get(result_i).map(Arg::binder).unwrap_or(0);
            let result_code = args.get(code_i).map(Arg::int).unwrap_or(0);
            let result_data = args
                .get(data_i)
                .and_then(Arg::string)
                .map(str::to_string);
            let count = state
                .broadcast(&intent, result_to, result_code, result_data)
                .map_err(err)?;
            reply.write_int32(count);
            Ok(Vec::new())
        }
        "getContentProvider" => {
            // `getContentProvider(IApplicationThread caller, String callingPackage,
            // String name, int userId, boolean stable)`: the requested authority
            // is the *second* string (arg 2), not the calling package.
            let authority = args.get(2).and_then(Arg::string).unwrap_or_default();
            match state.get_content_provider(authority) {
                Some(p) => {
                    // The hardcoded `settings` record carries no binder id (it
                    // is not published by an app); use the binder the host
                    // registered for the `IContentProvider` service so the
                    // holder's `provider` is real and `Settings` can call it.
                    let binder = if p.binder != 0 {
                        p.binder
                    } else {
                        acos_services::provider::settings_provider_binder()
                    };
                    let holder =
                        content_provider_holder(vm, &p.package, &p.class, authority, binder)?;
                    crate::pms_java::typed_object(vm, Some(holder), reply)?;
                }
                None => crate::pms_java::typed_object(vm, None, reply)?,
            }
            Ok(Vec::new())
        }
        "publishContentProviders" => Err(err(
            "publishContentProviders: the app-side List<ContentProviderHolder> is not decoded \
             yet (W51-A owns the provider parcel fix)",
        )),
        other => Err(err(format!("unhandled owned method {other}"))),
    })
}

fn intent_at(args: &[Arg], index: usize) -> JResult<IntentFacts> {
    match args.get(index) {
        Some(Arg::Intent(Some(facts))) => Ok(facts.clone()),
        Some(Arg::Intent(None)) => Err(err(format!("arg {index}: intent is null"))),
        _ => Err(err(format!("arg {index}: expected an Intent"))),
    }
}

// ---------------------------------------------------------------------------
// IContentProvider
// ---------------------------------------------------------------------------

/// `IContentProvider` query/insert/update/delete.
///
/// ACOS V1 has no guest-hosted provider process, so there is no real
/// `ContentProviderNative`/`Transport` to marshal against. Rather than return a
/// faked `Cursor`, every mutating/reading method raises a structured
/// `UNSUPPORTED_ANDROID_API`.
fn transact_content_provider(
    vm: &mut Vm,
    method: &acos_aidl::MethodInfo,
    data: &mut Parcel,
    reply: &mut Parcel,
) -> JResult<Vec<BinderDelivery>> {
    let _ = vm;
    let _ = data;
    reply.reset();
    acos_services::exception::write_no_exception(reply);
    Err(err(format!(
        "IContentProvider.{}: no guest provider process; query/insert/update/delete require a \
         real ContentProviderNative marshaller (CursorWindow/BulkCursorDescriptor) that ACOS \
         does not have yet (UNSUPPORTED_ANDROID_API)",
        method.name
    )))
}

// ---------------------------------------------------------------------------
// Deliveries
// ---------------------------------------------------------------------------

/// Materialise the queued `IServiceConnection.connected` deliveries as real
/// binder deliveries.
pub fn service_connection_deliveries(vm: &mut Vm) -> JResult<Vec<BinderDelivery>> {
    let pending = take_pending_binds();
    let mut out = Vec::with_capacity(pending.len());
    for bind in pending {
        let mut data = Parcel::new();
        data.write_interface_token("android.app.IServiceConnection")
            .map_err(err)?;
        // `connected(ComponentName name, IBinder service, boolean dead)`.
        let cn = component_name(vm, &bind.package, &bind.class)?;
        crate::pms_java::typed_object(vm, Some(cn), &mut data)?;
        data.write_strong_binder(BinderId(bind.binder));
        data.write_int32(0); // dead
        out.push(BinderDelivery {
            target: bind.connection,
            // `IServiceConnection.connected` is the first transaction.
            code: 1,
            flags: 1,
            data,
        });
    }
    Ok(out)
}

/// Materialise the queued `IIntentReceiver.performReceive` deliveries.
pub fn intent_receiver_deliveries(vm: &mut Vm) -> JResult<Vec<BinderDelivery>> {
    let pending = take_pending_receivers();
    let mut out = Vec::with_capacity(pending.len());
    for receiver in pending {
        let mut data = Parcel::new();
        data.write_interface_token("android.content.IIntentReceiver")
            .map_err(err)?;
        // `performReceive(Intent intent, int resultCode, String data, Bundle
        // extras, boolean ordered, boolean sticky, int sendingUser)`.
        let intent = materialize_intent(vm, &receiver.intent)?;
        crate::pms_java::typed_object(vm, Some(intent), &mut data)?;
        data.write_int32(receiver.result_code);
        data.write_string16_opt(receiver.result_data.as_deref())
            .map_err(err)?;
        // extras: null.
        data.write_int32(0);
        data.write_int32(i32::from(receiver.ordered));
        data.write_int32(0); // sticky
        data.write_int32(0); // sendingUser
        out.push(BinderDelivery {
            target: receiver.receiver,
            code: 1,
            flags: 1,
            data,
        });
    }
    Ok(out)
}

/// Materialise every queued delivery (binds + broadcasts).
pub fn take_deliveries(vm: &mut Vm) -> JResult<Vec<BinderDelivery>> {
    let mut out = service_connection_deliveries(vm)?;
    out.extend(intent_receiver_deliveries(vm)?);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Launch materialisation
// ---------------------------------------------------------------------------

/// Duplicated from `client_transaction` so the AMS launch path does not reach
/// into a file another workstream owns.
const LAUNCH_OBTAIN_SIG: &str = "(Landroid/content/Intent;ILandroid/content/pm/ActivityInfo;Landroid/content/res/Configuration;Landroid/content/res/Configuration;ILjava/lang/String;Lcom/android/internal/app/IVoiceInteractor;ILandroid/os/Bundle;Landroid/os/PersistableBundle;Ljava/util/List;Ljava/util/List;Landroid/app/ActivityOptions;ZLandroid/app/ProfilerInfo;Landroid/os/IBinder;Landroid/app/IActivityClientController;Landroid/os/IBinder;ZLandroid/os/IBinder;)Landroid/app/servertransaction/LaunchActivityItem;";

/// Turn one queued [`PendingLaunch`] into a real `ClientTransaction` and hand
/// it to the real `ApplicationThread.scheduleTransaction`. Returns the
/// transaction token (a real `android.os.Binder`).
pub fn schedule_launch(
    vm: &mut Vm,
    app_thread: Handle,
    app: &AppInfo,
    launch: &PendingLaunch,
) -> JResult<Handle> {
    let activity = PmsActivityInfo {
        name: launch.class.clone(),
        package_name: launch.package.clone(),
        launch_mode: 0,
        flags: 0,
        theme: 0,
        exported: true,
        parent_activity_name: String::new(),
    };
    let info = crate::pms_java::activity_info(vm, app, &activity)?;
    let intent = materialize_intent(vm, &launch.intent)?;
    let config = crate::bind_app::build_config(vm).map_err(err)?;

    let launch_item = as_obj(
        vm.call_static(
            "android.app.servertransaction.LaunchActivityItem",
            "obtain",
            LAUNCH_OBTAIN_SIG,
            &[
                JavaValue::Object(intent),
                JavaValue::Int(0),
                JavaValue::Object(info),
                JavaValue::Object(config),
                JavaValue::Object(config),
                JavaValue::Int(0),
                JavaValue::Null,
                JavaValue::Null,
                JavaValue::Int(2),
                JavaValue::Null,
                JavaValue::Null,
                JavaValue::Null,
                JavaValue::Null,
                JavaValue::Null,
                JavaValue::Boolean(false),
                JavaValue::Null,
                JavaValue::Null,
                JavaValue::Null,
                JavaValue::Null,
                JavaValue::Boolean(false),
                JavaValue::Null,
            ],
        )
        .map_err(err)?,
    )?;
    let resume_item = as_obj(
        vm.call_static(
            "android.app.servertransaction.ResumeActivityItem",
            "obtain",
            "(ZZ)Landroid/app/servertransaction/ResumeActivityItem;",
            &[JavaValue::Boolean(false), JavaValue::Boolean(false)],
        )
        .map_err(err)?,
    )?;
    let token = vm
        .new_instance_ctor("android.os.Binder", "()V", &[])
        .map_err(err)?;
    let transaction = as_obj(
        vm.call_static(
            "android.app.servertransaction.ClientTransaction",
            "obtain",
            "(Landroid/app/IApplicationThread;Landroid/os/IBinder;)Landroid/app/servertransaction/ClientTransaction;",
            &[JavaValue::Object(app_thread), JavaValue::Object(token)],
        )
        .map_err(err)?,
    )?;
    vm.call_method(
        transaction,
        "addCallback",
        "(Landroid/app/servertransaction/ClientTransactionItem;)V",
        &[JavaValue::Object(launch_item)],
    )
    .map_err(err)?;
    vm.call_method(
        transaction,
        "setLifecycleStateRequest",
        "(Landroid/app/servertransaction/ActivityLifecycleItem;)V",
        &[JavaValue::Object(resume_item)],
    )
    .map_err(err)?;
    vm.call_method(
        app_thread,
        "scheduleTransaction",
        "(Landroid/app/servertransaction/ClientTransaction;)V",
        &[JavaValue::Object(transaction)],
    )
    .map_err(err)?;
    Ok(token)
}

/// Schedule every queued launch against `app_thread`.
pub fn schedule_pending_launches(
    vm: &mut Vm,
    app_thread: Handle,
    app: &AppInfo,
) -> JResult<Vec<Handle>> {
    let launches = take_pending_launches();
    let mut tokens = Vec::with_capacity(launches.len());
    for launch in launches {
        tokens.push(schedule_launch(vm, app_thread, app, &launch)?);
    }
    Ok(tokens)
}

/// Load a framework class and run its `<clinit>` (for a `CREATOR`).
pub fn ensure_class(vm: &mut Vm, class: &str) -> JResult<()> {
    let cid = vm.load_class(&class_descriptor(class)).map_err(err)?;
    vm.initialize_class(cid).map_err(err)
}
