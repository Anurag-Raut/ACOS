//! Materialise the real framework `android.content.pm.UserInfo` for the
//! `IUserManager` replies that return it.
//!
//! ACOS serves exactly one user (`id = 0`, name `ACOS`, `UserHandle.USER_SYSTEM`)
//! — the state documented in `acos_services::declared::UserState`. `UserInfo`
//! is a real framework parcelable with a bytecode-only `writeToParcel` layout
//! (`id`, `name`, flags, icon path, optional user type, optional profile
//! parent), so this module builds the real object with its public
//! `(int, String, int)` constructor and lets its own `writeToParcel` emit the
//! reply.
//!
//! `getUsers`/`getProfiles` return `List<UserInfo>`: the reply is the AIDL
//! `writeTypedList` shape (`int` count then one `writeTypedObject` per entry).

use acos_parcel::{binder::BinderId, Parcel};
use acos_services::declared::{USER_SYSTEM};
use acos_vm::heap::Handle;
use acos_vm::javabridge::JavaValue;
use acos_vm::natives::binder::BinderDelivery;
use acos_vm::Vm;

type JResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("user_java: {e}")
}

/// True when `descriptor` is routed to this module.
#[must_use]
pub fn owns(descriptor: Option<&str>) -> bool {
    descriptor == Some("android.os.IUserManager")
}

/// True when this module must marshal the reply for `method`.
#[must_use]
pub fn owns_method(method: &str) -> bool {
    matches!(
        method,
        "getUserInfo" | "getPrimaryUser" | "getProfileParent" | "getUsers" | "getProfiles"
    )
}

/// Handle one `IUserManager` transaction whose reply is a real `UserInfo`.
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
        "getUserInfo" => {
            let user = data.read_int32().map_err(err)?;
            if user == USER_SYSTEM {
                let info = user_info(vm)?;
                crate::pms_java::typed_object(vm, Some(info), reply)?;
            } else {
                crate::pms_java::typed_object(vm, None, reply)?;
            }
        }
        "getPrimaryUser" | "getProfileParent" => {
            let info = user_info(vm)?;
            crate::pms_java::typed_object(vm, Some(info), reply)?;
        }
        "getUsers" => {
            let _ = data.read_bool().map_err(err)?;
            let _ = data.read_bool().map_err(err)?;
            let _ = data.read_bool().map_err(err)?;
            let info = user_info(vm)?;
            reply.write_int32(1);
            crate::pms_java::typed_object(vm, Some(info), reply)?;
        }
        "getProfiles" => {
            let _user = data.read_int32().map_err(err)?;
            let _ = data.read_bool().map_err(err)?;
            let info = user_info(vm)?;
            reply.write_int32(1);
            crate::pms_java::typed_object(vm, Some(info), reply)?;
        }
        other => return Err(err(format!("unhandled method {other}"))),
    }
    Ok(Vec::new())
}

/// Materialise the single ACOS `UserInfo` through its real 3-arg constructor.
fn user_info(vm: &mut Vm) -> JResult<Handle> {
    let name = vm.new_string_rooted("ACOS").map_err(err)?;
    vm.new_instance_ctor(
        "android.content.pm.UserInfo",
        "(ILjava/lang/String;I)V",
        &[
            JavaValue::Int(USER_SYSTEM),
            JavaValue::Object(name),
            JavaValue::Int(0),
        ],
    )
    .map_err(err)
}
