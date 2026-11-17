//! The launch `Intent` a scenario / CLI / MCP caller can declare.
//!
//! A scenario's `launch:` step (and `acos launch`, `acos_launch`) can name an
//! action, a data URI and typed extras. They reach the guest's real
//! `Activity.getIntent()` through the host-built `LaunchActivityItem` Intent
//! (ADR-0029 amendment: the host builds launch transactions), so the values
//! must survive the scenario YAML → protocol → parcel round trip with their
//! Java types intact.
//!
//! # YAML spelling
//!
//! Plain scalars infer (the situation a person writing a scenario expects):
//!
//! ```yaml
//! extras:
//!   greeting: hello          # String
//!   count: 42                # int (i32)
//!   identifier: 5000000000   # long (i64; does not fit i32)
//!   enabled: true            # boolean
//!   ratio: 0.25              # double (a plain YAML float)
//!   names: [a, b]            # String[]
//! ```
//!
//! An explicit `{type, value}` object pins the type where inference cannot
//! (a `long` that fits in an `int`, a `float` rather than a `double`, or a type
//! the launcher refuses by name):
//!
//! ```yaml
//! extras:
//!   serial: {type: long, value: 7}
//!   scale: {type: float, value: 1.5}
//!   names: {type: string_array, value: [a, b]}
//!   device: {type: parcellable, value: android.os.Bundle}
//! ```
//!
//! `parcelable` is a legal declaration but not a supported extra: the ART
//! launcher refuses it with `UNSUPPORTED_ANDROID_API` naming the key and class
//! (a `Parcelable` extra needs the app's own class loader and a payload ACOS
//! cannot build from a scenario). A structured refusal is honest; silently
//! dropping the value is not.

use std::collections::BTreeMap;
use std::fmt;

use schemars::schema::{
    InstanceType, Metadata, ObjectValidation, Schema, SchemaObject, SingleOrVec,
    SubschemaValidation,
};
use schemars::{gen::SchemaGenerator, JsonSchema};
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One launch extra, with the Java type it is written as.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtraValue {
    /// `Intent.putExtra(String, String)`.
    String(String),
    /// `Intent.putExtra(String, int)`.
    Int(i32),
    /// `Intent.putExtra(String, long)`.
    Long(i64),
    /// `Intent.putExtra(String, float)`.
    Float(f32),
    /// `Intent.putExtra(String, double)`.
    Double(f64),
    /// `Intent.putExtra(String, boolean)`.
    Boolean(bool),
    /// `Intent.putExtra(String, String[])`.
    StringArray(Vec<String>),
    /// `Intent.putExtra(String, Parcelable)`, named by its class. Refused by
    /// the ART launcher with `UNSUPPORTED_ANDROID_API` (key + class named).
    Parcelable(String),
}

impl ExtraValue {
    /// The Java type name this extra is written as (`string`, `int`, …).
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::String(_) => "string",
            Self::Int(_) => "int",
            Self::Long(_) => "long",
            Self::Float(_) => "float",
            Self::Double(_) => "double",
            Self::Boolean(_) => "boolean",
            Self::StringArray(_) => "string_array",
            Self::Parcelable(_) => "parcelable",
        }
    }

    /// The `Parcelable` class name, when this extra is one.
    #[must_use]
    pub fn parcelable_class(&self) -> Option<&str> {
        match self {
            Self::Parcelable(class) => Some(class),
            _ => None,
        }
    }

    /// A one-line spelling for a structured error (`string "hello"`,
    /// `parcelable android.os.Bundle`).
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::String(value) => format!("string {value:?}"),
            Self::Int(value) => format!("int {value}"),
            Self::Long(value) => format!("long {value}"),
            Self::Float(value) => format!("float {value}"),
            Self::Double(value) => format!("double {value}"),
            Self::Boolean(value) => format!("boolean {value}"),
            Self::StringArray(values) => format!("string_array {values:?}"),
            Self::Parcelable(class) => format!("parcelable {class}"),
        }
    }
}

impl fmt::Display for ExtraValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

impl Serialize for ExtraValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::String(value) => serializer.serialize_str(value),
            Self::Int(value) => serializer.serialize_i32(*value),
            Self::Long(value) => serializer.serialize_i64(*value),
            Self::Float(value) => serializer.serialize_f32(*value),
            Self::Double(value) => serializer.serialize_f64(*value),
            Self::Boolean(value) => serializer.serialize_bool(*value),
            Self::StringArray(values) => values.serialize(serializer),
            // The explicit form is the only unambiguous spelling for a class
            // name and for the types inference cannot produce.
            Self::Parcelable(class) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "parcelable")?;
                map.serialize_entry("value", class)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ExtraValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ExtraVisitor;

        impl<'de> Visitor<'de> for ExtraVisitor {
            type Value = ExtraValue;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a typed extra: a plain scalar or string array, or \
                     {type: string|int|long|float|double|boolean|string_array|parcelable, value: ...}",
                )
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(ExtraValue::Boolean(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                if let Ok(value) = i32::try_from(value) {
                    Ok(ExtraValue::Int(value))
                } else {
                    Ok(ExtraValue::Long(value))
                }
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                let signed = i64::try_from(value)
                    .map_err(|_| E::custom(format!("extra {value} does not fit a Java long")))?;
                self.visit_i64(signed)
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(ExtraValue::Double(value))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(ExtraValue::String(value.to_string()))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(ExtraValue::String(value))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(item) = seq.next_element::<String>()? {
                    values.push(item);
                }
                Ok(ExtraValue::StringArray(values))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut kind: Option<String> = None;
                let mut value: Option<serde_json::Value> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => kind = Some(map.next_value()?),
                        "value" => value = Some(map.next_value()?),
                        other => {
                            return Err(de::Error::custom(format!(
                                "unknown extra field `{other}` (expected `type` and `value`)"
                            )))
                        }
                    }
                }
                let kind =
                    kind.ok_or_else(|| de::Error::custom("explicit extra needs a `type`"))?;
                let value = value
                    .ok_or_else(|| de::Error::custom(format!("explicit extra `{kind}` needs a `value`")))?;
                explicit_extra(&kind, value).map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_any(ExtraVisitor)
    }
}

/// Build one explicit `{type, value}` extra.
fn explicit_extra(kind: &str, value: serde_json::Value) -> Result<ExtraValue, String> {
    match kind {
        "string" => value
            .as_str()
            .map(|v| ExtraValue::String(v.to_string()))
            .ok_or_else(|| format!("extra type `string` needs a string value, got {value}")),
        "int" => {
            let number = value
                .as_i64()
                .ok_or_else(|| format!("extra type `int` needs an integer value, got {value}"))?;
            i32::try_from(number)
                .map(ExtraValue::Int)
                .map_err(|_| format!("{number} does not fit a Java int; use type `long`"))
        }
        "long" => value
            .as_i64()
            .map(ExtraValue::Long)
            .ok_or_else(|| format!("extra type `long` needs an integer value, got {value}")),
        "float" => value
            .as_f64()
            .map(|v| ExtraValue::Float(v as f32))
            .ok_or_else(|| format!("extra type `float` needs a number value, got {value}")),
        "double" => value
            .as_f64()
            .map(ExtraValue::Double)
            .ok_or_else(|| format!("extra type `double` needs a number value, got {value}")),
        "boolean" | "bool" => value
            .as_bool()
            .map(ExtraValue::Boolean)
            .ok_or_else(|| format!("extra type `boolean` needs a boolean value, got {value}")),
        "string_array" | "string-array" => {
            let items = value
                .as_array()
                .ok_or_else(|| format!("extra type `string_array` needs an array, got {value}"))?;
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(
                    item.as_str()
                        .ok_or_else(|| {
                            format!("extra type `string_array` needs string items, got {item}")
                        })?
                        .to_string(),
                );
            }
            Ok(ExtraValue::StringArray(out))
        }
        "parcelable" => value
            .as_str()
            .map(|class| ExtraValue::Parcelable(class.to_string()))
            .ok_or_else(|| {
                format!("extra type `parcelable` needs the class name as its value, got {value}")
            }),
        other => Err(format!(
            "unknown extra type `{other}` (expected string, int, long, float, double, boolean, \
             string_array or parcelable)"
        )),
    }
}

impl JsonSchema for ExtraValue {
    fn schema_name() -> String {
        "ExtraValue".to_string()
    }

    fn json_schema(gen: &mut SchemaGenerator) -> Schema {
        let scalar = |instance: InstanceType, description: &str| {
            Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(instance))),
                metadata: Some(Box::new(Metadata {
                    description: Some(description.to_string()),
                    ..Metadata::default()
                })),
                ..SchemaObject::default()
            })
        };
        let array = Schema::Object(SchemaObject {
            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Array))),
            array: Some(Box::new(schemars::schema::ArrayValidation {
                items: Some(SingleOrVec::Single(Box::new(scalar(
                    InstanceType::String,
                    "a string array extra (String[])",
                )))),
                ..schemars::schema::ArrayValidation::default()
            })),
            metadata: Some(Box::new(Metadata {
                description: Some("a string array extra (String[])".to_string()),
                ..Metadata::default()
            })),
            ..SchemaObject::default()
        });
        let explicit = {
            let mut properties = schemars::Map::new();
            properties.insert(
                "type".to_string(),
                scalar(
                    InstanceType::String,
                    "the Java type: string, int, long, float, double, boolean, string_array, parcelable",
                ),
            );
            properties.insert(
                "value".to_string(),
                gen.subschema_for::<serde_json::Value>(),
            );
            let mut required = std::collections::BTreeSet::new();
            required.insert("type".to_string());
            required.insert("value".to_string());
            Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
                object: Some(Box::new(ObjectValidation {
                    properties,
                    required,
                    ..ObjectValidation::default()
                })),
                metadata: Some(Box::new(Metadata {
                    description: Some(
                        "explicit form: {type: <java type>, value: <value>}; pins long vs int, \
                         float vs double, and names a refused parcelable extra"
                            .to_string(),
                    ),
                    ..Metadata::default()
                })),
                ..SchemaObject::default()
            })
        };
        Schema::Object(SchemaObject {
            metadata: Some(Box::new(Metadata {
                description: Some(
                    "A launch extra: a plain scalar (string, int, long when it does not fit an \
                     int, boolean, double for a plain float) or string array, or the explicit \
                     {type, value} form."
                        .to_string(),
                ),
                ..Metadata::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation {
                any_of: Some(vec![
                    scalar(InstanceType::String, "a string extra"),
                    scalar(InstanceType::Integer, "an int extra (a long when it does not fit)"),
                    scalar(InstanceType::Number, "a double extra (a plain YAML float)"),
                    scalar(InstanceType::Boolean, "a boolean extra"),
                    array,
                    explicit,
                ]),
                ..SubschemaValidation::default()
            })),
            ..SchemaObject::default()
        })
    }
}

/// The launch `Intent` a caller declared: action, data URI and typed extras.
///
/// `extras` is keyed by the extra name; the map is ordered so the encoded
/// `Bundle` is deterministic.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LaunchIntent {
    /// `Intent.setAction` (absent keeps the launcher's `ACTION_MAIN`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// `Intent.setData(Uri.parse(...))`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// `Intent.putExtra` values, typed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extras: BTreeMap<String, ExtraValue>,
}

impl LaunchIntent {
    /// `true` when the caller declared nothing (today's manifest launch).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.action.is_none() && self.data.is_none() && self.extras.is_empty()
    }

    /// The first extra whose type the launcher cannot carry, as
    /// `(key, class)`; `None` when every extra is writable.
    #[must_use]
    pub fn unsupported_extra(&self) -> Option<(&str, &str)> {
        self.extras.iter().find_map(|(key, value)| {
            value
                .parcelable_class()
                .map(|class| (key.as_str(), class))
        })
    }

    /// The structured detail for a refused `Parcelable` extra.
    #[must_use]
    pub fn unsupported_detail(&self) -> Option<String> {
        let (key, class) = self.unsupported_extra()?;
        Some(format!(
            "the launch Intent cannot carry the `{key}` extra: a Parcelable ({class}) needs the \
             app's own class loader and a payload a scenario cannot build. Refusing instead of \
             launching an Intent without the declared value."
        ))
    }
}
