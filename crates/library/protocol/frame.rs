//! Defines frame behavior for `backend-library`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the frame invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Bounded `Content-Length` frame I/O used by the MCP process adapter.

use std::io::{self, BufRead, Write};

/// Largest accepted JSON-RPC frame body; requests over this bound are rejected before parsing.
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
/// Largest retained header line, kept on the stack before allocating the earned body frame.
pub const MAX_HEADER_LINE_BYTES: usize = 128;
/// Largest accepted header count before an MCP body is considered for allocation.
pub const MAX_HEADER_LINES: usize = 8;

struct HeaderLine {
    bytes: [u8; MAX_HEADER_LINE_BYTES],
    length: usize,
}

impl HeaderLine {
    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

/// Reads exactly one `Content-Length` framed body, or `None` on clean EOF.
///
/// # Errors
///
/// Returns an I/O error for malformed headers, an oversized frame, truncated body, or reader I/O.
pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    read_frame_bounded(reader, MAX_FRAME_BYTES)
}

/// Reads exactly one framed body under a caller-chosen bound, or `None` on clean EOF.
///
/// Requests and responses are bounded separately: a request that needs more than a small budget is
/// almost always a mistake, while a legitimate answer can be large.
///
/// # Errors
///
/// Returns an I/O error for malformed headers, a frame over `maximum`, a truncated body, or
/// reader I/O.
pub fn read_frame_bounded(
    reader: &mut impl BufRead,
    maximum: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut saw_header = false;
    let mut header_lines = 0;
    loop {
        let Some(line) = read_header_line(reader)? else {
            return if saw_header {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "MCP frame ended before its blank header line",
                ))
            } else {
                Ok(None)
            };
        };
        saw_header = true;
        let line = line.as_bytes();
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        if header_lines == MAX_HEADER_LINES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP header count exceeds the fixed adapter bound",
            ));
        }
        header_lines += 1;
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP header has no colon",
            ));
        };
        let (name, value) = line.split_at(separator);
        if name.eq_ignore_ascii_case(b"content-length") {
            let parsed = decimal(trim_ascii(&value[1..]))?;
            if parsed > maximum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "MCP content length exceeds the bounded adapter frame",
                ));
            }
            length = Some(parsed);
        }
    }
    let length = length.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "MCP frame has no content length header",
        )
    })?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

fn read_header_line(reader: &mut impl BufRead) -> io::Result<Option<HeaderLine>> {
    let mut line = HeaderLine {
        bytes: [0; MAX_HEADER_LINE_BYTES],
        length: 0,
    };
    loop {
        if line.length == MAX_HEADER_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP header line exceeds the fixed adapter bound",
            ));
        }
        let read = reader.read(&mut line.bytes[line.length..=line.length])?;
        if read == 0 {
            return if line.length == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "MCP header ended before its newline",
                ))
            };
        }
        line.length += 1;
        if line.bytes[line.length - 1] == b'\n' {
            return Ok(Some(line));
        }
    }
}

fn trim_ascii(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .map_or(value.len(), |position| position);
    let end = value
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |position| position + 1);
    &value[start..end]
}

fn decimal(value: &[u8]) -> io::Result<usize> {
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MCP content length is not a number",
        ));
    }
    value.iter().try_fold(0_usize, |total, byte| {
        if !byte.is_ascii_digit() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP content length is not a number",
            ));
        }
        total
            .checked_mul(10)
            .and_then(|scaled| scaled.checked_add(usize::from(*byte - b'0')))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "MCP content length overflow")
            })
    })
}

/// Writes one complete `Content-Length` frame and flushes it for a process client.
///
/// # Errors
///
/// Returns an I/O error when the response exceeds the bounded frame or the writer rejects output.
pub fn write_frame(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    write_frame_bounded(writer, body, MAX_FRAME_BYTES)
}

/// Writes one complete framed body under a caller-chosen bound and flushes it.
///
/// # Errors
///
/// Returns an I/O error when the body exceeds `maximum` or the writer rejects output.
pub fn write_frame_bounded(writer: &mut impl Write, body: &[u8], maximum: usize) -> io::Result<()> {
    if body.len() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MCP response exceeds the bounded adapter frame",
        ));
    }
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}
