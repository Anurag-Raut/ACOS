//! Lazily-decoded `ResStringPool` (Android resource string pool), including
//! UTF-8 / UTF-16 string decoding and styled-span access.
//!
//! Only the header and the offset table are read up front; each string is
//! decoded on demand. This is what keeps the heap cost of a 34 MB
//! `framework-res.apk` at tens of KiB rather than the ~11 MB string blob.

use crate::res::bytes::u16_at;
use crate::res::error::{ResError, Result, RES_STRING_POOL};

/// `RES_STRING_POOL_TYPE`.
pub const RES_STRING_POOL_TYPE: u16 = 0x0001;
/// `SORTED_FLAG`.
pub const SORTED_FLAG: u32 = 1 << 0;
/// `UTF8_FLAG`.
pub const UTF8_FLAG: u32 = 1 << 8;

/// One styled span (`ResStringPool_span`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleSpan {
    /// String-pool index of the span tag (e.g. `b`).
    pub name: u32,
    /// Inclusive first character index.
    pub first_char: u32,
    /// Inclusive last character index.
    pub last_char: u32,
}

/// A non-owning view over a `ResStringPool` inside a larger buffer.
#[derive(Debug, Clone)]
pub struct StringPool<'a> {
    data: &'a [u8],
    start: usize,
    header_size: usize,
    string_count: usize,
    style_count: usize,
    strings_start: usize,
    styles_start: usize,
    is_utf8: bool,
    sorted: bool,
}

impl<'a> StringPool<'a> {
    /// Parse the pool header at `start` in `data`.
    pub fn parse(data: &'a [u8], start: usize) -> Result<Self> {
        let chunk_type = u16_at(data, start)?;
        if chunk_type != RES_STRING_POOL_TYPE {
            return Err(ResError::new(
                RES_STRING_POOL,
                format!("expected string pool chunk, found {chunk_type:#06x}"),
            ));
        }
        let header_size = u16_at(data, start + 2)? as usize;
        if header_size < 28 {
            return Err(ResError::new(
                RES_STRING_POOL,
                "string pool header too small",
            ));
        }
        let string_count = crate::res::bytes::u32_at(data, start + 8)? as usize;
        let style_count = crate::res::bytes::u32_at(data, start + 12)? as usize;
        let flags = crate::res::bytes::u32_at(data, start + 16)?;
        let strings_start = crate::res::bytes::u32_at(data, start + 20)? as usize;
        let styles_start = crate::res::bytes::u32_at(data, start + 24)? as usize;
        // The offset table must fit in the buffer.
        let offsets_end = start
            .checked_add(header_size)
            .and_then(|v| v.checked_add(string_count.saturating_mul(4)))
            .and_then(|v| v.checked_add(style_count.saturating_mul(4)))
            .ok_or_else(|| ResError::new(RES_STRING_POOL, "offset table overflow"))?;
        if offsets_end > data.len() {
            return Err(ResError::new(
                RES_STRING_POOL,
                "string pool offset table out of range",
            ));
        }
        Ok(StringPool {
            data,
            start,
            header_size,
            string_count,
            style_count,
            strings_start,
            styles_start,
            is_utf8: flags & UTF8_FLAG != 0,
            sorted: flags & SORTED_FLAG != 0,
        })
    }

    /// Number of strings.
    pub fn len(&self) -> usize {
        self.string_count
    }

    /// True when the pool has no strings.
    pub fn is_empty(&self) -> bool {
        self.string_count == 0
    }

    /// True when the pool is marked sorted (binary-searchable).
    pub fn is_sorted(&self) -> bool {
        self.sorted
    }

    /// True when strings are stored as UTF-8.
    pub fn is_utf8(&self) -> bool {
        self.is_utf8
    }

    /// Number of styled spans.
    pub fn style_count(&self) -> usize {
        self.style_count
    }

    fn string_offset(&self, idx: usize) -> Result<usize> {
        let at = self.start + self.header_size + idx * 4;
        Ok(crate::res::bytes::u32_at(self.data, at)? as usize)
    }

    fn style_offset(&self, idx: usize) -> Result<usize> {
        let at = self.start + self.header_size + (self.string_count + idx) * 4;
        Ok(crate::res::bytes::u32_at(self.data, at)? as usize)
    }

    /// Decode string `idx`; out-of-range or negative indices yield `None`.
    pub fn get(&self, idx: i32) -> Option<String> {
        if idx < 0 || idx as usize >= self.string_count {
            return None;
        }
        self.get_usize(idx as usize).ok()
    }

    /// Decode string `idx` with an explicit `Result`.
    pub fn get_usize(&self, idx: usize) -> Result<String> {
        if idx >= self.string_count {
            return Err(ResError::new(RES_STRING_POOL, "string index out of range"));
        }
        let off = self.string_offset(idx)?;
        let abs = self
            .start
            .checked_add(self.strings_start)
            .and_then(|v| v.checked_add(off))
            .ok_or_else(|| ResError::new(RES_STRING_POOL, "string offset overflow"))?;
        if self.is_utf8 {
            decode_utf8(self.data, abs)
        } else {
            decode_utf16(self.data, abs)
        }
    }

    /// Decode string `idx` or the empty string.
    pub fn get_or_empty(&self, idx: i32) -> String {
        self.get(idx).unwrap_or_default()
    }

    /// Styled spans for string `idx` (styles are keyed by string index; only the
    /// first `style_count` strings can carry them).
    pub fn get_style(&self, idx: i32) -> Result<Vec<StyleSpan>> {
        if self.style_count == 0 || idx < 0 || idx as usize >= self.style_count {
            return Ok(Vec::new());
        }
        let off = self.style_offset(idx as usize)?;
        let mut pos = self
            .start
            .checked_add(self.styles_start)
            .and_then(|v| v.checked_add(off))
            .ok_or_else(|| ResError::new(RES_STRING_POOL, "style offset overflow"))?;
        let mut spans = Vec::new();
        for _ in 0..1_000_000 {
            if pos + 12 > self.data.len() {
                return Err(ResError::new(RES_STRING_POOL, "style span out of range"));
            }
            let name = crate::res::bytes::u32_at(self.data, pos)?;
            if name == 0xFFFF_FFFF {
                return Ok(spans);
            }
            let first = crate::res::bytes::u32_at(self.data, pos + 4)?;
            let last = crate::res::bytes::u32_at(self.data, pos + 8)?;
            spans.push(StyleSpan {
                name,
                first_char: first,
                last_char: last,
            });
            pos += 12;
        }
        Err(ResError::new(RES_STRING_POOL, "style span chain too long"))
    }
}

/// Decoded `StringBlock` handle payload: a standalone owned string pool.
#[derive(Debug, Clone, Default)]
pub struct StringBlock {
    strings: Vec<String>,
    styles: Vec<Vec<StyleSpan>>,
}

impl StringBlock {
    /// Parse a standalone string pool from `data[offset .. offset+size]`.
    pub fn from_bytes(data: &[u8], offset: usize, size: usize) -> Result<Self> {
        let end = offset
            .checked_add(size)
            .ok_or_else(|| ResError::new(RES_STRING_POOL, "size overflow"))?;
        let slice = data
            .get(offset..end)
            .ok_or_else(|| ResError::new(RES_STRING_POOL, "range out of bounds"))?;
        Self::from_pool(&StringPool::parse(slice, 0)?)
    }

    /// Materialise every string and style from a string pool view.
    pub fn from_pool(pool: &StringPool<'_>) -> Result<Self> {
        let mut strings = Vec::with_capacity(pool.len());
        let mut styles = Vec::with_capacity(pool.style_count());
        for i in 0..pool.len() {
            strings.push(pool.get_usize(i)?);
        }
        for i in 0..pool.style_count() {
            styles.push(pool.get_style(i as i32)?);
        }
        Ok(StringBlock { strings, styles })
    }

    /// Number of strings.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// String at `idx`.
    pub fn get(&self, idx: i32) -> Option<&str> {
        if idx < 0 {
            None
        } else {
            self.strings.get(idx as usize).map(String::as_str)
        }
    }

    /// Styled spans at `idx`.
    pub fn get_style(&self, idx: i32) -> &[StyleSpan] {
        if idx < 0 {
            return &[];
        }
        self.styles
            .get(idx as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

fn read_var_len(data: &[u8], pos: &mut usize) -> Result<usize> {
    let b0 = *data
        .get(*pos)
        .ok_or_else(|| ResError::new(RES_STRING_POOL, "short var-len"))?;
    *pos += 1;
    if b0 & 0x80 != 0 {
        let b1 = *data
            .get(*pos)
            .ok_or_else(|| ResError::new(RES_STRING_POOL, "short var-len"))?;
        *pos += 1;
        Ok(((((b0 & 0x7f) as usize) << 8) | b1 as usize).min(5_000_000))
    } else {
        Ok(b0 as usize)
    }
}

fn decode_utf8(data: &[u8], at: usize) -> Result<String> {
    let mut pos = at;
    let _utf16_len = read_var_len(data, &mut pos)?;
    let byte_len = read_var_len(data, &mut pos)?;
    let bytes = data
        .get(pos..pos.saturating_add(byte_len))
        .ok_or_else(|| ResError::new(RES_STRING_POOL, "utf-8 string out of range"))?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn decode_utf16(data: &[u8], at: usize) -> Result<String> {
    let mut pos = at;
    let l0 = u16_at(data, pos)?;
    pos += 2;
    let len = if l0 & 0x8000 != 0 {
        let l1 = u16_at(data, pos)?;
        pos += 2;
        (((l0 & 0x7fff) as usize) << 16) | l1 as usize
    } else {
        l0 as usize
    };
    if len > 5_000_000 {
        return Err(ResError::new(RES_STRING_POOL, "implausible utf-16 length"));
    }
    let mut units = Vec::with_capacity(len);
    for _ in 0..len {
        units.push(u16_at(data, pos)?);
        pos += 2;
    }
    Ok(String::from_utf16_lossy(&units))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_pool() {
        let err = StringPool::parse(&[0, 0, 0, 0], 0).unwrap_err();
        assert_eq!(err.code(), RES_STRING_POOL);
    }

    #[test]
    fn empty_pool_is_empty() {
        // Minimal valid header with zero strings/styles.
        let mut data = Vec::new();
        data.extend_from_slice(&RES_STRING_POOL_TYPE.to_le_bytes());
        data.extend_from_slice(&28u16.to_le_bytes());
        data.extend_from_slice(&28u32.to_le_bytes());
        for _ in 0..5 {
            data.extend_from_slice(&0u32.to_le_bytes());
        }
        let pool = StringPool::parse(&data, 0).unwrap();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }
}
