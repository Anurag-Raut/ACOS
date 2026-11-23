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

use std::path::{Path, PathBuf};
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

/// Locate the workspace root by walking up from the cwd for `tools/build/apk_append.py`.
fn find_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("tools").join("build").join("apk_append.py").is_file() {
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

fn check_build_tools(args: &DoctorArgs, installed: bool) -> Vec<Check> {
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
    // `aapt2` and `d8` are executed by `acos-art` on every launch (APK manifest +
    // resources, and the guest DEX compile), so they are runtime requirements and
    // a missing one refuses the launch. `zipalign`/`apksigner` only sign and
    // align fixture APKs that the build scripts produce, so an installed ACOS
    // does not need them.
    for (tool, runtime) in [
        ("aapt2", true),
        ("d8", true),
        ("zipalign", false),
        ("apksigner", false),
    ] {
        let path = bt.join(tool);
        if path.is_file() {
            out.push(Check::ok(tool, format!("{}", path.display())));
        } else if runtime {
            out.push(Check::fail(
                tool,
                format!("missing {} (needed at run time)", path.display()),
                format!(
                    "Reinstall build-tools;{} (the file is part of that package).",
                    args.build_tools
                ),
            ));
        } else if installed {
            out.push(Check::warn(
                tool,
                format!("missing {} (build-fixtures only)", path.display()),
                format!(
                    "Not needed to run an APK; install build-tools;{} only to build fixtures.",
                    args.build_tools
                ),
            ));
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
    // `javac` compiles the guest Java sources on every (cold) launch.
    for (tool, runtime) in [("java", true), ("javac", true), ("keytool", false)] {
        match which(tool) {
            Some(path) => {
                let version = java_version(tool).unwrap_or_else(|| path.display().to_string());
                out.push(Check::ok(tool, version));
            }
            None => out.push(Check::fail(
                tool,
                format!(
                    "`{tool}` not on PATH{}",
                    if runtime {
                        " (needed at run time)"
                    } else {
                        " (fixtures only)"
                    }
                ),
                "Install a JDK 11+ (e.g. `apt install openjdk-17-jdk`) and put its bin/ on PATH.",
            )),
        }
    }
    out
}

fn check_workspace(installed: bool) -> Vec<Check> {
    match find_root() {
        None if installed => vec![
            Check::ok(
                "workspace",
                "installed layout (no checkout: runtime assets come from ACOS_HOME)",
            ),
            Check::warn(
                "apk_append",
                "not installed (fixture builds only)",
                "Nothing to do to run APKs; the fixture build scripts live in the ACOS checkout.",
            ),
        ],
        None => vec![Check::fail(
            "workspace",
            "could not find a workspace root (no tools/build/apk_append.py walking up)",
            "Run `acos doctor` from inside the ACOS checkout, or set ACOS_HOME to an installed ACOS.",
        )],
        Some(root) => {
            let mut out = vec![Check::ok("workspace", format!("{}", root.display()))];
            let helper = root.join("tools").join("build").join("apk_append.py");
            if helper.is_file() {
                out.push(Check::ok("apk_append", format!("{}", helper.display())));
            } else {
                out.push(Check::fail(
                    "apk_append",
                    format!("missing {}", helper.display()),
                    "Restore tools/build/apk_append.py from the repo; the build cannot attach classes.dex without it.",
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
                    "Run tools/build/build-dogfood-apps.sh; it generates a throwaway debug keystore on first build.",
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

/// REL1a: the installed layout. Only run when `ACOS_HOME` is set: every runtime
/// asset must live under that root (the checkout is not consulted, and nothing
/// is written into it).
fn check_install(home: &Path) -> Vec<Check> {
    let mut out = Vec::new();
    if home.is_dir() {
        out.push(Check::ok("acos_home", format!("{}", home.display())));
    } else {
        return vec![Check::fail(
            "acos_home",
            format!("ACOS_HOME={} is not a directory", home.display()),
            "Point ACOS_HOME at an install root (tools/release/install.sh prints it), or unset it to use a checkout.",
        )];
    }
    match std::fs::read_to_string(home.join("VERSION")) {
        Ok(version) => out.push(Check::ok("install_version", version.trim().to_string())),
        Err(_) => out.push(Check::warn(
            "install_version",
            format!("no {}/VERSION marker", home.display()),
            "Reinstall with tools/release/install.sh so the root is complete and labelled.",
        )),
    }
    // The ART runtime: the sysroot is the framework, and without it nothing runs.
    let sysroot = home.join(".cache/art/sysroot");
    if sysroot.join("system/apex").is_dir() {
        out.push(Check::ok(
            "install_sysroot",
            format!("{}", sysroot.display()),
        ));
    } else {
        out.push(Check::fail(
            "install_sysroot",
            format!("{} has no system/apex", sysroot.display()),
            "The install is incomplete: reinstall with tools/release/install.sh (it copies $ACOS_HOME/.cache/art/sysroot).",
        ));
    }
    // Boot assets: fonts decide whether text renders at all, the overlays decide
    // the framework resources, both ship in the install.
    let boot = home.join(".cache/boot/android-34");
    if boot.join("fonts").is_dir() {
        out.push(Check::ok(
            "install_fonts",
            format!("{}", boot.join("fonts").display()),
        ));
    } else {
        out.push(Check::fail(
            "install_fonts",
            format!("{} is missing", boot.join("fonts").display()),
            "Text layout needs the boot-cache fonts; reinstall with tools/release/install.sh.",
        ));
    }
    if boot.join("overlays").is_dir() {
        out.push(Check::ok(
            "install_overlays",
            format!("{}", boot.join("overlays").display()),
        ));
    } else {
        out.push(Check::warn(
            "install_overlays",
            format!("{} is missing", boot.join("overlays").display()),
            "The app still renders, un-overlaid (INCREMENT instead of the device's label); reinstall to restore them.",
        ));
    }
    if boot.join("BOOTCLASSPATH").is_file() {
        out.push(Check::ok(
            "install_bootclasspath",
            format!("{}", boot.join("BOOTCLASSPATH").display()),
        ));
    } else {
        out.push(Check::warn(
            "install_bootclasspath",
            format!("{} is missing", boot.join("BOOTCLASSPATH").display()),
            "Only legacy sysroots need the jar-name list; reinstall to be safe.",
        ));
    }
    // The guest Java sources are compiled by `javac`+`d8` on a cold launch.
    let guest = home.join("crates/acos-art/guest/java/dev/acos/art/AppMain.java");
    if guest.is_file() {
        out.push(Check::ok(
            "install_guest_sources",
            format!("{}", guest.display()),
        ));
    } else {
        out.push(Check::fail(
            "install_guest_sources",
            format!("{} is missing", guest.display()),
            "Every launch compiles the guest from these sources; reinstall with tools/release/install.sh.",
        ));
    }
    // `acos` spawns `acos-art` from its own directory.
    let art = home.join("bin/acos-art");
    if art.is_file() {
        out.push(Check::ok("install_acos_art", format!("{}", art.display())));
    } else {
        out.push(Check::fail(
            "install_acos_art",
            format!("{} is missing", art.display()),
            "Reinstall with tools/release/install.sh, or set ACOS_ART_BIN.",
        ));
    }
    // ADR-0031: `webview: real` needs both the sibling `acos-webview` broker
    // binary (`acos-art` resolves it next to `current_exe()`) and the staged
    // `com.android.webview` provider APK. Optional (`webview` defaults to `off`),
    // so a missing piece is a `warn`, not a `fail`.
    out.push(check_install_webview(home));
    // Mutable state: created on demand, but it must be writable, never the repo.
    let state = home.join(".cache");
    match std::fs::create_dir_all(state.join("art/data")) {
        Ok(()) => out.push(Check::ok(
            "install_state",
            format!(
                "{} (data, sessions, sockets, build caches)",
                state.display()
            ),
        )),
        Err(error) => out.push(Check::fail(
            "install_state",
            format!("cannot write {}: {error}", state.join("art/data").display()),
            "The install root must be writable by the user that runs ACOS.",
        )),
    }
    out
}

fn check_install_webview(home: &Path) -> Check {
    let broker = home.join("bin/acos-webview");
    let provider = home.join(acos_services::modules::webview::PROVIDER_APK_IMAGE_REL);
    match (broker.is_file(), provider.is_file()) {
        (true, true) => Check::ok(
            "install_webview",
            format!("broker {}, provider {}", broker.display(), provider.display()),
        ),
        (false, true) => Check::warn(
            "install_webview",
            format!(
                "missing broker {} (provider {} present)",
                broker.display(),
                provider.display()
            ),
            "The `webview: real` module cannot start its shared broker without `acos-webview`. \
             Reinstall with tools/release/install.sh, or set ACOS_WEBVIEW_BIN.",
        ),
        (true, false) => Check::warn(
            "install_webview",
            format!(
                "missing provider {} (broker {} present)",
                provider.display(),
                broker.display()
            ),
            "The `webview: real` module needs com.android.webview.apk. Reinstall with \
             tools/release/install.sh, or set ACOS_WEBVIEW_PROVIDER_APK.",
        ),
        (false, false) => Check::warn(
            "install_webview",
            format!(
                "missing broker {} and provider {}",
                broker.display(),
                provider.display()
            ),
            "The `webview: real` module needs both `acos-webview` and com.android.webview.apk. \
             Reinstall with tools/release/install.sh.",
        ),
    }
}

/// The host helpers the sandbox and the cgroup limit need: `bwrap` for the
/// namespace, `systemd-run --user --scope` for the memory cgroup. Both are
/// required at run time, in a checkout and in an install.
fn check_sandbox_helpers() -> Vec<Check> {
    let mut out = Vec::new();
    for tool in ["bwrap", "systemd-run"] {
        match which(tool) {
            Some(path) => out.push(Check::ok(tool, format!("{}", path.display()))),
            None => out.push(Check::fail(
                tool,
                format!("`{tool}` not on PATH (needed at run time)"),
                if tool == "bwrap" {
                    "Install bubblewrap (`apt install bubblewrap`); the guest runs inside it."
                } else {
                    "Install systemd (`apt install systemd`); it provides the per-run memory cgroup."
                },
            )),
        }
    }
    out
}

/// The NDK clang `acos-art` compiles its JNI shims with at run time.
fn check_ndk(args: &DoctorArgs) -> Check {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(p) =
        std::env::var_os("ANDROID_NDK_HOME").or_else(|| std::env::var_os("ANDROID_NDK_ROOT"))
    {
        roots.push(PathBuf::from(p));
    }
    if let Some(sdk) = sdk_root(args) {
        if let Ok(entries) = std::fs::read_dir(sdk.join("ndk")) {
            let mut found: Vec<PathBuf> =
                entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            found.sort();
            found.reverse();
            roots.extend(found);
        }
    }
    for root in roots {
        let bin = root.join("toolchains/llvm/prebuilt/linux-x86_64/bin");
        if bin.join("x86_64-linux-android34-clang").is_file() {
            return Check::ok("ndk_clang", format!("{}", bin.display()));
        }
    }
    Check::fail(
        "ndk_clang",
        "no x86_64-linux-android34-clang (needed at run time)",
        "sdkmanager \"ndk;27.0.12077973\", or set ANDROID_NDK_HOME; the guest's JNI shims are built with it.",
    )
}

/// ADR-0030's transport: the pinned static `pasta`.
///
/// One rule owns this lookup (`acos_netd_ctl::pasta`): the relay supervisor
/// starts its NAT process from it, `tools/release/install.sh` installs the
/// binary at `<root>/libexec/pasta`, and a scenario that declares
/// `needs_env: [pasta]` is resolved with the same answer (`acos-netd pasta`).
///
/// It is **optional** at run time — the default network profile is `off` — so a
/// missing binary is a `warn`, not an unhealthy install. What the detail must
/// carry is the honest fact: the version the binary reports, or `missing`, plus
/// everywhere the lookup went.
fn pasta_check(
    found: Option<acos_netd::pasta::Resolution>,
    version: Option<String>,
    searched: &str,
) -> Check {
    use acos_netd::pasta::{is_pinned, TAG};
    let make_fix = || {
        format!(
            "Build the pinned tag and install it where the relay looks:\n \
             git clone --depth 1 -b {TAG} https://passt.top/passt && \
             make -C passt static VERSION={TAG} && \
             install -m 755 passt/pasta <root>/libexec/pasta\n \
             (`tools/release/install.sh` does this when it finds a pinned pasta; \
             the sha256 is recorded in research/spike-net/artifacts/passt-build.txt)"
        )
    };
    match found {
        Some(resolution) => {
            let where_ = format!(
                "{} [{}]",
                resolution.path.display(),
                resolution.source.as_str()
            );
            match version {
                Some(version) if is_pinned(Some(&version)) => {
                    Check::ok("pasta", format!("{version} ({where_})"))
                }
                Some(version) => Check::warn(
                    "pasta",
                    format!("{version} ({where_}) is not the pinned tag {TAG}"),
                    format!(
                        "The relay reports both tags in `status`; reinstall the pinned build \
                         (fixed tag, measured transport). {}",
                        make_fix()
                    ),
                ),
                None => Check::warn(
                    "pasta",
                    format!("{} exists but does not run --version", resolution.path.display()),
                    make_fix(),
                ),
            }
        }
        None => Check::warn(
            "pasta",
            format!("missing (searched: {searched})"),
            format!(
                "The `network:real` module and its `needs_env: [pasta]` scenarios \
                 cannot run without it. {}",
                make_fix()
            ),
        ),
    }
}

/// METRO3: what a `needs_env: [metro]` scenario needs — `node`, and a
/// `node_modules` holding Metro + React Native. The rule lives in
/// `acos_verify::dev_server::metro_environment`, the same one `dev_server start`
/// uses, so a skipped scenario and a failing step cannot disagree. Optional at run
/// time (only the dev-loop scenarios need it), so a miss is a `warn`.
fn check_metro() -> Check {
    match acos_verify::dev_server::metro_environment() {
        Ok(env) => Check::ok(
            "metro",
            format!(
                "node {} ({}), node_modules {}",
                env.node_version,
                env.node.display(),
                env.node_modules.display()
            ),
        ),
        Err(reason) => Check::warn(
            "metro",
            reason,
            "`needs_env: [metro]` scenarios (the React Native dev loop) cannot run without node and a \
             React Native node_modules; `tools/build/build-rn-hello.sh fetch` populates .cache/rn-0.81.4, or set ACOS_METRO_NODE_MODULES.",
        ),
    }
}

fn check_pasta() -> Check {
    let (found, looked) = acos_netd::pasta::resolve();
    let version = found
        .as_ref()
        .and_then(|resolution| acos_netd::pasta::version(&resolution.path));
    pasta_check(found, version, &acos_netd::pasta::searched(&looked))
}

/// REL3D: the running `acos serve` daemon's build identity vs this CLI's.
///
/// `acos serve` is shared across invocations of one user, so after an upgrade (or
/// when two builds are installed) an *older* daemon can still be listening on the
/// session socket. That daemon used to answer new clients with its own stale
/// behaviour (MODA11Y); `doctor` now names both identities so an operator can see
/// whether the daemon on the socket is the CLI they are running.
fn check_session_daemon() -> Check {
    use acos_mcp::daemon::DaemonClient;
    use acos_mcp::identity::Identity;

    let socket = acos_mcp::daemon::socket_path();
    let cli = Identity::local();
    if !socket.exists() {
        return Check::ok(
            "session_daemon",
            format!(
                "no daemon running ({} absent); cli {}",
                socket.display(),
                cli.build_id
            ),
        );
    }
    match DaemonClient::connect(&socket) {
        Ok(client) => {
            let daemon = client.daemon_identity();
            match client.mismatch() {
                None => Check::ok(
                    "session_daemon",
                    format!(
                        "pid {} build {} matches the cli ({})",
                        daemon.pid().unwrap_or(0),
                        daemon
                            .known()
                            .map(|identity| identity.build_id.clone())
                            .unwrap_or_else(|| "unknown".to_string()),
                        socket.display()
                    ),
                ),
                Some(mismatch) => Check::warn(
                    "session_daemon",
                    format!(
                        "running daemon is a different build: {}\n  daemon: {}\n  cli:    {}",
                        mismatch.reason.rule(),
                        daemon
                            .known()
                            .map(Identity::label)
                            .unwrap_or_else(|| "predates the identity protocol".to_string()),
                        cli.label()
                    ),
                    format!(
                        "`acos session …` replaces an idle auto-started daemon and otherwise uses a \
                         build-private one, so its answers are never the old build's. Restart an \
                         operator-started daemon after an upgrade: kill {}",
                        daemon.pid().unwrap_or(0)
                    ),
                ),
            }
        }
        Err(e) => Check::warn(
            "session_daemon",
            format!("{} but unreachable: {e}", socket.display()),
            "Remove the stale socket (or restart `acos serve`); the CLI removes it on the next auto-start.",
        ),
    }
}

/// GLSHIP: the guest's software GL, as the capability line agents read:
/// `gles: angle-swiftshader | none`.
///
/// Software GL (ANGLE on SwiftShader, GL ES 3.1) is ON by default when the
/// image installed the provider; `ACOS_GUEST_GLES=off` turns it off and an
/// explicit `ACOS_GUEST_GLES=<provider>` picks another staged one. Missing is a
/// `warn`, not a `fail`: every app still launches exactly as before, only the
/// GPU paths (a `Bitmap.Config.HARDWARE` readback) are `UNSUPPORTED_BACKEND`.
fn check_gles() -> Check {
    let (label, detail) = acos_gles_provider::capability_for(crate::root::sysroot().as_deref());
    if label == "none" {
        Check::warn(
            "gles",
            format!("none ({detail})"),
            "Apps still launch; only GPU readbacks (Bitmap.Config.HARDWARE) are unsupported. \
             Stage the provider with tools/probes/art-gles.sh (the image build does it).",
        )
    } else {
        Check::ok("gles", format!("{label} ({detail})"))
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
    let install = crate::root::acos_home();
    let installed = install.is_some();
    let mut checks = Vec::new();
    if let Some(home) = &install {
        checks.extend(check_install(home));
    }
    checks.push(check_sdk(args));
    checks.extend(check_build_tools(args, installed));
    checks.push(check_android_jar(args));
    checks.extend(check_java());
    checks.push(check_ndk(args));
    checks.extend(check_sandbox_helpers());
    checks.push(check_pasta());
    checks.push(check_metro());
    checks.extend(check_workspace(installed));
    checks.push(check_gles());
    checks.push(check_session_daemon());
    checks.push(check_memory());
    let subject = json!({
        "acos_version": env!("CARGO_PKG_VERSION"),
        "layout": if installed { "installed" } else { "checkout" },
        "acos_home": install.as_ref().map(|p| p.display().to_string()),
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

    /// `acos doctor`'s pasta line is the capability a scenario's
    /// `needs_env: [pasta]` resolves: the pinned version when the binary is
    /// there, `missing` (with every place the lookup went) when it is not.
    #[test]
    fn pasta_check_names_the_version_or_missing() {
        use acos_netd::pasta::{Resolution, Source, TAG};
        let found = Resolution {
            path: PathBuf::from("/opt/acos/libexec/pasta"),
            source: Source::AcosHome,
        };
        let ok = pasta_check(Some(found.clone()), Some(TAG.to_string()), "");
        assert_eq!(ok.status, "ok");
        assert!(ok.detail.starts_with(TAG), "{}", ok.detail);
        assert!(ok.detail.contains("acos_home"), "{}", ok.detail);
        assert!(ok.fix.is_none());

        let missing = pasta_check(None, None, "/x/libexec/pasta, PATH");
        assert_eq!(missing.status, "warn");
        assert!(missing.detail.starts_with("missing"), "{}", missing.detail);
        assert!(missing.detail.contains("/x/libexec/pasta"), "{}", missing.detail);
        let fix = missing.fix.expect("a missing transport names the build recipe");
        assert!(fix.contains(TAG), "{fix}");
        assert!(fix.contains("libexec/pasta"), "{fix}");

        // A binary that is there but is another build is never `ok`: the
        // relay's transport is pinned (measured), not "some pasta".
        let other = pasta_check(Some(found), Some("0.0.1".to_string()), "");
        assert_eq!(other.status, "warn");
        assert!(other.detail.contains("0.0.1") && other.detail.contains(TAG));
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

    #[test]
    fn install_webview_check_requires_broker_and_provider() {
        let root = std::env::temp_dir().join(format!(
            "acos-doctor-wv-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let broker = root.join("bin/acos-webview");
        let provider = root.join(acos_services::modules::webview::PROVIDER_APK_IMAGE_REL);
        std::fs::create_dir_all(broker.parent().unwrap()).unwrap();
        std::fs::create_dir_all(provider.parent().unwrap()).unwrap();

        let neither = check_install_webview(&root);
        assert_eq!(neither.name, "install_webview");
        assert_eq!(neither.status, "warn");
        assert!(neither.detail.contains("acos-webview"), "{}", neither.detail);
        assert!(
            neither.detail.contains("com.android.webview.apk"),
            "{}",
            neither.detail
        );

        std::fs::write(&provider, b"apk").unwrap();
        let no_broker = check_install_webview(&root);
        assert_eq!(no_broker.status, "warn");
        assert!(
            no_broker.detail.contains("missing broker"),
            "{}",
            no_broker.detail
        );

        std::fs::write(&broker, b"elf").unwrap();
        let both = check_install_webview(&root);
        assert_eq!(both.status, "ok");
        assert!(both.fix.is_none());
        assert!(both.detail.contains("acos-webview"), "{}", both.detail);
        assert!(
            both.detail.contains("com.android.webview.apk"),
            "{}",
            both.detail
        );

        std::fs::remove_file(&provider).unwrap();
        let no_provider = check_install_webview(&root);
        assert_eq!(no_provider.status, "warn");
        assert!(
            no_provider.detail.contains("missing provider"),
            "{}",
            no_provider.detail
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
