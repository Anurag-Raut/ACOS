//! # acos-dex
//!
//! A clean-room parser, instruction decoder, and structural verifier for real
//! Android Dalvik `.dex` files (as produced by `d8`), written from the public
//! [DEX format](https://source.android.com/docs/core/runtime/dex-format) and
//! [Dalvik bytecode](https://source.android.com/docs/core/runtime/dalvik-bytecode)
//! specifications. No AOSP source was consulted or copied.
//!
//! ## Scope
//!
//! The crate provides:
//!
//! * [`DexFile::parse`] — a fully bounds-checked DEX parser: header
//!   (magic/version/Adler-32/endian, plus a lazily-computed SHA-1 check), all id
//!   tables (`string_ids`, `type_ids`, `proto_ids`, `field_ids`, `method_ids`,
//!   `class_defs`, `call_site_ids`, `method_handles`), `map_list`,
//!   `class_data_item`, `code_item` (instructions, tries, catch handlers),
//!   `debug_info_item` line tables, `encoded_value`/`encoded_array`,
//!   annotation directories, and the ART `hiddenapi_class_data_item`.
//! * [`decode`] — a decoder for every defined opcode (0x00–0xff), plus payload
//!   decoders for packed/sparse switch and fill-array-data.
//! * [`verify`] — a **structural** (pass-1) verifier: index bounds, instruction
//!   decoding, branch/switch targets on instruction boundaries, register
//!   indices, `ins_size`, try/handler addresses, payload well-formedness, and
//!   fallthrough past the end of a method.
//!
//! ## Android load semantics
//!
//! Platform build tools (hiddenapi encoding) rewrite boot-classpath DEX files
//! and fix up only the Adler-32 checksum, so the SHA-1 signature is frequently
//! stale even in system images. Android's runtime does not validate it at load
//! time, so [`DexFile::parse`] **accepts** a signature mismatch by default (the
//! result is available, lazily, from [`DexFile::signature_valid`]). Use
//! [`DexFile::parse_with`] and [`ParseOptions::STRICT`] to reject it.
//!
//! ## Performance
//!
//! String decoding and the SHA-1 check are deferred and cached, so parsing a
//! 10 MB boot-classpath DEX is a few milliseconds and does not materialise the
//! string table. Long-lived callers that need strings or signature validity pay
//! for them on first access.
//!
//! ## Explicit non-goals (for now)
//!
//! **Full type-flow / data-flow verification is out of scope.** The verifier
//! does not track register types, verify that `invoke-*` argument types match
//! the prototype, check uninitialized-object rules, or ensure class hierarchy
//! consistency. It is a structural pass only, as documented on [`verify`].
//!
//! The crate is `#![forbid(unsafe_code)]` and never panics on malformed input:
//! every failure is a structured [`DexError`].

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod annotation;
pub mod classdata;
pub mod code;
pub mod decode;
pub mod disasm;
pub mod encoded;
pub mod error;
pub mod hash;
pub mod hiddenapi;
pub mod mutf8;
pub mod opcode;
pub mod reader;
pub mod verify;

pub use annotation::{Annotation, AnnotationsDirectory};
pub use classdata::{ClassData, EncodedField, EncodedMethod};
pub use code::{CatchHandler, CodeItem, TryItem, TypeAddrPair};
pub use decode::{
    decode, decode_payload, payload_kind, payload_len, FillArrayDataPayload, Instruction, Payload,
    SwitchPayload,
};
pub use disasm::{disassemble, format_instruction, mnemonic_sequence};
pub use encoded::{EncodedAnnotation, EncodedValue};
pub use error::{DexError, DexResult};
pub use hiddenapi::HiddenApiClassData;
pub use opcode::{defined_opcodes, Format, Opcode};
pub use reader::Reader;
pub use verify::{verify, VerifyReport};

use std::sync::Arc;
use std::sync::OnceLock;

/// Little-endian endian tag written into every standard DEX file.
pub const ENDIAN_CONSTANT: u32 = 0x1234_5678;
/// "Reverse" endian tag; rejected.
pub const REVERSE_ENDIAN_CONSTANT: u32 = 0x7856_3412;
/// Sentinel index value meaning "absent" (`0xffffffff`).
pub const NO_INDEX: u32 = 0xffff_ffff;
/// Size of the fixed DEX header in bytes.
pub const HEADER_SIZE: usize = 0x70;

/// Options controlling header validation during [`DexFile::parse_with`].
///
/// The defaults match Android's runtime load semantics: the Adler-32 checksum
/// is enforced, but the SHA-1 signature is only recorded, not required.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseOptions {
    /// Verify the Adler-32 checksum. Default `true`.
    pub verify_checksum: bool,
    /// Verify the SHA-1 signature. Default `false`.
    ///
    /// Android's runtime does not validate the SHA-1 signature field at load
    /// time, and platform build tools (hiddenapi encoding) rewrite boot
    /// classpath DEX files while fixing up only the Adler-32 checksum. A
    /// mismatch is reported through [`DexFile::signature_valid`]; set this to
    /// `true` (or use
    /// [`ParseOptions::STRICT`]) to turn a mismatch into
    /// [`DexError::BadSignature`].
    pub verify_signature: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions {
            verify_checksum: true,
            verify_signature: false,
        }
    }
}

impl ParseOptions {
    /// Strict mode: both the Adler-32 checksum and the SHA-1 signature must
    /// match.
    pub const STRICT: ParseOptions = ParseOptions {
        verify_checksum: true,
        verify_signature: true,
    };
}

/// Standard DEX access flags.
pub mod access {
    pub const PUBLIC: u32 = 0x0000_0001;
    pub const PRIVATE: u32 = 0x0000_0002;
    pub const PROTECTED: u32 = 0x0000_0004;
    pub const STATIC: u32 = 0x0000_0008;
    pub const FINAL: u32 = 0x0000_0010;
    pub const SYNCHRONIZED: u32 = 0x0000_0020;
    pub const VOLATILE: u32 = 0x0000_0040;
    pub const BRIDGE: u32 = 0x0000_0040;
    pub const TRANSIENT: u32 = 0x0000_0080;
    pub const VARARGS: u32 = 0x0000_0080;
    pub const NATIVE: u32 = 0x0000_0100;
    pub const INTERFACE: u32 = 0x0000_0200;
    pub const ABSTRACT: u32 = 0x0000_0400;
    pub const STRICT: u32 = 0x0000_0800;
    pub const SYNTHETIC: u32 = 0x0000_1000;
    pub const ANNOTATION: u32 = 0x0000_2000;
    pub const ENUM: u32 = 0x0000_4000;
    pub const CONSTRUCTOR: u32 = 0x0001_0000;
    pub const DECLARED_SYNCHRONIZED: u32 = 0x0002_0000;
}

/// `map_item` type codes.
pub mod map_type {
    pub const HEADER_ITEM: u16 = 0x0000;
    pub const STRING_ID_ITEM: u16 = 0x0001;
    pub const TYPE_ID_ITEM: u16 = 0x0002;
    pub const PROTO_ID_ITEM: u16 = 0x0003;
    pub const FIELD_ID_ITEM: u16 = 0x0004;
    pub const METHOD_ID_ITEM: u16 = 0x0005;
    pub const CLASS_DEF_ITEM: u16 = 0x0006;
    pub const CALL_SITE_ID_ITEM: u16 = 0x0007;
    pub const METHOD_HANDLE_ITEM: u16 = 0x0008;
    pub const MAP_LIST: u16 = 0x1000;
    pub const TYPE_LIST: u16 = 0x1001;
    pub const ANNOTATION_SET_REF_LIST: u16 = 0x1002;
    pub const ANNOTATION_SET_ITEM: u16 = 0x1003;
    pub const CLASS_DATA_ITEM: u16 = 0x2000;
    pub const CODE_ITEM: u16 = 0x2001;
    pub const STRING_DATA_ITEM: u16 = 0x2002;
    pub const DEBUG_INFO_ITEM: u16 = 0x2003;
    pub const ANNOTATION_ITEM: u16 = 0x2004;
    pub const ENCODED_ARRAY_ITEM: u16 = 0x2005;
    pub const ANNOTATIONS_DIRECTORY_ITEM: u16 = 0x2006;
    pub const HIDDENAPI_CLASS_DATA_ITEM: u16 = 0xF000;
}

/// Parsed DEX header (the fixed 112-byte `header_item`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// Numeric version: 35, 37, 38 or 39.
    pub version: u32,
    pub checksum: u32,
    pub signature: [u8; 20],
    pub file_size: u32,
    pub header_size: u32,
    pub endian_tag: u32,
    pub link_size: u32,
    pub link_off: u32,
    pub map_off: u32,
    pub string_ids_size: u32,
    pub string_ids_off: u32,
    pub type_ids_size: u32,
    pub type_ids_off: u32,
    pub proto_ids_size: u32,
    pub proto_ids_off: u32,
    pub field_ids_size: u32,
    pub field_ids_off: u32,
    pub method_ids_size: u32,
    pub method_ids_off: u32,
    pub class_defs_size: u32,
    pub class_defs_off: u32,
    pub data_size: u32,
    pub data_off: u32,
}

/// `proto_id_item` plus its eagerly-resolved parameter type indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtoId {
    pub shorty_idx: u32,
    pub return_type_idx: u32,
    pub parameters_off: u32,
    pub parameters: Vec<u32>,
}

/// `field_id_item`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldId {
    pub class_idx: u16,
    pub type_idx: u16,
    pub name_idx: u32,
}

/// `method_id_item`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodId {
    pub class_idx: u16,
    pub proto_idx: u16,
    pub name_idx: u32,
}

/// `class_def_item` plus its eagerly-parsed interface list and annotations.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassDef {
    pub class_idx: u32,
    pub access_flags: u32,
    pub superclass_idx: u32,
    pub interfaces_off: u32,
    pub source_file_idx: u32,
    pub annotations_off: u32,
    pub class_data_off: u32,
    pub static_values_off: u32,
    pub interfaces: Vec<u32>,
    pub annotations: Option<AnnotationsDirectory>,
}

/// `method_handle_item`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodHandle {
    pub handle_type: u16,
    pub unused1: u16,
    pub field_or_method_id: u16,
    pub unused2: u16,
}

/// `map_item`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapItem {
    pub type_: u16,
    pub unused: u16,
    pub size: u32,
    pub offset: u32,
}

/// A fully parsed reference to a method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodRef<'a> {
    pub class: &'a str,
    pub name: &'a str,
    pub shorty: &'a str,
    pub return_type: &'a str,
    pub parameters: Vec<&'a str>,
    pub proto_idx: u32,
}

impl MethodRef<'_> {
    /// `Lcom/example/Foo;.bar:(ILjava/lang/String;)V`
    pub fn full_name(&self) -> String {
        format!(
            "{}.{}:({}){}",
            self.class,
            self.name,
            self.parameters.concat(),
            self.return_type
        )
    }
}

/// A fully parsed reference to a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldRef<'a> {
    pub class: &'a str,
    pub name: &'a str,
    pub type_: &'a str,
}

impl FieldRef<'_> {
    /// `Lcom/example/Foo;.value:I`
    pub fn full_name(&self) -> String {
        format!("{}.{}:{}", self.class, self.name, self.type_)
    }
}

/// A parsed DEX file.
#[derive(Clone, Debug)]
pub struct DexFile {
    bytes: Arc<[u8]>,
    header: Header,
    /// Lazily-decoded string table. `parse` only records the `string_data_offs`;
    /// the `String`s are materialised on first access and cached.
    strings: OnceLock<Result<Vec<String>, DexError>>,
    string_data_offs: Vec<u32>,
    type_ids: Vec<u32>,
    proto_ids: Vec<ProtoId>,
    field_ids: Vec<FieldId>,
    method_ids: Vec<MethodId>,
    class_defs: Vec<ClassDef>,
    call_site_ids: Vec<u32>,
    method_handles: Vec<MethodHandle>,
    map: Vec<MapItem>,
    hiddenapi: Option<HiddenApiClassData>,
    /// Lazily-computed SHA-1 signature validity (see [`DexFile::signature_valid`]).
    signature_check: OnceLock<bool>,
}

fn section_start(
    off: u32,
    count: u32,
    elem: usize,
    file_len: usize,
    name: &'static str,
) -> DexResult<usize> {
    if count == 0 {
        return Ok(0);
    }
    let bytes = (count as usize)
        .checked_mul(elem)
        .ok_or(DexError::Overflow { what: name })?;
    let start = off as usize;
    let end = start
        .checked_add(bytes)
        .ok_or(DexError::Overflow { what: name })?;
    if end > file_len {
        return Err(DexError::Truncated {
            offset: start as u64,
            needed: bytes,
            len: file_len.saturating_sub(start),
        });
    }
    Ok(start)
}

fn parse_type_list(r: &Reader, off: u32, type_count: u32) -> DexResult<Vec<u32>> {
    if off == 0 {
        return Ok(Vec::new());
    }
    let off = off as usize;
    let size = r.u32(off)?;
    let mut out = Vec::with_capacity((size as usize).min(65536));
    for i in 0..size as usize {
        let t = r.u16(off + 4 + i * 2)? as u32;
        if t >= type_count {
            return Err(DexError::IndexOutOfRange {
                kind: "type",
                index: t,
                size: type_count,
                offset: r.base() + (off + 4 + i * 2) as u64,
            });
        }
        out.push(t);
    }
    Ok(out)
}

impl DexFile {
    /// Parse a DEX image from an owned `Vec<u8>`, `Arc<[u8]>`, or byte slice.
    ///
    /// Validates magic/version, endianness, file size and the Adler-32
    /// checksum, and parses every section. The SHA-1 signature is computed and
    /// available lazily via [`DexFile::signature_valid`], but a mismatch is
    /// **accepted** by default, matching Android's runtime load semantics. Use
    /// [`DexFile::parse_with`] with [`ParseOptions::STRICT`] to enforce it.
    pub fn parse(bytes: impl Into<Arc<[u8]>>) -> DexResult<DexFile> {
        Self::parse_with(bytes, ParseOptions::default())
    }

    /// Like [`DexFile::parse`], but with explicit validation options.
    pub fn parse_with(
        bytes: impl Into<Arc<[u8]>>,
        options: ParseOptions,
    ) -> DexResult<DexFile> {
        let bytes: Arc<[u8]> = bytes.into();
        let len = bytes.len();
        let r = Reader::new(&bytes);

        if len < 8 {
            return Err(DexError::Truncated {
                offset: 0,
                needed: 8,
                len,
            });
        }
        let mut found = [0u8; 8];
        found.copy_from_slice(&bytes[..8]);
        if &bytes[0..4] != b"dex\n" || bytes[7] != 0 {
            return Err(DexError::BadMagic { offset: 0, found });
        }
        let version = match &bytes[4..7] {
            b"035" => 35,
            b"037" => 37,
            b"038" => 38,
            b"039" => 39,
            other => {
                return Err(DexError::UnsupportedVersion {
                    offset: 4,
                    version: String::from_utf8_lossy(other).into_owned(),
                })
            }
        };
        if len < HEADER_SIZE {
            return Err(DexError::Truncated {
                offset: 0,
                needed: HEADER_SIZE,
                len,
            });
        }

        let mut signature = [0u8; 20];
        signature.copy_from_slice(r.bytes(12, 20)?);

        let header = Header {
            version,
            checksum: r.u32(8)?,
            signature,
            file_size: r.u32(32)?,
            header_size: r.u32(36)?,
            endian_tag: r.u32(40)?,
            link_size: r.u32(44)?,
            link_off: r.u32(48)?,
            map_off: r.u32(52)?,
            string_ids_size: r.u32(56)?,
            string_ids_off: r.u32(60)?,
            type_ids_size: r.u32(64)?,
            type_ids_off: r.u32(68)?,
            proto_ids_size: r.u32(72)?,
            proto_ids_off: r.u32(76)?,
            field_ids_size: r.u32(80)?,
            field_ids_off: r.u32(84)?,
            method_ids_size: r.u32(88)?,
            method_ids_off: r.u32(92)?,
            class_defs_size: r.u32(96)?,
            class_defs_off: r.u32(100)?,
            data_size: r.u32(104)?,
            data_off: r.u32(108)?,
        };

        if header.file_size as usize > len {
            return Err(DexError::Truncated {
                offset: 32,
                needed: header.file_size as usize,
                len,
            });
        }
        let file_len = header.file_size as usize;
        if file_len < HEADER_SIZE {
            return Err(DexError::BadHeader {
                field: "file_size",
                reason: "smaller than the 112-byte header",
            });
        }
        if header.header_size < HEADER_SIZE as u32 {
            return Err(DexError::BadHeader {
                field: "header_size",
                reason: "smaller than the 112-byte header",
            });
        }
        if header.endian_tag != ENDIAN_CONSTANT {
            return Err(DexError::BadEndian {
                offset: 40,
                tag: header.endian_tag,
            });
        }

        // Checksum first, then signature, matching the spec's dependency order.
        // Android enforces the checksum but does not verify the signature at
        // load time, so a signature mismatch is accepted unless `options` says
        // otherwise. SHA-1 is only computed eagerly in strict mode; otherwise it
        // is deferred to `signature_valid()` to keep parse latency low.
        let computed_checksum = hash::adler32(&bytes[12..file_len]);
        if options.verify_checksum && computed_checksum != header.checksum {
            return Err(DexError::BadChecksum {
                stored: header.checksum,
                computed: computed_checksum,
            });
        }
        let signature_check = OnceLock::new();
        if options.verify_signature {
            let computed_sig = hash::sha1(&bytes[32..file_len]);
            if computed_sig != header.signature {
                return Err(DexError::BadSignature {
                    stored: hash::hex(&header.signature),
                    computed: hash::hex(&computed_sig),
                });
            }
            let _ = signature_check.set(true);
        }

        // --- string_ids / string_data_item ---
        let string_ids_off = section_start(
            header.string_ids_off,
            header.string_ids_size,
            4,
            file_len,
            "string_ids",
        )?;
        // String data is decoded lazily (see `decode_strings`); parse only
        // records and bounds-checks the `string_data_off`s. This keeps parse
        // time and peak memory lower for large boot-classpath DEX files.
        let string_count = header.string_ids_size;
        let mut string_data_offs = Vec::with_capacity(header.string_ids_size as usize);
        for i in 0..header.string_ids_size as usize {
            let data_off = r.u32(string_ids_off + i * 4)?;
            if data_off == 0 {
                return Err(DexError::BadHeader {
                    field: "string_id_item",
                    reason: "null string_data_off",
                });
            }
            if data_off as usize >= file_len {
                return Err(DexError::Truncated {
                    offset: data_off as u64,
                    needed: 1,
                    len: file_len,
                });
            }
            string_data_offs.push(data_off);
        }

        // --- type_ids ---
        let type_ids_off = section_start(
            header.type_ids_off,
            header.type_ids_size,
            4,
            file_len,
            "type_ids",
        )?;
        let mut type_ids = Vec::with_capacity(header.type_ids_size as usize);
        for i in 0..header.type_ids_size as usize {
            let idx = r.u32(type_ids_off + i * 4)?;
            if idx >= string_count {
                return Err(DexError::IndexOutOfRange {
                    kind: "string",
                    index: idx,
                    size: string_count,
                    offset: r.base() + (type_ids_off + i * 4) as u64,
                });
            }
            type_ids.push(idx);
        }

        // --- proto_ids ---
        let proto_ids_off = section_start(
            header.proto_ids_off,
            header.proto_ids_size,
            12,
            file_len,
            "proto_ids",
        )?;
        let mut proto_ids = Vec::with_capacity(header.proto_ids_size as usize);
        for i in 0..header.proto_ids_size as usize {
            let o = proto_ids_off + i * 12;
            let shorty_idx = r.u32(o)?;
            let return_type_idx = r.u32(o + 4)?;
            let parameters_off = r.u32(o + 8)?;
            if shorty_idx >= string_count {
                return Err(DexError::IndexOutOfRange {
                    kind: "string",
                    index: shorty_idx,
                    size: string_count,
                    offset: r.base() + o as u64,
                });
            }
            if return_type_idx >= type_ids.len() as u32 {
                return Err(DexError::IndexOutOfRange {
                    kind: "type",
                    index: return_type_idx,
                    size: type_ids.len() as u32,
                    offset: r.base() + (o + 4) as u64,
                });
            }
            let parameters = parse_type_list(&r, parameters_off, type_ids.len() as u32)?;
            proto_ids.push(ProtoId {
                shorty_idx,
                return_type_idx,
                parameters_off,
                parameters,
            });
        }

        // --- field_ids ---
        let field_ids_off = section_start(
            header.field_ids_off,
            header.field_ids_size,
            8,
            file_len,
            "field_ids",
        )?;
        let mut field_ids = Vec::with_capacity(header.field_ids_size as usize);
        for i in 0..header.field_ids_size as usize {
            let o = field_ids_off + i * 8;
            let class_idx = r.u16(o)?;
            let type_idx = r.u16(o + 2)?;
            let name_idx = r.u32(o + 4)?;
            if class_idx as u32 >= type_ids.len() as u32 {
                return Err(DexError::IndexOutOfRange {
                    kind: "type",
                    index: class_idx as u32,
                    size: type_ids.len() as u32,
                    offset: r.base() + o as u64,
                });
            }
            if type_idx as u32 >= type_ids.len() as u32 {
                return Err(DexError::IndexOutOfRange {
                    kind: "type",
                    index: type_idx as u32,
                    size: type_ids.len() as u32,
                    offset: r.base() + (o + 2) as u64,
                });
            }
            if name_idx >= string_count {
                return Err(DexError::IndexOutOfRange {
                    kind: "string",
                    index: name_idx,
                    size: string_count,
                    offset: r.base() + (o + 4) as u64,
                });
            }
            field_ids.push(FieldId {
                class_idx,
                type_idx,
                name_idx,
            });
        }

        // --- method_ids ---
        let method_ids_off = section_start(
            header.method_ids_off,
            header.method_ids_size,
            8,
            file_len,
            "method_ids",
        )?;
        let mut method_ids = Vec::with_capacity(header.method_ids_size as usize);
        for i in 0..header.method_ids_size as usize {
            let o = method_ids_off + i * 8;
            let class_idx = r.u16(o)?;
            let proto_idx = r.u16(o + 2)?;
            let name_idx = r.u32(o + 4)?;
            if class_idx as u32 >= type_ids.len() as u32 {
                return Err(DexError::IndexOutOfRange {
                    kind: "type",
                    index: class_idx as u32,
                    size: type_ids.len() as u32,
                    offset: r.base() + o as u64,
                });
            }
            if proto_idx as u32 >= proto_ids.len() as u32 {
                return Err(DexError::IndexOutOfRange {
                    kind: "proto",
                    index: proto_idx as u32,
                    size: proto_ids.len() as u32,
                    offset: r.base() + (o + 2) as u64,
                });
            }
            if name_idx >= string_count {
                return Err(DexError::IndexOutOfRange {
                    kind: "string",
                    index: name_idx,
                    size: string_count,
                    offset: r.base() + (o + 4) as u64,
                });
            }
            method_ids.push(MethodId {
                class_idx,
                proto_idx,
                name_idx,
            });
        }

        // --- map_list (needed before class_defs for call sites / method handles) ---
        let mut map = Vec::new();
        if header.map_off != 0 {
            let mo = header.map_off as usize;
            let size = r.u32(mo)?;
            // The u32 size precedes the map_item array.
            let items_off = header.map_off.checked_add(4).ok_or(DexError::Overflow {
                what: "map_list offset",
            })?;
            let start = section_start(items_off, size, 12, file_len, "map_list")?;
            for i in 0..size as usize {
                let o = start + i * 12;
                map.push(MapItem {
                    type_: r.u16(o)?,
                    unused: r.u16(o + 2)?,
                    size: r.u32(o + 4)?,
                    offset: r.u32(o + 8)?,
                });
            }
        }

        // --- class_defs (with interfaces + annotations) ---
        let class_defs_off = section_start(
            header.class_defs_off,
            header.class_defs_size,
            32,
            file_len,
            "class_defs",
        )?;
        let mut class_defs = Vec::with_capacity(header.class_defs_size as usize);
        for i in 0..header.class_defs_size as usize {
            let o = class_defs_off + i * 32;
            let class_idx = r.u32(o)?;
            let access_flags = r.u32(o + 4)?;
            let superclass_idx = r.u32(o + 8)?;
            let interfaces_off = r.u32(o + 12)?;
            let source_file_idx = r.u32(o + 16)?;
            let annotations_off = r.u32(o + 20)?;
            let class_data_off = r.u32(o + 24)?;
            let static_values_off = r.u32(o + 28)?;

            if class_idx >= type_ids.len() as u32 {
                return Err(DexError::IndexOutOfRange {
                    kind: "type",
                    index: class_idx,
                    size: type_ids.len() as u32,
                    offset: r.base() + o as u64,
                });
            }
            if superclass_idx != NO_INDEX && superclass_idx >= type_ids.len() as u32 {
                return Err(DexError::IndexOutOfRange {
                    kind: "type",
                    index: superclass_idx,
                    size: type_ids.len() as u32,
                    offset: r.base() + (o + 8) as u64,
                });
            }
            if source_file_idx != NO_INDEX && source_file_idx >= string_count {
                return Err(DexError::IndexOutOfRange {
                    kind: "string",
                    index: source_file_idx,
                    size: string_count,
                    offset: r.base() + (o + 16) as u64,
                });
            }

            let interfaces = parse_type_list(&r, interfaces_off, type_ids.len() as u32)?;
            let annotations = if annotations_off == 0 {
                None
            } else {
                Some(annotation::parse_directory(&r, annotations_off)?)
            };

            class_defs.push(ClassDef {
                class_idx,
                access_flags,
                superclass_idx,
                interfaces_off,
                source_file_idx,
                annotations_off,
                class_data_off,
                static_values_off,
                interfaces,
                annotations,
            });
        }

        // --- call_site_ids / method_handles (found via the map) ---
        let mut call_site_ids = Vec::new();
        let mut method_handles = Vec::new();
        for m in &map {
            match m.type_ {
                map_type::CALL_SITE_ID_ITEM => {
                    let start = section_start(m.offset, m.size, 4, file_len, "call_site_ids")?;
                    for i in 0..m.size as usize {
                        call_site_ids.push(r.u32(start + i * 4)?);
                    }
                }
                map_type::METHOD_HANDLE_ITEM => {
                    let start = section_start(m.offset, m.size, 8, file_len, "method_handles")?;
                    for i in 0..m.size as usize {
                        let o = start + i * 8;
                        method_handles.push(MethodHandle {
                            handle_type: r.u16(o)?,
                            unused1: r.u16(o + 2)?,
                            field_or_method_id: r.u16(o + 4)?,
                            unused2: r.u16(o + 6)?,
                        });
                    }
                }
                _ => {}
            }
        }

        // --- hiddenapi_class_data_item (optional, ART extension) ---
        // Parsing is best-effort: a malformed section must never fail the whole
        // parse, since it is not required for structural verification.
        let mut hiddenapi = None;
        for m in &map {
            if m.type_ == map_type::HIDDENAPI_CLASS_DATA_ITEM {
                if let Ok(h) = hiddenapi::parse(&r, m.offset, header.class_defs_size) {
                    hiddenapi = Some(h);
                }
                break;
            }
        }

        Ok(DexFile {
            bytes,
            header,
            strings: OnceLock::new(),
            string_data_offs,
            type_ids,
            proto_ids,
            field_ids,
            method_ids,
            class_defs,
            call_site_ids,
            method_handles,
            map,
            hiddenapi,
            signature_check,
        })
    }

    /// Parse a DEX file from disk.
    pub fn parse_file(path: impl AsRef<std::path::Path>) -> DexResult<DexFile> {
        let data = std::fs::read(path).map_err(|_| DexError::BadHeader {
            field: "file",
            reason: "could not read file",
        })?;
        DexFile::parse(data)
    }

    /// A reader over the raw bytes.
    pub fn reader(&self) -> Reader<'_> {
        Reader::new(&self.bytes)
    }

    /// The raw DEX bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Whether the stored SHA-1 signature matches the computed one.
    ///
    /// A mismatch is accepted by default (Android does not verify the
    /// signature at load time); see [`ParseOptions`]. The value is computed on
    /// first call and cached, so a default parse does not pay for SHA-1 unless
    /// the caller asks for it.
    pub fn signature_valid(&self) -> bool {
        *self.signature_check.get_or_init(|| {
            let end = (self.header.file_size as usize).min(self.bytes.len());
            hash::sha1(&self.bytes[32..end]) == self.header.signature
        })
    }

    /// Raw hiddenapi class data, if the DEX carries a
    /// `hiddenapi_class_data_item` (map type `0xF000`).
    pub fn hiddenapi_class_data(&self) -> Option<&HiddenApiClassData> {
        self.hiddenapi.as_ref()
    }

    /// Raw hiddenapi flags for the class at `class_index` in `class_defs`
    /// order, if hiddenapi data is present.
    pub fn hiddenapi_flags_by_index(&self, class_index: usize) -> Option<&[u32]> {
        self.hiddenapi
            .as_ref()
            .and_then(|h| h.per_class.get(class_index).map(Vec::as_slice))
    }

    /// Raw hiddenapi flags for a class definition, if hiddenapi data is
    /// present. `def` must be a reference obtained from [`DexFile::class_defs`]
    /// (or a class found via [`DexFile::find_class`]).
    pub fn hiddenapi_flags(&self, def: &ClassDef) -> Option<&[u32]> {
        let idx = self
            .class_defs
            .iter()
            .position(|d| std::ptr::eq(d, def))?;
        self.hiddenapi_flags_by_index(idx)
    }

    pub fn map(&self) -> &[MapItem] {
        &self.map
    }

    /// The decoded string table, materialised on first access and cached.
    ///
    /// Returns an empty slice if the string data is malformed; use
    /// [`DexFile::string`] to observe the underlying error.
    pub fn strings(&self) -> &[String] {
        match self.strings.get_or_init(|| self.decode_strings()) {
            Ok(v) => v,
            Err(_) => &[],
        }
    }

    /// Decode every `string_data_item` into a `String`.
    pub(crate) fn decode_strings(&self) -> DexResult<Vec<String>> {
        let r = self.reader();
        let mut out = Vec::with_capacity(self.string_data_offs.len());
        for &data_off in &self.string_data_offs {
            let sr = r.sub_to_end(data_off as usize)?;
            let (_utf16_len, n) = sr.uleb128(0)?;
            let tail = sr.sub_to_end(n)?;
            let (s, _) = mutf8::decode(tail.as_slice(), tail.base())?;
            out.push(s);
        }
        Ok(out)
    }

    pub fn string_data_offsets(&self) -> &[u32] {
        &self.string_data_offs
    }

    pub fn type_ids(&self) -> &[u32] {
        &self.type_ids
    }

    pub fn proto_ids(&self) -> &[ProtoId] {
        &self.proto_ids
    }

    pub fn field_ids(&self) -> &[FieldId] {
        &self.field_ids
    }

    pub fn method_ids(&self) -> &[MethodId] {
        &self.method_ids
    }

    pub fn class_defs(&self) -> &[ClassDef] {
        &self.class_defs
    }

    pub fn call_site_ids(&self) -> &[u32] {
        &self.call_site_ids
    }

    pub fn method_handles(&self) -> &[MethodHandle] {
        &self.method_handles
    }

    /// Resolve a string index.
    pub fn string(&self, idx: u32) -> DexResult<&str> {
        match self.strings.get_or_init(|| self.decode_strings()) {
            Ok(v) => v.get(idx as usize).map(String::as_str).ok_or_else(|| {
                DexError::index("string", idx, v.len() as u32, 0)
            }),
            Err(e) => Err(e.clone()),
        }
    }

    /// Resolve a type index to its descriptor string.
    pub fn type_descriptor(&self, idx: u32) -> DexResult<&str> {
        let s = *self.type_ids.get(idx as usize).ok_or_else(|| {
            DexError::index("type", idx, self.type_ids.len() as u32, 0)
        })?;
        self.string(s)
    }

    /// Resolve a proto index to its parameters.
    pub fn proto(&self, idx: u32) -> DexResult<&ProtoId> {
        self.proto_ids.get(idx as usize).ok_or_else(|| {
            DexError::index("proto", idx, self.proto_ids.len() as u32, 0)
        })
    }

    /// Resolve a method index into class/name/prototype.
    pub fn method_ref(&self, idx: u32) -> DexResult<MethodRef<'_>> {
        let m = *self.method_ids.get(idx as usize).ok_or_else(|| {
            DexError::index("method", idx, self.method_ids.len() as u32, 0)
        })?;
        let proto = self.proto(m.proto_idx as u32)?;
        let class = self.type_descriptor(m.class_idx as u32)?;
        let name = self.string(m.name_idx)?;
        let shorty = self.string(proto.shorty_idx)?;
        let return_type = self.type_descriptor(proto.return_type_idx)?;
        let mut parameters = Vec::with_capacity(proto.parameters.len());
        for &t in &proto.parameters {
            parameters.push(self.type_descriptor(t)?);
        }
        Ok(MethodRef {
            class,
            name,
            shorty,
            return_type,
            parameters,
            proto_idx: m.proto_idx as u32,
        })
    }

    /// Resolve a field index into class/name/type.
    pub fn field_ref(&self, idx: u32) -> DexResult<FieldRef<'_>> {
        let f = *self.field_ids.get(idx as usize).ok_or_else(|| {
            DexError::index("field", idx, self.field_ids.len() as u32, 0)
        })?;
        Ok(FieldRef {
            class: self.type_descriptor(f.class_idx as u32)?,
            name: self.string(f.name_idx)?,
            type_: self.type_descriptor(f.type_idx as u32)?,
        })
    }

    /// Resolve a method handle entry.
    pub fn method_handle(&self, idx: u32) -> DexResult<&MethodHandle> {
        self.method_handles.get(idx as usize).ok_or_else(|| {
            DexError::index("method_handle", idx, self.method_handles.len() as u32, 0)
        })
    }

    /// Decode a call site (an `encoded_array_item`) by index.
    pub fn call_site(&self, idx: u32) -> DexResult<Vec<EncodedValue>> {
        let off = *self.call_site_ids.get(idx as usize).ok_or_else(|| {
            DexError::index("call_site", idx, self.call_site_ids.len() as u32, 0)
        })?;
        let r = self.reader();
        Ok(encoded::decode_array(&r, off as usize)?.0)
    }

    /// The class descriptor for a `class_def_item`.
    pub fn class_descriptor(&self, def: &ClassDef) -> DexResult<&str> {
        self.type_descriptor(def.class_idx)
    }

    /// The superclass descriptor, if any.
    pub fn superclass_descriptor(&self, def: &ClassDef) -> DexResult<Option<&str>> {
        if def.superclass_idx == NO_INDEX {
            Ok(None)
        } else {
            Ok(Some(self.type_descriptor(def.superclass_idx)?))
        }
    }

    /// The `source_file` string, if any.
    pub fn source_file(&self, def: &ClassDef) -> DexResult<Option<&str>> {
        if def.source_file_idx == NO_INDEX {
            Ok(None)
        } else {
            Ok(Some(self.string(def.source_file_idx)?))
        }
    }

    /// Find a class by descriptor (`Lcom/example/Foo;`).
    pub fn find_class(&self, descriptor: &str) -> Option<&ClassDef> {
        self.class_defs
            .iter()
            .find(|d| self.class_descriptor(d).map(|s| s == descriptor).unwrap_or(false))
    }

    /// Parse the `class_data_item` for a class, if present.
    pub fn class_data(&self, def: &ClassDef) -> DexResult<Option<ClassData>> {
        if def.class_data_off == 0 {
            return Ok(None);
        }
        let r = self.reader();
        Ok(Some(classdata::parse_class_data(
            &r,
            def.class_data_off,
            self.field_ids.len() as u32,
            self.method_ids.len() as u32,
        )?))
    }

    /// Parse a `code_item` at a byte offset.
    pub fn code_item(&self, code_off: u32) -> DexResult<CodeItem> {
        let r = self.reader();
        code::parse_code_item(&r, code_off, self.type_ids.len() as u32)
    }

    /// Parse the static field initial values (`encoded_array_item`).
    pub fn static_values(&self, def: &ClassDef) -> DexResult<Option<Vec<EncodedValue>>> {
        if def.static_values_off == 0 {
            return Ok(None);
        }
        let r = self.reader();
        Ok(Some(encoded::decode_array(&r, def.static_values_off as usize)?.0))
    }

    /// The eagerly-parsed annotations directory for a class, if present.
    pub fn annotations<'a>(&self, def: &'a ClassDef) -> Option<&'a AnnotationsDirectory> {
        def.annotations.as_ref()
    }

    /// Parse a `debug_info_item` line table for a code item.
    pub fn line_table(&self, code: &CodeItem) -> DexResult<Vec<(u32, u32)>> {
        let r = self.reader();
        code::line_table(&r, code.debug_info_off)
    }

    /// Iterate over every method (direct and virtual) that has a code item.
    pub fn methods_with_code(&self) -> DexResult<Vec<(u32, EncodedMethod)>> {
        let mut out = Vec::new();
        for def in &self.class_defs {
            if let Some(cd) = self.class_data(def)? {
                for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
                    if m.code_off != 0 {
                        out.push((def.class_idx, *m));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Run the structural pass-1 verifier.
    pub fn verify(&self) -> DexResult<VerifyReport> {
        verify(self)
    }

    /// The largest DEX in the workspace is compared against `dexdump`; this is
    /// exposed so callers can report the file size.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}
