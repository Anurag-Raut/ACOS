//! Typed values exchanged between the dispatcher and the service state
//! machines.
//!
//! The dispatcher decodes a request `Parcel` into a `Vec<Value>` strictly in the
//! order of the mechanically-decoded [`acos_aidl::WireOp`] list. A handler
//! returns the reply `Vec<Value>` in the same decoded order. Because the
//! `WireOp` list is read out of the framework DEX, a wrong field order in a
//! service implementation fails the layout check instead of silently producing
//! a wrong parcel.

use std::collections::BTreeMap;

use acos_parcel::{BinderId, ParcelFd};

/// A parcelable value.
#[derive(Debug)]
pub enum Parcelable {
    /// `null`.
    Null,
    /// A parcelable encoded as its decoded field sequence.
    Ordered {
        /// Dotted class name (`android.content.pm.ApplicationInfo`).
        class: String,
        /// Field values in `writeToParcel` order.
        values: Vec<Value>,
    },
    /// A `ParcelFileDescriptor`; the parcel owns a duplicate of the descriptor.
    Fd(Option<ParcelFd>),
}

impl Clone for Parcelable {
    fn clone(&self) -> Self {
        match self {
            Self::Null => Self::Null,
            Self::Ordered { class, values } => Self::Ordered {
                class: class.clone(),
                values: values.clone(),
            },
            Self::Fd(fd) => Self::Fd(fd.as_ref().and_then(|f| f.try_clone().ok())),
        }
    }
}

impl PartialEq for Parcelable {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (
                Self::Ordered {
                    class: a,
                    values: av,
                },
                Self::Ordered {
                    class: b,
                    values: bv,
                },
            ) => a == b && av == bv,
            (Self::Fd(Some(a)), Self::Fd(Some(b))) => a.as_raw_fd() == b.as_raw_fd(),
            (Self::Fd(None), Self::Fd(None)) => true,
            _ => false,
        }
    }
}

impl Parcelable {
    /// Build an ordered parcelable.
    #[must_use]
    pub fn ordered(class: impl Into<String>, values: Vec<Value>) -> Self {
        Self::Ordered {
            class: class.into(),
            values,
        }
    }

    /// The dotted class name, when this is not `null`.
    #[must_use]
    pub fn class(&self) -> Option<&str> {
        match self {
            Self::Ordered { class, .. } => Some(class),
            _ => None,
        }
    }

    /// The ordered field values, when this is an ordered parcelable.
    #[must_use]
    pub fn fields(&self) -> Option<&[Value]> {
        match self {
            Self::Ordered { values, .. } => Some(values),
            _ => None,
        }
    }

    /// Field `index`, if present.
    #[must_use]
    pub fn field(&self, index: usize) -> Option<&Value> {
        self.fields().and_then(|v| v.get(index))
    }
}

/// A minimal `android.os.Bundle` model.
///
/// ACOS only needs the scalar/string subset used by `Settings` and framework
/// metadata; nested bundles, lists and parcelables are represented but not
/// required by the V1 launch path.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bundle {
    /// Entries in insertion order (Android's `ArrayMap` preserves it).
    pub entries: Vec<(String, Value)>,
}

impl Bundle {
    /// An empty bundle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a key.
    pub fn put(&mut self, key: impl Into<String>, value: Value) {
        let key = key.into();
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }

    /// Look up a key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A `Bundle{Parcelable}` for the Settings provider reply.
    #[must_use]
    pub fn parcelable(&self) -> Parcelable {
        Parcelable::ordered("android.os.Bundle", vec![Value::Bundle(Some(self.clone()))])
    }
}

/// A dynamically-typed `Parcel.writeValue` payload.
#[derive(Debug, Clone, PartialEq)]
pub enum DynamicValue {
    /// `VAL_NULL`.
    Null,
    /// `VAL_STRING` / `VAL_CHARSEQUENCE`.
    String(Option<String>),
    /// `VAL_INTEGER`.
    Int(i32),
    /// `VAL_SHORT`: `Bundle.putShort`/`Intent.putExtra(String, short)`. The
    /// image writes the tag then a 4-byte int (`Parcel.writeValue` VAL_SHORT →
    /// `writeInt`), and the client's `getShort` casts to `java.lang.Short` — an
    /// `Integer` there is a `ClassCastException` warning and the default comes
    /// back (`BluetoothDevice.EXTRA_RSSI` was read as 0 that way).
    Short(i16),
    /// `VAL_LONG`.
    Long(i64),
    /// `VAL_FLOAT`.
    Float(f32),
    /// `VAL_DOUBLE`.
    Double(f64),
    /// `VAL_BOOLEAN`.
    Bool(bool),
    /// `VAL_BYTE`.
    Byte(i8),
    /// `VAL_BYTEARRAY`.
    ByteArray(Option<Vec<u8>>),
    /// `VAL_INTARRAY`.
    IntArray(Option<Vec<i32>>),
    /// `VAL_LONGARRAY`.
    LongArray(Option<Vec<i64>>),
    /// `VAL_STRINGARRAY`.
    StringArray(Option<Vec<Option<String>>>),
    /// `VAL_IBINDER`.
    Binder(Option<BinderId>),
    /// A `VAL_MAP`/`VAL_BUNDLE`-shaped nested map (best effort).
    Map(Vec<(DynamicValue, DynamicValue)>),
    /// A `VAL_SPARSEARRAY` (the image's tag 12): an `int` key per value.
    ///
    /// `activityStopped`'s saved-state `Bundle` carries the window hierarchy
    /// state, which `Activity.onSaveInstanceState` puts in as a
    /// `SparseArray<Parcelable>`; the reader had no model for the tag, so the
    /// whole request failed (`unmodelled Parcel.VAL_* tag 12`) on every
    /// `activityStopped` (MPROC7).
    SparseArray(Vec<(i32, DynamicValue)>),
    /// A `VAL_PARCELABLE` payload.
    Parcelable(Parcelable),
    /// `VAL_OBJECTARRAY`: `Parcel.writeArray(Object[])` — a count word then one
    /// `writeValue` payload per element. Android's `SMS_RECEIVED` broadcast
    /// carries its PDUs this way (`Object[]` of `byte[]`).
    ObjectArray(Option<Vec<DynamicValue>>),
    /// `VAL_LIST`: `Parcel.writeList(List)` — the Bluetooth bonded-device
    /// result (a `List<BluetoothDevice>`) is written as one.
    List(Option<Vec<DynamicValue>>),
    /// A value this crate does not model; its bytes were skipped.
    Skipped {
        /// `Parcel.VAL_*` tag.
        tag: i32,
    },
}

impl DynamicValue {
    /// Interpret as an integer.
    #[must_use]
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// Interpret as a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(Some(s)) => Some(s),
            _ => None,
        }
    }
}

/// A `ParcelFileDescriptor` payload with value semantics.
///
/// `ParcelFd` is move-only; this wrapper clones by duplicating the descriptor
/// and compares by raw fd so `Value` can stay `Clone + PartialEq`.
#[derive(Debug)]
pub struct RawFdValue(pub ParcelFd);

impl Clone for RawFdValue {
    fn clone(&self) -> Self {
        Self(
            self.0
                .try_clone()
                .unwrap_or_else(|_| ParcelFd::memfd("acos-fd-clone-fallback").expect("memfd")),
        )
    }
}

impl PartialEq for RawFdValue {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_raw_fd() == other.0.as_raw_fd()
    }
}

impl RawFdValue {
    /// Borrow the descriptor.
    #[must_use]
    pub fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }

    /// Raw descriptor number.
    #[must_use]
    pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0.as_raw_fd()
    }
}

/// A value in the decoded argument/reply sequence.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// No value (used for `void`).
    Unit,
    /// `int`.
    Int(i32),
    /// `short` (a `Bundle`/`Intent` extra value, `VAL_SHORT`).
    Short(i16),
    /// `long`.
    Long(i64),
    /// `float`.
    Float(f32),
    /// `double`.
    Double(f64),
    /// `boolean`.
    Bool(bool),
    /// `byte`.
    Byte(i8),
    /// `char`.
    Char(u16),
    /// `String` written with `writeString`/`writeString16`.
    Str(Option<String>),
    /// `String` written with `writeString8`.
    Str8(Option<String>),
    /// A strong binder reference.
    Binder(Option<BinderId>),
    /// `String[]`.
    StringArray(Option<Vec<Option<String>>>),
    /// `int[]`.
    IntArray(Option<Vec<i32>>),
    /// `long[]`.
    LongArray(Option<Vec<i64>>),
    /// `float[]`.
    FloatArray(Option<Vec<f32>>),
    /// `double[]`.
    DoubleArray(Option<Vec<f64>>),
    /// `boolean[]`.
    BooleanArray(Option<Vec<bool>>),
    /// `char[]`.
    CharArray(Option<Vec<u16>>),
    /// `byte[]`.
    ByteArray(Option<Vec<u8>>),
    /// `Object[]` (a `Parcel.writeArray` inside a `Bundle` value). Android's
    /// `SMS_RECEIVED` broadcast stores its PDUs as `Object[]` of `byte[]`.
    ObjectArray(Option<Vec<Value>>),
    /// `List<String>`.
    StringList(Option<Vec<Option<String>>>),
    /// A parcelable payload.
    Parcelable(Parcelable),
    /// A `writeTypedObject` (4-byte presence marker + payload).
    TypedObject(Parcelable),
    /// A `writeParcelable` (class name + payload).
    NamedParcelable(Parcelable),
    /// A `Bundle`.
    Bundle(Option<Bundle>),
    /// A dynamically typed `writeValue` payload.
    Dynamic(DynamicValue),
    /// A conditional reply group: the `I32` marker plus its body.
    Cond {
        /// Whether the body is present (marker `1`).
        present: bool,
        /// Body values in decoded order.
        values: Vec<Value>,
    },
    /// A list of values (for `List`/`Array` wire ops).
    List(Option<Vec<Value>>),
    /// A map of dynamic values.
    Map(Option<Vec<(DynamicValue, DynamicValue)>>),
    /// A raw file descriptor written inline.
    RawFd(Option<RawFdValue>),
    /// A value whose bytes were consumed but not modelled.
    Skipped,
}

impl Value {
    /// `true` for [`Value::Unit`].
    #[must_use]
    pub fn is_unit(&self) -> bool {
        matches!(self, Self::Unit)
    }

    /// Interpret as an `i32`.
    #[must_use]
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// Interpret as an `i64`.
    #[must_use]
    pub fn as_long(&self) -> Option<i64> {
        match self {
            Self::Long(v) => Some(*v),
            _ => None,
        }
    }

    /// Interpret as a `bool`.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// Interpret as a `f32`.
    #[must_use]
    pub fn as_float(&self) -> Option<f32> {
        match self {
            Self::Float(v) => Some(*v),
            _ => None,
        }
    }

    /// Interpret as a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(Some(s)) | Self::Str8(Some(s)) => Some(s),
            _ => None,
        }
    }

    /// Interpret as a parcelable.
    #[must_use]
    pub fn as_parcelable(&self) -> Option<&Parcelable> {
        match self {
            Self::Parcelable(p) | Self::TypedObject(p) | Self::NamedParcelable(p) => Some(p),
            _ => None,
        }
    }

    /// Interpret as a bundle.
    #[must_use]
    pub fn as_bundle(&self) -> Option<&Bundle> {
        match self {
            Self::Bundle(Some(b)) => Some(b),
            _ => None,
        }
    }

    /// Interpret as a binder handle.
    #[must_use]
    pub fn as_binder(&self) -> Option<BinderId> {
        match self {
            Self::Binder(Some(id)) => Some(*id),
            _ => None,
        }
    }

    /// A string value.
    #[must_use]
    pub fn string(s: impl Into<String>) -> Self {
        Self::Str(Some(s.into()))
    }
}

/// Ordered field accessor used by the typed parcelable builders and the tests.
#[must_use]
pub fn field_i32(values: &[Value], index: usize) -> Option<i32> {
    values.get(index).and_then(Value::as_int)
}

/// Ordered string field accessor.
#[must_use]
pub fn field_str(values: &[Value], index: usize) -> Option<&str> {
    values.get(index).and_then(Value::as_str)
}

/// Stable key ordering for evidence summaries.
#[must_use]
pub fn bundle_keys(bundle: &Bundle) -> Vec<&str> {
    bundle.entries.iter().map(|(k, _)| k.as_str()).collect()
}

/// Deterministic map for tests.
#[must_use]
pub fn collect_map<'a>(entries: impl Iterator<Item = (&'a str, Value)>) -> BTreeMap<String, Value> {
    entries.map(|(k, v)| (k.to_string(), v)).collect()
}
