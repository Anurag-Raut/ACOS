//! DEVPANEL1 CP4: real `expo-device-hub` `listDevices` discovery (bare
//! `getprop`, QEMU emulator console `avd name`), agent-settable `device_name`,
//! and Option B `agent-device` adb commands (`input`, `screencap -p`,
//! `uiautomator dump`, `pm`/`am`/`dumpsys`/`wm`) + verify watch-only guard.
//!
//! Run live ART tests under the shared guard:
//!   env -u TMPDIR tools/art-runtime/run-exclusive.sh --task DEVPANEL1-HUB -- \
//!     tools/guard/guarded.sh --mem 12G --time 1500 -- \
//!     cargo test --offline -p acos-cli --test devpanel_cp4_hub -- --ignored --test-threads=1

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod common;

use common::ShortSocketDir;

fn repo_root() -> PathBuf {
    let raw = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

fn acos_bin() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

fn find_bin(name: &str, env_var: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env_var) {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let sibling = Path::new(acos_bin()).with_file_name(name);
    if sibling.is_file() {
        return Some(sibling);
    }
    None
}

fn find_fixture_apk(name: &str) -> Option<PathBuf> {
    for candidate in [
        repo_root().join("fixtures/build/apps").join(name),
        PathBuf::from("/home/Qui/Projects/acos/fixtures/build/apps").join(name),
    ] {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn find_hub_server_entry() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("EXPO_DEVICE_HUB_ENTRY") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let candidate = PathBuf::from(home)
        .join(".t3/tools/expo-device-hub/0.22.1/node_modules/expo-device-hub/dist/server/index.mjs");
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache")
        .join(format!("cp4-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::create_dir_all(dir.join("avd-running")).unwrap();
    dir
}

struct Daemon {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
    avd_dir: PathBuf,
    _sockets: ShortSocketDir,
}

impl Daemon {
    fn start(tag: &str, extra_args: &[&str], extra_envs: &[(&str, &str)]) -> Self {
        let dir = scratch(tag);
        let lock_dir = dir.join("adb-locks");
        let avd_dir = dir.join("avd-running");
        let sockets = ShortSocketDir::new(tag);
        let socket = sockets.socket("serve.sock");
        let mut cmd = Command::new(acos_bin());
        cmd.arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg("180")
            .args(extra_args)
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
            .env("ACOS_CACHE_DIR", dir.join("cache"))
            .env("ACOS_ADB_LOCK_DIR", &lock_dir)
            .env("ACOS_AVD_RUNNING_DIR", &avd_dir)
            .env("ACOS_MEMORY_GUARD", "off")
            .env("ACOS_VIEW_REGISTRY_DIR", dir.join("viewreg"))
            .env_remove("ACOS_SESSION_SOCKET")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(dir.join("serve.err")).unwrap()));
        if let Some(art) = find_bin("acos-art", "ACOS_ART_BIN") {
            cmd.env("ACOS_ART_BIN", art);
        }
        if let Some(bridge) = find_bin("acos-devbridge", "ACOS_DEVBRIDGE_BIN") {
            cmd.env("ACOS_DEVBRIDGE_BIN", bridge);
        }
        for (k, v) in extra_envs {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawn acos serve");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !socket.exists() {
            assert!(
                Instant::now() < deadline,
                "serve socket {} never appeared",
                socket.display()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Self {
            child,
            socket,
            dir,
            avd_dir,
            _sockets: sockets,
        }
    }

    fn connect(&self) -> Conn {
        Conn::new(&self.socket)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next: u64,
}

impl Conn {
    fn new(path: &Path) -> Self {
        let stream = UnixStream::connect(path).expect("connect to serve socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            next: 0,
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.next += 1;
        let req = json!({ "id": self.next, "tool": tool, "args": args });
        self.writer
            .write_all((req.to_string() + "\n").as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let resp: Value = serde_json::from_str(&line).expect("response JSON");
        assert_eq!(resp["id"], json!(self.next));
        resp["envelope"].clone()
    }
}

struct PrivateAdbServer {
    port: u16,
}

impl PrivateAdbServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral adb port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let p = port.to_string();
        let _ = Command::new("adb")
            .args(["-P", &p, "kill-server"])
            .env("ANDROID_ADB_SERVER_PORT", &p)
            .output();
        let status = Command::new("adb")
            .args(["-P", &p, "start-server"])
            .env("ANDROID_ADB_SERVER_PORT", &p)
            .status()
            .expect("adb start-server");
        assert!(status.success(), "adb -P {p} start-server failed");
        Self { port }
    }
}

impl Drop for PrivateAdbServer {
    fn drop(&mut self) {
        let p = self.port.to_string();
        let _ = Command::new("adb")
            .args(["-P", &p, "kill-server"])
            .env("ANDROID_ADB_SERVER_PORT", &p)
            .output();
    }
}

fn adb_cmd(port: u16, serial: &str, args: &[&str]) -> std::process::Output {
    let p = port.to_string();
    Command::new("adb")
        .args(["-P", &p, "-s", serial])
        .args(args)
        .env("ANDROID_ADB_SERVER_PORT", &p)
        .output()
        .expect("run adb")
}

fn wait_serial(port: u16, serial: &str, limit: Duration) {
    let p = port.to_string();
    let deadline = Instant::now() + limit;
    loop {
        let out = Command::new("adb")
            .args(["-P", &p, "devices"])
            .env("ANDROID_ADB_SERVER_PORT", &p)
            .output()
            .expect("adb devices");
        let text = String::from_utf8_lossy(&out.stdout);
        if text.lines().any(|l| l.starts_with(serial) && l.contains("\tdevice")) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{serial} never appeared in adb devices:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn parse_bounds_val(val: &Value) -> Option<(i32, i32, i32, i32)> {
    if let Some(s) = val.as_str() {
        let s = s.trim();
        let rest = s.strip_prefix('[')?;
        let mid = rest.find("][")?;
        let first = &rest[..mid];
        let second = rest[mid + 2..].strip_suffix(']')?;
        let (l, t) = first.split_once(',')?;
        let (r, b) = second.split_once(',')?;
        return Some((
            l.trim().parse().ok()?,
            t.trim().parse().ok()?,
            r.trim().parse().ok()?,
            b.trim().parse().ok()?,
        ));
    }
    if let Some(b) = val.as_array() {
        if b.len() == 4 {
            return Some((
                b[0].as_i64()? as i32,
                b[1].as_i64()? as i32,
                b[2].as_i64()? as i32,
                b[3].as_i64()? as i32,
            ));
        }
    }
    None
}

fn find_node_bounds(
    node: &Value,
    needle_text: Option<&str>,
    needle_class: Option<&str>,
) -> Option<(i32, i32, i32, i32)> {
    let text = node.get("text").and_then(Value::as_str).unwrap_or("");
    let class = node.get("class").and_then(Value::as_str).unwrap_or("");
    let text_ok = needle_text.map_or(true, |t| text == t);
    let class_ok = needle_class.map_or(true, |c| class.contains(c));
    if text_ok && class_ok {
        if let Some(b) = node.get("bounds").and_then(parse_bounds_val) {
            return Some(b);
        }
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for ch in children {
            if let Some(found) = find_node_bounds(ch, needle_text, needle_class) {
                return Some(found);
            }
        }
    }
    None
}

fn find_bounds_in_tree(
    resp: &Value,
    needle_text: Option<&str>,
    needle_class: Option<&str>,
) -> Option<(i32, i32, i32, i32)> {
    let nodes = resp["result"]["tree"]["nodes"].as_array()?;
    for n in nodes {
        if let Some(found) = find_node_bounds(n, needle_text, needle_class) {
            return Some(found);
        }
    }
    None
}

fn start_probe(socket: &str, out: &Path, seconds: u32) -> Child {
    std::fs::create_dir_all(out).unwrap();
    Command::new("python3")
        .arg("-I")
        .arg(repo_root().join("tools/view/stream_probe.py"))
        .args(["--socket", socket, "--no-frames", "--seconds"])
        .arg(seconds.to_string())
        .arg("--out")
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn stream_probe.py")
}

fn read_events(out: &Path) -> Vec<Value> {
    std::fs::read_to_string(out.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn wait_events(out: &Path, want: impl Fn(&[Value]) -> bool, secs: u64) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let ev = read_events(out);
        if want(&ev) || Instant::now() > deadline {
            return ev;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn registry_stream_socket(dir: &Path, session_id: &str) -> String {
    let reg = dir.join("viewreg");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(rd) = std::fs::read_dir(&reg) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with(session_id) && name.ends_with(".json") {
                    if let Ok(text) = std::fs::read_to_string(e.path()) {
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            if let Some(s) = v["stream_socket"].as_str() {
                                return s.to_string();
                            }
                        }
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "no stream_socket in {} for {session_id}",
            reg.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// Test 1: Hub discovery (`getprop`, console `avd name`, `device_name`)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "boots real ART guests and runs expo-device-hub 0.22.1 listDevices against private adb"]
fn cp4_hub_lists_acos_devices_with_default_and_custom_names() {
    let counter_apk = find_fixture_apk("counter.apk").expect("counter.apk not found");
    let text_apk = find_fixture_apk("text-input.apk").expect("text-input.apk not found");
    let hub_entry = find_hub_server_entry().expect("expo-device-hub 0.22.1 server entry not found");

    let adb_srv = PrivateAdbServer::start();
    let adb_port = adb_srv.port.to_string();
    let daemon = Daemon::start(
        "cp4-hub",
        &["--adb"],
        &[("ANDROID_ADB_SERVER_PORT", &adb_port)],
    );
    let mut conn = daemon.connect();

    // 1. Default device_name: "<app label> · <session_id>"
    let base_def = json!({ "session_id": "cp4-def" });
    let launch_def = conn.call("acos_launch", json!({ "base": base_def, "apk": counter_apk }));
    assert_eq!(launch_def["ok"], json!(true), "{launch_def:#?}");
    let dp_def = &launch_def["result"]["device_panel"];
    assert_eq!(dp_def["exposed"], json!(true), "{dp_def:#?}");
    assert_eq!(dp_def["avd_name"], json!("Counter · cp4-def"), "{dp_def:#?}");
    let serial_def = dp_def["serial"].as_str().unwrap().to_string();
    wait_serial(adb_srv.port, &serial_def, Duration::from_secs(10));

    // Verify bare `getprop` returns [key]: [value] lines and `emu avd name` returns the exact name + OK.
    let getprop_out = adb_cmd(adb_srv.port, &serial_def, &["shell", "getprop"]);
    assert!(
        getprop_out.status.success(),
        "bare getprop failed: {}",
        String::from_utf8_lossy(&getprop_out.stderr)
    );
    let getprop_str = String::from_utf8_lossy(&getprop_out.stdout);
    assert!(
        getprop_str.contains("[ro.kernel.qemu]: [1]")
            && getprop_str.contains("[ro.boot.qemu.avd_name]: [Counter · cp4-def]"),
        "unexpected bare getprop output:\n{getprop_str}"
    );

    let emu_out = adb_cmd(adb_srv.port, &serial_def, &["emu", "avd", "name"]);
    assert!(
        emu_out.status.success(),
        "adb emu avd name failed: {}",
        String::from_utf8_lossy(&emu_out.stderr)
    );
    let emu_str = String::from_utf8_lossy(&emu_out.stdout);
    assert!(
        emu_str.contains("Counter · cp4-def") && emu_str.contains("OK"),
        "unexpected emu avd name output:\n{emu_str}"
    );

    // 2. Agent-provided device_name via acos_launch (with control chars & '=' stripped).
    let base_custom = json!({ "session_id": "cp4-custom" });
    let launch_custom = conn.call(
        "acos_launch",
        json!({
            "base": base_custom,
            "apk": text_apk,
            "device_name": "Checkout\n=Probe   Device"
        }),
    );
    assert_eq!(launch_custom["ok"], json!(true), "{launch_custom:#?}");
    let dp_custom = &launch_custom["result"]["device_panel"];
    assert_eq!(
        dp_custom["avd_name"],
        json!("Checkout Probe Device"),
        "{dp_custom:#?}"
    );
    let serial_custom = dp_custom["serial"].as_str().unwrap().to_string();
    wait_serial(adb_srv.port, &serial_custom, Duration::from_secs(10));

    // 3. CLI `--device-name` via `acos launch --keep`.
    let cli_out = Command::new(acos_bin())
        .args([
            "launch",
            "--keep",
            "--device-name",
            "CLI Device Name",
            counter_apk.to_str().unwrap(),
            "--json",
        ])
        .env("ACOS_SESSION_SOCKET", &daemon.socket)
        .output()
        .expect("acos launch --keep --device-name");
    assert!(
        cli_out.status.success(),
        "acos launch --keep failed:\n{}\n{}",
        String::from_utf8_lossy(&cli_out.stdout),
        String::from_utf8_lossy(&cli_out.stderr)
    );
    let cli_json: Value =
        serde_json::from_slice(&cli_out.stdout).expect("acos launch --json output");
    let cli_sid = cli_json["result"]["session_id"]
        .as_str()
        .unwrap_or("s_default")
        .to_string();
    let dp_cli = &cli_json["result"]["device_panel"];
    assert_eq!(dp_cli["avd_name"], json!("CLI Device Name"), "{cli_json:#?}");
    let serial_cli = dp_cli["serial"].as_str().unwrap().to_string();
    wait_serial(adb_srv.port, &serial_cli, Duration::from_secs(10));

    // Verify pid_*.ini contains `acos.managed=1` and the exact `avd.name`.
    let ini_contents: Vec<String> = std::fs::read_dir(&daemon.avd_dir)
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect();
    assert!(
        ini_contents
            .iter()
            .any(|s| s.contains("acos.managed=1") && s.contains("avd.name=Checkout Probe Device")),
        "missing acos.managed=1 or custom avd.name in ini files: {ini_contents:?}"
    );

    // 4. Query the real `expo-device-hub` 0.22.1 HTTP handler `/api/devices?booted=1`.
    // Provide a lightweight ANDROID_HOME wrapper whose `platform-tools/adb`
    // filters `adb devices -l` to only our test's spawned serials (so an
    // unrelated pre-fix host process on 5554/5556 cannot poison `listDevices`
    // on the private adb server's automatic 5555..5585 scan), while forwarding
    // every `getprop` / `emu avd name` / etc. call directly to real `adb`.
    let fake_sdk = daemon.dir.join("fake-sdk");
    std::fs::create_dir_all(fake_sdk.join("platform-tools")).unwrap();
    std::fs::create_dir_all(fake_sdk.join("cmdline-tools/latest/bin")).unwrap();
    let real_avdmanager = std::env::var_os("ANDROID_HOME")
        .or_else(|| std::env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Android/Sdk")
        })
        .join("cmdline-tools/latest/bin/avdmanager");
    let avdmanager_wrap = fake_sdk.join("cmdline-tools/latest/bin/avdmanager");
    if real_avdmanager.is_file() {
        std::os::unix::fs::symlink(&real_avdmanager, &avdmanager_wrap).unwrap();
    } else {
        std::fs::write(&avdmanager_wrap, "#!/usr/bin/env sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&avdmanager_wrap, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
    }
    let adb_wrap = fake_sdk.join("platform-tools/adb");
    std::fs::write(
        &adb_wrap,
        format!(
            "#!/usr/bin/env sh\n\
             if [ \"$1\" = \"devices\" ]; then\n\
               /usr/bin/adb \"$@\" | awk -v keep=\" $ACOS_TEST_SERIALS \" 'NR==1 || ($1 ~ /^emulator-/ && index(keep, \" \" $1 \" \") > 0)'\n\
               exit $?\n\
             fi\n\
             exec /usr/bin/adb \"$@\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&adb_wrap, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let keep_serials = format!("{serial_def} {serial_custom} {serial_cli}");

    let node_script = daemon.dir.join("hub_list.mjs");
    std::fs::write(
        &node_script,
        r#"
import { pathToFileURL } from "node:url";
const mod = await import(pathToFileURL(process.env.HUB_ENTRY).href);
const handler = mod.default;
const res = await handler(new Request("http://127.0.0.1/api/devices?booted=1"));
const body = await res.json();
console.log(JSON.stringify(body));
"#,
    )
    .unwrap();
    let hub_out = Command::new("node")
        .arg(&node_script)
        .env("HUB_ENTRY", &hub_entry)
        .env("ANDROID_HOME", &fake_sdk)
        .env("ANDROID_SDK_ROOT", &fake_sdk)
        .env("ACOS_TEST_SERIALS", &keep_serials)
        .env("ANDROID_ADB_SERVER_PORT", &adb_port)
        .output()
        .expect("run expo-device-hub listDevices");
    assert!(
        hub_out.status.success(),
        "expo-device-hub failed:\n{}\n{}",
        String::from_utf8_lossy(&hub_out.stdout),
        String::from_utf8_lossy(&hub_out.stderr)
    );
    let hub_json: Value =
        serde_json::from_slice(&hub_out.stdout).expect("expo-device-hub JSON output");
    let emulators = hub_json["emulators"].as_array().expect("emulators array");
    let find_emu = |serial: &str| {
        emulators
            .iter()
            .find(|d| d["serial"] == serial || d["id"] == serial)
            .cloned()
    };
    let e_def = find_emu(&serial_def).unwrap_or_else(|| panic!("missing {serial_def}: {hub_json:#?}"));
    let e_custom = find_emu(&serial_custom)
        .unwrap_or_else(|| panic!("missing {serial_custom}: {hub_json:#?}"));
    let e_cli = find_emu(&serial_cli).unwrap_or_else(|| panic!("missing {serial_cli}: {hub_json:#?}"));

    assert_eq!(e_def["name"], json!("Counter · cp4-def"), "{e_def:#?}");
    assert_eq!(e_def["booted"], json!(true), "{e_def:#?}");
    assert_eq!(
        e_custom["name"],
        json!("Checkout Probe Device"),
        "{e_custom:#?}"
    );
    assert_eq!(e_cli["name"], json!("CLI Device Name"), "{e_cli:#?}");

    for sid in ["cp4-def", "cp4-custom", cli_sid.as_str()] {
        let _ = conn.call("acos_destroy", json!({ "base": { "session_id": sid } }));
    }
    drop(adb_srv);
}

// ---------------------------------------------------------------------------
// Test 2: Option B `agent-device` adb commands + `source: adb` event feed
// ---------------------------------------------------------------------------

#[test]
#[ignore = "boots a real ART guest and exercises Option B adb shell commands"]
fn cp4_agent_device_adb_commands_and_source_adb_feed() {
    let text_apk = find_fixture_apk("text-input.apk").expect("text-input.apk not found");
    let adb_srv = PrivateAdbServer::start();
    let adb_port = adb_srv.port.to_string();
    let daemon = Daemon::start(
        "cp4-adb",
        &["--adb"],
        &[("ANDROID_ADB_SERVER_PORT", &adb_port)],
    );
    let mut conn = daemon.connect();
    let base = json!({ "session_id": "cp4-adb" });
    let launch = conn.call("acos_launch", json!({ "base": base, "apk": text_apk }));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    let serial = launch["result"]["device_panel"]["serial"]
        .as_str()
        .unwrap()
        .to_string();
    wait_serial(adb_srv.port, &serial, Duration::from_secs(10));

    // Read EditText bounds via acos_observe first.
    let obs0 = conn.call("acos_observe", json!({ "base": base, "tree": true }));
    let (l, t, r, b) = find_bounds_in_tree(&obs0, None, Some("EditText")).expect("EditText bounds");
    let (cx, cy) = ((l + r) / 2, (t + b) / 2);

    // 1. Query commands used by agent-device.
    let canon_text_apk = std::fs::canonicalize(&text_apk).unwrap_or_else(|_| text_apk.clone());
    let expected_pm_path = format!("package:{}", canon_text_apk.display());
    for (args, expected) in [
        (&["shell", "wm", "size"][..], "Physical size: 1080x2400"),
        (&["shell", "dumpsys", "display"][..], "DISPLAY MANAGER"),
        (&["shell", "dumpsys", "input_method"][..], "dev.acos.fixtures.textinput"),
        (&["shell", "dumpsys", "window"][..], "mCurrentFocus="),
        (&["shell", "dumpsys", "activity", "activities"][..], "ResumedActivity:"),
        (&["shell", "pm", "list", "packages", "-3"][..], "package:dev.acos.fixtures.textinput"),
        (&["shell", "pm", "path", "dev.acos.fixtures.textinput"][..], expected_pm_path.as_str()),
        (&["shell", "am", "start", "-n", "dev.acos.fixtures.textinput/.MainActivity"][..], "Starting: Intent"),
    ] {
        let out = adb_cmd(adb_srv.port, &serial, args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{args:?} failed:\n{stdout}\n{stderr}");
        assert!(
            stdout.contains(expected),
            "{args:?} missing {expected:?}:\n{stdout}"
        );
    }

    // `pm install` must refuse honestly.
    let install_out = adb_cmd(adb_srv.port, &serial, &["shell", "pm", "install", "/tmp/helper.apk"]);
    assert!(!install_out.status.success(), "pm install must fail");
    assert!(
        String::from_utf8_lossy(&install_out.stderr).contains("acos: unsupported"),
        "expected acos: unsupported on pm install, got: {}",
        String::from_utf8_lossy(&install_out.stderr)
    );

    // `exec-out screencap -p` -> real PNG bytes.
    let cap_out = adb_cmd(adb_srv.port, &serial, &["exec-out", "screencap", "-p"]);
    assert!(cap_out.status.success(), "screencap failed");
    assert!(
        cap_out.stdout.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "screencap -p output is not a valid PNG (len={})",
        cap_out.stdout.len()
    );

    // `exec-out uiautomator dump /dev/tty` -> AOSP XML containing EditText.
    let dump_out = adb_cmd(
        adb_srv.port,
        &serial,
        &["exec-out", "uiautomator", "dump", "/dev/tty"],
    );
    assert!(dump_out.status.success(), "uiautomator dump /dev/tty failed");
    let xml = String::from_utf8_lossy(&dump_out.stdout);
    assert!(
        xml.contains("<hierarchy rotation=\"0\">")
            && xml.contains("</hierarchy>")
            && xml.contains("EditText")
            && xml.contains(&format!("bounds=\"[{l},{t}][{r},{b}]\"")),
        "unexpected uiautomator XML:\n{xml}"
    );

    // Compound `uiautomator dump /sdcard/window_dump.xml && cat /sdcard/window_dump.xml && rm -f /sdcard/window_dump.xml`
    let file_dump_out = adb_cmd(
        adb_srv.port,
        &serial,
        &[
            "shell",
            "uiautomator dump /sdcard/window_dump.xml && cat /sdcard/window_dump.xml && rm -f /sdcard/window_dump.xml",
        ],
    );
    assert!(file_dump_out.status.success());
    assert!(String::from_utf8_lossy(&file_dump_out.stdout).contains("<hierarchy"));

    // 2. Attach live-view event feed probe and send adb `input tap`, `input text`, `input keyevent`.
    let socket = registry_stream_socket(&daemon.dir, "cp4-adb");
    let probe_dir = daemon.dir.join("probe-adb");
    let mut probe = start_probe(&socket, &probe_dir, 40);
    std::thread::sleep(Duration::from_millis(800));

    let tap_arg = format!("input tap {cx} {cy}");
    let tap_out = adb_cmd(adb_srv.port, &serial, &["shell", &tap_arg]);
    assert!(
        tap_out.status.success(),
        "input tap failed: {}",
        String::from_utf8_lossy(&tap_out.stderr)
    );

    let text_out = adb_cmd(adb_srv.port, &serial, &["shell", "input text hello%sworld"]);
    assert!(
        text_out.status.success(),
        "input text failed: {}",
        String::from_utf8_lossy(&text_out.stderr)
    );

    let key_out = adb_cmd(adb_srv.port, &serial, &["shell", "input keyevent KEYCODE_DEL"]);
    assert!(
        key_out.status.success(),
        "input keyevent failed: {}",
        String::from_utf8_lossy(&key_out.stderr)
    );

    // Wait for UI tree to reflect "hello worl".
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_text = false;
    while Instant::now() < deadline {
        let obs = conn.call("acos_observe", json!({ "base": base, "tree": true }));
        let dumped = obs.to_string();
        if dumped.contains("hello worl") {
            saw_text = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(saw_text, "EditText never reflected `input text` + `KEYCODE_DEL`");

    let events = wait_events(
        &probe_dir,
        |ev| {
            ev.iter().any(|e| e["source"] == "adb" && e["cmd"] == "tap")
                && ev.iter().any(|e| e["source"] == "adb" && e["cmd"] == "text")
                && ev.iter().any(|e| e["source"] == "adb" && e["cmd"] == "key")
        },
        10,
    );
    let _ = probe.kill();
    let _ = probe.wait();

    assert!(
        events.iter().any(|e| e["source"] == "adb" && e["cmd"] == "tap"),
        "missing source=adb tap in {events:#?}"
    );
    assert!(
        events.iter().any(|e| e["source"] == "adb" && e["cmd"] == "text"),
        "missing source=adb text in {events:#?}"
    );
    assert!(
        events.iter().any(|e| e["source"] == "adb" && e["cmd"] == "key"),
        "missing source=adb key in {events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e["source"] == "adb" && e["cmd"] == "text" && e["text"] == "hello world"),
        "expected source=adb text=\"hello world\" in {events:#?}"
    );

    assert_eq!(
        conn.call("acos_destroy", json!({ "base": base }))["ok"],
        json!(true)
    );
    drop(adb_srv);
}

// ---------------------------------------------------------------------------
// Test 3: `acos verify --adb` rejects `adb shell input ...` as watch-only
// ---------------------------------------------------------------------------

#[test]
#[ignore = "boots a real ART guest running `acos verify --adb` and checks adb input is ignored"]
fn cp4_verify_ignores_adb_input_and_emits_watch_only() {
    let counter_apk = find_fixture_apk("counter.apk").expect("counter.apk not found");
    let adb_srv = PrivateAdbServer::start();
    let adb_port = adb_srv.port.to_string();
    let dir = scratch("cp4-verify");
    let avd_dir = dir.join("avd-running");
    let scenario = dir.join("watch-only-adb.yaml");
    std::fs::write(
        &scenario,
        format!(
            r#"version: 1
name: watch-only-adb
description: Launch counter, hold 14 s while adb shell input tries to tap Increment, and assert count is still 0.
app: {apk}
engine: native-preferred
fallback: report-and-escalate
api_level: 34
isolation: process
display:
  mode: offscreen
  width: 1080
  height: 2400
  density_dpi: 420
capabilities: [lifecycle, ui.offscreen, ui.text, input]
expectation: pass
budgets:
  memory_mib: 512
  wall_ms: 90000
steps:
  - launch:
      activity: dev.acos.fixtures.counter/.MainActivity
      clear_state: true
  - wait_for:
      selector: {{id: count}}
      timeout_ms: 5000
  - wait:
      ms: 14000
  - assert_text:
      selector: {{id: count}}
      text: {{exact: "0"}}
"#,
            apk = counter_apk.display()
        ),
    )
    .unwrap();

    let artifacts = dir.join("artifacts");
    let mut child = Command::new(acos_bin())
        .args(["verify", "--adb", "--engine", "art", "--overwrite", "--json"])
        .arg(&scenario)
        .arg("--artifacts")
        .arg(&artifacts)
        .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
        .env("ACOS_CACHE_DIR", dir.join("cache"))
        .env("ACOS_ADB_LOCK_DIR", dir.join("adb-locks"))
        .env("ACOS_AVD_RUNNING_DIR", &avd_dir)
        .env("ACOS_MEMORY_GUARD", "off")
        .env("ANDROID_ADB_SERVER_PORT", &adb_port)
        .env(
            "ACOS_ART_BIN",
            find_bin("acos-art", "ACOS_ART_BIN").expect("acos-art"),
        )
        .env(
            "ACOS_DEVBRIDGE_BIN",
            find_bin("acos-devbridge", "ACOS_DEVBRIDGE_BIN").expect("acos-devbridge"),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn acos verify --adb");

    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let announce: Value = {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let line = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("verify never announced device_panel");
            if let Some(rest) = line.strip_prefix("acos verify: device_panel ") {
                break serde_json::from_str(rest).expect("device_panel JSON");
            }
        }
    };
    assert_eq!(announce["device_panel"]["exposed"], json!(true), "{announce:#?}");
    let serial = announce["device_panel"]["serial"]
        .as_str()
        .unwrap()
        .to_string();
    let socket = announce["stream_socket"]
        .as_str()
        .expect("stream_socket")
        .to_string();
    wait_serial(adb_srv.port, &serial, Duration::from_secs(10));

    let out = dir.join("probe-verify-adb");
    let mut probe = start_probe(&socket, &out, 40);
    std::thread::sleep(Duration::from_millis(800));

    let tap_out = adb_cmd(adb_srv.port, &serial, &["shell", "input tap 157 431"]);
    assert!(
        !tap_out.status.success(),
        "adb input tap must fail during verify run"
    );
    let tap_err = String::from_utf8_lossy(&tap_out.stderr);
    assert!(
        tap_err.contains("adb input ignored: verify run"),
        "expected verify watch-only stderr, got: {tap_err}"
    );

    let events = wait_events(
        &out,
        |ev| ev.iter().any(|e| e["status"] == "VERIFY_WATCH_ONLY"),
        10,
    );
    let _ = probe.kill();
    let _ = probe.wait();
    assert!(
        events.iter().any(|e| e["source"] == "adb"
            && e["status"] == "VERIFY_WATCH_ONLY"
            && e["note"] == "adb input ignored: verify run"),
        "feed does not record source=adb VERIFY_WATCH_ONLY: {events:#?}"
    );

    let status = child.wait().expect("verify exit");
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    assert!(status.success(), "verify did not pass:\n{stdout}");
    let verdict: Value = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    assert_eq!(verdict["status"], json!("pass"), "{stdout}");
    drop(adb_srv);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Test 4: `acos-devbridge` survives launching client disconnect (`PR_SET_PDEATHSIG`)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "boots a real ART guest and verifies acos-devbridge outlives short-lived client connections"]
fn cp4_devbridge_survives_launching_client_disconnect() {
    let counter_apk = find_fixture_apk("counter.apk").expect("counter.apk not found");
    let adb_srv = PrivateAdbServer::start();
    let adb_port = adb_srv.port.to_string();
    let mut daemon = Daemon::start(
        "cp4-pdeathsig",
        &["--adb"],
        &[("ANDROID_ADB_SERVER_PORT", &adb_port)],
    );

    // Simulate `acos launch --keep`: launch on a short-lived connection and
    // drop it immediately so `acos serve`'s per-connection worker thread exits.
    {
        let mut conn1 = daemon.connect();
        let launch = conn1.call(
            "acos_launch",
            json!({
                "base": { "session_id": "cp4-pdeathsig" },
                "apk": counter_apk,
                "device_name": "PDeathSig Probe"
            }),
        );
        assert_eq!(launch["ok"], json!(true), "{launch:#?}");
        assert_eq!(launch["result"]["device_panel"]["exposed"], json!(true));
    }

    // Give Linux plenty of time to deliver PR_SET_PDEATHSIG if the bridge had
    // mistakenly been forked from the exited client worker thread.
    std::thread::sleep(Duration::from_millis(350));

    let grpc_port: u16 = std::fs::read_dir(&daemon.avd_dir)
        .unwrap()
        .flatten()
        .find_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            text.lines()
                .find_map(|l| l.strip_prefix("grpc.port="))
                .and_then(|v| v.trim().parse::<u16>().ok())
        })
        .expect("missing grpc.port in pid_*.ini after client disconnect");

    // Verify the gRPC HTTP/2 listener in acos-devbridge is still alive and
    // answers a client connection preface with an HTTP/2 SETTINGS frame (type 0x04).
    let mut stream = TcpStream::connect(("127.0.0.1", grpc_port))
        .expect("acos-devbridge died after launching client disconnected");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00")
        .unwrap();
    let mut frame_hdr = [0u8; 9];
    stream
        .read_exact(&mut frame_hdr)
        .expect("expected HTTP/2 SETTINGS frame from live acos-devbridge");
    assert_eq!(
        frame_hdr[3], 0x04,
        "expected HTTP/2 SETTINGS frame type 0x04, got {:?}",
        frame_hdr
    );
    drop(stream);

    // Also confirm PR_SET_PDEATHSIG still reaps `acos-devbridge` when `acos serve`
    // itself is SIGKILLed.
    let _ = daemon.child.kill();
    let _ = daemon.child.wait();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if TcpStream::connect(("127.0.0.1", grpc_port)).is_err() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "acos-devbridge stayed alive on port {grpc_port} after acos serve SIGKILL"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(adb_srv);
}
