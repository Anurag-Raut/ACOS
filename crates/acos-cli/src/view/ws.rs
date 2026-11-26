//! Minimal RFC 6455 WebSocket server framing for `acos view` (`/api/input/<session_id>`).
//!
//! Keeps zero external WebSocket dependencies by reusing `acos_dex::hash::sha1`
//! for the opening handshake (`Sec-WebSocket-Accept`). Enforces client masking
//! and a bounded per-message cap (`MAX_WS_PAYLOAD`) so a rogue local peer
//! cannot allocate unbounded memory.

use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Maximum accepted WebSocket payload (64 KiB; input events are tiny JSON).
pub const MAX_WS_PAYLOAD: usize = 64 * 1024;

pub const OPCODE_CONT: u8 = 0x0;
pub const OPCODE_TEXT: u8 = 0x1;
pub const OPCODE_BINARY: u8 = 0x2;
pub const OPCODE_CLOSE: u8 = 0x8;
pub const OPCODE_PING: u8 = 0x9;
pub const OPCODE_PONG: u8 = 0xA;

/// An incoming complete WebSocket message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
}

/// Compute `Sec-WebSocket-Accept` from `Sec-WebSocket-Key` per RFC 6455 §4.2.2.
#[must_use]
pub fn accept_key(client_key: &str) -> String {
    let mut input = Vec::with_capacity(client_key.trim().len() + WS_GUID.len());
    input.extend_from_slice(client_key.trim().as_bytes());
    input.extend_from_slice(WS_GUID.as_bytes());
    let digest = acos_dex::hash::sha1(&input);
    base64_encode(&digest)
}

/// Write the `101 Switching Protocols` response for a validated upgrade request.
pub fn write_handshake_response(stream: &mut TcpStream, client_key: &str) -> io::Result<()> {
    let accept = accept_key(client_key);
    write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    )?;
    stream.flush()
}

use std::sync::Mutex;

/// Read one complete text or binary message, answering Ping with Pong and
/// handling Close cleanly. Returns `Ok(None)` on Close or EOF.
#[cfg_attr(not(test), allow(dead_code))]
pub fn read_message(stream: &mut TcpStream) -> io::Result<Option<WsMessage>> {
    let mut writer = stream.try_clone()?;
    read_message_inner(stream, &mut |opcode, payload| {
        send_control(&mut writer, opcode, payload)
    })
}

/// Same as [`read_message`], but writes control replies (Pong, Close) through
/// a shared [`Mutex<TcpStream>`] so another thread can safely call
/// [`send_text_locked`] concurrently.
pub fn read_message_with_writer(
    read_stream: &mut TcpStream,
    writer: &Mutex<TcpStream>,
) -> io::Result<Option<WsMessage>> {
    read_message_inner(read_stream, &mut |opcode, payload| {
        let mut guard = writer.lock().unwrap_or_else(|e| e.into_inner());
        send_control(&mut guard, opcode, payload)
    })
}

/// Send an unmasked server text frame under a shared write mutex.
pub fn send_text_locked(writer: &Mutex<TcpStream>, text: &str) -> io::Result<()> {
    let mut guard = writer.lock().unwrap_or_else(|e| e.into_inner());
    send_text(&mut guard, text)
}

fn read_message_inner(
    stream: &mut TcpStream,
    write_ctrl: &mut dyn FnMut(u8, &[u8]) -> io::Result<()>,
) -> io::Result<Option<WsMessage>> {
    let mut fragmented_opcode: Option<u8> = None;
    let mut fragmented_buf: Vec<u8> = Vec::new();

    loop {
        let Some((fin, opcode, payload)) = read_raw_frame(stream, write_ctrl)? else {
            return Ok(None);
        };
        match opcode {
            OPCODE_PING => {
                write_ctrl(OPCODE_PONG, &payload)?;
            }
            OPCODE_PONG => {}
            OPCODE_CLOSE => {
                let code = if payload.len() >= 2 {
                    &payload[..2]
                } else {
                    &1000u16.to_be_bytes()[..]
                };
                let _ = write_ctrl(OPCODE_CLOSE, code);
                return Ok(None);
            }
            OPCODE_TEXT | OPCODE_BINARY => {
                if fragmented_opcode.is_some() {
                    let _ = send_close_via(write_ctrl, 1002, b"unexpected new data frame");
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        "unexpected new WebSocket data frame during fragmentation",
                    ));
                }
                if fin {
                    return decode_data_message(opcode, payload);
                }
                fragmented_opcode = Some(opcode);
                fragmented_buf = payload;
            }
            OPCODE_CONT => {
                let Some(start_op) = fragmented_opcode else {
                    let _ = send_close_via(write_ctrl, 1002, b"unexpected continuation");
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        "unexpected WebSocket continuation frame",
                    ));
                };
                if fragmented_buf.len() + payload.len() > MAX_WS_PAYLOAD {
                    let _ = send_close_via(write_ctrl, 1009, b"message too large");
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        "fragmented WebSocket message exceeds limit",
                    ));
                }
                fragmented_buf.extend_from_slice(&payload);
                if fin {
                    let full = std::mem::take(&mut fragmented_buf);
                    return decode_data_message(start_op, full);
                }
            }
            other => {
                let _ = send_close_via(write_ctrl, 1002, b"unsupported opcode");
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("unsupported WebSocket opcode {other:#x}"),
                ));
            }
        }
    }
}

fn send_close_via(
    write_ctrl: &mut dyn FnMut(u8, &[u8]) -> io::Result<()>,
    code: u16,
    reason: &[u8],
) -> io::Result<()> {
    let max_reason = 123usize.min(reason.len());
    let mut payload = Vec::with_capacity(2 + max_reason);
    payload.extend_from_slice(&code.to_be_bytes());
    payload.extend_from_slice(&reason[..max_reason]);
    write_ctrl(OPCODE_CLOSE, &payload)
}

fn decode_data_message(opcode: u8, payload: Vec<u8>) -> io::Result<Option<WsMessage>> {
    match opcode {
        OPCODE_TEXT => {
            let text = String::from_utf8(payload).map_err(|_| {
                io::Error::new(ErrorKind::InvalidData, "WebSocket text frame is not UTF-8")
            })?;
            Ok(Some(WsMessage::Text(text)))
        }
        OPCODE_BINARY => Ok(Some(WsMessage::Binary(payload))),
        _ => Err(io::Error::new(ErrorKind::InvalidData, "bad data opcode")),
    }
}

fn read_raw_frame(
    stream: &mut TcpStream,
    write_ctrl: &mut dyn FnMut(u8, &[u8]) -> io::Result<()>,
) -> io::Result<Option<(bool, u8, Vec<u8>)>> {
    // Between messages, block cleanly with no periodic polling wakeups (keeps idle CPU at zero).
    let _ = stream.set_read_timeout(None);
    let mut first = [0u8; 1];
    loop {
        match stream.read(&mut first) {
            Ok(0) => return Ok(None),
            Ok(1) => break,
            Ok(_) => unreachable!(),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    // Once a frame starts arriving, bound the rest of the frame read so a stalled peer cannot hang.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut second = [0u8; 1];
    stream.read_exact(&mut second)?;
    let header = [first[0], second[0]];
    let fin = (header[0] & 0x80) != 0;
    let rsv = header[0] & 0x70;
    if rsv != 0 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "non-zero WebSocket RSV bits",
        ));
    }
    let opcode = header[0] & 0x0f;
    let masked = (header[1] & 0x80) != 0;
    if !masked {
        let _ = send_close_via(write_ctrl, 1002, b"client frames must be masked");
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "unmasked client WebSocket frame",
        ));
    }
    let len7 = header[1] & 0x7f;
    let payload_len: usize = match len7 {
        0..=125 => usize::from(len7),
        126 => {
            let mut ext = [0u8; 2];
            stream.read_exact(&mut ext)?;
            usize::from(u16::from_be_bytes(ext))
        }
        127 => {
            let mut ext = [0u8; 8];
            stream.read_exact(&mut ext)?;
            let raw = u64::from_be_bytes(ext);
            usize::try_from(raw)
                .map_err(|_| io::Error::new(ErrorKind::InvalidData, "frame length overflow"))?
        }
        _ => unreachable!(),
    };
    if opcode >= 0x8 && (!fin || payload_len > 125) {
        let _ = send_close_via(write_ctrl, 1002, b"invalid control frame");
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "fragmented or oversized WebSocket control frame",
        ));
    }
    if payload_len > MAX_WS_PAYLOAD {
        let _ = send_close_via(write_ctrl, 1009, b"payload too large");
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("WebSocket payload {payload_len} exceeds {MAX_WS_PAYLOAD}"),
        ));
    }
    let mut mask = [0u8; 4];
    stream.read_exact(&mut mask)?;
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        stream.read_exact(&mut payload)?;
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i & 3];
        }
    }
    Ok(Some((fin, opcode, payload)))
}

/// Send an unmasked server text frame.
pub fn send_text(stream: &mut TcpStream, text: &str) -> io::Result<()> {
    send_frame(stream, OPCODE_TEXT, text.as_bytes())
}

/// Send a close frame with status code and short reason.
#[allow(dead_code)]
pub fn send_close(stream: &mut TcpStream, code: u16, reason: &[u8]) -> io::Result<()> {
    let max_reason = 123usize.min(reason.len());
    let mut payload = Vec::with_capacity(2 + max_reason);
    payload.extend_from_slice(&code.to_be_bytes());
    payload.extend_from_slice(&reason[..max_reason]);
    send_frame(stream, OPCODE_CLOSE, &payload)
}

fn send_control(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let capped = &payload[..payload.len().min(125)];
    send_frame(stream, opcode, capped)
}

fn send_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut head = Vec::with_capacity(10);
    head.push(0x80 | (opcode & 0x0f));
    let len = payload.len();
    if len <= 125 {
        head.push(len as u8);
    } else if len <= u16::MAX as usize {
        head.push(126);
        head.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        head.push(127);
        head.extend_from_slice(&(len as u64).to_be_bytes());
    }
    stream.write_all(&head)?;
    if !payload.is_empty() {
        stream.write_all(payload)?;
    }
    stream.flush()
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};

    fn make_client_masked_frame(fin: bool, opcode: u8, mask: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push((if fin { 0x80 } else { 0 }) | (opcode & 0x0f));
        if payload.len() <= 125 {
            out.push(0x80 | (payload.len() as u8));
        } else if payload.len() <= u16::MAX as usize {
            out.push(0x80 | 126);
            out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            out.push(0x80 | 127);
            out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        out.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            out.push(*b ^ mask[i & 3]);
        }
        out
    }

    #[test]
    fn rfc6455_accept_key_matches_spec_vector() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn masked_text_ping_and_close_roundtrip() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_thread = std::thread::spawn(move || {
            let (mut srv, _) = listener.accept().unwrap();
            let msg = read_message(&mut srv).unwrap();
            assert_eq!(msg, Some(WsMessage::Text("hello".to_string())));
            send_text(&mut srv, "world").unwrap();
            // Ping is answered transparently before next EOF/Close
            let end = read_message(&mut srv).unwrap();
            assert_eq!(end, None);
        });

        let mut cli = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        cli.write_all(&make_client_masked_frame(
            true,
            OPCODE_PING,
            [1, 2, 3, 4],
            b"hi",
        ))
        .unwrap();
        cli.write_all(&make_client_masked_frame(
            true,
            OPCODE_TEXT,
            [0xaa, 0xbb, 0xcc, 0xdd],
            b"hello",
        ))
        .unwrap();
        cli.flush().unwrap();

        // Read pong ("hi") and "world" from server.
        let mut pong = [0u8; 4];
        cli.read_exact(&mut pong).unwrap();
        assert_eq!(pong[0], 0x8a);
        assert_eq!(pong[1], 2);
        assert_eq!(&pong[2..4], b"hi");

        let mut world = [0u8; 7];
        cli.read_exact(&mut world).unwrap();
        assert_eq!(world[0], 0x81);
        assert_eq!(world[1], 5);
        assert_eq!(&world[2..7], b"world");

        cli.write_all(&make_client_masked_frame(
            true,
            OPCODE_CLOSE,
            [9, 8, 7, 6],
            &1000u16.to_be_bytes(),
        ))
        .unwrap();
        server_thread.join().unwrap();
    }

    #[test]
    fn unmasked_client_frame_is_rejected() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_thread = std::thread::spawn(move || {
            let (mut srv, _) = listener.accept().unwrap();
            let err = read_message(&mut srv).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::InvalidData);
        });

        let mut cli = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        cli.write_all(&[0x81, 0x02, b'h', b'i']).unwrap();
        server_thread.join().unwrap();
    }
}
