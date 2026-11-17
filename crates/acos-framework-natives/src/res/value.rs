//! Android resource value primitives: `Res_value` type codes, the `TypedValue`
//! output shape, and the fixed `int[]` layout that `android.util.TypedArray`
//! reads back from native code.

/// `Res_value::TYPE_NULL`.
pub const TYPE_NULL: u8 = 0x00;
/// `Res_value::TYPE_REFERENCE` (`@resid`).
pub const TYPE_REFERENCE: u8 = 0x01;
/// `Res_value::TYPE_ATTRIBUTE` (`?attr`).
pub const TYPE_ATTRIBUTE: u8 = 0x02;
/// `Res_value::TYPE_STRING`.
pub const TYPE_STRING: u8 = 0x03;
/// `Res_value::TYPE_FLOAT`.
pub const TYPE_FLOAT: u8 = 0x04;
/// `Res_value::TYPE_DIMENSION`.
pub const TYPE_DIMENSION: u8 = 0x05;
/// `Res_value::TYPE_FRACTION`.
pub const TYPE_FRACTION: u8 = 0x06;
/// `Res_value::TYPE_DYNAMIC_REFERENCE`.
pub const TYPE_DYNAMIC_REFERENCE: u8 = 0x07;
/// `Res_value::TYPE_DYNAMIC_ATTRIBUTE`.
pub const TYPE_DYNAMIC_ATTRIBUTE: u8 = 0x08;
/// `Res_value::TYPE_FIRST_INT` / `TYPE_INT_DEC`.
pub const TYPE_INT_DEC: u8 = 0x10;
/// `Res_value::TYPE_INT_HEX`.
pub const TYPE_INT_HEX: u8 = 0x11;
/// `Res_value::TYPE_INT_BOOLEAN`.
pub const TYPE_INT_BOOLEAN: u8 = 0x12;
/// `Res_value::TYPE_INT_COLOR_ARGB8`.
pub const TYPE_INT_COLOR_ARGB8: u8 = 0x1c;
/// `Res_value::TYPE_INT_COLOR_RGB8`.
pub const TYPE_INT_COLOR_RGB8: u8 = 0x1d;
/// `Res_value::TYPE_INT_COLOR_ARGB4`.
pub const TYPE_INT_COLOR_ARGB4: u8 = 0x1e;
/// `Res_value::TYPE_INT_COLOR_RGB4`.
pub const TYPE_INT_COLOR_RGB4: u8 = 0x1f;

/// Number of `int` slots `android.util.TypedArray` reserves per attribute.
pub const STYLE_NUM_ENTRIES: usize = 7;
/// `TypedArray.STYLE_TYPE`.
pub const STYLE_TYPE: usize = 0;
/// `TypedArray.STYLE_DATA`.
pub const STYLE_DATA: usize = 1;
/// `TypedArray.STYLE_ASSET_COOKIE`.
pub const STYLE_ASSET_COOKIE: usize = 2;
/// `TypedArray.STYLE_RESOURCE_ID`.
pub const STYLE_RESOURCE_ID: usize = 3;
/// `TypedArray.STYLE_CHANGING_CONFIGURATIONS`.
pub const STYLE_CHANGING_CONFIGURATIONS: usize = 4;
/// `TypedArray.STYLE_DENSITY`.
pub const STYLE_DENSITY: usize = 5;
/// `TypedArray.STYLE_SOURCE_RESOURCE_ID`.
pub const STYLE_SOURCE_RESOURCE_ID: usize = 6;

/// A raw `Res_value`: a 1-byte type tag and a 4-byte payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResValue {
    /// The `Res_value::dataType` byte.
    pub data_type: u8,
    /// The `Res_value::data` payload.
    pub data: u32,
}

impl ResValue {
    /// The null value.
    pub const NULL: ResValue = ResValue {
        data_type: TYPE_NULL,
        data: 0,
    };

    /// A reference (`@resid`).
    pub const fn reference(id: u32) -> Self {
        ResValue {
            data_type: TYPE_REFERENCE,
            data: id,
        }
    }

    /// An attribute reference (`?attr`).
    pub const fn attribute(id: u32) -> Self {
        ResValue {
            data_type: TYPE_ATTRIBUTE,
            data: id,
        }
    }

    /// True for `@resid` and `?attr` indirections.
    pub fn is_reference(&self) -> bool {
        matches!(
            self.data_type,
            TYPE_REFERENCE | TYPE_ATTRIBUTE | TYPE_DYNAMIC_REFERENCE | TYPE_DYNAMIC_ATTRIBUTE
        )
    }

    /// True for string-pool-backed values.
    pub fn is_string(&self) -> bool {
        self.data_type == TYPE_STRING
    }

    /// True for `TYPE_NULL`.
    pub fn is_null(&self) -> bool {
        self.data_type == TYPE_NULL
    }

    /// Decode a dimension/fraction to a float plus its unit suffix.
    pub fn to_float(&self) -> Option<f64> {
        match self.data_type {
            TYPE_FLOAT => Some(f32::from_bits(self.data) as f64),
            TYPE_DIMENSION => Some(complex_to_float(self.data, &DIMENSION_UNITS).0 as f64),
            TYPE_FRACTION => Some(complex_to_float(self.data, &FRACTION_UNITS).0 as f64),
            _ => None,
        }
    }

    /// `TypedValue.coerceToString` for non-string values (used by
    /// `XmlBlock.getAttributeValue`).
    pub fn coerce_to_string(&self) -> Option<String> {
        Some(match self.data_type {
            TYPE_NULL => return None,
            TYPE_REFERENCE | TYPE_DYNAMIC_REFERENCE => format!("@{}", self.data),
            TYPE_ATTRIBUTE | TYPE_DYNAMIC_ATTRIBUTE => format!("?{}", self.data),
            TYPE_FLOAT => format_float(f32::from_bits(self.data) as f64),
            TYPE_DIMENSION => {
                let (v, unit) = complex_to_float(self.data, &DIMENSION_UNITS);
                format!("{}{}", format_float(v as f64), unit)
            }
            TYPE_FRACTION => {
                let (v, unit) = complex_to_float(self.data, &FRACTION_UNITS);
                format!("{}{}", format_float(v as f64), unit)
            }
            TYPE_INT_DEC => format!("{}", self.data as i32),
            TYPE_INT_HEX => format!("{:#x}", self.data),
            TYPE_INT_BOOLEAN => {
                if self.data != 0 {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
            TYPE_INT_COLOR_ARGB8 | TYPE_INT_COLOR_RGB8 | TYPE_INT_COLOR_ARGB4
            | TYPE_INT_COLOR_RGB4 => format!("#{:08x}", self.data),
            _ => format!("{:#x}", self.data),
        })
    }
}

/// The full result of a resource lookup, matching the fields `TypedValue`
/// exposes to Java.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedValueOut {
    /// `TypedValue.type`.
    pub data_type: u8,
    /// `TypedValue.data`.
    pub data: u32,
    /// `TypedValue.assetCookie`.
    pub asset_cookie: i32,
    /// `TypedValue.resourceId`.
    pub resource_id: u32,
    /// `TypedValue.changingConfigurations` (`-1` when unknown).
    pub changing_configurations: i32,
    /// `TypedValue.density`.
    pub density: u16,
    /// `TypedValue.sourceResourceId`.
    pub source_resource_id: u32,
    /// `TypedValue.flags`.
    pub flags: u32,
    /// `TypedValue.string` (already decoded when the value is a string).
    pub string: Option<String>,
}

impl Default for TypedValueOut {
    fn default() -> Self {
        TypedValueOut {
            data_type: TYPE_NULL,
            data: 0,
            asset_cookie: -1,
            resource_id: 0,
            changing_configurations: -1,
            density: 0,
            source_resource_id: 0,
            flags: 0,
            string: None,
        }
    }
}

impl TypedValueOut {
    /// Write this value into the 7-int `TypedArray` layout.
    pub fn write_style(&self, out: &mut [i32]) {
        if out.len() < STYLE_NUM_ENTRIES {
            return;
        }
        out[STYLE_TYPE] = self.data_type as i32;
        out[STYLE_DATA] = self.data as i32;
        out[STYLE_ASSET_COOKIE] = self.asset_cookie;
        out[STYLE_RESOURCE_ID] = self.resource_id as i32;
        out[STYLE_CHANGING_CONFIGURATIONS] = self.changing_configurations;
        out[STYLE_DENSITY] = self.density as i32;
        out[STYLE_SOURCE_RESOURCE_ID] = self.source_resource_id as i32;
    }

    /// Read the 7-int layout back (inverse of [`TypedValueOut::write_style`]).
    pub fn read_style(slice: &[i32]) -> Option<TypedValueOut> {
        if slice.len() < STYLE_NUM_ENTRIES {
            return None;
        }
        Some(TypedValueOut {
            data_type: slice[STYLE_TYPE] as u8,
            data: slice[STYLE_DATA] as u32,
            asset_cookie: slice[STYLE_ASSET_COOKIE],
            resource_id: slice[STYLE_RESOURCE_ID] as u32,
            changing_configurations: slice[STYLE_CHANGING_CONFIGURATIONS],
            density: slice[STYLE_DENSITY] as u16,
            source_resource_id: slice[STYLE_SOURCE_RESOURCE_ID] as u32,
            flags: 0,
            string: None,
        })
    }
}

/// `Res_value` dimension unit suffixes.
pub const DIMENSION_UNITS: [&str; 6] = ["px", "dp", "sp", "pt", "in", "mm"];
/// `Res_value` fraction unit suffixes.
pub const FRACTION_UNITS: [&str; 2] = ["%", "%p"];

/// Port of `Res_value::complexToFloat`.
pub fn complex_to_float(complex: u32, units: &[&str]) -> (f32, &'static str) {
    const MANTISSA_MULT: f32 = 1.0 / (1 << 8) as f32;
    const RADIX_MULTS: [f32; 4] = [
        1.0 * MANTISSA_MULT,
        1.0 / (1 << 7) as f32 * MANTISSA_MULT,
        1.0 / (1 << 15) as f32 * MANTISSA_MULT,
        1.0 / (1 << 23) as f32 * MANTISSA_MULT,
    ];
    let radix = ((complex >> 4) & 0x3) as usize;
    let value = (complex & (0xFF_FFFF << 8)) as f32 * RADIX_MULTS[radix];
    let unit_idx = (complex & 0xF) as usize;
    let unit: &'static str = match units.get(unit_idx) {
        Some(s) => match *s {
            "px" => "px",
            "dip" | "dp" => "dp",
            "sp" => "sp",
            "pt" => "pt",
            "in" => "in",
            "mm" => "mm",
            "%" => "%",
            "%p" => "%p",
            _ => "?",
        },
        None => "?",
    };
    (value, unit)
}

fn format_float(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        v.to_string()
    }
}
