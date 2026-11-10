//! `acos doctor` — preflight the host for the build/run/observe loop.
//!
//! An agent that only has the ACOS tools should be able to answer "why did the
//! build or launch fail before the app even ran?" without guessing. `doctor`
//! checks the pieces the fixture/build path depends on (Android SDK tools,
//! `android.jar`, JDK, the APK-append helper, the debug keystore, the boot
//! classpath cache, workspace layout and host memory) and reports each with a
//! status and, on failure, a concrete fix.
//!
//! Nothing here mutates the host: it only reads paths and runs `java -version`.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use serde::Serialize;
use serde_json::json;

/// Arguments for `acos doctor`.
#[derive(Debug, clap::Args)]
pub struct DoctorArgs {
    /// Emit the report as JSON instead of a text checklist.
    #[arg(long)]
    pub json: bool,

    /// Override the Android SDK root (default `$ANDROID_HOME`, then
    /// `$ANDROID_SDK_ROOT`, then `$HOME/Android/Sdk`).
    #[arg(long)]
    pub sdk: Option<PathBuf>,

    /// Build-tools version under the SDK (default `$BUILD_TOOLS_VERSION` or
    /// `35.0.0`).
    #[arg(long, default_value = "35.0.0")]
    pub build_tools: String,
}

/// One preflight check.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// Stable check id (`android_sdk`, `d8`, ...).
    pub name: String,
    /// `ok`, `warn` or `fail`.
    pub status: &'static str,
    /// What was found (or not).
    pub detail: String,
    /// A concrete next action when the check is not `ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Check {
            name: name.to_string(),
            status: "ok",
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Check {
            name: name.to_string(),
            status: "warn",
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Check {
            name: name.to_string(),
            status: "fail",
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// The whole preflight report.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    /// False when at least one required check is `fail`.
    pub ok: bool,
    /// Host/workspace summary.
    pub subject: serde_json::Value,
    /// Every check, in stable order.
    pub checks: Vec<Check>,
}

impl DoctorReport {
    /// Whether any check failed.
    #[must_use]
    pub fn all_ok(&self) -> bool {
        self.checks.iter().all(|c| c.status != "fail")
    }

    /// The names of checks that failed.
    #[must_use]
    pub fn failures(&self) -> Vec<&str> {
        self.checks
            .iter()
            .filter(|c| c.status == "fail")
            .map(|c| c.name.as_str())
            .collect()
    }

    /// Human-readable checklist.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for check in &self.checks {
            let mark = match check.status {
                "ok" => "ok  ",
                "warn" => "warn",
                _ => "FAIL",
            };
            out.push_str(&format!("[{mark}] {:<18} {}\n", check.name, check.detail));
            if let Some(fix) = &check.fix {
                out.push_str(&format!("       fix: {fix}\n"));
            }
        }
        out.push_str(&format!(
            "\nacos doctor: {} ({} failure(s))\n",
            if self.ok { "healthy" } else { "unhealthy" },
            self.failures().len()
        ));
        out
    }
}

/// Locate the workspace root by walking up from the cwd for `tools/apk_append.py`.
fn find_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("tools").join("apk_append.py").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|p| p.join(tool))
        .find(|p| p.is_file())
}

fn sdk_root(args: &DoctorArgs) -> Option<PathBuf> {
    if let Some(explicit) = &args.sdk {
        return Some(explicit.clone());
    }
    std::env::var_os("ANDROID_HOME")
        .or_else(|| std::env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Android/Sdk")))
}

fn java_version(tool: &str) -> Option<String> {
    // `keytool` has no `-version`; presence is the check, so report the path.
    if tool == "keytool" {
        return which(tool).map(|p| p.display().to_string());
    }
    // `java -version` writes to stderr, `javac -version` to stdout.
    let out = Command::new(tool).arg("-version").output().ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    Some(line.trim().to_string())
}

/// Parse `MemAvailable`/`MemTotal` from `/proc/meminfo` into `(used_percent,
/// total_mib, available_mib)`.
#[must_use]
pub fn mem_info(meminfo: &str) -> Option<(u64, u64, u64)> {
    let mut total = None;
    let mut avail = None;
    for line in meminfo.lines() {
        let (key, rest) = line.split_once(':')?;
        let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
        match key {
            "MemTotal" => total = Some(kb),
            "MemAvailable" => avail = Some(kb),
            _ => {}
        }
    }
    let total = total?;
    let avail = avail?;
    let used_percent = if total == 0 {
        0
    } else {
        (total - avail) * 100 / total
    };
    Some((used_percent, total / 1024, avail / 1024))
}

fn check_sdk(args: &DoctorArgs) -> Check {
    match sdk_root(args) {
        Some(root) if root.is_dir() => Check::ok("android_sdk", format!("{}", root.display())),
        Some(root) => Check::fail(
            "android_sdk",
            format!("{} is not a directory", root.display()),
            "Set ANDROID_HOME or pass --sdk <path> to an installed Android SDK (sdkmanager).",
        ),
        None => Check::fail(
            "android_sdk",
            "ANDROID_HOME/ANDROID_SDK_ROOT are unset",
            "Install the Android command-line tools, then export ANDROID_HOME=/path/to/Sdk.",
        ),
    }
}

fn check_build_tools(args: &DoctorArgs) -> Vec<Check> {
    let Some(root) = sdk_root(args) else {
        return vec![Check::fail(
            "build_tools",
            "no SDK root",
            "Fix the android_sdk check first.",
        )];
    };
    let bt = root.join("build-tools").join(&args.build_tools);
    let mut out = Vec::new();
    if !bt.is_dir() {
        out.push(Check::fail(
            "build_tools",
            format!("{} not found", bt.display()),
            format!(
                "sdkmanager \"build-tools;{}\" (or pass --build-tools <version>).",
                args.build_tools
            ),
        ));
        return out;
    }
    out.push(Check::ok("build_tools", format!("{}", bt.display())));
    for tool in ["aapt2", "d8", "zipalign", "apksigner"] {
        let path = bt.join(tool);
        if path.is_file() {
            out.push(Check::ok(tool, format!("{}", path.display())));
        } else {
            out.push(Check::fail(
                tool,
                format!("missing {}", path.display()),
                format!(
                    "Reinstall build-tools;{} (the file is part of that package).",
                    args.build_tools
                ),
            ));
        }
    }
    out
}

fn check_android_jar(args: &DoctorArgs) -> Check {
    let Some(root) = sdk_root(args) else {
        return Check::fail(
            "android_jar",
            "no SDK root",
            "Fix the android_sdk check first.",
        );
    };
    for platform in ["android-34", "android-35"] {
        let jar = root.join("platforms").join(platform).join("android.jar");
        if jar.is_file() {
            return Check::ok("android_jar", format!("{}", jar.display()));
        }
    }
    Check::fail(
        "android_jar",
        format!("no android.jar under {}", root.join("platforms").display()),
        "sdkmanager \"platforms;android-34\" — the build scripts compile against android-34.",
    )
}

fn check_java() -> Vec<Check> {
    let mut out = Vec::new();
    for tool in ["java", "javac", "keytool"] {
        match which(tool) {
            Some(path) => {
                let version = java_version(tool).unwrap_or_else(|| path.display().to_string());
                out.push(Check::ok(tool, version));
            }
            None => out.push(Check::fail(
                tool,
                format!("`{tool}` not on PATH"),
                "Install a JDK 11+ (e.g. `apt install openjdk-17-jdk`) and put its bin/ on PATH.",
            )),
        }
    }
    out
}

fn check_workspace() -> Vec<Check> {
    match find_root() {
        None => vec![Check::fail(
            "workspace",
            "could not find a workspace root (no tools/apk_append.py walking up)",
            "Run `acos doctor` from inside the ACOS checkout.",
        )],
        Some(root) => {
            let mut out = vec![Check::ok("workspace", format!("{}", root.display()))];
            let helper = root.join("tools").join("apk_append.py");
            if helper.is_file() {
                out.push(Check::ok("apk_append", format!("{}", helper.display())));
            } else {
                out.push(Check::fail(
                    "apk_append",
                    format!("missing {}", helper.display()),
                    "Restore tools/apk_append.py from the repo; the build cannot attach classes.dex without it.",
                ));
            }
            let keystore = root
                .join("fixtures")
                .join("build")
                .join("apps")
                .join("debug.keystore");
            if keystore.is_file() {
                out.push(Check::ok(
                    "debug_keystore",
                    format!("{}", keystore.display()),
                ));
            } else {
                out.push(Check::warn(
                    "debug_keystore",
                    format!("{} not generated yet", keystore.display()),
                    "Run tools/build-dogfood-apps.sh; it generates a throwaway debug keystore on first build.",
                ));
            }
            let boot = root.join(".cache").join("boot").join("android-34");
            if boot.is_dir() {
                out.push(Check::ok("boot_classpath", format!("{}", boot.display())));
            } else {
                out.push(Check::warn(
                    "boot_classpath",
                    format!("{} not populated yet", boot.display()),
                    "Run `acos run-dex --corelib real ...` once; the real boot classpath is cached on demand.",
                ));
            }
            out
        }
    }
}

fn check_memory() -> Check {
    let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else {
        return Check::warn(
            "host_memory",
            "/proc/meminfo unreadable (non-Linux host?)",
            "Memory headroom is not checked on this host.",
        );
    };
    let Some((used_percent, total_mib, avail_mib)) = mem_info(&meminfo) else {
        return Check::warn(
            "host_memory",
            "could not parse /proc/meminfo",
            "Memory headroom is not checked on this host.",
        );
    };
    let detail = format!("{used_percent}% used ({avail_mib} MiB available of {total_mib} MiB)");
    if used_percent >= 90 {
        Check::fail(
            "host_memory",
            detail,
            "Free memory below the 90% used ceiling; stop other builds/VMs before a launch.",
        )
    } else if used_percent >= 80 {
        Check::warn(
            "host_memory",
            detail,
            "Headroom is getting tight; a full launch needs ~2-4 GiB.",
        )
    } else {
        Check::ok("host_memory", detail)
    }
}

/// Run all checks and build the report.
#[must_use]
pub fn diagnose(args: &DoctorArgs) -> DoctorReport {
    let mut checks = vec![check_sdk(args)];
    checks.extend(check_build_tools(args));
    checks.push(check_android_jar(args));
    checks.extend(check_java());
    checks.extend(check_workspace());
    checks.push(check_memory());
    let subject = json!({
        "acos_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cwd": std::env::current_dir().ok().map(|p| p.display().to_string()),
    });
    let mut report = DoctorReport {
        ok: false,
        subject,
        checks,
    };
    report.ok = report.all_ok();
    report
}

/// Run `acos doctor`.
pub fn run(args: DoctorArgs) -> ExitCode {
    let report = diagnose(&args);
    if args.json {
        crate::print_json(&report);
    } else {
        print!("{}", report.to_text());
    }
    if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_info_computes_used_percent() {
        let meminfo = "MemTotal:       1000000 kB\nMemFree: 1 kB\nMemAvailable:  250000 kB\n";
        let (used, total, avail) = mem_info(meminfo).expect("parse");
        assert_eq!(used, 75);
        assert_eq!(total, 976);
        assert_eq!(avail, 244);
        assert_eq!(mem_info("garbage"), None);
    }

    #[test]
    fn report_ok_only_when_no_failures() {
        let mut report = DoctorReport {
            ok: true,
            subject: json!({}),
            checks: vec![
                Check::ok("a", "fine"),
                Check::warn("b", "nearly", "watch it"),
            ],
        };
        report.ok = report.all_ok();
        assert!(report.ok);
        assert!(report.failures().is_empty());
        report.checks.push(Check::fail("c", "broken", "fix it"));
        report.ok = report.all_ok();
        assert!(!report.ok);
        assert_eq!(report.failures(), vec!["c"]);
        assert!(report.to_text().contains("fix: fix it"));
    }
}
