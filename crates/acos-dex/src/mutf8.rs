//! Decoding of DEX "Modified UTF-8" (MUTF-8).
//!
//! DEX strings use a variant of UTF-8 with two differences from standard UTF-8:
//!
//! * U+0000 is encoded as the two-byte sequence `0xC0 0x80`, never as a literal
//!   zero byte (which terminates the string).
//! * Supplementary characters (above U+FFFF) are stored as a UTF-16 surrogate
//!   pair, with each surrogate encoded as a 3-byte sequence (CESU-8 style).
//!
//! Decoding therefore produces UTF-16 code units first and then recombines
//! surrogate pairs into `char`s. Lone surrogates are rejected.

use crate::error::{DexError, DexResult};

/// Decode a NUL-terminated MUTF-8 string starting at `bytes[0]`.
///
/// Returns the decoded `String` and the number of bytes consumed, including the
/// terminating NUL byte. `base_offset` is used only for error reporting.
pub fn decode(bytes: &[u8], base_offset: u64) -> DexResult<(String, usize)> {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;

    loop {
        let b = match bytes.get(i) {
            Some(&b) => b,
            None => {
                return Err(DexError::BadMutf8 {
                    offset: base_offset + i as u64,
                    reason: "missing NUL terminator",
                })
            }
        };
        if b == 0 {
            i += 1;
            break;
        }

        if b < 0x80 {
            units.push(b as u16);
            i += 1;
        } else if b & 0xE0 == 0xC0 {
            let b1 = *bytes.get(i + 1).ok_or(DexError::BadMutf8 {
                offset: base_offset + i as u64,
                reason: "truncated 2-byte sequence",
            })?;
            if b1 & 0xC0 != 0x80 {
                return Err(DexError::BadMutf8 {
                    offset: base_offset + i as u64 + 1,
                    reason: "bad continuation byte in 2-byte sequence",
                });
            }
            let unit = (((b & 0x1F) as u16) << 6) | ((b1 & 0x3F) as u16);
            // Overlong sequences are only legal for U+0000 (0xC0 0x80).
            if unit != 0 && unit < 0x80 {
                return Err(DexError::BadMutf8 {
                    offset: base_offset + i as u64,
                    reason: "overlong 2-byte sequence",
                });
            }
            units.push(unit);
            i += 2;
        } else if b & 0xF0 == 0xE0 {
            let b1 = *bytes.get(i + 1).ok_or(DexError::BadMutf8 {
                offset: base_offset + i as u64,
                reason: "truncated 3-byte sequence",
            })?;
            let b2 = *bytes.get(i + 2).ok_or(DexError::BadMutf8 {
                offset: base_offset + i as u64,
                reason: "truncated 3-byte sequence",
            })?;
            if b1 & 0xC0 != 0x80 || b2 & 0xC0 != 0x80 {
                return Err(DexError::BadMutf8 {
                    offset: base_offset + i as u64 + 1,
                    reason: "bad continuation byte in 3-byte sequence",
                });
            }
            let unit =
                (((b & 0x0F) as u16) << 12) | (((b1 & 0x3F) as u16) << 6) | ((b2 & 0x3F) as u16);
            if unit < 0x800 {
                return Err(DexError::BadMutf8 {
                    offset: base_offset + i as u64,
                    reason: "overlong 3-byte sequence",
                });
            }
            units.push(unit);
            i += 3;
        } else {
            return Err(DexError::BadMutf8 {
                offset: base_offset + i as u64,
                reason: "invalid lead byte",
            });
        }
    }

    // Combine surrogate pairs.
    let mut s = String::with_capacity(units.len());
    let mut k = 0usize;
    while k < units.len() {
        let u = units[k];
        if (0xD800..=0xDBFF).contains(&u) {
            let low = match units.get(k + 1) {
                Some(&l) if (0xDC00..=0xDFFF).contains(&l) => l,
                _ => {
                    return Err(DexError::BadMutf8 {
                        offset: base_offset,
                        reason: "unpaired high surrogate",
                    })
                }
            };
            let c = 0x10000 + (((u as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
            let ch = char::from_u32(c).ok_or(DexError::BadMutf8 {
                offset: base_offset,
                reason: "invalid supplementary code point",
            })?;
            s.push(ch);
            k += 2;
        } else if (0xDC00..=0xDFFF).contains(&u) {
            return Err(DexError::BadMutf8 {
                offset: base_offset,
                reason: "unpaired low surrogate",
            });
        } else {
            // Non-surrogate BMP unit; from_u32 cannot fail here.
            let ch = char::from_u32(u as u32).ok_or(DexError::BadMutf8 {
                offset: base_offset,
                reason: "invalid BMP code unit",
            })?;
            s.push(ch);
            k += 1;
        }
    }

    Ok((s, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii() {
        let (s, n) = decode(b"hello\0", 0).unwrap();
        assert_eq!(s, "hello");
        assert_eq!(n, 6);
    }

    #[test]
    fn embedded_nul() {
        // "a\0b"
        let (s, n) = decode(b"a\xc0\x80b\0", 0).unwrap();
        assert_eq!(s, "a\0b");
        assert_eq!(n, 5);
    }

    #[test]
    fn non_ascii_bmp() {
        // U+00E9 (é) = C3 A9, U+4E2D (中) = E4 B8 AD
        let (s, _) = decode(b"\xc3\xa9\xe4\xb8\xad\0", 0).unwrap();
        assert_eq!(s, "é中");
    }

    #[test]
    fn supplementary() {
        // U+1F600 = surrogate pair D83D DE00, each encoded 3 bytes.
        let (s, _) = decode(b"\xed\xa0\xbd\xed\xb8\x80\0", 0).unwrap();
        assert_eq!(s, "\u{1F600}");
    }

    #[test]
    fn missing_nul() {
        assert!(decode(b"abc", 0).is_err());
    }

    #[test]
    fn lone_surrogate() {
        assert!(decode(b"\xed\xa0\xbd\0", 0).is_err());
    }
}
