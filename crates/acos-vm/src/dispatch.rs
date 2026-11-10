//! Fast dispatch for frame-local DEX opcodes.
//!
//! The general interpreter path (`interp::exec_one` + `interp::execute`) pays
//! a frame/method lookup and a non-inlined call for *every* instruction. This
//! module runs a whole run of frame-local opcodes in one tight loop, holding
//! the instruction stream and the pc/instruction index as locals so the hot
//! loop does not chase the frame for them on every step.
//!
//! Only opcodes that can allocate, resolve a reference, dispatch a call, enter
//! a monitor, or throw fall back to the general path (the loop stops at them).
//! Everything here is safe Rust and mirrors the general path's semantics
//! exactly; arithmetic edge cases delegate to the same helpers.

use crate::class::Insn;
use crate::frame::Frame;
use crate::heap::{ArrayData, Heap, Object, NULL};
use crate::interp::{div_int, div_long, rem_int, rem_long};

/// Upper bound on instructions run before returning to the scheduler, so a
/// tight interpreter loop still yields to other green threads.
pub(crate) const FAST_CAP: u64 = 8192;

#[inline(always)]
fn advance(pc: &mut u32, ii: &mut usize, size: u8) {
    *pc += size as u32;
    *ii += 1;
}

#[inline(always)]
fn branch(pc: &mut u32, ii: &mut usize, pc_to_idx: &[u32], target: u32) -> bool {
    match pc_to_idx.get(target as usize).copied() {
        Some(i) if i != u32::MAX => {
            *pc = target;
            *ii = i as usize;
            true
        }
        _ => false,
    }
}

fn rel_target(pc: u32, target: i32) -> u32 {
    (pc as i64 + target as i64) as u32
}

/// Run frame-local instructions on the top frame. Returns the number executed;
/// stops before the first opcode that needs the general path (the caller runs
/// that one with the interpreter), and writes the final pc/index back onto the
/// frame.
pub(crate) fn fast_run(f: &mut Frame, heap: &mut Heap, cap: u64) -> u64 {
    let code = f.code.clone();
    let insns: &[Insn] = &code.insns;
    let pc_to_idx: &[u32] = &code.pc_to_idx;
    let mut pc = f.pc;
    let mut ii = f.insn_index;
    let mut count = 0u64;
    while count < cap {
        let insn = match insns.get(ii) {
            Some(i) => *i,
            None => break,
        };
        if !fast_op(f, heap, &insn, &mut pc, &mut ii, pc_to_idx) {
            break;
        }
        count += 1;
    }
    f.pc = pc;
    f.insn_index = ii;
    count
}

#[inline(always)]
fn fast_op(
    f: &mut Frame,
    heap: &mut Heap,
    insn: &Insn,
    pc: &mut u32,
    ii: &mut usize,
    pc_to_idx: &[u32],
) -> bool {
    let r0 = insn.regs[0];
    let r1 = insn.regs[1];
    let r2 = insn.regs[2];
    let size = insn.size;
    match insn.op {
        // ---- moves ----
        0x01 | 0x02 => {
            let v = f.reg(r1);
            if f.refs[r1 as usize] {
                f.set_reg_ref(r0, v);
            } else {
                f.set_reg(r0, v);
            }
            advance(pc, ii, size);
        }
        0x03 => {
            let v = f.reg(r1);
            f.set_reg(r0, v);
            advance(pc, ii, size);
        }
        0x04..=0x06 => {
            let v = f.reg_wide(r1);
            f.set_reg_wide(r0, v);
            advance(pc, ii, size);
        }
        0x07..=0x09 => {
            let v = f.reg(r1);
            f.set_reg_ref(r0, v);
            advance(pc, ii, size);
        }
        // ---- move-result ----
        0x0a => {
            let v = f.pending_result as u32;
            f.set_reg(r0, v);
            f.has_pending_result = false;
            advance(pc, ii, size);
        }
        0x0b => {
            let v = f.pending_result;
            f.set_reg_wide(r0, v);
            f.has_pending_result = false;
            advance(pc, ii, size);
        }
        0x0c => {
            let v = f.pending_result as u32;
            f.set_reg_ref(r0, v);
            f.has_pending_result = false;
            advance(pc, ii, size);
        }
        0x0d => {
            let e = f.pending_exception;
            f.set_reg_ref(r0, e);
            f.pending_exception = NULL;
            advance(pc, ii, size);
        }
        // ---- constants ----
        0x12..=0x15 => {
            f.set_reg(r0, insn.literal as u32);
            advance(pc, ii, size);
        }
        0x16..=0x19 => {
            f.set_reg_wide(r0, insn.literal as u64);
            advance(pc, ii, size);
        }
        // ---- arrays ----
        0x21 => {
            let h = f.reg(r1);
            match heap.get(h) {
                Some(Object::Array(a)) => {
                    f.set_reg(r0, a.len() as u32);
                    advance(pc, ii, size);
                }
                _ => return false,
            }
        }
        0x44..=0x4a => {
            let arr = f.reg(r1);
            let idx = f.reg(r2) as i32;
            match heap.get(arr) {
                Some(Object::Array(a)) => {
                    if idx < 0 || idx as usize >= a.len() {
                        return false;
                    }
                    let i = idx as usize;
                    match &a.data {
                        ArrayData::Bool(v) => f.set_reg(r0, v[i] as u32),
                        ArrayData::Byte(v) => f.set_reg(r0, v[i] as i32 as u32),
                        ArrayData::Char(v) => f.set_reg(r0, v[i] as u32),
                        ArrayData::Short(v) => f.set_reg(r0, v[i] as i32 as u32),
                        ArrayData::Int(v) => f.set_reg(r0, v[i] as u32),
                        ArrayData::Long(v) => f.set_reg_wide(r0, v[i] as u64),
                        ArrayData::Float(v) => f.set_reg(r0, v[i].to_bits()),
                        ArrayData::Double(v) => f.set_reg_wide(r0, v[i].to_bits()),
                        ArrayData::Ref(v) => {
                            if insn.op == 0x46 {
                                f.set_reg_ref(r0, v[i]);
                            } else {
                                f.set_reg(r0, v[i]);
                            }
                        }
                    }
                    advance(pc, ii, size);
                }
                _ => return false,
            }
        }
        0x4b..=0x51 => {
            // aput-object needs the ArrayStoreException check; leave it to the
            // general path. Everything else is a pure primitive array store.
            if insn.op == 0x4d {
                return false;
            }
            let arr = f.reg(r1);
            let idx = f.reg(r2) as i32;
            let rv = f.reg(r0);
            let rv_wide = if insn.op == 0x4c { f.reg_wide(r0) } else { 0 };
            match heap.get_mut(arr) {
                Some(Object::Array(a)) => {
                    if idx < 0 || idx as usize >= a.len() {
                        return false;
                    }
                    let i = idx as usize;
                    match &mut a.data {
                        ArrayData::Bool(v) => v[i] = rv != 0,
                        ArrayData::Byte(v) => v[i] = rv as i8,
                        ArrayData::Char(v) => v[i] = rv as u16,
                        ArrayData::Short(v) => v[i] = rv as i16,
                        ArrayData::Int(v) => v[i] = rv as i32,
                        ArrayData::Long(v) => v[i] = rv_wide as i64,
                        ArrayData::Float(v) => v[i] = f32::from_bits(rv),
                        ArrayData::Double(v) => v[i] = f64::from_bits(rv_wide),
                        ArrayData::Ref(v) => v[i] = rv,
                    }
                    advance(pc, ii, size);
                }
                _ => return false,
            }
        }
        // ---- goto / branches ----
        0x28..=0x2a => {
            if !branch(pc, ii, pc_to_idx, rel_target(*pc, insn.target)) {
                return false;
            }
        }
        0x32..=0x37 => {
            let a = f.reg(r0);
            let b = f.reg(r1);
            let take = match insn.op {
                0x32 => a == b,
                0x33 => a != b,
                0x34 => (a as i32) < (b as i32),
                0x35 => (a as i32) >= (b as i32),
                0x36 => (a as i32) > (b as i32),
                _ => (a as i32) <= (b as i32),
            };
            if take {
                if !branch(pc, ii, pc_to_idx, rel_target(*pc, insn.target)) {
                    return false;
                }
            } else {
                advance(pc, ii, size);
            }
        }
        0x38..=0x3d => {
            let a = f.reg(r0) as i32;
            let take = match insn.op {
                0x38 => a == 0,
                0x39 => a != 0,
                0x3a => a < 0,
                0x3b => a >= 0,
                0x3c => a > 0,
                _ => a <= 0,
            };
            if take {
                if !branch(pc, ii, pc_to_idx, rel_target(*pc, insn.target)) {
                    return false;
                }
            } else {
                advance(pc, ii, size);
            }
        }
        // ---- compare ----
        0x2d..=0x2e => {
            let a = f32::from_bits(f.reg(r1));
            let b = f32::from_bits(f.reg(r2));
            let r: i32 = if a.is_nan() || b.is_nan() {
                if insn.op == 0x2d { -1 } else { 1 }
            } else if a < b {
                -1
            } else if a > b {
                1
            } else {
                0
            };
            f.set_reg(r0, r as u32);
            advance(pc, ii, size);
        }
        0x2f..=0x30 => {
            let a = f64::from_bits(f.reg_wide(r1));
            let b = f64::from_bits(f.reg_wide(r2));
            let r: i32 = if a.is_nan() || b.is_nan() {
                if insn.op == 0x2f { -1 } else { 1 }
            } else if a < b {
                -1
            } else if a > b {
                1
            } else {
                0
            };
            f.set_reg(r0, r as u32);
            advance(pc, ii, size);
        }
        0x31 => {
            let a = f.reg_wide(r1) as i64;
            let b = f.reg_wide(r2) as i64;
            let r = if a < b {
                -1i32
            } else if a > b {
                1
            } else {
                0
            };
            f.set_reg(r0, r as u32);
            advance(pc, ii, size);
        }
        // ---- unary ----
        0x7b..=0x80 => {
            match insn.op {
                0x7b => {
                    let a = f.reg(r1) as i32;
                    f.set_reg(r0, a.wrapping_neg() as u32);
                }
                0x7c => {
                    let a = f.reg(r1) as i32;
                    f.set_reg(r0, (!a) as u32);
                }
                0x7d => {
                    let a = f.reg_wide(r1) as i64;
                    f.set_reg_wide(r0, a.wrapping_neg() as u64);
                }
                0x7e => {
                    let a = f.reg_wide(r1) as i64;
                    f.set_reg_wide(r0, (!a) as u64);
                }
                0x7f => {
                    let a = f32::from_bits(f.reg(r1));
                    f.set_reg(r0, (-a).to_bits());
                }
                _ => {
                    let a = f64::from_bits(f.reg_wide(r1));
                    f.set_reg_wide(r0, (-a).to_bits());
                }
            }
            advance(pc, ii, size);
        }
        // ---- conversions ----
        0x81..=0x8f => {
            match insn.op {
                0x81 => {
                    let v = f.reg(r1) as i32 as i64;
                    f.set_reg_wide(r0, v as u64);
                }
                0x82 => {
                    let v = f.reg(r1) as i32 as f32;
                    f.set_reg(r0, v.to_bits());
                }
                0x83 => {
                    let v = f.reg(r1) as i32 as f64;
                    f.set_reg_wide(r0, v.to_bits());
                }
                0x84 => {
                    let v = f.reg_wide(r1) as i64 as i32;
                    f.set_reg(r0, v as u32);
                }
                0x85 => {
                    let v = f.reg_wide(r1) as i64 as f32;
                    f.set_reg(r0, v.to_bits());
                }
                0x86 => {
                    let v = f.reg_wide(r1) as i64 as f64;
                    f.set_reg_wide(r0, v.to_bits());
                }
                0x87 => {
                    let v = f32::from_bits(f.reg(r1)) as i32;
                    f.set_reg(r0, v as u32);
                }
                0x88 => {
                    let v = f32::from_bits(f.reg(r1)) as i64;
                    f.set_reg_wide(r0, v as u64);
                }
                0x89 => {
                    let v = f32::from_bits(f.reg(r1)) as f64;
                    f.set_reg_wide(r0, v.to_bits());
                }
                0x8a => {
                    let v = f64::from_bits(f.reg_wide(r1)) as i32;
                    f.set_reg(r0, v as u32);
                }
                0x8b => {
                    let v = f64::from_bits(f.reg_wide(r1)) as i64;
                    f.set_reg_wide(r0, v as u64);
                }
                0x8c => {
                    let v = f64::from_bits(f.reg_wide(r1)) as f32;
                    f.set_reg(r0, v.to_bits());
                }
                0x8d => {
                    let v = f.reg(r1) as i32 as i8 as i32;
                    f.set_reg(r0, v as u32);
                }
                0x8e => {
                    let v = f.reg(r1) as u16;
                    f.set_reg(r0, v as u32);
                }
                _ => {
                    let v = f.reg(r1) as i32 as i16 as i32;
                    f.set_reg(r0, v as u32);
                }
            }
            advance(pc, ii, size);
        }
        // ---- arithmetic (frame-local, non-throwing subset) ----
        0x90..=0xe2 => {
            if !fast_arith(f, insn) {
                return false;
            }
            advance(pc, ii, size);
        }
        _ => return false,
    }
    true
}

/// Arithmetic on the frame alone. Returns `false` when the opcode needs the
/// general path (integer divide/remainder by zero must throw a Java exception).
#[inline(always)]
fn fast_arith(f: &mut Frame, insn: &Insn) -> bool {
    let r0 = insn.regs[0];
    let r1 = insn.regs[1];
    let r2 = insn.regs[2];
    match insn.op {
        0x90 => {
            let (a, b) = (f.reg(r1) as i32, f.reg(r2) as i32);
            f.set_reg(r0, a.wrapping_add(b) as u32);
        }
        0x91 => {
            let (a, b) = (f.reg(r1) as i32, f.reg(r2) as i32);
            f.set_reg(r0, a.wrapping_sub(b) as u32);
        }
        0x92 => {
            let (a, b) = (f.reg(r1) as i32, f.reg(r2) as i32);
            f.set_reg(r0, a.wrapping_mul(b) as u32);
        }
        0x93 => {
            let (a, b) = (f.reg(r1) as i32, f.reg(r2) as i32);
            if b == 0 {
                return false;
            }
            f.set_reg(r0, div_int(a, b).unwrap() as u32);
        }
        0x94 => {
            let (a, b) = (f.reg(r1) as i32, f.reg(r2) as i32);
            if b == 0 {
                return false;
            }
            f.set_reg(r0, rem_int(a, b).unwrap() as u32);
        }
        0x95 => {
            let (a, b) = (f.reg(r1), f.reg(r2));
            f.set_reg(r0, a & b);
        }
        0x96 => {
            let (a, b) = (f.reg(r1), f.reg(r2));
            f.set_reg(r0, a | b);
        }
        0x97 => {
            let (a, b) = (f.reg(r1), f.reg(r2));
            f.set_reg(r0, a ^ b);
        }
        0x98 => {
            let (a, b) = (f.reg(r1) as i32, f.reg(r2));
            f.set_reg(r0, a.wrapping_shl(b & 0x1f) as u32);
        }
        0x99 => {
            let (a, b) = (f.reg(r1) as i32, f.reg(r2));
            f.set_reg(r0, a.wrapping_shr(b & 0x1f) as u32);
        }
        0x9a => {
            let (a, b) = (f.reg(r1), f.reg(r2));
            f.set_reg(r0, a.wrapping_shr(b & 0x1f));
        }
        0x9b..=0xa5 => {
            let a = f.reg_wide(r1) as i64;
            // `shl-long`/`shr-long`/`ushr-long` (0xa3..=0xa5) take an *int*
            // shift amount in `vCC`; the other long binary ops take a wide
            // `vCC`. Reading the shift amount as wide both miscompiles and can
            // index past the register file (the general path has the same fix).
            let v: i64 = if (0xa3..=0xa5).contains(&insn.op) {
                let sh = (f.reg(r2) as i32 & 0x3f) as u32;
                match insn.op {
                    0xa3 => (a as u64).wrapping_shl(sh) as i64,
                    0xa4 => a.wrapping_shr(sh),
                    _ => ((a as u64).wrapping_shr(sh)) as i64,
                }
            } else {
                let b = f.reg_wide(r2) as i64;
                match insn.op {
                    0x9b => a.wrapping_add(b),
                    0x9c => a.wrapping_sub(b),
                    0x9d => a.wrapping_mul(b),
                    0x9e => {
                        if b == 0 {
                            return false;
                        }
                        div_long(a, b).unwrap()
                    }
                    0x9f => {
                        if b == 0 {
                            return false;
                        }
                        rem_long(a, b).unwrap()
                    }
                    0xa0 => a & b,
                    0xa1 => a | b,
                    _ => a ^ b,
                }
            };
            f.set_reg_wide(r0, v as u64);
        }
        0xa6..=0xaf => {
            if insn.op >= 0xab {
                let a = f64::from_bits(f.reg_wide(r1));
                let b = f64::from_bits(f.reg_wide(r2));
                let v = match insn.op {
                    0xab => a + b,
                    0xac => a - b,
                    0xad => a * b,
                    0xae => a / b,
                    _ => a % b,
                };
                f.set_reg_wide(r0, v.to_bits());
            } else {
                let a = f32::from_bits(f.reg(r1));
                let b = f32::from_bits(f.reg(r2));
                let v = match insn.op {
                    0xa6 => a + b,
                    0xa7 => a - b,
                    0xa8 => a * b,
                    0xa9 => a / b,
                    _ => a % b,
                };
                f.set_reg(r0, v.to_bits());
            }
        }
        0xb0..=0xba => {
            let a = f.reg(r0) as i32;
            let b = f.reg(r1);
            let v: i32 = match insn.op {
                0xb0 => a.wrapping_add(b as i32),
                0xb1 => a.wrapping_sub(b as i32),
                0xb2 => a.wrapping_mul(b as i32),
                0xb3 => {
                    if b as i32 == 0 {
                        return false;
                    }
                    div_int(a, b as i32).unwrap()
                }
                0xb4 => {
                    if b as i32 == 0 {
                        return false;
                    }
                    rem_int(a, b as i32).unwrap()
                }
                0xb5 => a & b as i32,
                0xb6 => a | b as i32,
                0xb7 => a ^ b as i32,
                0xb8 => a.wrapping_shl(b & 0x1f),
                0xb9 => a.wrapping_shr(b & 0x1f),
                _ => (a as u32).wrapping_shr(b & 0x1f) as i32,
            };
            f.set_reg(r0, v as u32);
        }
        0xbb..=0xc5 => {
            let a = f.reg_wide(r0) as i64;
            // Long shift /2addr forms (0xc3..=0xc5) take an int shift amount in
            // `vB`; the other long /2addr ops take a wide `vB`.
            let v: i64 = if (0xc3..=0xc5).contains(&insn.op) {
                let sh = (f.reg(r1) as i32 & 0x3f) as u32;
                match insn.op {
                    0xc3 => ((a as u64).wrapping_shl(sh)) as i64,
                    0xc4 => a.wrapping_shr(sh),
                    _ => ((a as u64).wrapping_shr(sh)) as i64,
                }
            } else {
                let b = f.reg_wide(r1) as i64;
                match insn.op {
                    0xbb => a.wrapping_add(b),
                    0xbc => a.wrapping_sub(b),
                    0xbd => a.wrapping_mul(b),
                    0xbe => {
                        if b == 0 {
                            return false;
                        }
                        div_long(a, b).unwrap()
                    }
                    0xbf => {
                        if b == 0 {
                            return false;
                        }
                        rem_long(a, b).unwrap()
                    }
                    0xc0 => a & b,
                    0xc1 => a | b,
                    _ => a ^ b,
                }
            };
            f.set_reg_wide(r0, v as u64);
        }
        0xc6..=0xcf => {
            if insn.op >= 0xcb {
                let a = f64::from_bits(f.reg_wide(r0));
                let b = f64::from_bits(f.reg_wide(r1));
                let v = match insn.op {
                    0xcb => a + b,
                    0xcc => a - b,
                    0xcd => a * b,
                    0xce => a / b,
                    _ => a % b,
                };
                f.set_reg_wide(r0, v.to_bits());
            } else {
                let a = f32::from_bits(f.reg(r0));
                let b = f32::from_bits(f.reg(r1));
                let v = match insn.op {
                    0xc6 => a + b,
                    0xc7 => a - b,
                    0xc8 => a * b,
                    0xc9 => a / b,
                    _ => a % b,
                };
                f.set_reg(r0, v.to_bits());
            }
        }
        0xd0..=0xd7 => {
            let a = f.reg(r1) as i32;
            let lit = insn.literal as i32;
            let v = match insn.op {
                0xd0 => a.wrapping_add(lit),
                0xd1 => lit.wrapping_sub(a),
                0xd2 => a.wrapping_mul(lit),
                0xd3 => {
                    if lit == 0 {
                        return false;
                    }
                    div_int(a, lit).unwrap()
                }
                0xd4 => {
                    if lit == 0 {
                        return false;
                    }
                    rem_int(a, lit).unwrap()
                }
                0xd5 => a & lit,
                0xd6 => a | lit,
                _ => a ^ lit,
            };
            f.set_reg(r0, v as u32);
        }
        0xd8..=0xe2 => {
            let a = f.reg(r1) as i32;
            let lit = insn.literal as i32;
            let v = match insn.op {
                0xd8 => a.wrapping_add(lit),
                0xd9 => lit.wrapping_sub(a),
                0xda => a.wrapping_mul(lit),
                0xdb => {
                    if lit == 0 {
                        return false;
                    }
                    div_int(a, lit).unwrap()
                }
                0xdc => {
                    if lit == 0 {
                        return false;
                    }
                    rem_int(a, lit).unwrap()
                }
                0xdd => a & lit,
                0xde => a | lit,
                0xdf => a ^ lit,
                0xe0 => a.wrapping_shl(lit as u32 & 0x1f),
                0xe1 => a.wrapping_shr(lit as u32 & 0x1f),
                _ => (a as u32).wrapping_shr(lit as u32 & 0x1f) as i32,
            };
            f.set_reg(r0, v as u32);
        }
        _ => return false,
    }
    true
}
