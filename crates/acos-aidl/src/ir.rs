//! Typed wire-layout IR for AIDL transactions.
//!
//! The IR mirrors the sequence of `Parcel` operations a generated
//! `Foo$Stub$Proxy` performs: writes into the request `Parcel` before
//! `IBinder.transact`, and reads from the reply `Parcel` after
//! `Parcel.readException`. It is deliberately small and `serde`-serialisable so
//! the generator can emit both Rust static tables and JSON dumps.

use serde::{Deserialize, Serialize};

/// One step of a Parcel wire layout.
///
/// The variants correspond one-to-one with `Parcel.write*` / `Parcel.read*`
/// families (or the AIDL-level primitive they encode). `Unknown` is the
/// explicit "the scanner could not decode this" case: it always carries the
/// method and program counter that produced it and is never guessed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WireOp {
    /// `writeInt` / `readInt` (`int`).
    I32,
    /// `writeLong` / `readLong` (`long`).
    I64,
    /// `writeFloat` / `readFloat` (`float`).
    F32,
    /// `writeDouble` / `readDouble` (`double`).
    F64,
    /// `writeBoolean` / `readBoolean` (`boolean`).
    Bool,
    /// `writeByte` / `readByte` (`byte`).
    Byte,
    /// `writeChar` / `readChar` (`char`).
    Char,
    /// `writeString` / `readString` (UTF-16 `String`).
    Str16,
    /// `writeString8` / `readString8` (UTF-8 `String`).
    Str8,
    /// A strong binder reference. `iface` is the AIDL interface descriptor (with
    /// surrounding `L...;`) when the static type is known, e.g.
    /// `Landroid/view/IWindow;`.
    Binder {
        #[serde(skip_serializing_if = "Option::is_none")]
        iface: Option<String>,
    },
    ByteArray,
    IntArray,
    LongArray,
    FloatArray,
    DoubleArray,
    BooleanArray,
    CharArray,
    StringArray,
    /// `writeStringList` / `createStringArrayList`.
    StringList,
    /// `writeBinderList` / `createBinderArrayList`.
    BinderList,
    /// `writeRawFileDescriptor` / `readRawFileDescriptor` (a bare fd).
    RawFd,
    /// `writeTypedArray` / `createTypedArray` (element class from the CREATOR).
    TypedArray {
        class: String,
    },
    /// `writeTypedList` / `createTypedArrayList`.
    TypedList {
        class: String,
    },
    /// `writeParcelable` / `readParcelable` with an explicit class loader.
    Parcelable {
        class: String,
    },
    /// `writeTypedObject` / `readTypedObject`. `nullable` marks the intrinsic
    /// presence marker the `Parcel` API writes (`0` = null, `1` = present).
    TypedObject {
        class: String,
        nullable: bool,
    },
    /// `writeList` / `readArrayList`.
    List {
        elem: Box<WireOp>,
    },
    /// `writeArray` / `readArray` (`Object[]`).
    Array {
        elem: Box<WireOp>,
    },
    /// `writeMap` / `readHashMap`.
    Map {
        key: Box<WireOp>,
        value: Box<WireOp>,
    },
    /// `writeBundle` / `readBundle`.
    Bundle,
    /// `writeValue` / `readValue` (generic `Object`).
    Value,
    /// A conditional presence marker: `marker` is the wire value (`I32` for
    /// `if (x != null) writeInt(1) else writeInt(0)`), `ops` are the operations
    /// emitted only when the marker is non-zero.
    Cond {
        marker: Box<WireOp>,
        ops: Vec<WireOp>,
    },
    /// Undecodable fragment. `note` records the method/PC and the reason.
    Unknown {
        note: String,
    },
}

impl WireOp {
    /// True when this op (or anything nested inside it) is undecoded.
    pub fn is_unknown(&self) -> bool {
        match self {
            WireOp::Unknown { .. } => true,
            WireOp::List { elem } | WireOp::Array { elem } => elem.is_unknown(),
            WireOp::Map { key, value } => key.is_unknown() || value.is_unknown(),
            WireOp::Cond { marker, ops } => {
                marker.is_unknown() || ops.iter().any(WireOp::is_unknown)
            }
            _ => false,
        }
    }

    /// A compact, deterministic rendering used in reports and tests.
    pub fn render(&self) -> String {
        match self {
            WireOp::I32 => "I32".into(),
            WireOp::I64 => "I64".into(),
            WireOp::F32 => "F32".into(),
            WireOp::F64 => "F64".into(),
            WireOp::Bool => "Bool".into(),
            WireOp::Byte => "Byte".into(),
            WireOp::Char => "Char".into(),
            WireOp::Str16 => "Str16".into(),
            WireOp::Str8 => "Str8".into(),
            WireOp::Binder { iface } => match iface {
                Some(i) => format!("Binder({i})"),
                None => "Binder".into(),
            },
            WireOp::ByteArray => "ByteArray".into(),
            WireOp::IntArray => "IntArray".into(),
            WireOp::LongArray => "LongArray".into(),
            WireOp::FloatArray => "FloatArray".into(),
            WireOp::DoubleArray => "DoubleArray".into(),
            WireOp::BooleanArray => "BooleanArray".into(),
            WireOp::CharArray => "CharArray".into(),
            WireOp::StringArray => "StringArray".into(),
            WireOp::StringList => "StringList".into(),
            WireOp::BinderList => "BinderList".into(),
            WireOp::RawFd => "RawFd".into(),
            WireOp::TypedArray { class } => format!("TypedArray({class})"),
            WireOp::TypedList { class } => format!("TypedList({class})"),
            WireOp::Parcelable { class } => format!("Parcelable({class})"),
            WireOp::TypedObject { class, nullable } => {
                format!("TypedObject({class}, nullable={nullable})")
            }
            WireOp::List { elem } => format!("List({})", elem.render()),
            WireOp::Array { elem } => format!("Array({})", elem.render()),
            WireOp::Map { key, value } => {
                format!("Map({}, {})", key.render(), value.render())
            }
            WireOp::Bundle => "Bundle".into(),
            WireOp::Value => "Value".into(),
            WireOp::Cond { marker, ops } => {
                let inner: Vec<String> = ops.iter().map(WireOp::render).collect();
                format!("Cond({}, [{}])", marker.render(), inner.join(", "))
            }
            WireOp::Unknown { note } => format!("Unknown({note})"),
        }
    }
}

/// A single AIDL transaction method.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodInfo {
    /// Java method name, e.g. `relayout`.
    pub name: String,
    /// JVM/AIDL descriptor proto, e.g. `(Landroid/view/IWindow;I)I`.
    pub signature: String,
    /// Return type descriptor, e.g. `I` or `Landroid/view/DisplayInfo;`.
    pub return_type: String,
    /// Transaction code passed to `IBinder.transact`.
    pub code: u32,
    /// `true` when the proxy passes `FLAG_ONEWAY`.
    pub oneway: bool,
    /// Request-layout ops in order (interface token excluded).
    pub args: Vec<WireOp>,
    /// Reply-layout ops in order (`readException` header excluded).
    pub reply: Vec<WireOp>,
    /// Set when decoding was incomplete for a reason not expressible as a
    /// single `Unknown` op (e.g. the proxy method is missing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<String>,
}

impl MethodInfo {
    /// True when every arg/reply op decoded without `Unknown`.
    pub fn fully_decoded(&self) -> bool {
        self.incomplete.is_none()
            && self.args.iter().all(|o| !o.is_unknown())
            && self.reply.iter().all(|o| !o.is_unknown())
    }

    /// Render `args` as a compact `[ ... ]` string.
    pub fn render_args(&self) -> String {
        render_ops(&self.args)
    }

    /// Render `reply` as a compact `[ ... ]` string.
    pub fn render_reply(&self) -> String {
        render_ops(&self.reply)
    }
}

fn render_ops(ops: &[WireOp]) -> String {
    let parts: Vec<String> = ops.iter().map(WireOp::render).collect();
    format!("[{}]", parts.join(", "))
}

/// All transactions of one AIDL interface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceInfo {
    /// AIDL interface descriptor, e.g. `android.view.IWindowSession` (dotted,
    /// not `L...;`).
    pub descriptor: String,
    /// Value of `Foo$Stub.FIRST_CALL_TRANSACTION`; normally `1`.
    pub first_call_transaction: u32,
    /// Methods ordered by transaction code.
    pub methods: Vec<MethodInfo>,
}

impl InterfaceInfo {
    /// Look up a method by transaction code.
    pub fn method_by_code(&self, code: u32) -> Option<&MethodInfo> {
        self.methods.iter().find(|m| m.code == code)
    }

    /// Look up a method by name.
    pub fn method_by_name(&self, name: &str) -> Option<&MethodInfo> {
        self.methods.iter().find(|m| m.name == name)
    }
}
