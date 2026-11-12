//! `acos-art session` — **one long-lived ART process per app**, driven by a
//! line-JSON command protocol.
//!
//! `render-app` answers "what does this APK's first frame look like?"; a session
//! answers "what does the app do next?" without paying process + inflate +
//! resource setup again for every step. The guest
//! ([`super::app::GUEST_SOURCE`]`/dev/acos/art/SessionMain.java`) stays alive,
//! runs the manifest's launcher `Activity` for real (`attach` →
//! `performCreate` → `performStart` → `performResume`, so the app's own
//! `onCreate` — including `setOnClickListener` — executes) and serves:
//!
//! ```text
//! {"cmd":"screenshot"}          -> PNG path + md5 + frame_seq
//! {"cmd":"tree"}                -> the full View tree (class/id/text/bounds/…)
//! {"cmd":"tap","x":..,"y":..}   -> real MotionEvent DOWN/UP through
//!                                  DecorView.dispatchTouchEvent (app listener)
//! {"cmd":"key","code":..}       -> real KeyEvent DOWN/UP
//! {"cmd":"reset"}               -> fresh Activity (count back to 0)
//! {"cmd":"close"}               -> stop the session
//! ```
//!
//! One reply line per command; `frame_seq` advances **only when the pixels of a
//! frame are complete**, and every reply that carries a `frame_seq` carries that
//! frame's `png` + `md5`, so a caller never has to guess whether a screenshot it
//! took is the frame before or after its tap.
//!
//! ## What this process owns (and what it does not)
//!
//! Host side: provisioning (fonts, the image's static RRO idmaps, the guest dex
//! + substrate shim — all reused from [`crate::app`]), the sandbox, a
//! **per-session** `/dev/socket` (so two sessions in parallel cannot fight over
//! `logdw`), a **per-session** copy of the overlay idmaps (so two sessions
//! cannot race on idmap generation), the process, and the relay of the protocol
//! between the caller's stdin/stdout and the guest's.
//!
//! Guest side: everything that needs a `JNIEnv` or a `View` — the guest is the
//! one that owns the pixels and the tree. The host never touches them.
//!
//! The host therefore does no interpretation of app state: `tap` is not
//! translated into anything, `tree` is not post-processed, `png` bytes are not
//! re-encoded. It forwards one line and returns one line, which is what makes
//! the latency numbers in `results/art-runtime/P3d.md` honest (host relay
//! overhead is measured separately).

use crate::capture::{stream_cap_from_env, TailBuffer};
use crate::launcher::Outcome;
use crate::logd::Logd;
use crate::watchdog::{MemoryWatchdog, HOST_MEMORY_LIMIT};
use crate::{ArtRoot, Error, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How many replies a [`SessionReport`] retains (drop-oldest, counted).
pub const MAX_REPLIES: usize = 10_000;

/// Prefix of a protocol line on the guest's stdout (everything else on that
/// stream — the `ACOSAPP` verdict, app `System.out` — is not protocol).
pub const CMD_PREFIX: &str = "ACOSCMD ";

/// Prefix of the guest's final verdict line (same shape `render-app` parses).
pub const VERDICT_PREFIX: &str = "ACOSAPP ";

/// Guest class that serves the session.
pub const SESSION_CLASS: &str = "dev.acos.art.SessionMain";

/// How the caller wants the session.
#[derive(Debug, Clone)]
pub struct SessionOpts {
    pub apk: PathBuf,
    /// `None` ⇒ from the APK's binary manifest.
    pub package: Option<String>,
    /// `None` ⇒ the manifest's `MAIN`/`LAUNCHER` activity.
    pub activity: Option<String>,
    /// Where the published frames are mirrored on the host.
    pub out: Option<PathBuf>,
    pub width: u32,
    pub panel_height: u32,
    pub content_height: u32,
    pub status_inset: u32,
    pub nav_inset: u32,
    pub density_dpi: u32,
    /// Startup + per-command budget.
    pub timeout: Duration,
    /// Session label (also the per-session data-dir name); `None` ⇒ generated.
    pub session_id: Option<String>,
    /// Serve the line-JSON protocol on this unix socket instead of stdin/stdout.
    /// A launcher that cannot hand a child its stdin (every process started
    /// through a wrapper that detaches it) can still drive a session this way,
    /// and two sessions never share a channel.
    pub socket: Option<PathBuf>,
}

impl Default for SessionOpts {
    fn default() -> Self {
        Self {
            apk: PathBuf::new(),
            package: None,
            activity: None,
            out: None,
            width: 1080,
            panel_height: 2400,
            content_height: 2062,
            status_inset: 128,
            nav_inset: 63,
            density_dpi: 420,
            timeout: Duration::from_secs(120),
            session_id: None,
            socket: None,
        }
    }
}

/// One protocol reply, as it went to the caller (host paths already mapped).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReply {
    pub ok: bool,
    pub cmd: String,
    pub frame_seq: Option<u64>,
    pub png: Option<String>,
    pub md5: Option<String>,
    pub host_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<serde_json::Value>,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

/// What one session did, for the caller's summary and for tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReport {
    pub outcome: Outcome,
    pub code: Option<String>,
    pub session: String,
    pub apk: String,
    pub package: String,
    pub activity: String,
    pub pid: Option<i32>,
    pub width: u32,
    pub height: u32,
    pub ready: Option<serde_json::Value>,
    pub replies: Vec<SessionReply>,
    /// Commands the guest answered.
    pub commands: u64,
    pub frames: u64,
    /// Last `frame_seq` handed to the caller.
    pub frame_seq: u64,
    pub wall_ms: u64,
    pub peak_rss_mb: Option<f64>,
    /// The unix socket the protocol was served on, when `--socket` was used.
    pub socket: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub diagnostics: Vec<String>,
    pub notes: Vec<String>,
}

impl SessionReport {
    pub fn succeeded(&self) -> bool {
        self.outcome == Outcome::Ok
    }
}

// --------------------------------------------------------- session guest dex

/// Guest sources the session needs: its own entry point plus the rungs it
/// reuses from `render-app`. Compiled **on their own**, not with every file in
/// `guest/java`: the session is a separate process with a separate entry point,
/// and an unrelated in-flight guest file must not be able to break it (a lesson
/// from a concurrent `LaunchMain.java` that referenced a not-yet-existing
/// package and took `render-app` down with it).
pub fn session_guest_sources() -> Vec<PathBuf> {
    let dir = PathBuf::from(super::app::GUEST_SOURCE).join("dev/acos/art");
    ["AppMain.java", "RenderNat.java", "SessionMain.java"]
        .iter()
        .map(|f| dir.join(f))
        .filter(|p| p.is_file())
        .collect()
}

/// javac + d8 over [`session_guest_sources`] into `<data>/app/session-guest`,
/// cached by the content hash of exactly those files.
pub fn build_session_guest(root: &ArtRoot) -> Result<super::app::GuestDex> {
    let out_dir = root.data.join("app/session-guest");
    fs::create_dir_all(&out_dir)?;
    let sources = session_guest_sources();
    if sources.is_empty() {
        return Err(Error::Layout("no session guest sources".into()));
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for p in &sources {
        hash = hash.wrapping_mul(1099511628211);
        hash = hash.wrapping_mul(1099511628211) ^ fs::read(p)?.len() as u64;
        for b in fs::read(p)? {
            hash ^= b as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
    }
    let stamp = out_dir.join(".hash");
    let dex = out_dir.join("classes.dex");
    let cached = dex.is_file()
        && fs::read_to_string(&stamp).map(|s| s.trim() == hash.to_string()).unwrap_or(false);
    if cached {
        return Ok(super::app::GuestDex { dex_dir: out_dir, cached: true });
    }
    let sdk = super::app::android_sdk()?;
    let android_jar = sdk.join("platforms/android-34/android.jar");
    if !android_jar.is_file() {
        return Err(Error::Tool(format!("no android.jar at {}", android_jar.display())));
    }
    let d8 = super::app::find_tool(&sdk, "d8")?;
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
            "javac failed for the session guest:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let mut class_files = Vec::new();
    let mut stack = vec![classes.clone()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d)? {
            let p = e?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("class") {
                class_files.push(p);
            }
        }
    }
    let out = Command::new(&d8)
        .arg("--lib")
        .arg(&android_jar)
        .args(["--min-api", "34", "--output"])
        .arg(&out_dir)
        .args(&class_files)
        .output()?;
    if !out.status.success() || !dex.is_file() {
        return Err(Error::Tool(format!(
            "d8 failed for the session guest (status {:?}):\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    fs::write(&stamp, format!("{hash}\n"))?;
    let _ = fs::remove_dir_all(&classes);
    Ok(super::app::GuestDex { dex_dir: out_dir, cached: false })
}

// ------------------------------------------------------------------- paths

/// Map a guest path (`/data/...` inside the sandbox) to the host path it is
/// bound from. The sandbox binds [`ArtRoot::data`] at `/data`, so this is the
/// identity once the prefix is swapped — the guest never sees a host path and
/// the caller never sees a sandbox path.
pub fn host_path(data: &Path, guest: &str) -> PathBuf {
    match guest.strip_prefix("/data/") {
        Some(rest) => data.join(rest),
        None => PathBuf::from(guest),
    }
}

/// The guest argv (everything after the class name). Pure so it can be tested
/// without a sandbox.
#[allow(clippy::too_many_arguments)]
pub fn guest_argv(
    apk: &str,
    package: &str,
    out: &str,
    width: u32,
    content_height: u32,
    density_dpi: u32,
    idmaps: &str,
    panel_height: u32,
    status_inset: u32,
    nav_inset: u32,
    theme: &str,
    title: &str,
    target_sdk: u32,
    activity: &str,
    session: &str,
) -> Vec<String> {
    vec![
        apk.to_string(),
        package.to_string(),
        out.to_string(),
        width.to_string(),
        content_height.to_string(),
        density_dpi.to_string(),
        idmaps.to_string(),
        panel_height.to_string(),
        status_inset.to_string(),
        nav_inset.to_string(),
        theme.to_string(),
        title.to_string(),
        target_sdk.to_string(),
        activity.to_string(),
        session.to_string(),
    ]
}

/// `ACOSCMD {…}` → the JSON value, or `None` for any other line.
pub fn parse_protocol_line(line: &str) -> Option<serde_json::Value> {
    let body = line.trim_end_matches(['\r', '\n']).strip_prefix(CMD_PREFIX)?;
    serde_json::from_str(body).ok()
}

/// The manifest's launcher component (`PackageParser` rules: `<activity>` or
/// `<activity-alias>` with MAIN/LAUNCHER). Nothing per-app is hardcoded here.
pub fn apk_activity(apk: &Path) -> Result<String> {
    acos_services::manifest_resolve::launcher_activity(apk)
        .map(|l| l.component)
        .map_err(|e| {
            Error::Tool(format!(
                "{}: {} ({}; resolved from AndroidManifest.xml, not aapt2 badging)",
                apk.display(),
                e,
                e.code()
            ))
        })
}

// -------------------------------------------------------------------- session

/// Run one session: start the guest, relay the protocol until `close`/EOF.
///
/// The relay is deliberately thin: one line in, one line out. Anything the guest
/// says that is not a protocol line is kept (`stdout`/`stderr`/verdict) and
/// reported, never filtered into silence.
pub fn session(root: &ArtRoot, opts: &SessionOpts) -> Result<SessionReport> {
    if !opts.apk.is_file() {
        return Err(Error::Layout(format!("no APK at {}", opts.apk.display())));
    }
    root.ensure_data()?;
    root.ensure_linkerconfig(false)?;
    root.ensure_classpath_env(false)?;

    let started = Instant::now();
    let mut diagnostics = Vec::new();
    // Provisioning is `render-app`'s, unchanged (P3c owns ensure_system_fonts).
    super::app::ensure_system_fonts(root, &mut diagnostics)?;
    let overlays = super::app::provision_overlays(root, &mut diagnostics)?;
    let guest = build_session_guest(root)?;
    let shim = super::app::build_render_shim(root)?;
    let art_shim = crate::build_shim(&root.data.join("shim"))?;

    let session_id = opts
        .session_id
        .clone()
        .unwrap_or_else(|| format!("s{}-{}", std::process::id(), started.elapsed().as_nanos()));
    // Private per-run root (S4): it *is* the session dir, bound into the guest at
    // `/data/session/<id>`; it is removed when this function returns.
    let run = root.new_run_dir(&format!("session-{session_id}"))?;
    let sdir = run.path().to_path_buf();
    let out_dir = run.dir("out");

    // The APK lives under /data (bound read-write), as in render-app.
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
        None => super::app::apk_package(&opts.apk)?,
    };
    let (theme, title, target_sdk) = super::app::apk_chrome(&opts.apk)?;
    let activity = match &opts.activity {
        Some(a) => a.clone(),
        None => apk_activity(&opts.apk)?,
    };

    // Per-session overlay idmaps: the shared cache is written by whoever runs
    // first, and two sessions starting together must not race on it. The idmap
    // records absolute APK paths (identical for every session), so copying the
    // file is enough.
    let ov_dir = sdir.join("overlay");
    fs::create_dir_all(&ov_dir)?;
    let mut idmap_paths = Vec::new();
    for src in &overlays.host_idmaps {
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        let dst = ov_dir.join(&name);
        fs::copy(src, &dst)?;
        idmap_paths.push(format!("/data/session/{session_id}/overlay/{name}"));
    }

    crate::Props::android34().write(&root.data.join(crate::props::PROPS_FILE))?;

    // Per-session /dev/socket: `Logd` unlinks and rebinds `logdw`, so sharing
    // the directory with another session would hijack its log sink.
    let sock_dir = sdir.join("socket");
    fs::create_dir_all(&sock_dir)?;
    let logd = Logd::start(&sock_dir).map_err(Error::Io)?;

    let guest_apk = format!("/data/app/{apk_name}");
    let guest_out = format!("/data/session/{session_id}/out");
    let argv = guest_argv(
        &guest_apk,
        &package,
        &guest_out,
        opts.width,
        opts.content_height,
        opts.density_dpi,
        &idmap_paths.join(","),
        opts.panel_height,
        opts.status_inset,
        opts.nav_inset,
        &theme,
        &title,
        target_sdk,
        &activity,
        &session_id,
    );

    crate::seccomp::enable(root)?;
    let mut sb = root.sandbox();
    sb.run_root(run.path());
    sb.seccomp_guest(true);
    sb.with_shim(false);
    sb.ro_bind(&art_shim, "/data/shim/libartshim.so");
    sb.ro_bind(&shim, "/data/app/libacosart.so");
    sb.ro_bind(&root.sysroot.join("shim/libacoshw.so"), "/data/libacoshw.so");
    // The session dir is writable to the guest, but only this session's:
    // `/data/session/<id>` is the run root, `/data/session` itself is a plain
    // temp dir with nothing in it.
    sb.rw_bind(&sdir, &format!("/data/session/{session_id}"));
    sb.rw_bind(&sock_dir, "/dev/socket");
    sb.env(
        "LD_PRELOAD",
        "/data/shim/libartshim.so:/data/app/libacosart.so:/data/libacoshw.so",
    );
    sb.env("ACOSART_LOAD", "libandroid_runtime.so:libhwui.so");
    sb.env("ACOSART_BINDER_FAKE", "1");

    let dalvik = "/system/apex/com.android.art/bin/dalvikvm64";
    let cmd = sb.command(
        dalvik,
        &[
            vec![
                "-cp".to_string(),
                "/data/app/session-guest/classes.dex".to_string(),
                SESSION_CLASS.to_string(),
            ],
            argv.clone(),
        ]
        .concat(),
    )?;
    // S5: a proven memory/pids cgroup scope, and the guest seccomp filter
    // (fail-closed, exactly as on the `run`/`verify` route).
    let scope_info = std::fs::canonicalize(run.path())
        .unwrap_or_else(|_| run.path().to_path_buf())
        .join("cgroup");
    let (mut child, mut cgroup) = crate::cgroup::spawn_scoped(
        crate::cgroup::MemoryLimits::from_env()?,
        scope_info,
        &cmd,
        |c| {
            unsafe {
                c.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0);
            let core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            libc::setrlimit(libc::RLIMIT_CORE, &core);
            let nofile = libc::rlimit { rlim_cur: 4096, rlim_max: 4096 };
            libc::setrlimit(libc::RLIMIT_NOFILE, &nofile);
            // The launcher's memory backstops bound acos-art, never ART.
            crate::watchdog::relax_child_limits();
            // init.rc: setrlimit nice 40 40 (host best-effort; see watchdog).
            crate::watchdog::apply_init_rlimit_nice();
            Ok(())
                });
            }
            c.process_group(0);
            c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        },
    )?;
    let pid = child.id() as i32;

    // The launcher's own ceiling (ACOS_ART_RSS_MAX, default 2 GiB): a leak the
    // caps do not know about ends as HOST_MEMORY_LIMIT, never a kernel OOM.
    let dog = MemoryWatchdog::from_env();
    if let Some(d) = &dog {
        d.arm_process_group(pid);
    }

    let cap = stream_cap_from_env();
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");

    // One reader thread: protocol lines go to the relay channel, everything
    // else is kept for the report (the guest's ACOSAPP verdict lives there).
    let (tx, rx) = mpsc::channel::<Line>();
    let tx2 = tx.clone();
    let proto = std::thread::spawn(move || {
        // Non-protocol stdout is kept as a *bounded* tail (R1): a guest that
        // prints in a loop must not be able to grow the session host.
        let mut other = TailBuffer::with_cap(cap);
        for line in BufReader::new(stdout_pipe).lines() {
            let Ok(line) = line else { break };
            if let Some(v) = parse_protocol_line(&line) {
                if tx2.send(Line::Protocol(v)).is_err() {
                    break;
                }
            } else {
                other.push(line.as_bytes());
                other.push(b"\n");
                if tx2.send(Line::Other).is_err() {
                    break;
                }
            }
        }
        if tx2.send(Line::Eof(other)).is_err() {}
    });
    // The guest's stderr is streamed to `<session>/guest-stderr.txt` as it is
    // produced, not only at the end: if the guest wedges, the last thing it said
    // (and an ART thread dump from SIGQUIT) is already on disk.
    // The in-memory copy is a bounded tail (R1); the on-disk stream is capped at
    // the same byte count, so a stderr spammer cannot fill the disk either.
    let stderr_file_cap = cap;
    let err_path = sdir.join("guest-stderr.txt");
    let err_thread = std::thread::spawn(move || {
        use std::io::Write as _;
        let mut tail = TailBuffer::with_cap(cap);
        let mut file = fs::File::create(&err_path).ok();
        let mut written = 0usize;
        let mut chunk = vec![0u8; 8192];
        loop {
            match stderr_pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    tail.push(&chunk[..n]);
                    if let Some(f) = &mut file {
                        let left = stderr_file_cap.saturating_sub(written);
                        if left > 0 {
                            let take = left.min(n);
                            let _ = f.write_all(&chunk[..take]);
                            let _ = f.flush();
                            written += take;
                        } else if written == stderr_file_cap {
                            // One marker, then the file stops growing.
                            let _ = writeln!(
                                f,
                                "\n[acos-art: guest stderr file capped at {stderr_file_cap} bytes]"
                            );
                            let _ = f.flush();
                            written += 1;
                        }
                    }
                }
            }
        }
        tail
    });

    let mut replies: Vec<SessionReply> = Vec::new();
    let mut replies_dropped: u64 = 0;
    let mut ready: Option<serde_json::Value> = None;
    let mut notes = Vec::new();
    let mut frames = 0u64;
    let mut frame_seq = 0u64;
    let mut diagnostics_extra: Vec<String> = Vec::new();

    // --- startup: wait for the ready line
    let startup_deadline = Instant::now() + opts.timeout.min(Duration::from_secs(90));
    let mut relay_error: Option<(String, String)> = None;
    loop {
        if Instant::now() > startup_deadline {
            diagnostics.push("session startup timed out before the ready line".into());
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Line::Protocol(v)) => {
                let reply = host_reply(v, &root.data, 0.0);
                if reply.ok {
                    frame_seq = reply.frame_seq.unwrap_or(0);
                    if let Some(t) = &reply.tree {
                        let _ = t;
                    }
                    notes.extend(tree_texts(&reply));
                    ready = Some(serde_json::to_value(&reply).unwrap_or_default());
                } else {
                    relay_error = Some((
                        reply.rest.get("code").and_then(|c| c.as_str()).unwrap_or("STARTUP_FAILED").into(),
                        reply.rest.get("error").and_then(|c| c.as_str()).unwrap_or("").into(),
                    ));
                }
                break;
            }
            Ok(Line::Other) => {}
            Ok(Line::Eof(other)) => {
                diagnostics.extend(
                    other.to_text().lines().map(|l| format!("guest stdout: {l}")),
                );
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // --- the relay loop: stdin/stdout, or one unix socket when `--socket` was
    // given (the caller's channel of choice; two sessions never share one).
    let mut host_out = std::io::stdout();
    if let Some(r) = &ready {
        let mut v = r.clone();
        add_derived(&mut v, &root.data);
        writeln!(host_out, "{}", serde_json::to_string(&v).unwrap_or_default())?;
        host_out.flush()?;
    }

    let mut socket_path: Option<PathBuf> = None;
    let mut socket_rx: Option<mpsc::Receiver<String>> = None;
    let mut sink = Sink::Stdout;
    if let Some(sp) = &opts.socket {
        let stream = bind_and_accept(sp, opts.timeout)?;
        socket_path = Some(sp.clone());
        let announce = serde_json::json!({
            "ok": true,
            "cmd": "listening",
            "socket": sp.display().to_string(),
            "session": session_id.clone(),
        });
        writeln!(host_out, "{}", announce)?;
        host_out.flush()?;
        let rd = stream.try_clone()?;
        let (stx, srx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut r = BufReader::new(rd);
            loop {
                let mut line = String::new();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if stx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        socket_rx = Some(srx);
        sink = Sink::Socket(stream);
        diagnostics.push(format!("protocol on unix socket {}", sp.display()));
    }

    let mut closed = relay_error.is_some() || ready.is_none();
    if let Some((code, error)) = &relay_error {
        diagnostics.push(format!("session failed to start: {code}: {error}"));
    }
    while !closed {
        // One request line from the caller (stdin or the session socket).
        let line = match &socket_rx {
            Some(rx) => match rx.recv_timeout(opts.timeout) {
                Ok(l) => l,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    diagnostics
                        .push("no command on the session socket within the timeout".into());
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => {
                let mut line = String::new();
                match std::io::stdin().read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => line,
                    Err(e) => {
                        diagnostics.push(format!("stdin: {e}"));
                        break;
                    }
                }
            }
        };
        let trimmed = line.trim_end_matches(['\r', '\n']).trim();
        if trimmed.is_empty() {
            continue;
        }
        let t0 = Instant::now();
        if writeln!(stdin, "{trimmed}").is_err() || stdin.flush().is_err() {
            diagnostics.push("guest stdin closed".into());
            break;
        }
        // One reply for that request.
        let reply = match wait_reply(&rx, opts.timeout, &mut diagnostics, &mut frames) {
            Some(v) => host_reply(v, &root.data, t0.elapsed().as_secs_f64() * 1000.0),
            None => break,
        };
        frame_seq = reply.frame_seq.unwrap_or(frame_seq);
        if reply.png.is_some() {
            frames += 1;
        }
        if reply.cmd == "close" {
            closed = true;
        }
        if !reply.ok {
            diagnostics.push(format!(
                "{}: {}",
                reply.rest.get("code").and_then(|c| c.as_str()).unwrap_or("ERROR"),
                reply.rest.get("error").and_then(|c| c.as_str()).unwrap_or("")
            ));
        }
        let mut v = serde_json::to_value(&reply).unwrap_or_default();
        add_derived(&mut v, &root.data);
        sink.line(&serde_json::to_string(&v).unwrap_or_default())?;
        // Bounded command history: a caller that drives a million commands gets
        // the newest replies, and the report says how many fell off (R1).
        if replies.len() >= MAX_REPLIES {
            let drop_n = MAX_REPLIES / 8;
            replies.drain(..drop_n);
            replies_dropped = replies_dropped.saturating_add(drop_n as u64);
        }
        replies.push(reply);
    }

    // --- shutdown: close the guest, collect everything
    let _ = writeln!(stdin, "{}", "{\"cmd\":\"close\"}");
    let _ = stdin.flush();
    drop(stdin);
    let status = wait_child(&mut child, Duration::from_secs(10));
    if status.is_none() {
        // A session that had to be killed must not leave its scope behind.
        cgroup.kill();
    }
    cgroup.sample();
    let batch = logd.finish_capped();
    let log_stats = batch.stats;
    let log_first_dropped = batch.first_dropped.clone();
    let _logs = batch.entries;
    let _ = proto.join();
    let stderr_tail = err_thread.join().unwrap_or_else(|_| TailBuffer::with_cap(cap));
    let stderr = stderr_tail.to_text();
    let mut other = TailBuffer::with_cap(cap);
    while let Ok(l) = rx.try_recv() {
        if let Line::Eof(tail) = l {
            other = tail;
        }
    }
    let stdout_lines: Vec<String> =
        other.to_text().lines().map(str::to_string).collect();
    let stdout = stdout_lines.join("\n");
    let wall_ms = started.elapsed().as_millis() as u64;

    // Keep both streams next to the frames: a guest that crashes mid-command is
    // diagnosed from its own words, and an ART thread dump (SIGQUIT) lands here.
    let _ = fs::write(sdir.join("guest-stdout.txt"), &stdout);
    // `guest-stderr.txt` was streamed (and capped) by the reader thread; only
    // rewrite it when that thread produced nothing on disk (e.g. it failed to
    // create the file).
    if !sdir.join("guest-stderr.txt").is_file() {
        let _ = fs::write(sdir.join("guest-stderr.txt"), &stderr);
    }
    if let Some(dir) = &opts.out {
        let _ = fs::create_dir_all(dir);
        // Mirror every frame the guest published (and its tree), so the caller
        // does not have to know the sandbox's /data layout.
        if let Ok(entries) = fs::read_dir(&out_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_file() {
                    let _ = fs::copy(&p, dir.join(p.file_name().unwrap_or_default()));
                }
            }
        }
        let _ = fs::write(
            dir.join("replies.json"),
            serde_json::to_vec_pretty(&replies).unwrap_or_default(),
        );
    }

    // The guest's own verdict is the authority on what it did (frames drawn,
    // outcome). A session that was never asked for a command is still a healthy
    // session, so "no command carried a png" is not a crash.
    let verdict = guest_verdict(&stdout_lines);
    if let Some(n) = verdict
        .as_ref()
        .and_then(|v| v.get("frames_drawn"))
        .and_then(|n| n.as_u64())
    {
        frames = frames.max(n);
    }
    let mut outcome = Outcome::Ok;
    let mut code = None;
    if let Some(v) = &verdict {
        match v.get("outcome").and_then(|o| o.as_str()) {
            Some("ok") | None => {}
            Some(other) => {
                outcome = Outcome::Crash;
                code = Some(
                    v.get("code")
                        .and_then(|c| c.as_str())
                        .unwrap_or(other)
                        .to_string(),
                );
            }
        }
    }
    if let Some(s) = status {
        if !s.success() && outcome == Outcome::Ok {
            outcome = Outcome::Crash;
            code = Some("GUEST_EXIT".into());
        }
    }
    if let Some((c, _)) = relay_error {
        outcome = Outcome::Crash;
        code = Some(c);
    }
    // A watchdog kill outranks every other reading: the host stopped the run for
    // its own memory safety, which is `HOST_MEMORY_LIMIT`, not a guest crash.
    if let Some(t) = dog.as_ref().and_then(|d| d.trip()) {
        outcome = Outcome::Unsupported;
        code = Some(HOST_MEMORY_LIMIT.to_string());
        let _ = t;
    }
    // A ready line with no published frame means the guest died before the
    // first draw; that is a crash, not a session.
    if frame_seq == 0 && outcome == Outcome::Ok {
        outcome = Outcome::Crash;
        code = Some("NO_FRAME".into());
    }
    diagnostics.extend(diagnostics_extra.drain(..));
    diagnostics.push(log_stats.summary());
    if let Some(e) = &log_first_dropped {
        diagnostics.push(format!(
            "logd: oldest entries dropped; first dropped was [{}] {}: {}",
            e.priority_name(),
            e.tag,
            crate::capture::head(&e.message, 200)
        ));
    }
    if let Some(d) = &dog {
        if let Some(t) = d.trip() {
            diagnostics.push(format!(
                "{HOST_MEMORY_LIMIT}: launcher RSS {} MiB exceeded the {} MiB ceiling \
                 (ACOS_ART_RSS_MAX); guest process group {:?} killed",
                t.rss_bytes / 1024 / 1024,
                t.limit_bytes / 1024 / 1024,
                t.killed_pgid
            ));
        }
    }
    // S5: the session's scope is its ceiling; an OOM kill is a host-resource
    // outcome, and the guest seccomp filter must prove it installed.
    diagnostics.push(cgroup.summary());
    if cgroup.oom_kills() > 0 {
        outcome = Outcome::Unsupported;
        code = Some(HOST_MEMORY_LIMIT.to_string());
        diagnostics.push(format!(
            "{HOST_MEMORY_LIMIT}: memcg OOM-killed the session ({} kill(s) in {})",
            cgroup.oom_kills(),
            cgroup.path().display()
        ));
    } else if dog.as_ref().and_then(|d| d.trip()).is_none() {
        let line = crate::seccomp::require_guard(run.path())?;
        diagnostics.push(format!("seccomp: {line}"));
    }
    diagnostics.push(format!(
        "guest dex: {} ({})",
        guest.dex_dir.display(),
        if guest.cached { "cached" } else { "built" }
    ));
    if replies_dropped > 0 {
        diagnostics.push(format!(
            "replies: kept the newest {} of {} ({} dropped, cap {MAX_REPLIES})",
            replies.len(),
            replies.len() as u64 + replies_dropped,
            replies_dropped
        ));
    }
    let peak = peak_rss_mb(&stdout_lines, &stderr);
    let _ = &shim;
    if let Some(p) = &socket_path {
        let _ = fs::remove_file(p);
    }
    // Scratch is scratch (H1's leak finding, S4): the per-run tree goes away on
    // every exit path, including a crashed guest — the artefacts the caller
    // asked for are copied to `--out` above, and the janitor is only for a
    // SIGKILLed launcher. `ACOS_ART_KEEP_SESSION=1` keeps it for diagnosis.
    if std::env::var_os("ACOS_ART_KEEP_SESSION").is_some() {
        run.keep();
    }

    Ok(SessionReport {
        outcome,
        code,
        session: session_id,
        apk: opts.apk.display().to_string(),
        package,
        activity,
        pid: Some(pid),
        width: opts.width,
        height: opts.panel_height,
        ready,
        commands: replies.len() as u64,
        frames,
        frame_seq,
        replies,
        wall_ms,
        peak_rss_mb: peak,
        socket: socket_path.map(|p| p.display().to_string()),
        stdout,
        stderr,
        diagnostics,
        notes,
    })
}

/// A line from the guest's stdout.
enum Line {
    Protocol(serde_json::Value),
    Other,
    /// End of the guest's stdout, with the bounded non-protocol tail it kept.
    Eof(TailBuffer),
}

/// Where a protocol reply goes: the caller's stdout, or its socket.
enum Sink {
    Stdout,
    Socket(UnixStream),
}

impl Sink {
    fn line(&mut self, text: &str) -> std::io::Result<()> {
        match self {
            Sink::Stdout => {
                let out = std::io::stdout();
                let mut lock = out.lock();
                writeln!(lock, "{text}")?;
                lock.flush()
            }
            Sink::Socket(stream) => {
                writeln!(stream, "{text}")?;
                stream.flush()
            }
        }
    }
}

/// Bind `path` and wait up to `timeout` for exactly one client. The socket file
/// stays on disk for the whole session (so a caller can poll for readiness) and
/// is removed on the way out.
fn bind_and_accept(path: &Path, timeout: Duration) -> Result<UnixStream> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    let _ = fs::remove_file(path);
    let listener = UnixListener::bind(path)
        .map_err(|e| Error::Tool(format!("bind {}: {e}", path.display())))?;
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let _ = fs::remove_file(path);
                    return Err(Error::Tool(format!(
                        "no client connected to {} within {timeout:?}",
                        path.display()
                    )));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                let _ = fs::remove_file(path);
                return Err(Error::Io(e));
            }
        }
    }
}

/// The guest's final `ACOSAPP {…}` verdict, if it wrote one.
fn guest_verdict(stdout_lines: &[String]) -> Option<serde_json::Value> {
    stdout_lines
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix(VERDICT_PREFIX))
        .and_then(|body| serde_json::from_str(body).ok())
}

fn wait_reply(
    rx: &mpsc::Receiver<Line>,
    timeout: Duration,
    diagnostics: &mut Vec<String>,
    _frames: &mut u64,
) -> Option<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            diagnostics.push("command timed out waiting for a reply".into());
            return None;
        }
        match rx.recv_timeout(left.min(Duration::from_millis(200))) {
            Ok(Line::Protocol(v)) => return Some(v),
            Ok(Line::Other) => {}
            Ok(Line::Eof(other)) => {
                let text = other.to_text();
                diagnostics.push(format!(
                    "guest stdout closed ({} bytes: {}): {}",
                    other.total(),
                    other.summary(),
                    text.lines().last().unwrap_or_default()
                ));
                return None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// The guest's reply → the caller's reply: guest paths become host paths and the
/// host-side round trip is recorded (`host_ms`) next to the guest's own
/// `guest_ms`.
fn host_reply(v: serde_json::Value, data: &Path, host_ms: f64) -> SessionReply {
    let mut v = v;
    let mut map = match v.as_object_mut() {
        Some(m) => std::mem::take(m),
        None => serde_json::Map::new(),
    };
    if let Some(serde_json::Value::String(p)) = map.get("png") {
        let host = host_path(data, p);
        map.insert(
            "png".into(),
            serde_json::Value::String(host.display().to_string()),
        );
    }
    map.insert(
        "host_ms".into(),
        serde_json::Value::from((host_ms * 1000.0).round() / 1000.0),
    );
    let value = serde_json::Value::Object(map);
    match serde_json::from_value(value.clone()) {
        Ok(r) => r,
        Err(_) => SessionReply {
            ok: value.get("ok").and_then(|b| b.as_bool()).unwrap_or(false),
            cmd: value
                .get("cmd")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string(),
            frame_seq: value.get("frame_seq").and_then(|s| s.as_u64()),
            png: value.get("png").and_then(|s| s.as_str()).map(str::to_string),
            md5: value.get("md5").and_then(|s| s.as_str()).map(str::to_string),
            host_ms: Some(host_ms),
            tree: value.get("tree").cloned(),
            rest: serde_json::Map::new(),
        },
    }
}

/// `png_bytes` / `png_exists`: derived facts the caller can trust without
/// re-reading the frame (still verifiable from disk).
fn add_derived(v: &mut serde_json::Value, _data: &Path) {
    if let Some(serde_json::Value::String(p)) = v.get("png").cloned() {
        let path = PathBuf::from(&p);
        let meta = fs::metadata(&path).ok();
        v["png_bytes"] = serde_json::Value::from(meta.as_ref().map(|m| m.len()).unwrap_or(0));
        v["png_exists"] = serde_json::Value::from(meta.is_some());
    }
}

/// Text of every `TextView`-ish node in a reply's tree (so a caller can assert
/// state without re-implementing the walk). Order is the tree's own order.
fn tree_texts(reply: &SessionReply) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        if let Some(nodes) = v.get("nodes").and_then(|n| n.as_array()) {
            for n in nodes {
                walk(n, out);
            }
        }
        if let Some(children) = v.get("children").and_then(|c| c.as_array()) {
            for c in children {
                walk(c, out);
            }
        }
        if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
            if !t.is_empty() {
                out.push(t.to_string());
            }
        }
    }
    if let Some(tree) = &reply.tree {
        walk(tree, &mut out);
    }
    out
}

fn wait_child(child: &mut Child, grace: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(child.id() as i32, libc::SIGKILL);
            }
            return child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Peak RSS of the guest, read from the guest's own `/proc/<pid>/status`? The
/// sandbox does not expose it, so the guest reports `VmHWM` in its verdict
/// (`peak_rss_kb`) — read it from the ACOSAPP line.
fn peak_rss_mb(stdout_lines: &[String], stderr: &str) -> Option<f64> {
    let _ = stderr;
    guest_verdict(stdout_lines)
        .as_ref()
        .and_then(|v| v.get("peak_rss_kb"))
        .and_then(|k| k.as_u64())
        .map(|kb| kb as f64 / 1024.0)
}

/// One-shot CLI helper for `acos-art session --apk X`: run the relay against the
/// real process stdio and report what happened on stderr.
pub fn session_cli(root: &ArtRoot, opts: &SessionOpts) -> Result<i32> {
    let report = session(root, opts)?;
    eprintln!(
        "acos-art: session={} commands={} frames={} frame_seq={} wall={}ms peak_rss={}MB outcome={:?}",
        report.session,
        report.commands,
        report.frames,
        report.frame_seq,
        report.wall_ms,
        report
            .peak_rss_mb
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "-".into()),
        report.outcome
    );
    for d in &report.diagnostics {
        eprintln!("acos-art: {d}");
    }
    if !report.succeeded() {
        // Nothing about a failed session may be swallowed: the guest's own
        // words are the diagnosis.
        for l in report.stdout.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev() {
            eprintln!("guest stdout: {l}");
        }
        for l in report.stderr.lines().rev().take(24).collect::<Vec<_>>().into_iter().rev() {
            eprintln!("guest stderr: {l}");
        }
    }
    Ok(if report.succeeded() { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_line_parses_only_with_the_prefix() {
        assert!(parse_protocol_line("ACOSCMD {\"ok\":true,\"cmd\":\"tree\"}").is_some());
        assert!(parse_protocol_line("ACOSAPP {\"outcome\":\"ok\"}").is_none());
        assert!(parse_protocol_line("\tat dev.acos.art.SessionMain.tap").is_none());
        let v = parse_protocol_line("ACOSCMD {\"ok\":true,\"cmd\":\"tap\",\"frame_seq\":3}\r\n").unwrap();
        assert_eq!(v["cmd"], "tap");
        assert_eq!(v["frame_seq"], 3);
    }

    #[test]
    fn guest_paths_map_onto_the_data_dir() {
        let data = Path::new("/home/x/.cache/art/data");
        assert_eq!(
            host_path(data, "/data/session/s1234/out/frame_2.png"),
            data.join("session/s1234/out/frame_2.png")
        );
        // Anything not under /data is left alone (never silently rewritten).
        assert_eq!(host_path(data, "/system/fonts/Roboto.ttf"), Path::new("/system/fonts/Roboto.ttf"));
    }

    #[test]
    fn argv_order_is_the_guest_contract() {
        let a = guest_argv(
            "/data/app/counter.apk", "dev.acos.fixtures.counter", "/data/session/s1/out",
            1080, 2062, 420, "/data/session/s1/overlay/a.idmap", 2400, 128, 63,
            "0", "Counter", 34, "dev.acos.fixtures.counter.MainActivity", "s1",
        );
        assert_eq!(a.len(), 15);
        assert_eq!(a[0], "/data/app/counter.apk");
        assert_eq!(a[6], "/data/session/s1/overlay/a.idmap");
        assert_eq!(a[7], "2400");
        assert_eq!(a[13], "dev.acos.fixtures.counter.MainActivity");
        assert_eq!(a[14], "s1");
    }

    #[test]
    fn reply_maps_png_and_keeps_the_tree() {
        let data = Path::new("/d");
        let v: serde_json::Value = serde_json::from_str(
            r#"{"ok":true,"cmd":"tap","frame_seq":4,"png":"/data/session/s/out/frame_4.png",
                "md5":"ab","tree":{"bounds":"[0,0][1080,2400]","nodes":[{"text":"1"}]},
                "tap":{"handled":true}}"#,
        )
        .unwrap();
        let r = host_reply(v, data, 12.5);
        assert!(r.ok);
        assert_eq!(r.cmd, "tap");
        assert_eq!(r.frame_seq, Some(4));
        assert_eq!(r.png.as_deref(), Some("/d/session/s/out/frame_4.png"));
        assert_eq!(r.md5.as_deref(), Some("ab"));
        assert_eq!(r.host_ms, Some(12.5));
        assert_eq!(r.rest["tap"]["handled"], true);
        assert_eq!(tree_texts(&r), vec!["1".to_string()]);
    }

    #[test]
    fn peak_rss_comes_from_the_verdict() {
        let lines = vec![
            "some app output".to_string(),
            "ACOSAPP {\"outcome\":\"ok\",\"peak_rss_kb\":215040}".to_string(),
        ];
        assert_eq!(peak_rss_mb(&lines, ""), Some(210.0));
        assert_eq!(peak_rss_mb(&[], ""), None);
    }
}
