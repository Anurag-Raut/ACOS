//! Human-readable disassembly, in a `dexdump`-like style.
//!
//! [`Instruction`] implements [`std::fmt::Display`] using raw index numbers.
//! [`format_instruction`] resolves indices against a [`DexFile`] and produces
//! the form used by the `dexdump` example, e.g.
//! `invoke-virtual {v0, v1}, Ljava/io/PrintStream;.println:(Ljava/lang/String;)V`.

use crate::code::CodeItem;
use crate::decode::{payload_kind, payload_len, Instruction};
use crate::error::DexResult;
use crate::opcode::{Format, Opcode};
use crate::DexFile;
use std::fmt;

fn lit(op: Opcode, n: i64) -> String {
    match op.byte() {
        0x16..=0x19 => format!("#long {n}"),
        _ => format!("#int {n}"),
    }
}

fn index_kind(op: Opcode) -> &'static str {
    match op.byte() {
        0x1a | 0x1b => "string",
        0x1c | 0x1f | 0x20 | 0x22 | 0x23 | 0x24 | 0x25 => "type",
        0x52..=0x6d => "field",
        0x6e..=0x72 | 0x74..=0x78 => "method",
        0xfa | 0xfb => "method",
        0xfc | 0xfd => "call_site",
        0xfe => "method_handle",
        0xff => "proto",
        _ => "index",
    }
}

fn raw_index(op: Opcode, index: u32) -> String {
    format!("{}@{:04x}", index_kind(op), index)
}

fn resolved_index(dex: &DexFile, op: Opcode, index: u32) -> String {
    match op.byte() {
        0x1a | 0x1b => match dex.string(index) {
            Ok(s) => format!("{s:?}"),
            Err(_) => raw_index(op, index),
        },
        0x1c | 0x1f | 0x20 | 0x22 | 0x23 | 0x24 | 0x25 => match dex.type_descriptor(index) {
            Ok(s) => s.to_string(),
            Err(_) => raw_index(op, index),
        },
        0x52..=0x6d => match dex.field_ref(index) {
            Ok(f) => f.full_name(),
            Err(_) => raw_index(op, index),
        },
        0x6e..=0x72 | 0x74..=0x78 | 0xfa | 0xfb => match dex.method_ref(index) {
            Ok(m) => m.full_name(),
            Err(_) => raw_index(op, index),
        },
        0xfe => format!("method_handle@{index:04x}"),
        0xff => match dex.proto(index) {
            Ok(p) => {
                let mut s = String::from("(");
                for &t in &p.parameters {
                    if let Ok(d) = dex.type_descriptor(t) {
                        s.push_str(d);
                    }
                }
                s.push(')');
                if let Ok(r) = dex.type_descriptor(p.return_type_idx) {
                    s.push_str(r);
                }
                s
            }
            Err(_) => raw_index(op, index),
        },
        _ => raw_index(op, index),
    }
}

fn reg(insn: &Instruction, i: usize) -> String {
    format!("v{}", insn.regs[i])
}

fn reg_list(insn: &Instruction) -> String {
    let mut parts = Vec::with_capacity(insn.reg_count as usize);
    for i in 0..insn.reg_count as usize {
        parts.push(reg(insn, i));
    }
    parts.join(", ")
}

fn reg_range(insn: &Instruction) -> String {
    let start = insn.regs[0] as u32;
    let count = insn.reg_count as u32;
    if count == 0 {
        return String::new();
    }
    format!("v{start} .. v{}", start + count - 1)
}

fn write_operands(
    f: &mut fmt::Formatter<'_>,
    dex: Option<&DexFile>,
    insn: &Instruction,
    pc: usize,
) -> fmt::Result {
    let op = insn.opcode;
    let idx = |index: u32| match dex {
        Some(d) => resolved_index(d, op, index),
        None => raw_index(op, index),
    };
    match insn.format {
        Format::F10x => Ok(()),
        Format::F12x => write!(f, " {}, {}", reg(insn, 0), reg(insn, 1)),
        Format::F11n => write!(f, " {}, {}", reg(insn, 0), lit(op, insn.literal)),
        Format::F11x => write!(f, " {}", reg(insn, 0)),
        Format::F10t | Format::F20t => {
            write!(f, " {:#06x}", (pc as i64 + insn.target as i64) as u32)
        }
        Format::F22x => write!(f, " {}, {}", reg(insn, 0), reg(insn, 1)),
        Format::F21t => write!(
            f,
            " {}, {:#06x}",
            reg(insn, 0),
            (pc as i64 + insn.target as i64) as u32
        ),
        Format::F21s | Format::F21h => write!(f, " {}, {}", reg(insn, 0), lit(op, insn.literal)),
        Format::F21c => write!(f, " {}, {}", reg(insn, 0), idx(insn.index)),
        Format::F23x => write!(
            f,
            " {}, {}, {}",
            reg(insn, 0),
            reg(insn, 1),
            reg(insn, 2)
        ),
        Format::F22b => write!(
            f,
            " {}, {}, {}",
            reg(insn, 0),
            reg(insn, 1),
            lit(op, insn.literal)
        ),
        Format::F22t => write!(
            f,
            " {}, {}, {:#06x}",
            reg(insn, 0),
            reg(insn, 1),
            (pc as i64 + insn.target as i64) as u32
        ),
        Format::F22s => write!(
            f,
            " {}, {}, {}",
            reg(insn, 0),
            reg(insn, 1),
            lit(op, insn.literal)
        ),
        Format::F22c => write!(f, " {}, {}, {}", reg(insn, 0), reg(insn, 1), idx(insn.index)),
        Format::F30t => write!(f, " {:#x}", (pc as i64 + insn.target as i64) as u32),
        Format::F32x => write!(f, " {}, {}", reg(insn, 0), reg(insn, 1)),
        Format::F31i => write!(f, " {}, {}", reg(insn, 0), lit(op, insn.literal)),
        Format::F31t => write!(
            f,
            " {}, {:#x}",
            reg(insn, 0),
            (pc as i64 + insn.target as i64) as u32
        ),
        Format::F31c => write!(f, " {}, {}", reg(insn, 0), idx(insn.index)),
        Format::F35c => write!(f, " {{{}}}, {}", reg_list(insn), idx(insn.index)),
        Format::F3rc => write!(f, " {{{}}}, {}", reg_range(insn), idx(insn.index)),
        Format::F51l => write!(f, " {}, {}", reg(insn, 0), lit(op, insn.literal)),
        Format::F45cc => write!(
            f,
            " {{{}}}, {}, proto@{:04x}",
            reg_list(insn),
            idx(insn.index),
            insn.index2
        ),
        Format::F4rcc => write!(
            f,
            " {{{}}}, {}, proto@{:04x}",
            reg_range(insn),
            idx(insn.index),
            insn.index2
        ),
    }
}

impl fmt::Display for Instruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.opcode.mnemonic())?;
        write_operands(f, None, self, 0)
    }
}

/// Format an instruction with indices resolved against `dex`, at code-unit
/// address `pc`.
pub fn format_instruction(dex: &DexFile, insn: &Instruction, pc: usize) -> String {
    let mut s = String::new();
    // Use a small adapter to reuse write_operands with the resolved context.
    struct Adapter<'a> {
        dex: &'a DexFile,
        insn: &'a Instruction,
        pc: usize,
    }
    impl fmt::Display for Adapter<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.insn.opcode.mnemonic())?;
            write_operands(f, Some(self.dex), self.insn, self.pc)
        }
    }
    use std::fmt::Write;
    let _ = write!(
        s,
        "{}",
        Adapter {
            dex,
            insn,
            pc
        }
    );
    s
}

/// Disassemble a method's code into `(pc, text)` pairs.
///
/// Payload pseudo-instructions are included as descriptive lines.
pub fn disassemble(dex: &DexFile, code: &CodeItem) -> DexResult<Vec<(usize, String)>> {
    let insns = &code.insns;
    let n = insns.len();
    let mut out = Vec::new();
    let mut pc = 0usize;
    while pc < n {
        if let Some(kind) = payload_kind(insns, pc) {
            let len = payload_len(insns, pc)?;
            let name = match kind {
                1 => "packed-switch-data",
                2 => "sparse-switch-data",
                _ => "array-data",
            };
            out.push((pc, format!("{name} ({len} units)")));
            pc += len;
            continue;
        }
        let insn = crate::decode::decode(insns, pc)?;
        out.push((pc, format_instruction(dex, &insn, pc)));
        pc += insn.size_in_code_units();
    }
    Ok(out)
}

/// Disassemble a method's code into the mnemonic sequence only.
pub fn mnemonic_sequence(code: &CodeItem) -> DexResult<Vec<&'static str>> {
    let insns = &code.insns;
    let n = insns.len();
    let mut out = Vec::new();
    let mut pc = 0usize;
    while pc < n {
        if payload_kind(insns, pc).is_some() {
            let len = payload_len(insns, pc)?;
            out.push(match payload_kind(insns, pc) {
                Some(1) => "packed-switch-data",
                Some(2) => "sparse-switch-data",
                _ => "array-data",
            });
            pc += len;
            continue;
        }
        let insn = crate::decode::decode(insns, pc)?;
        out.push(insn.opcode.mnemonic());
        pc += insn.size_in_code_units();
    }
    Ok(out)
}
