//! Newline-delimited frame reading with a hard size bound.
//!
//! MCP stdio frames are single-line JSON. A client must not be able to make the
//! server allocate unbounded memory, so reads are capped. An oversized frame is
//! drained (to keep the stream in sync) and reported as a parse error.

use std::io::{self, BufRead};

/// One read outcome from [`read_line_bounded`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    /// End of stream.
    Eof,
    /// A complete line (without the trailing `\n`).
    Line(Vec<u8>),
    /// A line longer than the configured maximum; its bytes were discarded.
    Oversized,
}

/// Read one `\n`-terminated line, storing at most `max` bytes.
///
/// When the line exceeds `max`, the remaining bytes up to the next newline are
/// discarded so the next call starts on a frame boundary.
pub fn read_line_bounded<R: BufRead>(reader: &mut R, max: usize) -> io::Result<ReadOutcome> {
    let mut out: Vec<u8> = Vec::new();
    let mut oversized = false;
    let mut saw_any = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if !saw_any && !oversized {
                return Ok(ReadOutcome::Eof);
            }
            break;
        }
        saw_any = true;

        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            if !oversized {
                if out.len() + pos > max {
                    oversized = true;
                } else {
                    out.extend_from_slice(&available[..pos]);
                }
            }
            reader.consume(pos + 1);
            break;
        }

        let len = available.len();
        if !oversized {
            if out.len() + len > max {
                oversized = true;
            } else {
                out.extend_from_slice(available);
            }
        }
        reader.consume(len);
    }

    // Strip a trailing CR (tolerate CRLF clients).
    if !oversized && out.last() == Some(&b'\r') {
        out.pop();
    }

    if oversized {
        Ok(ReadOutcome::Oversized)
    } else {
        Ok(ReadOutcome::Line(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_lines_and_reports_eof() {
        let mut r = Cursor::new(b"one\ntwo\n".to_vec());
        assert_eq!(read_line_bounded(&mut r, 64).unwrap(), ReadOutcome::Line(b"one".to_vec()));
        assert_eq!(read_line_bounded(&mut r, 64).unwrap(), ReadOutcome::Line(b"two".to_vec()));
        assert_eq!(read_line_bounded(&mut r, 64).unwrap(), ReadOutcome::Eof);
    }

    #[test]
    fn oversized_line_is_drained() {
        let mut r = Cursor::new(b"aaaaaaaaaa\nok\n".to_vec());
        assert_eq!(read_line_bounded(&mut r, 4).unwrap(), ReadOutcome::Oversized);
        assert_eq!(read_line_bounded(&mut r, 4).unwrap(), ReadOutcome::Line(b"ok".to_vec()));
    }

    #[test]
    fn crlf_is_tolerated() {
        let mut r = Cursor::new(b"hi\r\n".to_vec());
        assert_eq!(read_line_bounded(&mut r, 64).unwrap(), ReadOutcome::Line(b"hi".to_vec()));
    }
}
