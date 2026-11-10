//! The `acos` command-line interface.
//!
//! Implements exactly one command so far: `acos inspect <apk> [--json]`.
//! Unimplemented subcommands deliberately do not exist yet.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use acos_apk::arsc::ResourceTable;
use acos_apk::manifest::{Component, ComponentKind};
use acos_apk::{Apk, ApkError};
use acos_vm::{Vm, VmError, VmOptions};
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::{json, Value};

/// ACOS — agent-native Android execution OS (static analysis front-end).
#[derive(Debug, Parser)]
#[command(name = "acos", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect an APK and print a static-analysis summary.
    Inspect(InspectArgs),
    /// Execute a DEX/APK's `main` on the ACOS Rust interpreter.
    RunDex(RunDexArgs),
}

#[derive(Debug, clap::Args)]
struct RunDexArgs {
    /// Path to a `.dex`, `.apk`, or a directory containing `classes.dex`.
    path: PathBuf,

    /// Fully-qualified main class, e.g. `Main` or `com.example.Main`.
    #[arg(long, default_value = "Main")]
    main: String,

    /// Emit the standard ACOS JSON response envelope instead of text.
    #[arg(long)]
    json: bool,

    /// A program argument passed to `main(String[])` (repeatable).
    #[arg(long = "arg")]
    args: Vec<String>,

    /// Core-library provider: `bootstrap` (Rust built-ins) or `real`
    /// (the real Android 14 boot-classpath DEX).
    #[arg(long, default_value = "bootstrap")]
    corelib: String,

    /// Boot-classpath cache directory (default: `.cache/boot/android-34`).
    #[arg(long = "boot-dir")]
    boot_dir: Option<PathBuf>,

    /// Record every native invocation and emit `natives.tsv`.
    #[arg(long = "trace-natives")]
    trace_natives: bool,

    /// Where to write the native trace (implies `--trace-natives`).
    #[arg(long = "trace-natives-file")]
    trace_natives_file: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct InspectArgs {
    /// Path to the APK to inspect.
    apk: PathBuf,

    /// Emit the standard ACOS JSON response envelope instead of text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct Envelope {
    request_id: String,
    ok: bool,
    status: &'static str,
    backend: &'static str,
    fidelity: &'static str,
    capabilities_used: Vec<&'static str>,
    error: Option<ErrorObj>,
    result: Option<Value>,
    metrics: Metrics,
}

#[derive(Debug, Clone, Serialize)]
struct ErrorObj {
    code: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct Metrics {
    wall_ms: u128,
}

const FIDELITY: &str = "static-analysis";
const BACKEND: &str = "acos-native";

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect(args) => inspect(args),
        Command::RunDex(args) => run_dex(args),
    }
}

fn inspect(args: InspectArgs) -> ExitCode {
    let request_id = new_request_id();
    let started = Instant::now();

    match load_and_build(&args.apk) {
        Ok(result) => {
            let metrics = Metrics {
                wall_ms: started.elapsed().as_millis(),
            };
            if args.json {
                let envelope = Envelope {
                    request_id,
                    ok: true,
                    status: "pass",
                    backend: BACKEND,
                    fidelity: FIDELITY,
                    capabilities_used: vec!["apk.inspect"],
                    error: None,
                    result: Some(result.clone()),
                    metrics,
                };
                print_json(&envelope);
            } else {
                print_text(&result);
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            let error = ErrorObj {
                code: err.code().to_string(),
                message: err.message().to_string(),
            };
            if args.json {
                let envelope = Envelope {
                    request_id,
                    ok: false,
                    status: "error",
                    backend: BACKEND,
                    fidelity: FIDELITY,
                    capabilities_used: vec!["apk.inspect"],
                    error: Some(error.clone()),
                    result: None,
                    metrics: Metrics {
                        wall_ms: started.elapsed().as_millis(),
                    },
                };
                print_json(&envelope);
            } else {
                eprintln!("error: {}: {}", error.code, error.message);
            }
            ExitCode::FAILURE
        }
    }
}

fn load_and_build(path: &PathBuf) -> Result<Value, ApkError> {
    let apk = Apk::open(path)?;
    let manifest = apk.manifest()?;
    let resources = apk.resources()?;
    let res_ref: Option<&ResourceTable> = resources.as_ref();

    let label = manifest.app_label(res_ref);
    let launcher = manifest.launcher_activity().map(|c| c.name.clone());

    let components = json!({
        "activities": components_json(&manifest.activities, res_ref),
        "services": components_json(&manifest.services, res_ref),
        "receivers": components_json(&manifest.receivers, res_ref),
        "providers": components_json(&manifest.providers, res_ref),
    });

    let dex_files: Vec<Value> = apk
        .dex_files
        .iter()
        .map(|d| json!({"name": d.name, "size": d.size}))
        .collect();

    let native_libs: Vec<Value> = apk
        .native_libs
        .iter()
        .flat_map(|(abi, libs)| {
            let abi = abi.clone();
            libs.iter()
                .map(move |l| json!({"abi": abi, "name": l.name, "size": l.size}))
        })
        .collect();

    let assets: Vec<Value> = apk
        .assets
        .iter()
        .map(|a| json!({"name": a.name, "size": a.size}))
        .collect();

    let resource_summary = match res_ref {
        Some(res) => json!({
            "present": true,
            "packages": res.packages.len(),
            "entries": res.entry_count(),
            "type_specs": res.packages.iter().map(|p| p.type_specs.len()).sum::<usize>(),
            "global_strings": res.global_strings.len(),
            "types": res.type_summary().iter().map(|(name, count)| {
                json!({"name": name, "entries": count})
            }).collect::<Vec<_>>(),
        }),
        None => json!({
            "present": false,
            "packages": 0,
            "entries": 0,
            "type_specs": 0,
            "global_strings": 0,
            "types": [],
        }),
    };

    let result = json!({
        "apk_path": apk.source,
        "apk_size_bytes": apk.raw_bytes().len(),
        "zip_entries": apk.entries.len(),
        "package": manifest.package,
        "version_code": manifest.version_code,
        "version_code_major": manifest.version_code_major,
        "version_name": manifest.version_name,
        "min_sdk": manifest.min_sdk,
        "target_sdk": manifest.target_sdk,
        "max_sdk": manifest.max_sdk,
        "compile_sdk": manifest.compile_sdk,
        "label": label,
        "application": {
            "name": manifest.application.name,
            "debuggable": manifest.application.debuggable,
            "allow_backup": manifest.application.allow_backup,
            "extract_native_libs": manifest.application.extract_native_libs,
            "theme": manifest.application.theme,
            "process": manifest.application.process,
        },
        "launcher_activity": launcher,
        "permissions": manifest.uses_permissions.iter().map(|p| json!({
            "name": p.name,
            "max_sdk": p.max_sdk,
        })).collect::<Vec<_>>(),
        "uses_features": manifest.uses_features.iter().map(|f| json!({
            "name": f.name,
            "required": f.required,
            "version": f.version,
        })).collect::<Vec<_>>(),
        "components": components,
        "dex_files": dex_files,
        "native_abis": apk.native_abis(),
        "native_libs": native_libs,
        "assets": assets,
        "signing": apk.signing,
        "resources": resource_summary,
    });
    Ok(result)
}

fn components_json(components: &[Component], res: Option<&ResourceTable>) -> Vec<Value> {
    components
        .iter()
        .map(|c| {
            let label = c.label.clone().or_else(|| {
                c.label_ref
                    .and_then(|id| res.and_then(|r| r.resolve_string(id)))
            });
            json!({
                "kind": kind_name(c.kind),
                "name": c.name,
                "exported": c.exported,
                "launch_mode": c.launch_mode,
                "process": c.process,
                "permission": c.permission,
                "label": label,
                "intent_filters": c.intent_filters.iter().map(|f| json!({
                    "actions": f.actions,
                    "categories": f.categories,
                    "data": f.data.iter().map(|d| json!({
                        "scheme": d.scheme,
                        "host": d.host,
                        "port": d.port,
                        "path": d.path,
                        "path_prefix": d.path_prefix,
                        "path_pattern": d.path_pattern,
                        "mime_type": d.mime_type,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn kind_name(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Activity => "activity",
        ComponentKind::Service => "service",
        ComponentKind::Receiver => "receiver",
        ComponentKind::Provider => "provider",
    }
}

/// Peak resident set size in KiB (Linux `/proc/self/status` `VmHWM`).
fn peak_rss_kb() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(kb) = rest.split_whitespace().next() {
                return kb.parse().unwrap_or(0);
            }
        }
    }
    0
}

fn new_request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("req_{:x}_{:x}", std::process::id(), nanos)
}

fn load_dex_images(path: &PathBuf) -> Result<Vec<Vec<u8>>, String> {
    if path.is_dir() {
        let mut out = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(path)
            .map_err(|e| format!("read dir {path:?}: {e}"))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "dex").unwrap_or(false))
            .collect();
        entries.sort();
        for p in entries {
            out.push(std::fs::read(&p).map_err(|e| format!("read {p:?}: {e}"))?);
        }
        if out.is_empty() {
            return Err(format!("no .dex files in {path:?}"));
        }
        return Ok(out);
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("apk") | Some("zip") => {
            let apk = Apk::open(path).map_err(|e| format!("open apk: {e}"))?;
            let mut out = Vec::new();
            for d in &apk.dex_files {
                if let Some(b) = apk.dex_bytes(&d.name) {
                    out.push(b.to_vec());
                }
            }
            if out.is_empty() {
                return Err("apk contains no dex".to_string());
            }
            Ok(out)
        }
        _ => {
            let b = std::fs::read(path).map_err(|e| format!("read {path:?}: {e}"))?;
            Ok(vec![b])
        }
    }
}

fn run_dex(args: RunDexArgs) -> ExitCode {
    let request_id = new_request_id();
    let started = Instant::now();

    let corelib = match acos_vm::CoreLib::parse(&args.corelib) {
        Some(c) => c,
        None => {
            return run_error(
                &request_id,
                args.json,
                "VM_INTERNAL",
                &format!("unknown --corelib value {:?} (expected bootstrap|real)", args.corelib),
                None,
                started,
            )
        }
    };

    let options = VmOptions {
        heap_max_bytes: std::env::var("ACOS_HEAP_MAX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(256 * 1024 * 1024),
        gc_stress: std::env::var_os("ACOS_GC_STRESS").is_some(),
        corelib,
        boot_dir: args.boot_dir.clone(),
        trace_natives: args.trace_natives || args.trace_natives_file.is_some(),
    };

    let images = match load_dex_images(&args.path) {
        Ok(i) => i,
        Err(msg) => {
            return run_error(&request_id, args.json, "VM_INTERNAL", &msg, None, started)
        }
    };
    let image_read_ms = started.elapsed().as_millis();

    let vm_started = Instant::now();
    let mut vm = match Vm::new(options) {
        Ok(v) => v,
        Err(e) => {
            return run_error(&request_id, args.json, e.code(), &e.to_string(), None, started)
        }
    };
    let vm_new_ms = vm_started.elapsed().as_millis();
    for img in images {
        match acos_dex::DexFile::parse(img) {
            Ok(dex) => {
                vm.add_dex(dex);
            }
            Err(e) => {
                return run_error(
                    &request_id,
                    args.json,
                    "DEX_LOAD_ERROR",
                    &format!("{e}"),
                    None,
                    started,
                )
            }
        }
    }

    let startup_ms = started.elapsed().as_millis();
    let run_started = Instant::now();
    let outcome = vm.run(&args.main, &args.args);
    let run_ms = run_started.elapsed().as_millis();
    let wall_ms = started.elapsed().as_millis();
    let m = &outcome.metrics;

    if let Some(path) = &args.trace_natives_file {
        if let Some(tr) = &vm.native_trace {
            let _ = std::fs::write(path, tr.to_tsv());
        }
    }
    if args.trace_natives && args.trace_natives_file.is_none() {
        if let Some(tr) = &vm.native_trace {
            eprint!("{}", tr.to_tsv());
        }
    }

    if !args.json {
        use std::io::Write;
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
        return ExitCode::from(outcome.exit_code.clamp(0, 255) as u8);
    }

    let result = json!({
        "exit_code": outcome.exit_code,
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
        "uncaught_exception": outcome.uncaught.as_ref().map(|u| json!({
            "class": u.class,
            "message": u.message,
            "stack": u.stack,
        })),
    });

    if outcome.exit_code == 70 && outcome.uncaught.is_none() {
        // Prefer the typed error so host ceilings and future codes surface
        // verbatim (`HOST_MEMORY_LIMIT`, `TIMEOUT`) instead of `VM_INTERNAL`.
        let (code, message) = match &outcome.vm_error {
            Some(e) => (e.code().to_string(), e.to_string()),
            None => parse_vm_error(&outcome.stderr),
        };
        let details = outcome.vm_error.as_ref().and_then(vm_error_details);
        return run_error(&request_id, true, &code, &message, details, started);
    }

    let envelope = json!({
        "request_id": request_id,
        "ok": true,
        "status": "pass",
        "backend": "acos-native",
        "fidelity": "native-runtime",
        "capabilities_used": ["dex.run"],
        "error": Value::Null,
        "result": result,
        "metrics": {
            "wall_ms": wall_ms,
            "startup_ms": startup_ms,
            "image_read_ms": image_read_ms,
            "vm_new_ms": vm_new_ms,
            "dex_parse_ms": startup_ms.saturating_sub(image_read_ms + vm_new_ms),
            "run_ms": run_ms,
            "peak_rss_kb": peak_rss_kb(),
            "instructions": m.instructions,
            "invokes": m.invokes,
            "allocations": m.allocations,
            "gc_count": m.gc_count,
            "gc_ms": m.gc_ms,
            "heap_peak_bytes": m.heap_peak_bytes,
            "classes_loaded": m.classes_loaded,
            "clinit_count": m.clinit_count,
            "natives_registered": vm.natives.len(),
        },
    });
    print_json(&envelope);
    ExitCode::from(outcome.exit_code.clamp(0, 255) as u8)
}

fn parse_vm_error(stderr: &str) -> (String, String) {
    let line = stderr.lines().next().unwrap_or("");
    let body = line.strip_prefix("acos-vm error: ").unwrap_or(line);
    if let Some(rest) = body.strip_prefix("UNSUPPORTED_JAVA_API") {
        return (
            "UNSUPPORTED_JAVA_API".to_string(),
            rest.trim_start_matches([':', ' ']).trim().to_string(),
        );
    }
    if let Some(rest) = body.strip_prefix("UNSUPPORTED_BYTECODE") {
        return (
            "UNSUPPORTED_BYTECODE".to_string(),
            rest.trim_start_matches([':', ' ']).trim().to_string(),
        );
    }
    ("VM_INTERNAL".to_string(), body.to_string())
}

fn run_error(
    request_id: &str,
    json: bool,
    code: &str,
    message: &str,
    details: Option<Value>,
    started: Instant,
) -> ExitCode {
    if json {
        let status = if code.starts_with("UNSUPPORTED") {
            "unsupported"
        } else {
            "error"
        };
        let envelope = json!({
            "request_id": request_id,
            "ok": false,
            "status": status,
            "backend": "acos-native",
            "fidelity": "native-runtime",
            "capabilities_used": ["dex.run"],
            "error": { "code": code, "message": message, "details": details },
            "result": Value::Null,
            "metrics": { "wall_ms": started.elapsed().as_millis() },
        });
        print_json(&envelope);
    } else {
        eprintln!("acos-vm error: {code}: {message}");
    }
    ExitCode::from(70u8)
}

#[allow(dead_code)]
fn vm_error_details(e: &VmError) -> Option<Value> {
    match e {
        VmError::UnsupportedApi { api, at } => Some(json!({"api": api, "at": at})),
        VmError::UnsupportedBytecode { opcode, at } => {
            Some(json!({"opcode": opcode, "at": at}))
        }
        _ => None,
    }
}

fn print_json<T: Serialize>(envelope: &T) {
    match serde_json::to_string_pretty(envelope) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error: failed to serialize JSON envelope: {e}"),
    }
}

fn print_text(result: &Value) {
    let s = |key: &str| {
        result
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let n = |key: &str| {
        result
            .get(key)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string())
    };

    println!("APK: {}", s("apk_path"));
    println!(
        "  size: {} bytes, {} zip entries",
        result
            .get("apk_size_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        result
            .get("zip_entries")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
    println!(
        "  package: {} versionCode={} versionName={}",
        s("package"),
        n("version_code"),
        n("version_name"),
    );
    println!("  sdk: min={} target={}", n("min_sdk"), n("target_sdk"),);
    let label = s("label");
    if !label.is_empty() {
        println!("  label: {label}");
    }
    let launcher = s("launcher_activity");
    if !launcher.is_empty() {
        println!("  launcher activity: {launcher}");
    }

    if let Some(perms) = result.get("permissions").and_then(Value::as_array) {
        if !perms.is_empty() {
            let names: Vec<String> = perms
                .iter()
                .filter_map(|p| p.get("name").and_then(Value::as_str).map(str::to_string))
                .collect();
            println!("  permissions: {}", names.join(", "));
        }
    }

    if let Some(components) = result.get("components").and_then(Value::as_object) {
        let counts: Vec<String> = components
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_array().map_or(0, Vec::len)))
            .collect();
        println!("  components: {}", counts.join(" "));
    }

    if let Some(dex) = result.get("dex_files").and_then(Value::as_array) {
        let list: Vec<String> = dex
            .iter()
            .map(|d| {
                format!(
                    "{} ({} bytes)",
                    d.get("name").and_then(Value::as_str).unwrap_or(""),
                    d.get("size").and_then(Value::as_u64).unwrap_or(0)
                )
            })
            .collect();
        println!("  dex: {}", list.join(", "));
    }

    if let Some(native) = result.get("native_libs").and_then(Value::as_array) {
        let list: Vec<String> = native
            .iter()
            .map(|l| {
                format!(
                    "{}/{} ({} bytes)",
                    l.get("abi").and_then(Value::as_str).unwrap_or(""),
                    l.get("name").and_then(Value::as_str).unwrap_or(""),
                    l.get("size").and_then(Value::as_u64).unwrap_or(0)
                )
            })
            .collect();
        if !list.is_empty() {
            println!("  native libs: {}", list.join(", "));
        }
    }

    if let Some(signing) = result.get("signing") {
        println!(
            "  signing: v1={} v2={} v3={} ({})",
            signing.get("v1").and_then(Value::as_bool).unwrap_or(false),
            signing.get("v2").and_then(Value::as_bool).unwrap_or(false),
            signing.get("v3").and_then(Value::as_bool).unwrap_or(false),
            signing.get("note").and_then(Value::as_str).unwrap_or(""),
        );
    }

    if let Some(res) = result.get("resources") {
        if res.get("present").and_then(Value::as_bool).unwrap_or(false) {
            let types: Vec<String> = res
                .get("types")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|t| {
                            format!(
                                "{}({})",
                                t.get("name").and_then(Value::as_str).unwrap_or(""),
                                t.get("entries").and_then(Value::as_u64).unwrap_or(0)
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            println!(
                "  resources: {} package(s), {} entries; types: {}",
                res.get("packages").and_then(Value::as_u64).unwrap_or(0),
                res.get("entries").and_then(Value::as_u64).unwrap_or(0),
                types.join(", "),
            );
        } else {
            println!("  resources: none");
        }
    }
}
