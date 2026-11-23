//! REL3A: a settle that timed out is never a plain pass on the agent-facing
//! channel either.
//!
//! The v0.3.0 feedback (F-06) reported three `acos_tap "Skip"` calls that returned
//! `status: pass` with `settle.settled:false, bound:"timeout"` on an endlessly
//! animating React Native screen. An agent that reads only `status` sees a green
//! light on a frame nothing can be asserted about.
//!
//! A scripted `acos-art` session (`ACOS_ART_BIN`) keeps this fast and guest-free;
//! the socket, the daemon and the reply envelope are the real ones.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod common;

use common::ShortSocketDir;

/// A session whose taps deliver a fresh frame and a tree, but whose settle
/// reports the strict rule timing out (`bound: "timeout"`), as ART does on a
/// screen that keeps animating.
const SESSION_SCRIPT: &str = r##"#!/usr/bin/env python3
import base64, hashlib, json, os, sys

PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC"
)
out = os.environ["FAKE_ART_OUT"]
os.makedirs(out, exist_ok=True)
state = {"seq": 0}

def png_for_frame():
    path = os.path.join(out, "frame_%d.png" % state["seq"])
    data = PNG + ("#seq=%d" % state["seq"]).encode()
    with open(path, "wb") as f:
        f.write(data)
    return path, hashlib.md5(data).hexdigest()

def tree():
    return {"nodes": [{
        "class": "android.widget.FrameLayout",
        "bounds": "[0,0][1080,2400]", "visibility": 0,
        "children": [{
            "class": "android.widget.TextView",
            "id": "dev.acos.fixtures.counter:id/count",
            "text": "1", "bounds": "[42,317][63,368]", "visibility": 0,
        }],
    }]}

def frame(cmd):
    state["seq"] += 1
    path, md5 = png_for_frame()
    return {"ok": True, "cmd": cmd, "frame_seq": state["seq"], "png": path, "md5": md5,
            "tree": tree()}

print(json.dumps(frame("ready")), flush=True)

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    request = json.loads(line)
    cmd = request.get("cmd", "")
    if cmd == "close":
        print(json.dumps({"ok": True, "cmd": "close", "frame_seq": state["seq"]}), flush=True)
        break
    if cmd == "tap":
        reply = frame("tap")
        reply["settle"] = {"mode": "strict", "settled": False, "bound": "timeout",
                          "reason": "newer-store", "animating": True}
        reply["tap"] = {"handled": True}
    elif cmd == "tree":
        reply = frame("tree")
        reply["tree"] = tree()
    elif cmd == "screenshot":
        reply = frame("screenshot")
    else:
        reply = {"ok": False, "cmd": cmd, "code": "BAD_CMD", "error": "unknown command"}
    print(json.dumps(reply), flush=True)
"##;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let mut dir = repo_root().join(".cache").join(format!("rel3a-test-{tag}-{pid}"));
    if dir.as_os_str().len() > 100 {
        dir = repo_root().join(".cache").join(format!("rel3a-{pid}-{tag}"));
    }
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::create_dir_all(dir.join("frames")).unwrap();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

fn session_script(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake_art_session.py");
    std::fs::write(&path, SESSION_SCRIPT).expect("write session script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod session script");
    path
}

fn base(id: &str) -> Value {
    json!({"request_id": format!("rel3a-{id}"), "session_id": id})
}

struct Daemon {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
    sockets: ShortSocketDir,
}

impl Daemon {
    fn start(tag: &str) -> Daemon {
        let dir = scratch(tag);
        // Written *after* the scratch dir is (re)created, so the script cannot be
        // deleted by a later `scratch` call.
        let script = session_script(&dir);
        let sockets = ShortSocketDir::new(tag);
        let socket = sockets.socket("serve.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_acos"))
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg("120")
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
            // The suite may export ACOS_SESSION_SOCKET; it wins over the runtime
            // dir, so this test must not inherit it.
            .env_remove("ACOS_SESSION_SOCKET")
            .env("ACOS_CACHE_DIR", dir.join("cache"))
            .env("ACOS_ART_BIN", script)
            .env("FAKE_ART_OUT", dir.join("frames"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn acos serve");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !socket.exists() {
            assert!(
                Instant::now() < deadline,
                "serve socket {} never appeared",
                socket.display()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Daemon {
            child,
            socket,
            dir,
            sockets,
        }
    }

    fn call(&self, tool: &str, args: Value) -> Value {
        let stream = UnixStream::connect(&self.socket).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let request = json!({"id": 1, "tool": tool, "args": args});
        writer
            .write_all((request.to_string() + "\n").as_bytes())
            .unwrap();
        writer.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).expect("response is JSON")["envelope"].clone()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// REL3A §3: an action whose settle timed out is `status: timeout`, carries
/// `details.reason: "settle_timeout"` and the settle record, and still hands the
/// caller the delivered facts so the screen is not lost.
#[test]
fn a_settle_timeout_on_an_action_is_not_a_pass() {
    let daemon = Daemon::start("settle-timeout");
    // A `..` component is refused by the MCP path validator, so resolve the path.
    let apk = std::fs::canonicalize(repo_root().join("fixtures/build/apps/counter.apk"))
        .unwrap_or_else(|_| repo_root().join("fixtures/build/apps/counter.apk"));
    assert!(
        apk.is_file(),
        "build fixtures first: `tools/build/build-apps.sh` (missing {})",
        apk.display()
    );
    let launch = daemon.call(
        "acos_launch",
        json!({"base": base("rel3a"), "apk": apk.display().to_string()}),
    );
    assert_eq!(launch["ok"], json!(true), "launch: {launch}");
    let envelope = daemon.call(
        "acos_tap",
        json!({"base": base("rel3a"), "x": 100, "y": 200, "settle": "strict"}),
    );
    assert_eq!(
        envelope["status"], json!("timeout"),
        "a settle timeout must not be reported as pass: {envelope}"
    );
    assert_eq!(envelope["ok"], json!(false));
    assert_eq!(envelope["error"]["code"], json!("TIMEOUT"));
    assert_eq!(envelope["error"]["class"], json!("infra_timeout"));
    let details = &envelope["error"]["details"];
    assert_eq!(details["reason"], json!("settle_timeout"));
    assert_eq!(details["settle"]["settled"], json!(false));
    // The record is normalised (REL3A §2) ...
    assert!(details["settle"]["waited_ms"].as_u64().is_some());
    assert_eq!(details["settle"]["reason"], json!("newer-store"));
    // ... and the delivered facts survive, so the agent still sees the screen.
    assert_eq!(details["delivery"]["accepted"], json!(true));
    assert_eq!(details["delivery"]["frame"]["frame_seq"], json!(3));
    assert!(envelope["result"]["frame"]["frame_seq"].as_u64().is_some());
}
