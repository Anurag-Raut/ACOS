//! `XmlBlock` & `StringBlock`: a pull parser over compiled binary XML (AXML)
//! plus the `StringBlock` string/style access it exposes.
//!
//! The engine parses the AXML chunk stream once into an owned event list whose
//! name/namespace/text fields are *string-pool indices* (exactly what the
//! natives return); the Java layer decodes them through the `StringBlock`.
//!
//! Event integers match `org.xmlpull.v1.XmlPullParser`:
//! `START_DOCUMENT=0`, `END_DOCUMENT=1`, `START_TAG=2`, `END_TAG=3`, `TEXT=4`.

use crate::res::bytes::{axml_u16, axml_u32};
use crate::res::error::{ResError, Result, RES_AXML};
use crate::res::strpool::{StringBlock, StringPool, StyleSpan};
use crate::res::value::ResValue;

/// `XmlPullParser.START_DOCUMENT`.
pub const START_DOCUMENT: i32 = 0;
/// `XmlPullParser.END_DOCUMENT`.
pub const END_DOCUMENT: i32 = 1;
/// `XmlPullParser.START_TAG`.
pub const START_TAG: i32 = 2;
/// `XmlPullParser.END_TAG`.
pub const END_TAG: i32 = 3;
/// `XmlPullParser.TEXT`.
pub const TEXT: i32 = 4;

const RES_XML_TYPE: u16 = 0x0003;
const RES_XML_RESOURCE_MAP_TYPE: u16 = 0x0180;
const RES_XML_START_NAMESPACE_TYPE: u16 = 0x0100;
const RES_XML_END_NAMESPACE_TYPE: u16 = 0x0101;
const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;
const RES_XML_END_ELEMENT_TYPE: u16 = 0x0103;
const RES_XML_CDATA_TYPE: u16 = 0x0104;

/// A decoded XML attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlAttribute {
    /// Namespace string-pool index (`-1` = none).
    pub namespace_index: i32,
    /// Name string-pool index.
    pub name_index: i32,
    /// Raw value string-pool index (`-1` = none).
    pub raw_value_index: i32,
    /// Decoded typed value.
    pub value: ResValue,
    /// Resource id (0 when the attribute is not a framework attribute).
    pub resource_id: u32,
}

/// A start/end tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlTag {
    /// Namespace string-pool index.
    pub namespace_index: i32,
    /// Name string-pool index.
    pub name_index: i32,
    /// Source line number.
    pub line: u32,
    /// Attributes (empty for end tags).
    pub attributes: Vec<XmlAttribute>,
    /// 0-based index of the `id` attribute (`-1` = none).
    pub id_index: i32,
    /// 0-based index of the `class` attribute (`-1` = none).
    pub class_index: i32,
    /// 0-based index of the `style` attribute (`-1` = none).
    pub style_index: i32,
}

/// One AXML event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlEvent {
    /// Start of document.
    StartDocument,
    /// A start tag.
    StartTag(XmlTag),
    /// A text (CDATA) node.
    Text {
        /// Text string-pool index.
        text_index: i32,
        /// Source line number.
        line: u32,
    },
    /// An end tag.
    EndTag {
        /// Namespace string-pool index.
        namespace_index: i32,
        /// Name string-pool index.
        name_index: i32,
        /// Source line number.
        line: u32,
    },
    /// End of document.
    EndDocument,
}

/// A parsed binary XML document.
#[derive(Debug, Clone)]
pub struct XmlDocument {
    /// Names/values string block.
    pub string_block: StringBlock,
    /// XML resource map (attribute name index -> resource id).
    pub resource_ids: Vec<u32>,
    /// Event stream (`StartDocument` first, `EndDocument` last).
    pub events: Vec<XmlEvent>,
}

/// A native `XmlBlock` handle payload.
#[derive(Debug, Clone)]
pub struct XmlBlock {
    /// Raw AXML bytes (owned; the Java layer may pass a `byte[]`).
    pub data: Vec<u8>,
    /// Parsed document.
    pub doc: XmlDocument,
}

impl XmlBlock {
    /// Parse `data[offset .. offset+size]` into an `XmlBlock`.
    pub fn from_bytes(data: &[u8], offset: usize, size: usize) -> Result<Self> {
        let end = offset
            .checked_add(size)
            .ok_or_else(|| ResError::new(RES_AXML, "size overflow"))?;
        let slice = data
            .get(offset..end)
            .ok_or_else(|| ResError::new(RES_AXML, "range out of bounds"))?;
        let doc = parse_axml(slice)?;
        Ok(XmlBlock {
            data: slice.to_vec(),
            doc,
        })
    }

    /// The `StringBlock` backing this document.
    pub fn string_block(&self) -> &StringBlock {
        &self.doc.string_block
    }
}

fn axml(msg: impl Into<String>) -> ResError {
    ResError::new(RES_AXML, msg)
}

/// Parse a full AXML buffer into an owned document.
pub fn parse_axml(data: &[u8]) -> Result<XmlDocument> {
    let chunk_type = axml_u16(data, 0)?;
    let header_size = axml_u16(data, 2)? as usize;
    let chunk_size = axml_u32(data, 4)? as usize;
    if chunk_type != RES_XML_TYPE {
        return Err(axml(format!(
            "not an AXML document (chunk type {chunk_type:#06x})"
        )));
    }
    if header_size < 8 {
        return Err(axml("AXML header too small"));
    }
    let end = chunk_size.min(data.len());
    if end < header_size {
        return Err(axml("AXML chunk smaller than header"));
    }

    let mut pool: Option<StringBlock> = None;
    let mut live_pool: Option<StringPool<'_>> = None;
    let mut resource_ids: Vec<u32> = Vec::new();
    let mut events: Vec<XmlEvent> = vec![XmlEvent::StartDocument];

    let mut pos = header_size;
    while pos + 8 <= end {
        let ct = axml_u16(data, pos)?;
        let cs = axml_u16(data, pos + 2)? as usize;
        let size = axml_u32(data, pos + 4)? as usize;
        if size < 8 || pos.saturating_add(size) > end {
            return Err(axml(format!("bad chunk size {size} at {pos}")));
        }
        match ct {
            crate::res::strpool::RES_STRING_POOL_TYPE => {
                if pool.is_none() {
                    let view = StringPool::parse(data, pos)?;
                    pool = Some(StringBlock::from_pool(&view)?);
                    live_pool = Some(view);
                }
            }
            RES_XML_RESOURCE_MAP_TYPE => {
                let base = pos + cs.max(8);
                let chunk_end = pos + size;
                if chunk_end > base {
                    let count = (chunk_end - base) / 4;
                    resource_ids.clear();
                    for i in 0..count {
                        resource_ids.push(axml_u32(data, base + i * 4)?);
                    }
                }
            }
            RES_XML_START_NAMESPACE_TYPE | RES_XML_END_NAMESPACE_TYPE => {}
            RES_XML_START_ELEMENT_TYPE => {
                if live_pool.is_none() {
                    return Err(axml("start element before string pool"));
                }
                let tag = parse_start_element(data, pos, cs, &resource_ids)?;
                events.push(XmlEvent::StartTag(tag));
            }
            RES_XML_END_ELEMENT_TYPE => {
                let node = pos + cs.max(8);
                let namespace_index = axml_u32(data, node).map(|v| v as i32).unwrap_or(-1);
                let name_index = axml_u32(data, node + 4).map(|v| v as i32).unwrap_or(-1);
                let line = axml_u32(data, pos + 8).unwrap_or(0);
                events.push(XmlEvent::EndTag {
                    namespace_index,
                    name_index,
                    line,
                });
            }
            RES_XML_CDATA_TYPE => {
                let node = pos + cs.max(8);
                let text_index = axml_u32(data, node).map(|v| v as i32).unwrap_or(-1);
                let line = axml_u32(data, pos + 8).unwrap_or(0);
                events.push(XmlEvent::Text { text_index, line });
            }
            _ => {}
        }
        pos = pos.saturating_add(size);
    }
    events.push(XmlEvent::EndDocument);
    let string_block = pool.ok_or_else(|| axml("AXML document has no string pool"))?;
    Ok(XmlDocument {
        string_block,
        resource_ids,
        events,
    })
}

fn parse_start_element(
    data: &[u8],
    pos: usize,
    header_size: usize,
    resource_ids: &[u32],
) -> Result<XmlTag> {
    let node = pos
        .checked_add(header_size)
        .ok_or_else(|| axml("offset overflow"))?;
    let namespace_index = axml_u32(data, node)? as i32;
    let name_index = axml_u32(data, node + 4)? as i32;
    let attribute_start = axml_u16(data, node + 8)? as usize;
    let attribute_size = axml_u16(data, node + 10)? as usize;
    let attribute_count = axml_u16(data, node + 12)? as usize;
    let id_index = axml_u16(data, node + 14)? as i32;
    let class_index = axml_u16(data, node + 16)? as i32;
    let style_index = axml_u16(data, node + 18)? as i32;
    let line = axml_u32(data, pos + 8)?;

    let stride = attribute_size.max(20);
    let attrs_base = node
        .checked_add(attribute_start)
        .ok_or_else(|| axml("offset overflow"))?;
    let mut attributes = Vec::with_capacity(attribute_count.min(1024));
    for i in 0..attribute_count {
        let apos = attrs_base + i * stride;
        let a_ns = axml_u32(data, apos)? as i32;
        let a_name = axml_u32(data, apos + 4)? as i32;
        let a_raw = axml_u32(data, apos + 8)? as i32;
        let v_type = *data.get(apos + 15).ok_or_else(|| axml("short attribute"))?;
        let v_data = axml_u32(data, apos + 16)?;
        let resource_id = if a_name >= 0 {
            resource_ids
                .get(a_name as usize)
                .copied()
                .unwrap_or(0)
        } else {
            0
        };
        attributes.push(XmlAttribute {
            namespace_index: a_ns,
            name_index: a_name,
            raw_value_index: a_raw,
            value: ResValue {
                data_type: v_type,
                data: v_data,
            },
            resource_id,
        });
    }

    Ok(XmlTag {
        namespace_index,
        name_index,
        line,
        attributes,
        id_index: header_index_to_attr_index(id_index),
        class_index: header_index_to_attr_index(class_index),
        style_index: header_index_to_attr_index(style_index),
    })
}

/// The AXML header stores `idIndex`/`classIndex`/`styleIndex` as 1-based
/// attribute indexes; `0` means "absent". The native API returns 0-based
/// indexes with `-1` meaning absent.
fn header_index_to_attr_index(v: i32) -> i32 {
    if v == 0 {
        -1
    } else {
        v - 1
    }
}

/// One `XmlBlock.nativeCreateParseState` parse state.
#[derive(Debug, Clone)]
pub struct XmlParserState {
    /// The owning `XmlBlock` handle.
    pub block: u64,
    /// Index into the document event stream (0 = `StartDocument`).
    pub position: usize,
    /// Resource id passed to `nativeCreateParseState`.
    pub source_res_id: i32,
    /// Whether the synthetic `START_DOCUMENT` event has been returned.
    pub started: bool,
}

impl XmlParserState {
    /// Create a parse state over `block`.
    pub fn new(block: u64, source_res_id: i32) -> Self {
        XmlParserState {
            block,
            position: 0,
            source_res_id,
            started: false,
        }
    }

    /// The current event index.
    pub fn position(&self) -> usize {
        self.position
    }

    /// Mirror of the Java `XmlBlock.Parser.next()` (returns `START_DOCUMENT`
    /// on the first call).
    pub fn next_event(&mut self, doc: &XmlDocument) -> i32 {
        if !self.started {
            self.started = true;
            self.position = 0;
            return START_DOCUMENT;
        }
        self.native_next(doc)
    }

    /// Mirror of `XmlBlock.nativeNext`.
    pub fn native_next(&mut self, doc: &XmlDocument) -> i32 {
        if self.position + 1 >= doc.events.len() {
            self.position = doc.events.len().saturating_sub(1);
            return END_DOCUMENT;
        }
        self.position += 1;
        event_code(&doc.events[self.position])
    }

    /// The current event.
    pub fn current<'a>(&self, doc: &'a XmlDocument) -> Option<&'a XmlEvent> {
        doc.events.get(self.position)
    }

    /// The current event code.
    pub fn event_code(&self, doc: &XmlDocument) -> i32 {
        doc.events
            .get(self.position)
            .map(event_code)
            .unwrap_or(END_DOCUMENT)
    }

    /// `nativeGetName`.
    pub fn get_name(&self, doc: &XmlDocument) -> i32 {
        match doc.events.get(self.position) {
            Some(XmlEvent::StartTag(t)) => t.name_index,
            Some(XmlEvent::EndTag { name_index, .. }) => *name_index,
            _ => -1,
        }
    }

    /// `nativeGetNamespace`.
    pub fn get_namespace(&self, doc: &XmlDocument) -> i32 {
        match doc.events.get(self.position) {
            Some(XmlEvent::StartTag(t)) => t.namespace_index,
            Some(XmlEvent::EndTag {
                namespace_index, ..
            }) => *namespace_index,
            _ => -1,
        }
    }

    /// `nativeGetText`.
    pub fn get_text(&self, doc: &XmlDocument) -> i32 {
        match doc.events.get(self.position) {
            Some(XmlEvent::Text { text_index, .. }) => *text_index,
            _ => -1,
        }
    }

    /// `nativeGetLineNumber`.
    pub fn get_line_number(&self, doc: &XmlDocument) -> i32 {
        match doc.events.get(self.position) {
            Some(XmlEvent::StartTag(t)) => t.line as i32,
            Some(XmlEvent::EndTag { line, .. }) | Some(XmlEvent::Text { line, .. }) => {
                *line as i32
            }
            _ => -1,
        }
    }

    /// `nativeGetAttributeCount`.
    pub fn get_attribute_count(&self, doc: &XmlDocument) -> i32 {
        match doc.events.get(self.position) {
            Some(XmlEvent::StartTag(t)) => t.attributes.len() as i32,
            _ => -1,
        }
    }

    fn attribute<'a>(&self, doc: &'a XmlDocument, idx: i32) -> Option<&'a XmlAttribute> {
        match doc.events.get(self.position) {
            Some(XmlEvent::StartTag(t)) if idx >= 0 => t.attributes.get(idx as usize),
            _ => None,
        }
    }

    /// `nativeGetAttributeNamespace` (`-1` = none, `-2` = out of range).
    pub fn get_attribute_namespace(&self, doc: &XmlDocument, idx: i32) -> i32 {
        self.attribute(doc, idx)
            .map(|a| a.namespace_index)
            .unwrap_or(-2)
    }

    /// `nativeGetAttributeName`.
    pub fn get_attribute_name(&self, doc: &XmlDocument, idx: i32) -> i32 {
        self.attribute(doc, idx).map(|a| a.name_index).unwrap_or(-2)
    }

    /// `nativeGetAttributeResource`.
    pub fn get_attribute_resource(&self, doc: &XmlDocument, idx: i32) -> i32 {
        self.attribute(doc, idx)
            .map(|a| a.resource_id as i32)
            .unwrap_or(-2)
    }

    /// `nativeGetAttributeDataType`.
    pub fn get_attribute_data_type(&self, doc: &XmlDocument, idx: i32) -> i32 {
        self.attribute(doc, idx)
            .map(|a| a.value.data_type as i32)
            .unwrap_or(-2)
    }

    /// `nativeGetAttributeData`.
    pub fn get_attribute_data(&self, doc: &XmlDocument, idx: i32) -> i32 {
        self.attribute(doc, idx)
            .map(|a| a.value.data as i32)
            .unwrap_or(-2)
    }

    /// `nativeGetAttributeStringValue`.
    pub fn get_attribute_string_value(&self, doc: &XmlDocument, idx: i32) -> i32 {
        match self.attribute(doc, idx) {
            Some(a) if a.raw_value_index >= 0 => a.raw_value_index,
            Some(a) if a.value.data_type == crate::res::value::TYPE_STRING => a.value.data as i32,
            _ => -1,
        }
    }

    /// `nativeGetIdAttribute`.
    pub fn get_id_attribute(&self, doc: &XmlDocument) -> i32 {
        match doc.events.get(self.position) {
            Some(XmlEvent::StartTag(t)) => t.id_index,
            _ => -1,
        }
    }

    /// `nativeGetClassAttribute`.
    pub fn get_class_attribute(&self, doc: &XmlDocument) -> i32 {
        match doc.events.get(self.position) {
            Some(XmlEvent::StartTag(t)) => t.class_index,
            _ => -1,
        }
    }

    /// `nativeGetStyleAttribute`.
    pub fn get_style_attribute(&self, doc: &XmlDocument) -> i32 {
        match doc.events.get(self.position) {
            Some(XmlEvent::StartTag(t)) => t.style_index,
            _ => -1,
        }
    }

    /// `nativeGetSourceResId`.
    pub fn get_source_res_id(&self) -> i32 {
        self.source_res_id
    }

    /// `nativeGetAttributeIndex(namespace, name)`.
    pub fn get_attribute_index(
        &self,
        doc: &XmlDocument,
        namespace: Option<&str>,
        name: &str,
    ) -> i32 {
        let Some(XmlEvent::StartTag(t)) = doc.events.get(self.position) else {
            return -1;
        };
        for (i, a) in t.attributes.iter().enumerate() {
            let ns_matches = match namespace {
                None => a.namespace_index < 0,
                Some(ns) => {
                    doc.string_block.get(a.namespace_index).unwrap_or("") == ns
                }
            };
            if !ns_matches {
                continue;
            }
            if doc.string_block.get(a.name_index).unwrap_or("") == name {
                return i as i32;
            }
        }
        -1
    }

    /// The styled spans of the current text event.
    pub fn text_style<'a>(&self, doc: &'a XmlDocument) -> &'a [StyleSpan] {
        let idx = self.get_text(doc);
        doc.string_block.get_style(idx)
    }
}

fn event_code(event: &XmlEvent) -> i32 {
    match event {
        XmlEvent::StartDocument => START_DOCUMENT,
        XmlEvent::EndDocument => END_DOCUMENT,
        XmlEvent::StartTag(_) => START_TAG,
        XmlEvent::EndTag { .. } => END_TAG,
        XmlEvent::Text { .. } => TEXT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_magic() {
        let err = parse_axml(&[0u8; 8]).unwrap_err();
        assert_eq!(err.code(), RES_AXML);
    }
}
