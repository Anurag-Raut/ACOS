//! Binary XML (AXML) parser for `AndroidManifest.xml`.
//!
//! Implemented from the public `ResChunk` / `ResXMLTree` format. It produces a
//! small DOM ([`XmlDocument`]) of elements and typed attributes; the typed
//! [`Manifest`](crate::manifest::Manifest) model is layered on top.

use crate::binary::{read_u16, read_u32, Cursor, StringPool};
use crate::error::{ApkError, Result, AXML_MALFORMED};

const RES_XML_TYPE: u16 = 0x0003;
const RES_XML_RESOURCE_MAP_TYPE: u16 = 0x0180;
const RES_XML_START_NAMESPACE_TYPE: u16 = 0x0100;
const RES_XML_END_NAMESPACE_TYPE: u16 = 0x0101;
const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;
const RES_XML_END_ELEMENT_TYPE: u16 = 0x0103;
const RES_XML_CDATA_TYPE: u16 = 0x0104;

const TYPE_NULL: u8 = 0x00;
const TYPE_REFERENCE: u8 = 0x01;
const TYPE_ATTRIBUTE: u8 = 0x02;
const TYPE_STRING: u8 = 0x03;
const TYPE_FLOAT: u8 = 0x04;
const TYPE_DIMENSION: u8 = 0x05;
const TYPE_FRACTION: u8 = 0x06;
const TYPE_DYNAMIC_REFERENCE: u8 = 0x07;
const TYPE_INT_DEC: u8 = 0x10;
const TYPE_INT_HEX: u8 = 0x11;
const TYPE_INT_BOOLEAN: u8 = 0x12;
const TYPE_INT_COLOR_ARGB8: u8 = 0x1c;
const TYPE_INT_COLOR_RGB8: u8 = 0x1d;
const TYPE_INT_COLOR_ARGB4: u8 = 0x1e;
const TYPE_INT_COLOR_RGB4: u8 = 0x1f;

fn malformed(msg: impl Into<String>) -> ApkError {
    ApkError::new(AXML_MALFORMED, msg)
}

/// A typed Android resource value (`Res_value`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TypedValue {
    Null,
    Reference(u32),
    Attribute(u32),
    String(String),
    Float(f32),
    Dimension { value: f32, unit: String },
    Fraction { value: f32, unit: String },
    IntDec(i32),
    IntHex(u32),
    Boolean(bool),
    ColorArgb8(u32),
    ColorRgb8(u32),
    ColorArgb4(u32),
    ColorRgb4(u32),
    Unknown { data_type: u8, data: u32 },
}

impl TypedValue {
    /// A compact, human-readable rendering used for CLI output.
    pub fn display(&self) -> String {
        match self {
            TypedValue::Null => "null".to_string(),
            TypedValue::Reference(r) => format!("@{r:#010x}"),
            TypedValue::Attribute(a) => format!("?{a:#010x}"),
            TypedValue::String(s) => s.clone(),
            TypedValue::Float(f) => f.to_string(),
            TypedValue::Dimension { value, unit } => format!("{value}{unit}"),
            TypedValue::Fraction { value, unit } => format!("{value}{unit}"),
            TypedValue::IntDec(i) => i.to_string(),
            TypedValue::IntHex(h) => format!("0x{h:08x}"),
            TypedValue::Boolean(b) => b.to_string(),
            TypedValue::ColorArgb8(c)
            | TypedValue::ColorRgb8(c)
            | TypedValue::ColorArgb4(c)
            | TypedValue::ColorRgb4(c) => format!("#{c:08x}"),
            TypedValue::Unknown { data_type, data } => {
                format!("<type {data_type:#04x}>:{data:#010x}")
            }
        }
    }
}

/// A single decoded XML attribute.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct XmlAttribute {
    pub namespace: Option<String>,
    pub name: String,
    pub raw_value: Option<String>,
    pub value: TypedValue,
    pub resource_id: Option<u32>,
    /// Index into the document resource map, if any (used for AXML emission).
    pub name_index: u32,
}

/// A decoded XML element.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct XmlNode {
    pub namespace: Option<String>,
    pub name: String,
    pub line: u32,
    pub attributes: Vec<XmlAttribute>,
    pub children: Vec<XmlNode>,
}

impl XmlNode {
    /// Find a direct attribute by its (unprefixed) local name.
    pub fn attribute(&self, name: &str) -> Option<&XmlAttribute> {
        self.attributes.iter().find(|a| a.name == name)
    }

    /// Direct child elements with the given local name.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a XmlNode> {
        self.children.iter().filter(move |c| c.name == name)
    }
}

/// A parsed binary XML document.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct XmlDocument {
    pub strings: Vec<String>,
    pub resource_ids: Vec<u32>,
    pub root: XmlNode,
}

/// Parse a binary `AndroidManifest.xml` byte slice into a DOM.
pub fn parse(data: &[u8]) -> Result<XmlDocument> {
    let mut c = Cursor::new(data);
    let chunk_type = c.u16()?;
    let header_size = c.u16()? as usize;
    let chunk_size = c.u32()? as usize;
    if chunk_type != RES_XML_TYPE {
        return Err(malformed(format!(
            "not an AXML document (chunk type {chunk_type:#06x})"
        )));
    }
    if header_size < 8 {
        return Err(malformed("AXML header too small"));
    }
    let end = chunk_size.min(data.len());
    if end < header_size {
        return Err(malformed("AXML chunk size smaller than header"));
    }

    let mut pool: Option<StringPool> = None;
    let mut resource_ids: Vec<u32> = Vec::new();
    let mut stack: Vec<XmlNode> = Vec::new();
    let mut root: Option<XmlNode> = None;

    let mut pos = header_size;
    while pos + 8 <= end {
        let ct = read_u16(data, pos)?;
        let cs = read_u16(data, pos + 2)? as usize;
        let size = read_u32(data, pos + 4)? as usize;
        if size < 8 || pos.saturating_add(size) > end {
            return Err(malformed(format!("bad chunk size {size} at offset {pos}")));
        }
        match ct {
            crate::binary::RES_STRING_POOL_TYPE => {
                if pool.is_none() {
                    pool = Some(StringPool::parse(data, pos)?);
                }
            }
            RES_XML_RESOURCE_MAP_TYPE => {
                let base = pos + cs.max(8);
                let chunk_end = pos + size;
                if chunk_end > base {
                    let count = (chunk_end - base) / 4;
                    resource_ids.clear();
                    for i in 0..count {
                        resource_ids.push(read_u32(data, base + i * 4)?);
                    }
                }
            }
            RES_XML_START_NAMESPACE_TYPE | RES_XML_END_NAMESPACE_TYPE => {
                // Namespace prefix/uri are only needed to label output; the
                // element/attribute `ns` fields already index the pool.
            }
            RES_XML_START_ELEMENT_TYPE => {
                let pool_ref = pool
                    .as_ref()
                    .ok_or_else(|| malformed("start element before string pool chunk"))?;
                let node = parse_start_element(data, pos, cs, pool_ref, &resource_ids)?;
                stack.push(node);
            }
            RES_XML_END_ELEMENT_TYPE => {
                let node = stack
                    .pop()
                    .ok_or_else(|| malformed("end element without matching start"))?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => {
                        if root.is_some() {
                            return Err(malformed("multiple root elements"));
                        }
                        root = Some(node);
                    }
                }
            }
            RES_XML_CDATA_TYPE => {
                // Character data is not needed for the manifest/resource model.
            }
            _ => {}
        }
        pos += size;
    }

    if !stack.is_empty() {
        return Err(malformed("unclosed XML elements at end of document"));
    }
    let root = root.ok_or_else(|| malformed("AXML document has no root element"))?;
    let pool = pool.ok_or_else(|| malformed("AXML document has no string pool"))?;
    Ok(XmlDocument {
        strings: pool.strings().to_vec(),
        resource_ids,
        root,
    })
}

fn parse_start_element(
    data: &[u8],
    pos: usize,
    header_size: usize,
    pool: &StringPool,
    resource_ids: &[u32],
) -> Result<XmlNode> {
    let node = pos
        .checked_add(header_size)
        .ok_or_else(|| malformed("offset overflow"))?;
    let mut c = Cursor::at(data, node);
    let ns_idx = c.u32()?;
    let name_idx = c.u32()?;
    let attribute_start = c.u16()? as usize;
    let attribute_size = c.u16()? as usize;
    let attribute_count = c.u16()? as usize;
    let _id_index = c.u16()?;
    let _class_index = c.u16()?;
    let _style_index = c.u16()?;

    let line = read_u32(data, pos + 8)?;
    let name = pool.get_or_empty(name_idx).to_string();
    let namespace = pool.get(ns_idx).map(str::to_string);

    let attrs_base = node
        .checked_add(attribute_start)
        .ok_or_else(|| malformed("offset overflow"))?;
    let stride = attribute_size.max(20);
    let mut attributes = Vec::with_capacity(attribute_count);
    for i in 0..attribute_count {
        let apos = attrs_base + i * stride;
        let mut ac = Cursor::at(data, apos);
        let a_ns = ac.u32()?;
        let a_name = ac.u32()?;
        let a_raw = ac.u32()?;
        let v_size = ac.u16()?;
        let _v_res0 = ac.u8()?;
        let v_type = ac.u8()?;
        let v_data = ac.u32()?;
        let _ = v_size;

        let name = pool.get_or_empty(a_name).to_string();
        let value = decode_value(pool, v_type, v_data);
        let resource_id =
            if (a_name as usize) < resource_ids.len() && resource_ids[a_name as usize] != 0 {
                Some(resource_ids[a_name as usize])
            } else {
                None
            };
        attributes.push(XmlAttribute {
            namespace: pool.get(a_ns).map(str::to_string),
            name,
            raw_value: pool.get(a_raw).map(str::to_string),
            value,
            resource_id,
            name_index: a_name,
        });
    }

    Ok(XmlNode {
        namespace,
        name,
        line,
        attributes,
        children: Vec::new(),
    })
}

fn decode_value(pool: &StringPool, data_type: u8, data: u32) -> TypedValue {
    match data_type {
        TYPE_NULL => TypedValue::Null,
        TYPE_REFERENCE | TYPE_DYNAMIC_REFERENCE => TypedValue::Reference(data),
        TYPE_ATTRIBUTE => TypedValue::Attribute(data),
        TYPE_STRING => TypedValue::String(pool.get_or_empty(data).to_string()),
        TYPE_FLOAT => TypedValue::Float(f32::from_bits(data)),
        TYPE_DIMENSION => {
            let (value, unit) = complex_to_float(data, &DIMENSION_UNITS);
            TypedValue::Dimension { value, unit }
        }
        TYPE_FRACTION => {
            let (value, unit) = complex_to_float(data, &FRACTION_UNITS);
            TypedValue::Fraction { value, unit }
        }
        TYPE_INT_DEC => TypedValue::IntDec(data as i32),
        TYPE_INT_HEX => TypedValue::IntHex(data),
        TYPE_INT_BOOLEAN => TypedValue::Boolean(data != 0),
        TYPE_INT_COLOR_ARGB8 => TypedValue::ColorArgb8(data),
        TYPE_INT_COLOR_RGB8 => TypedValue::ColorRgb8(data),
        TYPE_INT_COLOR_ARGB4 => TypedValue::ColorArgb4(data),
        TYPE_INT_COLOR_RGB4 => TypedValue::ColorRgb4(data),
        other => TypedValue::Unknown {
            data_type: other,
            data,
        },
    }
}

pub(crate) const DIMENSION_UNITS: [&str; 6] = ["px", "dip", "sp", "pt", "in", "mm"];
pub(crate) const FRACTION_UNITS: [&str; 2] = ["%", "%p"];

pub(crate) fn complex_to_float(complex: u32, units: &[&str]) -> (f32, String) {
    // See AOSP Res_value::complexToFloat / getDimensionUnit / getFractionUnit.
    const MANTISSA_MULT: f32 = 1.0 / (1 << 8) as f32;
    const RADIX_MULTS: [f32; 4] = [
        1.0 * MANTISSA_MULT,
        1.0 / (1 << 7) as f32 * MANTISSA_MULT,
        1.0 / (1 << 15) as f32 * MANTISSA_MULT,
        1.0 / (1 << 23) as f32 * MANTISSA_MULT,
    ];
    let radix = ((complex >> 4) & 0x3) as usize;
    let value = (complex & (0xFFFFFF << 8)) as f32 * RADIX_MULTS[radix];
    let unit_idx = (complex & 0xF) as usize;
    let unit = units.get(unit_idx).copied().unwrap_or("?").to_string();
    (value, unit)
}

/// Serialize a DOM back to the textual shape produced by
/// `aapt2 dump xmltree --file ...`, for differential testing.
pub fn to_xmltree(doc: &XmlDocument) -> String {
    let mut out = String::new();
    emit_node(&doc.root, 0, &mut out);
    out
}

fn emit_node(node: &XmlNode, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    out.push_str(&format!("{indent}E: {} (line={})\n", node.name, node.line));
    for a in &node.attributes {
        let qual = match (&a.namespace, a.resource_id) {
            (Some(ns), Some(id)) => format!("{ns}:{}(0x{id:08x})", a.name),
            (Some(ns), None) => format!("{ns}:{}", a.name),
            (None, _) => a.name.clone(),
        };
        out.push_str(&format!("{indent}  A: {qual}={}\n", a.value.display()));
    }
    for child in &node.children {
        emit_node(child, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_and_fraction_decode() {
        // 16 dip: mantissa 16 in bits 8.., radix 0, unit DIP(1).
        let (v, u) = complex_to_float((16u32 << 8) | 0x01, &DIMENSION_UNITS);
        assert_eq!(v, 16.0);
        assert_eq!(u, "dip");

        // 50%: mantissa 50, unit fraction(0) => "%".
        let (v, u) = complex_to_float(50u32 << 8, &FRACTION_UNITS);
        assert_eq!(v, 50.0);
        assert_eq!(u, "%");
    }

    #[test]
    fn typed_value_decode_non_string() {
        let pool = StringPool::default();
        assert_eq!(decode_value(&pool, TYPE_NULL, 0), TypedValue::Null);
        assert_eq!(decode_value(&pool, TYPE_INT_DEC, 5), TypedValue::IntDec(5));
        assert_eq!(
            decode_value(&pool, TYPE_INT_HEX, 0x1f),
            TypedValue::IntHex(0x1f)
        );
        assert_eq!(
            decode_value(&pool, TYPE_INT_BOOLEAN, 1),
            TypedValue::Boolean(true)
        );
        assert_eq!(
            decode_value(&pool, TYPE_INT_BOOLEAN, 0),
            TypedValue::Boolean(false)
        );
        assert_eq!(
            decode_value(&pool, TYPE_REFERENCE, 0x7f01_0000),
            TypedValue::Reference(0x7f01_0000)
        );
        assert_eq!(
            decode_value(&pool, TYPE_FLOAT, 2.5f32.to_bits()),
            TypedValue::Float(2.5)
        );
        assert_eq!(
            decode_value(&pool, TYPE_STRING, u32::MAX),
            TypedValue::String(String::new())
        );
    }

    #[test]
    fn malformed_axml_is_structured() {
        let err = parse(&[0u8; 4]).unwrap_err();
        assert_eq!(err.code(), "AXML_MALFORMED");
    }
}
