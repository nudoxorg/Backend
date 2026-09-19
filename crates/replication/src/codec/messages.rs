//! Versioned payload encoding for each transport message family.

use crate::{ReplicationError, TransportLimits, TransportMessage};

use super::primitives::Reader;

const MAGIC: [u8; 4] = *b"RPL2";
// Version 3 widens execution scope from an owner-local u64 to a canonical 32-byte identity.
// Decoders reject v2 frames rather than guessing which scope grammar an execution payload used.
const VERSION: u8 = 3;
const HEADER_BYTES: usize = 10;

const TAG_CAPABILITIES: u8 = 1;
const TAG_ROOT_SUMMARY: u8 = 2;
const TAG_NODE_REQUEST: u8 = 3;
const TAG_RANGE_REQUEST: u8 = 4;
const TAG_CHUNK: u8 = 5;
const TAG_RESUME_REQUEST: u8 = 6;
const TAG_RECIPE_REQUEST: u8 = 7;
const TAG_RECIPE_RESULT: u8 = 8;
const TAG_PACK: u8 = 9;
const TAG_CANCEL_ATTEMPT: u8 = 10;
const TAG_CLOSURE_PAGE_REQUEST: u8 = 11;
const TAG_CLOSURE_PAGE_RESPONSE: u8 = 12;
const TAG_CLOSURE_ROOT_OFFER: u8 = 13;
const TAG_CLOSURE_ROOT_ACK: u8 = 14;
const TAG_CLOSURE_NEED_REQUEST: u8 = 15;

#[path = "../codec/message_payload.rs"]
mod message_payload;
use message_payload::{decode_payload, encode_payload};

/// Encodes one bounded protocol message with a versioned, length-delimited
/// envelope.
///
/// Typed convenience messages are converted to their untrusted wire form
/// before encoding.  This makes local transport and a remote stream share the
/// exact same admission boundary.
///
/// # Errors
///
/// Returns a size, limit, identity, or structural error when the message
/// cannot be represented within `limits`.
pub fn encode_message(
    message: &TransportMessage,
    limits: TransportLimits,
) -> Result<Vec<u8>, ReplicationError> {
    limits.validate()?;
    message.validate(limits)?;
    let (tag, payload) = encode_payload(message, limits)?;
    let payload_len =
        u32::try_from(payload.len()).map_err(|_| ReplicationError::MessageTooLarge)?;
    let total = HEADER_BYTES
        .checked_add(payload.len())
        .ok_or(ReplicationError::Overflow)?;
    if total > limits.max_frame {
        return Err(ReplicationError::MessageTooLarge);
    }
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(&MAGIC);
    bytes.push(VERSION);
    bytes.push(tag);
    bytes.extend_from_slice(&payload_len.to_be_bytes());
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

/// Decodes exactly one bounded protocol message.
///
/// The result remains at the wire boundary.  For example, a root summary is
/// returned as [`TransportMessage::WireRootSummary`] because its digest claims
/// cannot be made typed without the caller's expected closure.
///
/// # Errors
///
/// Returns a truncation, version, tag, size, or structural error when `bytes`
/// is not one complete message accepted by `limits`.
pub fn decode_message(
    bytes: &[u8],
    limits: TransportLimits,
) -> Result<TransportMessage, ReplicationError> {
    limits.validate()?;
    if bytes.len() > limits.max_frame {
        return Err(ReplicationError::MessageTooLarge);
    }
    if bytes.len() < HEADER_BYTES {
        return Err(ReplicationError::TruncatedFrame);
    }
    if bytes[..4] != MAGIC {
        return Err(ReplicationError::InvalidWire);
    }
    if bytes[4] != VERSION {
        return Err(ReplicationError::UnsupportedWireVersion);
    }
    let tag = bytes[5];
    let payload_len = u32::from_be_bytes(
        bytes[6..10]
            .try_into()
            .map_err(|_| ReplicationError::TruncatedFrame)?,
    ) as usize;
    let expected = HEADER_BYTES
        .checked_add(payload_len)
        .ok_or(ReplicationError::Overflow)?;
    if expected != bytes.len() {
        return if expected > bytes.len() {
            Err(ReplicationError::TruncatedFrame)
        } else {
            Err(ReplicationError::TrailingFrame)
        };
    }
    let mut reader = Reader::new(&bytes[HEADER_BYTES..]);
    let message = decode_payload(tag, &mut reader, limits)?;
    reader.finish()?;
    message.validate(limits)?;
    Ok(message)
}
