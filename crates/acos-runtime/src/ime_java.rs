//! Materialise the real framework `InputBindResult` for the null in-process
//! IME (`IInputMethodManager.startInputOrWindowGainedFocus`).
//!
//! # Why a Java marshaller and not a table
//!
//! `InputBindResult.writeToParcel` writes eight fields including a
//! `writeStrongInterface` binder, a `SparseArray` length, a nullable
//! `InputChannel`, a `float[]` and a boolean (see
//! `results/wm/parcelable_layouts.txt`). The generic `Value` codec has no spec
//! for it and deliberately refuses to guess. This module builds a **real**
//! `com.android.internal.inputmethod.InputBindResult`, sets the field values the
//! ACOS null IME serves, and lets its own `writeToParcel` emit the reply bytes —
//! so the framework's `InputBindResult.CREATOR` reads exactly what
//! `writeToParcel` wrote.
//!
//! The request carries a real `EditorInfo` (and possibly an
//! `ImeOnBackInvokedDispatcher`), which the `Value` codec also cannot read; the
//! arguments are decoded here with the class's own `CREATOR`
//! ([`crate::parcel_java::read_typed_object`]) and the null IME state machine
//! (`acos_services::ime`) is driven directly.
//!
//! # The null IME result
//!
//! ACOS V1 runs no `InputMethodService`: `startInputOrWindowGainedFocus` always
//! records `NO_IME` and returns a real `InputBindResult` with `result = 1`, a
//! `null` `IInputMethodSession`, no channel and the sequence the request came
//! with. This is the **declared divergence** documented in
//! `results/m2/SERVICES_COVERAGE.md`; typed text arrives as injected
//! `KeyEvent`s, never through an `InputConnection`.

use acos_aidl::WireOp;
use acos_parcel::{binder::BinderId, Parcel};
use acos_services::ime::shared;
use acos_vm::heap::Handle;
use acos_vm::javabridge::JavaValue;
use acos_vm::natives::binder::BinderDelivery;
use acos_vm::Vm;

type JResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("ime_java: {e}")
}

/// True when `descriptor` is routed to this module.
#[must_use]
pub fn owns(descriptor: Option<&str>) -> bool {
    descriptor == Some("com.android.internal.view.IInputMethodManager")
}

/// True when this module must marshal the reply for `method`.
///
/// Only `startInputOrWindowGainedFocus` returns an `InputBindResult`; every
/// other `IInputMethodManager` transaction is served by the generic codec.
#[must_use]
pub fn owns_method(method: &str) -> bool {
    method == "startInputOrWindowGainedFocus"
}

/// Handle one `IInputMethodManager` transaction whose reply is a real Java
/// `InputBindResult`.
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

    let mut client: Option<BinderId> = None;
    let mut remote: Option<BinderId> = None;
    let mut reason: i32 = 0;
    for (index, op) in method.args.iter().enumerate() {
        match op {
            WireOp::I32 => {
                let value = data.read_int32().map_err(err)?;
                if index == 0 {
                    reason = value;
                }
            }
            WireOp::Binder { .. } => {
                let value = data.read_strong_binder().map_err(err)?;
                match index {
                    1 => client = value,
                    7 => remote = value,
                    _ => {}
                }
            }
            WireOp::TypedObject { class, .. } => {
                let _ = crate::parcel_java::read_typed_object(vm, data, class)?;
            }
            other => {
                return Err(err(format!(
                    "{}: undecoded argument {}",
                    method.name,
                    other.render()
                )))
            }
        }
    }

    // Drive the shared null-IME state machine. This is the same `ImeState` the
    // host's `ImeService` records into, so the Java and VM-free paths agree.
    // The recorded result is the ACOS `NO_IME` marker; the wire value below is
    // the framework's own `InputBindResult.NO_IME.result`.
    let (_result, _method, _session) = shared().lock().unwrap().start_input(client, reason, remote);

    reply.reset();
    acos_services::exception::write_no_exception(reply);
    let bind = input_bind_result(vm, reason)?;
    crate::pms_java::typed_object(vm, Some(bind), reply)?;
    Ok(Vec::new())
}

/// The framework's own `InputBindResult.NO_IME.result`.
///
/// API 34's constant is a static `InputBindResult` object built by
/// `InputBindResult.error(int)`; reading its `result` field is the only way to
/// get the code the framework itself uses (it is **not** `1`).
fn no_ime_result(vm: &mut Vm) -> JResult<i32> {
    let class = "com.android.internal.inputmethod.InputBindResult";
    let cid = vm
        .load_class(&acos_vm::javabridge::class_descriptor(class))
        .map_err(err)?;
    vm.initialize_class(cid).map_err(err)?;
    let no_ime = match vm.get_static_field(class, "NO_IME").map_err(err)? {
        JavaValue::Object(h) => h,
        other => return Err(err(format!("NO_IME is {}", other.kind()))),
    };
    match vm.get_field(no_ime, "result").map_err(err)? {
        JavaValue::Int(v) => Ok(v),
        other => Err(err(format!("NO_IME.result is {}", other.kind()))),
    }
}

/// Materialise a real `InputBindResult` with the null-IME field values.
///
/// The object is allocated without running a constructor (the API-34 ctor is
/// package-private); every field `writeToParcel` reads is set explicitly, then
/// the real `writeToParcel` emits the payload.
fn input_bind_result(vm: &mut Vm, sequence: i32) -> JResult<Handle> {
    let result = no_ime_result(vm)?;
    let h = vm
        .new_instance("com.android.internal.inputmethod.InputBindResult")
        .map_err(err)?;
    vm.set_field(h, "result", JavaValue::Int(result))
        .map_err(err)?;
    vm.set_field(h, "method", JavaValue::Null).map_err(err)?;
    vm.set_field(h, "accessibilitySessions", JavaValue::Null)
        .map_err(err)?;
    vm.set_field(h, "channel", JavaValue::Null).map_err(err)?;
    vm.set_field(h, "id", JavaValue::Null).map_err(err)?;
    vm.set_field(h, "sequence", JavaValue::Int(sequence))
        .map_err(err)?;
    vm.set_field(h, "mVirtualDisplayToScreenMatrixValues", JavaValue::Null)
        .map_err(err)?;
    vm.set_field(
        h,
        "isInputMethodSuppressingSpellChecker",
        JavaValue::Boolean(false),
    )
    .map_err(err)?;
    Ok(h)
}
