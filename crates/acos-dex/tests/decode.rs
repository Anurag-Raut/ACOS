//! Decoder coverage: every defined opcode, every format, and the three payload
//! kinds. Uses synthetic instruction arrays (the *DEX fixtures* are real files;
//! these unit tests exercise the decoder directly).

use acos_dex::decode::{decode, decode_payload, payload_kind, payload_len, Payload};
use acos_dex::opcode::{defined_opcodes, Format, Opcode};

fn insns_for(op: Opcode) -> Vec<u16> {
    let mut v = vec![0u16; op.format().size()];
    v[0] = op.byte() as u16;
    v
}

#[test]
fn every_defined_opcode_decodes() {
    let mut count = 0;
    for op in defined_opcodes() {
        count += 1;
        let insns = insns_for(op);
        let insn = decode(&insns, 0).unwrap_or_else(|e| panic!("decode {op:?}: {e}"));
        assert_eq!(insn.opcode, op);
        assert_eq!(insn.size_in_code_units(), op.format().size());
        // Display and register iteration must not panic.
        let _ = format!("{insn}");
        let _ = insn.register_operands().count();
    }
    assert!(count > 200, "expected >200 defined opcodes, got {count}");
}

#[test]
fn unused_opcodes_are_rejected() {
    for &op in &[0x3eu8, 0x3f, 0x43, 0x73, 0x79, 0x7a, 0xe3, 0xf9] {
        let err = decode(&[op as u16], 0).unwrap_err();
        assert_eq!(err.code(), "DEX_UNUSED_OPCODE", "opcode {op:#04x}");
    }
}

#[test]
fn truncated_instruction_is_error() {
    // invoke-virtual needs 3 units.
    let err = decode(&[0x006e], 0).unwrap_err();
    assert_eq!(err.code(), "DEX_BAD_INSTRUCTION");
    assert_eq!(decode(&[], 0).unwrap_err().code(), "DEX_BAD_INSTRUCTION");
}

#[test]
fn format_operand_extraction() {
    // move v0, v1  -> 12x
    let i = decode(&[0x1001], 0).unwrap();
    assert_eq!(i.opcode.mnemonic(), "move");
    assert_eq!(i.regs[..2], [0, 1]);
    assert_eq!(i.reg_count, 2);

    // const/4 v2, #-1 -> 11n (A=2, B=0xf)
    let i = decode(&[0xf212], 0).unwrap();
    assert_eq!(i.literal, -1);
    assert_eq!(i.regs[0], 2);

    // const/16 v1, #-2 -> 21s
    let i = decode(&[0x0113, 0xfffe], 0).unwrap();
    assert_eq!(i.literal, -2);

    // goto +0x10 -> 10t (offset in high byte)
    let i = decode(&[0x1028], 0).unwrap();
    assert_eq!(i.target, 0x10);

    // goto/32 30t
    let i = decode(&[0x002a, 0x0004, 0x0000], 0).unwrap();
    assert_eq!(i.target, 4);

    // invoke-virtual {v0, v1}, method@0x1234 -> 35c
    // count=2 (A=2), G=0, method=0x1234, C=0, D=1
    let i = decode(&[0x206e, 0x1234, 0x0010], 0).unwrap();
    assert_eq!(i.index, 0x1234);
    assert_eq!(i.reg_count, 2);
    assert_eq!(i.regs[..2], [0, 1]);

    // invoke-static/range {v5 .. v7}, method@0x10 -> 3rc
    let i = decode(&[0x0377, 0x0010, 0x0005], 0).unwrap();
    assert_eq!(i.reg_count, 3);
    assert_eq!(i.regs[0], 5);

    // const-wide v0, #0x0123456789abcdef -> 51l
    let i = decode(&[0x0018, 0xcdef, 0x89ab, 0x4567, 0x0123], 0).unwrap();
    assert_eq!(i.literal, 0x0123_4567_89ab_cdef);

    // invoke-polymorphic {v0, v1}, method@0x10, proto@0x20 -> 45cc
    let i = decode(&[0x20fa, 0x0010, 0x0010, 0x0020], 0).unwrap();
    assert_eq!(i.index, 0x10);
    assert_eq!(i.index2, 0x20);
    assert_eq!(i.reg_count, 2);

    // const-method-handle v0, handle@0x5 -> 21c
    let i = decode(&[0x00fe, 0x0005], 0).unwrap();
    assert_eq!(i.index, 5);
}

#[test]
fn packed_switch_payload() {
    // ident, size=2, first_key=1, targets=[9, 6]
    let insns = [0x0100u16, 2, 1, 0, 9, 0, 6, 0];
    assert_eq!(payload_kind(&insns, 0), Some(1));
    assert_eq!(payload_len(&insns, 0).unwrap(), 8);
    match decode_payload(&insns, 0).unwrap() {
        Payload::PackedSwitch(p) => {
            assert_eq!(p.keys, vec![1, 2]);
            assert_eq!(p.targets, vec![9, 6]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn sparse_switch_payload() {
    // ident, size=2, keys=[5,9], targets=[1,2]
    let insns = [0x0200u16, 2, 5, 0, 9, 0, 1, 0, 2, 0];
    assert_eq!(payload_kind(&insns, 0), Some(2));
    assert_eq!(payload_len(&insns, 0).unwrap(), 10);
    match decode_payload(&insns, 0).unwrap() {
        Payload::SparseSwitch(p) => {
            assert_eq!(p.keys, vec![5, 9]);
            assert_eq!(p.targets, vec![1, 2]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn fill_array_data_payload() {
    // ident, width=4, size=2, data = 1u32, 2u32
    let insns = [0x0300u16, 4, 2, 0, 1, 0, 2, 0];
    assert_eq!(payload_kind(&insns, 0), Some(3));
    assert_eq!(payload_len(&insns, 0).unwrap(), 8);
    match decode_payload(&insns, 0).unwrap() {
        Payload::FillArrayData(p) => {
            assert_eq!(p.element_width, 4);
            assert_eq!(p.size, 2);
            assert_eq!(p.data.len(), 8);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn format_names_and_sizes() {
    assert_eq!(Format::F10x.size(), 1);
    assert_eq!(Format::F35c.size(), 3);
    assert_eq!(Format::F51l.size(), 5);
    assert_eq!(Format::F45cc.size(), 4);
    assert_eq!(Format::F45cc.name(), "45cc");
}
