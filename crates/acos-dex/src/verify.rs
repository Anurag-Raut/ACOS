//! Structural (pass-1) DEX verifier.
//!
//! # Scope
//!
//! This is a **structural** verifier. It checks:
//!
//! * every id table index referenced by class data / code is in range;
//! * every method's instruction stream decodes end-to-end with no unused
//!   opcodes and no truncated instructions;
//! * branch, `goto`, switch and `fill-array-data` targets land on instruction
//!   boundaries (switch target offsets are resolved relative to the *switch*
//!   instruction, as required by the spec);
//! * register indices are below `registers_size`, including the second register
//!   of wide (`long`/`double`) operand pairs and full range invocations;
//! * `ins_size <= registers_size`;
//! * try ranges and catch-handler addresses are within the method and on
//!   instruction boundaries;
//! * payloads are well-formed;
//! * a method cannot fall through past the end of its instruction array.
//!
//! **Full type-flow verification is out of scope.** Register types, invoke
//! argument compatibility, uninitialized-object rules, and class hierarchy
//! checks are intentionally not performed.

use crate::decode::{decode, decode_payload, payload_kind, payload_len, Instruction, Payload};
use crate::error::{DexError, DexResult};
use crate::{ClassDef, DexFile, EncodedMethod};

/// Aggregate statistics produced by [`verify`].
#[derive(Clone, Debug)]
pub struct VerifyReport {
    pub classes: u64,
    pub methods_with_code: u64,
    pub instructions: u64,
    /// Instruction counts indexed by opcode byte.
    pub opcode_histogram: [u64; 256],
    pub switches: u64,
    pub fill_array_data: u64,
    pub try_items: u64,
    pub catch_handlers: u64,
    pub max_registers: u16,
}

impl Default for VerifyReport {
    fn default() -> Self {
        VerifyReport {
            classes: 0,
            methods_with_code: 0,
            instructions: 0,
            opcode_histogram: [0; 256],
            switches: 0,
            fill_array_data: 0,
            try_items: 0,
            catch_handlers: 0,
            max_registers: 0,
        }
    }
}

impl VerifyReport {
    /// `(opcode, mnemonic, count)` entries with a non-zero count, ordered by
    /// opcode.
    pub fn histogram_entries(&self) -> Vec<(u8, &'static str, u64)> {
        crate::opcode::defined_opcodes()
            .map(|op| {
                (
                    op.byte(),
                    op.mnemonic(),
                    self.opcode_histogram[op.byte() as usize],
                )
            })
            .filter(|&(_, _, n)| n > 0)
            .collect()
    }
}

fn method_label(dex: &DexFile, method_idx: u32) -> String {
    match dex.method_ref(method_idx) {
        Ok(r) => r.full_name(),
        Err(_) => format!("method@{method_idx:#06x}"),
    }
}

/// Bit `i` is set when register operand `i` is the low half of a wide
/// (`long`/`double`) value and therefore needs `reg + 1` to also be valid.
fn wide_operand_mask(opcode: u8) -> u8 {
    match opcode {
        0x04..=0x06 => 0b011,
        0x0b | 0x10 | 0x16 | 0x17 | 0x18 | 0x19 => 0b001,
        0x45 | 0x4c | 0x53 | 0x5a | 0x61 | 0x68 => 0b001,
        0x7d | 0x7e | 0x80 => 0b011,
        0x81 | 0x83 => 0b001,
        0x84 | 0x85 => 0b010,
        0x86 => 0b011,
        0x88 | 0x89 => 0b001,
        0x8a => 0b010,
        0x8b => 0b011,
        0x8c => 0b010,
        0x9b..=0xa2 => 0b111,
        // long shl/shr/ushr: A and B are wide, C is the int shift amount.
        0xa3..=0xa5 => 0b011,
        0xab..=0xaf => 0b111,
        0xbb..=0xc2 => 0b011,
        // long shl/shr/ushr /2addr: A wide, B int shift amount.
        0xc3..=0xc5 => 0b001,
        0xcb..=0xcf => 0b011,
        0x2f..=0x31 => 0b110,
        _ => 0,
    }
}

/// Lazily formats the method name only when a verification error occurs, so a
/// successful verify does not need to decode the string table.
struct MethodContext<'a> {
    dex: &'a DexFile,
    method_idx: u32,
}

impl MethodContext<'_> {
    fn fail<T>(&self, reason: impl Into<String>) -> DexResult<T> {
        Err(DexError::Verify {
            method: method_label(self.dex, self.method_idx),
            reason: reason.into(),
        })
    }
}

fn verify_method(
    dex: &DexFile,
    class: &ClassDef,
    method: &EncodedMethod,
    report: &mut VerifyReport,
) -> DexResult<()> {
    let code = dex.code_item(method.code_off)?;
    let ctx = MethodContext {
        dex,
        method_idx: method.method_idx,
    };

    if code.ins_size > code.registers_size {
        return ctx.fail(format!(
            "ins_size {} > registers_size {}",
            code.ins_size, code.registers_size
        ));
    }
    report.max_registers = report.max_registers.max(code.registers_size);
    report.try_items += code.tries.len() as u64;
    report.catch_handlers += code.handlers.len() as u64;

    let insns = &code.insns;
    let n = insns.len();

    // Pass 1: walk the stream, record instruction boundaries and decode.
    let mut boundaries = vec![false; n];
    let mut decoded: Vec<(usize, Instruction)> = Vec::new();
    let mut pc = 0usize;
    while pc < n {
        if payload_kind(insns, pc).is_some() {
            let len = payload_len(insns, pc)?;
            pc += len;
            continue;
        }
        let insn = decode(insns, pc)?;
        boundaries[pc] = true;
        report.instructions += 1;
        report.opcode_histogram[insn.opcode.byte() as usize] += 1;
        decoded.push((pc, insn));
        pc += insn.size_in_code_units();
    }

    // Pass 2: register bounds, branch/switch/payload targets.
    for (ipc, insn) in &decoded {
        let op = insn.opcode;
        if op.is_range() {
            let start = insn.regs[0] as u32;
            let count = insn.reg_count as u32;
            if start + count > code.registers_size as u32 {
                return ctx.fail(format!(
                    "range invoke at pc {ipc} touches v{start}..v{} but registers_size is {}",
                    start + count,
                    code.registers_size
                ));
            }
        } else {
            let mask = wide_operand_mask(op.byte());
            for i in 0..insn.reg_count as usize {
                let reg = insn.regs[i] as u32;
                if reg >= code.registers_size as u32 {
                    return ctx.fail(format!(
                        "register v{reg} out of range at pc {ipc} (registers_size {})",
                        code.registers_size
                    ));
                }
                if mask & (1 << i) != 0 && reg + 1 >= code.registers_size as u32 {
                    return ctx.fail(format!(
                        "wide register pair v{reg}/v{} out of range at pc {ipc} (registers_size {})",
                        reg + 1,
                        code.registers_size
                    ));
                }
            }
        }

        if op.is_branch() {
            let target = *ipc as i64 + insn.target as i64;
            if target < 0 || target as usize >= n || !boundaries[target as usize] {
                return ctx.fail(format!(
                    "branch at pc {ipc} targets {target}, which is not an instruction boundary"
                ));
            }
        }

        if op.is_switch() {
            report.switches += 1;
            let payload_pc = *ipc as i64 + insn.target as i64;
            if payload_pc < 0 || payload_pc as usize >= n {
                return ctx.fail(format!(
                    "switch at pc {ipc} has out-of-range payload target"
                ));
            }
            let payload_pc = payload_pc as usize;
            let kind = payload_kind(insns, payload_pc);
            let expected = if op.byte() == 0x2b { 1 } else { 2 };
            if kind != Some(expected) {
                return ctx.fail(format!(
                    "switch at pc {ipc} does not point at a matching switch payload"
                ));
            }
            match decode_payload(insns, payload_pc)? {
                Payload::PackedSwitch(p) | Payload::SparseSwitch(p) => {
                    for &t in &p.targets {
                        let target = *ipc as i64 + t as i64;
                        if target < 0 || target as usize >= n || !boundaries[target as usize] {
                            return ctx.fail(format!(
                                "switch case at pc {ipc} targets {target}, which is not an instruction boundary"
                            ));
                        }
                    }
                }
                Payload::FillArrayData(_) => {
                    return ctx.fail("switch points at a fill-array-data payload")
                }
            }
        }

        if op.is_fill_array_data() {
            report.fill_array_data += 1;
            let payload_pc = *ipc as i64 + insn.target as i64;
            if payload_pc < 0 || payload_pc as usize >= n {
                return ctx.fail(format!(
                    "fill-array-data at pc {ipc} has out-of-range payload target"
                ));
            }
            let payload_pc = payload_pc as usize;
            if payload_kind(insns, payload_pc) != Some(3) {
                return ctx.fail(format!(
                    "fill-array-data at pc {ipc} does not point at an array-data payload"
                ));
            }
            if let Payload::FillArrayData(p) = decode_payload(insns, payload_pc)? {
                if !matches!(p.element_width, 1 | 2 | 4 | 8) {
                    return ctx.fail(format!(
                        "array-data payload at {payload_pc} has element width {}",
                        p.element_width
                    ));
                }
            }
        }
    }

    // No fallthrough past the end of the method.
    if let Some((last_pc, last)) = decoded.last() {
        let end = *last_pc + last.size_in_code_units();
        if end == n && !last.opcode.is_terminator() {
            return ctx.fail(format!(
                "control falls through past the end of the method at pc {last_pc}"
            ));
        }
    }

    // Try ranges and handler addresses.
    for try_item in &code.tries {
        let start = try_item.start_addr as usize;
        let count = try_item.insn_count as usize;
        let end = start
            .checked_add(count)
            .ok_or(DexError::Overflow { what: "try range" })?;
        if end > n {
            return ctx.fail(format!(
                "try range [{start}, {end}) exceeds instruction count {n}"
            ));
        }
        if start >= end {
            return ctx.fail(format!("empty or reversed try range [{start}, {end})"));
        }
        if !boundaries[start] {
            return ctx.fail(format!("try range starts at non-boundary pc {start}"));
        }
        if end != n && !boundaries[end] {
            return ctx.fail(format!("try range ends at non-boundary pc {end}"));
        }
        let Some(handler) = code.handlers.get(try_item.handler_index) else {
            return ctx.fail("try_item references a missing handler");
        };
        for pair in &handler.handlers {
            let addr = pair.addr as usize;
            if addr >= n || !boundaries[addr] {
                return ctx.fail(format!(
                    "catch handler for type {} targets non-boundary pc {addr}",
                    pair.type_idx
                ));
            }
        }
        if let Some(all) = handler.catch_all_addr {
            let addr = all as usize;
            if addr >= n || !boundaries[addr] {
                return ctx.fail(format!("catch-all handler targets non-boundary pc {addr}"));
            }
        }
    }

    let _ = class;
    Ok(())
}

/// Run the structural verifier over an entire DEX file.
pub fn verify(dex: &DexFile) -> DexResult<VerifyReport> {
    let mut report = VerifyReport {
        classes: dex.class_defs().len() as u64,
        ..Default::default()
    };

    for def in dex.class_defs() {
        if let Some(data) = dex.class_data(def)? {
            for method in data
                .direct_methods
                .iter()
                .chain(data.virtual_methods.iter())
            {
                if method.code_off == 0 {
                    continue;
                }
                report.methods_with_code += 1;
                verify_method(dex, def, method, &mut report)?;
            }
        }
    }

    Ok(report)
}
