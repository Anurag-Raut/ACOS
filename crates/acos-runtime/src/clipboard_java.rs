//! The real `ClipData`/`ClipDescription` marshalling for `IClipboard`.
//!
//! `setPrimaryClip` carries a real `ClipData` the generic `Value` codec cannot
//! read, and `getPrimaryClip`/`getPrimaryClipDescription` return real framework
//! parcelables. This module bridges both directions with the class's own
//! `CREATOR`/`writeToParcel` so the caller's `ClipData.CREATOR` reads exactly
//! what `writeToParcel` wrote (the invariant in
//! `research/service-layer/REPORT.md` §3.6):
//!
//! * `setPrimaryClip`/`setPrimaryClipAsPackage` — read the incoming `ClipData`
//!   through `parcel_java::read_typed_object`, store the bytes its own
//!   `writeToParcel` emits plus the semantic facts read from the real object
//!   ([`ClipFacts`]), and deliver `IOnPrimaryClipChangedListener`
//!   `onPrimaryClipChanged()` oneway to every registered app listener.
//! * `getPrimaryClip` — re-emit the stored `writeToParcel` bytes verbatim.
//! * `getPrimaryClipDescription` — materialise a real `ClipDescription` from the
//!   stored facts and let it write its own payload.
//! * `clearPrimaryClip` — clear and notify the listeners.
//!
//! The scalar clipboard transactions (`hasPrimaryClip`, `hasClipboardText`,
//! `getPrimaryClipSource`, `add/removePrimaryClipChangedListener`, …) stay on
//! the generic codec in `acos_services::clipboard`; both paths share the same
//! process-wide store ([`acos_services::clipboard::shared`]).

use acos_parcel::{binder::BinderId, Parcel};
use acos_services::clipboard::{shared, ClipFacts};
use acos_vm::heap::{Handle, NULL};
use acos_vm::javabridge::JavaValue;
use acos_vm::natives::binder::BinderDelivery;
use acos_vm::Vm;

type JResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("clipboard_java: {e}")
}

/// True when `descriptor` is routed to this module.
#[must_use]
pub fn owns(descriptor: Option<&str>) -> bool {
    descriptor == Some("android.content.IClipboard")
}

/// True when this module must marshal the request and/or the reply for
/// `method`.
#[must_use]
pub fn owns_method(method: &str) -> bool {
    matches!(
        method,
        "setPrimaryClip"
            | "setPrimaryClipAsPackage"
            | "getPrimaryClip"
            | "getPrimaryClipDescription"
            | "clearPrimaryClip"
    )
}

/// Handle one `IClipboard` transaction that carries or returns a real
/// `ClipData`/`ClipDescription`.
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
    if !owns_method(&method.name) {
        return Err(err(format!("method {} is not owned here", method.name)));
    }
    data.enforce_interface(&descriptor).map_err(err)?;

    reply.reset();
    acos_services::exception::write_no_exception(reply);

    match method.name.as_str() {
        "setPrimaryClip" | "setPrimaryClipAsPackage" => {
            let clip = crate::parcel_java::read_typed_object(vm, data, "android.content.ClipData")?;
            let package = read_string16(data)?;
            let _attribution = read_string16(data)?;
            let _user = data.read_int32().map_err(err)?;
            let _device = data.read_int32().map_err(err)?;
            let source = if method.name == "setPrimaryClipAsPackage" {
                let calling = read_string16(data)?;
                let _ = package;
                calling
            } else {
                package
            };
            let clip = clip.ok_or_else(|| err("setPrimaryClip with a null ClipData"))?;
            let facts = clip_facts(vm, clip)?;
            let bytes = crate::pms_java::write_object_payload(vm, clip)?;
            let clipboard = shared();
            let mut state = clipboard.lock().unwrap();
            state.set_primary_clip(bytes, facts, source);
            let listeners = state.listeners.clone();
            drop(state);
            Ok(listener_deliveries(&listeners)?)
        }
        "clearPrimaryClip" => {
            let _package = read_string16(data)?;
            let _attribution = read_string16(data)?;
            let _user = data.read_int32().map_err(err)?;
            let _device = data.read_int32().map_err(err)?;
            let clipboard = shared();
            let mut state = clipboard.lock().unwrap();
            state.clear();
            let listeners = state.listeners.clone();
            drop(state);
            Ok(listener_deliveries(&listeners)?)
        }
        "getPrimaryClip" => {
            read_clip_args(data)?;
            let entry = shared().lock().unwrap().get_primary_clip().cloned();
            match entry {
                Some(entry) => {
                    reply.write_int32(1);
                    append_bytes(reply, &entry.bytes)?;
                }
                None => reply.write_int32(0),
            }
            Ok(Vec::new())
        }
        "getPrimaryClipDescription" => {
            read_clip_args(data)?;
            let facts = shared().lock().unwrap().description().cloned();
            match facts {
                Some(facts) => {
                    let desc = clip_description(vm, &facts)?;
                    crate::pms_java::typed_object(vm, Some(desc), reply)?;
                }
                None => crate::pms_java::typed_object(vm, None, reply)?,
            }
            Ok(Vec::new())
        }
        other => Err(err(format!("unhandled method {other}"))),
    }
}

fn read_clip_args(data: &mut Parcel) -> JResult<()> {
    let _package = read_string16(data)?;
    let _attribution = read_string16(data)?;
    let _user = data.read_int32().map_err(err)?;
    let _device = data.read_int32().map_err(err)?;
    Ok(())
}

fn read_string16(data: &mut Parcel) -> JResult<String> {
    Ok(data.read_string16().map_err(err)?.unwrap_or_default())
}

/// The outgoing `IOnPrimaryClipChangedListener.onPrimaryClipChanged()` oneway
/// deliveries for every registered listener.
fn listener_deliveries(listeners: &[BinderId]) -> JResult<Vec<BinderDelivery>> {
    let mut out = Vec::with_capacity(listeners.len());
    for listener in listeners {
        let mut data = Parcel::new();
        data.write_interface_token("android.content.IOnPrimaryClipChangedListener")
            .map_err(err)?;
        out.push(BinderDelivery {
            target: listener.0,
            // `IOnPrimaryClipChangedListener.onPrimaryClipChanged()` is the
            // first (and only) AIDL transaction; the method is `oneway`.
            code: 1,
            flags: 1,
            data,
        });
    }
    Ok(out)
}

/// Append the stored `writeToParcel` payload bytes to `reply`.
fn append_bytes(reply: &mut Parcel, bytes: &[u8]) -> JResult<()> {
    let mut tmp = Parcel::new();
    tmp.unmarshall(bytes);
    let len = tmp.data_size();
    reply.append_from(&tmp, 0, len).map_err(err)
}

/// Read the semantic facts out of the real `ClipData` object.
fn clip_facts(vm: &mut Vm, clip: Handle) -> JResult<ClipFacts> {
    let description = object_field(
        vm,
        clip,
        "getDescription",
        "()Landroid/content/ClipDescription;",
        &[],
    )?;
    let mut facts = ClipFacts::default();
    if description != NULL {
        let label = object_field(
            vm,
            description,
            "getLabel",
            "()Ljava/lang/CharSequence;",
            &[],
        )?;
        facts.label = crate::parcel_java::char_sequence_to_string(vm, label)?;
        let count = int_call(vm, description, "getMimeTypeCount", "()I")?;
        for i in 0..count.max(0) {
            let mime = object_field(
                vm,
                description,
                "getMimeType",
                "(I)Ljava/lang/String;",
                &[JavaValue::Int(i)],
            )?;
            if mime != NULL {
                facts
                    .mime_types
                    .push(vm.java_string_to_rust(mime).map_err(err)?);
            }
        }
    }
    facts.item_count = int_call(vm, clip, "getItemCount", "()I")?;
    if facts.item_count > 0 {
        let item = object_field(
            vm,
            clip,
            "getItemAt",
            "(I)Landroid/content/ClipData$Item;",
            &[JavaValue::Int(0)],
        )?;
        if item != NULL {
            facts.has_text =
                object_field(vm, item, "getText", "()Ljava/lang/CharSequence;", &[])? != NULL;
            facts.has_uri = object_field(vm, item, "getUri", "()Landroid/net/Uri;", &[])? != NULL;
            facts.has_intent =
                object_field(vm, item, "getIntent", "()Landroid/content/Intent;", &[])? != NULL;
        }
    }
    Ok(facts)
}

/// Materialise a real `android.content.ClipDescription` from the stored facts.
fn clip_description(vm: &mut Vm, facts: &ClipFacts) -> JResult<Handle> {
    let label = vm.new_string_rooted(&facts.label).map_err(err)?;
    let mime_types = if facts.mime_types.is_empty() {
        vec!["text/plain".to_string()]
    } else {
        facts.mime_types.clone()
    };
    let arr = vm
        .new_array_empty("Ljava/lang/String;", mime_types.len())
        .map_err(err)?;
    for (i, mime) in mime_types.iter().enumerate() {
        let s = vm.new_string_rooted(mime).map_err(err)?;
        vm.set_array_element(arr, i, JavaValue::Object(s))
            .map_err(err)?;
    }
    vm.new_instance_ctor(
        "android.content.ClipDescription",
        "(Ljava/lang/CharSequence;[Ljava/lang/String;)V",
        &[JavaValue::Object(label), JavaValue::Object(arr)],
    )
    .map_err(err)
}

fn object_field(
    vm: &mut Vm,
    obj: Handle,
    method: &str,
    sig: &str,
    args: &[JavaValue],
) -> JResult<Handle> {
    match vm.call_method(obj, method, sig, args).map_err(err)? {
        JavaValue::Object(h) => Ok(h),
        JavaValue::Null => Ok(NULL),
        other => Err(err(format!("{method} returned {}", other.kind()))),
    }
}

fn int_call(vm: &mut Vm, obj: Handle, method: &str, sig: &str) -> JResult<i32> {
    match vm.call_method(obj, method, sig, &[]).map_err(err)? {
        JavaValue::Int(v) => Ok(v),
        other => Err(err(format!("{method} returned {}", other.kind()))),
    }
}
