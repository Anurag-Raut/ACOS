//! Import-coverage analysis for real third-party Android x86_64 libraries.
//!
//! This **parses only** — it never maps or executes the analysed libraries.
//! Usage:
//!
//! ```text
//! cargo run -p acos-native-loader --example coverage -- out.json lib1.so lib2.so ...
//! ```
//!
//! With no paths it analyses the libraries discovered on this machine.

use acos_native_loader::bionic;
use acos_native_loader::dyninfo::DynInfo;
use acos_native_loader::elf::Elf;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Serialize)]
struct LibCoverage {
    name: String,
    path: String,
    total_imports: usize,
    resolved_forwarded: usize,
    resolved_shimmed: usize,
    missing: Vec<String>,
}

#[derive(Serialize)]
struct Coverage {
    bionic_table_forwarded: usize,
    bionic_table_shimmed: usize,
    bionic_table_total: usize,
    libraries: Vec<LibCoverage>,
    note: String,
}

fn default_libs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let ndk = std::env::var("ANDROID_NDK_HOME").unwrap_or_else(|_| {
        format!("{home}/Android/Sdk/ndk/27.1.12297006")
    });
    let bt = format!("{home}/Android/Sdk/build-tools/35.0.0/renderscript/lib");
    vec![
        PathBuf::from(format!(
            "{ndk}/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/x86_64-linux-android/libc++_shared.so"
        )),
        PathBuf::from(format!("{bt}/packaged/x86_64/libRSSupport.so")),
        PathBuf::from(format!("{bt}/packaged/x86_64/librsjni_androidx.so")),
        PathBuf::from(format!("{bt}/packaged/x86_64/librsjni.so")),
        PathBuf::from(format!("{bt}/intermediates/x86_64/libc.so")),
        PathBuf::from(format!("{bt}/intermediates/x86_64/libm.so")),
    ]
}

fn analyse(path: &PathBuf) -> Option<LibCoverage> {
    let bytes = std::fs::read(path).ok()?;
    let elf = Elf::parse(&bytes).ok()?;
    let dyninfo = DynInfo::parse(&elf).ok()?;
    let mut imports: BTreeSet<String> = BTreeSet::new();
    for (i, s) in dyninfo.symbols.iter().enumerate() {
        if !s.is_undefined() {
            continue;
        }
        if let Some(name) = dyninfo.symbol_name(i) {
            if !name.is_empty() {
                imports.insert(name.to_string());
            }
        }
    }
    let mut forwarded = 0;
    let mut shimmed = 0;
    let mut missing = Vec::new();
    for name in &imports {
        match bionic::resolve(name) {
            Some(s) => match s.category {
                bionic::Category::Forwarded => forwarded += 1,
                bionic::Category::Shimmed => shimmed += 1,
            },
            None => missing.push(name.clone()),
        }
    }
    Some(LibCoverage {
        name: path.file_name()?.to_string_lossy().into_owned(),
        path: path.display().to_string(),
        total_imports: imports.len(),
        resolved_forwarded: forwarded,
        resolved_shimmed: shimmed,
        missing,
    })
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let out = if !args.is_empty() && args[0].ends_with(".json") {
        PathBuf::from(args.remove(0))
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../results/native-loader/coverage.json")
    };
    let libs: Vec<PathBuf> = if args.is_empty() {
        default_libs()
    } else {
        args.into_iter().map(PathBuf::from).collect()
    };

    let (forwarded, shimmed) = bionic::counts();
    let mut libraries = Vec::new();
    for lib in &libs {
        match analyse(lib) {
            Some(c) => {
                eprintln!(
                    "{}: {} imports, {} forwarded, {} shimmed, {} missing",
                    c.name,
                    c.total_imports,
                    c.resolved_forwarded,
                    c.resolved_shimmed,
                    c.missing.len()
                );
                libraries.push(c);
            }
            None => eprintln!("skipped (not an ELF64 x86_64 shared object): {}", lib.display()),
        }
    }

    let coverage = Coverage {
        bionic_table_forwarded: forwarded,
        bionic_table_shimmed: shimmed,
        bionic_table_total: forwarded + shimmed,
        libraries,
        note: "Parsed only; these libraries were not mapped or executed.".to_string(),
    };
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&out, serde_json::to_string_pretty(&coverage).unwrap()).unwrap();
    println!("wrote {}", out.display());
}
