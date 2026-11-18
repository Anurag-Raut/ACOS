//! W55: `acos serve` correctness and trust guards.
//!
//! * `a_dead_guest_never_serves_a_cached_frame` — the W50 §7 item-3 bug: after
//!   the guest died, `acos_observe_tree`/`acos_status` answered from the cached
//!   frame and `acos_launch` handed back the dead session's tree. Now every verb
//!   checks the child, the cached frame is discarded, and `acos_launch`
//!   relaunches a fresh guest.
//! * `a_launcher_killed_without_an_exit_record_is_classified` — the W53
//!   defect: a dead `acos-art` with no `session-exit.json` was `INTERNAL`. The
//!   child's own exit status classifies it as `GUEST_EXITED`.
//! * `untrusted_serve_applies_the_trust_guard` — the same fail-closed rules
//!   `trust::TrustGuard` applies on the verify path (`observe` and
//!   `logs_since`), including the frame-only `capture` path that stays allowed.
//!
//! These run against a fake `acos-art` (`ACOS_ART_BIN`), so no guest boots and
//! they are safe on any host. The real-ART counterpart is the ignored
//! `w50_serve_art::art_agent_loop_over_the_socket` plus the agent-loop bench.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod common;

use common::ShortSocketDir;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache")
        .join(format!("w55-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

/// A fake `acos-art` interactive session.
///
/// * its tree carries its own pid (`gen-<pid>`), so a reply can never be
///   mistaken for another process's frame;
/// * `die` in the session directory makes the next `tap` SIGKILL the launcher
///   **without** writing `session-exit.json` (the W53 case);
/// * `exit-record` makes the next `tap` write a `session-exit.json` first and
///   then exit 1 (the W46 path).
fn fake_art(dir: &Path) -> PathBuf {
    let png = dir.join("tiny.png");
    std::fs::write(&png, b"\x89PNG\r\n\x1a\n-fake-frame").unwrap();
    let die = dir.join("die");
    let record = dir.join("exit-record");
    let script = dir.join("fake-acos-art.sh");
    let mut body = String::new();
    body.push_str("#!/bin/sh\n");
    body.push_str("gen=$$\n");
    // `acos-art` is spawned as `launch --apk A --until S --out <artifacts>`;
    // the engine reads `<out>/session-exit.json`.
    body.push_str("out=\"\"\n");
    body.push_str("while [ $# -gt 0 ]; do\n");
    body.push_str("  case \"$1\" in --out) out=\"$2\"; shift 2 ;; *) shift ;; esac\n");
    body.push_str("done\n");
    body.push_str("seq=0\n");
    body.push_str("echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"ready\\\",\\\"package\\\":\\\"dev.test\\\",\\\"activity\\\":\\\"dev.test/.Main\\\",\\\"width\\\":1080,\\\"height\\\":2400,\\\"density\\\":420}\"\n");
    body.push_str("frame() {\n");
    body.push_str("  seq=$((seq+1))\n");
    body.push_str(&format!(
        "  printf '{{\"ok\":true,\"cmd\":\"tree\",\"frame_seq\":%s,\"md5\":\"m%s\",\"png\":\"{}\",\"tree\":{{\"nodes\":[{{\"id\":\"dev.test:id/count\",\"class\":\"android.widget.TextView\",\"text\":\"gen-%s-%s\",\"bounds\":\"[0,0][100,100]\",\"bounds_on_screen\":\"[0,128][100,228]\"}}],\"truncated\":false}},\"windows\":[]}}\\n' \"$seq\" \"$seq\" \"$gen\" \"$seq\"\n",
        png.display()
    ));
    body.push_str("}\n");
    body.push_str("while IFS= read -r line; do\n");
    body.push_str("  case \"$line\" in\n");
    body.push_str("    *'\"cmd\":\"tree\"'*) frame ;;\n");
    body.push_str("    *'\"cmd\":\"screenshot\"'*)\n");
    body.push_str("      seq=$((seq+1))\n");
    body.push_str(&format!(
        "      printf '{{\"ok\":true,\"cmd\":\"screenshot\",\"frame_seq\":%s,\"md5\":\"s%s\",\"png\":\"{}\"}}\\n' \"$seq\" \"$seq\"\n",
        png.display()
    ));
    body.push_str("      ;;\n");
    body.push_str("    *'\"cmd\":\"tap\"'*|*'\"cmd\":\"key\"'*)\n");
    body.push_str(&format!(
        "      if [ -f '{}' ]; then rm -f '{}'; kill -9 $$; fi\n",
        die.display(),
        die.display()
    ));
    body.push_str(&format!(
        "      if [ -f '{}' ]; then rm -f '{}'; printf '{{\"outcome\":\"crash\",\"code\":\"GUEST_EXITED\",\"stage\":\"interactive\",\"guest_exit\":1}}' > \"$out/session-exit.json\"; exit 1; fi\n",
        record.display(),
        record.display()
    ));
    body.push_str("      frame\n");
    body.push_str("      ;;\n");
    body.push_str("    *'\"cmd\":\"logs\"'*)\n");
    body.push_str("      echo '{\"ok\":true,\"cmd\":\"logs\",\"total\":1,\"kept\":1,\"dropped\":0,\"entries\":[{\"prio\":4,\"tag\":\"AcosTest\",\"message\":\"onCreate\"}]}'\n");
    body.push_str("      ;;\n");
    body.push_str("    *'\"cmd\":\"close\"'*)\n");
    body.push_str("      echo '{\"ok\":true,\"cmd\":\"close\"}'\n");
    body.push_str("      exit 0\n");
    body.push_str("      ;;\n");
    body.push_str(
        "    *) echo '{\"ok\":false,\"code\":\"BAD_REQUEST\",\"error\":\"unsupported\"}' ;;\n",
    );
    body.push_str("  esac\n");
    body.push_str("done\n");
    std::fs::write(&script, body).unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    script
}

struct Daemon {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
    art_dir: PathBuf,
    // Last field: outlives every use of `socket`, removed with the daemon.
    sockets: ShortSocketDir,
}

impl Daemon {
    fn start(tag: &str) -> Daemon {
        let dir = scratch(tag);
        let art_dir = dir.join("art-home");
        std::fs::create_dir_all(art_dir.join("art-frames")).unwrap();
        let fake = fake_art(&art_dir);
        // `sun_path` is 108 bytes: the socket never lives under the worktree.
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
            .env("ACOS_ART_BIN", &fake)
            // ZDEFAULT1: this is a fake exec-path launcher; the zygote is the default.
            .env("ACOS_ART_ZYGOTE", "off")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn acos serve");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "serve socket never appeared");
            std::thread::sleep(Duration::from_millis(25));
        }
        Daemon {
            child,
            dir,
            socket,
            art_dir,
            sockets,
        }
    }

    fn connect(&self) -> Conn {
        Conn::new(&self.socket)
    }

    fn apk(&self) -> PathBuf {
        let apk = self.dir.join("fake.apk");
        if !apk.exists() {
            std::fs::write(&apk, b"not-a-real-apk").unwrap();
        }
        apk
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
    fn new(path: &Path) -> Conn {
        let stream = UnixStream::connect(path).expect("connect to serve socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        Conn {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            next: 0,
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.next += 1;
        let request = json!({"id": self.next, "tool": tool, "args": args});
        self.writer
            .write_all((request.to_string() + "\n").as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let value: Value = serde_json::from_str(&line).expect("response is JSON");
        assert_eq!(value["id"], json!(self.next));
        value["envelope"].clone()
    }
}

fn base(id: &str) -> Value {
    json!({"request_id": format!("w55-{id}"), "session_id": id})
}

fn details(envelope: &Value) -> Value {
    envelope
        .pointer("/error/details")
        .cloned()
        .unwrap_or(Value::Null)
}

fn count_text(envelope: &Value) -> String {
    let nodes = envelope
        .pointer("/result/tree/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for node in nodes {
        if node["resource_id"] == json!("dev.test:id/count") {
            return node["text"].as_str().unwrap_or_default().to_string();
        }
    }
    panic!("no count node in {envelope:#?}");
}

/// The W50 §7 item 3 bug, as a test: a dead guest must never serve its cache.
#[test]
fn a_dead_guest_never_serves_a_cached_frame() {
    let daemon = Daemon::start("dead-guest");
    let mut conn = daemon.connect();
    let b = base("s_dead");

    let launch = conn.call("acos_launch", json!({"base": b, "apk": daemon.apk()}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    let first = count_text(&launch);
    assert!(first.starts_with("gen-"), "{first}");
    assert_eq!(launch["result"]["relaunch"], json!(false));

    // Kill the launcher on the next tap, without writing session-exit.json.
    std::fs::write(daemon.art_dir.join("die"), b"1").unwrap();
    let tap = conn.call("acos_tap", json!({"base": b, "id": "count"}));
    assert_eq!(tap["ok"], json!(false), "{tap:#?}");
    assert_eq!(
        tap["error"]["code"],
        json!("GUEST_EXITED"),
        "a guest death is never INTERNAL: {tap:#?}"
    );
    assert_eq!(tap["result"], Value::Null, "no cached result on death");

    // Every verb now refuses: no stale tree, no stale status frame.
    for tool in [
        "acos_observe_tree",
        "acos_status",
        "acos_logs",
        "acos_assert",
    ] {
        let args = if tool == "acos_assert" {
            json!({"base": b, "id": "count", "expect": first})
        } else {
            json!({"base": b})
        };
        let reply = conn.call(tool, args);
        assert_eq!(
            reply["ok"],
            json!(false),
            "{tool} served a dead session: {reply:#?}"
        );
        assert_eq!(
            reply["error"]["code"],
            json!("GUEST_EXITED"),
            "{tool}: {reply:#?}"
        );
        assert!(
            reply.pointer("/result/tree").is_none(),
            "{tool} returned a tree from a dead guest"
        );
        let details = details(&reply);
        assert_eq!(details["verb"], json!(tool));
        // The verb that first observes the death reports the discarded frame;
        // later verbs have nothing left to discard and say nothing served it.
        if !details["discarded_frame"].is_null() {
            assert_eq!(
                details["discarded_frame"]["served"],
                json!(false),
                "{tool}: {details:#?}"
            );
        }
        assert!(
            reply.pointer("/result/frame").is_none(),
            "{tool}: {reply:#?}"
        );
        // The child's SIGKILL is the evidence (there is no exit record).
        assert_eq!(
            details["guest_exit"]["signal"],
            json!(9),
            "{tool}: {details:#?}"
        );
    }

    // `session_list` reports the session as dead, with its exit facts, and no
    // live frame sequence (the cached one was discarded).
    let listed = conn.call("session_list", json!({}));
    let sessions = listed["result"]["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let entry = sessions
        .iter()
        .find(|s| s["session_id"] == json!("s_dead"))
        .cloned()
        .expect("the dead session is still listed (it holds its slot until relaunch)");
    assert_eq!(entry["live"], json!(false), "{entry:#?}");
    assert_eq!(entry["mode"], json!("dead"));
    assert_eq!(entry["frame_seq"], json!(0));
    assert_eq!(entry["guest_exit"]["signal"], json!(9));

    // And `acos_launch` replaces the guest instead of handing back the cache.
    let relaunch = conn.call("acos_launch", json!({"base": b, "apk": daemon.apk()}));
    assert_eq!(relaunch["ok"], json!(true), "{relaunch:#?}");
    let replaced = relaunch["result"]["replaced_guest"].clone();
    assert!(
        !replaced.is_null(),
        "the reply must say a guest was replaced: {relaunch:#?}"
    );
    assert_eq!(replaced["guest_exit"]["signal"], json!(9), "{replaced:#?}");
    if !replaced["discarded_frame"].is_null() {
        assert_eq!(replaced["discarded_frame"]["served"], json!(false));
    }
    let second = count_text(&relaunch);
    assert_ne!(
        first, second,
        "the relaunched guest's frame must not be the dead session's cached frame"
    );
    assert!(second.starts_with("gen-"), "{second}");

    let _ = conn.call("acos_destroy", json!({"base": b}));
}

/// A launcher killed before it could write `session-exit.json` is classified
/// from its own exit status (W53), and one that wrote the record uses it.
#[test]
fn a_lost_launcher_is_classified_never_internal() {
    let daemon = Daemon::start("lost-launcher");
    let mut conn = daemon.connect();

    // Case 1: no exit record, killed by SIGKILL.
    let b = base("s_lost_no_record");
    let launch = conn.call("acos_launch", json!({"base": b, "apk": daemon.apk()}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    std::fs::write(daemon.art_dir.join("die"), b"1").unwrap();
    let tap = conn.call("acos_tap", json!({"base": b, "id": "count"}));
    assert_eq!(tap["error"]["code"], json!("GUEST_EXITED"), "{tap:#?}");
    let exit_details = details(&tap);
    // The guest died mid-command, so the engine's own lost-session path
    // classified it from the child status (no session-exit.json exists).
    assert_eq!(
        exit_details["session_exit"]["source"],
        json!("acos-art child exit status (no session-exit.json)"),
        "{exit_details:#?}"
    );
    assert_eq!(exit_details["session_exit"]["guest_signal"], json!(9));
    // The protocol taxonomy classes a guest exit as `harness` until triaged
    // (`acos-protocol` owns that table); the point here is the code and its
    // evidence, never `INTERNAL`.
    assert_ne!(tap["error"]["code"], json!("INTERNAL"));
    assert_eq!(tap["error"]["retryable"], json!(false));
    let _ = conn.call("acos_destroy", json!({"base": b}));

    // Case 2: the launcher wrote its exit record before dying.
    let b = base("s_lost_record");
    let launch = conn.call("acos_launch", json!({"base": b, "apk": daemon.apk()}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    std::fs::write(daemon.art_dir.join("exit-record"), b"1").unwrap();
    let tap = conn.call("acos_tap", json!({"base": b, "id": "count"}));
    assert_eq!(tap["error"]["code"], json!("GUEST_EXITED"), "{tap:#?}");
    let exit_details = details(&tap);
    assert_eq!(
        exit_details["session_exit"]["code"],
        json!("GUEST_EXITED"),
        "{exit_details:#?}"
    );
    assert_eq!(exit_details["session_exit"]["stage"], json!("interactive"));
    assert_ne!(tap["error"]["code"], json!("INTERNAL"));
    let _ = conn.call("acos_destroy", json!({"base": b}));
}

/// Serve applies the same fail-closed rules as verify's `TrustGuard`.
#[test]
fn untrusted_serve_applies_the_trust_guard() {
    let daemon = Daemon::start("trust");
    let mut conn = daemon.connect();

    let b = json!({"request_id": "w55-trust", "session_id": "s_untrusted", "trust": "untrusted"});
    let launch = conn.call(
        "acos_launch",
        json!({"base": b, "apk": daemon.apk(), "trust": "untrusted"}),
    );
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    assert_eq!(launch["result"]["trust"], json!("untrusted"));

    // `observe` (tree) is refused on an untrusted run, in both spellings.
    for (tool, args) in [
        ("acos_observe_tree", json!({"base": b})),
        (
            "acos_observe",
            json!({"base": b, "tree": true, "png": false}),
        ),
        (
            "acos_assert",
            json!({"base": b, "id": "count", "expect": "gen"}),
        ),
    ] {
        let reply = conn.call(tool, args);
        assert_eq!(reply["ok"], json!(false), "{tool}: {reply:#?}");
        assert_eq!(
            reply["error"]["code"],
            json!("INSUFFICIENT_HOST_EVIDENCE"),
            "{tool}: {reply:#?}"
        );
        assert_eq!(
            details(&reply)["assertion"],
            json!("UNTRUSTED_TREE_ASSERTION"),
            "{tool}: {reply:#?}"
        );
    }

    // `logs_since` is refused too (a forged line cannot justify a pass).
    let logs = conn.call("acos_logs", json!({"base": b}));
    assert_eq!(
        logs["error"]["code"],
        json!("INSUFFICIENT_HOST_EVIDENCE"),
        "{logs:#?}"
    );
    assert_eq!(
        details(&logs)["assertion"],
        json!("UNTRUSTED_LOG_ASSERTION")
    );

    // `capture` (host pixels) and `input` stay allowed, exactly like TrustGuard.
    let frame = conn.call(
        "acos_observe",
        json!({"base": b, "tree": false, "text": false, "png": false}),
    );
    assert_eq!(frame["ok"], json!(true), "{frame:#?}");
    assert_eq!(frame["result"]["evidence_label"], json!("host-pixel"));
    assert_eq!(frame["result"]["trust"], json!("untrusted"));

    let tap = conn.call("acos_tap", json!({"base": b, "id": "count"}));
    assert_eq!(
        tap["ok"],
        json!(true),
        "input is host-injected and allowed: {tap:#?}"
    );

    // The same verbs succeed on a trusted session: the guard is the trust
    // level, not the verb.
    let tb = json!({"request_id": "w55-trust-b", "session_id": "s_trusted"});
    let launch = conn.call("acos_launch", json!({"base": tb, "apk": daemon.apk()}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    assert_eq!(launch["result"]["trust"], json!("trusted"));
    let tree = conn.call("acos_observe_tree", json!({"base": tb}));
    assert_eq!(tree["ok"], json!(true), "{tree:#?}");
    let logs = conn.call("acos_logs", json!({"base": tb}));
    assert_eq!(logs["ok"], json!(true), "{logs:#?}");

    let _ = conn.call("acos_destroy", json!({"base": tb}));
    let _ = conn.call("acos_destroy", json!({"base": b}));
}
