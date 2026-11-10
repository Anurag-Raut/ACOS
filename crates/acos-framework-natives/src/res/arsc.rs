//! Lazily-parsed `resources.arsc` (`ResTable` / `ResTable_package` /
//! `ResTable_type`).
//!
//! Parsing reads only the table header, package headers, type/key string-pool
//! headers, and the *offset* of every type chunk. Nothing else is
//! materialised: each `ResTable_type` header (and its `ResTable_config`) and
//! every entry are decoded on demand from the mapped bytes. A 16 MB
//! `resources.arsc` therefore costs a few tens of KiB of index.
//!
//! [`ResTableIndex`] holds no borrows: every accessor takes the backing
//! `&[u8]`, which keeps it usable from a struct that also owns the mmap.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::res::bytes::{u16_at, u32_at, utf16_fixed};
use crate::res::config::ResConfig;
use crate::res::error::{ResError, Result, RES_ARSC, RES_NO_PACKAGE, RES_NOT_FOUND};
use crate::res::strpool::StringPool;
use crate::res::value::ResValue;

const RES_TABLE_TYPE: u16 = 0x0002;
const RES_TABLE_PACKAGE_TYPE: u16 = 0x0200;
const RES_TABLE_TYPE_TYPE: u16 = 0x0201;
const RES_TABLE_TYPE_SPEC_TYPE: u16 = 0x0202;

const ENTRY_FLAG_COMPLEX: u16 = 0x0001;
const ENTRY_FLAG_COMPACT: u16 = 0x0008;
const TYPE_FLAG_SPARSE: u8 = 0x01;
const TYPE_FLAG_OFFSET16: u8 = 0x02;
const NO_ENTRY: u32 = 0xFFFF_FFFF;

/// `SPEC_PUBLIC` flag on a type-spec entry.
pub const SPEC_PUBLIC: u32 = 0x4000_0000;
/// `SPEC_WEAK` flag on a type-spec entry.
pub const SPEC_WEAK: u32 = 0x8000_0000;

fn arsc(msg: impl Into<String>) -> ResError {
    ResError::new(RES_ARSC, msg)
}

/// A parsed resource table index (no borrows).
#[derive(Debug, Clone)]
pub struct ResTableIndex {
    /// Resource packages.
    pub packages: Vec<PackageIndex>,
    /// Offset of the global value string pool.
    global_strings_at: usize,
    /// Cache of `(type id, entry id, requested config) -> winning chunk offset`.
    best_chunk: RefCell<HashMap<(u8, u16, ResConfig), Option<u32>>>,
}

/// A parsed package.
#[derive(Debug, Clone)]
pub struct PackageIndex {
    /// Package id (the high byte of every resource id).
    pub id: u32,
    /// Package name.
    pub name: String,
    /// Offset of the type string pool.
    pub type_strings_at: usize,
    /// Offset of the key string pool.
    pub key_strings_at: usize,
    /// `type_id_offset` added to the chunk type id when indexing types.
    pub type_id_offset: u32,
    /// Type chunk start offsets, grouped by the declared type id at byte 8.
    pub chunk_offsets: Vec<Vec<u32>>,
    /// Start offset of the type-spec chunk for each type id.
    pub spec_at: Vec<Option<u32>>,
}

/// One `ResTable_type` chunk, parsed on demand.
#[derive(Debug, Clone)]
pub struct TypeChunk {
    /// Chunk start offset in the backing bytes.
    pub chunk_at: usize,
    /// Declared type id (byte 8).
    pub type_id: u8,
    /// Type chunk flags (`sparse` / `offset16`).
    pub flags: u8,
    /// Number of entries.
    pub entry_count: usize,
    /// Absolute offset of the offsets table.
    pub offsets_at: usize,
    /// Absolute offset of the entry data.
    pub entries_at: usize,
    /// Parsed configuration.
    pub config: ResConfig,
}

/// A complex/bag entry (style, array, map).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bag {
    /// Parent bag resource id (`0` when none).
    pub parent: u32,
    /// `(name_resource_id, value)` pairs.
    pub entries: Vec<(u32, ResValue)>,
}

/// A resolved resource entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEntry {
    /// The value (simple entries) or `NULL` (complex entries).
    pub value: ResValue,
    /// The bag when the entry is complex.
    pub bag: Option<Bag>,
    /// The configuration the value came from.
    pub config: ResConfig,
    /// `toString()` label of that configuration.
    pub config_label: String,
    /// Type-spec change flags for this entry.
    pub changing_configurations: u32,
    /// Package name.
    pub package_name: String,
    /// Type name (`string`, `layout`, ...).
    pub type_name: String,
    /// Entry key name.
    pub key: String,
    /// Entry id (low 16 bits).
    pub entry_id: u16,
    /// Type id (bits 16..24).
    pub type_id: u8,
}

/// A simple `(config label, value)` pair used by differential tooling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValue {
    /// Config label.
    pub config_label: String,
    /// The decoded value.
    pub value: ResValue,
    /// The bag, if complex.
    pub bag: Option<Bag>,
}

impl ResTableIndex {
    /// Parse the table header, packages, and per-type chunk offsets.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let chunk_type = u16_at(data, 0)?;
        if chunk_type != RES_TABLE_TYPE {
            return Err(arsc(format!(
                "not a resource table (chunk type {chunk_type:#06x})"
            )));
        }
        let header_size = u16_at(data, 2)? as usize;
        let total_size = u32_at(data, 4)? as usize;
        let package_count = u32_at(data, 8)? as usize;
        let end = total_size.min(data.len());
        if header_size < 12 || header_size > end {
            return Err(arsc("resource table header out of range"));
        }
        let global_strings_at = header_size;
        let pool_size = u32_at(data, header_size + 4).unwrap_or(0) as usize;
        let mut pos = header_size.saturating_add(pool_size);
        if pos > end || pos < header_size {
            pos = end;
        }
        let mut packages = Vec::new();
        while pos + 8 <= end && packages.len() <= package_count.max(64) {
            let ct = u16_at(data, pos)?;
            let size = u32_at(data, pos + 4)? as usize;
            if size < 8 || pos.saturating_add(size) > end {
                return Err(arsc(format!("bad package chunk size {size} at {pos}")));
            }
            if ct == RES_TABLE_PACKAGE_TYPE {
                packages.push(parse_package(data, pos)?);
            }
            pos = pos.saturating_add(size);
        }
        Ok(ResTableIndex {
            packages,
            global_strings_at,
            best_chunk: RefCell::new(HashMap::new()),
        })
    }

    /// The global value string pool.
    pub fn global_strings<'a>(&self, data: &'a [u8]) -> Result<StringPool<'a>> {
        StringPool::parse(data, self.global_strings_at)
    }

    /// Decode a global string-pool value.
    pub fn global_string(&self, data: &[u8], idx: u32) -> Option<String> {
        let pool = self.global_strings(data).ok()?;
        if idx == u32::MAX {
            None
        } else {
            pool.get(idx as i32)
        }
    }

    /// Package by id.
    pub fn package(&self, pkg_id: u32) -> Option<&PackageIndex> {
        self.packages.iter().find(|p| p.id == pkg_id)
    }

    /// The package id of a resource id.
    pub fn package_id(id: u32) -> u32 {
        id >> 24
    }

    /// The type id of a resource id.
    pub fn type_id(id: u32) -> u8 {
        ((id >> 16) & 0xff) as u8
    }

    /// The entry id of a resource id.
    pub fn entry_id(id: u32) -> u16 {
        (id & 0xffff) as u16
    }

    /// The type name of a resource id (e.g. `string`).
    pub fn type_name(&self, data: &[u8], id: u32) -> Option<String> {
        let pkg = self.package(Self::package_id(id))?;
        pkg.type_name(data, Self::type_id(id)).ok()
    }

    /// The package name of a resource id.
    pub fn package_name(&self, id: u32) -> Option<String> {
        self.package(Self::package_id(id)).map(|p| p.name.clone())
    }

    /// The entry key name of a resource id.
    pub fn entry_name(&self, data: &[u8], id: u32) -> Option<String> {
        let pkg = self.package(Self::package_id(id))?;
        let type_id = Self::type_id(id);
        let entry_id = Self::entry_id(id);
        let key_index = pkg
            .chunk_offsets
            .get(type_id as usize)?
            .iter()
            .find_map(|off| {
                let chunk = parse_type_chunk(data, *off as usize).ok()?;
                let off = lookup_entry_offset(data, &chunk, entry_id)?;
                decode_entry(data, chunk.entries_at + off)
                    .ok()
                    .map(|e| e.key_index)
            })?;
        pkg.key_name(data, key_index).ok()
    }

    /// Resolve `id` against `requested` using the full Android config matcher.
    pub fn resolve(&self, data: &[u8], id: u32, requested: &ResConfig) -> Option<ResolvedEntry> {
        let pkg = self.package(Self::package_id(id))?;
        let type_id = Self::type_id(id);
        let entry_id = Self::entry_id(id);
        let offsets = pkg.chunk_offsets.get(type_id as usize)?;
        let winner = self.best_chunk_offset(data, type_id, entry_id, offsets, requested);
        let chunk = parse_type_chunk(data, winner? as usize).ok()?;
        let off = lookup_entry_offset(data, &chunk, entry_id)?;
        let entry = decode_entry(data, chunk.entries_at + off).ok()?;
        Some(ResolvedEntry {
            value: entry.value,
            bag: entry.bag,
            config: chunk.config.clone(),
            config_label: chunk.config.label(),
            changing_configurations: pkg.spec_flag(data, type_id, entry_id),
            package_name: pkg.name.clone(),
            type_name: pkg.type_name(data, type_id).unwrap_or_default(),
            key: pkg.key_name(data, entry.key_index).unwrap_or_default(),
            entry_id,
            type_id,
        })
    }

    /// Find the offset of the best-matching chunk for `(type_id, requested)`,
    /// caching the result per type/config pair.
    fn best_chunk_offset(
        &self,
        data: &[u8],
        type_id: u8,
        entry_id: u16,
        offsets: &[u32],
        requested: &ResConfig,
    ) -> Option<u32> {
        let key = (type_id, entry_id, requested.clone());
        if let Some(hit) = self.best_chunk.borrow().get(&key) {
            return *hit;
        }
        let mut best: Option<(u32, ResConfig)> = None;
        for off in offsets {
            let Ok(chunk) = parse_type_chunk(data, *off as usize) else {
                continue;
            };
            if !chunk.config.matches(requested) {
                continue;
            }
            // A config only competes when this entry actually exists in it.
            if lookup_entry_offset(data, &chunk, entry_id).is_none() {
                continue;
            }
            let better = match &best {
                None => true,
                Some((_, cur)) => chunk.config.is_better_than(cur, requested),
            };
            if better {
                best = Some((*off, chunk.config));
            }
        }
        let result = best.map(|(off, _)| off);
        self.best_chunk.borrow_mut().insert(key, result);
        result
    }

    /// Resolve a bag entry by its name resource id.
    pub fn resolve_bag_entry(
        &self,
        data: &[u8],
        id: u32,
        bag_entry_id: u32,
        requested: &ResConfig,
    ) -> Option<ResolvedEntry> {
        let mut entry = self.resolve(data, id, requested)?;
        let bag = entry.bag.clone()?;
        let (_, value) = bag.entries.into_iter().find(|(n, _)| *n == bag_entry_id)?;
        entry.value = value;
        entry.bag = None;
        entry.changing_configurations = 0;
        Some(entry)
    }

    /// Every configuration variant of `id` (for differential tooling).
    pub fn all_configs(&self, data: &[u8], id: u32) -> Vec<ConfigValue> {
        let Some(pkg) = self.package(Self::package_id(id)) else {
            return Vec::new();
        };
        let type_id = Self::type_id(id);
        let entry_id = Self::entry_id(id);
        let Some(offsets) = pkg.chunk_offsets.get(type_id as usize) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for off in offsets {
            let Ok(chunk) = parse_type_chunk(data, *off as usize) else {
                continue;
            };
            let Some(off) = lookup_entry_offset(data, &chunk, entry_id) else {
                continue;
            };
            let Ok(entry) = decode_entry(data, chunk.entries_at + off) else {
                continue;
            };
            out.push(ConfigValue {
                config_label: chunk.config.label(),
                value: entry.value,
                bag: entry.bag,
            });
        }
        out
    }

    /// Resolve `id` to a bag (complex entry) and return its `(name, value)`
    /// pairs plus the array parent.
    pub fn array_entries(
        &self,
        data: &[u8],
        id: u32,
        requested: &ResConfig,
    ) -> Option<(u32, Vec<(u32, ResValue)>)> {
        let entry = self.resolve(data, id, requested)?;
        let bag = entry.bag?;
        Some((bag.parent, bag.entries))
    }

    /// The attr ids of a style bag's entries.
    pub fn style_attributes(
        &self,
        data: &[u8],
        id: u32,
        requested: &ResConfig,
    ) -> Option<Vec<u32>> {
        let bag = self.resolve(data, id, requested)?.bag?;
        Some(bag.entries.into_iter().map(|(n, _)| n).collect())
    }

    /// Reverse lookup: name (optionally `type/name`) to a resource id.
    pub fn identifier(
        &self,
        data: &[u8],
        name: &str,
        def_type: Option<&str>,
        def_package: Option<&str>,
    ) -> Option<u32> {
        let (type_part, name_part) = match name.split_once('/') {
            Some((t, n)) => (Some(t), n),
            None => (def_type, name),
        };
        let type_part = type_part?;
        for pkg in &self.packages {
            if let Some(dp) = def_package {
                if pkg.name != dp {
                    continue;
                }
            }
            let Ok(types) = pkg.type_pool(data) else {
                continue;
            };
            let Ok(keys) = pkg.key_pool(data) else {
                continue;
            };
            for type_idx in 0..types.len() {
                let Some(type_name) = types.get(type_idx as i32) else {
                    continue;
                };
                if type_name != type_part {
                    continue;
                }
                let type_id = (type_idx as u32 + 1 + pkg.type_id_offset) as u8;
                for key_idx in 0..keys.len() {
                    let Some(key_name) = keys.get(key_idx as i32) else {
                        continue;
                    };
                    if key_name != name_part {
                        continue;
                    }
                    if let Some(entry_id) = pkg.find_entry_by_key(data, type_id, &key_name) {
                        return Some((pkg.id << 24) | ((type_id as u32) << 16) | entry_id as u32);
                    }
                }
            }
        }
        None
    }

    /// Distinct configurations that carry a size qualifier.
    pub fn size_configurations(&self, data: &[u8]) -> Vec<ResConfig> {
        let mut seen: Vec<ResConfig> = Vec::new();
        for pkg in &self.packages {
            for offsets in &pkg.chunk_offsets {
                for off in offsets {
                    if let Ok(chunk) = parse_type_chunk(data, *off as usize) {
                        let c = &chunk.config;
                        let has_size = c.smallest_screen_width_dp != 0
                            || c.screen_width_dp != 0
                            || c.screen_height_dp != 0
                            || c.screen_width != 0
                            || c.screen_height != 0;
                        if has_size && !seen.contains(c) {
                            seen.push(c.clone());
                        }
                    }
                }
            }
        }
        seen
    }

    /// All type chunk configurations (used for `getSizeAndUiModeConfigurations`).
    pub fn all_configurations(&self, data: &[u8]) -> Vec<ResConfig> {
        let mut seen: Vec<ResConfig> = Vec::new();
        for pkg in &self.packages {
            for offsets in &pkg.chunk_offsets {
                for off in offsets {
                    if let Ok(chunk) = parse_type_chunk(data, *off as usize) {
                        if !seen.contains(&chunk.config) {
                            seen.push(chunk.config);
                        }
                    }
                }
            }
        }
        seen
    }

    /// The parent theme resource id of a style, if declared.
    pub fn parent_theme_identifier(
        &self,
        data: &[u8],
        id: u32,
        requested: &ResConfig,
    ) -> Option<u32> {
        let entry = self.resolve(data, id, requested)?;
        let bag = entry.bag?;
        if bag.parent != 0 {
            Some(bag.parent)
        } else {
            None
        }
    }

    /// True when the package's `resources.arsc` has any entries at all.
    pub fn is_empty(&self) -> bool {
        self.packages
            .iter()
            .all(|p| p.chunk_offsets.iter().all(Vec::is_empty))
    }
}

impl PackageIndex {
    /// Parse a type string pool.
    pub fn type_pool<'a>(&self, data: &'a [u8]) -> Result<StringPool<'a>> {
        StringPool::parse(data, self.type_strings_at)
    }

    /// Parse a key string pool.
    pub fn key_pool<'a>(&self, data: &'a [u8]) -> Result<StringPool<'a>> {
        StringPool::parse(data, self.key_strings_at)
    }

    /// Resolve a type id (declared byte) to its type name.
    pub fn type_name(&self, data: &[u8], type_id: u8) -> Result<String> {
        let pool = self.type_pool(data)?;
        let index = (type_id as usize)
            .saturating_sub(1)
            .saturating_add(self.type_id_offset as usize);
        Ok(pool.get(index as i32).unwrap_or_default())
    }

    /// Decode an entry key name.
    pub fn key_name(&self, data: &[u8], key_index: u32) -> Result<String> {
        let pool = self.key_pool(data)?;
        Ok(pool.get(key_index as i32).unwrap_or_default())
    }

    /// Type-spec change flags for `entry_id` (`0` when absent).
    pub fn spec_flag(&self, data: &[u8], type_id: u8, entry_id: u16) -> u32 {
        let Some(Some(spec_at)) = self.spec_at.get(type_id as usize) else {
            return 0;
        };
        let spec_at = *spec_at as usize;
        let Ok(header_size) = u16_at(data, spec_at + 2) else {
            return 0;
        };
        u32_at(data, spec_at + header_size as usize + entry_id as usize * 4).unwrap_or(0)
    }

    fn find_entry_by_key(&self, data: &[u8], type_id: u8, key: &str) -> Option<u16> {
        let offsets = self.chunk_offsets.get(type_id as usize)?;
        let key_pool = self.key_pool(data).ok()?;
        for off in offsets {
            let Ok(chunk) = parse_type_chunk(data, *off as usize) else {
                continue;
            };
            for i in 0..chunk.entry_count {
                let Some(off) = lookup_entry_offset(data, &chunk, i as u16) else {
                    continue;
                };
                let Ok(entry) = decode_entry(data, chunk.entries_at + off) else {
                    continue;
                };
                if key_pool.get(entry.key_index as i32).as_deref() == Some(key) {
                    return Some(i as u16);
                }
            }
        }
        None
    }
}

fn parse_package(data: &[u8], start: usize) -> Result<PackageIndex> {
    let header_size = u16_at(data, start + 2)? as usize;
    let size = u32_at(data, start + 4)? as usize;
    let id = u32_at(data, start + 8)?;
    let name = utf16_fixed(data, start + 12, 128)?;
    let type_strings_off = u32_at(data, start + 0x10c)? as usize;
    let key_strings_off = u32_at(data, start + 0x114)? as usize;
    let type_id_offset = if header_size >= 0x120 {
        u32_at(data, start + 0x11c).unwrap_or(0)
    } else {
        0
    };
    if type_strings_off == 0 || key_strings_off == 0 {
        return Err(arsc("package missing string pools"));
    }
    StringPool::parse(data, start + type_strings_off)?;
    StringPool::parse(data, start + key_strings_off)?;

    let end = start.saturating_add(size).min(data.len());
    let mut pos = start.saturating_add(header_size.max(8));
    let mut chunk_offsets: Vec<Vec<u32>> = vec![Vec::new(); 256];
    let mut spec_at: Vec<Option<u32>> = vec![None; 256];
    while pos + 8 <= end {
        let ct = u16_at(data, pos)?;
        let chunk_size = u32_at(data, pos + 4)? as usize;
        if chunk_size < 8 || pos.saturating_add(chunk_size) > end {
            return Err(arsc(format!("bad resource chunk size {chunk_size} at {pos}")));
        }
        if ct == RES_TABLE_TYPE_TYPE {
            if let Ok(chunk) = parse_type_chunk(data, pos) {
                if (chunk.type_id as usize) < chunk_offsets.len() {
                    chunk_offsets[chunk.type_id as usize].push(pos as u32);
                }
            }
        } else if ct == RES_TABLE_TYPE_SPEC_TYPE {
            if let Ok(type_id) = data
                .get(pos + 8)
                .copied()
                .ok_or_else(|| arsc("type spec truncated"))
            {
                if (type_id as usize) < spec_at.len() {
                    spec_at[type_id as usize] = Some(pos as u32);
                }
            }
        }
        pos = pos.saturating_add(chunk_size);
    }
    Ok(PackageIndex {
        id,
        name,
        type_strings_at: start + type_strings_off,
        key_strings_at: start + key_strings_off,
        type_id_offset,
        chunk_offsets,
        spec_at,
    })
}

pub fn parse_type_chunk(data: &[u8], pos: usize) -> Result<TypeChunk> {
    let header_size = u16_at(data, pos + 2)? as usize;
    let type_id = *data
        .get(pos + 8)
        .ok_or_else(|| arsc("type chunk truncated"))?;
    let flags = *data
        .get(pos + 9)
        .ok_or_else(|| arsc("type chunk truncated"))?;
    let entry_count = u32_at(data, pos + 12)? as usize;
    let entries_start = u32_at(data, pos + 16)? as usize;
    let config = ResConfig::parse(data, pos + 20)?;
    let offsets_at = pos.saturating_add(header_size.max(20));
    let entries_at = pos.saturating_add(entries_start);
    let offset_width = if flags & TYPE_FLAG_OFFSET16 != 0 { 2 } else { 4 };
    let offsets_end = offsets_at
        .checked_add(entry_count.saturating_mul(offset_width))
        .ok_or_else(|| arsc("offsets table overflow"))?;
    if offsets_end > data.len() {
        return Err(arsc("offsets table out of range"));
    }
    Ok(TypeChunk {
        chunk_at: pos,
        type_id,
        flags,
        entry_count,
        offsets_at,
        entries_at,
        config,
    })
}

/// Find the chunk-relative entry offset for `entry_id`, or `None`.
pub fn lookup_entry_offset(data: &[u8], chunk: &TypeChunk, entry_id: u16) -> Option<usize> {
    if chunk.flags & TYPE_FLAG_SPARSE != 0 {
        for i in 0..chunk.entry_count {
            let at = chunk.offsets_at + i * 4;
            let Ok(idx) = u16_at(data, at) else {
                return None;
            };
            if idx != entry_id {
                continue;
            }
            let off = u16_at(data, at + 2).ok()? as usize;
            return Some(off * 4);
        }
        None
    } else if chunk.flags & TYPE_FLAG_OFFSET16 != 0 {
        if (entry_id as usize) >= chunk.entry_count {
            return None;
        }
        let at = chunk.offsets_at + (entry_id as usize) * 2;
        let off = u16_at(data, at).ok()? as usize;
        if off == 0xFFFF {
            None
        } else {
            Some(off * 4)
        }
    } else {
        if (entry_id as usize) >= chunk.entry_count {
            return None;
        }
        let at = chunk.offsets_at + (entry_id as usize) * 4;
        let off = u32_at(data, at).ok()?;
        if off == NO_ENTRY {
            None
        } else {
            Some(off as usize)
        }
    }
}

/// Decoded entry fields.
#[derive(Debug, Clone)]
pub struct DecodedEntry {
    /// Key string-pool index.
    pub key_index: u32,
    /// Simple value (or `ResValue::NULL` for complex entries).
    pub value: ResValue,
    /// Bag entries for complex entries.
    pub bag: Option<Bag>,
}

/// Decode a `ResTable_entry` at an absolute offset.
pub fn decode_entry(data: &[u8], at: usize) -> Result<DecodedEntry> {
    let first = u16_at(data, at)?;
    let flags = u16_at(data, at + 2)?;
    let complex = flags & ENTRY_FLAG_COMPLEX != 0;
    let compact = flags & ENTRY_FLAG_COMPACT != 0;
    if compact {
        let data_type = (flags >> 8) as u8;
        let raw = u32_at(data, at + 4)?;
        return Ok(DecodedEntry {
            key_index: first as u32,
            value: ResValue {
                data_type,
                data: raw,
            },
            bag: None,
        });
    }
    let key_index = u32_at(data, at + 4)?;
    if complex {
        let parent = u32_at(data, at + 8)?;
        let count = u32_at(data, at + 12)? as usize;
        if count > 1_000_000 {
            return Err(arsc("implausible bag entry count"));
        }
        let mut entries = Vec::with_capacity(count.min(4096));
        for i in 0..count {
            let m = at
                .checked_add(16)
                .and_then(|v| v.checked_add(i * 12))
                .ok_or_else(|| arsc("bag offset overflow"))?;
            let name = u32_at(data, m)?;
            let value = decode_res_value(data, m + 4)?;
            entries.push((name, value));
        }
        Ok(DecodedEntry {
            key_index,
            value: ResValue::NULL,
            bag: Some(Bag { parent, entries }),
        })
    } else {
        Ok(DecodedEntry {
            key_index,
            value: decode_res_value(data, at + 8)?,
            bag: None,
        })
    }
}

/// Decode a `Res_value` at an absolute offset.
pub fn decode_res_value(data: &[u8], at: usize) -> Result<ResValue> {
    let data_type = *data.get(at + 3).ok_or_else(|| arsc("short Res_value"))?;
    let raw = u32_at(data, at + 4)?;
    Ok(ResValue {
        data_type,
        data: raw,
    })
}

/// `NoPackage` error for callers that need it.
pub fn no_package(pkg_id: u8) -> ResError {
    ResError::new(RES_NO_PACKAGE, format!("no package 0x{pkg_id:02x}"))
}

/// `NotFound` error for callers that need it.
pub fn not_found(id: u32) -> ResError {
    ResError::new(RES_NOT_FOUND, format!("resource 0x{id:08x} not found"))
}
