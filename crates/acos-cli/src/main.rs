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

fn new_request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("req_{:x}_{:x}", std::process::id(), nanos)
}

fn print_json(envelope: &Envelope) {
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
