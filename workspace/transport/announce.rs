//! Control-ALPN announcement path for federation sync push (CONSOLIDATION-NOTES §8e).
//!
//! After [`blob::Provider`] has served the content-addressed bytes, the
//! sender announces the `(id, transport_hash)` pairing to each remote peer
//! over a dedicated QUIC control stream. The peer's [`FederationSync::pull`]
//! then fetches the blob by transport hash and verifies it with its own
//! `ContentIo::verify` — the content-address trust model is unchanged.
//!
//! # Protocol
//!
//! One request/response over a QUIC bi-stream:
//!
//!   1. Sender opens a bi-stream and writes a length-prefixed postcard
//!      [`Announcement`] frame (`send_framed`).
//!   2. Receiver reads the announcement, invokes the caller's handler, and
//!      replies with an [`Ack`] frame.
//!   3. Sender reads the [`Ack`] and closes the connection.
//!
//! The frame cap for control messages is intentionally small (16 KiB):
//! announcements carry only a string id and a 32-byte hash; the actual bytes
//! ride the blob channel.
//!
//! # Receiver side
//!
//! [`serve_announcements`] runs an accept-loop on a bound endpoint, dispatching
//! each inbound connection to the caller's async handler. The handler receives
//! the decoded [`Announcement`] and returns an [`Ack`]. A failed handler gets
//! an `Ack { accepted: false }` sent back and the loop continues.
//!
//! Recommended integration:
//!
//! ```text
//! let ann_ep = bind_endpoint(vec![ANNOUNCE_ALPN.to_vec()], key, lookup).await?;
//! tokio::spawn(serve_announcements(ann_ep, |ann| async move {
//!     federation_sync.pull(&io, &fetcher, &ann.id, ann.transport_hash).await?;
//!     Ok(Ack { accepted: true })
//! }));
//! ```

use std::future::Future;
use std::sync::Arc;

use heart::sync::SyncError;

use crate::blob::TransportHash;
use crate::endpoint::EndpointId;
use crate::frame::{recv_framed, send_framed};

/// ALPN for the federation sync announcement/ack control protocol.
pub const ANNOUNCE_ALPN: &[u8] = b"nudox/sync-announce/1";

/// Cap on a single announcement/ack control frame (16 KiB). Announcements
/// carry only an id string and a 32-byte hash; the cap is intentionally small
/// to prevent a hostile peer from forcing a large allocation on the control path.
const CONTROL_FRAME_CAP: usize = 16 * 1024;

/// An announcement sent by a replicating node to each federation peer.
///
/// Carries the plane-level content-address id (rendered as a `String` so the
/// control protocol stays plane-agnostic — `ContentIo::Id: Display` renders
/// it at the call site) and the [`TransportHash`] the receiver fetches the
/// blob by.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Announcement {
    /// The plane-level content-address id (e.g. a pijul change hash, a pack
    /// member hash) rendered as a string. The receiver passes it back to its
    /// own `ContentIo` for pull lookup.
    pub id: String,
    /// The iroh-blobs BLAKE3 transport hash the peer fetches the blob by.
    pub transport_hash: TransportHash,
}

/// The acknowledgement returned by the receiver after handling an announcement.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Ack {
    /// Whether the receiver accepted (and will pull) the announced item.
    /// `false` means the receiver rejected or failed — the sender records this.
    pub accepted: bool,
}

/// Open a control stream to `peer` on [`ANNOUNCE_ALPN`] and deliver `ann`,
/// returning the peer's [`Ack`].
///
/// Error mapping:
/// * connection / stream errors → [`SyncError::Transport`]
/// * framing / codec errors     → [`SyncError::Codec`]
pub async fn send_announcement(
    endpoint: &iroh::Endpoint,
    peer: EndpointId,
    ann: &Announcement,
) -> Result<Ack, SyncError> {
    let conn = endpoint
        .connect(peer, ANNOUNCE_ALPN)
        .await
        .map_err(|error| SyncError::Transport(error.to_string()))?;

    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|error| SyncError::Transport(error.to_string()))?;

    send_framed(&mut send, ann).await?;
    send.finish()
        .map_err(|error| SyncError::Transport(error.to_string()))?;

    let ack: Ack = recv_framed(&mut recv, CONTROL_FRAME_CAP).await?;

    conn.close(0u32.into(), b"done");

    Ok(ack)
}

/// Accept incoming announcement connections on `endpoint` in a loop, invoking
/// `handler` for each decoded [`Announcement`], and writing the returned [`Ack`]
/// back to the peer.
///
/// The caller must bind `endpoint` advertising [`ANNOUNCE_ALPN`].  The loop runs
/// until the endpoint is closed (`accept()` returns `None`).
///
/// Each connection is handled in its own spawned task so the accept-loop keeps
/// running while pulls are in flight.  A handler error causes
/// `Ack { accepted: false }` to be sent; the loop then continues.
///
/// # Example
///
/// ```no_run
/// # use transport::announce::{ANNOUNCE_ALPN, serve_announcements, Ack};
/// # use transport::endpoint::{bind_endpoint, AddressLookup, SecretKey};
/// # use transport::SyncError;
/// # async fn example() -> Result<(), SyncError> {
/// let ep = bind_endpoint(
///     vec![ANNOUNCE_ALPN.to_vec()],
///     SecretKey::generate(),
///     None,
/// ).await?;
/// tokio::spawn(serve_announcements(ep, |ann| async move {
///     tracing::info!(id = %ann.id, "received push announcement");
///     // Caller would call FederationSync::pull here.
///     Ok(Ack { accepted: true })
/// }));
/// # Ok(())
/// # }
/// ```
pub async fn serve_announcements<H, F>(endpoint: iroh::Endpoint, handler: H)
where
    H: Fn(Announcement) -> F + Send + Sync + 'static,
    F: Future<Output = Result<Ack, SyncError>> + Send + 'static,
{
    let handler = Arc::new(handler);

    loop {
        let Some(incoming) = endpoint.accept().await else {
            // Endpoint closed — stop accepting.
            break;
        };

        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            let result = handle_one_connection(incoming, &*handler).await;
            if let Err(error) = result {
                tracing::warn!(%error, "announce: connection error");
            }
        });
    }
}

/// Handle one accepted incoming connection: check ALPN, read the announcement,
/// invoke the handler, write the ack.
async fn handle_one_connection<H, F>(
    incoming: iroh::endpoint::Incoming,
    handler: &H,
) -> Result<(), SyncError>
where
    H: Fn(Announcement) -> F,
    F: Future<Output = Result<Ack, SyncError>> + Send + 'static,
{
    let mut accepting = incoming
        .accept()
        .map_err(|error| SyncError::Transport(error.to_string()))?;

    let alpn = accepting
        .alpn()
        .await
        .map_err(|error| SyncError::Transport(error.to_string()))?;

    if alpn != ANNOUNCE_ALPN {
        return Err(SyncError::Transport(format!(
            "unexpected ALPN on announce listener: {:?}",
            alpn
        )));
    }

    let conn = accepting
        .await
        .map_err(|error| SyncError::Transport(error.to_string()))?;

    let (mut send, mut recv) = conn
        .accept_bi()
        .await
        .map_err(|error| SyncError::Transport(error.to_string()))?;

    let ann: Announcement = recv_framed(&mut recv, CONTROL_FRAME_CAP).await?;

    let ack = handler(ann).await.unwrap_or_else(|error| {
        tracing::warn!(%error, "announce: handler error, replying rejected");
        Ack { accepted: false }
    });

    send_framed(&mut send, &ack).await?;
    send.finish()
        .map_err(|error| SyncError::Transport(error.to_string()))?;

    conn.closed().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{AddressLookup, SecretKey, bind_endpoint};

    async fn make_receiver(key: SecretKey, lookup: AddressLookup) -> iroh::Endpoint {
        bind_endpoint(
            vec![ANNOUNCE_ALPN.to_vec()],
            key,
            Some(lookup),
        )
        .await
        .expect("bind receiver endpoint")
    }

    /// A sender endpoint needs no special ALPN — it only connects outward.
    async fn make_sender(key: SecretKey, lookup: AddressLookup) -> iroh::Endpoint {
        bind_endpoint(vec![], key, Some(lookup))
            .await
            .expect("bind sender endpoint")
    }

    #[tokio::test]
    async fn send_recv_announcement_round_trip() {
        let lookup = AddressLookup::default();

        let receiver_key = SecretKey::from_bytes(&[1u8; 32]);
        let receiver_id = receiver_key.public(); // derive id before moving key
        let receiver_ep = make_receiver(receiver_key, lookup.clone()).await;

        // Serve exactly one announcement in a spawned task.
        let receiver_task = tokio::spawn(async move {
            handle_one_connection(
                receiver_ep.accept().await.expect("incoming"),
                &|ann: Announcement| async move {
                    Ok(Ack { accepted: ann.id == "test-id-42" })
                },
            )
            .await
            .expect("handle_one_connection");
        });

        let sender_ep = make_sender(SecretKey::from_bytes(&[2u8; 32]), lookup).await;
        let ann = Announcement {
            id: "test-id-42".to_string(),
            transport_hash: TransportHash([0xab; 32]),
        };

        let ack = send_announcement(&sender_ep, receiver_id, &ann)
            .await
            .expect("send_announcement");

        assert!(ack.accepted, "receiver must accept the announcement");
        receiver_task.await.expect("receiver task panicked");
    }

    #[tokio::test]
    async fn handler_error_yields_rejected_ack() {
        let lookup = AddressLookup::default();

        let receiver_key = SecretKey::from_bytes(&[3u8; 32]);
        let receiver_id = receiver_key.public();
        let receiver_ep = make_receiver(receiver_key, lookup.clone()).await;

        let receiver_task = tokio::spawn(async move {
            handle_one_connection(
                receiver_ep.accept().await.expect("incoming"),
                &|_ann: Announcement| async move {
                    Err(SyncError::Other("simulated pull failure".into()))
                },
            )
            .await
            .expect("handle_one_connection");
        });

        let sender_ep = make_sender(SecretKey::from_bytes(&[4u8; 32]), lookup).await;
        let ann = Announcement {
            id: "will-fail".to_string(),
            transport_hash: TransportHash([0u8; 32]),
        };

        let ack = send_announcement(&sender_ep, receiver_id, &ann)
            .await
            .expect("send_announcement");

        assert!(!ack.accepted, "handler error must yield rejected ack");
        receiver_task.await.expect("receiver task panicked");
    }
}
