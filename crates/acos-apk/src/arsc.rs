//! `resources.arsc` (`ResTable`) parser.
//!
//! Supports the common case: one or more packages, type/key string pools, type
//! specs, and simple + bag entries, including sparse type chunks and compact
//! entries. Config matching is best-effort: the default configuration is
//! preferred, then the first matching entry. This is documented as sufficient
//! for the current ACOS static-analysis fidelity.

use crate::axml::{complex_to_float, DIMENSION_UNITS, FRACTION_UNITS};
use crate::binary::{read_u16, read_u32, StringPool};
use crate::error::{ApkError, Result, ARSC_MALFORMED};

const RES_TABLE_TYPE: u16 = 0x0002;
const RES_TABLE_PACKAGE_TYPE: u16 = 0x0200;
const RES_TABLE_TYPE_TYPE: u16 = 0x0201;
const RES_TABLE_TYPE_SPEC_TYPE: u16 = 0x0202;

const ENTRY_FLAG_COMPLEX: u16 = 0x0001;
const ENTRY_FLAG_COMPACT: u16 = 0x0008;
const NO_ENTRY: u32 = 0xFFFF_FFFF;

fn malformed(msg: impl Into<String>) -> ApkError {
    ApkError::new(ARSC_MALFORMED, msg)
}

/// A decoded resource value.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResValue {
    Null,
    Reference(u32),
    Attribute(u32),
    String(String),
    Float(f32),
    Dimension {
        value: f32,
        unit: String,
    },
    Fraction {
        value: f32,
        unit: String,
    },
    IntDec(i32),
    IntHex(u32),
    Boolean(bool),
    Color(u32),
    /// A bag/map entry (e.g. style, array): `(name_resource_id, value)` pairs.
    Bag(Vec<(u32, ResValue)>),
    Other {
        data_type: u8,
        data: u32,
    },
}

impl ResValue {
    /// A compact human-readable rendering.
    pub fn display(&self) -> String {
        match self {
            ResValue::Null => "null".to_string(),
            ResValue::Reference(r) => format!("@{r:#010x}"),
            ResValue::Attribute(a) => format!("?{a:#010x}"),
            ResValue::String(s) => s.clone(),
            ResValue::Float(f) => f.to_string(),
            ResValue::Dimension { value, unit } => format!("{value}{unit}"),
            ResValue::Fraction { value, unit } => format!("{value}{unit}"),
            ResValue::IntDec(i) => i.to_string(),
            ResValue::IntHex(h) => format!("0x{h:x}"),
            ResValue::Boolean(b) => b.to_string(),
            ResValue::Color(c) => format!("#{c:08x}"),
            ResValue::Bag(items) => format!("<bag of {}>", items.len()),
            ResValue::Other { data_type, data } => format!("<type {data_type:#04x}>:{data:#010x}"),
        }
    }
}

/// A resource configuration (locale/density subset parsed; raw bytes retained).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Config {
    pub language: String,
    pub country: String,
    pub density: u16,
    #[serde(skip)]
    pub raw: Vec<u8>,
}

impl Config {
    /// The default configuration: no locale and no density.
    pub fn is_default(&self) -> bool {
        self.language.is_empty() && self.country.is_empty() && self.density == 0
    }

    /// A short label such as `""`, `"en"`, `"en-rUS"`, `"xxhdpi"`.
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if !self.language.is_empty() {
            if self.country.is_empty() {
                parts.push(self.language.clone());
            } else {
                parts.push(format!("{}-r{}", self.language, self.country));
            }
        }
        if self.density != 0 {
            parts.push(format!("{}dpi", self.density));
        }
        parts.join("-")
    }
}

/// A single resource entry.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResourceEntry {
    pub id: u32,
    pub type_id: u8,
    pub entry_id: u16,
    pub type_name: String,
    pub key: String,
    pub config: Config,
    pub value: ResValue,
}

/// A `ResTable_typeSpec` declaration (configuration flags per entry).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TypeSpec {
    pub type_id: u8,
    pub entry_count: u32,
}

/// A resource package (`ResTable_package`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResourcePackage {
    pub id: u32,
    pub name: String,
    pub type_names: Vec<String>,
    pub key_names: Vec<String>,
    pub type_specs: Vec<TypeSpec>,
    pub entries: Vec<ResourceEntry>,
}

/// A decoded `resources.arsc`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResourceTable {
    pub packages: Vec<ResourcePackage>,
    pub global_strings: Vec<String>,
}

impl ResourceTable {
    /// Parse a `resources.arsc` byte slice.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let chunk_type = read_u16(data, 0)?;
        if chunk_type != RES_TABLE_TYPE {
            return Err(malformed(format!(
                "not a resource table (chunk type {chunk_type:#06x})"
            )));
        }
        let header_size = read_u16(data, 2)? as usize;
        let total_size = read_u32(data, 4)? as usize;
        let package_count = read_u32(data, 8)? as usize;
        let end = total_size.min(data.len());
        if header_size < 12 || header_size > end {
            return Err(malformed("resource table header out of range"));
        }

        let global = if header_size + 8 <= end {
            StringPool::parse(data, header_size)?
        } else {
            StringPool::default()
        };
        let pool_size = read_u32(data, header_size + 4)? as usize;
        let mut pos = header_size.saturating_add(pool_size);
        if pos > end {
            pos = end;
        }

        let mut packages = Vec::new();
        while pos + 8 <= end && packages.len() < package_count.max(64) {
            let ct = read_u16(data, pos)?;
            let size = read_u32(data, pos + 4)? as usize;
            if size < 8 || pos.saturating_add(size) > end {
                return Err(malformed(format!("bad package chunk size {size}")));
            }
            if ct == RES_TABLE_PACKAGE_TYPE {
                packages.push(parse_package(data, pos, &global)?);
            }
            pos = pos.saturating_add(size);
        }

        Ok(ResourceTable {
            packages,
            global_strings: global.strings().to_vec(),
        })
    }

    /// Resolve a resource id to its best-matching entry, preferring the default
    /// configuration and otherwise the first match.
    pub fn resolve(&self, id: u32) -> Option<&ResourceEntry> {
        self.resolve_with_config(id, &Config::default())
    }

    /// Best-effort config matching: an exact [`Config`] match wins, then the
    /// default configuration, then the first entry found. (Full AAPT config
    /// precedence is out of scope for static analysis.)
    pub fn resolve_with_config(&self, id: u32, preferred: &Config) -> Option<&ResourceEntry> {
        let pkg_id = id >> 24;
        let type_id = ((id >> 16) & 0xff) as u8;
        let entry_id = (id & 0xffff) as u16;
        let pkg = self.packages.iter().find(|p| p.id == pkg_id)?;
        let mut exact: Option<&ResourceEntry> = None;
        let mut default: Option<&ResourceEntry> = None;
        let mut first: Option<&ResourceEntry> = None;
        for e in &pkg.entries {
            if e.type_id == type_id && e.entry_id == entry_id {
                if first.is_none() {
                    first = Some(e);
                }
                if e.config == *preferred {
                    exact = Some(e);
                }
                if e.config.is_default() && default.is_none() {
                    default = Some(e);
                }
            }
        }
        exact.or(default).or(first)
    }

    /// Resolve a resource id to a string, following at most 16 references.
    pub fn resolve_string(&self, id: u32) -> Option<String> {
        self.resolve_string_inner(id, 0)
    }

    fn resolve_string_inner(&self, id: u32, depth: usize) -> Option<String> {
        if depth > 16 {
            return None;
        }
        let entry = self.resolve(id)?;
        match &entry.value {
            ResValue::String(s) => Some(s.clone()),
            ResValue::Reference(r) => self.resolve_string_inner(*r, depth + 1),
            _ => None,
        }
    }

    /// Summary of resource types, `(type_name, entry_count)` sorted by name.
    pub fn type_summary(&self) -> Vec<(String, usize)> {
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for pkg in &self.packages {
            for e in &pkg.entries {
                *counts.entry(e.type_name.clone()).or_insert(0) += 1;
            }
        }
        counts.into_iter().collect()
    }

    /// Total number of entries across all packages.
    pub fn entry_count(&self) -> usize {
        self.packages.iter().map(|p| p.entries.len()).sum()
    }
}

fn parse_package(data: &[u8], start: usize, global: &StringPool) -> Result<ResourcePackage> {
    let header_size = read_u16(data, start + 2)? as usize;
    let size = read_u32(data, start + 4)? as usize;
    let id = read_u32(data, start + 8)?;
    let name = read_utf16_fixed(data, start + 12, 128)?;
    let type_strings_off = read_u32(data, start + 0x10c)? as usize;
    let key_strings_off = read_u32(data, start + 0x114)? as usize;
    let type_id_offset = if header_size >= 0x120 {
        read_u32(data, start + 0x11c).unwrap_or(0)
    } else {
        0
    };

    let type_pool = if type_strings_off != 0 {
        StringPool::parse(data, start + type_strings_off)?
    } else {
        StringPool::default()
    };
    let key_pool = if key_strings_off != 0 {
        StringPool::parse(data, start + key_strings_off)?
    } else {
        StringPool::default()
    };

    let end = start.saturating_add(size).min(data.len());
    let mut pos = start.saturating_add(header_size.max(8));
    let mut entries = Vec::new();
    let mut type_specs = Vec::new();
    while pos + 8 <= end {
        let ct = read_u16(data, pos)?;
        let chunk_size = read_u32(data, pos + 4)? as usize;
        if chunk_size < 8 || pos.saturating_add(chunk_size) > end {
            return Err(malformed(format!("bad resource chunk size {chunk_size}")));
        }
        if ct == RES_TABLE_TYPE_TYPE {
            parse_type_chunk(
                data,
                pos,
                chunk_size,
                id,
                type_id_offset,
                &type_pool,
                &key_pool,
                global,
                &mut entries,
            )?;
        } else if ct == RES_TABLE_TYPE_SPEC_TYPE {
            let type_id = *data
                .get(pos + 8)
                .ok_or_else(|| malformed("type spec truncated"))?;
            let entry_count = read_u32(data, pos + 12)?;
            type_specs.push(TypeSpec {
                type_id,
                entry_count,
            });
        }
        pos = pos.saturating_add(chunk_size);
    }

    Ok(ResourcePackage {
        id,
        name,
        type_names: type_pool.strings().to_vec(),
        key_names: key_pool.strings().to_vec(),
        type_specs,
        entries,
    })
}

#[allow(clippy::too_many_arguments)]
fn parse_type_chunk(
    data: &[u8],
    pos: usize,
    _chunk_size: usize,
    pkg_id: u32,
    type_id_offset: u32,
    type_pool: &StringPool,
    key_pool: &StringPool,
    global: &StringPool,
    out: &mut Vec<ResourceEntry>,
) -> Result<()> {
    let header_size = read_u16(data, pos + 2)? as usize;
    let type_id = *data
        .get(pos + 8)
        .ok_or_else(|| malformed("type chunk truncated"))?;
    let flags = *data
        .get(pos + 9)
        .ok_or_else(|| malformed("type chunk truncated"))?;
    let entry_count = read_u32(data, pos + 12)? as usize;
    let entries_start = read_u32(data, pos + 16)? as usize;

    let type_index = (type_id as usize)
        .saturating_sub(1)
        .saturating_add(type_id_offset as usize);
    let type_name = type_pool
        .strings()
        .get(type_index)
        .cloned()
        .unwrap_or_else(|| format!("type_{type_id}"));

    // Config begins after the fixed fields (8 header + 1+1+2+4+4 = 20).
    let config_at = pos + 20;
    let config_size = read_u32(data, config_at)? as usize;
    let config = parse_config(data, config_at, config_size)?;

    let offsets_at = pos.saturating_add(header_size.max(20));
    let entries_at = pos.saturating_add(entries_start);
    let sparse = flags & 0x01 != 0;

    let mut push_entry = |entry_id: u16, entry_at: usize| -> Result<()> {
        let entry = parse_entry(
            data, entry_at, entry_id, pkg_id, type_id, &type_name, &config, key_pool, global,
        )?;
        out.push(entry);
        Ok(())
    };

    if sparse {
        for i in 0..entry_count {
            let at = offsets_at + i * 4;
            let idx = read_u16(data, at)?;
            let off = read_u16(data, at + 2)? as usize * 4;
            push_entry(idx, entries_at + off)?;
        }
    } else {
        for i in 0..entry_count {
            let off = read_u32(data, offsets_at + i * 4)?;
            if off == NO_ENTRY {
                continue;
            }
            push_entry(i as u16, entries_at + off as usize)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn parse_entry(
    data: &[u8],
    at: usize,
    entry_id: u16,
    pkg_id: u32,
    type_id: u8,
    type_name: &str,
    config: &Config,
    key_pool: &StringPool,
    global: &StringPool,
) -> Result<ResourceEntry> {
    let first = read_u16(data, at)?;
    let flags = read_u16(data, at + 2)?;
    let complex = flags & ENTRY_FLAG_COMPLEX != 0;
    let compact = flags & ENTRY_FLAG_COMPACT != 0;

    let (key_index, value) = if compact {
        // Compact entry (Android S+): [key:u16][flags:u16][data:u32] where the
        // data type is stored in the high byte of `flags`.
        let data_type = (flags >> 8) as u8;
        let raw = read_u32(data, at + 4)?;
        (first as u32, decode_res_value(global, data_type, raw))
    } else {
        let key = read_u32(data, at + 4)?;
        if complex {
            let _parent = read_u32(data, at + 8)?;
            let count = read_u32(data, at + 12)? as usize;
            let mut map = Vec::with_capacity(count.min(4096));
            for i in 0..count {
                let m = at
                    .checked_add(16)
                    .and_then(|v| v.checked_add(i * 12))
                    .ok_or_else(|| malformed("bag offset overflow"))?;
                let name = read_u32(data, m)?;
                let (dt, raw) = read_res_value(data, m + 4)?;
                map.push((name, decode_res_value(global, dt, raw)));
            }
            (key, ResValue::Bag(map))
        } else {
            let (dt, raw) = read_res_value(data, at + 8)?;
            (key, decode_res_value(global, dt, raw))
        }
    };

    let key = key_pool
        .strings()
        .get(key_index as usize)
        .cloned()
        .unwrap_or_default();

    let id = (pkg_id << 24) | ((type_id as u32) << 16) | (entry_id as u32);

    Ok(ResourceEntry {
        id,
        type_id,
        entry_id,
        type_name: type_name.to_string(),
        key,
        config: config.clone(),
        value,
    })
}

fn read_res_value(data: &[u8], at: usize) -> Result<(u8, u32)> {
    let _size = read_u16(data, at)?;
    let data_type = *data
        .get(at + 3)
        .ok_or_else(|| malformed("short Res_value"))?;
    let raw = read_u32(data, at + 4)?;
    Ok((data_type, raw))
}

fn decode_res_value(pool: &StringPool, data_type: u8, data: u32) -> ResValue {
    match data_type {
        0x00 => ResValue::Null,
        0x01 | 0x07 => ResValue::Reference(data),
        0x02 => ResValue::Attribute(data),
        0x03 => ResValue::String(pool.get_or_empty(data).to_string()),
        0x04 => ResValue::Float(f32::from_bits(data)),
        0x05 => {
            let (value, unit) = complex_to_float(data, &DIMENSION_UNITS);
            ResValue::Dimension { value, unit }
        }
        0x06 => {
            let (value, unit) = complex_to_float(data, &FRACTION_UNITS);
            ResValue::Fraction { value, unit }
        }
        0x10 => ResValue::IntDec(data as i32),
        0x11 => ResValue::IntHex(data),
        0x12 => ResValue::Boolean(data != 0),
        0x1c..=0x1f => ResValue::Color(data),
        other => ResValue::Other {
            data_type: other,
            data,
        },
    }
}

fn parse_config(data: &[u8], at: usize, size: usize) -> Result<Config> {
    let raw = data
        .get(at..at.saturating_add(size))
        .ok_or_else(|| malformed("config out of range"))?
        .to_vec();
    let mut c = std::io::Cursor::new(raw.as_slice());
    use std::io::Read;
    let mut buf = [0u8; 16];
    let default = Config {
        language: String::new(),
        country: String::new(),
        density: 0,
        raw: raw.clone(),
    };
    // Need at least: size(4) imsi(4) locale(4) screenType(4).
    if c.read_exact(&mut buf).is_err() {
        return Ok(default);
    }
    let l0 = buf[8];
    let l1 = buf[9];
    let c0 = buf[10];
    let c1 = buf[11];
    let density = u16::from_le_bytes([buf[14], buf[15]]);
    Ok(Config {
        language: decode_locale_part(l0, l1),
        country: decode_locale_part(c0, c1),
        density,
        raw,
    })
}

fn decode_locale_part(a: u8, b: u8) -> String {
    if a == 0 || b == 0 {
        return String::new();
    }
    let mut s = String::new();
    s.push(a as char);
    s.push(b as char);
    s
}

fn read_utf16_fixed(data: &[u8], at: usize, units: usize) -> Result<String> {
    let mut out = Vec::with_capacity(units);
    for i in 0..units {
        let v = read_u16(data, at + i * 2)?;
        if v == 0 {
            break;
        }
        out.push(v);
    }
    Ok(String::from_utf16_lossy(&out))
}
