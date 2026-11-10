//! Low-level bounds-checked readers shared by the AXML and ARSC parsers.
//!
//! Nothing here indexes a slice directly; every read returns
//! [`ApkError`] on a short buffer. This is what keeps malformed/fuzzed input
//! panic-free.

use crate::error::{ApkError, Result};

/// Resource string pool chunk type (`RES_STRING_POOL_TYPE`).
pub(crate) const RES_STRING_POOL_TYPE: u16 = 0x0001;

/// A cursor over a byte slice with checked reads.
pub(crate) struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    pub(crate) fn at(data: &'a [u8], pos: usize) -> Self {
        Cursor { data, pos }
    }

    pub(crate) fn slice(&mut self, n: usize) -> Result<&'a [u8]> {
        let next = self
            .pos
            .checked_add(n)
            .ok_or_else(|| ApkError::new(crate::error::AXML_MALFORMED, "offset overflow"))?;
        let out = self.data.get(self.pos..next).ok_or_else(|| {
            ApkError::new(crate::error::AXML_MALFORMED, "unexpected end of buffer")
        })?;
        self.pos = next;
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        let b = *self.data.get(self.pos).ok_or_else(|| {
            ApkError::new(crate::error::AXML_MALFORMED, "unexpected end of buffer")
        })?;
        self.pos += 1;
        Ok(b)
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        let b = self.slice(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        let b = self.slice(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Read a little-endian `u16` at an absolute offset.
pub(crate) fn read_u16(data: &[u8], at: usize) -> Result<u16> {
    let b = data
        .get(at..at + 2)
        .ok_or_else(|| ApkError::new(crate::error::AXML_MALFORMED, "short read (u16)"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

/// Read a little-endian `u32` at an absolute offset.
pub(crate) fn read_u32(data: &[u8], at: usize) -> Result<u32> {
    let b = data
        .get(at..at + 4)
        .ok_or_else(|| ApkError::new(crate::error::AXML_MALFORMED, "short read (u32)"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A decoded `RES_STRING_POOL_TYPE` chunk.
#[derive(Debug, Clone, Default)]
pub(crate) struct StringPool {
    strings: Vec<String>,
}

impl StringPool {
    /// Parse a string pool chunk whose header begins at `start`.
    pub(crate) fn parse(data: &[u8], start: usize) -> Result<Self> {
        let mut c = Cursor::at(data, start);
        let chunk_type = c.u16()?;
        let header_size = c.u16()? as usize;
        let chunk_size = c.u32()? as usize;
        if chunk_type != RES_STRING_POOL_TYPE {
            return Err(ApkError::new(
                crate::error::AXML_MALFORMED,
                format!("expected string pool chunk, found 0x{chunk_type:04x}"),
            ));
        }
        if header_size < 28 {
            return Err(ApkError::new(
                crate::error::AXML_MALFORMED,
                "string pool header too small",
            ));
        }
        let string_count = c.u32()? as usize;
        let _style_count = c.u32()? as usize;
        let flags = c.u32()?;
        let strings_start = c.u32()? as usize;
        let _styles_start = c.u32()?;

        let is_utf8 = flags & 0x100 != 0;
        let offsets_base = start
            .checked_add(header_size)
            .ok_or_else(|| ApkError::new(crate::error::AXML_MALFORMED, "offset overflow"))?;
        let strings_base = start
            .checked_add(strings_start)
            .ok_or_else(|| ApkError::new(crate::error::AXML_MALFORMED, "offset overflow"))?;
        let _ = chunk_size;

        // Sanity cap so a hostile stringCount cannot allocate unbounded memory.
        if string_count > 5_000_000 {
            return Err(ApkError::new(
                crate::error::AXML_MALFORMED,
                format!("implausible string count {string_count}"),
            ));
        }

        let mut strings = Vec::with_capacity(string_count);
        for i in 0..string_count {
            let off_at = offsets_base + i * 4;
            let off = read_u32(data, off_at)? as usize;
            let abs = strings_base
                .checked_add(off)
                .ok_or_else(|| ApkError::new(crate::error::AXML_MALFORMED, "offset overflow"))?;
            let s = if is_utf8 {
                read_utf8_string(data, abs)?
            } else {
                read_utf16_string(data, abs)?
            };
            strings.push(s);
        }
        Ok(StringPool { strings })
    }

    /// Resolve a string index; `0xFFFF_FFFF` and out-of-range indices yield
    /// `None`.
    pub(crate) fn get(&self, idx: u32) -> Option<&str> {
        if idx == u32::MAX {
            None
        } else {
            self.strings.get(idx as usize).map(String::as_str)
        }
    }

    pub(crate) fn get_or_empty(&self, idx: u32) -> &str {
        self.get(idx).unwrap_or("")
    }

    pub(crate) fn strings(&self) -> &[String] {
        &self.strings
    }
}

fn read_utf8_string(data: &[u8], off: usize) -> Result<String> {
    let mut c = Cursor::at(data, off);
    // Android's UTF-8 string pool stores two lengths before the data: the
    // length in UTF-16 code units, then the length in UTF-8 bytes. Each uses a
    // 1- or 2-byte high-bit-continuation encoding.
    let _utf16_len = read_var_len(&mut c)?;
    let byte_len = read_var_len(&mut c)?;
    let bytes = c.slice(byte_len)?;
    // Android uses modified UTF-8; `from_utf8_lossy` never panics and keeps
    // the common (standard UTF-8) case byte-exact.
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn read_var_len(c: &mut Cursor<'_>) -> Result<usize> {
    let b0 = c.u8()?;
    if b0 & 0x80 != 0 {
        let b1 = c.u8()?;
        Ok(((((b0 & 0x7f) as usize) << 8) | b1 as usize).min(5_000_000))
    } else {
        Ok(b0 as usize)
    }
}

fn read_utf16_string(data: &[u8], off: usize) -> Result<String> {
    let mut c = Cursor::at(data, off);
    let l0 = c.u16()?;
    let len = if l0 & 0x8000 != 0 {
        let l1 = c.u16()?;
        (((l0 & 0x7fff) as usize) << 16) | l1 as usize
    } else {
        l0 as usize
    };
    if len > 5_000_000 {
        return Err(ApkError::new(
            crate::error::AXML_MALFORMED,
            format!("implausible utf-16 string length {len}"),
        ));
    }
    let mut units = Vec::with_capacity(len);
    for _ in 0..len {
        units.push(c.u16()?);
    }
    Ok(String::from_utf16_lossy(&units))
}
