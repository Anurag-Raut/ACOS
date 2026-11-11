//! `acos-art render-app` — an APK's first frame, on real ART, as data.
//!
//! Productised form of the E4H feasibility spike (`results/art-feasibility/E4H.md`):
//! one `dalvikvm64` in the acos-art `bwrap` sandbox inflates the APK's launcher
//! layout under a real `Theme`, measures/lays it out at the device's panel size
//! and draws it into a software `Bitmap` with stock `libhwui`/Skia — no zygote,
//! no `system_server`, no `servicemanager`, no `/dev/binder`, no GPU.
//!
//! Output (in `--out`, default `<data>/out`):
//!
//! | file | content |
//! |---|---|
//! | `frame.png` | the frame at `width × contentHeight` (the device's app content band) |
//! | `tree.json` | `{bounds, nodes:[{class,id,text,bounds,measured,padding,…}]}` |
//!
//! and one [`AppResult`] with the [`launcher::Outcome`], a stable `code`, the
//! guest's timings and the md5 of the PNG (repeatability is checkable without
//! re-reading the file).
//!
//! ## The three workarounds this productises (all measured in E4H)
//!
//! 1. **Font map** — `Typeface.sSystemFontMap` is a *zygote artefact*
//!    (`sSystemFontMapSharedMemory`), so a bare `dalvikvm64` has an empty map and
//!    `TextView`'s constructor NPEs in `Typeface.create(null,0)`. The guest
//!    rebuilds the default family from `/system/fonts` and installs it with the
//!    @hide `Typeface.setDefault(Typeface)` (Java field **and** libhwui's
//!    `gDefaultTypeface`; the field alone trips libhwui's
//!    `ASSERT src == nullptr && gDefaultTypeface == nullptr`). The font *files*
//!    are provisioned into the sysroot by [`ensure_system_fonts`] — see its doc
//!    comment for why the sandbox cannot bind them.
//! 2. **Ripple on a software canvas** — `RippleDrawable.draw()` SIGSEGVs the
//!    process inside libhwui (silent: no tombstone, no log line) unless the
//!    canvas is hardware accelerated. The guest downgrades every
//!    `RippleDrawable` to its content layer (`getDrawable(0)`) before drawing;
//!    a ripple is a pressed-state animation and the static frame *is* its
//!    content layer.
//! 3. **No binder peer** — `View.<init>` → `ViewConfiguration` →
//!    `IWindowManager.hasNavigationBar()` → `ServiceManager` → `null` → NPE. The
//!    guest installs a reflective `Proxy` as
//!    `WindowManagerGlobal.sWindowManagerService` answering defaults.
//!
//! Never fake: a missing sysroot/SDK, an APK without a launcher layout, a guest
//! exception and an ART crash are all structured, never a silent fallback.

use crate::launcher::{self, Outcome};
use crate::logd::Logd;
use crate::{ArtRoot, Error, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The guest Java source tree (`dev.acos.art.AppMain` + `RenderNat`).
pub const GUEST_SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/guest/java");
/// The substrate shim source (`dev.acos.art.RenderNat`'s JNI side).
pub const RENDER_SHIM_SOURCE: &str = include_str!("../shim/render_shim.c");

/// Guest class that does the inflate/layout/draw.
pub const GUEST_CLASS: &str = "dev.acos.art.AppMain";

/// Everything the renderer needs to be told.
#[derive(Debug, Clone)]
pub struct AppOpts {
    /// Host path of the APK.
    pub apk: PathBuf,
    /// Application package name; derived from the manifest when `None`.
    pub package: Option<String>,
    /// Layout resource name (default: `main`, then `activity_main`).
    pub layout: Option<String>,
    /// Output directory for `frame.png` / `tree.json`.
    pub out: Option<PathBuf>,
    /// Panel width in px (device oracle: 1080).
    pub width: u32,
    /// App *content* height in px: the oracle's `mAppBounds` minus the status
    /// bar and the action bar (1080×2400 @ 420 dpi ⇒ 2337 − 275 = 2062).
    pub content_height: u32,
    pub density_dpi: u32,
    /// `Widget.Material.Button` resolves `textAllCaps=true` in our theme setup
    /// while the device oracle renders sentence case (a shared theme-resolution
    /// gap, E4H §7). `device` (default) forces `textAllCaps=false` so the frame
    /// and the tree match the device; `theme` keeps the as-inflated behaviour.
    pub text_all_caps: AllCaps,
    pub timeout: Duration,
}

/// Which `textAllCaps` value the guest renders with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AllCaps {
    /// Match the device oracle (sentence case).
    Device,
    /// Leave the theme's value (`true` for Material buttons).
    Theme,
}

impl AllCaps {
    pub fn as_str(self) -> &'static str {
        match self {
            AllCaps::Device => "device",
            AllCaps::Theme => "theme",
        }
    }
}

impl Default for AppOpts {
    fn default() -> Self {
        Self {
            apk: PathBuf::new(),
            package: None,
            layout: None,
            out: None,
            width: 1080,
            content_height: 2062,
            density_dpi: 420,
            text_all_caps: AllCaps::Device,
            timeout: Duration::from_secs(120),
        }
    }
}

/// The structured result of one `render-app` run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppResult {
    /// `ok` only when a frame and a tree were written.
    pub outcome: Outcome,
    /// Stable code (`NO_LAYOUT`, `INFLATE_FAILED`, `NO_FONTS`, `GUEST_ERROR`,
    /// `ART_CRASH`, `TIMEOUT`, `UNSUPPORTED_SYSCALL`, …) or `None`.
    pub code: Option<String>,
    pub apk: String,
    pub package: String,
    pub layout: String,
    pub width: u32,
    pub content_height: u32,
    pub density_dpi: u32,
    pub frame: Option<String>,
    pub tree: Option<String>,
    /// md5 of `frame.png` (repeatability without re-reading it).
    pub frame_md5: Option<String>,
    pub nodes: Option<u64>,
    pub draw_cold_us: Option<u64>,
    pub draw_warm_us: Option<u64>,
    pub nonbg_px: Option<u64>,
    pub guest_wall_ms: Option<u64>,
    pub wall_ms: u64,
    /// Peak RSS of the whole guest process (`VmHWM`), MiB.
    pub peak_rss_mb: Option<f64>,
    /// Guest stdout, truncated to the last 8 KiB (the `ACOSAPP` line, cause
    /// chains and stack traces of a failed rung live here).
    pub stdout: String,
    /// Guest stderr, truncated to the last 8 KiB.
    pub stderr: String,
    pub logs: Vec<crate::logd::LogEntry>,
    pub diagnostics: Vec<String>,
    /// The guest's own note stream (rung timings, downgrades, stub calls).
    pub notes: Vec<String>,
    pub argv: Vec<String>,
}

impl AppResult {
    pub fn succeeded(&self) -> bool {
        self.outcome == Outcome::Ok
    }
}

// ---------------------------------------------------------------- entry point

/// Render one frame of `opts.apk` and write `frame.png` + `tree.json`.
pub fn render_app(root: &ArtRoot, opts: &AppOpts) -> Result<AppResult> {
    if !opts.apk.is_file() {
        return Err(Error::Layout(format!("no APK at {}", opts.apk.display())));
    }
    root.ensure_data()?;
    root.ensure_linkerconfig(false)?;
    root.ensure_classpath_env(false)?;

    let mut diagnostics = Vec::new();
    ensure_system_fonts(root, &mut diagnostics)?;

    // Guest dex + substrate shim (both hash-cached).
    let guest = build_guest(root)?;
    let shim = build_render_shim(root)?;
    let art_shim = crate::build_shim(&root.data.join("shim"))?;

    // The APK and the guest classpath live under /data, which the sandbox
    // already binds read-write.
    let app_dir = root.data.join("app");
    fs::create_dir_all(&app_dir)?;
    let apk_name = opts
        .apk
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "app.apk".to_string());
    let apk = app_dir.join(&apk_name);
    fs::copy(&opts.apk, &apk)?;

    let package = match &opts.package {
        Some(p) => p.clone(),
        None => apk_package(&opts.apk)?,
    };
    let layout = match &opts.layout {
        Some(l) => l.clone(),
        None => pick_layout(&opts.apk, &package)?,
    };
    // The guest always writes into the sandbox's /data (it has no other writable
    // path); the artefacts are copied to --out afterwards.
    let staging = root.data.join("out/render-app");
    fs::create_dir_all(&staging)?;
    for stale in ["frame.png", "tree.json"] {
        let _ = fs::remove_file(staging.join(stale));
    }
    let out = opts.out.clone().unwrap_or_else(|| staging.clone());
    fs::create_dir_all(&out)?;

    let argv: Vec<String> = vec![
        "-cp".to_string(),
        "/data/app/guest/classes.dex".to_string(),
        GUEST_CLASS.to_string(),
        format!("/data/app/{apk_name}"),
        package.clone(),
        "/data/out/render-app".to_string(),
        opts.width.to_string(),
        opts.content_height.to_string(),
        opts.density_dpi.to_string(),
    ];

    // Property table (the art shim serves it) — Build.<clinit> needs the ABI
    // list, ResourcesImpl.updateConfiguration needs ro.build.version.sdk.
    crate::Props::android34().write(&root.data.join(crate::props::PROPS_FILE))?;

    let logd = Logd::start(&root.data.join("socket")).map_err(Error::Io)?;
    let mut sb = root.sandbox();
    // LD_PRELOAD is managed here (not by Sandbox::with_shim) so that the logd /
    // property shim and the substrate shim are loaded together, in this order.
    sb.with_shim(false);
    sb.ro_bind(&art_shim, "/data/shim/libartshim.so");
    sb.ro_bind(&shim, "/data/app/libacosart.so");
    sb.rw_bind(root.data.join("socket"), "/dev/socket");
    sb.env(
        "LD_PRELOAD",
        "/data/shim/libartshim.so:/data/app/libacosart.so",
    );
    sb.env("ACOSART_LOAD", "libandroid_runtime.so:libhwui.so");
    sb.env("acosart.allcaps", opts.text_all_caps.as_str());
    sb.env("ACOSART_BINDER_FAKE", "1");

    let dalvik = "/system/apex/com.android.art/bin/dalvikvm64";
    let mut cmd = sb.command(dalvik, &argv)?;
    unsafe {
        cmd.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0);
            let core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            libc::setrlimit(libc::RLIMIT_CORE, &core);
            let nofile = libc::rlimit { rlim_cur: 4096, rlim_max: 4096 };
            libc::setrlimit(libc::RLIMIT_NOFILE, &nofile);
            Ok(())
        });
    }
    cmd.process_group(0);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let full_argv: Vec<String> =
        std::iter::once(dalvik.to_string()).chain(argv.iter().cloned()).collect();

    let started = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Tool(format!("spawning bwrap: {e}")))?;
    let pid = child.id() as i32;
    let (tx, rx) = mpsc::channel::<(u8, Vec<u8>)>();
    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let tx_out = tx.clone();
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        let _ = tx_out.send((0u8, buf));
    });
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let tx_err = tx.clone();
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        let _ = tx_err.send((1u8, buf));
    });

    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                return Err(Error::Io(e));
            }
        }
        if started.elapsed() >= opts.timeout {
            timed_out = true;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let wall_ms = started.elapsed().as_millis() as u64;

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    for _ in 0..2 {
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok((0, b)) => stdout_buf = b,
            Ok((_, b)) => stderr_buf = b,
            Err(_) => break,
        }
    }
    let _ = out_thread.join();
    let _ = err_thread.join();
    drop(tx);

    let logs = logd.finish();
    let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
    let stderr = String::from_utf8_lossy(&stderr_buf).to_string();

    // bwrap re-exits with 128+signal when the guest died of a signal.
    let raw_exit = status.and_then(|s| s.code());
    let exit = match raw_exit {
        Some(c) if (129..=192).contains(&c) => None,
        other => other,
    };
    let mut signal = status.and_then(|s| std::os::unix::process::ExitStatusExt::signal(&s));
    if signal.is_none() {
        if let Some(c) = raw_exit {
            if (129..=192).contains(&c) {
                signal = Some(c - 128);
            }
        }
    }
    let (mut outcome, mut code, mut diags) =
        launcher::classify(exit, signal, timed_out, &stderr, &logs, opts.timeout);

    // The guest's own verdict (one ACOSAPP <json> line) outranks the generic
    // exit classification: a guest exception with exit 1 is a renderer crash,
    // not a NONZERO_EXIT of some arbitrary DEX.
    let verdict = parse_guest_line(&stdout);
    let mut notes = Vec::new();
    let mut guest_wall_ms = None;
    let mut rss_mb = None;
    let mut draw = (None, None, None);
    let mut nodes = None;
    if let Some(g) = &verdict {
        // The guest echoes the panel it rendered at; a mismatch means the
        // arguments did not survive the sandbox, not a style difference.
        if (g.width, g.content_height, g.density_dpi)
            != (Some(opts.width), Some(opts.content_height), Some(opts.density_dpi))
        {
            diags.push(format!(
                "guest panel {:?}x{:?}@{:?} != requested {}x{}@{}",
                g.width, g.content_height, g.density_dpi,
                opts.width, opts.content_height, opts.density_dpi
            ));
        }
        notes = g.notes.clone();
        guest_wall_ms = g.wall_ms;
        rss_mb = g.peak_rss_kb.map(|kb| kb as f64 / 1024.0);
        draw = (g.draw_cold_us, g.draw_warm_us, g.nonbg_px);
        nodes = g.nodes;
        match g.outcome.as_str() {
            "ok" => {
                outcome = Outcome::Ok;
                code = None;
            }
            other => {
                outcome = if other == "unsupported" { Outcome::Unsupported } else { Outcome::Crash };
                code = g.code.clone().or(Some("GUEST_ERROR".into()));
                diags.push(format!("guest: {}", g.message.clone().unwrap_or_default()));
            }
        }
    }

    let staged_frame = staging.join("frame.png");
    let staged_tree = staging.join("tree.json");
    let frame = out.join("frame.png");
    let tree = out.join("tree.json");
    if staged_frame.is_file() {
        fs::copy(&staged_frame, &frame)?;
    }
    if staged_tree.is_file() {
        fs::copy(&staged_tree, &tree)?;
    }
    let mut frame_md5 = None;
    if !frame.is_file() || !tree.is_file() {
        if outcome == Outcome::Ok {
            outcome = Outcome::Crash;
            code = Some("NO_OUTPUT".into());
            diags.push(format!(
                "guest reported ok but {} / {} missing",
                frame.display(),
                tree.display()
            ));
        }
        if let Some(g) = &verdict {
            diags.extend(g.diagnostics());
        }
    } else {
        frame_md5 = Some(md5_file(&frame)?);
    }
    let _ = (&art_shim, &apk);
    diags.push(format!("logd: {} entries", logs.len()));
    diags.push(format!(
        "guest dex: {} ({})",
        guest.dex_dir.display(),
        if guest.cached { "cached" } else { "built" }
    ));

    Ok(AppResult {
        outcome,
        code,
        apk: opts.apk.display().to_string(),
        package,
        layout,
        width: opts.width,
        content_height: opts.content_height,
        density_dpi: opts.density_dpi,
        frame: frame.is_file().then(|| frame.display().to_string()),
        tree: tree.is_file().then(|| tree.display().to_string()),
        frame_md5,
        nodes,
        draw_cold_us: draw.0,
        draw_warm_us: draw.1,
        nonbg_px: draw.2,
        guest_wall_ms,
        wall_ms,
        peak_rss_mb: rss_mb,
        stdout: tail(&stdout, 8192),
        stderr: tail(&stderr, 8192),
        logs,
        diagnostics: diags,
        notes,
        argv: full_argv,
    })
}

// -------------------------------------------------------------- system fonts

/// Provision `/system/fonts` **into the sysroot**.
///
/// `Typeface`/`SystemFonts` read `/system/etc/fonts.xml` (present in the
/// extracted sysroot) and the font files it names (not present — the extraction
/// only carries `/system`, and on a device the fonts are in the product
/// partition). They cannot be bind-mounted at `/system/fonts`: bwrap cannot
/// create a mount point inside a read-only bind (`bwrap: Can't create file
/// /system/fonts: Read-only file system`), and `fonts.xml` uses absolute
/// `/system/fonts/...` paths. So they are hard-linked from the extracted boot
/// image font set into the sysroot, exactly like
/// `research/art-feasibility/h-view/scripts/hroot.sh` does for the spike
/// (0 bytes copied, idempotent, undo with `rm -rf <sysroot>/system/fonts`).
///
/// Without them text layout produces nothing and `TextView`'s constructor NPEs;
/// the run is then reported as `NoSysroot`/`UNSUPPORTED` rather than silently
/// rendering blank text.
pub fn ensure_system_fonts(root: &ArtRoot, diagnostics: &mut Vec<String>) -> Result<PathBuf> {
    let dest = root.sysroot.join("system/fonts");
    if dest.join("Roboto-Regular.ttf").is_file() {
        return Ok(dest);
    }
    let src = root.repo_root.join(".cache/boot/android-34/fonts");
    if !src.is_dir() {
        diagnostics.push(format!(
            "no system fonts: {} is missing (rebuild them with the boot-image extraction); \
             text will not render",
            src.display()
        ));
        return Ok(dest);
    }
    fs::create_dir_all(&dest)?;
    let mut linked = 0usize;
    let mut copied = 0usize;
    for e in fs::read_dir(&src)?.filter_map(|e| e.ok()) {
        let from = e.path();
        if !from.is_file() {
            continue;
        }
        let to = dest.join(e.file_name());
        if to.exists() {
            continue;
        }
        if fs::hard_link(&from, &to).is_ok() {
            linked += 1;
        } else {
            fs::copy(&from, &to)?;
            copied += 1;
        }
    }
    if let Some(xml) = src.join("fonts.xml").is_file().then(|| src.join("fonts.xml")) {
        let to = root.sysroot.join("system/etc/fonts.xml");
        if !to.is_file() {
            let _ = fs::hard_link(&xml, &to).or_else(|_| fs::copy(&xml, &to).map(|_| ()));
        }
    }
    let n = fs::read_dir(&dest).map(|d| d.count()).unwrap_or(0);
    diagnostics.push(format!(
        "provisioned {n} system fonts into {} ({linked} hard links, {copied} copies)",
        dest.display()
    ));
    Ok(dest)
}

// ------------------------------------------------------------------ guest dex

/// A built guest classpath.
#[derive(Debug, Clone)]
pub struct GuestDex {
    pub dex_dir: PathBuf,
    pub cached: bool,
}

/// Build `dev.acos.art.AppMain` (javac + d8) into `<data>/app/guest`, cached by
/// source hash. Also returns the SDK paths used, for the result diagnostics.
pub fn build_guest(root: &ArtRoot) -> Result<GuestDex> {
    let src_dir = PathBuf::from(GUEST_SOURCE);
    let out_dir = root.data.join("app/guest");
    fs::create_dir_all(&out_dir)?;
    let sources = java_sources(&src_dir)?;
    if sources.is_empty() {
        return Err(Error::Layout(format!("no guest Java sources under {}", src_dir.display())));
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for p in &sources {
        hash = fnv(hash, p.to_string_lossy().as_bytes());
        hash = fnv(hash, &fs::read(p)?);
    }
    let stamp = out_dir.join(".hash");
    let dex = out_dir.join("classes.dex");
    let up_to_date = dex.is_file()
        && fs::read_to_string(&stamp).map(|s| s.trim() == hash.to_string()).unwrap_or(false);
    if up_to_date {
        return Ok(GuestDex { dex_dir: out_dir, cached: true });
    }

    let sdk = android_sdk()?;
    let android_jar = sdk.join("platforms/android-34/android.jar");
    if !android_jar.is_file() {
        return Err(Error::Tool(format!(
            "no android.jar at {} (set ANDROID_SDK_ROOT)",
            android_jar.display()
        )));
    }
    let d8 = find_tool(&sdk, "d8")?;
    let classes = out_dir.join("classes");
    let _ = fs::remove_dir_all(&classes);
    fs::create_dir_all(&classes)?;
    let mut javac = Command::new(std::env::var("JAVAC").unwrap_or_else(|_| "javac".into()));
    javac
        .args(["-nowarn", "-source", "11", "-target", "11"])
        .arg("-cp")
        .arg(&android_jar)
        .arg("-d")
        .arg(&classes)
        .args(&sources);
    let out = javac.output()?;
    if !out.status.success() {
        return Err(Error::Tool(format!(
            "javac failed for the render-app guest:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let class_files = walk(&classes, "class");
    let out = Command::new(&d8)
        .arg("--lib")
        .arg(&android_jar)
        .args(["--min-api", "34", "--output"])
        .arg(&out_dir)
        .args(&class_files)
        .output()?;
    if !out.status.success() || !dex.is_file() {
        return Err(Error::Tool(format!(
            "d8 failed for the render-app guest (status {:?}):\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    fs::write(&stamp, format!("{hash}\n"))?;
    let _ = fs::remove_dir_all(&classes);
    Ok(GuestDex { dex_dir: out_dir, cached: false })
}

/// Compile `shim/render_shim.c` with the NDK's bionic clang (cached by source
/// hash) into `<data>/app/libacosart.so`.
///
/// It has to be a *bionic* object: it is loaded into the ART process, calls
/// `dlsym` on bionic's linker namespace and is entered through JNI's `JNIEnv`.
pub fn build_render_shim(root: &ArtRoot) -> Result<PathBuf> {
    let dir = root.data.join("app");
    fs::create_dir_all(&dir)?;
    let out = dir.join("libacosart.so");
    let src = dir.join("render_shim.c");
    let hash = fnv(0xcbf2_9ce4_8422_2325, RENDER_SHIM_SOURCE.as_bytes());
    let stamp = dir.join("libacosart.hash");
    if out.is_file()
        && fs::read_to_string(&stamp).map(|s| s.trim() == hash.to_string()).unwrap_or(false)
    {
        return Ok(out);
    }
    fs::write(&src, RENDER_SHIM_SOURCE)?;
    let clang = ndk_clang()?;
    let st = Command::new(&clang)
        .args(["-O2", "-fPIC", "-shared", "-Wall", "-Wno-unused-parameter", "-o"])
        .arg(&out)
        .arg(&src)
        .arg("-ldl")
        .status()?;
    if !st.success() {
        return Err(Error::Tool(format!(
            "{} failed to build {}",
            clang.display(),
            src.display()
        )));
    }
    fs::write(&stamp, format!("{hash}\n"))?;
    Ok(out)
}

// --------------------------------------------------------------- apk metadata

fn android_sdk() -> Result<PathBuf> {
    for var in ["ANDROID_SDK_ROOT", "ANDROID_HOME"] {
        if let Some(p) = std::env::var_os(var) {
            let p = PathBuf::from(p);
            if p.join("platforms").is_dir() {
                return Ok(p);
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join("Android/Sdk");
        if p.join("platforms").is_dir() {
            return Ok(p);
        }
    }
    Err(Error::Tool(
        "no Android SDK found (set ANDROID_SDK_ROOT; needs platforms/android-34/android.jar and build-tools/*/d8)"
            .into(),
    ))
}

fn find_tool(sdk: &Path, name: &str) -> Result<PathBuf> {
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for e in fs::read_dir(sdk.join("build-tools")).map_err(|_| {
        Error::Tool(format!("no build-tools under {}", sdk.display()))
    })? {
        let e = e?;
        let p = e.path().join(name);
        if !p.is_file() {
            continue;
        }
        let ver: Vec<u32> = e
            .file_name()
            .to_string_lossy()
            .split('.')
            .map(|c| c.parse().unwrap_or(0))
            .collect();
        if best.as_ref().map(|(v, _)| ver > *v).unwrap_or(true) {
            best = Some((ver, p));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| Error::Tool(format!("no {name} under {}/build-tools", sdk.display())))
}

fn ndk_clang() -> Result<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("ANDROID_NDK_HOME").or_else(|| std::env::var_os("ANDROID_NDK_ROOT")) {
        roots.push(PathBuf::from(p));
    }
    if let Ok(sdk) = android_sdk() {
        if let Ok(rd) = fs::read_dir(sdk.join("ndk")) {
            let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            v.sort();
            v.reverse();
            roots.extend(v);
        }
    }
    for r in roots {
        let dir = r.join("toolchains/llvm/prebuilt/linux-x86_64/bin");
        for target in ["x86_64-linux-android34-clang", "x86_64-linux-android33-clang"] {
            let p = dir.join(target);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    if let Ok(cc) = std::env::var("CC") {
        return Ok(PathBuf::from(cc));
    }
    Err(Error::Tool(
        "no NDK clang found (set ANDROID_NDK_HOME; needs x86_64-linux-android34-clang)".into(),
    ))
}

/// The APK's package name, from `aapt2 dump packagename` (the SDK binary that
/// owns binary-manifest parsing).
pub fn apk_package(apk: &Path) -> Result<String> {
    let sdk = android_sdk()?;
    let aapt2 = find_tool(&sdk, "aapt2")?;
    let out = Command::new(&aapt2)
        .arg("dump")
        .arg("packagename")
        .arg(apk)
        .output()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || name.is_empty() {
        return Err(Error::Tool(format!(
            "aapt2 could not read the package of {}: {}{} (pass --package)",
            apk.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(name)
}

/// The layout resource to inflate: `main` when the APK has it, else
/// `activity_main`, else the first `layout/*` entry the APK ships.
fn pick_layout(apk: &Path, package: &str) -> Result<String> {
    let sdk = android_sdk()?;
    let aapt2 = find_tool(&sdk, "aapt2")?;
    let out = Command::new(&aapt2).arg("dump").arg("resources").arg(apk).output()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut layouts: Vec<String> = Vec::new();
    for line in text.lines() {
        // aapt2 dump resources prints one of
        //   "resource 0x7f020000 layout/main"          (aapt2 >= 30)
        //   "resource 0x7f020000 pkg:layout/main"
        let Some(idx) = line
            .find(" layout/")
            .map(|i| i + " layout/".len())
            .or_else(|| line.find(&format!("{package}:layout/")).map(|i| i + package.len() + ":layout/".len()))
        else {
            continue;
        };
        let name = line[idx..].split_whitespace().next().unwrap_or("").to_string();
        if !name.is_empty() && !name.contains('/') && !layouts.contains(&name) {
            layouts.push(name);
        }
    }
    for want in ["main", "activity_main"] {
        if layouts.iter().any(|l| l == want) {
            return Ok(want.to_string());
        }
    }
    match layouts.first() {
        Some(l) => Ok(l.clone()),
        None => Err(Error::Tool(format!(
            "{} has no layout/ resources (pass --layout)",
            apk.display()
        ))),
    }
}

// ------------------------------------------------------------------- helpers

/// The guest's `ACOSAPP {...}` verdict line.
#[derive(Debug, Clone, Deserialize)]
struct GuestLine {
    outcome: String,
    code: Option<String>,
    message: Option<String>,
    width: Option<u32>,
    #[serde(rename = "contentHeight")]
    content_height: Option<u32>,
    #[serde(rename = "densityDpi")]
    density_dpi: Option<u32>,
    #[serde(rename = "draw_cold_us")]
    draw_cold_us: Option<u64>,
    #[serde(rename = "draw_warm_us")]
    draw_warm_us: Option<u64>,
    #[serde(rename = "nonbg_px")]
    nonbg_px: Option<u64>,
    #[serde(rename = "peak_rss_kb")]
    peak_rss_kb: Option<u64>,
    #[serde(rename = "wall_ms")]
    wall_ms: Option<u64>,
    nodes: Option<u64>,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    notes: Vec<String>,
}

impl GuestLine {
    fn diagnostics(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(c) = &self.code {
            v.push(format!("guest code: {c}"));
        }
        if let Some(m) = &self.message {
            v.push(format!("guest message: {m}"));
        }
        for w in &self.warnings {
            v.push(format!("guest warning: {w}"));
        }
        v
    }
}

fn parse_guest_line(stdout: &str) -> Option<GuestLine> {
    stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("ACOSAPP ").and_then(|j| serde_json::from_str(j).ok()))
}

fn java_sources(dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(walk(dir, "java"))
}

fn walk(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().map(|x| x == ext).unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Last `n` bytes of `s` (on a char boundary), for the diagnostic fields.
fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut start = s.len() - n;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn md5_file(p: &Path) -> Result<String> {
    // No md5 crate in this workspace: shell out to the system `md5sum`.
    let out = Command::new("md5sum").arg(p).output()?;
    if !out.status.success() {
        return Err(Error::Tool(format!("md5sum failed for {}", p.display())));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(s.split_whitespace().next().unwrap_or("").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_line_parses() {
        let line = r##"ACOSAPP {"outcome":"ok","code":null,"width":1080,"contentHeight":2062,"densityDpi":420,"draw_cold_us":450,"draw_warm_us":90,"nonbg_px":44097,"background":"#fff0f0f3","nodes":5,"wall_ms":216,"peak_rss_kb":247000,"warnings":[],"notes":["x"]}"##;
        let g = parse_guest_line(line).expect("parsed");
        assert_eq!(g.outcome, "ok");
        assert_eq!(g.width, Some(1080));
        assert_eq!(g.content_height, Some(2062));
        assert_eq!(g.draw_cold_us, Some(450));
        assert_eq!(g.nodes, Some(5));
    }

    #[test]
    fn no_layout_is_a_tool_error() {
        let e = pick_layout(Path::new("/nonexistent.apk"), "pkg");
        assert!(e.is_err());
    }
}
