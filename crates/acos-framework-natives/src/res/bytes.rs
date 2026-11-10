//! Bounds-checked little-endian readers shared by the ARSC, AXML and string
//! pool parsers. Nothing here indexes a slice directly, so a hostile or fuzzed
//! buffer can only produce an [`ResError`], never a panic.

use crate::res::error::{ResError, Result, RES_ARSC, RES_AXML};

/// Read a little-endian `u16` at an absolute offset.
#[inline]
pub(crate) fn u16_at(data: &[u8], at: usize) -> Result<u16> {
    let b = data
        .get(at..at.saturating_add(2))
        .ok_or_else(|| ResError::new(RES_ARSC, "short read (u16)"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

/// Read a little-endian `u32` at an absolute offset.
#[inline]
pub(crate) fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    let b = data
        .get(at..at.saturating_add(4))
        .ok_or_else(|| ResError::new(RES_ARSC, "short read (u32)"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Read a little-endian `u16` from an AXML buffer.
#[inline]
pub(crate) fn axml_u16(data: &[u8], at: usize) -> Result<u16> {
    let b = data
        .get(at..at.saturating_add(2))
        .ok_or_else(|| ResError::new(RES_AXML, "short read (u16)"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

/// Read a little-endian `u32` from an AXML buffer.
#[inline]
pub(crate) fn axml_u32(data: &[u8], at: usize) -> Result<u32> {
    let b = data
        .get(at..at.saturating_add(4))
        .ok_or_else(|| ResError::new(RES_AXML, "short read (u32)"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Read a fixed-size NUL-terminated UTF-16 string (ARSC package names).
pub(crate) fn utf16_fixed(data: &[u8], at: usize, units: usize) -> Result<String> {
    let mut out = Vec::new();
    for i in 0..units {
        let v = u16_at(data, at + i * 2)?;
        if v == 0 {
            break;
        }
        out.push(v);
    }
    Ok(String::from_utf16_lossy(&out))
}
