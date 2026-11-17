//! Read a real framework parcelable out of an incoming ACOS request `Parcel`.
//!
//! The ACOS service codec is `Value`-based: for a `writeToParcel` argument it
//! needs a [`acos_services::parcelable::spec_for`] table. For the IME/clipboard
//! requests (`EditorInfo`, `ImeOnBackInvokedDispatcher`, `ClipData`) no table
//! exists, and hand-writing one would be a second implementation of a format
//! only the framework's own `writeToParcel` may define (the invariant in
//! `research/service-layer/REPORT.md` §3.6).
//!
//! This module goes the other way: it copies the remaining request bytes into a
//! **real Java `Parcel`**, calls the class's own static `CREATOR`, and then
//! advances the ACOS `Parcel` by exactly the bytes the real reader consumed.
//! The object handed back is a genuine framework instance whose own
//! `writeToParcel` (see [`crate::pms_java::write_object_payload`]) emits the
//! bytes for the stored/re-emitted reply.

use acos_parcel::Parcel;
use acos_vm::heap::Handle;
use acos_vm::javabridge::{class_descriptor, JavaValue};
use acos_vm::Vm;

type JResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("parcel_java: {e}")
}

/// Create a real Java `Parcel` holding `bytes`, positioned at `0`.
pub fn java_parcel_from_bytes(vm: &mut Vm, bytes: &[u8]) -> JResult<Handle> {
    let parcel = as_obj(
        vm.call_static("android.os.Parcel", "obtain", "()Landroid/os/Parcel;", &[])
            .map_err(err)?,
    )?;
    if bytes.is_empty() {
        return Ok(parcel);
    }
    let arr = vm.new_array_empty("B", bytes.len()).map_err(err)?;
    for (i, b) in bytes.iter().enumerate() {
        vm.set_array_element(arr, i, JavaValue::Byte(*b as i8))
            .map_err(err)?;
    }
    vm.call_method(
        parcel,
        "unmarshall",
        "([BII)V",
        &[
            JavaValue::Object(arr),
            JavaValue::Int(0),
            JavaValue::Int(i32::try_from(bytes.len()).map_err(err)?),
        ],
    )
    .map_err(err)?;
    vm.call_method(parcel, "setDataPosition", "(I)V", &[JavaValue::Int(0)])
        .map_err(err)?;
    Ok(parcel)
}

/// Read `class.CREATOR.createFromParcel(parcel)` for a payload `parcel`.
pub fn create_from_parcel(vm: &mut Vm, class: &str, parcel: Handle) -> JResult<Handle> {
    let creator = creator_of(vm, class)?;
    match vm
        .call_method(
            creator,
            "createFromParcel",
            "(Landroid/os/Parcel;)Ljava/lang/Object;",
            &[JavaValue::Object(parcel)],
        )
        .map_err(err)?
    {
        JavaValue::Object(h) => Ok(h),
        JavaValue::Null => Ok(acos_vm::heap::NULL),
        other => Err(format!(
            "parcel_java: {class}.CREATOR.createFromParcel returned {}",
            other.kind()
        )),
    }
}

/// The static `CREATOR` of a framework parcelable, after running its `<clinit>`.
pub fn creator_of(vm: &mut Vm, class: &str) -> JResult<Handle> {
    let desc = class_descriptor(class);
    let cid = vm.load_class(&desc).map_err(err)?;
    vm.initialize_class(cid).map_err(err)?;
    match vm.get_static_field(class, "CREATOR").map_err(err)? {
        JavaValue::Object(h) => Ok(h),
        other => Err(format!("parcel_java: {class}.CREATOR is {}", other.kind())),
    }
}

/// Read one AIDL `writeTypedObject` argument (`int` presence then payload) and
/// advance `data` past it. `None` means the `null` marker was read.
pub fn read_typed_object(vm: &mut Vm, data: &mut Parcel, class: &str) -> JResult<Option<Handle>> {
    let present = data.read_int32().map_err(err)?;
    if present == 0 {
        return Ok(None);
    }
    let start = data.data_position();
    let all = data.marshall();
    if start > all.len() {
        return Err(format!(
            "parcel_java: position {start} past data size {}",
            all.len()
        ));
    }
    let payload = &all[start..];
    let parcel = java_parcel_from_bytes(vm, payload)?;
    let obj = create_from_parcel(vm, class, parcel)?;
    let consumed = match vm
        .call_method(parcel, "dataPosition", "()I", &[])
        .map_err(err)?
    {
        JavaValue::Int(v) if v >= 0 => v as usize,
        other => {
            return Err(format!(
                "parcel_java: {class} reader left dataPosition={}",
                other.kind()
            ))
        }
    };
    let _ = vm.call_method(parcel, "recycle", "()V", &[]);
    data.set_data_position(start + consumed);
    if obj == acos_vm::heap::NULL {
        return Ok(None);
    }
    Ok(Some(obj))
}

/// Convert a Java `CharSequence` (possibly null) to a Rust string.
pub fn char_sequence_to_string(vm: &mut Vm, h: Handle) -> JResult<String> {
    if h == acos_vm::heap::NULL {
        return Ok(String::new());
    }
    match vm
        .call_method(h, "toString", "()Ljava/lang/String;", &[])
        .map_err(err)?
    {
        JavaValue::Object(s) => vm.java_string_to_rust(s).map_err(err),
        JavaValue::Null => Ok(String::new()),
        other => Err(format!(
            "parcel_java: CharSequence.toString returned {}",
            other.kind()
        )),
    }
}

fn as_obj(v: JavaValue) -> JResult<Handle> {
    match v {
        JavaValue::Object(h) => Ok(h),
        JavaValue::Null => Err("parcel_java: unexpected null object".into()),
        other => Err(format!(
            "parcel_java: expected object, got {}",
            other.kind()
        )),
    }
}
