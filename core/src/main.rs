//! Remote Volume audio core: line-delimited JSON over stdio. See CONTRACT.md.
//!
//! stdout carries protocol messages only (one JSON object per line); logs go to stderr.
//! Requests are handled one at a time, in order. The process exits when stdin closes, so a
//! parent that dies never leaves the core behind.

mod backend;
mod protocol;
mod volume;

use std::io::{self, BufRead, Read, Write};

use serde_json::Value;

enum Line {
    Eof,
    /// A complete line is in the buffer (without its newline).
    Request,
    /// The line exceeded `MAX_LINE_BYTES` and was discarded up to the next newline.
    TooLong,
}

fn read_line<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<Line> {
    buf.clear();
    let limit = protocol::MAX_LINE_BYTES as u64 + 1;
    let read = reader.by_ref().take(limit).read_until(b'\n', buf)?;
    if read == 0 {
        return Ok(Line::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(Line::Request);
    }
    if buf.len() > protocol::MAX_LINE_BYTES {
        // Throw away the rest of the oversized line without buffering it.
        loop {
            let chunk = reader.fill_buf()?;
            if chunk.is_empty() {
                break;
            }
            match chunk.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    reader.consume(i + 1);
                    break;
                }
                None => {
                    let len = chunk.len();
                    reader.consume(len);
                }
            }
        }
        return Ok(Line::TooLong);
    }
    // Last line of the stream, no trailing newline.
    Ok(Line::Request)
}

fn send<W: Write>(out: &mut W, message: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *out, message)?;
    out.write_all(b"\n")?;
    out.flush()
}

fn run() -> io::Result<()> {
    let mut backend = backend::create();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    send(&mut out, &protocol::ready_event(backend.as_mut()))?;

    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut buf = Vec::with_capacity(256);
    loop {
        let reply = match read_line(&mut input, &mut buf)? {
            Line::Eof => return Ok(()),
            Line::TooLong => protocol::line_too_long(),
            Line::Request => {
                if buf.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                protocol::handle_line(backend.as_mut(), &buf)
            }
        };
        send(&mut out, &reply)?;
    }
}

fn main() {
    match run() {
        Ok(()) => {}
        // The parent went away mid-write: nothing left to talk to.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {}
        Err(e) => {
            eprintln!("remote-volume-core: {e}");
            std::process::exit(1);
        }
    }
}
