//! Serialization of a parsed [`DexFile`]'s id/string/type/method pools.
//!
//! The on-disk boot cache persists the fully linked [`BootImage`]. Rebuilding
//! that image currently re-opens every boot-classpath DEX and re-runs
//! [`DexFile::parse`] on it (header + id tables + class_defs + annotations).
//! This module persists the *result* of that parse so a cold process can
//! rebuild the same `DexFile` from the cached tables plus a plain re-read of the
//! raw bytes, skipping the parse.
//!
//! Only the eagerly-parsed pools travel: the raw bytes are re-read (a plain
//! `read`, no `mmap`/`unsafe`) and re-attached, so the lazily-decoded string
//! table, `class_data_item`, `code_item` and `debug_info_item` paths keep
//! working exactly as before. Nothing here re-decodes a class body.

use std::sync::{Arc, OnceLock};

use borsh::{BorshDeserialize, BorshSerialize};

use crate::{
    ClassDef, DexError, FieldId, Header, HiddenApiClassData, MapItem, MethodHandle, MethodId,
    ProtoId,
};

/// Version of the [`DexTables`] wire layout. Bump on any change.
pub const DEX_TABLES_VERSION: u32 = 1;

/// A serializable snapshot of a [`DexFile`]'s eagerly-parsed pools.
///
/// The raw DEX bytes are deliberately **not** part of this struct: the caller
/// re-reads them and passes them to [`crate::DexFile::from_tables`].
#[derive(Clone, Debug, PartialEq, BorshSerialize, BorshDeserialize)]
pub struct DexTables {
    /// Wire layout version.
    pub version: u32,
    pub header: Header,
    pub string_data_offs: Vec<u32>,
    pub type_ids: Vec<u32>,
    pub proto_ids: Vec<ProtoId>,
    pub field_ids: Vec<FieldId>,
    pub method_ids: Vec<MethodId>,
    pub class_defs: Vec<ClassDef>,
    pub call_site_ids: Vec<u32>,
    pub method_handles: Vec<MethodHandle>,
    pub map: Vec<MapItem>,
    pub hiddenapi: Option<HiddenApiClassData>,
}

impl crate::DexFile {
    /// Snapshot the eagerly-parsed pools for on-disk persistence.
    #[must_use]
    pub fn to_tables(&self) -> DexTables {
        DexTables {
            version: DEX_TABLES_VERSION,
            header: self.header.clone(),
            string_data_offs: self.string_data_offs.clone(),
            type_ids: self.type_ids.clone(),
            proto_ids: self.proto_ids.clone(),
            field_ids: self.field_ids.clone(),
            method_ids: self.method_ids.clone(),
            class_defs: self.class_defs.clone(),
            call_site_ids: self.call_site_ids.clone(),
            method_handles: self.method_handles.clone(),
            map: self.map.clone(),
            hiddenapi: self.hiddenapi.clone(),
        }
    }

    /// Rebuild a [`crate::DexFile`] from [`DexFile::to_tables`] plus the raw
    /// bytes it was parsed from.
    ///
    /// The raw bytes are validated against the stored header (magic, size) but
    /// are **not** re-parsed: the tables are trusted as far as the caller's own
    /// cache-key validation goes. Any structural mismatch is a structured
    /// [`DexError`] so the caller can fall back to a real parse.
    pub fn from_tables(tables: DexTables, raw: Arc<[u8]>) -> Result<crate::DexFile, DexError> {
        if tables.version != DEX_TABLES_VERSION {
            return Err(DexError::BadHeader {
                field: "cache_tables_version",
                reason: "unsupported parsed-table cache version",
            });
        }
        let file_len = tables.header.file_size as usize;
        if raw.len() < crate::HEADER_SIZE {
            return Err(DexError::Truncated {
                offset: 0,
                needed: crate::HEADER_SIZE,
                len: raw.len(),
            });
        }
        if &raw[0..4] != b"dex\n" || raw[7] != 0 {
            return Err(DexError::BadMagic {
                offset: 0,
                found: {
                    let mut f = [0u8; 8];
                    f.copy_from_slice(&raw[..8]);
                    f
                },
            });
        }
        if file_len > raw.len() {
            return Err(DexError::Truncated {
                offset: 32,
                needed: file_len,
                len: raw.len(),
            });
        }
        // Cheap consistency: the id pool sizes must agree with the header the
        // tables came from, so a truncated/poisoned table is rejected here
        // rather than surfacing as an index panic in a later accessor.
        if tables.string_data_offs.len() != tables.header.string_ids_size as usize
            || tables.type_ids.len() != tables.header.type_ids_size as usize
            || tables.proto_ids.len() != tables.header.proto_ids_size as usize
            || tables.field_ids.len() != tables.header.field_ids_size as usize
            || tables.method_ids.len() != tables.header.method_ids_size as usize
            || tables.class_defs.len() != tables.header.class_defs_size as usize
        {
            return Err(DexError::BadHeader {
                field: "cache_tables_size",
                reason: "id-table lengths do not match the cached header",
            });
        }
        Ok(crate::DexFile {
            id: crate::next_dex_id(),
            bytes: raw,
            header: tables.header,
            strings: OnceLock::new(),
            string_data_offs: tables.string_data_offs,
            type_ids: tables.type_ids,
            proto_ids: tables.proto_ids,
            field_ids: tables.field_ids,
            method_ids: tables.method_ids,
            class_defs: tables.class_defs,
            call_site_ids: tables.call_site_ids,
            method_handles: tables.method_handles,
            map: tables.map,
            hiddenapi: tables.hiddenapi,
            signature_check: OnceLock::new(),
            class_index: OnceLock::new(),
        })
    }
}
