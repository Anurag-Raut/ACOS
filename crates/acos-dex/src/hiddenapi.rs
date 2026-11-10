//! `hiddenapi_class_data_item` (map item type `0xF000`) parsing.
//!
//! This is an ART extension present in platform (boot classpath) DEX files.
//! The item layout is:
//!
//! ```text
//! u32  size;                        // total size of the item in bytes
//! u32  offsets[class_defs_size];    // per-class offset from item start
//! u8   flags[];                     // ULEB128 flag values, grouped per class
//! ```
//!
//! Each class's flag group contains one ULEB128 value per field and method of
//! that class, in `class_data_item` order (static fields, instance fields,
//! direct methods, virtual methods). We expose the raw flag values.

use crate::error::{DexError, DexResult};
use crate::reader::Reader;

/// Raw hiddenapi flags for every class, in `class_defs` order.
#[derive(Clone, Debug, Default, PartialEq, borsh::BorshSerialize, borsh::BorshDeserialize)]
pub struct HiddenApiClassData {
    /// `per_class[i]` holds the raw flags for the class at index `i` in
    /// `class_defs` order; the slice length equals that class's field+method
    /// count.
    pub per_class: Vec<Vec<u32>>,
}

/// Parse a `hiddenapi_class_data_item` at `off` for `class_count` classes.
///
/// The item must lie entirely within `r`. Per-class flag groups are decoded
/// with ULEB128 until the next class's offset (or the end of the item).
pub fn parse(r: &Reader, off: u32, class_count: u32) -> DexResult<HiddenApiClassData> {
    let base = off as usize;
    let size = r.u32(base)? as usize;
    if size < 4 {
        return Err(DexError::BadMap {
            offset: r.base() + base as u64,
            reason: "hiddenapi_class_data_item size < 4",
        });
    }
    let offsets_bytes = (class_count as usize)
        .checked_mul(4)
        .ok_or(DexError::Overflow {
            what: "hiddenapi offsets",
        })?;
    if 4 + offsets_bytes > size {
        return Err(DexError::BadMap {
            offset: r.base() + base as u64,
            reason: "hiddenapi_class_data_item too small for class offsets",
        });
    }
    let item_end = base.checked_add(size).ok_or(DexError::Overflow {
        what: "hiddenapi item end",
    })?;
    if item_end > r.len() {
        return Err(DexError::Truncated {
            offset: r.base() + base as u64,
            needed: size,
            len: r.len().saturating_sub(base),
        });
    }

    let mut offsets = Vec::with_capacity(class_count as usize);
    for i in 0..class_count as usize {
        offsets.push(r.u32(base + 4 + i * 4)? as usize);
    }

    // A zero offset means the class has no hiddenapi flags. For a non-zero
    // offset the flag group extends to the next non-zero offset (or the end of
    // the item). Non-zero offsets are strictly increasing.
    let n = class_count as usize;
    let mut next_nonzero = vec![size; n];
    let mut nn = size;
    for i in (0..n).rev() {
        next_nonzero[i] = nn;
        if offsets[i] != 0 {
            nn = offsets[i];
        }
    }

    let mut per_class = Vec::with_capacity(n);
    for i in 0..n {
        if offsets[i] == 0 {
            per_class.push(Vec::new());
            continue;
        }
        let start = base.checked_add(offsets[i]).ok_or(DexError::Overflow {
            what: "hiddenapi class offset",
        })?;
        let end = base
            .checked_add(next_nonzero[i])
            .ok_or(DexError::Overflow {
                what: "hiddenapi class offset",
            })?;
        if start > end || end > item_end {
            return Err(DexError::BadMap {
                offset: r.base() + base as u64,
                reason: "hiddenapi class offset out of range",
            });
        }
        let mut flags = Vec::new();
        let mut p = start;
        while p < end {
            let (v, nbytes) = r.uleb128(p)?;
            if p + nbytes > end {
                return Err(DexError::BadMap {
                    offset: r.base() + p as u64,
                    reason: "hiddenapi flag runs past class group",
                });
            }
            flags.push(v as u32);
            p += nbytes;
        }
        per_class.push(flags);
    }

    Ok(HiddenApiClassData { per_class })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_with_zero_groups() {
        // 3 classes: class0 flags [1, 2], class1 has offset 0 (no flags),
        // class2 flags [3]. Layout: size(4) + 3 offsets(12) + 3 flag bytes.
        let mut buf = vec![0u8; 19];
        buf[0..4].copy_from_slice(&19u32.to_le_bytes());
        buf[4..8].copy_from_slice(&16u32.to_le_bytes());
        buf[8..12].copy_from_slice(&0u32.to_le_bytes());
        buf[12..16].copy_from_slice(&18u32.to_le_bytes());
        buf[16] = 1;
        buf[17] = 2;
        buf[18] = 3;
        let r = Reader::new(&buf);
        let data = parse(&r, 0, 3).unwrap();
        assert_eq!(data.per_class, vec![vec![1, 2], vec![], vec![3]]);
    }

    #[test]
    fn rejects_truncated_item() {
        let buf = vec![0u8; 8];
        let r = Reader::new(&buf);
        assert!(parse(&r, 0, 4).is_err());
    }
}
