//! Length-prefixed local transport framing and lightweight header inspection.

use super::{LOCAL_CONTROL_HEADER_BYTES, LocalControlError, LocalControlLimits};
use std::io::{self, Read, Write};

/// Magic prefix for the local control envelope.
pub const LOCAL_CONTROL_MAGIC: [u8; 4] = *b"LDC2";

fn map_read_error(error: &io::Error) -> LocalControlError {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        LocalControlError::Truncated
    } else {
        LocalControlError::Io(error.kind())
    }
}

/// Returns whether a payload starts with the local control magic.
#[must_use]
pub fn is_control(payload: &[u8]) -> bool {
    payload.len() >= LOCAL_CONTROL_MAGIC.len()
        && payload[..LOCAL_CONTROL_MAGIC.len()] == LOCAL_CONTROL_MAGIC
}

/// Extracts a correlation ID from a control header without admitting the
/// operation itself.
#[must_use]
pub fn control_request_id(payload: &[u8]) -> Option<u64> {
    if payload.len() < LOCAL_CONTROL_HEADER_BYTES || !is_control(payload) {
        return None;
    }
    Some(u64::from_be_bytes(payload[6..14].try_into().ok()?))
}

/// Adds one bounded four-byte length prefix to a payload.
///
/// # Errors
///
/// Returns a frame-size or limits error when the payload cannot fit the
/// configured local envelope.
pub fn frame(body: &[u8], limits: LocalControlLimits) -> Result<Vec<u8>, LocalControlError> {
    limits.validate()?;
    if body.len() > limits.max_frame {
        return Err(LocalControlError::FrameTooLarge);
    }
    let length = u32::try_from(body.len()).map_err(|_| LocalControlError::FrameTooLarge)?;
    let capacity = body
        .len()
        .checked_add(4)
        .ok_or(LocalControlError::FrameTooLarge)?;
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(body);
    Ok(output)
}

/// Removes one complete four-byte length prefix without allocating.
///
/// # Errors
///
/// Returns a truncation, trailing-bytes, frame-size, or limits error when the
/// input is not exactly one bounded frame.
pub fn unframe(input: &[u8], limits: LocalControlLimits) -> Result<&[u8], LocalControlError> {
    limits.validate()?;
    if input.len() < 4 {
        return Err(LocalControlError::Truncated);
    }
    let length = usize::try_from(u32::from_be_bytes(
        input[..4]
            .try_into()
            .map_err(|_| LocalControlError::Truncated)?,
    ))
    .map_err(|_| LocalControlError::FrameTooLarge)?;
    if length > limits.max_frame {
        return Err(LocalControlError::FrameTooLarge);
    }
    let expected = length
        .checked_add(4)
        .ok_or(LocalControlError::FrameTooLarge)?;
    if input.len() != expected {
        return Err(if input.len() < expected {
            LocalControlError::Truncated
        } else {
            LocalControlError::Trailing
        });
    }
    Ok(&input[4..])
}

/// Reads one complete bounded length-prefixed frame.
///
/// # Errors
///
/// Returns an I/O, closure, truncation, frame-size, or limits error when the
/// reader does not yield one bounded frame.
pub fn read_frame(
    reader: &mut impl Read,
    limits: LocalControlLimits,
) -> Result<Vec<u8>, LocalControlError> {
    let mut body = Vec::new();
    read_frame_into(reader, &mut body, limits)?;
    Ok(body)
}

/// Reads one complete bounded frame into caller-owned storage.
///
/// # Errors
///
/// Returns an I/O, closure, truncation, frame-size, or limits error when the
/// reader does not yield one bounded frame.
pub fn read_frame_into(
    reader: &mut impl Read,
    body: &mut Vec<u8>,
    limits: LocalControlLimits,
) -> Result<(), LocalControlError> {
    limits.validate()?;
    body.clear();
    let mut header = [0_u8; 4];
    let first = reader
        .read(&mut header[..1])
        .map_err(|error| map_read_error(&error))?;
    if first == 0 {
        return Err(LocalControlError::Closed);
    }
    reader
        .read_exact(&mut header[1..])
        .map_err(|error| map_read_error(&error))?;
    let length = usize::try_from(u32::from_be_bytes(header))
        .map_err(|_| LocalControlError::FrameTooLarge)?;
    if length > limits.max_frame {
        return Err(LocalControlError::FrameTooLarge);
    }
    body.resize(length, 0);
    reader
        .read_exact(body.as_mut_slice())
        .map_err(|error| map_read_error(&error))?;
    Ok(())
}

/// Writes one complete bounded length-prefixed frame and flushes it.
///
/// # Errors
///
/// Returns an I/O, frame-size, or limits error when the frame cannot be
/// written within the configured local envelope.
pub fn write_frame(
    writer: &mut impl Write,
    body: &[u8],
    limits: LocalControlLimits,
) -> Result<(), LocalControlError> {
    limits.validate()?;
    if body.len() > limits.max_frame {
        return Err(LocalControlError::FrameTooLarge);
    }
    let length = u32::try_from(body.len()).map_err(|_| LocalControlError::FrameTooLarge)?;
    writer
        .write_all(&length.to_be_bytes())
        .and_then(|()| writer.write_all(body))
        .and_then(|()| writer.flush())
        .map_err(|error| LocalControlError::Io(error.kind()))
}
