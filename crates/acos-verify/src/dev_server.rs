//! METRO3: the host dev server (Metro) a scenario's `dev_server` steps drive.
//!
//! The executor **owns** the server: it copies the project into run scratch,
//! starts Metro there, edits files of the *copy* (never the original tree),
//! asks it what it serves, and stops it — on `dev_server: {op: stop}`, at the end
//! of the scenario, and from `Drop` when the run unwinds, so a failed step
//! cannot leave a Metro behind.
//!
//! Nothing here fakes a result: an edit whose `find` text is absent fails, a
//! bundle that never contains the expected string times out with the last
//! observation, and a reload only reports that Metro accepted the broadcast.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The bundle URL React Native's debug app asks for (`DevServerHelper`), so the
/// "does Metro serve my edit" probe builds the same bundle the guest fetches.
const BUNDLE_PATH: &str =
    "/index.bundle?platform=android&dev=true&minify=false&app=rnhello&modulesOnly=false&runModule=true";

/// What the host provides: `node`, and a `node_modules` holding Metro + React Native.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetroEnvironment {
    /// The `node` binary found on `PATH`.
    pub node: PathBuf,
    /// `node --version` output.
    pub node_version: String,
    /// A directory containing `react-native/cli.js` and `metro/`.
    pub node_modules: PathBuf,
}

/// Directory that holds the React Native `node_modules`, in lookup order:
/// `$ACOS_METRO_NODE_MODULES`, then the checkout's `.cache/rn-0.81.4/jsproject/node_modules`
/// (working directory, or its ancestors).
pub fn default_node_modules() -> Option<PathBuf> {
    let looks_right = |dir: &Path| {
        dir.join("react-native").join("cli.js").is_file() && dir.join("metro").is_dir()
    };
    if let Some(dir) = std::env::var_os("ACOS_METRO_NODE_MODULES") {
        let dir = PathBuf::from(dir);
        return looks_right(&dir).then_some(dir);
    }
    let mut here = std::env::current_dir().ok();
    while let Some(dir) = here {
        let candidate = dir
            .join(".cache")
            .join("rn-0.81.4")
            .join("jsproject")
            .join("node_modules");
        if looks_right(&candidate) {
            return Some(candidate);
        }
        here = dir.parent().map(Path::to_path_buf);
    }
    None
}

/// The one rule for "this host can run a `needs_env: [metro]` scenario" (used by
/// `acos doctor`, which `tools/run-matrix.sh` asks, and by `dev_server start`).
pub fn metro_environment() -> Result<MetroEnvironment, String> {
    let output = Command::new("node")
        .arg("--version")
        .output()
        .map_err(|error| format!("`node` is not runnable ({error}); install Node.js"))?;
    if !output.status.success() {
        return Err("`node --version` failed".to_string());
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let node = which("node").unwrap_or_else(|| PathBuf::from("node"));
    let node_modules = default_node_modules().ok_or_else(|| {
        "no React Native node_modules with Metro (set ACOS_METRO_NODE_MODULES, or run `tools/build-rn-hello.sh fetch` for .cache/rn-0.81.4)".to_string()
    })?;
    Ok(MetroEnvironment {
        node,
        node_version: version,
        node_modules,
    })
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// A step-level failure: a message plus structured detail for the verdict.
#[derive(Debug, Clone)]
pub struct DevServerError {
    /// One-line reason.
    pub message: String,
    /// Evidence (last reply, log tail, ...).
    pub details: Value,
    /// The environment cannot do this at all (a skipped capability, not a failed check).
    pub unsupported: bool,
}

impl DevServerError {
    fn fail(message: impl Into<String>, details: Value) -> Self {
        Self {
            message: message.into(),
            details,
            unsupported: false,
        }
    }
}

type Outcome = Result<Value, DevServerError>;

/// The running dev server, if a `start` step ran.
#[derive(Default)]
pub struct DevServerHost {
    inner: Option<Running>,
    /// Where the Metro log of the last server was saved (for the verdict).
    saved_log: Option<PathBuf>,
    /// Where to keep the Metro log when the server stops.
    log_dir: Option<PathBuf>,
    /// The `replace`/`contents` text of the latest edit (what `fast_refresh` waits for).
    last_edit_needle: Option<String>,
}

struct Running {
    child: Child,
    port: u16,
    root: PathBuf,
    project: PathBuf,
    log: PathBuf,
}

impl DevServerHost {
    /// Keep the Metro log under `dir` when the server stops.
    pub fn set_log_dir(&mut self, dir: PathBuf) {
        self.log_dir = Some(dir);
    }

    /// Whether a server is running.
    pub fn is_running(&self) -> bool {
        self.inner.is_some()
    }

    /// The port of the running server.
    pub fn port(&self) -> Option<u16> {
        self.inner.as_ref().map(|running| running.port)
    }

    /// Where `stop` saved Metro's log (relative reporting is the caller's job).
    pub fn saved_log(&self) -> Option<&Path> {
        self.saved_log.as_deref()
    }

    /// `start`: copy the project, start Metro, wait for `/status`.
    pub fn start(
        &mut self,
        project: &Path,
        node_modules: Option<&Path>,
        port: u16,
        timeout: Duration,
    ) -> Outcome {
        if self.inner.is_some() {
            return Err(DevServerError::fail(
                "a dev server is already running",
                Value::Null,
            ));
        }
        let env = metro_environment().map_err(|message| DevServerError {
            message,
            details: Value::Null,
            unsupported: true,
        })?;
        let node_modules = node_modules.map(Path::to_path_buf).unwrap_or(env.node_modules);
        if !node_modules.join("react-native").join("cli.js").is_file() {
            return Err(DevServerError::fail(
                format!("{} has no react-native/cli.js", node_modules.display()),
                Value::Null,
            ));
        }
        if !project.is_dir() {
            return Err(DevServerError::fail(
                format!("project directory {} does not exist", project.display()),
                Value::Null,
            ));
        }
        // Never adopt somebody else's Metro: an owned server is the only one whose
        // edits and lifetime this run controls.
        if http_get(port, "/status", Duration::from_secs(2)).is_ok() {
            return Err(DevServerError::fail(
                format!("port {port} already serves /status; refusing to drive a Metro this run does not own"),
                json!({"port": port}),
            ));
        }
        let scratch_base = std::env::var_os("ACOS_DEV_SERVER_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = scratch_base.join(format!("acos-dev-server-{}-{port}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let copy = root.join("project");
        fs::create_dir_all(&copy).map_err(io_error("create scratch"))?;
        copy_tree(project, &copy, false).map_err(io_error("copy the project"))?;
        // `node_modules` is hard-linked (no 200 MB copy); an edit never goes through it
        // (`edit` refuses node_modules and writes by rename, which breaks links).
        let nm_copy = copy.join("node_modules");
        let _ = fs::remove_dir_all(&nm_copy);
        copy_tree(&node_modules, &nm_copy, true).map_err(io_error("link node_modules"))?;

        let log = root.join("metro.log");
        let log_file = fs::File::create(&log).map_err(io_error("create the Metro log"))?;
        let err_file = log_file.try_clone().map_err(io_error("clone the log"))?;
        let mut command = Command::new("node");
        command
            .arg("node_modules/react-native/cli.js")
            .arg("start")
            .arg("--port")
            .arg(port.to_string())
            .arg("--no-interactive")
            .current_dir(&copy)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(err_file));
        // Its own process group: `stop` signals the whole group (Metro spawns workers).
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let child = command
            .spawn()
            .map_err(io_error("spawn `node ... react-native start`"))?;
        let mut running = Running {
            child,
            port,
            root,
            project: copy,
            log,
        };
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = running.child.try_wait() {
                let tail = log_tail(&running.log);
                let _ = running.stop(self.log_dir.as_deref());
                return Err(DevServerError::fail(
                    format!("Metro exited during start ({status})"),
                    json!({"log_tail": tail}),
                ));
            }
            if let Ok(body) = http_get(port, "/status", Duration::from_secs(2)) {
                if String::from_utf8_lossy(&body).contains("packager-status:running") {
                    break;
                }
            }
            if Instant::now() >= deadline {
                let tail = log_tail(&running.log);
                let _ = running.stop(self.log_dir.as_deref());
                return Err(DevServerError::fail(
                    format!("Metro did not answer /status within {} ms", timeout.as_millis()),
                    json!({"log_tail": tail}),
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let reply = json!({
            "port": port,
            "pid": running.child.id(),
            "project_copy": running.project.display().to_string(),
            "node": env.node_version,
            "node_modules": node_modules.display().to_string(),
        });
        self.inner = Some(running);
        self.last_edit_needle = None;
        Ok(reply)
    }

    /// `edit`: change a file of the project copy.
    pub fn edit(
        &mut self,
        file: &str,
        find: Option<&str>,
        replace: Option<&str>,
        contents: Option<&str>,
    ) -> Outcome {
        let running = self.running()?;
        let target = running.project.join(file);
        // Defence in depth: validation rejects these, the runtime does not trust it.
        let relative = Path::new(file);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir))
            || relative.components().next().is_some_and(
                |c| matches!(c, std::path::Component::Normal(n) if n == "node_modules"),
            )
        {
            return Err(DevServerError::fail(
                format!("`{file}` is not an editable file of the project copy"),
                Value::Null,
            ));
        }
        let (new_text, replaced) = match (find, replace, contents) {
            (Some(find), Some(replace), None) => {
                let old = fs::read_to_string(&target)
                    .map_err(io_error(&format!("read {}", target.display())))?;
                let count = old.matches(find).count();
                if count == 0 {
                    return Err(DevServerError::fail(
                        format!("`find` text not present in {file}; nothing was edited"),
                        json!({"file": file, "find": find}),
                    ));
                }
                (old.replace(find, replace), count)
            }
            (None, None, Some(contents)) => (contents.to_string(), 1),
            _ => {
                return Err(DevServerError::fail(
                    "edit needs find+replace or contents",
                    Value::Null,
                ))
            }
        };
        // Write beside the target, then rename: a rename never writes through a
        // hard link, so the shared source tree stays untouched.
        let staging = target.with_extension("acos-edit.tmp");
        fs::write(&staging, &new_text).map_err(io_error("write the edit"))?;
        fs::rename(&staging, &target).map_err(io_error("move the edit into place"))?;
        self.last_edit_needle = replace.or(contents).map(str::to_string);
        Ok(json!({"file": file, "replacements": replaced, "bytes": new_text.len()}))
    }

    /// `wait_serves`: poll the bundle until it contains/omits the given text.
    pub fn wait_serves(
        &mut self,
        contains: Option<&str>,
        absent: Option<&str>,
        timeout: Duration,
    ) -> Outcome {
        let port = self.running()?.port;
        let started = Instant::now();
        let deadline = started + timeout;
        let mut attempts = 0u32;
        let mut last: Value;
        loop {
            attempts += 1;
            let remaining = deadline.saturating_duration_since(Instant::now());
            match http_get(port, BUNDLE_PATH, remaining.max(Duration::from_secs(1))) {
                Ok(body) => {
                    let text = String::from_utf8_lossy(&body);
                    let has = contains.map(|needle| text.contains(needle));
                    let lacks = absent.map(|needle| !text.contains(needle));
                    last = json!({
                        "bytes": body.len(),
                        "contains": has,
                        "absent": lacks,
                    });
                    if has.unwrap_or(true) && lacks.unwrap_or(true) {
                        return Ok(json!({
                            "bundle_bytes": body.len(),
                            "contains": contains,
                            "absent": absent,
                            "attempts": attempts,
                            "waited_ms": started.elapsed().as_millis() as u64,
                        }));
                    }
                }
                Err(error) => last = json!({"error": error}),
            }
            if Instant::now() >= deadline {
                return Err(DevServerError::fail(
                    format!(
                        "the served bundle did not reach the expected content within {} ms",
                        timeout.as_millis()
                    ),
                    json!({"last": last, "contains": contains, "absent": absent, "attempts": attempts}),
                ));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// `fast_refresh`: wait until the latest edit is what Metro serves. The update
    /// reaches the app over `/hot` by itself; no reload is sent.
    pub fn fast_refresh(&mut self, timeout: Duration) -> Outcome {
        let needle = self.last_edit_needle.clone().ok_or_else(|| {
            DevServerError::fail(
                "fast_refresh has nothing to deliver: no `edit` step ran before it",
                Value::Null,
            )
        })?;
        let mut reply = self.wait_serves(Some(&needle), None, timeout)?;
        if let Value::Object(map) = &mut reply {
            map.insert("mode".into(), json!("fast-refresh"));
            map.insert("reload_sent".into(), json!(false));
            map.insert("transport".into(), json!("metro /hot websocket (file watcher)"));
        }
        Ok(reply)
    }

    /// `reload`: `POST /reload`, the broadcast the CLI's `r` key sends.
    pub fn reload(&mut self) -> Outcome {
        let port = self.running()?.port;
        let body = http_request(port, "POST", "/reload", Duration::from_secs(10))
            .map_err(|error| DevServerError::fail(format!("POST /reload failed: {error}"), Value::Null))?;
        Ok(json!({
            "mode": "reload",
            "reload_sent": true,
            "response": String::from_utf8_lossy(&body).trim().to_string(),
        }))
    }

    /// `stop`: stop Metro and delete the project copy.
    pub fn stop(&mut self) -> Outcome {
        let mut running = self.inner.take().ok_or_else(|| {
            DevServerError::fail("no dev server is running", Value::Null)
        })?;
        let saved = running.stop(self.log_dir.as_deref());
        self.saved_log = saved.clone();
        Ok(json!({
            "stopped": true,
            "log": saved.map(|path| path.display().to_string()),
        }))
    }

    /// Stop whatever is running; ignores "nothing running". Safe from `Drop`.
    pub fn shutdown(&mut self) {
        if let Some(mut running) = self.inner.take() {
            self.saved_log = running.stop(self.log_dir.as_deref());
        }
    }

    fn running(&self) -> Result<&Running, DevServerError> {
        self.inner.as_ref().ok_or_else(|| {
            DevServerError::fail("no dev server is running (`dev_server: {op: start}` first)", Value::Null)
        })
    }
}

impl Drop for DevServerHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Running {
    /// SIGTERM the process group, then SIGKILL; save the log; delete the scratch.
    fn stop(&mut self, log_dir: Option<&Path>) -> Option<PathBuf> {
        let group = format!("-{}", self.child.id());
        let signal = |name: &str| {
            let _ = Command::new("kill")
                .args([name, "--", &group])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        };
        signal("-TERM");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        signal("-KILL");
        let _ = self.child.kill();
        let _ = self.child.wait();
        let saved = log_dir.and_then(|dir| {
            fs::create_dir_all(dir).ok()?;
            let target = dir.join("metro.log");
            let bytes = fs::read(&self.log).ok()?;
            // the log is evidence, not an archive: keep the last 256 KiB
            let keep = bytes.len().saturating_sub(256 * 1024);
            fs::write(&target, &bytes[keep..]).ok()?;
            Some(target)
        });
        let _ = fs::remove_dir_all(&self.root);
        saved
    }
}

fn io_error(what: &str) -> impl Fn(std::io::Error) -> DevServerError + '_ {
    move |error| DevServerError::fail(format!("{what}: {error}"), Value::Null)
}

fn log_tail(log: &Path) -> String {
    let bytes = fs::read(log).unwrap_or_default();
    let keep = bytes.len().saturating_sub(4000);
    String::from_utf8_lossy(&bytes[keep..]).to_string()
}

/// Copy a directory tree; with `link`, files are hard-linked (falling back to a
/// copy across filesystems). Symlinks are recreated as symlinks.
fn copy_tree(src: &Path, dst: &Path, link: bool) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&from, &to, link)?;
        } else if kind.is_symlink() {
            let target = fs::read_link(&from)?;
            let _ = fs::remove_file(&to);
            std::os::unix::fs::symlink(target, &to)?;
        } else if link {
            if fs::hard_link(&from, &to).is_err() {
                fs::copy(&from, &to)?;
            }
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn http_get(port: u16, path: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    http_request(port, "GET", path, timeout)
}

/// A tiny HTTP/1.0 client (one request, read to EOF): all the probes need, and no
/// new dependency. Fails on a non-2xx status.
fn http_request(port: u16, method: &str, path: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    let request = format!(
        "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "malformed HTTP reply".to_string())?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| "no HTTP status".to_string())?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}"));
    }
    Ok(raw[split + 4..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("acos-ds-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An edit goes to the copy, never through a hard link to the source tree.
    #[test]
    fn edit_breaks_hard_links_and_leaves_the_source_untouched() {
        let src = scratch("src");
        fs::write(src.join("App.js"), "Hello ACOS").unwrap();
        let copy = scratch("copy");
        copy_tree(&src, &copy, true).unwrap();
        let mut host = DevServerHost::default();
        host.inner = Some(Running {
            child: Command::new("sleep").arg("30").spawn().unwrap(),
            port: 1,
            root: copy.clone(),
            project: copy.clone(),
            log: copy.join("metro.log"),
        });
        let reply = host
            .edit("App.js", Some("ACOS"), Some("Metro"), None)
            .unwrap();
        assert_eq!(reply["replacements"], 1);
        assert_eq!(fs::read_to_string(copy.join("App.js")).unwrap(), "Hello Metro");
        assert_eq!(fs::read_to_string(src.join("App.js")).unwrap(), "Hello ACOS");
        host.shutdown();
        assert!(!copy.exists(), "stop deletes the project copy");
        let _ = fs::remove_dir_all(&src);
    }

    #[test]
    fn edit_refuses_a_missing_find_text_and_escaping_paths() {
        let copy = scratch("refuse");
        fs::write(copy.join("App.js"), "one").unwrap();
        let mut host = DevServerHost::default();
        host.inner = Some(Running {
            child: Command::new("sleep").arg("30").spawn().unwrap(),
            port: 1,
            root: copy.clone(),
            project: copy.clone(),
            log: copy.join("metro.log"),
        });
        let missing = host.edit("App.js", Some("zzz"), Some("y"), None).unwrap_err();
        assert!(missing.message.contains("not present"), "{}", missing.message);
        assert_eq!(fs::read_to_string(copy.join("App.js")).unwrap(), "one");
        for bad in ["../x", "/etc/passwd", "node_modules/react/index.js"] {
            assert!(host.edit(bad, None, None, Some("x")).is_err(), "{bad}");
        }
        host.shutdown();
    }

    #[test]
    fn dropping_the_host_stops_the_server_process() {
        let copy = scratch("drop");
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        {
            let mut host = DevServerHost::default();
            host.inner = Some(Running {
                child,
                port: 1,
                root: copy.clone(),
                project: copy.clone(),
                log: copy.join("metro.log"),
            });
        }
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists()
                || fs::read_to_string(format!("/proc/{pid}/stat"))
                    .map(|s| s.contains(") Z"))
                    .unwrap_or(true),
            "the process outlived its owner"
        );
        assert!(!copy.exists());
    }

    #[test]
    fn wait_serves_times_out_with_evidence_when_the_text_never_appears() {
        // a throw-away HTTP server that always serves "OLD"
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.0 200 OK\r\n\r\nOLD bundle");
            }
        });
        let copy = scratch("wait");
        let mut host = DevServerHost::default();
        host.inner = Some(Running {
            child: Command::new("sleep").arg("30").spawn().unwrap(),
            port,
            root: copy.clone(),
            project: copy.clone(),
            log: copy.join("metro.log"),
        });
        let error = host
            .wait_serves(Some("NEW"), None, Duration::from_millis(900))
            .unwrap_err();
        assert!(error.message.contains("did not reach"), "{}", error.message);
        host.wait_serves(Some("OLD"), Some("NEW"), Duration::from_secs(3))
            .expect("present text and absent text both hold");
        host.shutdown();
    }
}
