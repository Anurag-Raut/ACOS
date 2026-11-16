//! A minimal WIRE v1 guest for the WV5b tests: it speaks exactly what
//! `results/handoff/WVB/WIRE.md` specifies (framed JSON, `render` = JSON then
//! blob) and nothing more, so a gate/broker regression shows up as a wire
//! assertion rather than a helper's convenience.

use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acos_webview::wire::{read_frame, write_frame, Blob, Frame};
use serde_json::{json, Value};

pub struct WireGuest {
    writer: Arc<Mutex<UnixStream>>,
    pending: Arc<Mutex<HashMap<String, SyncSender<Value>>>>,
    blobs: Mutex<Receiver<Blob>>,
    /// Host→guest requests the broker sends (interception, JS calls) are
    /// answered "continue"/empty: these tests never ask the page for anything.
    seen: Arc<Mutex<Vec<(String, Value)>>>,
    next: AtomicU32,
    pub closed: Arc<AtomicBool>,
}

impl WireGuest {
    /// Connect to a session socket. The reader thread lives until EOF.
    pub fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path)
            .unwrap_or_else(|e| panic!("connect {}: {e}", path.display()));
        stream
            .set_read_timeout(Some(Duration::from_secs(45)))
            .expect("read timeout");
        let read_half = stream.try_clone().expect("clone socket");
        let writer = Arc::new(Mutex::new(stream));
        let (blob_tx, blob_rx) = sync_channel::<Blob>(8);
        let blob_rx = Mutex::new(blob_rx);
        let pending = Arc::new(Mutex::new(HashMap::<String, SyncSender<Value>>::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        {
            let pending = Arc::clone(&pending);
            let seen = Arc::clone(&seen);
            let closed = Arc::clone(&closed);
            let writer = Arc::clone(&writer);
            std::thread::Builder::new()
                .name("wire-guest".into())
                .spawn(move || {
                    let mut reader = BufReader::new(read_half);
                    loop {
                        match read_frame(&mut reader) {
                            Ok(Frame::Json(msg)) => {
                                let id = msg["id"].as_str().unwrap_or("").to_string();
                                if msg.get("op").is_some() && id.starts_with('h') {
                                    seen.lock().unwrap().push((id.clone(), msg.clone()));
                                    let answer = match msg["op"].as_str().unwrap_or("") {
                                        "req.intercept" => json!({"action": "continue"}),
                                        "nav.shouldOverride" => json!({"allow": true}),
                                        _ => json!({"resultJson": "null"}),
                                    };
                                    let out = json!({"id": id, "ok": true, "result": answer});
                                    if let Ok(bytes) = acos_webview::wire::encode_json(&out) {
                                        let mut w = writer.lock().unwrap();
                                        let _ = write_frame(&mut *w, &bytes);
                                    }
                                    continue;
                                }
                                if id.is_empty() {
                                    continue; // event
                                }
                                if let Some(tx) = pending.lock().unwrap().remove(&id) {
                                    let _ = tx.send(msg);
                                }
                            }
                            Ok(Frame::Blob(b)) => {
                                let _ = blob_tx.try_send(b);
                            }
                            Err(_) => {
                                closed.store(true, Ordering::SeqCst);
                                return;
                            }
                        }
                    }
                })
                .expect("reader thread");
        }
        Self {
            writer,
            pending,
            blobs: blob_rx,
            seen,
            next: AtomicU32::new(1),
            closed,
        }
    }

    fn send(&self, msg: &Value) -> std::io::Result<()> {
        let bytes = acos_webview::wire::encode_json(msg).expect("encode");
        let mut w = self.writer.lock().unwrap();
        write_frame(&mut *w, &bytes)
    }

    fn pending_for(&self, id: &str) -> Receiver<Value> {
        let (tx, rx) = sync_channel(1);
        self.pending.lock().unwrap().insert(id.to_string(), tx);
        rx
    }

    /// One request → its `result`, or the `error` object (never a silent
    /// success).
    pub fn call(&self, op: &str, mut params: Value) -> Result<Value, Value> {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        let id = format!("g{n}");
        params["id"] = json!(id);
        params["op"] = json!(op);
        let rx = self.pending_for(&id);
        self.send(&params).expect("send request");
        let answer = rx
            .recv_timeout(Duration::from_secs(45))
            .unwrap_or_else(|e| panic!("{op}: no answer ({e})"));
        if answer["ok"].as_bool().unwrap_or(false) {
            Ok(answer["result"].clone())
        } else {
            Err(answer["error"].clone())
        }
    }

    /// `hello` (WIRE.md §4.1).
    pub fn hello(&self) -> Value {
        self.call("hello", json!({"proto": 2, "client": "acos-webview-test/0"}))
            .expect("hello")
    }

    /// `render` (JSON result, then the PNG blob, in that order).
    pub fn render(&self, view: i64) -> (Value, Blob) {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        let id = format!("g{n}");
        let rx = self.pending_for(&id);
        self.send(&json!({"id": id, "op": "render", "view": view, "format": "png"}))
            .expect("send render");
        let answer = rx.recv_timeout(Duration::from_secs(45)).expect("render json");
        assert_eq!(
            answer["ok"].as_bool(),
            Some(true),
            "render failed: {}",
            answer["error"]
        );
        let blob = self
            .blobs
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(45))
            .expect("render blob");
        assert_eq!(blob.id, id, "the blob must belong to the JSON before it");
        (answer["result"].clone(), blob)
    }

    /// Close the connection the way the provider does at session end: shut the
    /// socket down (both directions), so the peer sees EOF and the reader thread
    /// exits. A plain `drop` is not enough in a test: the reader thread's own
    /// dup of the fd keeps the socket end open.
    pub fn close(&self) {
        if let Ok(s) = self.writer.lock() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    pub fn seen(&self) -> Vec<(String, Value)> {
        self.seen.lock().unwrap().clone()
    }
}

/// PNG magic: a frame that is not a PNG must never be reported as one.
pub fn assert_png(bytes: &[u8]) {
    assert!(
        bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "expected a PNG frame, got {} bytes starting {:?}",
        bytes.len(),
        &bytes[..bytes.len().min(8)]
    );
}
