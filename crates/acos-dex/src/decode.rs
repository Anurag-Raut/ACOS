//! Dalvik instruction decoder.
//!
//! [`decode`] decodes one instruction at a given code-unit index. It validates
//! the opcode and that the full instruction (including operands) fits within the
//! supplied code array. Switch and array-data payloads are not instructions;
//! they are decoded by [`decode_payload`] / measured by [`payload_len`].

use crate::error::{DexError, DexResult};
use crate::opcode::{Format, Opcode};

/// A decoded Dalvik instruction.
///
/// Fields are only meaningful for the formats that use them:
///
/// * `regs[0..reg_count]` holds register operands in the order they should be
///   displayed. For range formats (`3rc`, `4rcc`) `regs[0]` is the first
///   register and `reg_count` is the number of consecutive registers.
/// * `literal` holds the sign-extended immediate for literal formats.
/// * `index` holds the string/type/field/method/proto/call-site index.
/// * `index2` holds the proto index for `invoke-polymorphic`.
/// * `target` holds the relative branch/payload offset in code units from the
///   start of this instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub opcode: Opcode,
    pub format: Format,
    pub size: u8,
    pub regs: [u16; 5],
    pub reg_count: u8,
    pub literal: i64,
    pub index: u32,
    pub index2: u32,
    pub target: i32,
}

impl Instruction {
    /// Size of this instruction in 16-bit code units.
    #[inline]
    pub const fn size_in_code_units(&self) -> usize {
        self.size as usize
    }

    /// Absolute register ranges touched by this instruction (used by the
    /// verifier). For range formats the full range is returned.
    pub fn register_operands(&self) -> impl Iterator<Item = u16> + '_ {
        let count = self.reg_count as usize;
        (0..count).map(move |i| self.regs[i])
    }
}

#[inline]
const fn sx4(v: u32) -> i32 {
    (((v & 0xf) as i32) << 28) >> 28
}

#[inline]
const fn sx8(v: u32) -> i32 {
    (((v & 0xff) as i32) << 24) >> 24
}

/// Decode the instruction starting at code-unit index `pc`.
pub fn decode(insns: &[u16], pc: usize) -> DexResult<Instruction> {
    let w0 = *insns.get(pc).ok_or(DexError::BadInstruction {
        pc: pc as u32,
        opcode: 0,
        reason: "pc out of range",
    })? as u32;

    let op_byte = (w0 & 0xff) as u8;
    let high = (w0 >> 8) as u8;

    if op_byte == 0 && high != 0 {
        return Err(DexError::BadInstruction {
            pc: pc as u32,
            opcode: 0,
            reason: "payload data where an instruction was expected",
        });
    }

    let opcode = Opcode::from_u8(op_byte).ok_or(DexError::UnusedOpcode {
        pc: pc as u32,
        opcode: op_byte,
    })?;
    let format = opcode.format();
    let size = format.size();

    if pc.checked_add(size).is_none_or(|end| end > insns.len()) {
        return Err(DexError::BadInstruction {
            pc: pc as u32,
            opcode: op_byte,
            reason: "truncated instruction at end of code",
        });
    }

    let w1 = if size > 1 { insns[pc + 1] as u32 } else { 0 };
    let w2 = if size > 2 { insns[pc + 2] as u32 } else { 0 };
    let w3 = if size > 3 { insns[pc + 3] as u32 } else { 0 };
    let w4 = if size > 4 { insns[pc + 4] as u32 } else { 0 };

    let mut insn = Instruction {
        opcode,
        format,
        size: size as u8,
        regs: [0; 5],
        reg_count: 0,
        literal: 0,
        index: 0,
        index2: 0,
        target: 0,
    };

    match format {
        Format::F10x => {}
        Format::F12x => {
            insn.regs[0] = ((w0 >> 8) & 0xf) as u16;
            insn.regs[1] = ((w0 >> 12) & 0xf) as u16;
            insn.reg_count = 2;
        }
        Format::F11n => {
            insn.regs[0] = ((w0 >> 8) & 0xf) as u16;
            insn.reg_count = 1;
            insn.literal = sx4(w0 >> 12) as i64;
        }
        Format::F11x => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.reg_count = 1;
        }
        Format::F10t => {
            insn.target = sx8(w0 >> 8);
        }
        Format::F20t => {
            insn.target = (w1 as u16 as i16) as i32;
        }
        Format::F22x => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.regs[1] = (w1 & 0xffff) as u16;
            insn.reg_count = 2;
        }
        Format::F21t => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.reg_count = 1;
            insn.target = (w1 as u16 as i16) as i32;
        }
        Format::F21s => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.reg_count = 1;
            insn.literal = (w1 as u16 as i16) as i64;
        }
        Format::F21h => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.reg_count = 1;
            let half = (w1 as u16 as i16) as i64;
            // const-wide/high16 shifts into the high 16 bits of a 64-bit value.
            insn.literal = if opcode.byte() == 0x19 {
                half << 48
            } else {
                half << 16
            };
        }
        Format::F21c => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.reg_count = 1;
            insn.index = w1 & 0xffff;
        }
        Format::F23x => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.regs[1] = (w1 & 0xff) as u16;
            insn.regs[2] = ((w1 >> 8) & 0xff) as u16;
            insn.reg_count = 3;
        }
        Format::F22b => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.regs[1] = (w1 & 0xff) as u16;
            insn.reg_count = 2;
            insn.literal = sx8(w1 >> 8) as i64;
        }
        Format::F22t => {
            insn.regs[0] = ((w0 >> 8) & 0xf) as u16;
            insn.regs[1] = ((w0 >> 12) & 0xf) as u16;
            insn.reg_count = 2;
            insn.target = (w1 as u16 as i16) as i32;
        }
        Format::F22s => {
            insn.regs[0] = ((w0 >> 8) & 0xf) as u16;
            insn.regs[1] = ((w0 >> 12) & 0xf) as u16;
            insn.reg_count = 2;
            insn.literal = (w1 as u16 as i16) as i64;
        }
        Format::F22c => {
            insn.regs[0] = ((w0 >> 8) & 0xf) as u16;
            insn.regs[1] = ((w0 >> 12) & 0xf) as u16;
            insn.reg_count = 2;
            insn.index = w1 & 0xffff;
        }
        Format::F30t => {
            insn.target = (w1 | (w2 << 16)) as i32;
        }
        Format::F32x => {
            insn.regs[0] = (w1 & 0xffff) as u16;
            insn.regs[1] = (w2 & 0xffff) as u16;
            insn.reg_count = 2;
        }
        Format::F31i => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.reg_count = 1;
            insn.literal = (w1 | (w2 << 16)) as i32 as i64;
        }
        Format::F31t => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.reg_count = 1;
            insn.target = (w1 | (w2 << 16)) as i32;
        }
        Format::F31c => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.reg_count = 1;
            insn.index = w1 | (w2 << 16);
        }
        Format::F35c => {
            let count = ((w0 >> 12) & 0xf) as u8;
            if count > 5 {
                return Err(DexError::BadInstruction {
                    pc: pc as u32,
                    opcode: op_byte,
                    reason: "register count > 5 in 35c instruction",
                });
            }
            insn.index = w1 & 0xffff;
            insn.regs[0] = (w2 & 0xf) as u16;
            insn.regs[1] = ((w2 >> 4) & 0xf) as u16;
            insn.regs[2] = ((w2 >> 8) & 0xf) as u16;
            insn.regs[3] = ((w2 >> 12) & 0xf) as u16;
            insn.regs[4] = ((w0 >> 8) & 0xf) as u16;
            insn.reg_count = count;
        }
        Format::F3rc => {
            insn.reg_count = ((w0 >> 8) & 0xff) as u8;
            insn.index = w1 & 0xffff;
            insn.regs[0] = (w2 & 0xffff) as u16;
        }
        Format::F51l => {
            insn.regs[0] = ((w0 >> 8) & 0xff) as u16;
            insn.reg_count = 1;
            let raw = (w1 as u64) | ((w2 as u64) << 16) | ((w3 as u64) << 32) | ((w4 as u64) << 48);
            insn.literal = raw as i64;
        }
        Format::F45cc => {
            let count = ((w0 >> 12) & 0xf) as u8;
            if count > 5 {
                return Err(DexError::BadInstruction {
                    pc: pc as u32,
                    opcode: op_byte,
                    reason: "register count > 5 in 45cc instruction",
                });
            }
            insn.index = w1 & 0xffff;
            insn.regs[0] = (w2 & 0xf) as u16;
            insn.regs[1] = ((w2 >> 4) & 0xf) as u16;
            insn.regs[2] = ((w2 >> 8) & 0xf) as u16;
            insn.regs[3] = ((w2 >> 12) & 0xf) as u16;
            insn.regs[4] = ((w0 >> 8) & 0xf) as u16;
            insn.reg_count = count;
            insn.index2 = w3 & 0xffff;
        }
        Format::F4rcc => {
            insn.reg_count = ((w0 >> 8) & 0xff) as u8;
            insn.index = w1 & 0xffff;
            insn.regs[0] = (w2 & 0xffff) as u16;
            insn.index2 = w3 & 0xffff;
        }
    }

    Ok(insn)
}

/// The three payload pseudo-structures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    PackedSwitch(SwitchPayload),
    SparseSwitch(SwitchPayload),
    FillArrayData(FillArrayDataPayload),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchPayload {
    pub keys: Vec<i32>,
    pub targets: Vec<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FillArrayDataPayload {
    pub element_width: u16,
    pub size: u32,
    pub data: Vec<u8>,
}

/// Identify the payload kind at `pc`, if any.
pub fn payload_kind(insns: &[u16], pc: usize) -> Option<u8> {
    let w = *insns.get(pc)?;
    if (w & 0xff) == 0 {
        match (w >> 8) as u8 {
            1 => Some(1),
            2 => Some(2),
            3 => Some(3),
            _ => None,
        }
    } else {
        None
    }
}

/// Length of the payload starting at `pc`, in code units.
pub fn payload_len(insns: &[u16], pc: usize) -> DexResult<usize> {
    let kind = payload_kind(insns, pc).ok_or(DexError::BadInstruction {
        pc: pc as u32,
        opcode: (insns.get(pc).copied().unwrap_or(0) & 0xff) as u8,
        reason: "not a payload",
    })?;
    match kind {
        1 | 2 => {
            let size = *insns.get(pc + 1).ok_or(DexError::BadInstruction {
                pc: pc as u32,
                opcode: 0,
                reason: "truncated switch payload header",
            })? as usize;
            // packed: ident + size + first_key + size targets = 4 + 2*size units.
            // sparse: ident + size + size keys + size targets = 2 + 4*size units.
            let units = if kind == 1 {
                size.checked_mul(2).and_then(|v| v.checked_add(4))
            } else {
                size.checked_mul(4).and_then(|v| v.checked_add(2))
            }
            .ok_or(DexError::Overflow {
                what: "switch payload length",
            })?;
            if pc + units > insns.len() {
                return Err(DexError::BadInstruction {
                    pc: pc as u32,
                    opcode: 0,
                    reason: "truncated switch payload body",
                });
            }
            Ok(units)
        }
        _ => {
            let width = *insns.get(pc + 1).ok_or(DexError::BadInstruction {
                pc: pc as u32,
                opcode: 0,
                reason: "truncated array payload header",
            })? as usize;
            let lo = *insns.get(pc + 2).ok_or(DexError::BadInstruction {
                pc: pc as u32,
                opcode: 0,
                reason: "truncated array payload header",
            })? as usize;
            let hi = *insns.get(pc + 3).ok_or(DexError::BadInstruction {
                pc: pc as u32,
                opcode: 0,
                reason: "truncated array payload header",
            })? as usize;
            let size = lo | (hi << 16);
            let bytes = width
                .checked_mul(size)
                .and_then(|v| v.checked_add(8))
                .ok_or(DexError::Overflow {
                    what: "array payload length",
                })?;
            let units = bytes.div_ceil(2);
            if pc + units > insns.len() {
                return Err(DexError::BadInstruction {
                    pc: pc as u32,
                    opcode: 0,
                    reason: "truncated array payload body",
                });
            }
            Ok(units)
        }
    }
}

/// Decode a payload at `pc`.
pub fn decode_payload(insns: &[u16], pc: usize) -> DexResult<Payload> {
    let kind = payload_kind(insns, pc).ok_or(DexError::BadInstruction {
        pc: pc as u32,
        opcode: (insns.get(pc).copied().unwrap_or(0) & 0xff) as u8,
        reason: "not a payload",
    })?;
    // Validate the payload length first.
    let _ = payload_len(insns, pc)?;

    match kind {
        1 => {
            let size = insns[pc + 1] as usize;
            let first_key = (insns[pc + 2] as u32 | ((insns[pc + 3] as u32) << 16)) as i32;
            let mut keys = Vec::with_capacity(size);
            let mut targets = Vec::with_capacity(size);
            for i in 0..size {
                keys.push(first_key.wrapping_add(i as i32));
            }
            let tbase = pc + 4;
            for i in 0..size {
                let t = tbase + i * 2;
                let target = (insns[t] as u32 | ((insns[t + 1] as u32) << 16)) as i32;
                targets.push(target);
            }
            Ok(Payload::PackedSwitch(SwitchPayload { keys, targets }))
        }
        2 => {
            let size = insns[pc + 1] as usize;
            let mut keys = Vec::with_capacity(size);
            let mut targets = Vec::with_capacity(size);
            for i in 0..size {
                let k = pc + 2 + i * 2;
                let key = (insns[k] as u32 | ((insns[k + 1] as u32) << 16)) as i32;
                keys.push(key);
            }
            let tbase = pc + 2 + size * 2;
            for i in 0..size {
                let t = tbase + i * 2;
                let target = (insns[t] as u32 | ((insns[t + 1] as u32) << 16)) as i32;
                targets.push(target);
            }
            Ok(Payload::SparseSwitch(SwitchPayload { keys, targets }))
        }
        _ => {
            let element_width = insns[pc + 1];
            let size = insns[pc + 2] as u32 | ((insns[pc + 3] as u32) << 16);
            let byte_len = element_width as usize * size as usize;
            let mut data = Vec::with_capacity(byte_len);
            let mut off = pc + 4;
            let mut remaining = byte_len;
            while remaining > 0 {
                let unit = insns[off];
                let b = unit.to_le_bytes();
                let take = remaining.min(2);
                data.extend_from_slice(&b[..take]);
                remaining -= take;
                off += 1;
            }
            Ok(Payload::FillArrayData(FillArrayDataPayload {
                element_width,
                size,
                data,
            }))
        }
    }
}
