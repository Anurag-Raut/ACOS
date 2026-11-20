//! Parse + verify the checked-in, real `d8`-compiled DEX fixtures and assert on
//! known structure.

use acos_dex::decode::{decode, decode_payload, payload_kind, payload_len, Instruction, Payload};
use acos_dex::{DexFile, EncodedValue};
use std::path::PathBuf;

fn data_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn load(name: &str) -> DexFile {
    let bytes = std::fs::read(data_path(name)).expect("fixture file present");
    DexFile::parse(bytes).expect("fixture parses")
}

fn find_method(dex: &DexFile, class: &str, name: &str) -> Option<u32> {
    let def = dex.find_class(class)?;
    let cd = dex.class_data(def).ok()??;
    for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
        if let Ok(r) = dex.method_ref(m.method_idx) {
            if r.name == name {
                return Some(m.code_off);
            }
        }
    }
    None
}

fn decode_all_pc(dex: &DexFile, code_off: u32) -> Vec<(usize, Instruction)> {
    let code = dex.code_item(code_off).expect("code item");
    let insns = &code.insns;
    let mut out = Vec::new();
    let mut pc = 0usize;
    while pc < insns.len() {
        if payload_kind(insns, pc).is_some() {
            pc += payload_len(insns, pc).expect("payload length");
            continue;
        }
        let insn = decode(insns, pc).expect("decode");
        out.push((pc, insn));
        pc += insn.size_in_code_units();
    }
    out
}

fn decode_all(dex: &DexFile, code_off: u32) -> Vec<Instruction> {
    decode_all_pc(dex, code_off)
        .into_iter()
        .map(|(_, i)| i)
        .collect()
}

#[test]
fn all_fixtures_parse_and_verify() {
    for name in [
        "fixtures.dex",
        "fixtures_api19.dex",
        "fixtures_invokecustom.dex",
    ] {
        let dex = load(name);
        assert!(
            matches!(dex.header().version, 35 | 37 | 38 | 39),
            "{name}: version {}",
            dex.header().version
        );
        assert!(dex.class_defs().len() >= 20, "{name}: classes");
        let report = dex
            .verify()
            .unwrap_or_else(|e| panic!("{name}: verify failed: {e}"));
        assert!(report.methods_with_code > 50, "{name}: methods");
        assert!(report.instructions > 500, "{name}: instructions");
        assert_eq!(
            report.instructions,
            report.opcode_histogram.iter().sum::<u64>(),
            "{name}: histogram sum"
        );
    }
}

#[test]
fn known_classes_and_methods() {
    let dex = load("fixtures.dex");
    assert!(dex.find_class("LArithmetic;").is_some());
    assert!(dex.find_class("LConstants;").is_some());
    assert!(dex.find_class("LSwitches;").is_some());
    assert!(dex.find_class("LTryCatch;").is_some());
    assert!(dex.find_class("LShapes;").is_some());
    assert!(dex.find_class("LEnums;").is_some());
    assert!(dex.find_class("LNoSuchClass;").is_none());

    // Resolve a method prototype precisely.
    let def = dex.find_class("LArithmetic;").unwrap();
    let cd = dex.class_data(def).unwrap().unwrap();
    let mut checked = false;
    for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
        let r = dex.method_ref(m.method_idx).unwrap();
        if r.name == "addLong" {
            assert_eq!(r.return_type, "J");
            assert_eq!(r.parameters, vec!["J", "J"]);
            assert_eq!(r.full_name(), "LArithmetic;.addLong:(JJ)J");
            checked = true;
        }
    }
    assert!(checked, "addLong present");
}

#[test]
fn non_ascii_and_supplementary_strings() {
    let dex = load("fixtures.dex");
    assert!(
        dex.strings().iter().any(|s| s.contains("héllo 中文")),
        "expected non-ASCII greeting string"
    );
    assert!(
        dex.strings().iter().any(|s| s.contains('\u{1F600}')),
        "expected supplementary emoji string"
    );
    assert!(
        dex.strings().iter().any(|s| s.contains('\u{1D56C}')),
        "expected mathematical bold fraktur supplementary char"
    );
    assert!(
        dex.strings().iter().any(|s| s == "a\0b"),
        "expected embedded-NUL string"
    );
}

#[test]
fn static_values_decode() {
    let dex = load("fixtures.dex");
    let def = dex.find_class("LConstants;").unwrap();
    let values = dex.static_values(def).unwrap();
    let values = values.expect("Constants has static_values");
    assert!(!values.is_empty());
    // Compile-time constants (primitives and strings) live in static_values;
    // array initializers are emitted in <clinit> via fill-array-data.
    assert!(
        values.iter().any(|v| matches!(v, EncodedValue::String(_))),
        "expected a string static value, got {values:?}"
    );
    assert!(
        values.iter().any(|v| matches!(
            v,
            EncodedValue::Int(_)
                | EncodedValue::Long(_)
                | EncodedValue::Float(_)
                | EncodedValue::Double(_)
        )),
        "expected a numeric static value"
    );
}

#[test]
fn annotations_directory_parses() {
    let dex = load("fixtures.dex");
    let def = dex.find_class("LEnums;").unwrap();
    let ann = dex.annotations(def).expect("annotations parsed");
    assert!(
        !ann.class_annotations.is_empty()
            || !ann.field_annotations.is_empty()
            || !ann.method_annotations.is_empty(),
        "LEnums carries a d8 system annotation"
    );
}

#[test]
fn packed_and_sparse_switch_payloads() {
    let dex = load("fixtures.dex");
    let code_off = find_method(&dex, "LSwitches;", "packed").expect("packed method");
    let insns = decode_all_pc(&dex, code_off);
    let (switch_pc, switch) = insns
        .iter()
        .find(|(_, i)| i.opcode.byte() == 0x2b)
        .expect("packed-switch instruction");
    let code = dex.code_item(code_off).unwrap();
    let payload_pc = (*switch_pc as i64 + switch.target as i64) as usize;
    match decode_payload(&code.insns, payload_pc).unwrap() {
        Payload::PackedSwitch(p) => {
            assert!(p.keys.len() >= 3, "packed keys");
            assert_eq!(p.keys.len(), p.targets.len());
            // Packed keys must be consecutive.
            for w in p.keys.windows(2) {
                assert_eq!(w[1], w[0] + 1);
            }
        }
        other => panic!("expected packed switch, got {other:?}"),
    }

    let code_off = find_method(&dex, "LSwitches;", "sparse").expect("sparse method");
    let insns = decode_all_pc(&dex, code_off);
    let (switch_pc, switch) = insns
        .iter()
        .find(|(_, i)| i.opcode.byte() == 0x2c)
        .expect("sparse-switch instruction");
    let code = dex.code_item(code_off).unwrap();
    let payload_pc = (*switch_pc as i64 + switch.target as i64) as usize;
    match decode_payload(&code.insns, payload_pc).unwrap() {
        Payload::SparseSwitch(p) => {
            assert!(p.keys.len() >= 4);
            assert_eq!(p.keys.len(), p.targets.len());
            for w in p.keys.windows(2) {
                assert!(w[0] < w[1], "sparse keys sorted");
            }
        }
        other => panic!("expected sparse switch, got {other:?}"),
    }
}

#[test]
fn try_catch_tables() {
    let dex = load("fixtures.dex");
    let code_off = find_method(&dex, "LTryCatch;", "multi").expect("multi method");
    let code = dex.code_item(code_off).unwrap();
    assert!(!code.tries.is_empty(), "expected try items");
    assert!(!code.handlers.is_empty(), "expected catch handlers");
    for t in &code.tries {
        assert!((t.start_addr as usize) < code.insns.len());
        assert!(t.handler_index < code.handlers.len());
    }
    assert!(
        code.handlers.iter().any(|h| h.catch_all_addr.is_some()),
        "expected a catch-all (finally) handler"
    );
}

#[test]
fn fill_array_data_payloads() {
    let dex = load("fixtures.dex");
    let code_off = find_method(&dex, "LConstants;", "<clinit>").expect("<clinit> method");
    let insns = decode_all_pc(&dex, code_off);
    let (fill_pc, fill) = insns
        .iter()
        .find(|(_, i)| i.opcode.byte() == 0x26)
        .expect("fill-array-data instruction");
    let code = dex.code_item(code_off).unwrap();
    let payload_pc = (*fill_pc as i64 + fill.target as i64) as usize;
    match decode_payload(&code.insns, payload_pc).unwrap() {
        Payload::FillArrayData(p) => {
            assert!(matches!(p.element_width, 1 | 2 | 4 | 8));
            assert!(p.size > 0);
            assert_eq!(p.data.len(), p.element_width as usize * p.size as usize);
        }
        other => panic!("expected array-data payload, got {other:?}"),
    }
}

#[test]
fn invoke_polymorphic_present() {
    let dex = load("fixtures.dex");
    let code_off = find_method(&dex, "LMethodHandlesUse;", "call").expect("call method");
    let insns = decode_all(&dex, code_off);
    let poly = insns
        .iter()
        .find(|i| i.opcode.byte() == 0xfa)
        .expect("invoke-polymorphic");
    assert!(poly.index2 != 0, "polymorphic proto index set");
    // Resolving the method and proto must work.
    dex.method_ref(poly.index).unwrap();
    dex.proto(poly.index2).unwrap();
}

#[test]
fn invoke_custom_and_call_sites() {
    let dex = load("fixtures_invokecustom.dex");
    assert!(!dex.call_site_ids().is_empty(), "call sites present");

    let mut found = false;
    for def in dex.class_defs() {
        let Some(cd) = dex.class_data(def).unwrap() else {
            continue;
        };
        for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
            if m.code_off == 0 {
                continue;
            }
            for insn in decode_all(&dex, m.code_off) {
                if insn.opcode.byte() == 0xfc {
                    found = true;
                    let values = dex.call_site(insn.index).expect("call site decodes");
                    assert!(!values.is_empty(), "call site bootstrap args");
                }
            }
        }
    }
    assert!(found, "expected at least one invoke-custom");
}

#[test]
fn line_tables_decode() {
    let dex = load("fixtures.dex");
    let code_off = find_method(&dex, "LArithmetic;", "main").expect("main");
    let code = dex.code_item(code_off).unwrap();
    let lines = dex.line_table(&code).unwrap();
    assert!(!lines.is_empty(), "expected a line table");
    assert!(lines.iter().all(|&(_, line)| line > 0));
}

#[test]
fn disassembly_is_dexdump_like() {
    let dex = load("fixtures.dex");
    let code_off = find_method(&dex, "LArithmetic;", "addLong").expect("addLong");
    let code = dex.code_item(code_off).unwrap();
    let lines = acos_dex::disasm::disassemble(&dex, &code).unwrap();
    assert!(!lines.is_empty());
    let text: String = lines
        .iter()
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("add-long"), "disassembly: {text}");
    assert!(text.contains("return-wide"), "disassembly: {text}");
}

#[test]
fn method_handles_present_in_invoke_custom_dex() {
    let dex = load("fixtures_invokecustom.dex");
    // d8 emits method handles for the lambda metafactory bootstrap.
    assert!(
        !dex.method_handles().is_empty(),
        "expected method handles in invoke-custom dex"
    );
    dex.method_handle(0).unwrap();
}
