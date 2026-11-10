//! `code_item`, try/catch tables, and `debug_info_item` line tables.

use crate::error::{DexError, DexResult};
use crate::reader::Reader;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeAddrPair {
    pub type_idx: u32,
    pub addr: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatchHandler {
    pub handlers: Vec<TypeAddrPair>,
    pub catch_all_addr: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TryItem {
    pub start_addr: u32,
    pub insn_count: u16,
    /// Byte offset from the start of the `encoded_catch_handler_list`.
    pub handler_off: u16,
    /// Index into [`CodeItem::handlers`].
    pub handler_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeItem {
    pub offset: u32,
    pub registers_size: u16,
    pub ins_size: u16,
    pub outs_size: u16,
    pub debug_info_off: u32,
    pub insns: Vec<u16>,
    pub tries: Vec<TryItem>,
    pub handlers: Vec<CatchHandler>,
}

fn to_u32(v: u64, r: &Reader, off: usize, reason: &'static str) -> DexResult<u32> {
    if v > u32::MAX as u64 {
        return Err(DexError::BadCodeItem {
            offset: r.base() + off as u64,
            reason,
        });
    }
    Ok(v as u32)
}

/// Parse a `code_item` at `off`. `type_count` is used to validate catch types.
pub fn parse_code_item(r: &Reader, off: u32, type_count: u32) -> DexResult<CodeItem> {
    let off = off as usize;
    let registers_size = r.u16(off)?;
    let ins_size = r.u16(off + 2)?;
    let outs_size = r.u16(off + 4)?;
    let tries_size = r.u16(off + 6)?;
    let debug_info_off = r.u32(off + 8)?;
    let insns_size = r.u32(off + 12)?;

    let insns_start = off + 16;
    let insns_units = insns_size as usize;
    let insns_end = insns_start
        .checked_add(insns_units.checked_mul(2).ok_or(DexError::Overflow {
            what: "code_item insns size",
        })?)
        .ok_or(DexError::Overflow {
            what: "code_item insns end",
        })?;
    if insns_end > r.len() {
        return Err(DexError::BadCodeItem {
            offset: r.base() + off as u64,
            reason: "insns array extends past end of file",
        });
    }

    let mut insns = Vec::with_capacity(insns_units);
    for i in 0..insns_units {
        insns.push(r.u16(insns_start + i * 2)?);
    }

    let mut tries = Vec::new();
    let mut handlers = Vec::new();

    if tries_size > 0 {
        let mut tries_start = insns_end;
        // 2 bytes of padding if the instruction array has an odd number of units.
        if insns_size % 2 == 1 {
            tries_start += 2;
        }
        let tries_bytes = (tries_size as usize)
            .checked_mul(8)
            .ok_or(DexError::Overflow {
                what: "code_item tries size",
            })?;
        let tries_end = tries_start
            .checked_add(tries_bytes)
            .ok_or(DexError::Overflow {
                what: "code_item tries end",
            })?;
        if tries_end > r.len() {
            return Err(DexError::BadCodeItem {
                offset: r.base() + off as u64,
                reason: "try_item array extends past end of file",
            });
        }

        // The encoded_catch_handler_list starts immediately after the tries.
        let list_start = tries_end;
        let (handler_count, n) = r.uleb128(list_start)?;
        let mut pos = list_start + n;
        let mut handler_starts: Vec<usize> = Vec::with_capacity((handler_count as usize).min(4096));
        for _ in 0..handler_count {
            handler_starts.push(pos - list_start);
            let (size, sn) = r.sleb128(pos)?;
            pos += sn;
            let mut handler = CatchHandler::default();
            if size > 0 {
                let count = size as u64;
                for _ in 0..count {
                    let (t, tn) = r.uleb128(pos)?;
                    pos += tn;
                    let (a, an) = r.uleb128(pos)?;
                    pos += an;
                    let type_idx = to_u32(t, r, pos, "catch type index overflow")?;
                    if type_idx >= type_count {
                        return Err(DexError::IndexOutOfRange {
                            kind: "type",
                            index: type_idx,
                            size: type_count,
                            offset: r.base() + pos as u64,
                        });
                    }
                    handler.handlers.push(TypeAddrPair {
                        type_idx,
                        addr: to_u32(a, r, pos, "catch address overflow")?,
                    });
                }
            } else {
                let count = size.unsigned_abs();
                for _ in 0..count {
                    let (t, tn) = r.uleb128(pos)?;
                    pos += tn;
                    let (a, an) = r.uleb128(pos)?;
                    pos += an;
                    let type_idx = to_u32(t, r, pos, "catch type index overflow")?;
                    if type_idx >= type_count {
                        return Err(DexError::IndexOutOfRange {
                            kind: "type",
                            index: type_idx,
                            size: type_count,
                            offset: r.base() + pos as u64,
                        });
                    }
                    handler.handlers.push(TypeAddrPair {
                        type_idx,
                        addr: to_u32(a, r, pos, "catch address overflow")?,
                    });
                }
                let (a, an) = r.uleb128(pos)?;
                pos += an;
                handler.catch_all_addr = Some(to_u32(a, r, pos, "catch-all address overflow")?);
            }
            handlers.push(handler);
        }

        let list_len = pos - list_start;
        for i in 0..tries_size as usize {
            let t = tries_start + i * 8;
            let start_addr = r.u32(t)?;
            let insn_count = r.u16(t + 4)?;
            let handler_off = r.u16(t + 6)?;
            let handler_index = handler_starts
                .iter()
                .position(|&s| s == handler_off as usize)
                .ok_or(DexError::BadCodeItem {
                    offset: r.base() + t as u64,
                    reason: "try_item handler_off does not point at a catch handler",
                })?;
            if handler_off as usize >= list_len {
                return Err(DexError::BadCodeItem {
                    offset: r.base() + t as u64,
                    reason: "try_item handler_off outside handler list",
                });
            }
            tries.push(TryItem {
                start_addr,
                insn_count,
                handler_off,
                handler_index,
            });
        }
    }

    Ok(CodeItem {
        offset: off as u32,
        registers_size,
        ins_size,
        outs_size,
        debug_info_off,
        insns,
        tries,
        handlers,
    })
}

/// Parse a `debug_info_item` into `(address, line)` pairs.
///
/// Returns an empty table when `debug_info_off == 0`. Position entries are
/// emitted in the order they appear (which is monotonically non-decreasing in
/// address for well-formed input).
pub fn line_table(r: &Reader, debug_info_off: u32) -> DexResult<Vec<(u32, u32)>> {
    if debug_info_off == 0 {
        return Ok(Vec::new());
    }
    let mut pos = debug_info_off as usize;

    let (line_start, n) = r.uleb128(pos)?;
    pos += n;
    let (parameters_size, n) = r.uleb128(pos)?;
    pos += n;
    for _ in 0..parameters_size {
        let (_name, n) = r.uleb128p1(pos)?;
        pos += n;
    }

    let mut address: u64 = 0;
    let mut line: i64 = line_start as i64;
    let mut out = Vec::new();
    // Hard cap on emitted entries to bound memory on malformed input.
    const MAX_ENTRIES: usize = 4_000_000;

    loop {
        let opcode = r.u8(pos)?;
        pos += 1;
        match opcode {
            0x00 => break, // DBG_END_SEQUENCE
            0x01 => {
                let (diff, n) = r.uleb128(pos)?;
                pos += n;
                address = address.checked_add(diff).ok_or(DexError::BadDebugInfo {
                    offset: r.base() + pos as u64,
                    reason: "address overflow",
                })?;
            }
            0x02 => {
                let (diff, n) = r.sleb128(pos)?;
                pos += n;
                line = line.checked_add(diff).ok_or(DexError::BadDebugInfo {
                    offset: r.base() + pos as u64,
                    reason: "line overflow",
                })?;
            }
            0x03 => {
                // DBG_START_LOCAL: register, name, type
                for _ in 0..3 {
                    let (_v, n) = r.uleb128(pos)?;
                    pos += n;
                }
            }
            0x04 => {
                // DBG_START_LOCAL_EXTENDED: register, name, type, signature
                for _ in 0..4 {
                    let (_v, n) = r.uleb128(pos)?;
                    pos += n;
                }
            }
            0x05 | 0x06 => {
                // DBG_END_LOCAL / DBG_RESTART_LOCAL: register
                let (_v, n) = r.uleb128(pos)?;
                pos += n;
            }
            0x07 | 0x08 => {}
            0x09 => {
                // DBG_SET_FILE
                let (_v, n) = r.uleb128p1(pos)?;
                pos += n;
            }
            special => {
                let adjusted = (special - 0x0a) as u64;
                address = address
                    .checked_add(adjusted / 15)
                    .ok_or(DexError::BadDebugInfo {
                        offset: r.base() + pos as u64,
                        reason: "address overflow",
                    })?;
                line += -4 + (adjusted % 15) as i64;
                if address > u32::MAX as u64 || line < 0 || line > u32::MAX as i64 {
                    return Err(DexError::BadDebugInfo {
                        offset: r.base() + pos as u64,
                        reason: "position out of range",
                    });
                }
                if out.len() >= MAX_ENTRIES {
                    return Err(DexError::BadDebugInfo {
                        offset: r.base() + pos as u64,
                        reason: "too many position entries",
                    });
                }
                out.push((address as u32, line as u32));
            }
        }
    }

    Ok(out)
}
