//! `acos native-inventory <apk-or-so...>` — static import inventory for guest
//! shared objects.
//!
//! Never maps or executes anything: it only parses ELF images (from a `.so` file
//! or an APK's `lib/<abi>/*.so` entries) and classifies every undefined import
//! against the `acos-native-loader` bionic table.
//!
//! ```text
//! acos native-inventory app.apk --json /tmp/gaten.json
//! acos native-inventory lib/x86_64/*.so --top 30
//! ```

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use acos_native_loader::inventory::{
    aggregate, inventory_bytes, inventory_file, InventoryReport, LibraryInventory, Provision,
};
use clap::Args;

#[derive(Debug, Args)]
pub struct NativeInventoryArgs {
    /// `.so` files or `.apk`s to inventory.
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,
    /// ABI directory to parse inside APKs.
    #[arg(long, default_value = "x86_64")]
    pub abi: String,
    /// Write the full JSON report to this path (`-` for stdout).
    #[arg(long)]
    pub json: Option<PathBuf>,
    /// Number of unresolved symbols to print in the text summary.
    #[arg(long, default_value_t = 30)]
    pub top: usize,
    /// Include per-library detail in the text summary.
    #[arg(long)]
    pub verbose: bool,
}

pub fn run(args: NativeInventoryArgs) -> ExitCode {
    match inventory(&args) {
        Ok(report) => {
            let json = match serde_json::to_string_pretty(&report) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("error: failed to serialise inventory: {e}");
                    return ExitCode::FAILURE;
                }
            };
            match args.json.as_deref() {
                Some(p) if p != Path::new("-") => {
                    if let Err(e) = std::fs::write(p, &json) {
                        eprintln!("error: cannot write {}: {e}", p.display());
                        return ExitCode::FAILURE;
                    }
                    println!("wrote {} ({} libraries)", p.display(), report.libraries.len());
                }
                _ => println!("{json}"),
            }
            if args.json.is_none() || args.json.as_deref() != Some(Path::new("-")) {
                print_summary(&report, args.top, args.verbose);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn inventory(args: &NativeInventoryArgs) -> Result<InventoryReport, String> {
    let mut libs: Vec<LibraryInventory> = Vec::new();
    for p in &args.inputs {
        let is_apk = p
            .extension()
            .map(|e| e.eq_ignore_ascii_case("apk") || e.eq_ignore_ascii_case("zip"))
            .unwrap_or(false);
        if is_apk {
            libs.extend(inventory_apk(p, &args.abi)?);
        } else {
            libs.push(inventory_file(p)?);
        }
    }
    if libs.is_empty() {
        return Err("no shared objects found in the given inputs".into());
    }
    let abi = libs[0].abi.clone();
    Ok(aggregate(&abi, libs))
}

/// Parse every `lib/<abi>/*.so` entry of an APK (read-only, in memory).
fn inventory_apk(path: &Path, abi: &str) -> Result<Vec<LibraryInventory>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| format!("{}: not a zip: {e}", path.display()))?;
    let suffix = format!("lib/{abi}/");
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = entry.name().to_string();
        if !name.starts_with(&suffix) || !name.ends_with(".so") || entry.is_dir() {
            continue;
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{name}: {e}"))?;
        let base = name.rsplit('/').next().unwrap_or(&name).to_string();
        match inventory_bytes(&format!("{}!{name}", path.display()), &base, &bytes) {
            Ok(l) => out.push(l),
            Err(e) => eprintln!("warn: {name}: {e}"),
        }
    }
    if out.is_empty() {
        return Err(format!(
            "{}: no lib/{abi}/*.so entries",
            path.display()
        ));
    }
    Ok(out)
}

fn print_summary(report: &InventoryReport, top: usize, verbose: bool) {
    let out = std::io::stdout();
    let mut w = out.lock();
    let t = &report.totals;
    let _ = writeln!(
        w,
        "\n{} libraries, {} distinct imports: {} provided, {} partial, {} missing ({} defined in corpus)",
        t.libraries, t.imports, t.provided, t.partial, t.missing, t.defined_in_corpus
    );
    if verbose {
        let _ = writeln!(
            w,
            "\n{:<36} {:>10} {:>7} {:>7} {:>7}  containers",
            "library", "bytes?", "imports", "tls", "relocs"
        );
        for l in &report.libraries {
            let tls = [
                l.tls.general_dynamic.then_some("gd"),
                l.tls.initial_exec.then_some("ie"),
                l.tls.tlsdesc.then_some("desc"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("+");
            let _ = writeln!(
                w,
                "{:<36} {:>10} {:>7} {:>7} {:>7}  {}",
                l.name,
                "-",
                l.import_count,
                if tls.is_empty() { "-" } else { &tls },
                l.relocations.total,
                l.relocations.containers.join(",")
            );
        }
    }
    let _ = writeln!(
        w,
        "\n{:<5} {:<9} {:<6} {:<40} note",
        "rank", "provision", "libs", "symbol"
    );
    for (i, r) in report
        .unresolved
        .iter()
        .filter(|r| r.provision != Provision::Provided)
        .take(top)
        .enumerate()
    {
        let version = r
            .version
            .as_deref()
            .map(|v| format!("@{v}"))
            .unwrap_or_default();
        let _ = writeln!(
            w,
            "{:<5} {:<9} {:<6} {}{:<40} {}",
            i + 1,
            r.provision.as_str(),
            r.libraries.len(),
            r.symbol,
            version,
            r.note.clone().unwrap_or_default()
        );
    }
    let _ = writeln!(
        w,
        "(sites: {} import references across the corpus)",
        report.unresolved.iter().map(|r| r.sites).sum::<usize>()
    );
}
