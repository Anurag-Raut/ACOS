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
//! launch cannot be materialised is a structured error.
//!
//! # Content providers
//!
//! The provider set is **not** owned here: `getContentProvider` and
//! `IContentProvider.call`/`query` are forwarded to the runtime [`ServiceHost`],
//! which answers from `acos_services::provider::ProviderRegistry` (Q2b). The
//! Rust VM route and the ART binder-peer route therefore share one
//! implementation, and no `ProviderInfo`/holder/`Bundle`/`CursorWindow` byte is
//! written by this module.
//!
//! # State
//!
//! The `AmsState` is process-wide (thread-local, matching the single-threaded
//! VM) so it survives across the guest's transactions. It is seeded from the
//! launched APK with [`install_manifest_from_apk`].

use std::cell::RefCell;
use std::rc::Rc;

use acos_parcel::{binder::BinderId, Parcel};
use acos_services::ams::{
    AmsState, Extra, IntentFacts, PendingBind, PendingLaunch, PendingReceiver,
};
use acos_services::pms::{ActivityInfo as PmsActivityInfo, AppInfo};
use acos_services::ServiceHost;
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
    /// The process-wide `ServiceHost`, so the provider transactions this module
    /// answers go through the same registry the ART binder-peer dispatches to.
    ///
    /// Interior mutability and the thread-local slot mirror [`STATE`]: the VM is
    /// single-threaded, and a nested binder transaction (a provider calling
    /// `notifyChange`, say) must be able to re-enter.
    static HOST: RefCell<Option<Rc<RefCell<ServiceHost>>>> = const { RefCell::new(None) };
}

/// Forget all AMS state (tests / a fresh launch).
pub fn reset() {
    STATE.with(|s| *s.borrow_mut() = AmsState::new());
}

/// Install the `ServiceHost` the provider transactions are answered from.
///
/// Called by [`crate::Runtime::with_host`]: the runtime owns one host, its
/// `ServiceTransport` dispatches to it, and this module forwards the content
/// provider transactions to the very same instance so the provider set lives in
/// exactly one place (`acos_services::provider::ProviderRegistry`).
pub fn install_host(host: Rc<RefCell<ServiceHost>>) {
    HOST.with(|slot| *slot.borrow_mut() = Some(host));
}

/// Drop the installed host (tests / teardown).
pub fn clear_host() {
    HOST.with(|slot| *slot.borrow_mut() = None);
}

fn installed_host() -> Option<Rc<RefCell<ServiceHost>>> {
    HOST.with(|slot| slot.borrow().clone())
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
    /// `publishContentProviders`' `List<ContentProviderHolder>` (real Java
    /// objects read with the framework `CREATOR`, one presence marker each).
    Holders(Vec<Handle>),
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

fn decode_args(
    vm: &mut Vm,
    method: &acos_aidl::MethodInfo,
    data: &mut Parcel,
) -> JResult<Vec<Arg>> {
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
            // `publishContentProviders(IApplicationThread, List<ContentProviderHolder>)`
            // is decoded from the decoded `writeTypedList` op: the table records
            // the list type (`java.util.List`), not the element type, so the
            // element class comes from the method contract.
            acos_aidl::WireOp::TypedList { .. }
                if method.name == "publishContentProviders" && out.len() == 1 =>
            {
                let count = data.read_int32().map_err(err)?;
                let mut holders = Vec::with_capacity(count.max(0) as usize);
                for _ in 0..count.max(0) {
                    if let Some(holder) = crate::parcel_java::read_typed_object(
                        vm,
                        data,
                        "android.app.ContentProviderHolder",
                    )? {
                        holders.push(holder);
                    }
                }
                Arg::Holders(holders)
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
        .call_method(intent, "getCategories", "()Ljava/util/Set;", &[])
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

    let type_ = opt_string(vm, intent, "getType", "()Ljava/lang/String;")?;
    let identifier = opt_string(vm, intent, "getIdentifier", "()Ljava/lang/String;")?;
    let flags = match vm
        .call_method(intent, "getFlags", "()I", &[])
        .map_err(err)?
    {
        JavaValue::Int(v) => v,
        _ => 0,
    };
    let source_bounds = match vm
        .call_method(intent, "getSourceBounds", "()Landroid/graphics/Rect;", &[])
        .map_err(err)?
    {
        JavaValue::Object(h) if h != NULL => {
            let int_field = |vm: &mut Vm, name: &str| -> JResult<i32> {
                match vm.get_field(h, name).map_err(err)? {
                    JavaValue::Int(v) => Ok(v),
                    other => Err(err(format!("Rect.{name} is {}", other.kind()))),
                }
            };
            Some([
                int_field(vm, "left")?,
                int_field(vm, "top")?,
                int_field(vm, "right")?,
                int_field(vm, "bottom")?,
            ])
        }
        _ => None,
    };
    let selector = match vm
        .call_method(intent, "getSelector", "()Landroid/content/Intent;", &[])
        .map_err(err)?
    {
        JavaValue::Object(h) if h != NULL => Some(Box::new(intent_facts(vm, h)?)),
        _ => None,
    };
    // `ClipData` is presence-only here: the reduced facts record that the field
    // is set (which is all `Intent.fillIn` reads), and a delivery that would
    // have to encode the body is a structured `UnsupportedParcelable` instead
    // of a guessed payload.
    let clip_data = match vm
        .call_method(intent, "getClipData", "()Landroid/content/ClipData;", &[])
        .map_err(err)?
    {
        JavaValue::Object(h) if h != NULL => Some(acos_services::Parcelable::ordered(
            "android.content.ClipData",
            Vec::new(),
        )),
        _ => None,
    };

    Ok(IntentFacts {
        action,
        data,

        type_,
        identifier,
        // MPROC1: the capture target records the flags too; the Rust VM path
        // reads them from the real Intent object, never infers them.
        flags,
        package,
        component,
        source_bounds,
        categories,
        selector,
        clip_data,
        // `getContentUserHint()` is `@hide`; the hint only matters for the
        // Uri-copy path, and a guest-sent intent carries `USER_CURRENT` unless
        // the framework set it (the fill-in path reads the hint off the wire,
        // not through this reducer).
        content_user_hint: acos_services::fill_in::USER_CURRENT,
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
fn bundle_extras(
    vm: &mut Vm,
    bundle: Handle,
) -> JResult<std::collections::BTreeMap<String, Extra>> {
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
                        other => {
                            return Err(err(format!("Integer.intValue returned {}", other.kind())))
                        }
                    },
                    "java.lang.Boolean" => {
                        match vm.call_method(h, "booleanValue", "()Z", &[]).map_err(err)? {
                            JavaValue::Boolean(v) => Extra::Bool(v),
                            JavaValue::Int(v) => Extra::Bool(v != 0),
                            other => {
                                return Err(err(format!(
                                    "Boolean.booleanValue returned {}",
                                    other.kind()
                                )))
                            }
                        }
                    }
                    "java.lang.String[]" => {
                        let len = match vm.call_method(h, "length", "()I", &[]).map_err(err)? {
                            JavaValue::Int(v) => v,
                            other => {
                                return Err(err(format!(
                                    "String[].length returned {}",
                                    other.kind()
                                )))
                            }
                        };
                        let mut items = Vec::new();
                        for index in 0..len.max(0) {
                            let item = vm
                                .call_method(
                                    h,
                                    "get",
                                    "(I)Ljava/lang/Object;",
                                    &[JavaValue::Int(index)],
                                )
                                .map_err(err)?;
                            match item {
                                JavaValue::Object(item) if item != NULL => {
                                    items.push(vm.java_string_to_rust(item).map_err(err)?)
                                }
                                _ => return Err(err("String[] element is null".to_string())),
                            }
                        }
                        // MODNOTIF2: `EXTRA_REQUEST_PERMISSIONS_NAMES` is the
                        // only String[] the ATMS request path reads.
                        Extra::Strings(items)
                    }
                    other => {
                        return Err(err(format!(
                            "extra `{key}` has unsupported type {other}; AMS the bridge \
                             only carries String/int/boolean/String[] extras"
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

fn counted_strings(vm: &mut Vm, obj: Handle, count: &str, get: &str) -> JResult<Vec<String>> {
    let n = match vm.call_method(obj, count, "()I", &[]).map_err(err)? {
        JavaValue::Int(v) if v > 0 => v as usize,
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        match vm
            .call_method(
                obj,
                get,
                "(I)Ljava/lang/String;",
                &[JavaValue::Int(i as i32)],
            )
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
fn provider_application_info(vm: &mut Vm, package: &str, process_name: &str) -> JResult<Handle> {
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
        icon: 0,
        resource_dirs: Vec::new(),
        split_names: Vec::new(),
        shared_library_files: Vec::new(),
        target_sandbox_version: 2,
        // The provider's application info carries no `<meta-data>`: the
        // runtime path materialises no `Bundle` (see `pms_java.rs`).
        meta_data: Vec::new(),
    };
    crate::pms_java::application_info(vm, &app)
}

/// Materialise a real `ProviderInfo` (`getContentProvider`).
///
/// The field set is the API-34 `PackageItemInfo` -> `ComponentInfo` ->
/// `ProviderInfo` chain; the framework's own `writeToParcel` writes them in
/// order once they are set.
fn provider_info(vm: &mut Vm, package: &str, class: &str, authority: &str) -> JResult<Handle> {
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
    set_str(
        vm,
        h,
        "writePermission",
        "android.permission.WRITE_SETTINGS",
    )?;
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

/// A `ProviderInfo` for an app-declared provider (bind-time install).
///
/// The shared [`provider_info`] builder targets the `settings` provider and
/// therefore carries the settings read/write permissions; those are replaced by
/// the **manifest's** `<provider android:exported>`, `android:permission` (or
/// `android:readPermission`/`writePermission`) values (W94-A #9). Hard-coding
/// `exported = true` with no permissions let the binder policy be bypassed for
/// a provider the app declared private.
///
/// `ContentProvider.Transport` calls `enforceReadPermission`/
/// `enforceWritePermission` for every CRUD call, so a leaked `READ_SETTINGS`
/// would also make the app fail its own provider access; a manifest without a
/// permission therefore yields null permissions.
pub(crate) fn app_provider_info(
    vm: &mut Vm,
    package: &str,
    spec: &crate::bind_app::ProviderSpec,
) -> JResult<Handle> {
    let h = provider_info(vm, package, &spec.class, &spec.authority)?;
    for (field, value) in [
        ("readPermission", &spec.read_permission),
        ("writePermission", &spec.write_permission),
    ] {
        if value.is_empty() {
            vm.set_field(h, field, JavaValue::Null).map_err(err)?;
        } else {
            let s = vm.new_string_rooted(value).map_err(err)?;
            vm.set_field(h, field, JavaValue::Object(s)).map_err(err)?;
        }
    }
    vm.set_field(h, "exported", JavaValue::Boolean(spec.exported))
        .map_err(err)?;
    Ok(h)
}

/// Materialise a real `ContentProviderHolder` carrying a real `ProviderInfo`
/// and (when known) a real `android.content.ContentProviderProxy` for the
/// provider.
///
/// This is the **app-published** holder path only (`publishContentProviders` at
/// bind time): those providers live in [`AmsState::providers`], not in the
/// `acos-services` registry, and carry `noReleaseNeeded = true` with no
/// connection token because the app owns them in this process. A registry-served
/// provider (the `settings` authority) never reaches this function — its holder
/// is written by `AtmsService` (Q2b).
///
/// The proxy wraps a `BinderProxy` whose `mNativeData` is the binder id, so
/// `IContentProvider.asBinder()` (called by `writeToParcel`) returns a binder
/// the ACOS transport routes to the app's local `ContentProvider.Transport`.
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
            .new_instance_ctor(
                "android.os.BinderProxy",
                "(J)V",
                &[JavaValue::Long(binder as i64)],
            )
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
    vm.set_field(obj, field, JavaValue::Boolean(value))
        .map_err(err)
}

/// The binder id carried by a real `IBinder`.
///
/// A `BinderProxy` (a system service) keeps it in `mNativeData`; a local
/// `Binder` (what a same-process `ContentProvider.Transport` resolves to) keeps
/// it in `mObject`. Both are the ids the ACOS `Parcel` writes.
fn binder_id_of(vm: &mut Vm, binder: Handle) -> JResult<u64> {
    for field in ["mNativeData", "mObject"] {
        match vm.get_field(binder, field) {
            Ok(JavaValue::Long(v)) => return Ok(u64::try_from(v).unwrap_or(0)),
            Ok(JavaValue::Int(v)) => return Ok(u32::try_from(v).unwrap_or(0) as u64),
            Ok(_) | Err(_) => {}
        }
    }
    Err(err(format!(
        "binder handle {binder} carries neither mNativeData nor mObject"
    )))
}

/// Read a `String` field (`None` when the field is null).
fn field_string(vm: &mut Vm, obj: Handle, field: &str) -> JResult<Option<String>> {
    match vm.get_field(obj, field).map_err(err)? {
        JavaValue::Object(h) if h != NULL => Ok(Some(vm.java_string_to_rust(h).map_err(err)?)),
        _ => Ok(None),
    }
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
            Extra::Short(v) => {
                vm.call_method(
                    intent,
                    "putExtra",
                    "(Ljava/lang/String;S)Landroid/content/Intent;",
                    &[JavaValue::Object(k), JavaValue::Short(*v)],
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
            Extra::Bytes(bytes) => {
                let values: Vec<JavaValue> =
                    bytes.iter().map(|b| JavaValue::Byte(*b as i8)).collect();
                let arr = vm.new_array("B", &values).map_err(err)?;
                vm.call_method(
                    intent,
                    "putExtra",
                    "(Ljava/lang/String;[B)Landroid/content/Intent;",
                    &[JavaValue::Object(k), JavaValue::Object(arr)],
                )
                .map_err(err)?;
            }
            Extra::ByteArrays(pdus) => {
                // `SMS_RECEIVED`'s `pdus` is an `Object[]` of `byte[]`, which
                // `putExtra(String, Serializable)` stores as a
                // `Parcel.VAL_OBJECTARRAY` (byte[][] is an Object[]).
                let mut elements = Vec::with_capacity(pdus.len());
                for pdu in pdus {
                    let values: Vec<JavaValue> =
                        pdu.iter().map(|b| JavaValue::Byte(*b as i8)).collect();
                    elements.push(JavaValue::Object(
                        vm.new_array("B", &values).map_err(err)?,
                    ));
                }
                let array = vm.new_array("[B", &elements).map_err(err)?;
                vm.call_method(
                    intent,
                    "putExtra",
                    "(Ljava/lang/String;Ljava/io/Serializable;)Landroid/content/Intent;",
                    &[JavaValue::Object(k), JavaValue::Object(array)],
                )
                .map_err(err)?;
            }
            Extra::Parcelable(_) => {
                return Err(err(format!(
                    "cannot re-materialise extra `{key}`: this value is a parcelable and the VM path has no class-loader construction for it"
                )))
            }
            // MODNOTIF2/MODNOTIF5: the request direction carries a `String[]`
            // (`EXTRA_REQUEST_PERMISSIONS_NAMES`), which this path does not
            // materialise back into Java; the `Present` refusal covers both.
            Extra::Strings(_) | Extra::Present => {
                return Err(err(format!(
                    "cannot re-materialise extra `{key}`: its type is not carried back into Java \
                     (only the framework->system direction needs it, e.g. the permission \
                     request's EXTRA_REQUEST_PERMISSIONS_NAMES)"
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
    binder: BinderId,
    code: u32,
    data: &mut Parcel,
    reply: &mut Parcel,
    flags: u32,
) -> JResult<Vec<BinderDelivery>> {
    let descriptor = data
        .peek_interface_token()
        .map_err(err)?
        .ok_or_else(|| err("missing interface token"))?;
    let method = acos_aidl::lookup(&descriptor, code)
        .ok_or_else(|| err(format!("unknown method code {code} for {descriptor}")))?;

    if descriptor == "android.content.IContentProvider" {
        return transact_content_provider(vm, binder, method, data, reply, flags);
    }
    if method.name == "getContentProvider" {
        // The provider set is the `acos-services` registry's, not this module's:
        // delegate before the generic request decoding runs, so the host sees
        // the untouched request parcel.
        return transact_get_content_provider(vm, binder, method, data, reply, flags);
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
            // The positions come from the shared, bytecode-pinned table so the
            // runtime and `acos-services` cannot drift apart (W74-A).
            let positions = acos_services::ams::start_activity_args(&descriptor, &method.name)
                .ok_or_else(|| err(format!("no startActivity layout for {}", method.name)))?;
            let intent = intent_at(&args, positions.intent)?;
            let flags = args.get(positions.flags).map(Arg::int).unwrap_or(0);
            let result_to = args.get(positions.result_to).map(Arg::binder).unwrap_or(0);
            let request_code = args.get(positions.request_code).map(Arg::int).unwrap_or(0);
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
            let binder = state
                .bind_service(connection, &intent, flags)
                .map_err(err)?;
            reply.write_int32(i32::try_from(binder & 0x7fff_ffff).unwrap_or(0));
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
            state.register_receiver(receiver, "", actions.clone(), categories.clone(), flags);
            // PIFILLIN: mirror the registration into the session's host-side
            // AMS state. A broadcast the **system** originates (a module's
            // control op: a scripted fix, an SMS, an adapter event — and a fired
            // `PendingIntent`, whose sender record lives host-side) is delivered
            // from there, and until this mirror existed those broadcasts matched
            // no dynamic receiver at all in ART. The guest's own
            // `broadcastIntent` keeps using this state, so a receiver is never
            // delivered twice.
            if let Some(host) = installed_host() {
                let _ = host
                    .borrow_mut()
                    .mirror_dynamic_receiver(receiver, actions, categories, flags);
            }
            crate::pms_java::typed_object(vm, None, reply)?;
            Ok(Vec::new())
        }
        "unregisterReceiver" => {
            let receiver = args.first().map(Arg::binder).unwrap_or(0);
            state.unregister_receiver(receiver);
            if let Some(host) = installed_host() {
                let _ = host.borrow_mut().mirror_unregister_receiver(receiver);
            }
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
            let result_data = args.get(data_i).and_then(Arg::string).map(str::to_string);
            let count = state
                .broadcast(&intent, result_to, result_code, result_data)
                .map_err(err)?;
            reply.write_int32(count);
            Ok(Vec::new())
        }
        "publishContentProviders" => {
            // `ActivityThread.installContentProviders` reports every provider it
            // created for this process. Decode the real holders and record the
            // authority -> (class, binder) mapping so a later
            // `getContentProvider` returns the provider the app actually owns.
            let holders = match args.get(1) {
                Some(Arg::Holders(holders)) => holders.clone(),
                Some(Arg::Other) | None => Vec::new(),
                _ => {
                    return Err(err(
                        "publishContentProviders: arg 1 is not a List<ContentProviderHolder>",
                    ))
                }
            };
            let mut records = Vec::with_capacity(holders.len());
            for holder in holders {
                let info = match vm.get_field(holder, "info").map_err(err)? {
                    JavaValue::Object(h) if h != NULL => h,
                    _ => return Err(err("publishContentProviders: holder.info is null")),
                };
                let authority = field_string(vm, info, "authority")?.ok_or_else(|| {
                    err("publishContentProviders: ProviderInfo.authority is null")
                })?;
                let class = field_string(vm, info, "name")?
                    .ok_or_else(|| err("publishContentProviders: ProviderInfo.name is null"))?;
                let package =
                    field_string(vm, info, "packageName")?.unwrap_or_else(|| "".to_string());
                let binder = match vm.get_field(holder, "provider").map_err(err)? {
                    JavaValue::Object(provider) if provider != NULL => {
                        match vm
                            .call_method(provider, "asBinder", "()Landroid/os/IBinder;", &[])
                            .map_err(err)?
                        {
                            JavaValue::Object(b) if b != NULL => binder_id_of(vm, b)?,
                            _ => 0,
                        }
                    }
                    _ => 0,
                };
                records.push(acos_services::ams::ProviderRecord {
                    authority,
                    package,
                    class,
                    binder,
                });
            }
            state.publish_content_providers(records);
            Ok(Vec::new())
        }
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
// Content providers (answered by `acos-services`)
// ---------------------------------------------------------------------------

/// `IActivityManager.getContentProvider`, answered from the `acos-services`
/// provider registry.
///
/// The authority list lives in exactly one place: [`acos_services::provider::ProviderRegistry`]
/// behind the `ServiceHost`. The Rust VM route (here) and the ART binder-peer
/// route (`acos-binder-peer` -> `ServiceHost::transact`) therefore return the
/// *same* holder, built by `AtmsService`: a real API-34
/// `ContentProviderHolder` (`ProviderInfo`, the local `IContentProvider`
/// binder, the `ContentProviderConnection` token, `noReleaseNeeded=0`,
/// `mLocal=1`).
///
/// The fallback is the app's *own* providers: `publishContentProviders` records
/// those in [`AmsState::providers`] because they are Java `Binder`s installed at
/// bind time, not registry entries.
fn transact_get_content_provider(
    vm: &mut Vm,
    binder: BinderId,
    method: &acos_aidl::MethodInfo,
    data: &mut Parcel,
    reply: &mut Parcel,
    flags: u32,
) -> JResult<Vec<BinderDelivery>> {
    let descriptor = data
        .peek_interface_token()
        .map_err(err)?
        .ok_or_else(|| err("missing interface token"))?;

    if let Some(host) = installed_host() {
        let mut request = data.clone();
        let deliveries = dispatch_to_host(
            &host,
            binder,
            &descriptor,
            method.code,
            &mut request,
            reply,
            flags,
        )?;
        // A registry hit is `writeNoException()` + a present typed object; only
        // a recorded miss falls through to the app-published provider table.
        if reply_has_typed_object(reply) {
            return Ok(deliveries);
        }
    }

    // `getContentProvider(IApplicationThread caller, String callingPackage,
    // String name, int userId, boolean stable)`: the requested authority is the
    // *second* string (arg 2), not the calling package.
    data.enforce_interface(&descriptor).map_err(err)?;
    let args = decode_args(vm, method, data)?;
    let authority = args.get(2).and_then(Arg::string).unwrap_or_default();
    let record = with_state(|state| state.providers.get(authority).cloned());
    reply.reset();
    acos_services::exception::write_no_exception(reply);
    match record {
        Some(p) => {
            let holder = content_provider_holder(vm, &p.package, &p.class, authority, p.binder)?;
            crate::pms_java::typed_object(vm, Some(holder), reply)?;
        }
        None => crate::pms_java::typed_object(vm, None, reply)?,
    }
    Ok(Vec::new())
}

/// Forward one transaction to the installed `ServiceHost`.
///
/// `binder` is used when the host knows it (the provider binder a
/// `ContentProviderProxy` calls through); otherwise the target is resolved from
/// the request's interface token, which is how the AMS bridge reaches
/// `IActivityManager`/`IActivityTaskManager` without knowing their handles.
fn dispatch_to_host(
    host: &Rc<RefCell<ServiceHost>>,
    binder: BinderId,
    descriptor: &str,
    code: u32,
    request: &mut Parcel,
    reply: &mut Parcel,
    flags: u32,
) -> JResult<Vec<BinderDelivery>> {
    let mut host = host.borrow_mut();
    let target = if host.descriptor_of(binder).is_some() {
        binder
    } else {
        host.binder_for_descriptor(descriptor).ok_or_else(|| {
            err(format!(
                "UNSUPPORTED_ANDROID_API: no ServiceHost entry for {descriptor}"
            ))
        })?
    };
    host.transact(target, code, request, reply, flags)
        .map_err(err)?;
    let pending = host.take_deliveries();
    Ok(pending
        .into_iter()
        .map(|d| BinderDelivery {
            target: d.target.0,
            code: d.code,
            flags: 0,
            data: d.data,
        })
        .collect())
}

/// True when `reply` (as the host just wrote it) carries a non-null typed
/// object: `writeNoException()` + `writeTypedObject(object)`.
fn reply_has_typed_object(reply: &Parcel) -> bool {
    let mut probe = reply.clone();
    probe.set_data_position(0);
    matches!(probe.read_int32(), Ok(0)) && matches!(probe.read_int32(), Ok(present) if present != 0)
}

// ---------------------------------------------------------------------------
// IContentProvider
// ---------------------------------------------------------------------------

/// `IContentProvider` query/insert/update/delete/call.
///
/// Answered by the same [`ServiceHost`] the ART binder-peer route dispatches to,
/// so the provider implementation (`acos_services::provider::ProviderService`:
/// `call` with `GET_*`/`PUT_*`/`GET_config`/`LIST_config`, and `query` with the
/// real `BulkCursorDescriptor` + `CursorWindow` reply) is shared. This module
/// hand-writes no provider payload: the request bytes go to the host untouched
/// and the reply comes back in the framework's own shape.
///
/// `insert`/`update`/`delete`/`bulkInsert` stay a structured
/// `UNSUPPORTED_ANDROID_API`, as the host answers them.
fn transact_content_provider(
    vm: &mut Vm,
    binder: BinderId,
    method: &acos_aidl::MethodInfo,
    data: &mut Parcel,
    reply: &mut Parcel,
    flags: u32,
) -> JResult<Vec<BinderDelivery>> {
    let descriptor = data
        .peek_interface_token()
        .map_err(err)?
        .ok_or_else(|| err("missing interface token"))?;
    let host = installed_host().ok_or_else(|| {
        err(format!(
            "IContentProvider.{}: UNSUPPORTED_ANDROID_API: no ServiceHost installed, so there is \
             no provider registry to answer from",
            method.name
        ))
    })?;
    let deliveries = dispatch_to_host(&host, binder, &descriptor, method.code, data, reply, flags)
        .map_err(|e| format!("IContentProvider.{}: {e}", method.name))?;
    // The framework instantiates the generation `MemoryIntArray` through its
    // static `CREATOR`; make sure `<clinit>` ran before the Java reader does.
    crate::settings_java::prepare_settings_reply(vm).map_err(err)?;
    Ok(deliveries)
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
        flags: 0x200, // ActivityInfo.FLAG_HARDWARE_ACCELERATED
        theme: 0,
        exported: true,
        parent_activity_name: String::new(),
        meta_data: Vec::new(),
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
