//! Generated AIDL tables and the small constructors they use.
//!
//! `tables.rs` is produced by `cargo run -p acos-aidl --example gen` and is
//! checked in. Regenerating it must be byte-identical (see the determinism
//! test).

pub mod tables;

use crate::ir::{InterfaceInfo, MethodInfo, WireOp};

/// Construct an [`InterfaceInfo`]. Used only by generated code.
#[doc(hidden)]
pub fn iface(
    descriptor: &str,
    first_call_transaction: u32,
    methods: Vec<MethodInfo>,
) -> InterfaceInfo {
    InterfaceInfo {
        descriptor: descriptor.to_string(),
        first_call_transaction,
        methods,
    }
}

/// Construct a [`MethodInfo`]. Used only by generated code.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn method(
    name: &str,
    signature: &str,
    return_type: &str,
    code: u32,
    oneway: bool,
    args: Vec<WireOp>,
    reply: Vec<WireOp>,
) -> MethodInfo {
    MethodInfo {
        name: name.to_string(),
        signature: signature.to_string(),
        return_type: return_type.to_string(),
        code,
        oneway,
        args,
        reply,
        incomplete: None,
    }
}

/// `WireOp::Binder`.
#[doc(hidden)]
pub fn binder(iface: Option<&str>) -> WireOp {
    WireOp::Binder {
        iface: iface.map(str::to_string),
    }
}

/// `WireOp::TypedObject` with the intrinsic presence marker.
#[doc(hidden)]
pub fn typed_object(class: &str) -> WireOp {
    WireOp::TypedObject {
        class: class.to_string(),
        nullable: true,
    }
}

/// `WireOp::TypedObject` without a nullable marker (not seen in AIDL output).
#[doc(hidden)]
pub fn typed_object_nn(class: &str) -> WireOp {
    WireOp::TypedObject {
        class: class.to_string(),
        nullable: false,
    }
}

/// `WireOp::Parcelable`.
#[doc(hidden)]
pub fn parcelable(class: &str) -> WireOp {
    WireOp::Parcelable {
        class: class.to_string(),
    }
}

/// `WireOp::TypedArray`.
#[doc(hidden)]
pub fn typed_array(class: &str) -> WireOp {
    WireOp::TypedArray {
        class: class.to_string(),
    }
}

/// `WireOp::TypedList`.
#[doc(hidden)]
pub fn typed_list(class: &str) -> WireOp {
    WireOp::TypedList {
        class: class.to_string(),
    }
}

/// `WireOp::List`.
#[doc(hidden)]
pub fn list_op(elem: WireOp) -> WireOp {
    WireOp::List {
        elem: Box::new(elem),
    }
}

/// `WireOp::Array`.
#[doc(hidden)]
pub fn array_op(elem: WireOp) -> WireOp {
    WireOp::Array {
        elem: Box::new(elem),
    }
}

/// `WireOp::Map`.
#[doc(hidden)]
pub fn map_op(key: WireOp, value: WireOp) -> WireOp {
    WireOp::Map {
        key: Box::new(key),
        value: Box::new(value),
    }
}

/// `WireOp::Cond`.
#[doc(hidden)]
pub fn cond(marker: WireOp, ops: Vec<WireOp>) -> WireOp {
    WireOp::Cond {
        marker: Box::new(marker),
        ops,
    }
}

/// `WireOp::Value`.
#[doc(hidden)]
pub fn value_op() -> WireOp {
    WireOp::Value
}

/// `WireOp::Unknown`.
#[doc(hidden)]
pub fn unknown(note: &str) -> WireOp {
    WireOp::Unknown {
        note: note.to_string(),
    }
}
