//! The real-ART engine: one `acos-art session` child, driven by its line-JSON
//! protocol.
//!
//! Protocol (from `crates/acos-art/src/session.rs`, P3d):
//!
//! ```text
//! caller  ->  {"cmd":"tree"}                  reply: tree + frame_seq + png + md5
//! caller  ->  {"cmd":"screenshot"}            reply: png + md5 + frame_seq
//! caller  ->  {"cmd":"tap","x":..,"y":..}     reply: tree + frame_seq + png + md5
//! caller  ->  {"cmd":"key","code":..}         reply: frame_seq (+png when it changed)
//! caller  ->  {"cmd":"reset"}                 reply: frame_seq (fresh Activity)
//! caller  ->  {"cmd":"close"}                 reply: ok, then exit
//! ```
//!
//! The first stdout line is the session's ready reply. Every line is bounded;
//! reads have the session timeout; the child is spawned in its own process group
//! and reaped on drop, so a wedged ART cannot leave orphans behind.
//!
//! The protocol has **no** `type` (text input), no `rotate` and no log command.
//! [`SessionCapabilities`] says so, and a scenario step that needs one fails with
//! `UNSUPPORTED_BACKEND` naming it. Nothing here renders offline: this engine is
//! [`Mode::Live`]; if the binary, the sysroot or a command is unavailable the
//! caller gets a structured error, never a preview pass.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acos_protocol::ui::UiTree;
use acos_protocol::{FrameMeta, Selector};
use acos_verify::{BackendError, LaunchOutcome};

use super::{
    key_code, ui_tree_from_json, Engine, Mode, ObservedFrame, SessionBackend, SessionCapabilities,
};

/// Largest single protocol line accepted (a full View tree fits easily).
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// Largest PNG accepted from one reply.
const MAX_PNG_BYTES: u64 = 64 * 1024 * 1024;

/// One ART session.
pub struct ArtSession {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    lines: Receiver<Result<String, String>>,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    timeout: Duration,
    /// The APK this session was started for (a session serves one app).
    apk: PathBuf,
    package: String,
    activity: Option<String>,
    width: u32,
    height: u32,
    density: u32,
    /// The newest frame seen, with the tree and PNG that belong to it.
    cached: Option<CachedFrame>,
    closed: bool,
}

struct CachedFrame {
    seq: u64,
    md5: String,
    png: Vec<u8>,
    tree: UiTree,
}

impl ArtSession {
    /// Spawn `acos-art session --apk <apk>` and wait for its ready line.
    ///
    /// `artifacts` is where the engine mirrors captured frames. The binary is
    /// `$ACOS_ART_BIN`, else `acos-art` on `PATH`, else `target/{debug,release}/
    /// acos-art` next to the running `acos` binary.
    pub fn start(apk: &Path, artifacts: &Path) -> Result<ArtSession, BackendError> {
        if !apk.is_file() {
            return Err(BackendError::Failed {
                code: acos_protocol::ErrorCode::ApkIo,
                detail: format!("APK not found: {}", apk.display()),
            });
        }
        let binary = resolve_binary()?;
        let out = artifacts.join("art-frames");
        std::fs::create_dir_all(&out).map_err(|error| BackendError::Error {
            code: acos_protocol::ErrorCode::RenderIo,
            detail: format!("cannot create {}: {error}", out.display()),
        })?;
        let timeout = Duration::from_secs(
            std::env::var("ACOS_ART_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(120),
        );

        let mut command = Command::new(&binary);
        command
            .arg("session")
            .arg("--apk")
            .arg(apk)
            .arg("--out")
            .arg(&out)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Own process group: a `close` that does not take must not leave ART or
        // its sandbox behind.
        command.process_group(0);
        let mut child = command.spawn().map_err(|error| BackendError::Unsupported {
            capability: "engine.art".into(),
            detail: format!(
                "cannot spawn {} ({}): {error}. Install/build acos-art or set ACOS_ART_BIN",
                binary.display(),
                "the ART session engine is unavailable on this host"
            ),
        })?;
        let _pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match reader.by_ref().take(MAX_LINE_BYTES as u64).read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if line.len() > MAX_LINE_BYTES {
                            let _ = tx.send(Err(format!(
                                "protocol line exceeds {MAX_LINE_BYTES} bytes"
                            )));
                            break;
                        }
                        let text = String::from_utf8_lossy(&line).trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        if tx.send(Ok(text)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(format!("reading acos-art stdout: {error}")));
                        break;
                    }
                }
            }
        });
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail = Arc::clone(&stderr_tail);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                        tail.extend(line.bytes());
                        while tail.len() > 8192 {
                            tail.pop_front();
                        }
                    }
                }
            }
        });

        let mut session = ArtSession {
            child: Some(child),
            stdin,
            lines,
            stderr_tail,
            timeout,
            apk: apk.to_path_buf(),
            package: String::new(),
            activity: None,
            width: 1080,
            height: 2400,
            density: 420,
            cached: None,
            closed: false,
        };
        // The ready line is the protocol's first reply.
        let ready = session.read_line(Duration::from_secs(90), "startup")?;
        let ready: serde_json::Value = serde_json::from_str(&ready).map_err(|error| {
            BackendError::Error {
                code: acos_protocol::ErrorCode::Internal,
                detail: format!("acos-art ready line is not JSON: {error}"),
            }
        })?;
        if ready.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let code = ready
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("STARTUP_FAILED");
            let message = ready
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("acos-art session did not start");
            return Err(BackendError::Unsupported {
                capability: "engine.art.session".into(),
                detail: format!("{code}: {message}; stderr tail: {}", session.stderr_tail()),
            });
        }
        if let Some(width) = ready.get("width").and_then(|v| v.as_u64()) {
            session.width = width as u32;
        }
        if let Some(height) = ready.get("height").and_then(|v| v.as_u64()) {
            session.height = height as u32;
        }
        if let Some(package) = ready.get("package").and_then(|v| v.as_str()) {
            session.package = package.to_string();
        }
        if let Some(activity) = ready.get("activity").and_then(|v| v.as_str()) {
            session.activity = Some(activity.to_string());
        }
        Ok(session)
    }

    fn stderr_tail(&self) -> String {
        let tail = self.stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
        let bytes: Vec<u8> = tail.iter().copied().collect();
        String::from_utf8_lossy(&bytes).trim().to_string()
    }

    fn read_line(&mut self, timeout: Duration, context: &str) -> Result<String, BackendError> {
        match self.lines.recv_timeout(timeout) {
            Ok(Ok(line)) => Ok(line),
            Ok(Err(error)) => Err(BackendError::Error {
                code: acos_protocol::ErrorCode::Internal,
                detail: format!("{context}: {error}; stderr tail: {}", self.stderr_tail()),
            }),
            Err(RecvTimeoutError::Timeout) => Err(BackendError::Timeout {
                detail: format!(
                    "{context}: no reply from acos-art within {}s; stderr tail: {}",
                    timeout.as_secs(),
                    self.stderr_tail()
                ),
            }),
            Err(RecvTimeoutError::Disconnected) => Err(BackendError::Error {
                code: acos_protocol::ErrorCode::Internal,
                detail: format!(
                    "{context}: acos-art session exited; stderr tail: {}",
                    self.stderr_tail()
                ),
            }),
        }
    }

    fn command(&mut self, request: serde_json::Value) -> Result<serde_json::Value, BackendError> {
        let stdin = self.stdin.as_mut().ok_or_else(|| BackendError::Error {
            code: acos_protocol::ErrorCode::Internal,
            detail: "the ART session stdin is closed".to_string(),
        })?;
        let line = request.to_string();
        writeln!(stdin, "{line}").and_then(|_| stdin.flush()).map_err(|error| {
            BackendError::Error {
                code: acos_protocol::ErrorCode::Internal,
                detail: format!("cannot write to acos-art: {error}"),
            }
        })?;
        let timeout = self.timeout;
        let reply = self.read_line(timeout, "command")?;
        let reply: serde_json::Value =
            serde_json::from_str(&reply).map_err(|error| BackendError::Error {
                code: acos_protocol::ErrorCode::Internal,
                detail: format!("acos-art reply is not JSON: {error}"),
            })?;
        if reply.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let code = reply
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("COMMAND_FAILED");
            let message = reply
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("acos-art session command failed");
            return Err(BackendError::Error {
                code: acos_protocol::ErrorCode::from_str_opt(code)
                    .unwrap_or(acos_protocol::ErrorCode::Internal),
                detail: format!("acos-art: {code}: {message}"),
            });
        }
        Ok(reply)
    }

    fn frame_meta(&self, seq: u64, md5: &str) -> FrameMeta {
        FrameMeta::new(
            seq,
            acos_verify::clock::rfc3339_from_unix_ms(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            ),
            "art",
            self.width,
            self.height,
            self.density,
            md5,
            true,
        )
    }

    /// Absorb a reply's frame: sequence, PNG bytes and tree, all or nothing.
    ///
    /// A reply that carries a `frame_seq` also carries that frame's PNG; a tree
    /// reply carries the tree as well. A frame with no tree is not cached as an
    /// observation (it would bind an older tree to newer pixels).
    fn absorb(&mut self, reply: &serde_json::Value) -> Result<(), BackendError> {
        let seq = reply
            .get("frame_seq")
            .and_then(|v| v.as_u64())
            .or_else(|| self.cached.as_ref().map(|frame| frame.seq))
            .unwrap_or(0);
        let md5 = reply
            .get("md5")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let png = match reply.get("png").and_then(|v| v.as_str()) {
            Some(path) => read_png(Path::new(path))?,
            None => Vec::new(),
        };
        let Some(tree_value) = reply.get("tree") else {
            // No tree in this reply (for example `key`); keep the cached tree
            // only for the frame it was bound to. A newer frame without a tree
            // is not turned into an observation.
            if let Some(cached) = self.cached.as_mut() {
                if cached.seq == seq && !png.is_empty() {
                    cached.png = png;
                }
            }
            return Ok(());
        };
        let tree = ui_tree_from_json(&self.package, self.frame_meta(seq, &md5), tree_value)?;
        self.cached = Some(CachedFrame {
            seq,
            md5,
            png,
            tree,
        });
        Ok(())
    }

    /// Ask for the live tree. The `tree` command re-traverses, draws and
    /// publishes a new frame, so the reply's tree and PNG share one `frame_seq`.
    fn request_tree(&mut self) -> Result<(), BackendError> {
        let reply = self.command(serde_json::json!({"cmd":"tree"}))?;
        self.absorb(&reply)
    }

    fn require_cached_tree(&mut self) -> Result<UiTree, BackendError> {
        if self.cached.is_none() {
            self.request_tree()?;
        }
        self.cached
            .as_ref()
            .map(|frame| frame.tree.clone())
            .ok_or_else(|| BackendError::Unsupported {
                capability: "ui.tree".into(),
                detail: "the ART session has not produced a tree yet".into(),
            })
    }

    fn kill_group(&mut self) {
        if let Some(mut child) = self.child.take() {
            let pid = child.id() as i32;
            let _ = child.kill();
            // SAFETY: `pid` is the direct child's pid and the child was spawned
            // into its own process group (`process_group(0)`), so `-pid` names
            // that group and no other process.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            let _ = child.wait();
        }
        self.stdin = None;
    }
}

impl SessionBackend for ArtSession {
    fn engine(&self) -> Engine {
        Engine::Art
    }

    fn mode(&self) -> Mode {
        // This engine launches a real Activity; `render-app` (offline preview)
        // is a different command and never reaches this adapter.
        Mode::Live
    }

    fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            launch: true,
            tree: true,
            screenshot: true,
            tap: true,
            // Host-side: resolve the node's text in the tree, tap its centre.
            tap_text: true,
            // The protocol has no text-input command yet.
            type_text: false,
            key: true,
            reset: true,
            close: true,
            logs: false,
            environment: false,
            crash_reports: false,
        }
    }

    fn launch(&mut self, _apk: &Path) -> Result<LaunchOutcome, BackendError> {
        let reply = self.command(serde_json::json!({"cmd":"tree"}))?;
        self.absorb(&reply)?;
        let cached = self.cached.as_ref().ok_or_else(|| BackendError::Error {
            code: acos_protocol::ErrorCode::Internal,
            detail: "the ART session started but produced no frame".to_string(),
        })?;
        Ok(LaunchOutcome {
            session_id: format!("s_art_{}", self.apk.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()),
            package: self.package.clone(),
            frame: Some(self.frame_meta(cached.seq, &cached.md5)),
            screenshot_png: Some(cached.png.clone()),
            crashed: None,
            detail: serde_json::json!({
                "engine": "art",
                "mode": "live",
                "activity": self.activity,
                "apk": self.apk.display().to_string(),
            }),
        })
    }

    fn tree(&mut self) -> Result<UiTree, BackendError> {
        self.request_tree()?;
        self.require_cached_tree()
    }

    fn frame(&mut self) -> Result<ObservedFrame, BackendError> {
        self.tree()?;
        let png = self
            .cached
            .as_ref()
            .map(|frame| frame.png.clone())
            .filter(|png| !png.is_empty())
            .ok_or_else(|| BackendError::Unsupported {
                capability: "screenshot".into(),
                detail: "the ART tree reply carried no PNG".into(),
            })?;
        let tree = self.require_cached_tree()?;
        Ok(ObservedFrame { tree, png })
    }

    fn capture(&mut self) -> Result<ObservedFrame, BackendError> {
        // `tree` re-traverses and publishes a new frame; its reply carries both
        // the tree and the PNG of that frame.
        self.frame()
    }

    fn screenshot(&mut self) -> Result<Vec<u8>, BackendError> {
        if self.cached.is_none() {
            self.request_tree()?;
        }
        self.cached
            .as_ref()
            .map(|frame| frame.png.clone())
            .filter(|png| !png.is_empty())
            .ok_or_else(|| BackendError::Unsupported {
                capability: "screenshot".into(),
                detail: "the ART session reply carried no PNG".into(),
            })
    }

    fn tap(&mut self, x: i32, y: i32) -> Result<(), BackendError> {
        let reply = self.command(serde_json::json!({"cmd":"tap","x":x,"y":y}))?;
        self.absorb(&reply)
    }

    fn tap_text(&mut self, text: &str) -> Result<(), BackendError> {
        let tree = self.require_cached_tree()?;
        let node = tree
            .find_one(&Selector::by_text(text))
            .ok_or_else(|| BackendError::Failed {
                code: acos_protocol::ErrorCode::AssertionFailed,
                detail: format!("no node with text {text:?} in the live ART tree"),
            })?
            .clone();
        let bounds = node.bounds;
        self.tap(
            (bounds.left() + bounds.right()) / 2,
            (bounds.top() + bounds.bottom()) / 2,
        )
    }

    fn type_text(&mut self, _text: &str) -> Result<(), BackendError> {
        Err(BackendError::unsupported(
            "input.type_text",
            "the ART session protocol has no text-input command yet (`type_text`)",
        ))
    }

    fn key(&mut self, key: &str) -> Result<(), BackendError> {
        let code = key_code(key);
        if code < 0 {
            return Err(BackendError::unsupported(
                "input.key",
                format!("unknown symbolic key {key:?}"),
            ));
        }
        let reply = self.command(serde_json::json!({"cmd":"key","code":code}))?;
        self.absorb(&reply)
    }

    fn reset(&mut self) -> Result<(), BackendError> {
        let reply = self.command(serde_json::json!({"cmd":"reset"}))?;
        self.absorb(&reply)
    }

    fn close(&mut self) -> Result<(), BackendError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "{}", serde_json::json!({"cmd":"close"}));
            let _ = stdin.flush();
        }
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        if let Some(child) = self.child.as_mut() {
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        self.child = None;
                        return Ok(());
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
        }
        self.kill_group();
        Ok(())
    }
}

impl Drop for ArtSession {
    fn drop(&mut self) {
        let _ = self.close();
        self.kill_group();
    }
}

impl std::fmt::Debug for ArtSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtSession")
            .field("apk", &self.apk)
            .field("package", &self.package)
            .field("closed", &self.closed)
            .finish()
    }
}

fn resolve_binary() -> Result<PathBuf, BackendError> {
    if let Ok(value) = std::env::var("ACOS_ART_BIN") {
        let path = PathBuf::from(&value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(BackendError::Unsupported {
            capability: "engine.art.binary".into(),
            detail: format!("ACOS_ART_BIN={value} is not a file"),
        });
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [dir.join("acos-art"), dir.join("../acos-art")] {
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    // Last resort: PATH lookup by spawning with a bare name. `Command` resolves
    // it; `start` turns a missing binary into `UNSUPPORTED_BACKEND`.
    Ok(PathBuf::from("acos-art"))
}

fn read_png(path: &Path) -> Result<Vec<u8>, BackendError> {
    let file = std::fs::File::open(path).map_err(|error| BackendError::Error {
        code: acos_protocol::ErrorCode::RenderIo,
        detail: format!("cannot open frame {}: {error}", path.display()),
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_PNG_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| BackendError::Error {
            code: acos_protocol::ErrorCode::RenderIo,
            detail: format!("cannot read frame {}: {error}", path.display()),
        })?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_capabilities_name_the_protocol_gaps() {
        let caps = SessionCapabilities {
            launch: true,
            tree: true,
            screenshot: true,
            tap: true,
            tap_text: true,
            type_text: false,
            key: true,
            reset: true,
            close: true,
            logs: false,
            environment: false,
            crash_reports: false,
        };
        assert_eq!(
            caps.missing(),
            vec!["type_text", "logs", "environment", "crash_reports"]
        );
    }

    #[test]
    fn keycodes_cover_the_art_protocol() {
        assert_eq!(key_code("back"), 4);
        assert_eq!(key_code("enter"), 66);
    }
}
