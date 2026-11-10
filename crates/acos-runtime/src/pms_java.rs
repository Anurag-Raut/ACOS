//! Materialise the real framework `ApplicationInfo` / `PackageInfo` /
//! `ActivityInfo` / `ResolveInfo` objects through the Java bridge so the
//! framework's own `IPackageManager$Stub$Proxy` reads them.
//!
//! # Why materialise instead of hand-roll the wire format
//!
//! `ApplicationInfo.writeToParcel` writes ~70 fields, delegates to
//! `PackageItemInfo.writeToParcel`, writes a `SparseArray`, a `Parcelling`
//! boolean and a `TypedList`; `PackageInfo` adds six `Parcelable[]` arrays and
//! a `SigningInfo`. A hand-written byte codec is both huge and easy to get
//! subtly wrong (the existing prefix codec *did* assign `sourceDir` and
//! `minSdkVersion` to the wrong slots). The invariant in
//! `research/service-layer/REPORT.md` §3.6 is: *the only code that writes a
//! parcelable's bytes is that parcelable's own `writeToParcel`*. This module
//! follows it — it builds a real Java object, sets the typed fields, calls
//! `writeToParcel` into a real Java `Parcel`, then copies the marshalled bytes
//! into the transport reply.
//!
//! `acos-services` cannot see the VM, so `PmsService` stashes the typed
//! description it served ([`acos_services::pms::cached_description`]) and the
//! transport re-writes the reply here, synchronously, before it is handed back
//! to Java.

use acos_parcel::Parcel;
use acos_services::pms::{
    cached_description, ActivityInfo as PmsActivityInfo, AppInfo, PackageInfo,
};
use acos_vm::heap::{ArrayData, Handle, Object};
use acos_vm::javabridge::JavaValue;
use acos_vm::Vm;

/// True when this module owns the reply for `method`.
#[must_use]
pub fn owns_method(method: &str) -> bool {
    matches!(
        method,
        "getApplicationInfo"
            | "getPackageInfo"
            | "getActivityInfo"
            | "resolveIntent"
            | "queryIntentActivities"
    )
}

type JResult<T> = Result<T, String>;

/// The `pms_java` error prefix.
pub(crate) fn err(e: impl std::fmt::Display) -> String {
    format!("pms_java: {e}")
}

fn as_obj(v: JavaValue) -> JResult<Handle> {
    match v {
        JavaValue::Object(h) => Ok(h),
        JavaValue::Null => Err("pms_java: unexpected null object".into()),
        other => Err(format!("pms_java: expected object, got {}", other.kind())),
    }
}

fn set_str(vm: &mut Vm, obj: Handle, field: &str, value: &str) -> JResult<()> {
    let h = vm.new_string_rooted(value).map_err(err)?;
    vm.set_field(obj, field, JavaValue::Object(h)).map_err(err)
}

fn set_opt_str(vm: &mut Vm, obj: Handle, field: &str, value: &str) -> JResult<()> {
    if value.is_empty() {
        vm.set_field(obj, field, JavaValue::Null).map_err(err)
    } else {
        set_str(vm, obj, field, value)
    }
}

fn set_int(vm: &mut Vm, obj: Handle, field: &str, value: i32) -> JResult<()> {
    vm.set_field(obj, field, JavaValue::Int(value)).map_err(err)
}

/// Materialise a real `android.content.pm.ApplicationInfo`.
pub(crate) fn application_info(vm: &mut Vm, app: &AppInfo) -> JResult<Handle> {
    let h = vm
        .new_instance_ctor("android.content.pm.ApplicationInfo", "()V", &[])
        .map_err(err)?;
    set_str(vm, h, "packageName", &app.package_name)?;
    set_opt_str(vm, h, "name", &app.name)?;
    set_opt_str(vm, h, "className", &app.class_name)?;
    set_str(vm, h, "sourceDir", &app.source_dir)?;
    set_str(vm, h, "publicSourceDir", &app.public_source_dir)?;
    set_str(vm, h, "dataDir", &app.data_dir)?;
    set_str(vm, h, "nativeLibraryDir", &app.native_library_dir)?;
    set_str(vm, h, "processName", &app.process_name)?;
    set_int(vm, h, "uid", app.uid)?;
    set_int(vm, h, "flags", app.flags)?;
    set_int(vm, h, "privateFlags", app.private_flags)?;
    set_int(vm, h, "targetSdkVersion", app.target_sdk)?;
    set_int(vm, h, "minSdkVersion", app.min_sdk)?;
    set_int(vm, h, "theme", app.theme)?;
    set_int(vm, h, "labelRes", app.label_res)?;
    set_int(vm, h, "targetSandboxVersion", app.target_sandbox_version)?;
    Ok(h)
}

/// Materialise a real `android.content.pm.ActivityInfo`.
fn activity_info(vm: &mut Vm, app: &AppInfo, act: &PmsActivityInfo) -> JResult<Handle> {
    let h = vm
        .new_instance_ctor("android.content.pm.ActivityInfo", "()V", &[])
        .map_err(err)?;
    set_str(vm, h, "name", &act.name)?;
    set_str(vm, h, "packageName", &act.package_name)?;
    vm.set_field(h, "exported", JavaValue::Boolean(act.exported))
        .map_err(err)?;
    set_int(vm, h, "launchMode", act.launch_mode)?;
    set_int(vm, h, "flags", act.flags)?;
    set_int(vm, h, "theme", act.theme)?;
    set_opt_str(vm, h, "parentActivityName", &act.parent_activity_name)?;
    let app_h = application_info(vm, app)?;
    vm.set_field(h, "applicationInfo", JavaValue::Object(app_h))
        .map_err(err)?;
    Ok(h)
}

fn activity_array(vm: &mut Vm, app: &AppInfo, acts: &[PmsActivityInfo]) -> JResult<Handle> {
    let arr = vm
        .new_array_empty("android.content.pm.ActivityInfo", acts.len())
        .map_err(err)?;
    for (i, act) in acts.iter().enumerate() {
        let h = activity_info(vm, app, act)?;
        vm.set_array_element(arr, i, JavaValue::Object(h))
            .map_err(err)?;
    }
    Ok(arr)
}

fn string_array(vm: &mut Vm, values: &[String]) -> JResult<Handle> {
    let arr = vm.new_array_empty("java.lang.String", values.len()).map_err(err)?;
    for (i, s) in values.iter().enumerate() {
        let h = vm.new_string_rooted(s).map_err(err)?;
        vm.set_array_element(arr, i, JavaValue::Object(h))
            .map_err(err)?;
    }
    Ok(arr)
}

/// Materialise a real `android.content.pm.PackageInfo`.
fn package_info(vm: &mut Vm, app: &AppInfo, pkg: &PackageInfo) -> JResult<Handle> {
    let h = vm
        .new_instance_ctor("android.content.pm.PackageInfo", "()V", &[])
        .map_err(err)?;
    set_str(vm, h, "packageName", &pkg.package_name)?;
    set_int(vm, h, "versionCode", pkg.version_code)?;
    set_str(vm, h, "versionName", &pkg.version_name)?;
    let app_h = application_info(vm, app)?;
    vm.set_field(h, "applicationInfo", JavaValue::Object(app_h))
        .map_err(err)?;
    let acts = activity_array(vm, app, &pkg.activities)?;
    vm.set_field(h, "activities", JavaValue::Object(acts))
        .map_err(err)?;
    vm.set_field(h, "firstInstallTime", JavaValue::Long(pkg.first_install_time))
        .map_err(err)?;
    vm.set_field(h, "lastUpdateTime", JavaValue::Long(pkg.last_update_time))
        .map_err(err)?;
    let perms = string_array(vm, &pkg.requested_permissions)?;
    vm.set_field(h, "requestedPermissions", JavaValue::Object(perms))
        .map_err(err)?;
    Ok(h)
}

/// Write `obj.writeToParcel(parcel, 0)` into a fresh Java `Parcel` and return
/// the marshalled payload bytes.
fn write_object_payload(vm: &mut Vm, obj: Handle) -> JResult<Vec<u8>> {
    let parcel = as_obj(
        vm.call_static("android.os.Parcel", "obtain", "()Landroid/os/Parcel;", &[])
            .map_err(err)?,
    )?;
    vm.call_method(
        obj,
        "writeToParcel",
        "(Landroid/os/Parcel;I)V",
        &[JavaValue::Object(parcel), JavaValue::Int(0)],
    )
    .map_err(err)?;
    let bytes_h = as_obj(
        vm.call_method(parcel, "marshall", "()[B", &[])
            .map_err(err)?,
    )?;
    let bytes = byte_array(vm, bytes_h).ok_or_else(|| "pms_java: marshall did not return [B".to_string())?;
    let _ = vm.call_method(parcel, "recycle", "()V", &[]);
    Ok(bytes)
}

fn byte_array(vm: &Vm, h: Handle) -> Option<Vec<u8>> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Byte(v) => Some(v.iter().map(|&b| b as u8).collect()),
            _ => None,
        },
        _ => None,
    }
}

fn append(reply: &mut Parcel, bytes: &[u8]) -> JResult<()> {
    let mut tmp = Parcel::new();
    tmp.unmarshall(bytes);
    let len = tmp.data_size();
    reply
        .append_from(&tmp, 0, len)
        .map_err(err)
}

/// Write a materialised typed object (presence marker + payload) or null.
fn typed_object(vm: &mut Vm, obj: Option<Handle>, reply: &mut Parcel) -> JResult<()> {
    match obj {
        None => {
            reply.write_int32(0);
            Ok(())
        }
        Some(h) => {
            reply.write_int32(1);
            let bytes = write_object_payload(vm, h)?;
            append(reply, &bytes)
        }
    }
}

/// Rewrite the reply for an `IPackageManager` method that returns a real Java
/// parcelable. The caller has already dispatched the handler, so the typed
/// description is in the cache.
///
/// Returns `Ok(false)` when this module does not own the method (the generic
/// reply is left untouched).
pub fn rewrite_reply(vm: &mut Vm, method: &str, reply: &mut Parcel) -> JResult<bool> {
    if !owns_method(method) {
        return Ok(false);
    }
    let Some(desc) = cached_description() else {
        return Ok(false);
    };
    // Reset the generic (prefix) reply; the Java object writes its own bytes.
    reply.reset();
    acos_services::exception::write_no_exception(reply);

    match method {
        "getApplicationInfo" => {
            let h = application_info(vm, &desc.app)?;
            typed_object(vm, Some(h), reply)?;
        }
        "getPackageInfo" => {
            let h = package_info(vm, &desc.app, &desc.package)?;
            typed_object(vm, Some(h), reply)?;
        }
        "getActivityInfo" => {
            // The handler resolved a `ComponentName` and cached the matching
            // activity (or `None`).
            match acos_services::pms::cached_activity_info() {
                Some(a) => {
                    let h = activity_info(vm, &desc.app, &a)?;
                    typed_object(vm, Some(h), reply)?;
                }
                None => typed_object(vm, None, reply)?,
            }
        }
        "resolveIntent" => {
            // Only the launcher intent is installed; anything else is a
            // genuine "no match".
            let h = launcher_resolve_info(vm, &desc.app, &desc.package)?;
            typed_object(vm, h, reply)?;
        }
        "queryIntentActivities" => {
            let h = launcher_list_slice(vm, &desc.app, &desc.package)?;
            typed_object(vm, h, reply)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// Materialise the launcher `ResolveInfo`, or `None` when the package has no
/// launcher activity.
fn launcher_resolve_info(
    vm: &mut Vm,
    app: &AppInfo,
    pkg: &PackageInfo,
) -> JResult<Option<Handle>> {
    let Some(act) = pkg
        .activities
        .iter()
        .find(|a| a.name.ends_with(".MainActivity"))
    else {
        return Ok(None);
    };
    let h = vm
        .new_instance_ctor("android.content.pm.ResolveInfo", "()V", &[])
        .map_err(err)?;
    let act_h = activity_info(vm, app, act)?;
    vm.set_field(h, "activityInfo", JavaValue::Object(act_h))
        .map_err(err)?;
    // `ResolveInfo.writeToParcel` writes `filter` unconditionally, so give it
    // the real MAIN/LAUNCHER filter.
    let filter = launcher_intent_filter(vm)?;
    vm.set_field(h, "filter", JavaValue::Object(filter))
        .map_err(err)?;
    set_int(vm, h, "priority", 0)?;
    set_int(vm, h, "preferredOrder", 0)?;
    set_int(vm, h, "match", 0x0010_0000)?; // IntentFilter.MATCH_CATEGORY_EMPTY
    set_int(vm, h, "specificIndex", -1)?;
    vm.set_field(h, "isDefault", JavaValue::Boolean(false))
        .map_err(err)?;
    Ok(Some(h))
}

fn launcher_intent_filter(vm: &mut Vm) -> JResult<Handle> {
    let filter = vm
        .new_instance_ctor("android.content.IntentFilter", "()V", &[])
        .map_err(err)?;
    let actions = vm
        .new_instance_ctor("java.util.ArrayList", "()V", &[])
        .map_err(err)?;
    let main = vm
        .new_string_rooted("android.intent.action.MAIN")
        .map_err(err)?;
    vm.call_method(actions, "add", "(Ljava/lang/Object;)Z", &[JavaValue::Object(main)])
        .map_err(err)?;
    vm.set_field(filter, "mActions", JavaValue::Object(actions))
        .map_err(err)?;
    // Categories are stored as a bitmask plus a list with the special
    // categories; MAIN/LAUNCHER is category 0x02000000 (`CATEGORY_LAUNCHER`).
    let categories = vm
        .new_instance_ctor("java.util.ArrayList", "()V", &[])
        .map_err(err)?;
    vm.set_field(filter, "mCategories", JavaValue::Object(categories))
        .map_err(err)?;
    set_int(vm, filter, "mPriority", 0)?;
    Ok(filter)
}

/// Materialise a `ParceledListSlice<ResolveInfo>` with the single launcher entry.
fn launcher_list_slice(vm: &mut Vm, app: &AppInfo, pkg: &PackageInfo) -> JResult<Option<Handle>> {
    let Some(resolve) = launcher_resolve_info(vm, app, pkg)? else {
        return Ok(None);
    };
    let list = vm
        .new_instance_ctor("java.util.ArrayList", "()V", &[])
        .map_err(err)?;
    vm.call_method(
        list,
        "add",
        "(Ljava/lang/Object;)Z",
        &[JavaValue::Object(resolve)],
    )
    .map_err(err)?;
    let slice = vm
        .new_instance_ctor(
            "android.content.pm.ParceledListSlice",
            "(Ljava/util/List;)V",
            &[JavaValue::Object(list)],
        )
        .map_err(err)?;
    Ok(Some(slice))
}
