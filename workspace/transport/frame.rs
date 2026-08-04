//! Length-prefixed postcard framing over an iroh QUIC bi-stream.
//!
//! Both the IR-sync plane and the object-pack plane framed their typed
//! request/response/announcement messages the same way — a little-endian length
//! prefix followed by the postcard bytes, with a hard cap on the declared
//! length so a hostile peer cannot force an unbounded allocation. That framing
//! lives here once.
//!
//! The length prefix is a `u64` (the widest of the two prior planes; the
//! IR-sync plane used a `u32`, the pack plane a `u64` — a `u64` is a strict
//! superset). The cap is a caller-supplied argument, since the two planes bound
//! their frames differently (IR announcements are tiny; whole-pack payloads are
//! large).

use heart::sync::SyncError;
use serde::{Serialize, de::DeserializeOwned};

/// A generous default frame cap (512 MiB) — the object-pack whole-pack bound.
/// IR-sync passes a much smaller cap for its announcements/acks.
pub const DEFAULT_FRAME_CAP_BYTES: usize = 512 * 1024 * 1024;

/// Encode `value` with postcard and write it as a length-prefixed frame.
pub async fn send_framed<T: Serialize>(
    send: &mut iroh::endpoint::SendStream,
    value: &T,
) -> Result<(), SyncError> {
    let encoded =
        postcard::to_allocvec(value).map_err(|error| SyncError::Codec(error.to_string()))?;
    let length_prefix = (encoded.len() as u64).to_le_bytes();
    send.write_all(&length_prefix)
        .await
        .map_err(|error| SyncError::Transport(error.to_string()))?;
    send.write_all(&encoded)
        .await
        .map_err(|error| SyncError::Transport(error.to_string()))?;
    Ok(())
}

/// Read one length-prefixed postcard frame, refusing a declared length above
/// `frame_cap_bytes` before allocating.
pub async fn recv_framed<T: DeserializeOwned>(
    recv: &mut iroh::endpoint::RecvStream,
    frame_cap_bytes: usize,
) -> Result<T, SyncError> {
    let mut length_buffer = [0u8; 8];
    recv.read_exact(&mut length_buffer)
        .await
        .map_err(|error| SyncError::Transport(error.to_string()))?;
    let length = u64::from_le_bytes(length_buffer) as usize;
    if length > frame_cap_bytes {
        return Err(SyncError::Transport(format!(
            "frame too large: {length} bytes (cap {frame_cap_bytes})"
        )));
    }
    let mut buffer = vec![0u8; length];
    recv.read_exact(&mut buffer)
        .await
        .map_err(|error| SyncError::Transport(error.to_string()))?;
    postcard::from_bytes(&buffer).map_err(|error| SyncError::Codec(error.to_string()))
}
