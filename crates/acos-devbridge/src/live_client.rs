use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

/// Latest RGB888 frame received from `LiveView` over `view.sock`.
#[derive(Clone, Debug)]
pub struct RawRgbFrame {
    pub seq: u32,
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
    /// Top-down RGB24 pixels (`width * height * 3` bytes).
    pub rgb888: Arc<Vec<u8>>,
}

/// Manages a lazy `LiveView` Unix-socket subscription for one session:
/// connects on the first `streamScreenshot` caller and drops the socket
/// immediately when the last caller disconnects.
pub struct LiveStreamHub {
    stream_socket: String,
    active_clients: Mutex<usize>,
    tx: watch::Sender<Option<RawRgbFrame>>,
    reader: Mutex<Option<ReaderHandle>>,
    seq: Arc<AtomicU32>,
}

struct ReaderHandle {
    stop: Arc<AtomicBool>,
    stream: UnixStream,
    task: Option<tokio::task::JoinHandle<()>>,
}

pub struct LiveSubscription {
    hub: Arc<LiveStreamHub>,
    pub rx: watch::Receiver<Option<RawRgbFrame>>,
}

impl Drop for LiveSubscription {
    fn drop(&mut self) {
        self.hub.release_client();
    }
}

impl LiveStreamHub {
    pub fn new(stream_socket: String) -> Arc<Self> {
        let (tx, _) = watch::channel(None);
        Arc::new(Self {
            stream_socket,
            active_clients: Mutex::new(0),
            tx,
            reader: Mutex::new(None),
            seq: Arc::new(AtomicU32::new(0)),
        })
    }

    pub fn subscribe(self: &Arc<Self>) -> Result<LiveSubscription, String> {
        let mut count = self.active_clients.lock().unwrap_or_else(|e| e.into_inner());
        let mut rx = self.tx.subscribe();
        // Reset cached value check so caller sees newly arriving frames, or sees
        // whatever frame is currently in `watch` right after attaching.
        rx.mark_changed();
        if *count == 0 {
            self.start_reader_locked()?;
        }
        *count += 1;
        Ok(LiveSubscription {
            hub: Arc::clone(self),
            rx,
        })
    }

    pub fn shutdown(&self) {
        self.stop_reader();
    }

    fn release_client(&self) {
        let mut count = self.active_clients.lock().unwrap_or_else(|e| e.into_inner());
        if *count > 0 {
            *count -= 1;
        }
        if *count == 0 {
            self.stop_reader();
        }
    }

    fn start_reader_locked(&self) -> Result<(), String> {
        // Clear any stale previous frame before attaching a new stream socket.
        let _ = self.tx.send(None);
        let stream = UnixStream::connect(Path::new(&self.stream_socket))
            .map_err(|e| format!("connect live_view socket {}: {e}", self.stream_socket))?;
        let stop_stream = stream
            .try_clone()
            .map_err(|e| format!("clone live_view socket: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let tx = self.tx.clone();
        let seq = Arc::clone(&self.seq);
        let task = tokio::task::spawn_blocking(move || {
            run_live_reader(stream, stop_bg, tx, seq);
        });
        let mut guard = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(ReaderHandle {
            stop,
            stream: stop_stream,
            task: Some(task),
        });
        Ok(())
    }

    fn stop_reader(&self) {
        let handle = self.reader.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut handle) = handle {
            handle.stop.store(true, Ordering::Release);
            let _ = handle.stream.shutdown(Shutdown::Both);
            drop(handle.stream);
            if let Some(task) = handle.task.take() {
                drop(task);
            }
        }
        let _ = self.tx.send(None);
    }
}

fn run_live_reader(
    mut stream: UnixStream,
    stop: Arc<AtomicBool>,
    tx: watch::Sender<Option<RawRgbFrame>>,
    seq_counter: Arc<AtomicU32>,
) {
    let subscribe_line =
        "{\"v\":1,\"subscribe\":{\"frames\":true,\"events\":false,\"max_fps\":30,\"scale\":1.0,\"format\":\"rgb888\"}}\n";
    if stream.write_all(subscribe_line.as_bytes()).is_err() || stream.flush().is_err() {
        return;
    }

    let mut reader = BufReader::new(stream);
    while !stop.load(Ordering::Acquire) {
        let mut len_buf = [0u8; 4];
        if reader.read_exact(&mut len_buf).is_err() {
            break;
        }
        let total_len = u32::from_be_bytes(len_buf) as usize;
        if total_len == 0 || total_len > 64 * 1024 * 1024 {
            break;
        }
        let mut msg_type = [0u8; 1];
        if reader.read_exact(&mut msg_type).is_err() {
            break;
        }
        let payload_len = total_len - 1;
        if msg_type[0] != b'F' {
            // Skip Hello (`H`) / Event (`E`) without allocating giant buffers.
            if io_skip_exact(&mut reader, payload_len).is_err() {
                break;
            }
            continue;
        }

        let mut header_line = Vec::new();
        match reader.read_until(b'\n', &mut header_line) {
            Ok(0) | Err(_) => break,
            Ok(n) if n > payload_len => break,
            Ok(_) => {}
        }
        let img_len = payload_len - header_line.len();
        let mut img_bytes = vec![0u8; img_len];
        if reader.read_exact(&mut img_bytes).is_err() {
            break;
        }

        let Ok(header) = serde_json::from_slice::<serde_json::Value>(&header_line) else {
            continue;
        };
        let w = header.get("w").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let h = header.get("h").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let fmt = header
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("rgb888");
        if w == 0 || h == 0 {
            continue;
        }

        let rgb888 = match fmt {
            "rgb888" => {
                if img_bytes.len() != w as usize * h as usize * 3 {
                    continue;
                }
                img_bytes
            }
            "png" | "jpeg" => match image::load_from_memory(&img_bytes) {
                Ok(img) => img.to_rgb8().into_raw(),
                Err(_) => continue,
            },
            _ => continue,
        };

        let ts_us = header
            .get("ts_ms")
            .and_then(|v| v.as_u64())
            .map(|ms| ms * 1000)
            .unwrap_or_else(now_micros);
        let seq = seq_counter.fetch_add(1, Ordering::Relaxed);
        let frame = RawRgbFrame {
            seq,
            width: w,
            height: h,
            timestamp_us: ts_us,
            rgb888: Arc::new(rgb888),
        };
        if tx.send(Some(frame)).is_err() {
            break;
        }
    }
}

fn io_skip_exact(reader: &mut impl Read, mut remaining: usize) -> std::io::Result<()> {
    let mut scratch = [0u8; 4096];
    while remaining > 0 {
        let take = remaining.min(scratch.len());
        reader.read_exact(&mut scratch[..take])?;
        remaining -= take;
    }
    Ok(())
}

pub fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}
