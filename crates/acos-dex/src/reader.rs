//! Bounds-checked little-endian byte reader.
//!
//! All reads are checked; a short read produces [`DexError::Truncated`]. The
//! reader tracks a `base` offset so that sub-slices report absolute file offsets
//! in errors.

use crate::error::{DexError, DexResult};

#[derive(Clone, Copy, Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    base: u64,
}

impl<'a> Reader<'a> {
    pub const fn new(data: &'a [u8]) -> Self {
        Reader { data, base: 0 }
    }

    pub const fn with_base(data: &'a [u8], base: u64) -> Self {
        Reader { data, base }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    pub const fn base(&self) -> u64 {
        self.base
    }

    #[inline]
    pub const fn as_slice(&self) -> &'a [u8] {
        self.data
    }

    #[inline]
    fn abs(&self, off: usize) -> u64 {
        self.base + off as u64
    }

    /// Borrow a sub-slice `[off, off+len)` as a reader with an adjusted base.
    pub fn sub(&self, off: usize, len: usize) -> DexResult<Reader<'a>> {
        let end = off.checked_add(len).ok_or(DexError::Overflow {
            what: "reader sub-slice",
        })?;
        if end > self.data.len() {
            return Err(DexError::Truncated {
                offset: self.abs(off),
                needed: len,
                len: self.data.len().saturating_sub(off),
            });
        }
        Ok(Reader {
            data: &self.data[off..end],
            base: self.base + off as u64,
        })
    }

    /// Borrow the remaining bytes from `off` to the end of this reader.
    pub fn sub_to_end(&self, off: usize) -> DexResult<Reader<'a>> {
        if off > self.data.len() {
            return Err(DexError::Truncated {
                offset: self.abs(off),
                needed: 0,
                len: self.data.len(),
            });
        }
        Ok(Reader {
            data: &self.data[off..],
            base: self.base + off as u64,
        })
    }

    #[inline]
    pub fn u8(&self, off: usize) -> DexResult<u8> {
        self.data.get(off).copied().ok_or(DexError::Truncated {
            offset: self.abs(off),
            needed: 1,
            len: self.data.len(),
        })
    }

    #[inline]
    pub fn u16(&self, off: usize) -> DexResult<u16> {
        let b = self
            .data
            .get(off..off.saturating_add(2))
            .ok_or(DexError::Truncated {
                offset: self.abs(off),
                needed: 2,
                len: self.data.len(),
            })?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    #[inline]
    pub fn i16(&self, off: usize) -> DexResult<i16> {
        Ok(self.u16(off)? as i16)
    }

    #[inline]
    pub fn u32(&self, off: usize) -> DexResult<u32> {
        let b = self
            .data
            .get(off..off.saturating_add(4))
            .ok_or(DexError::Truncated {
                offset: self.abs(off),
                needed: 4,
                len: self.data.len(),
            })?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    #[inline]
    pub fn i32(&self, off: usize) -> DexResult<i32> {
        Ok(self.u32(off)? as i32)
    }

    #[inline]
    pub fn u64(&self, off: usize) -> DexResult<u64> {
        let b = self
            .data
            .get(off..off.saturating_add(8))
            .ok_or(DexError::Truncated {
                offset: self.abs(off),
                needed: 8,
                len: self.data.len(),
            })?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    #[inline]
    pub fn bytes(&self, off: usize, len: usize) -> DexResult<&'a [u8]> {
        let end = off.checked_add(len).ok_or(DexError::Overflow {
            what: "reader bytes",
        })?;
        self.data.get(off..end).ok_or(DexError::Truncated {
            offset: self.abs(off),
            needed: len,
            len: self.data.len().saturating_sub(off),
        })
    }

    /// Read a ULEB128 value, returning `(value, bytes_consumed)`.
    pub fn uleb128(&self, off: usize) -> DexResult<(u64, usize)> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        let mut pos = off;
        loop {
            let byte = self.u8(pos)?;
            pos += 1;
            if shift == 63 && byte > 1 {
                return Err(DexError::BadLeb128 {
                    offset: self.abs(off),
                    reason: "uleb128 overflow",
                });
            }
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok((result, pos - off));
            }
            shift += 7;
            if shift > 63 {
                return Err(DexError::BadLeb128 {
                    offset: self.abs(off),
                    reason: "uleb128 too long",
                });
            }
        }
    }

    /// Read a SLEB128 value, returning `(value, bytes_consumed)`.
    pub fn sleb128(&self, off: usize) -> DexResult<(i64, usize)> {
        let mut result: i64 = 0;
        let mut shift: u32 = 0;
        let mut pos = off;
        loop {
            let byte = self.u8(pos)?;
            pos += 1;
            result |= ((byte & 0x7f) as i64) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && (byte & 0x40) != 0 {
                    result |= -1i64 << shift;
                }
                return Ok((result, pos - off));
            }
            if shift > 63 {
                return Err(DexError::BadLeb128 {
                    offset: self.abs(off),
                    reason: "sleb128 too long",
                });
            }
        }
    }

    /// Read a `uleb128p1` value: `uleb128 - 1`, with `0` meaning "no index"
    /// (`-1`). Returns the raw `i64` so callers can distinguish `NO_INDEX`.
    pub fn uleb128p1(&self, off: usize) -> DexResult<(i64, usize)> {
        let (v, n) = self.uleb128(off)?;
        Ok((v as i64 - 1, n))
    }
}
