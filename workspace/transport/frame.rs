//! Length-prefixed postcard framing over an iroh QUIC bi-stream.

use std::time::Duration;

use heart::sync::SyncError;
use serde::{Serialize, de::DeserializeOwned};

/// A generous default frame cap (512 MiB) — the object-pack whole-pack bound.
/// IR-sync passes a much smaller cap for its announcements/acks.
pub const DEFAULT_FRAME_CAP_BYTES: usize = 512 * 1024 * 1024;

/// How long a responder waits for its peer to take delivery of the terminal
/// frame and close, before it stops waiting and tears the connection down.
///
/// This bounds [`finish_and_drain`]. It is deliberately short: it covers a
/// round trip on a live link, not a transfer, because the payload is already
/// written by the time the drain starts.
pub const TERMINAL_DRAIN: Duration = Duration::from_secs(5);

/// Encode `value` with postcard and write it as a length-prefixed frame.
pub async fn send_framed<T: Serialize>(
    send: &mut iroh::endpoint::SendStream,
    value: &T,
) -> Result<(), SyncError> {
    let encoded = postcard::to_allocvec(value).map_err(SyncError::Codec)?;
    let length_prefix = (encoded.len() as u64).to_le_bytes();
    send.write_all(&length_prefix)
        .await
        .map_err(|error| SyncError::Transport(std::io::Error::other(error)))?;
    send.write_all(&encoded)
        .await
        .map_err(|error| SyncError::Transport(std::io::Error::other(error)))?;
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
        .map_err(|error| SyncError::Transport(std::io::Error::other(error)))?;
    let length = u64::from_le_bytes(length_buffer) as usize;
    if length > frame_cap_bytes {
        return Err(SyncError::ItemTooLarge {
            id: String::from("frame"),
            size: length,
            max: frame_cap_bytes,
        });
    }
    let mut buffer = vec![0u8; length];
    recv.read_exact(&mut buffer)
        .await
        .map_err(|error| SyncError::Transport(std::io::Error::other(error)))?;
    postcard::from_bytes(&buffer).map_err(SyncError::Codec)
}

/// End a framed exchange *after* the peer has taken delivery of the terminal
/// frame — the only correct way for a responder to return.
///
/// # Why this exists
///
/// `SendStream::finish` marks the stream complete locally; it does not wait for
/// the bytes to leave. QUIC only keeps retransmitting them while the connection
/// and its endpoint driver are alive. A responder that writes its last frame and
/// then returns — dropping the `Connection`, or calling `Connection::close`, or
/// dropping the whole `Endpoint` because the accept future owned it — discards
/// the queued stream data and the CONNECTION_CLOSE frame with it. Nothing
/// reaches the peer, which stays parked in [`recv_framed`]'s `read_exact` until
/// QUIC's idle timer fires ~30 s later. It then reports an opaque transport
/// timeout instead of the typed reason the responder actually computed and
/// wrote. That is not a slow success: the refusal is *lost*, and every typed
/// "the peer declined, do not retry" variant becomes unreachable in practice.
///
/// So: finish the stream, then hold the connection open until the peer closes
/// it, which is the acknowledgement that it read the frame. The wait is bounded
/// by `drain` ([`TERMINAL_DRAIN`] unless the caller has a reason to differ), so
/// the symmetric failure — a peer that never closes parking the responder
/// forever — is not reachable either.
///
/// # Errors
///
/// - [`SyncError::Transport`] if the stream was already closed by the peer.
/// - [`SyncError::Timeout`] if `drain` elapsed with the peer still not closed;
///   the connection is force-closed before returning, so the caller is never
///   left holding a live connection it believes is finished.
pub async fn finish_and_drain(
    send: &mut iroh::endpoint::SendStream,
    connection: &iroh::endpoint::Connection,
    drain: Duration,
) -> Result<(), SyncError> {
    send.finish()
        .map_err(|error| SyncError::Transport(std::io::Error::other(error)))?;

    match tokio::time::timeout(drain, connection.closed()).await {
        // `closed()` resolves with *why* the connection ended; for a drain, any
        // ending is success — the peer got the frame and hung up.
        Ok(_reason) => Ok(()),
        Err(_elapsed) => {
            connection.close(0u32.into(), b"terminal drain deadline");
            Err(SyncError::Timeout)
        }
    }
}
