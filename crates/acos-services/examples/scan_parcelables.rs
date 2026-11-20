//! Exploratory disassembler: print the `writeToParcel` invoke sequence of a
//! framework parcelable so the hand-written codec op lists can be verified.
//!
//! ```text
//! cargo run -p acos-services --example scan-parcelables -- \
//!     .cache/boot/android-34/dex/framework \
//!     android.content.pm.ApplicationInfo android.view.DisplayInfo
//! ```

use acos_dex::decode::{decode, payload_kind, payload_len};
use acos_dex::{ClassDef, CodeItem, DexFile};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: scan-parcelables <dex-dir> <class> [class ...]");
        std::process::exit(2);
    }
    let dir = &args[0];
    let classes: Vec<&str> = args[1..].iter().map(String::as_str).collect();
    let mut dexes = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("dex dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "dex").unwrap_or(false))
        .collect();
    entries.sort();
    for path in entries {
        let bytes = std::fs::read(&path).unwrap();
        match DexFile::parse(bytes) {
            Ok(d) => dexes.push((path, d)),
            Err(e) => eprintln!("skip {}: {e}", path.display()),
        }
    }
    for class in classes {
        let desc = format!("L{};", class.replace('.', "/"));
        let mut found = false;
        for (path, dex) in &dexes {
            if let Some(def) = dex.find_class(&desc) {
                found = true;
                println!("=== {class} ({}) ===", path.display());
                dump(dex, def, &desc);
            }
        }
        if !found {
            println!("=== {class} NOT FOUND ===");
        }
    }
}

fn dump(dex: &DexFile, def: &ClassDef, desc: &str) {
    let Ok(Some(cd)) = dex.class_data(def) else {
        return;
    };
    for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
        let Ok(r) = dex.method_ref(m.method_idx) else {
            continue;
        };
        if r.name != "writeToParcel"
            && r.name != "readFromParcel"
            && !r.name.contains("Squash")
            && r.name != "isSquashingAllowed"
            && r.name != "allowSquashing"
            && r.name != "writeException"
            && r.name != "readException"
        {
            continue;
        }
        if m.code_off == 0 {
            continue;
        }
        let Ok(code) = dex.code_item(m.code_off) else {
            continue;
        };
        println!(
            "--- {desc}->{}({}){} regs={}",
            r.name,
            r.parameters.concat(),
            r.return_type,
            code.registers_size
        );
        walk(dex, &code);
    }
}

fn walk(dex: &DexFile, code: &CodeItem) {
    let mut pc = 0usize;
    let mut guard = 0;
    while pc < code.insns.len() && guard < 5000 {
        guard += 1;
        let insn = if payload_kind(&code.insns, pc).is_some() {
            pc += payload_len(&code.insns, pc).unwrap_or(1);
            continue;
        } else {
            match decode(&code.insns, pc) {
                Ok(i) => i,
                Err(_) => {
                    pc += 1;
                    continue;
                }
            }
        };
        if insn.opcode.is_invoke() {
            if let Ok(m) = dex.method_ref(insn.index) {
                println!(
                    "  {pc:#06x} invoke {}->{}({}){}  regs={:?}",
                    m.class,
                    m.name,
                    m.parameters.concat(),
                    m.return_type,
                    &insn.regs[..insn.reg_count as usize]
                );
            } else {
                println!("  {pc:#06x} invoke method@{}", insn.index);
            }
        }
        pc += insn.size as usize;
    }
}
