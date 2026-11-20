//! `dexdump` — parse, verify and disassemble a DEX file.
//!
//! Usage:
//! ```text
//! cargo run -p acos-dex --example dexdump -- <file.dex> [--json] [--mnemonics] [--strict] [--bench[=N]]
//! ```
//!
//! `--strict` enforces the SHA-1 signature (default parse matches Android and
//! accepts a signature mismatch). `--bench` measures parse and verify
//! throughput across one or more DEX files and reports peak RSS.

use acos_dex::disasm::{disassemble, mnemonic_sequence};
use acos_dex::{DexFile, ParseOptions};
use std::sync::Arc;
use std::time::Instant;

fn print_usage() {
    eprintln!(
        "usage: dexdump <file.dex> [more.dex ...] [--json] [--mnemonics] [--strict] [--bench[=N]]"
    );
}

fn class_method_label(dex: &DexFile, method_idx: u32) -> String {
    match dex.method_ref(method_idx) {
        Ok(r) => format!("{}->{}{}", r.class, r.name, proto_string(dex, r.proto_idx)),
        Err(_) => format!("method@{method_idx:#06x}"),
    }
}

fn proto_string(dex: &DexFile, proto_idx: u32) -> String {
    match dex.proto(proto_idx) {
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
        Err(_) => "()?".to_string(),
    }
}

/// Peak resident set size in kilobytes, read from `/proc/self/status` (VmHWM).
fn vm_hwm_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .trim()
                .trim_end_matches(" kB")
                .trim()
                .parse::<u64>()
                .ok();
        }
    }
    None
}

fn run_bench(paths: &[&str], iters: u64) {
    let mut total_bytes = 0u64;
    let mut total_parse_s = 0f64;
    let mut total_verify_s = 0f64;
    let mut total_instructions = 0u64;
    let mut files = 0u64;

    for path in paths {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("skip {path}: {e}");
                continue;
            }
        };
        let arc: Arc<[u8]> = Arc::from(data);
        let mb = arc.len() as f64 / (1024.0 * 1024.0);

        let t = Instant::now();
        let mut last = None;
        for _ in 0..iters {
            last = Some(DexFile::parse(arc.clone()).expect("bench parse"));
        }
        let parse_s = t.elapsed().as_secs_f64();
        let dex = last.expect("at least one iteration");

        let t = Instant::now();
        let mut report = None;
        for _ in 0..iters {
            report = Some(dex.verify().expect("bench verify"));
        }
        let verify_s = t.elapsed().as_secs_f64();
        let report = report.expect("at least one iteration");

        // SHA-1 and full string materialisation are deferred out of parse;
        // report their one-off costs separately.
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(acos_dex::hash::sha1(&arc));
        }
        let sha1_s = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let n_strings = dex.strings().len();
        let strings_s = t.elapsed().as_secs_f64();

        println!(
            "{path}: bytes={} classes={} methods={} instructions={} strings={} parse_ms={:.2} verify_ms={:.2} sha1_ms={:.2} strings_ms={:.2} parse_MBps={:.1} verify_MBps={:.1}",
            arc.len(),
            report.classes,
            report.methods_with_code,
            report.instructions,
            n_strings,
            parse_s * 1000.0 / iters as f64,
            verify_s * 1000.0 / iters as f64,
            sha1_s * 1000.0 / iters as f64,
            strings_s * 1000.0,
            mb * iters as f64 / parse_s,
            mb * iters as f64 / verify_s,
        );

        total_bytes += arc.len() as u64;
        total_parse_s += parse_s;
        total_verify_s += verify_s;
        total_instructions += report.instructions;
        files += 1;
    }

    let total_work_mb = total_bytes as f64 * iters as f64 / (1024.0 * 1024.0);
    println!(
        "total: files={} bytes={} instructions={} iters={} aggregate_parse_MBps={:.1} aggregate_verify_MBps={:.1} peak_rss_kb={}",
        files,
        total_bytes,
        total_instructions,
        iters,
        total_work_mb / total_parse_s,
        total_work_mb / total_verify_s,
        vm_hwm_kb().unwrap_or(0),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        std::process::exit(2);
    }
    let mut json = false;
    let mut mnemonics = false;
    let mut strict = false;
    let mut bench: Option<u64> = None;
    let mut paths: Vec<&str> = Vec::new();
    for a in &args {
        match a.as_str() {
            "--json" => json = true,
            "--mnemonics" => mnemonics = true,
            "--strict" => strict = true,
            "--bench" => bench = Some(2000),
            "-h" | "--help" => {
                print_usage();
                return;
            }
            other if other.starts_with("--bench=") => {
                bench = other["--bench=".len()..].parse().ok();
            }
            other if other.starts_with('-') => {
                eprintln!("unknown flag: {other}");
                print_usage();
                std::process::exit(2);
            }
            other => paths.push(other),
        }
    }
    if paths.is_empty() {
        print_usage();
        std::process::exit(2);
    }

    if let Some(iters) = bench {
        run_bench(&paths, iters);
        return;
    }

    let path = paths[0];
    let options = if strict {
        ParseOptions::STRICT
    } else {
        ParseOptions::default()
    };

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": false,
                        "error": {"code": "DEX_IO_ERROR", "message": e.to_string()}
                    })
                );
            } else {
                eprintln!("error: {e}");
            }
            std::process::exit(1);
        }
    };

    let dex = match DexFile::parse_with(data, options) {
        Ok(d) => d,
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": false,
                        "error": {"code": e.code(), "message": e.to_string()}
                    })
                );
            } else {
                eprintln!("parse error [{}]: {e}", e.code());
            }
            std::process::exit(1);
        }
    };

    if mnemonics {
        emit_mnemonics(&dex);
        return;
    }

    let report = match dex.verify() {
        Ok(r) => r,
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": false,
                        "error": {"code": e.code(), "message": e.to_string()}
                    })
                );
            } else {
                eprintln!("verify error [{}]: {e}", e.code());
            }
            std::process::exit(1);
        }
    };

    if json {
        emit_json(&dex, &report);
    } else {
        emit_text(&dex, &report);
    }
}

fn emit_mnemonics(dex: &DexFile) {
    for def in dex.class_defs() {
        let class = dex.class_descriptor(def).unwrap_or("?");
        let data = match dex.class_data(def) {
            Ok(Some(d)) => d,
            _ => continue,
        };
        for m in data
            .direct_methods
            .iter()
            .chain(data.virtual_methods.iter())
        {
            if m.code_off == 0 {
                continue;
            }
            let (name, proto) = match dex.method_ref(m.method_idx) {
                Ok(r) => (r.name.to_string(), proto_string(dex, r.proto_idx)),
                Err(_) => (format!("method@{}", m.method_idx), "()?".to_string()),
            };
            let code = match dex.code_item(m.code_off) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let seq = mnemonic_sequence(&code).unwrap_or_default();
            let count = seq.len();
            println!("M {class} {name} {proto} {count} {}", seq.join(" "));
        }
    }
}

fn signature_line(valid: bool) -> String {
    if valid {
        "valid".to_string()
    } else {
        "MISMATCH (accepted, Android does not verify)".to_string()
    }
}

fn emit_text(dex: &DexFile, report: &acos_dex::VerifyReport) {
    let h = dex.header();
    println!("== header ==");
    println!("magic: dex\\n{:03}", h.version);
    println!("version: {:03}", h.version);
    println!("checksum: {:#010x}", h.checksum);
    println!("signature: {}", signature_line(dex.signature_valid()));
    println!("signature_hex: {}", acos_dex::hash::hex(&h.signature));
    println!("file_size: {}", h.file_size);
    println!("header_size: {:#x}", h.header_size);
    println!("endian_tag: {:#010x}", h.endian_tag);
    println!("map_off: {:#x}", h.map_off);
    println!(
        "string_ids: {} @ {:#x}",
        h.string_ids_size, h.string_ids_off
    );
    println!("type_ids: {} @ {:#x}", h.type_ids_size, h.type_ids_off);
    println!("proto_ids: {} @ {:#x}", h.proto_ids_size, h.proto_ids_off);
    println!("field_ids: {} @ {:#x}", h.field_ids_size, h.field_ids_off);
    println!(
        "method_ids: {} @ {:#x}",
        h.method_ids_size, h.method_ids_off
    );
    println!(
        "class_defs: {} @ {:#x}",
        h.class_defs_size, h.class_defs_off
    );
    println!("call_site_ids: {}", dex.call_site_ids().len());
    println!("method_handles: {}", dex.method_handles().len());
    println!(
        "hiddenapi_class_data: {}",
        dex.hiddenapi_class_data().is_some()
    );

    println!("\n== classes ==");
    for def in dex.class_defs() {
        let class = dex.class_descriptor(def).unwrap_or("?");
        let superclass = dex
            .superclass_descriptor(def)
            .ok()
            .flatten()
            .unwrap_or("<none>");
        println!(
            "\nclass {class}  access={:#06x}  super={superclass}",
            def.access_flags
        );
        if let Ok(Some(src)) = dex.source_file(def) {
            println!("  source: {src}");
        }
        let data = match dex.class_data(def) {
            Ok(Some(d)) => d,
            Ok(None) => {
                println!("  (no class data)");
                continue;
            }
            Err(e) => {
                println!("  (class data error: {e})");
                continue;
            }
        };
        for m in data
            .direct_methods
            .iter()
            .chain(data.virtual_methods.iter())
        {
            let label = class_method_label(dex, m.method_idx);
            if m.code_off == 0 {
                println!(
                    "  method {label}  access={:#06x}  (abstract/native)",
                    m.access_flags
                );
                continue;
            }
            match dex.code_item(m.code_off) {
                Ok(code) => {
                    println!(
                        "  method {label}  access={:#06x}  regs={} ins={} outs={} code={:#x}",
                        m.access_flags,
                        code.registers_size,
                        code.ins_size,
                        code.outs_size,
                        m.code_off
                    );
                    match disassemble(dex, &code) {
                        Ok(lines) => {
                            for (pc, text) in lines {
                                println!("    {pc:04x}: {text}");
                            }
                        }
                        Err(e) => println!("    <disassembly error: {e}>"),
                    }
                }
                Err(e) => println!("  method {label}  <code error: {e}>"),
            }
        }
    }

    println!("\n== verify ==");
    println!("classes: {}", report.classes);
    println!("methods_with_code: {}", report.methods_with_code);
    println!("instructions: {}", report.instructions);
    println!("switches: {}", report.switches);
    println!("fill_array_data: {}", report.fill_array_data);
    println!("try_items: {}", report.try_items);
    println!("catch_handlers: {}", report.catch_handlers);
    println!("max_registers: {}", report.max_registers);
    println!("OK");
}

fn emit_json(dex: &DexFile, report: &acos_dex::VerifyReport) {
    let h = dex.header();
    let mut classes = Vec::new();
    for def in dex.class_defs() {
        let class = dex.class_descriptor(def).unwrap_or("?");
        let superclass = dex
            .superclass_descriptor(def)
            .ok()
            .flatten()
            .unwrap_or("<none>");
        let mut methods = Vec::new();
        if let Ok(Some(data)) = dex.class_data(def) {
            for m in data
                .direct_methods
                .iter()
                .chain(data.virtual_methods.iter())
            {
                let label = class_method_label(dex, m.method_idx);
                let (regs, ins, outs, disasm) = match dex.code_item(m.code_off) {
                    Ok(code) if m.code_off != 0 => {
                        let lines = disassemble(dex, &code)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(pc, text)| serde_json::json!({"pc": pc, "text": text}))
                            .collect::<Vec<_>>();
                        (
                            code.registers_size as u64,
                            code.ins_size as u64,
                            code.outs_size as u64,
                            lines,
                        )
                    }
                    _ => (0, 0, 0, Vec::new()),
                };
                methods.push(serde_json::json!({
                    "method": label,
                    "method_idx": m.method_idx,
                    "access_flags": m.access_flags,
                    "code_off": m.code_off,
                    "registers_size": regs,
                    "ins_size": ins,
                    "outs_size": outs,
                    "disassembly": disasm,
                }));
            }
        }
        classes.push(serde_json::json!({
            "descriptor": class,
            "access_flags": def.access_flags,
            "superclass": superclass,
            "methods": methods,
        }));
    }

    let out = serde_json::json!({
        "ok": true,
        "header": {
            "version": h.version,
            "checksum": h.checksum,
            "signature": acos_dex::hash::hex(&h.signature),
            "signature_valid": dex.signature_valid(),
            "file_size": h.file_size,
            "endian_tag": h.endian_tag,
            "string_ids_size": h.string_ids_size,
            "type_ids_size": h.type_ids_size,
            "proto_ids_size": h.proto_ids_size,
            "field_ids_size": h.field_ids_size,
            "method_ids_size": h.method_ids_size,
            "class_defs_size": h.class_defs_size,
            "call_site_ids_size": dex.call_site_ids().len(),
            "method_handles_size": dex.method_handles().len(),
            "hiddenapi_class_data": dex.hiddenapi_class_data().is_some(),
        },
        "classes": classes,
        "verify": {
            "classes": report.classes,
            "methods_with_code": report.methods_with_code,
            "instructions": report.instructions,
            "switches": report.switches,
            "fill_array_data": report.fill_array_data,
            "try_items": report.try_items,
            "catch_handlers": report.catch_handlers,
            "max_registers": report.max_registers,
            "histogram": report
                .histogram_entries()
                .into_iter()
                .map(|(op, name, n)| serde_json::json!({"opcode": op, "mnemonic": name, "count": n}))
                .collect::<Vec<_>>(),
        },
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}
