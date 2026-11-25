//! QEMU emulator console server on `127.0.0.1:<5554 + 2*k>`.
//!
//! `adb emu <cmd>` and `expo-device-hub` query the emulator console port
//! (for `avd name`, `auth`, `quit`, etc.). Only commands backed by real ACOS
//! session state are answered; unsupported console commands return `KO: ...`.

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const GREETING: &[u8] = b"Android Console: type 'help' for a list of commands\r\nOK\r\n";

/// Serve one emulator console connection until `quit`, EOF, error, or `closing`.
pub fn handle_connection(mut stream: TcpStream, avd_name: &str, closing: Arc<AtomicBool>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    if stream.write_all(GREETING).is_err() || stream.flush().is_err() {
        return;
    }

    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();

    loop {
        if closing.load(Ordering::Acquire) {
            break;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (reply, should_quit) = handle_console_line(trimmed, avd_name);
        if stream.write_all(reply.as_bytes()).is_err() || stream.flush().is_err() {
            break;
        }
        if should_quit {
            break;
        }
    }

    let _ = stream.shutdown(Shutdown::Both);
}

/// Process a single console command line, returning `(response, should_quit)`.
pub fn handle_console_line(line: &str, avd_name: &str) -> (String, bool) {
    let parts: Vec<&str> = line.split_whitespace().collect();
    match parts.as_slice() {
        ["auth", ..] => ("OK\r\n".to_string(), false),
        ["avd", "name"] => (format!("{avd_name}\r\nOK\r\n"), false),
        ["avd", "path"] => (
            "KO: no AVD directory (live ACOS session)\r\n".to_string(),
            false,
        ),
        ["help"] => (
            "Android console commands supported by ACOS:\r\n\
             \x20   auth <token>\r\n\
             \x20   avd name\r\n\
             \x20   avd path\r\n\
             \x20   help\r\n\
             \x20   quit\r\n\
             OK\r\n"
                .to_string(),
            false,
        ),
        ["quit"] | ["exit"] => ("OK\r\n".to_string(), true),
        _ => (format!("KO: unsupported console command: {line}\r\n"), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::{Ipv4Addr, TcpListener};

    #[test]
    fn console_protocol_handles_avd_name_and_unsupported() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let closing = Arc::new(AtomicBool::new(false));
        let closing_bg = Arc::clone(&closing);

        let srv = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle_connection(stream, "Counter · s_1", closing_bg);
        });

        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut rdr = BufReader::new(client.try_clone().unwrap());
        let mut l1 = String::new();
        let mut l2 = String::new();
        rdr.read_line(&mut l1).unwrap();
        rdr.read_line(&mut l2).unwrap();
        assert!(l1.starts_with("Android Console:"));
        assert_eq!(l2, "OK\r\n");

        client.write_all(b"auth dummy-token\n").unwrap();
        l1.clear();
        rdr.read_line(&mut l1).unwrap();
        assert_eq!(l1, "OK\r\n");

        client.write_all(b"avd name\n").unwrap();
        l1.clear();
        l2.clear();
        rdr.read_line(&mut l1).unwrap();
        rdr.read_line(&mut l2).unwrap();
        assert_eq!(l1, "Counter · s_1\r\n");
        assert_eq!(l2, "OK\r\n");

        client.write_all(b"avd path\n").unwrap();
        l1.clear();
        rdr.read_line(&mut l1).unwrap();
        assert!(l1.starts_with("KO: no AVD directory"));

        client.write_all(b"kill\n").unwrap();
        l1.clear();
        rdr.read_line(&mut l1).unwrap();
        assert_eq!(l1, "KO: unsupported console command: kill\r\n");

        client.write_all(b"quit\n").unwrap();
        l1.clear();
        rdr.read_line(&mut l1).unwrap();
        assert_eq!(l1, "OK\r\n");

        let mut eof = Vec::new();
        let _ = rdr.read_to_end(&mut eof);
        srv.join().unwrap();
    }
}
