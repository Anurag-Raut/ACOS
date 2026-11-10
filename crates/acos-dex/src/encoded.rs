//! `encoded_value` and `encoded_array` decoding.
//!
//! Used for `static_values`, annotation elements, and `call_site_id` targets.

use crate::error::{DexError, DexResult};
use crate::reader::Reader;

/// Maximum nesting depth for arrays/annotations, to bound recursion on hostile
/// input (a stack overflow would be an abort, not a structured error).
const MAX_DEPTH: u32 = 64;

#[derive(Clone, Debug, PartialEq)]
pub enum EncodedValue {
    Byte(i8),
    Short(i16),
    Char(u16),
    Int(i32),
    Long(i64),
    /// Raw IEEE-754 single bits (right-zero-extended in the file).
    Float(u32),
    /// Raw IEEE-754 double bits (right-zero-extended in the file).
    Double(u64),
    MethodType(u32),
    MethodHandle(u32),
    String(u32),
    Type(u32),
    Field(u32),
    Method(u32),
    Enum(u32),
    Array(Vec<EncodedValue>),
    Annotation(EncodedAnnotation),
    Null,
    Boolean(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncodedAnnotation {
    pub type_idx: u32,
    pub elements: Vec<AnnotationElement>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationElement {
    pub name_idx: u32,
    pub value: EncodedValue,
}

fn read_uint(r: &Reader, off: usize, size: usize) -> DexResult<u64> {
    let b = r.bytes(off, size)?;
    let mut v = 0u64;
    for (i, &byte) in b.iter().enumerate() {
        v |= (byte as u64) << (8 * i);
    }
    Ok(v)
}

fn sign_extend(raw: u64, bits: u32) -> i64 {
    if bits == 0 {
        return 0;
    }
    if bits >= 64 {
        return raw as i64;
    }
    let shift = 64 - bits;
    ((raw << shift) as i64) >> shift
}

/// Decode one `encoded_value` at `off`, returning the value and bytes consumed.
pub fn decode_value(r: &Reader, off: usize, depth: u32) -> DexResult<(EncodedValue, usize)> {
    if depth > MAX_DEPTH {
        return Err(DexError::BadEncodedValue {
            offset: r.base() + off as u64,
            reason: "nesting too deep",
        });
    }
    let header = r.u8(off)?;
    // Header layout: high 3 bits are value_arg, low 5 bits are value_type.
    let value_type = header & 0x1f;
    let value_arg = (header >> 5) as u32;
    let size = (value_arg + 1) as usize;
    let abs = r.base() + off as u64;

    // Fixed-width value types encode at most 8 bytes; anything larger is
    // malformed and would otherwise overflow the shift below.
    if !matches!(value_type, 0x1c..=0x1f) && size > 8 {
        return Err(DexError::BadEncodedValue {
            offset: abs,
            reason: "fixed-width value larger than 8 bytes",
        });
    }

    let result = match value_type {
        0x00 => EncodedValue::Byte(sign_extend(read_uint(r, off + 1, size)?, (size * 8) as u32) as i8),
        0x02 => {
            EncodedValue::Short(sign_extend(read_uint(r, off + 1, size)?, (size * 8) as u32) as i16)
        }
        0x03 => EncodedValue::Char(read_uint(r, off + 1, size)? as u16),
        0x04 => {
            EncodedValue::Int(sign_extend(read_uint(r, off + 1, size)?, (size * 8) as u32) as i32)
        }
        0x06 => {
            EncodedValue::Long(sign_extend(read_uint(r, off + 1, size)?, (size * 8) as u32))
        }
        0x10 => EncodedValue::Float(read_uint(r, off + 1, size)? as u32),
        0x11 => EncodedValue::Double(read_uint(r, off + 1, size)?),
        0x15 => EncodedValue::MethodType(read_uint(r, off + 1, size)? as u32),
        0x16 => EncodedValue::MethodHandle(read_uint(r, off + 1, size)? as u32),
        0x17 => EncodedValue::String(read_uint(r, off + 1, size)? as u32),
        0x18 => EncodedValue::Type(read_uint(r, off + 1, size)? as u32),
        0x19 => EncodedValue::Field(read_uint(r, off + 1, size)? as u32),
        0x1a => EncodedValue::Method(read_uint(r, off + 1, size)? as u32),
        0x1b => EncodedValue::Enum(read_uint(r, off + 1, size)? as u32),
        0x1c => {
            if value_arg != 0 {
                return Err(DexError::BadEncodedValue {
                    offset: abs,
                    reason: "VALUE_ARRAY with non-zero value_arg",
                });
            }
            let (count, n) = r.uleb128(off + 1)?;
            let mut pos = off + 1 + n;
            let mut values = Vec::with_capacity(count.min(65536) as usize);
            for _ in 0..count {
                let (v, used) = decode_value(r, pos, depth + 1)?;
                pos += used;
                values.push(v);
            }
            return Ok((EncodedValue::Array(values), pos - off));
        }
        0x1d => {
            if value_arg != 0 {
                return Err(DexError::BadEncodedValue {
                    offset: abs,
                    reason: "VALUE_ANNOTATION with non-zero value_arg",
                });
            }
            let (ann, used) = decode_annotation(r, off + 1, depth + 1)?;
            return Ok((EncodedValue::Annotation(ann), 1 + used));
        }
        0x1e => {
            if value_arg != 0 {
                return Err(DexError::BadEncodedValue {
                    offset: abs,
                    reason: "VALUE_NULL with non-zero value_arg",
                });
            }
            EncodedValue::Null
        }
        0x1f => {
            if value_arg > 1 {
                return Err(DexError::BadEncodedValue {
                    offset: abs,
                    reason: "VALUE_BOOLEAN with value_arg > 1",
                });
            }
            EncodedValue::Boolean(value_arg == 1)
        }
        other => {
            return Err(DexError::BadEncodedValue {
                offset: abs,
                reason: match other {
                    0x01 => "reserved value type 0x01",
                    0x05 => "reserved value type 0x05",
                    0x07..=0x0f => "reserved value type",
                    0x12..=0x14 => "reserved value type",
                    _ => "unknown value type",
                },
            })
        }
    };

    let consumed = match result {
        EncodedValue::Null | EncodedValue::Boolean(_) => 1,
        _ => 1 + size,
    };
    Ok((result, consumed))
}

/// Decode an `encoded_annotation` (not including any visibility byte).
pub fn decode_annotation(r: &Reader, off: usize, depth: u32) -> DexResult<(EncodedAnnotation, usize)> {
    if depth > MAX_DEPTH {
        return Err(DexError::BadEncodedValue {
            offset: r.base() + off as u64,
            reason: "annotation nesting too deep",
        });
    }
    let (type_idx, n1) = r.uleb128(off)?;
    let (size, n2) = r.uleb128(off + n1)?;
    let mut pos = off + n1 + n2;
    let mut elements = Vec::with_capacity(size.min(65536) as usize);
    for _ in 0..size {
        let (name_idx, nn) = r.uleb128(pos)?;
        pos += nn;
        let (value, used) = decode_value(r, pos, depth + 1)?;
        pos += used;
        elements.push(AnnotationElement {
            name_idx: name_idx as u32,
            value,
        });
    }
    Ok((
        EncodedAnnotation {
            type_idx: type_idx as u32,
            elements,
        },
        pos - off,
    ))
}

/// Decode an `encoded_array_item` at `off` (uleb128 size followed by values).
pub fn decode_array(r: &Reader, off: usize) -> DexResult<(Vec<EncodedValue>, usize)> {
    let (size, n) = r.uleb128(off)?;
    let mut pos = off + n;
    let mut values = Vec::with_capacity(size.min(65536) as usize);
    for _ in 0..size {
        let (v, used) = decode_value(r, pos, 0)?;
        pos += used;
        values.push(v);
    }
    Ok((values, pos - off))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(bytes: &[u8]) -> EncodedValue {
        let r = Reader::new(bytes);
        decode_value(&r, 0, 0).unwrap().0
    }

    #[test]
    fn boolean_and_null() {
        assert_eq!(dec(&[0x1f]), EncodedValue::Boolean(false));
        assert_eq!(dec(&[0x3f]), EncodedValue::Boolean(true));
        assert_eq!(dec(&[0x1e]), EncodedValue::Null);
    }

    #[test]
    fn ints_sign_extend() {
        // Header = (value_arg << 5) | value_type.
        // VALUE_INT (0x04), size 1 (arg 0), 0xff -> -1
        assert_eq!(dec(&[0x04, 0xff]), EncodedValue::Int(-1));
        // VALUE_BYTE (0x00), 0x80 -> -128
        assert_eq!(dec(&[0x00, 0x80]), EncodedValue::Byte(-128));
        // VALUE_CHAR (0x03) zero-extends: 0xff -> 255
        assert_eq!(dec(&[0x03, 0xff]), EncodedValue::Char(255));
        // VALUE_STRING (0x17) with a 2-byte index (arg 1).
        assert_eq!(dec(&[0x37, 0x34, 0x12]), EncodedValue::String(0x1234));
    }
}
