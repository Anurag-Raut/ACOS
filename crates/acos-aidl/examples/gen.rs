//! `gen` — regenerate the AIDL tables from the real boot classpath.
//!
//! ```text
//! cargo run -p acos-aidl --example gen -- [--all] [--boot DIR]
//! ```
//!
//! * default: regenerate `crates/acos-aidl/src/generated/tables.rs` for the
//!   measured service-layer subset (deterministically);
//! * `--all`: scan every `$Stub` in the boot classpath and write
//!   `results/aidl/all_interfaces.json`, reporting totals.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use acos_aidl::generate::{
    coverage, read_subset_rows, render_json, render_tables, scan_default, BootScanner,
};

fn usage() {
    eprintln!(
        "usage: gen [--all] [--boot DIR] [--workspace DIR] [--out FILE] [--json FILE] [--quiet]"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut all = false;
    let mut boot: Option<String> = None;
    let mut workspace: Option<String> = None;
    let mut out: Option<String> = None;
    let mut json: Option<String> = None;
    let mut quiet = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--all" => all = true,
            "--quiet" => quiet = true,
            "--boot" => {
                i += 1;
                boot = args.get(i).cloned();
            }
            "--workspace" => {
                i += 1;
                workspace = args.get(i).cloned();
            }
            "--out" => {
                i += 1;
                out = args.get(i).cloned();
            }
            "--json" => {
                i += 1;
                json = args.get(i).cloned();
            }
            "--help" | "-h" => {
                usage();
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                usage();
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let workspace = PathBuf::from(workspace.unwrap_or_else(|| ".".to_string()));
    let boot_dir = PathBuf::from(
        boot.unwrap_or_else(|| workspace.join(".cache/boot/android-34").display().to_string()),
    );

    let scanner = match BootScanner::load(&boot_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load boot classpath at {}: {e}", boot_dir.display());
            std::process::exit(1);
        }
    };

    if !quiet {
        eprintln!(
            "boot: {} jars, {} dex files ({})",
            scanner.jars.len(),
            scanner.dex_count(),
            boot_dir.display()
        );
    }

    if all {
        let t0 = Instant::now();
        let (ifaces, errors) = scanner.decode_all();
        let scan_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let cov = coverage(&ifaces, None);
        let json_path = PathBuf::from(json.unwrap_or_else(|| {
            workspace
                .join("results/aidl/all_interfaces.json")
                .display()
                .to_string()
        }));
        if let Some(parent) = json_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("mkdir {}: {e}", parent.display());
                std::process::exit(1);
            }
        }
        let t1 = Instant::now();
        let text = match render_json(&ifaces) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("json: {e}");
                std::process::exit(1);
            }
        };
        let render_ms = t1.elapsed().as_secs_f64() * 1000.0;
        if let Err(e) = std::fs::write(&json_path, &text) {
            eprintln!("write {}: {e}", json_path.display());
            std::process::exit(1);
        }
        println!(
            "all: interfaces={} methods={} fully_decoded={} ({:.1}%) decode_errors={} scan_ms={:.1} render_ms={:.1} -> {} ({} bytes)",
            cov.interfaces,
            cov.methods,
            cov.fully_decoded,
            cov.percent(),
            errors.len(),
            scan_ms,
            render_ms,
            json_path.display(),
            text.len()
        );
        for (stub, err) in errors.iter().take(20) {
            println!("  error {stub}: {err}");
        }

        // Group `Unknown`s and decode errors by cause for the report.
        let mut by_reason: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for u in &cov.unknown {
            by_reason
                .entry(u.reason.clone())
                .or_default()
                .push(format!("{}.{} (code {})", u.interface, u.method, u.code));
        }
        let mut err_by_reason: BTreeMap<String, usize> = BTreeMap::new();
        for (_, err) in &errors {
            // Group by the generic failure kind (drop the class name).
            let key = err.split(": ").next().unwrap_or(err).trim().to_string();
            *err_by_reason.entry(key).or_default() += 1;
        }
        let mut report = String::new();
        report.push_str("# acos-aidl full boot-classpath coverage\n\n");
        report.push_str(&format!("interfaces: {}\n", cov.interfaces));
        report.push_str(&format!("methods: {}\n", cov.methods));
        report.push_str(&format!(
            "fully decoded: {} ({:.1}%)\n",
            cov.fully_decoded,
            cov.percent()
        ));
        report.push_str(&format!("decode errors: {}\n\n", errors.len()));
        report.push_str("## Unknowns grouped by cause\n\n");
        for (reason, list) in &by_reason {
            report.push_str(&format!("- {} ({} methods):\n", reason, list.len()));
            for l in list.iter().take(25) {
                report.push_str(&format!("    {l}\n"));
            }
            if list.len() > 25 {
                report.push_str(&format!("    ... and {} more\n", list.len() - 25));
            }
        }
        report.push_str("\n## Decode errors grouped by cause\n\n");
        for (reason, count) in &err_by_reason {
            report.push_str(&format!("- {}: {count}\n", reason));
        }
        let cov_path = json_path.with_file_name("all_coverage.txt");
        let _ = std::fs::write(&cov_path, report);
        println!("  coverage report -> {}", cov_path.display());
        return;
    }

    let rows = match read_subset_rows(&workspace) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to read measured work-lists: {e}");
            std::process::exit(1);
        }
    };
    let t0 = Instant::now();
    let (ifaces, errors) = scan_default(&scanner, &rows);
    let scan_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let subset = acos_aidl::generate::subset_methods(&rows);
    let cov = coverage(&ifaces, Some(&subset));
    let t1 = Instant::now();
    let text = render_tables(&ifaces);
    let render_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let out_path = PathBuf::from(out.unwrap_or_else(|| {
        workspace
            .join("crates/acos-aidl/src/generated/tables.rs")
            .display()
            .to_string()
    }));
    if let Some(parent) = out_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("mkdir {}: {e}", parent.display());
            std::process::exit(1);
        }
    }
    if let Err(e) = std::fs::write(&out_path, &text) {
        eprintln!("write {}: {e}", out_path.display());
        std::process::exit(1);
    }
    println!(
        "default: interfaces={} methods={} fully_decoded={} ({:.1}%) missing_interfaces={} scan_ms={:.1} render_ms={:.1} -> {} ({} bytes)",
        cov.interfaces,
        cov.methods,
        cov.fully_decoded,
        cov.percent(),
        errors.len(),
        scan_ms,
        render_ms,
        out_path.display(),
        text.len()
    );
    for (iface, err) in &errors {
        println!("  missing {iface}: {err}");
    }
    for u in cov.unknown.iter().take(40) {
        println!("  unknown {}.{} code={}: {}", u.interface, u.method, u.code, u.reason);
    }
}
