//! `gapscan` — ART sysroot symbol/feature gap inventory.
//!
//! Walks the `DT_NEEDED` closure from an entry library (default `libart.so`)
//! inside an extracted Android sysroot and classifies every undefined import:
//!
//!   * `table`      — provided by `acos_native_loader::bionic::resolve`
//!   * `corpus`     — exported by another library in the closure
//!   * `missing`    — nothing provides it: the real gap list
//!
//! Also reports the ELF facts that drive loader work: TLS model, relocation
//! containers/types, `DT_INIT`/init-array counts, and `STT_GNU_IFUNC`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use acos_native_loader::inventory::{inventory_file, Import, LibraryInventory};

/// Mirror of `loader::is_shim_system_lib` (that fn is `pub(crate)`).
fn is_shim_system_lib(name: &str) -> bool {
    matches!(
        name,
        "libc.so"
            | "libm.so"
            | "libdl.so"
            | "liblog.so"
            | "libandroid.so"
            | "libz.so"
            | "libEGL.so"
            | "libGLESv2.so"
            | "libGLESv1_CM.so"
            | "libOpenSLES.so"
            | "libvulkan.so"
            | "libjnigraphics.so"
            | "libstdc++.so"
            | "libcutils.so"
            | "libnativewindow.so"
            | "libsync.so"
            | "libui.so"
            | "libutils.so"
    )
}

struct Lib {
    path: PathBuf,
    inv: LibraryInventory,
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("so") {
            out.push(p);
        }
    }
}

fn main() {
    let mut root: Option<PathBuf> = None;
    let mut entry = "libart.so".to_string();
    let mut json_out: Option<PathBuf> = None;
    let mut md_out: Option<PathBuf> = None;
    let mut verbose = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                root = Some(PathBuf::from(&args[i]));
            }
            "--entry" => {
                i += 1;
                entry = args[i].clone();
            }
            "--json" => {
                i += 1;
                json_out = Some(PathBuf::from(&args[i]));
            }
            "--markdown" => {
                i += 1;
                md_out = Some(PathBuf::from(&args[i]));
            }
            "--verbose" => verbose = true,
            other => {
                eprintln!("gapscan: unknown argument {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let Some(root) = root else {
        eprintln!("usage: gapscan --root <sysroot> [--entry libart.so] [--json p] [--markdown p] [--verbose]");
        std::process::exit(2);
    };

    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();

    // name (file name and SONAME) -> index
    let mut libs: Vec<Lib> = Vec::new();
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    for f in &files {
        match inventory_file(f) {
            Ok(inv) => {
                let idx = libs.len();
                if let Some(soname) = &inv.soname {
                    by_name.entry(soname.clone()).or_insert(idx);
                }
                by_name
                    .entry(inv.name.clone())
                    .or_insert(idx);
                libs.push(Lib {
                    path: f.clone(),
                    inv,
                });
            }
            Err(e) => eprintln!("gapscan: skip {}: {e}", f.display()),
        }
    }
    eprintln!("gapscan: {} libraries inventoried under {}", libs.len(), root.display());

    // --- DT_NEEDED closure from the entry library --------------------------
    let Some(&entry_idx) = by_name.get(entry.as_str()).or_else(|| by_name.get(&format!("{entry}.so"))) else {
        eprintln!("gapscan: entry {entry} not found in sysroot");
        std::process::exit(3);
    };
    let mut closure: BTreeSet<usize> = BTreeSet::new();
    let mut queue: Vec<usize> = vec![entry_idx];
    let mut unresolved_needed: BTreeSet<String> = BTreeSet::new();
    while let Some(idx) = queue.pop() {
        if !closure.insert(idx) {
            continue;
        }
        for need in libs[idx].inv.needed.clone() {
            if is_shim_system_lib(&need) {
                continue;
            }
            match by_name.get(&need) {
                Some(&dep) => queue.push(dep),
                None => {
                    unresolved_needed.insert(need);
                }
            }
        }
    }

    // Exports of the closure (ELF global scope interposition).
    let mut export_owner: BTreeMap<&str, &str> = BTreeMap::new();
    for &idx in &closure {
        for e in &libs[idx].inv.export_names {
            export_owner.entry(e.as_str()).or_insert(libs[idx].inv.name.as_str());
        }
    }

    // --- classify imports --------------------------------------------------
    #[derive(Default)]
    struct Row {
        table_forwarded: usize,
        table_shimmed: usize,
        corpus: usize,
        missing: usize,
    }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut missing: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut per_lib: Vec<(String, usize, usize, usize)> = Vec::new();
    for &idx in &closure {
        let lib = &libs[idx];
        let mut r = Row::default();
        for imp in &lib.inv.imports {
            match classify(&imp, &export_owner) {
                Class::Table { shimmed } => {
                    if shimmed {
                        r.table_shimmed += 1;
                    } else {
                        r.table_forwarded += 1;
                    }
                }
                Class::Corpus => r.corpus += 1,
                Class::Missing => {
                    r.missing += 1;
                    missing
                        .entry(imp.name.clone())
                        .or_default()
                        .insert(lib.inv.name.clone());
                }
            }
        }
        per_lib.push((lib.inv.name.clone(), lib.inv.imports.len(), r.missing, r.corpus));
        rows.insert(lib.inv.name.clone(), r);
    }

    // --- feature facts -----------------------------------------------------
    let mut tls_facts: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut reloc_facts: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for &idx in &closure {
        let lib = &libs[idx];
        tls_facts.insert(
            lib.inv.name.clone(),
            serde_json::json!({
                "general_dynamic": lib.inv.tls.general_dynamic,
                "initial_exec": lib.inv.tls.initial_exec,
                "tlsdesc": lib.inv.tls.tlsdesc,
                "defined_tls_symbols": lib.inv.tls.defined_tls_symbols,
                "segment": lib.inv.tls.segment,
            }),
        );
        reloc_facts.insert(
            lib.inv.name.clone(),
            serde_json::json!({
                "total": lib.inv.relocations.total,
                "by_type": lib.inv.relocations.by_type,
                "containers": lib.inv.relocations.containers,
                "aps2_groups": lib.inv.relocations.aps2_groups,
                "textrel": lib.inv.relocations.textrel,
                "ifuncs": lib.inv.ifuncs,
                "init_array": lib.inv.init.init_array_count,
            }),
        );
    }

    // --- report ------------------------------------------------------------
    let total_missing: usize = missing.len();
    let mut txt = String::new();
    txt.push_str(&format!(
        "# ART sysroot gap scan\n\nentry: `{entry}`  root: `{}`\n\nlibraries in closure: {}\nunresolved DT_NEEDED: {:?}\n\n",
        root.display(),
        closure.len(),
        unresolved_needed
    ));
    txt.push_str("| library | imports | table-fwd | table-shim | corpus | missing |\n|---|---:|---:|---:|---:|---:|\n");
    for (name, imports, miss, corpus) in &per_lib {
        let r = &rows[name];
        txt.push_str(&format!(
            "| `{name}` | {imports} | {} | {} | {corpus} | {miss} |\n",
            r.table_forwarded, r.table_shimmed
        ));
    }
    txt.push_str(&format!("\n## Missing symbols ({total_missing})\n\n"));
    for (sym, users) in &missing {
        txt.push_str(&format!(
            "- `{sym}` — needed by {}\n",
            users.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if verbose {
        println!("{txt}");
    }
    println!("missing distinct symbols: {total_missing}");
    for (sym, users) in &missing {
        println!("MISSING {sym} <- {}", users.iter().cloned().collect::<Vec<_>>().join(","));
    }

    if let Some(p) = md_out {
        std::fs::write(p, txt).ok();
    }
    if let Some(p) = json_out {
        let doc = serde_json::json!({
            "entry": entry,
            "root": root.to_string_lossy(),
            "closure": closure.iter().map(|&i| libs[i].inv.name.clone()).collect::<Vec<_>>(),
            "unresolved_needed": unresolved_needed,
            "libraries": rows.iter().map(|(k, v)| (k.clone(), serde_json::json!({
                "forwarded": v.table_forwarded,
                "shimmed": v.table_shimmed,
                "corpus": v.corpus,
                "missing": v.missing,
            }))).collect::<BTreeMap<_, _>>(),
            "missing": missing,
            "tls": tls_facts,
            "relocs": reloc_facts,
        });
        std::fs::write(p, serde_json::to_string_pretty(&doc).unwrap()).ok();
    }
}

enum Class {
    Table { shimmed: bool },
    Corpus,
    Missing,
}

fn classify(imp: &Import, exports: &BTreeMap<&str, &str>) -> Class {
    let name = imp.name.strip_suffix("@LIBC").unwrap_or(&imp.name);
    if is_shim_system_lib(name) {
        return Class::Corpus;
    }
    if exports.contains_key(name) {
        return Class::Corpus;
    }
    match acos_native_loader::bionic::resolve(name) {
        Some(shim) => Class::Table {
            shimmed: shim.category == acos_native_loader::bionic::Category::Shimmed,
        },
        None => Class::Missing,
    }
}
