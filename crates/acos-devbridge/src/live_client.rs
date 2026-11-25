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
    state: Mutex<HubState>,
    seq: Arc<AtomicU32>,
}

struct HubState {
    generation: u64,
    active_clients: usize,
    reader_alive: bool,
    shutting_down: bool,
    tx: Option<watch::Sender<Option<RawRgbFrame>>>,
    reader: Option<ReaderHandle>,
}

struct ReaderHandle {
    stop: Arc<AtomicBool>,
    stream: UnixStream,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ReaderHandle {
    fn shutdown(mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.stream.shutdown(Shutdown::Both);
        drop(self.stream);
        if let Some(task) = self.task.take() {
            drop(task);
        }
    }
}

pub struct LiveSubscription {
    hub: Arc<LiveStreamHub>,
    generation: u64,
    pub rx: watch::Receiver<Option<RawRgbFrame>>,
}

impl Drop for LiveSubscription {
    fn drop(&mut self) {
        self.hub.release_client(self.generation);
    }
}

impl LiveStreamHub {
    pub fn new(stream_socket: String) -> Arc<Self> {
        Arc::new(Self {
            stream_socket,
            state: Mutex::new(HubState {
                generation: 0,
                active_clients: 0,
                reader_alive: false,
                shutting_down: false,
                tx: None,
                reader: None,
            }),
            seq: Arc::new(AtomicU32::new(0)),
        })
    }

    pub fn subscribe(self: &Arc<Self>) -> Result<LiveSubscription, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.shutting_down {
            return Err("live stream hub is shutting down".to_string());
        }
        if !state.reader_alive || state.tx.is_none() {
            self.start_reader_locked(&mut state)?;
        }
        let tx = state
            .tx
            .as_ref()
            .ok_or_else(|| "live stream channel unavailable".to_string())?;
        let mut rx = tx.subscribe();
        // Reset cached value check so caller sees newly arriving frames, or sees
        // whatever frame is currently in `watch` right after attaching.
        rx.mark_changed();
        state.active_clients += 1;
        let generation = state.generation;
        Ok(LiveSubscription {
            hub: Arc::clone(self),
            generation,
            rx,
        })
    }

    pub fn shutdown(&self) {
        let (tx, reader) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.shutting_down = true;
            state.active_clients = 0;
            state.reader_alive = false;
            (state.tx.take(), state.reader.take())
        };
        if let Some(reader) = reader {
            reader.shutdown();
        }
        drop(tx);
    }

    fn release_client(&self, generation: u64) {
        let to_stop = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.generation != generation {
                return;
            }
            if state.active_clients > 0 {
                state.active_clients -= 1;
            }
            if state.active_clients == 0 {
                state.reader_alive = false;
                Some((state.tx.take(), state.reader.take()))
            } else {
                None
            }
        };
        if let Some((tx, reader)) = to_stop {
            if let Some(reader) = reader {
                reader.shutdown();
            }
            drop(tx);
        }
    }

    fn on_reader_exit(&self, generation: u64) {
        let to_drop = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.generation != generation {
                return;
            }
            state.reader_alive = false;
            state.active_clients = 0;
            (state.tx.take(), state.reader.take())
        };
        drop(to_drop);
    }

    fn start_reader_locked(self: &Arc<Self>, state: &mut HubState) -> Result<(), String> {
        if let Some(old) = state.reader.take() {
            old.shutdown();
        }
        drop(state.tx.take());
        state.reader_alive = false;
        state.active_clients = 0;
        state.generation = state.generation.wrapping_add(1);
        let gen = state.generation;

        let stream = UnixStream::connect(Path::new(&self.stream_socket))
            .map_err(|e| format!("connect live_view socket {}: {e}", self.stream_socket))?;
        let stop_stream = stream
            .try_clone()
            .map_err(|e| format!("clone live_view socket: {e}"))?;
        let (tx, _) = watch::channel(None);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let tx_bg = tx.clone();
        let seq = Arc::clone(&self.seq);
        let hub = Arc::clone(self);
        let task = tokio::task::spawn_blocking(move || {
            run_live_reader(stream, stop_bg, tx_bg, seq);
            hub.on_reader_exit(gen);
        });
        state.tx = Some(tx);
        state.reader_alive = true;
        state.reader = Some(ReaderHandle {
            stop,
            stream: stop_stream,
            task: Some(task),
        });
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener as StdUnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    fn scratch_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = PathBuf::from("/tmp").join(format!(
            "acos-t-lc-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn write_frame_msg(stream: &mut UnixStream, fill: u8) {
        let header = serde_json::json!({
            "seq": 1,
            "generation": 1,
            "w": 2,
            "h": 2,
            "format": "rgb888",
            "ts_ms": 123,
        });
        let mut payload = header.to_string().into_bytes();
        payload.push(b'\n');
        payload.extend_from_slice(&[fill; 2 * 2 * 3]);
        let len = (payload.len() + 1) as u32;
        stream.write_all(&len.to_be_bytes()).expect("len");
        stream.write_all(b"F").expect("type");
        stream.write_all(&payload).expect("payload");
        stream.flush().expect("flush");
    }

    async fn next_frame(sub: &mut LiveSubscription) -> Result<RawRgbFrame, &'static str> {
        loop {
            match tokio::time::timeout(Duration::from_secs(1), sub.rx.changed()).await {
                Err(_) => return Err("timeout"),
                Ok(Err(_)) => return Err("closed"),
                Ok(Ok(())) => {
                    if let Some(frame) = sub.rx.borrow_and_update().clone() {
                        return Ok(frame);
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn live_reader_eof_closes_subscriber_and_allows_reconnect_while_held() {
        let dir = scratch_dir("eof");
        let sock = dir.join("v.sock");
        let listener = Arc::new(StdUnixListener::bind(&sock).expect("bind view sock"));
        let hub = LiveStreamHub::new(sock.display().to_string());

        // 1. First subscriber attaches, gets 1 frame, then server closes socket (EOF).
        let l1 = Arc::clone(&listener);
        let t1 = tokio::task::spawn_blocking(move || l1.accept().expect("accept 1"));
        let mut sub1 = hub.subscribe().expect("subscribe 1");
        let (mut s1, _) = t1.await.expect("accept 1 join");
        let mut line = String::new();
        let mut br1 = BufReader::new(s1.try_clone().expect("clone 1"));
        br1.read_line(&mut line).expect("read subscribe 1");
        write_frame_msg(&mut s1, 11);
        drop(s1);
        drop(br1);

        let f1 = next_frame(&mut sub1).await.expect("frame 1");
        assert_eq!(f1.rgb888[0], 11);

        // EOF MUST terminate sub1.rx.changed() with closed rather than hanging.
        let eof_res = next_frame(&mut sub1).await;
        assert_eq!(eof_res.err(), Some("closed"), "sub1 must see closed on EOF");

        // 2. While sub1 is STILL alive, a new subscriber starts a fresh reader and receives frame 2.
        let l2 = Arc::clone(&listener);
        let t2 = tokio::task::spawn_blocking(move || l2.accept().expect("accept 2"));
        let mut sub2 = hub.subscribe().expect("subscribe 2");
        let (mut s2, _) = tokio::time::timeout(Duration::from_secs(1), t2)
            .await
            .expect("fresh reader must connect even while sub1 is held")
            .expect("accept 2 join");
        line.clear();
        let mut br2 = BufReader::new(s2.try_clone().expect("clone 2"));
        br2.read_line(&mut line).expect("read subscribe 2");
        write_frame_msg(&mut s2, 22);

        let f2 = next_frame(&mut sub2).await.expect("frame 2");
        assert_eq!(f2.rgb888[0], 22);

        // Dropping old sub1 must NOT kill sub2's active reader.
        drop(sub1);
        write_frame_msg(&mut s2, 33);
        let f3 = next_frame(&mut sub2).await.expect("frame 3");
        assert_eq!(f3.rgb888[0], 33);

        drop(s2);
        drop(br2);
        drop(sub2);
        hub.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
