//! Minimal ADB transport wire protocol (multiplexed TCP streams, `shell_v2` + `exec`).

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::shell::{self, AdbSessionOps, AdbShellState};

const A_SYNC: u32 = 0x434e_5953;
const A_CNXN: u32 = 0x4e58_4e43;
const A_OPEN: u32 = 0x4e45_504f;
const A_OKAY: u32 = 0x5941_4b4f;
const A_CLSE: u32 = 0x4553_4c43;
const A_WRTE: u32 = 0x4554_5257;

const A_VERSION: u32 = 0x0100_0000;
const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
const OKAY_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

const ID_STDOUT: u8 = 1;
const ID_STDERR: u8 = 2;
const ID_EXIT: u8 = 3;

#[derive(Debug)]
struct Packet {
    command: u32,
    arg0: u32,
    arg1: u32,
    payload: Vec<u8>,
}

fn read_packet(stream: &mut TcpStream) -> io::Result<Packet> {
    let mut hdr = [0u8; 24];
    stream.read_exact(&mut hdr)?;
    let command = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
    let arg0 = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
    let arg1 = u32::from_le_bytes(hdr[8..12].try_into().unwrap());
    let data_length = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
    let _data_check = u32::from_le_bytes(hdr[16..20].try_into().unwrap());
    let magic = u32::from_le_bytes(hdr[20..24].try_into().unwrap());
    if magic != (command ^ 0xffff_ffff) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid adb packet magic",
        ));
    }
    if data_length > MAX_PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "adb packet payload exceeds limit",
        ));
    }
    let mut payload = vec![0u8; data_length];
    if data_length > 0 {
        stream.read_exact(&mut payload)?;
    }
    Ok(Packet {
        command,
        arg0,
        arg1,
        payload,
    })
}

fn write_packet(
    writer: &Mutex<TcpStream>,
    command: u32,
    arg0: u32,
    arg1: u32,
    payload: &[u8],
) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "adb payload too large"))?;
    let crc = payload
        .iter()
        .fold(0u32, |acc, &b| acc.wrapping_add(u32::from(b)));
    let magic = command ^ 0xffff_ffff;
    let mut hdr = [0u8; 24];
    hdr[0..4].copy_from_slice(&command.to_le_bytes());
    hdr[4..8].copy_from_slice(&arg0.to_le_bytes());
    hdr[8..12].copy_from_slice(&arg1.to_le_bytes());
    hdr[12..16].copy_from_slice(&len.to_le_bytes());
    hdr[16..20].copy_from_slice(&crc.to_le_bytes());
    hdr[20..24].copy_from_slice(&magic.to_le_bytes());

    let mut guard = writer.lock().unwrap_or_else(|e| e.into_inner());
    let res = (|| -> io::Result<()> {
        guard.write_all(&hdr)?;
        if !payload.is_empty() {
            guard.write_all(payload)?;
        }
        guard.flush()
    })();
    if res.is_err() {
        // A partial write or timeout leaves the multiplexed stream mid-frame;
        // shut down immediately so readers/other workers unwind without corrupting frames.
        let _ = guard.shutdown(Shutdown::Both);
    }
    res
}

struct StreamState {
    remote_id: u32,
    okay_tx: Sender<()>,
    in_tx: Option<Sender<Vec<u8>>>,
    peer_closed: AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceMode<'a> {
    ShellV2(&'a str),
    ShellV1(&'a str),
    Exec(&'a str),
    Sync,
    Unsupported(&'a str),
}

fn parse_service(raw: &str) -> ServiceMode<'_> {
    let trimmed = raw.trim_end_matches('\0');
    if let Some((head, cmd)) = trimmed.split_once(':') {
        if head == "sync" {
            return ServiceMode::Sync;
        }
        if head == "exec" {
            return ServiceMode::Exec(cmd);
        }
        if head == "shell" {
            return ServiceMode::ShellV1(cmd);
        }
        let mut parts = head.split(',');
        if parts.next() == Some("shell") {
            if parts.any(|p| p == "v2") {
                return ServiceMode::ShellV2(cmd);
            }
            return ServiceMode::ShellV1(cmd);
        }
    }
    ServiceMode::Unsupported(trimmed)
}

/// Serve one adb server TCP connection until EOF, error, or `closing` is set.
pub fn handle_connection(
    stream: TcpStream,
    target: Arc<dyn AdbSessionOps>,
    shell_state: Arc<Mutex<AdbShellState>>,
    closing: Arc<AtomicBool>,
) {
    handle_connection_with_write_timeout(stream, target, shell_state, closing, WRITE_TIMEOUT);
}

pub(crate) fn handle_connection_with_write_timeout(
    mut stream: TcpStream,
    target: Arc<dyn AdbSessionOps>,
    shell_state: Arc<Mutex<AdbShellState>>,
    closing: Arc<AtomicBool>,
    write_timeout: Duration,
) {
    let _ = stream.set_nodelay(true);
    let Ok(writer_stream) = stream.try_clone() else {
        return;
    };
    if writer_stream.set_write_timeout(Some(write_timeout)).is_err() {
        return;
    }
    let writer = Arc::new(Mutex::new(writer_stream));
    let streams: Arc<Mutex<HashMap<u32, Arc<StreamState>>>> = Arc::new(Mutex::new(HashMap::new()));
    let next_local_id = AtomicU32::new(1);
    let mut maxdata: usize = 64 * 1024;

    loop {
        if closing.load(Ordering::Acquire) {
            break;
        }
        let pkt = match read_packet(&mut stream) {
            Ok(p) => p,
            Err(_) => break,
        };
        match pkt.command {
            A_SYNC => {
                // Legacy sync ping; echo cleanly.
                let _ = write_packet(&writer, A_SYNC, pkt.arg0, pkt.arg1, &[]);
            }
            A_CNXN => {
                let peer_max = pkt.arg1 as usize;
                maxdata = peer_max.clamp(4096, 1024 * 1024);
                let banner = b"device::ro.product.name=acos;ro.product.model=ACOS;ro.product.device=acos;features=shell_v2,cmd\0";
                if write_packet(&writer, A_CNXN, A_VERSION, maxdata as u32, banner).is_err() {
                    break;
                }
            }
            A_OPEN => {
                let remote_id = pkt.arg0;
                let service_str = String::from_utf8_lossy(&pkt.payload).into_owned();
                let (okay_tx, okay_rx) = mpsc::channel();
                let mut local_id = next_local_id.fetch_add(1, Ordering::Relaxed);
                if local_id == 0 {
                    local_id = next_local_id.fetch_add(1, Ordering::Relaxed);
                }

                match parse_service(&service_str) {
                    ServiceMode::Unsupported(_) => {
                        // Reject unknown service immediately per adb wire spec.
                        let _ = write_packet(&writer, A_CLSE, 0, remote_id, &[]);
                    }
                    ServiceMode::Sync => {
                        let (in_tx, in_rx) = mpsc::channel();
                        let state_entry = Arc::new(StreamState {
                            remote_id,
                            okay_tx,
                            in_tx: Some(in_tx),
                            peer_closed: AtomicBool::new(false),
                        });
                        streams
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(local_id, Arc::clone(&state_entry));
                        if write_packet(&writer, A_OKAY, local_id, remote_id, &[]).is_err() {
                            streams
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .remove(&local_id);
                            break;
                        }
                        let writer = Arc::clone(&writer);
                        let streams = Arc::clone(&streams);
                        std::thread::spawn(move || {
                            run_sync_worker(
                                local_id,
                                remote_id,
                                writer,
                                streams,
                                state_entry,
                                in_rx,
                                okay_rx,
                                maxdata,
                            );
                        });
                    }
                    ServiceMode::ShellV2(cmd)
                    | ServiceMode::ShellV1(cmd)
                    | ServiceMode::Exec(cmd) => {
                        let mode = match parse_service(&service_str) {
                            ServiceMode::ShellV2(_) => 2u8,
                            ServiceMode::Exec(_) => 0u8,
                            _ => 1u8,
                        };
                        let cmd = cmd.to_string();
                        let state_entry = Arc::new(StreamState {
                            remote_id,
                            okay_tx,
                            in_tx: None,
                            peer_closed: AtomicBool::new(false),
                        });
                        streams
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(local_id, Arc::clone(&state_entry));
                        if write_packet(&writer, A_OKAY, local_id, remote_id, &[]).is_err() {
                            streams
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .remove(&local_id);
                            break;
                        }
                        let writer = Arc::clone(&writer);
                        let streams = Arc::clone(&streams);
                        let target = Arc::clone(&target);
                        let shell_state = Arc::clone(&shell_state);
                        std::thread::spawn(move || {
                            run_stream_worker(
                                local_id,
                                remote_id,
                                mode,
                                &cmd,
                                target,
                                shell_state,
                                writer,
                                streams,
                                state_entry,
                                okay_rx,
                                maxdata,
                            );
                        });
                    }
                }
            }
            A_OKAY => {
                let local_id = pkt.arg1;
                let entry = streams
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&local_id)
                    .cloned();
                if let Some(entry) = entry {
                    let _ = entry.okay_tx.send(());
                }
            }
            A_WRTE => {
                let local_id = pkt.arg1;
                let entry = streams
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&local_id)
                    .cloned();
                if let Some(entry) = entry {
                    if let Some(in_tx) = &entry.in_tx {
                        let _ = in_tx.send(pkt.payload);
                    }
                    let _ = write_packet(&writer, A_OKAY, local_id, entry.remote_id, &[]);
                }
            }
            A_CLSE => {
                let local_id = pkt.arg1;
                let entry = streams
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&local_id);
                if let Some(entry) = entry {
                    entry.peer_closed.store(true, Ordering::Release);
                    let _ = write_packet(&writer, A_CLSE, local_id, entry.remote_id, &[]);
                }
            }
            _ => {}
        }
    }

    // Mark all open streams closed so workers unwind promptly.
    {
        let mut guard = streams.lock().unwrap_or_else(|e| e.into_inner());
        for (_, entry) in guard.drain() {
            entry.peer_closed.store(true, Ordering::Release);
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

#[allow(clippy::too_many_arguments)]
fn run_stream_worker(
    local_id: u32,
    remote_id: u32,
    mode: u8, // 2 = shell_v2, 1 = shell_v1, 0 = exec
    cmd: &str,
    target: Arc<dyn AdbSessionOps>,
    shell_state: Arc<Mutex<AdbShellState>>,
    writer: Arc<Mutex<TcpStream>>,
    streams: Arc<Mutex<HashMap<u32, Arc<StreamState>>>>,
    state_entry: Arc<StreamState>,
    okay_rx: Receiver<()>,
    maxdata: usize,
) {
    let out = shell::run_command(cmd, target.as_ref(), &shell_state);
    if mode == 2 {
        let max_v2_chunk = maxdata.saturating_sub(5).max(1024);
        if !out.stdout.is_empty() {
            let _ = send_v2_frames(
                &writer,
                &state_entry,
                &okay_rx,
                local_id,
                remote_id,
                ID_STDOUT,
                &out.stdout,
                max_v2_chunk,
                maxdata,
            );
        }
        if !out.stderr.is_empty() {
            let _ = send_v2_frames(
                &writer,
                &state_entry,
                &okay_rx,
                local_id,
                remote_id,
                ID_STDERR,
                &out.stderr,
                max_v2_chunk,
                maxdata,
            );
        }
        let _ = send_v2_frames(
            &writer,
            &state_entry,
            &okay_rx,
            local_id,
            remote_id,
            ID_EXIT,
            &[out.exit_code],
            max_v2_chunk,
            maxdata,
        );
    } else if mode == 0 {
        // `exec:<cmd>` raw stream (e.g. `exec-out screencap -p`): stdout if ok, else stderr.
        let bytes = if out.exit_code == 0 || !out.stdout.is_empty() {
            &out.stdout
        } else {
            &out.stderr
        };
        if !bytes.is_empty() {
            let _ = send_wrte_all(&writer, &state_entry, &okay_rx, local_id, remote_id, bytes, maxdata);
        }
    } else {
        // Legacy `shell:<cmd>` merges stdout and stderr.
        if !out.stdout.is_empty() {
            let _ = send_wrte_all(&writer, &state_entry, &okay_rx, local_id, remote_id, &out.stdout, maxdata);
        }
        if !out.stderr.is_empty() {
            let _ = send_wrte_all(&writer, &state_entry, &okay_rx, local_id, remote_id, &out.stderr, maxdata);
        }
    }

    if !state_entry.peer_closed.load(Ordering::Acquire) {
        let _ = write_packet(&writer, A_CLSE, local_id, remote_id, &[]);
    }
    streams
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&local_id);
}

#[allow(clippy::too_many_arguments)]
fn send_v2_frames(
    writer: &Mutex<TcpStream>,
    state_entry: &StreamState,
    okay_rx: &Receiver<()>,
    local_id: u32,
    remote_id: u32,
    stream_id: u8,
    data: &[u8],
    chunk_limit: usize,
    maxdata: usize,
) -> io::Result<()> {
    for chunk in data.chunks(chunk_limit) {
        let mut frame = Vec::with_capacity(5 + chunk.len());
        frame.push(stream_id);
        frame.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        frame.extend_from_slice(chunk);
        send_wrte_all(writer, state_entry, okay_rx, local_id, remote_id, &frame, maxdata)?;
    }
    Ok(())
}

fn send_wrte_all(
    writer: &Mutex<TcpStream>,
    state_entry: &StreamState,
    okay_rx: &Receiver<()>,
    local_id: u32,
    remote_id: u32,
    data: &[u8],
    maxdata: usize,
) -> io::Result<()> {
    for chunk in data.chunks(maxdata.max(1024)) {
        if state_entry.peer_closed.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "adb stream closed"));
        }
        write_packet(writer, A_WRTE, local_id, remote_id, chunk)?;
        match okay_rx.recv_timeout(OKAY_WAIT_TIMEOUT) {
            Ok(()) => {}
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for adb OKAY",
                ));
            }
        }
    }
    Ok(())
}

struct SyncReader {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl SyncReader {
    fn new(rx: Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            buf: Vec::new(),
            pos: 0,
        }
    }

    fn read_exact_timeout(&mut self, mut dst: &mut [u8]) -> io::Result<()> {
        while !dst.is_empty() {
            if self.pos < self.buf.len() {
                let avail = &self.buf[self.pos..];
                let take = avail.len().min(dst.len());
                dst[..take].copy_from_slice(&avail[..take]);
                self.pos += take;
                dst = &mut dst[take..];
                continue;
            }
            match self.rx.recv_timeout(OKAY_WAIT_TIMEOUT) {
                Ok(chunk) => {
                    self.buf = chunk;
                    self.pos = 0;
                }
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "sync stream ended",
                    ));
                }
            }
        }
        Ok(())
    }

    fn discard_exact(&mut self, mut len: usize) -> io::Result<()> {
        let mut scratch = [0u8; 4096];
        while len > 0 {
            let step = len.min(scratch.len());
            self.read_exact_timeout(&mut scratch[..step])?;
            len -= step;
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn run_sync_worker(
    local_id: u32,
    remote_id: u32,
    writer: Arc<Mutex<TcpStream>>,
    streams: Arc<Mutex<HashMap<u32, Arc<StreamState>>>>,
    state_entry: Arc<StreamState>,
    in_rx: Receiver<Vec<u8>>,
    okay_rx: Receiver<()>,
    maxdata: usize,
) {
    let mut reader = SyncReader::new(in_rx);
    let msg = b"acos: unsupported: file sync / install into a live session is not supported; launch a new session with the APK";
    loop {
        if state_entry.peer_closed.load(Ordering::Acquire) {
            break;
        }
        let mut hdr = [0u8; 8];
        if reader.read_exact_timeout(&mut hdr).is_err() {
            break;
        }
        let tag = &hdr[0..4];
        let arg = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
        if tag == b"QUIT" {
            break;
        }
        // Drain request payload (`<path>` or `<path>,<mode>`).
        if arg > 64 * 1024 || reader.discard_exact(arg).is_err() {
            break;
        }
        match tag {
            b"STAT" => {
                // Report non-existent so `adb install` proceeds to SEND where we reject cleanly.
                let mut resp = [0u8; 16];
                resp[0..4].copy_from_slice(b"STAT");
                let _ = send_wrte_all(&writer, &state_entry, &okay_rx, local_id, remote_id, &resp, maxdata);
            }
            b"STA2" => {
                let mut resp = [0u8; 72];
                resp[0..4].copy_from_slice(b"STA2");
                resp[4..8].copy_from_slice(&2u32.to_le_bytes()); // ENOENT
                let _ = send_wrte_all(&writer, &state_entry, &okay_rx, local_id, remote_id, &resp, maxdata);
            }
            b"SEND" => {
                // Drain DATA chunks up to DONE so adb doesn't fail with EPIPE before reading FAIL.
                loop {
                    let mut sub = [0u8; 8];
                    if reader.read_exact_timeout(&mut sub).is_err() {
                        break;
                    }
                    let stag = &sub[0..4];
                    let sarg = u32::from_le_bytes(sub[4..8].try_into().unwrap()) as usize;
                    if stag == b"DATA" {
                        if sarg > 4 * 1024 * 1024 || reader.discard_exact(sarg).is_err() {
                            break;
                        }
                    } else {
                        // DONE or unexpected
                        break;
                    }
                }
                let mut fail = Vec::with_capacity(8 + msg.len());
                fail.extend_from_slice(b"FAIL");
                fail.extend_from_slice(&(msg.len() as u32).to_le_bytes());
                fail.extend_from_slice(msg);
                let _ = send_wrte_all(&writer, &state_entry, &okay_rx, local_id, remote_id, &fail, maxdata);
                break;
            }
            _ => {
                let mut fail = Vec::with_capacity(8 + msg.len());
                fail.extend_from_slice(b"FAIL");
                fail.extend_from_slice(&(msg.len() as u32).to_le_bytes());
                fail.extend_from_slice(msg);
                let _ = send_wrte_all(&writer, &state_entry, &okay_rx, local_id, remote_id, &fail, maxdata);
                break;
            }
        }
    }

    if !state_entry.peer_closed.load(Ordering::Acquire) {
        let _ = write_packet(&writer, A_CLSE, local_id, remote_id, &[]);
    }
    streams
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&local_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::os::unix::io::AsRawFd;
    use std::path::{Path, PathBuf};

    struct LargeShotMockOps;

    impl AdbSessionOps for LargeShotMockOps {
        fn snapshot(&self) -> Result<shell::AdbSessionSnapshot, String> {
            Ok(shell::AdbSessionSnapshot {
                package: "com.example.test".to_string(),
                activity: None,
                natural_size: (1080, 2400),
                display_size: (1080, 2400),
                rotation: 0,
                density: 420,
                animations: "on",
                guest_pid: None,
                launcher_pid: None,
                apk_path: PathBuf::from("/tmp/app.apk"),
                device_name: None,
                extra_apks: Vec::new(),
            })
        }
        fn rotate(&self, _rotation: u8) -> Result<(), String> {
            Ok(())
        }
        fn dump_tree(&self) -> Result<(acos_protocol::ui::UiTree, u8), String> {
            Err("unused".to_string())
        }
        fn screenshot_png(&self) -> Result<Vec<u8>, String> {
            Ok(vec![0xAB; 2 * 1024 * 1024])
        }
        fn raw_screenshot(&self, _out: &Path, _fmt: &str) -> Result<(u32, u32, u8), String> {
            Err("unused".to_string())
        }
        fn panel_touch(&self, _: i32, _: i64, _: i64, _: &[(i32, f32, f32)]) -> Result<(), String> {
            Ok(())
        }
        fn panel_key(&self, _: i32, _: &str, _: i32, _: i32) -> Result<(), String> {
            Ok(())
        }
        fn panel_text(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn adb_tap(&self, _: i32, _: i32) -> Result<(), String> {
            Ok(())
        }
        fn adb_swipe(&self, _: i32, _: i32, _: i32, _: i32, _: u64) -> Result<(), String> {
            Ok(())
        }
        fn adb_key(&self, _: i32, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn adb_text(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn stream_socket(&self) -> Option<PathBuf> {
            None
        }
        fn is_verify_run(&self) -> bool {
            false
        }
    }

    fn shrink_tcp_buffers(stream: &TcpStream, bytes: libc::c_int) {
        unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&bytes as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&bytes as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }

    fn send_raw_pkt(stream: &mut TcpStream, command: u32, arg0: u32, arg1: u32, payload: &[u8]) {
        let len = payload.len() as u32;
        let crc = payload
            .iter()
            .fold(0u32, |acc, &b| acc.wrapping_add(u32::from(b)));
        let magic = command ^ 0xffff_ffff;
        let mut hdr = [0u8; 24];
        hdr[0..4].copy_from_slice(&command.to_le_bytes());
        hdr[4..8].copy_from_slice(&arg0.to_le_bytes());
        hdr[8..12].copy_from_slice(&arg1.to_le_bytes());
        hdr[12..16].copy_from_slice(&len.to_le_bytes());
        hdr[16..20].copy_from_slice(&crc.to_le_bytes());
        hdr[20..24].copy_from_slice(&magic.to_le_bytes());
        stream.write_all(&hdr).expect("write hdr");
        if !payload.is_empty() {
            stream.write_all(payload).expect("write payload");
        }
        stream.flush().expect("flush");
    }

    #[test]
    fn stalled_peer_write_times_out_and_shuts_down_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");

        let mut client = TcpStream::connect(addr).expect("connect");
        shrink_tcp_buffers(&client, 4096);
        let (server, _) = listener.accept().expect("accept");
        shrink_tcp_buffers(&server, 4096);

        let closing = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = mpsc::channel();
        let closing_srv = Arc::clone(&closing);
        std::thread::spawn(move || {
            handle_connection_with_write_timeout(
                server,
                Arc::new(LargeShotMockOps),
                Arc::new(Mutex::new(AdbShellState::default())),
                closing_srv,
                Duration::from_millis(150),
            );
            let _ = done_tx.send(());
        });

        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        send_raw_pkt(&mut client, A_CNXN, A_VERSION, 1024 * 1024, b"host::\0");
        let cnxn = read_packet(&mut client).expect("read A_CNXN");
        assert_eq!(cnxn.command, A_CNXN);

        // Open `exec:screencap -p` (2 MiB payload) and read A_OKAY, then STOP reading.
        send_raw_pkt(&mut client, A_OPEN, 7, 0, b"exec:screencap -p\0");
        let okay = read_packet(&mut client).expect("read A_OKAY");
        assert_eq!(okay.command, A_OKAY);

        // With bounded write timeout, the stalled write must time out and tear down
        // the multiplexed connection rather than blocking forever.
        let res = done_rx.recv_timeout(Duration::from_secs(1));
        closing.store(true, Ordering::Release);
        drop(client);
        assert!(
            res.is_ok(),
            "handle_connection blocked indefinitely on stalled peer write"
        );
    }
}

