//! `class_data_item` parsing (diff-encoded field/method indices).

use crate::error::{DexError, DexResult};
use crate::reader::Reader;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodedField {
    pub field_idx: u32,
    pub access_flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodedMethod {
    pub method_idx: u32,
    pub access_flags: u32,
    pub code_off: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassData {
    pub static_fields: Vec<EncodedField>,
    pub instance_fields: Vec<EncodedField>,
    pub direct_methods: Vec<EncodedMethod>,
    pub virtual_methods: Vec<EncodedMethod>,
}

fn to_u32(v: u64, r: &Reader, off: usize, reason: &'static str) -> DexResult<u32> {
    if v > u32::MAX as u64 {
        return Err(DexError::BadClassData {
            offset: r.base() + off as u64,
            reason,
        });
    }
    Ok(v as u32)
}

/// Parse a `class_data_item` at `off`, validating running indices against the
/// supplied field/method table sizes.
pub fn parse_class_data(
    r: &Reader,
    off: u32,
    field_count: u32,
    method_count: u32,
) -> DexResult<ClassData> {
    let mut pos = off as usize;
    let (static_fields_size, n) = r.uleb128(pos)?;
    pos += n;
    let (instance_fields_size, n) = r.uleb128(pos)?;
    pos += n;
    let (direct_methods_size, n) = r.uleb128(pos)?;
    pos += n;
    let (virtual_methods_size, n) = r.uleb128(pos)?;
    pos += n;

    let mut data = ClassData::default();

    let read_fields = |pos: &mut usize, count: u64| -> DexResult<Vec<EncodedField>> {
        let mut out = Vec::with_capacity((count as usize).min(65536));
        let mut idx: u32 = 0;
        for _ in 0..count {
            let (diff, n) = r.uleb128(*pos)?;
            *pos += n;
            let diff = to_u32(diff, r, *pos, "field index diff overflow")?;
            idx = idx.checked_add(diff).ok_or(DexError::BadClassData {
                offset: r.base() + *pos as u64,
                reason: "field index overflow",
            })?;
            if idx >= field_count {
                return Err(DexError::IndexOutOfRange {
                    kind: "field",
                    index: idx,
                    size: field_count,
                    offset: r.base() + *pos as u64,
                });
            }
            let (flags, n) = r.uleb128(*pos)?;
            *pos += n;
            let flags = to_u32(flags, r, *pos, "field access flags overflow")?;
            out.push(EncodedField {
                field_idx: idx,
                access_flags: flags,
            });
        }
        Ok(out)
    };

    data.static_fields = read_fields(&mut pos, static_fields_size)?;
    data.instance_fields = read_fields(&mut pos, instance_fields_size)?;

    let read_methods = |pos: &mut usize, count: u64| -> DexResult<Vec<EncodedMethod>> {
        let mut out = Vec::with_capacity((count as usize).min(65536));
        let mut idx: u32 = 0;
        for _ in 0..count {
            let (diff, n) = r.uleb128(*pos)?;
            *pos += n;
            let diff = to_u32(diff, r, *pos, "method index diff overflow")?;
            idx = idx.checked_add(diff).ok_or(DexError::BadClassData {
                offset: r.base() + *pos as u64,
                reason: "method index overflow",
            })?;
            if idx >= method_count {
                return Err(DexError::IndexOutOfRange {
                    kind: "method",
                    index: idx,
                    size: method_count,
                    offset: r.base() + *pos as u64,
                });
            }
            let (flags, n) = r.uleb128(*pos)?;
            *pos += n;
            let flags = to_u32(flags, r, *pos, "method access flags overflow")?;
            let (code_off, n) = r.uleb128(*pos)?;
            *pos += n;
            let code_off = to_u32(code_off, r, *pos, "code offset overflow")?;
            out.push(EncodedMethod {
                method_idx: idx,
                access_flags: flags,
                code_off,
            });
        }
        Ok(out)
    };

    data.direct_methods = read_methods(&mut pos, direct_methods_size)?;
    data.virtual_methods = read_methods(&mut pos, virtual_methods_size)?;

    Ok(data)
}
