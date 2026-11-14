//! `acos artifacts gc` — prune run directories under an artifacts root (ARTGC).
//!
//! Every `acos verify` writes a run directory (`out/<scenario>/`). Retention
//! (see `acos_verify::retention`) bounds one run; this command bounds the
//! *collection* of runs. It has two hard rules, both checked before anything is
//! removed:
//!
//! * a **non-pass** run younger than `--older-than` is never touched — it is
//!   the diagnosis an agent is about to read;
//! * the newest `--keep-last` runs of every scenario are never touched.
//!
//! A run directory it cannot classify (unreadable `verdict.json`) is never
//! pruned: an unknown run is treated as a failure. `--dry-run` prints exactly
//! what would be freed and removes nothing.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use acos_protocol::artifacts::VERDICT_JSON;
use acos_protocol::Envelope;
use serde_json::json;

/// Arguments for `acos artifacts gc`.
#[derive(Debug, clap::Args)]
pub struct ArtifactsGcArgs {
    /// Artifacts root to prune (default: `$ACOS_ARTIFACTS_DIR`, else `out`).
    #[arg(long)]
    pub dir: Option<PathBuf>,

    /// Only prune run directories older than this (`30s`, `15m`, `24h`, `7d`).
    ///
    /// A non-pass run younger than this is never touched. Default `24h`.
    #[arg(long = "older-than", value_name = "DUR")]
    pub older_than: Option<String>,

    /// Never prune the newest N run directories of a scenario (default 1).
    #[arg(long = "keep-last", value_name = "N")]
    pub keep_last: Option<usize>,

    /// Print what would be pruned and freed without removing anything.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Emit the standard ACOS JSON envelope.
    #[arg(long)]
    pub json: bool,
}

/// One run directory considered by the collector.
#[derive(Debug, Clone)]
struct RunDir {
    path: PathBuf,
    scenario: String,
    status: String,
    age: Duration,
    bytes: u64,
    files: u64,
    classifiable: bool,
}

/// Run `acos artifacts gc`.
pub fn run(args: ArtifactsGcArgs) -> ExitCode {
    let root = args
        .dir
        .clone()
        .or_else(|| std::env::var_os("ACOS_ARTIFACTS_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("out"));
    let older_than = match args.older_than.as_deref() {
        None => Duration::from_secs(24 * 60 * 60),
        Some(value) => match parse_duration(value) {
            Some(parsed) => parsed,
            None => {
                eprintln!(
                    "error: --older-than {value:?} is not a duration (use 30s, 15m, 24h, 7d)"
                );
                return ExitCode::FAILURE;
            }
        },
    };
    let keep_last = args.keep_last.unwrap_or(1);
    match collect(&root, older_than, keep_last, args.dry_run) {
        Ok(report) => {
            if args.json {
                let envelope = Envelope::pass(
                    "req_artifacts_gc",
                    acos_protocol::Backend::HostTooling,
                    acos_protocol::Fidelity::StaticAnalysis,
                    acos_protocol::Metrics::wall_ms(0),
                )
                .with_result(&report)
                .unwrap_or_else(|_| {
                    Envelope::pass(
                        "req_artifacts_gc",
                        acos_protocol::Backend::HostTooling,
                        acos_protocol::Fidelity::StaticAnalysis,
                        acos_protocol::Metrics::wall_ms(0),
                    )
                });
                println!("{}", serde_json::to_string_pretty(&envelope).unwrap_or_default());
            } else {
                print_text(&report);
            }
            ExitCode::SUCCESS
        }
        Err(detail) => {
            eprintln!("error: {detail}");
            ExitCode::FAILURE
        }
    }
}

fn print_text(report: &serde_json::Value) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let runs = report["runs"].as_array().cloned().unwrap_or_default();
    for run in &runs {
        let action = run["action"].as_str().unwrap_or("");
        if action == "keep" {
            continue;
        }
        let _ = writeln!(
            out,
            "{}  {:>10}  {}  {}  {}",
            action,
            format_bytes(run["bytes"].as_u64().unwrap_or(0)),
            run["status"].as_str().unwrap_or(""),
            run["scenario"].as_str().unwrap_or(""),
            run["path"].as_str().unwrap_or("")
        );
    }
    let _ = writeln!(
        out,
        "artifacts gc{}: {} run(s) pruned, {} run(s) kept, {} freed ({} file(s)); root {}",
        if report["dry_run"].as_bool().unwrap_or(false) {
            " --dry-run"
        } else {
            ""
        },
        report["pruned_runs"].as_u64().unwrap_or(0),
        report["kept_runs"].as_u64().unwrap_or(0),
        format_bytes(report["freed_bytes"].as_u64().unwrap_or(0)),
        report["pruned_files"].as_u64().unwrap_or(0),
        report["root"].as_str().unwrap_or("")
    );
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Walk `root`, classify every run directory, and prune what policy allows.
///
/// The return value is the machine-readable report (also printed as JSON).
fn collect(
    root: &Path,
    older_than: Duration,
    keep_last: usize,
    dry_run: bool,
) -> Result<serde_json::Value, String> {
    if !root.is_dir() {
        return Err(format!("artifacts root {} is not a directory", root.display()));
    }
    let mut runs = scan_runs(root)?;

    // Order per scenario: newest first. The first `keep_last` are protected.
    runs.sort_by(|a, b| {
        a.scenario
            .cmp(&b.scenario)
            .then(a.age.cmp(&b.age))
            .then(a.path.cmp(&b.path))
    });

    let mut seen_per_scenario: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut entries = Vec::new();
    let mut freed_bytes = 0_u64;
    let mut pruned_files = 0_u64;
    let mut pruned_runs = 0_u64;
    let mut kept_runs = 0_u64;

    for run in &runs {
        let position = seen_per_scenario.entry(run.scenario.clone()).or_insert(0);
        let rank = *position;
        *position += 1;
        let reason = if !run.classifiable {
            Some("unreadable-verdict")
        } else if rank < keep_last {
            Some("keep-last")
        } else if run.status != "pass" && run.age < older_than {
            Some("recent-failure")
        } else if run.age < older_than {
            Some("younger-than-older-than")
        } else {
            None
        };
        let (action, note): (&str, Option<String>) = match reason {
            Some(reason) => ("keep", Some(reason.to_string())),
            None if dry_run => ("would-prune", None),
            None => match std::fs::remove_dir_all(&run.path) {
                Ok(()) => ("pruned", None),
                Err(error) => ("keep", Some(format!("remove-failed: {error}"))),
            },
        };
        match action {
            "keep" => kept_runs += 1,
            _ => {
                pruned_runs += 1;
                freed_bytes += run.bytes;
                pruned_files += run.files;
            }
        }
        entries.push(json!({
            "path": relative_or_full(root, &run.path),
            "scenario": run.scenario,
            "status": run.status,
            "age_seconds": run.age.as_secs(),
            "bytes": run.bytes,
            "files": run.files,
            "action": action,
            "reason": note,
        }));
    }
    entries.reverse();

    Ok(json!({
        "root": root.display().to_string(),
        "older_than_seconds": older_than.as_secs(),
        "keep_last": keep_last,
        "dry_run": dry_run,
        "runs": entries,
        "pruned_runs": pruned_runs,
        "kept_runs": kept_runs,
        "pruned_files": pruned_files,
        "freed_bytes": freed_bytes,
        "freed_human": format_bytes(freed_bytes),
    }))
}

/// Decide the action for one run, keeping the "never prune an unknown or a
/// recent failure" rules in one place.
fn scan_runs(root: &Path) -> Result<Vec<RunDir>, String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .map_err(|error| format!("cannot read {}: {error}", current.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if !kind.is_dir() || kind.is_symlink() {
                continue;
            }
            if path.join(VERDICT_JSON).is_file() {
                found.push(describe_run(&path)?);
            } else {
                stack.push(path);
            }
        }
    }
    Ok(found)
}

fn describe_run(path: &Path) -> Result<RunDir, String> {
    let (files, bytes) = usage(path);
    let verdict_path = path.join(VERDICT_JSON);
    let doc: Option<serde_json::Value> = std::fs::read_to_string(&verdict_path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok());
    let status = doc
        .as_ref()
        .and_then(|doc| doc.get("verdict"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    let scenario = doc
        .as_ref()
        .and_then(|doc| doc.pointer("/scenario/name"))
        .and_then(|value| value.as_str())
        .map(|name| name.to_string())
        .or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| path.display().to_string());
    let modified = newest_mtime(&verdict_path).or_else(|| newest_mtime(path));
    let age = modified
        .and_then(|time| SystemTime::now().duration_since(time).ok())
        .unwrap_or_default();
    Ok(RunDir {
        path: path.to_path_buf(),
        scenario,
        classifiable: doc.is_some() && status != "unknown",
        status,
        age,
        bytes,
        files,
    })
}

fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.is_dir() {
        let mut newest = meta.modified().ok();
        for entry in std::fs::read_dir(path).ok()?.flatten() {
            if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
                newest = Some(match newest {
                    Some(current) if current > modified => current,
                    _ => modified,
                });
            }
        }
        newest
    } else {
        meta.modified().ok()
    }
}

fn usage(dir: &Path) -> (u64, u64) {
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                files += 1;
                bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            }
        }
    }
    (files, bytes)
}

fn relative_or_full(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// Parse `30s`, `15m`, `24h`, `7d`; a bare number is seconds.
pub fn parse_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let (number, scale) = match value.char_indices().last() {
        Some((index, last)) => match last {
            's' | 'S' => (&value[..index], 1),
            'm' | 'M' => (&value[..index], 60),
            'h' | 'H' => (&value[..index], 60 * 60),
            'd' | 'D' => (&value[..index], 24 * 60 * 60),
            '0'..='9' => (value, 1),
            _ => return None,
        },
        None => return None,
    };
    let amount: f64 = number.trim().parse().ok()?;
    if !amount.is_finite() || amount < 0.0 {
        return None;
    }
    Some(Duration::from_secs((amount * scale as f64) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("acos-gc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_run(root: &Path, relative: &str, status: &str, age_secs: u64, bytes: usize) -> PathBuf {
        make_run_named(root, relative, relative, status, age_secs, bytes)
    }

    fn make_run_named(
        root: &Path,
        relative: &str,
        scenario: &str,
        status: &str,
        age_secs: u64,
        bytes: usize,
    ) -> PathBuf {
        let dir = root.join(relative);
        std::fs::create_dir_all(dir.join("frames")).unwrap();
        let verdict = json!({
            "verdict": status,
            "scenario": {"name": scenario},
        });
        std::fs::write(
            dir.join(VERDICT_JSON),
            serde_json::to_string_pretty(&verdict).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("frames/1.png"), vec![7_u8; bytes]).unwrap();
        set_age(&dir.join(VERDICT_JSON), age_secs);
        dir
    }

    fn set_age(path: &Path, age_secs: u64) {
        let when = SystemTime::now() - Duration::from_secs(age_secs);
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("15m"), Some(Duration::from_secs(900)));
        assert_eq!(parse_duration("24h"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_duration("7d"), Some(Duration::from_secs(604_800)));
        assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("nope"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn keep_last_and_recent_failures_are_protected() {
        let root = temp_root("keep-last");
        let h = 60 * 60;
        let newest = make_run_named(&root, "s-new", "s", "pass", h, 100);
        let middle = make_run_named(&root, "s-old", "s", "pass", 30 * h, 200);
        let old_fail = make_run_named(&root, "neg-old", "neg", "fail", 30 * h, 300);
        let recent_fail = make_run_named(&root, "neg-new", "neg", "fail", 60, 400);

        let report = collect(&root, Duration::from_secs(24 * h), 1, false).unwrap();
        assert_eq!(report["dry_run"], false);
        // Newest of each scenario survives.
        assert!(newest.is_dir(), "keep-last keeps the newest run of a scenario");
        assert!(recent_fail.is_dir(), "a recent failure is never pruned");
        // The older one of each scenario goes.
        assert!(!middle.is_dir(), "an old passing run is pruned");
        assert!(!old_fail.is_dir(), "an old failure is pruned");
        assert!(report["pruned_runs"].as_u64().unwrap() >= 2);
        // The two pruned runs carry the 200 B and 300 B frames plus their small
        // verdict files; nothing from the kept runs is counted.
        assert!(
            report["freed_bytes"].as_u64().unwrap() >= 200 + 300,
            "freed bytes must cover both pruned runs: {report}"
        );
        assert_eq!(report["pruned_runs"].as_u64().unwrap(), 2);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dry_run_frees_nothing_but_reports_the_same_bytes() {
        let root = temp_root("dry-run");
        let h = 60 * 60;
        make_run_named(&root, "s-new", "s", "pass", h, 100);
        let old = make_run_named(&root, "s-old", "s", "pass", 48 * h, 500);
        let dry = collect(&root, Duration::from_secs(24 * h), 1, true).unwrap();
        assert_eq!(dry["dry_run"], true);
        assert!(dry["freed_bytes"].as_u64().unwrap() >= 500);
        assert!(old.is_dir(), "dry-run removes nothing");
        assert!(dry["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["action"] == "would-prune"));
        let real = collect(&root, Duration::from_secs(24 * h), 1, false).unwrap();
        assert_eq!(
            real["freed_bytes"], dry["freed_bytes"],
            "dry-run reports the same bytes it would free"
        );
        assert_eq!(real["runs"].as_array().unwrap().len(), 2);
        assert!(!old.is_dir());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unreadable_verdict_is_never_pruned() {
        let root = temp_root("unreadable");
        let h = 60 * 60;
        let dir = root.join("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(VERDICT_JSON), b"{not json").unwrap();
        set_age(&dir.join(VERDICT_JSON), 100 * h);
        let report = collect(&root, Duration::from_secs(h), 0, false).unwrap();
        assert!(dir.is_dir(), "an unclassifiable run is never pruned");
        let entry = report["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["path"] == "broken")
            .unwrap();
        assert_eq!(entry["reason"], "unreadable-verdict");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn nested_scenario_directories_are_found() {
        let root = temp_root("nested");
        let h = 60 * 60;
        make_run_named(&root, "suite/good-new", "T03-render-good", "pass", h, 10);
        let old = make_run_named(
            &root,
            "suite/good-old",
            "T03-render-good",
            "pass",
            96 * h,
            20,
        );
        let report = collect(&root, Duration::from_secs(24 * h), 1, false).unwrap();
        assert_eq!(report["runs"].as_array().unwrap().len(), 2);
        assert!(!old.is_dir());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
