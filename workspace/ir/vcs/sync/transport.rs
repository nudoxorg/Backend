//! iroh transport for `nudox_ir::sync`, over `index::transport`.
//!
//! The bespoke iroh endpoint + iroh-blobs provide/fetch this module used to
//! carry now lives once in `index::transport` (CONSOLIDATION-NOTES §8b): the
//! endpoint builder is [`transport::bind_endpoint`], the opaque
//! content-addressed blob move is [`transport::blob::Provider`] /
//! [`transport::blob::Fetcher`], and the length-prefixed postcard framing is
//! [`transport::frame`]. This module keeps only the IR-plane control protocol
//! (announce tip → fetch missing changes → verify → write → apply → ack) and
//! its `heart::sync::{ContentIo, ApplyHook}` wiring.

use std::sync::Arc;
use std::time::Duration;

use heart::sync::{ApplyHook, ContentIo, SyncError};
use transport::blob::{Fetcher, Provider, TransportHash};
use transport::endpoint::{AddressLookup, EndpointId, SecretKey, bind_endpoint};
use transport::frame::{TERMINAL_DRAIN, finish_and_drain, recv_framed, send_framed};

use super::MAX_CHANGE_BYTES;
use super::types::{
    ChangeId, ChannelRef, IrohHash, MergeEvent, SyncAck, SyncResponse, TipAnnouncement,
};

/// ALPN for the ir-sync announcement/ack control protocol.
pub const ALPN: &[u8] = b"nudox/ir-sync/1";

const SYNC_TIMEOUT: Duration = Duration::from_secs(60);

/// Cap on a single announcement/ack control frame. Announcements are tiny; the
/// change bytes ride the blob channel (bounded by [`MAX_CHANGE_BYTES`]).
const CONTROL_FRAME_CAP: usize = 16 * 1024 * 1024;

/// Configuration for the remote node to push changes to.
#[derive(Clone, Debug)]
pub struct RemoteConfig {
    pub node_id: EndpointId,
}

/// Sender side: triggered by merge events, pushes changes to a remote.
pub struct Syncer<C: ContentIo<Id = ChangeId>> {
    provider: Provider,
    change_io: Arc<C>,
    remote: RemoteConfig,
}

impl<C: ContentIo<Id = ChangeId> + 'static> Syncer<C> {
    pub async fn new(
        change_io: Arc<C>,
        remote: RemoteConfig,
        address_lookup: Option<AddressLookup>,
    ) -> Result<Self, SyncError> {
        Self::new_with_key(change_io, remote, address_lookup, SecretKey::generate()).await
    }

    pub async fn new_with_key(
        change_io: Arc<C>,
        remote: RemoteConfig,
        address_lookup: Option<AddressLookup>,
        secret_key: SecretKey,
    ) -> Result<Self, SyncError> {
        // The sender serves change blobs over iroh-blobs' ALPN; the receiver
        // pulls them by transport hash after seeing our announcement.
        let endpoint = bind_endpoint(
            vec![transport::blob::BLOBS_ALPN.to_vec()],
            secret_key,
            address_lookup,
        )
        .await?;
        let provider = Provider::serve(endpoint);

        Ok(Self {
            provider,
            change_io,
            remote,
        })
    }

    pub fn endpoint(&self) -> &iroh::Endpoint {
        self.provider.endpoint()
    }

    pub async fn on_merge(&self, event: MergeEvent) -> Result<SyncAck, SyncError> {
        tokio::time::timeout(SYNC_TIMEOUT, self.push(event))
            .await
            .unwrap_or(Err(SyncError::Timeout))
    }

    async fn push(&self, event: MergeEvent) -> Result<SyncAck, SyncError> {
        let mut change_pairs: Vec<(ChangeId, IrohHash)> = Vec::new();
        for change_id in &event.new_changes {
            let bytes = self
                .change_io
                .read(change_id)
                .map_err(|_| SyncError::MissingItem(change_id.to_string()))?;

            if bytes.len() > MAX_CHANGE_BYTES {
                return Err(SyncError::ItemTooLarge {
                    id: change_id.to_string(),
                    size: bytes.len(),
                    max: MAX_CHANGE_BYTES,
                });
            }

            let transport_hash = self.provider.add_bytes(bytes).await?;
            change_pairs.push((change_id.clone(), IrohHash(transport_hash.0)));
        }

        let announcement = TipAnnouncement {
            channel: event.channel,
            tip: event.tip,
            changes: change_pairs,
        };

        let conn = self
            .provider
            .endpoint()
            .connect(self.remote.node_id, ALPN)
            .await
            .map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;

        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;

        send_framed(&mut send, &announcement).await?;
        send.finish()
            .map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;

        let response: SyncResponse = recv_framed(&mut recv, CONTROL_FRAME_CAP).await?;

        // Closing here is also what releases the receiver from its terminal
        // drain (`finish_and_drain`), so it must happen on both arms.
        conn.close(0u32.into(), b"done");

        match response {
            SyncResponse::Ack(ack) => Ok(ack),
            SyncResponse::Refused { reason } => Err(SyncError::RemoteRefused(reason)),
        }
    }
}

// ---------------------------------------------------------------------------
// Receiver (SyncService)
// ---------------------------------------------------------------------------

pub struct SyncService<C, A>
where
    C: ContentIo<Id = ChangeId>,
    A: ApplyHook<Id = ChangeId, Target = ChannelRef, Tip = ChangeId>,
{
    fetcher: Fetcher,
    change_io: Arc<C>,
    apply_hook: Arc<A>,
    sender_endpoint_id: EndpointId,
    enrolled: Vec<EndpointId>,
}

impl<C, A> SyncService<C, A>
where
    C: ContentIo<Id = ChangeId> + 'static,
    A: ApplyHook<Id = ChangeId, Target = ChannelRef, Tip = ChangeId> + 'static,
{
    pub async fn new(
        change_io: Arc<C>,
        apply_hook: Arc<A>,
        sender_endpoint_id: EndpointId,
        address_lookup: Option<AddressLookup>,
    ) -> Result<Self, SyncError> {
        Self::new_with_key(
            change_io,
            apply_hook,
            sender_endpoint_id,
            address_lookup,
            SecretKey::generate(),
        )
        .await
    }

    pub async fn new_with_key(
        change_io: Arc<C>,
        apply_hook: Arc<A>,
        sender_endpoint_id: EndpointId,
        address_lookup: Option<AddressLookup>,
        secret_key: SecretKey,
    ) -> Result<Self, SyncError> {
        // The receiver accepts the control protocol on our ALPN; blob pulls open
        // their own iroh-blobs connection back to the sender on demand.
        let endpoint = bind_endpoint(vec![ALPN.to_vec()], secret_key, address_lookup).await?;
        let fetcher = Fetcher::new(endpoint);

        Ok(Self {
            fetcher,
            change_io,
            apply_hook,
            sender_endpoint_id,
            enrolled: vec![sender_endpoint_id],
        })
    }

    pub fn enroll(mut self, endpoint: EndpointId) -> Self {
        if !self.enrolled.contains(&endpoint) {
            self.enrolled.push(endpoint);
        }
        self
    }

    fn is_enrolled(&self, endpoint: &EndpointId) -> bool {
        self.enrolled.iter().any(|allowed| allowed == endpoint)
    }

    pub fn endpoint(&self) -> &iroh::Endpoint {
        self.fetcher.endpoint()
    }

    pub async fn accept_one(self) -> Result<SyncAck, SyncError> {
        let incoming = self
            .fetcher
            .endpoint()
            .accept()
            .await
            .ok_or(SyncError::ConnectionFailed)?;

        let mut accepting = incoming
            .accept()
            .map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;

        let alpn = accepting
            .alpn()
            .await
            .map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;
        if alpn != ALPN {
            return Err(SyncError::RemoteRefused(format!(
                "unexpected ALPN: {alpn:?}"
            )));
        }

        let conn = accepting
            .await
            .map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;

        let remote_endpoint_id = conn.remote_id();
        if !self.is_enrolled(&remote_endpoint_id) {
            let reason = format!("push from non-enrolled endpoint {remote_endpoint_id}");
            // `accept_one` owns `self`, so returning here drops the whole
            // `Endpoint` — the peer never even received the CONNECTION_CLOSE and
            // sat on its read until QUIC's idle timer. Tell it, then leave.
            return Err(match refuse(&conn, &reason).await {
                Ok(()) => SyncError::RemoteRefused(reason),
                Err(undelivered) => SyncError::RemoteRefused(format!(
                    "{reason}; refusal could not be delivered: {undelivered}"
                )),
            });
        }

        self.handle_connection(conn).await
    }

    /// Run the exchange and answer with exactly one terminal frame, whatever the
    /// outcome.
    ///
    /// The receiver's local result and the frame the sender sees are derived
    /// from the same value, so there is no path that fails locally and stays
    /// silent on the wire — which is what turned every verification failure into
    /// a 30-second stall on the sender.
    async fn handle_connection(
        &self,
        conn: iroh::endpoint::Connection,
    ) -> Result<SyncAck, SyncError> {
        let (mut send, mut recv) = conn
            .accept_bi()
            .await
            .map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;

        let outcome = self.fetch_verify_apply(&mut recv).await;

        let response = match &outcome {
            Ok(ack) => SyncResponse::Ack(ack.clone()),
            Err(error) => SyncResponse::Refused {
                reason: error.to_string(),
            },
        };

        let delivery = async {
            send_framed(&mut send, &response).await?;
            finish_and_drain(&mut send, &conn, TERMINAL_DRAIN).await
        }
        .await;

        match outcome {
            Ok(ack) => {
                delivery?;
                Ok(ack)
            }
            // The local failure is the diagnosis; a failure to deliver the
            // refusal on top of it must not replace it with a transport error.
            Err(error) => Err(error),
        }
    }

    /// The receiver's real work: pull each announced change, verify it against
    /// our own content-address id, write it, then apply the ordered set.
    async fn fetch_verify_apply(
        &self,
        recv: &mut iroh::endpoint::RecvStream,
    ) -> Result<SyncAck, SyncError> {
        let announcement: TipAnnouncement = recv_framed(recv, CONTROL_FRAME_CAP).await?;

        let mut applied: u64 = 0;
        let mut to_apply: Vec<ChangeId> = Vec::new();

        for (change_id, iroh_hash) in &announcement.changes {
            if self.change_io.has(change_id)? {
                to_apply.push(change_id.clone());
                continue;
            }

            // Pull the opaque change blob by transport hash from the sender,
            // size-capped, then verify it against OUR content-address id before
            // writing — content-addressing stays the trust anchor.
            let bytes = self
                .fetcher
                .fetch(
                    self.sender_endpoint_id,
                    TransportHash(iroh_hash.0),
                    MAX_CHANGE_BYTES,
                )
                .await?;

            self.change_io
                .verify(change_id, &bytes)
                .map_err(SyncError::VerificationFailed)?;

            self.change_io.write(change_id, &bytes)?;

            to_apply.push(change_id.clone());
            applied += 1;
        }

        let new_tip = self.apply_hook.apply(&announcement.channel, &to_apply)?;

        Ok(SyncAck {
            tip: new_tip,
            applied,
        })
    }
}

/// Answer a connection we are declining before it has said anything.
///
/// The peer is already parked waiting for a response frame, so the refusal has
/// to travel on the bi-stream it opened. Both waits are bounded: a peer that
/// never opens a stream, and a peer that never closes, are the same denial of
/// service and neither may park the receiver.
///
/// Returns whether the refusal reached the peer, so the caller can say so rather
/// than silently discard it.
async fn refuse(conn: &iroh::endpoint::Connection, reason: &str) -> Result<(), SyncError> {
    let accepted = tokio::time::timeout(TERMINAL_DRAIN, conn.accept_bi())
        .await
        .map_err(|_| SyncError::Timeout)?;
    let (mut send, _recv) =
        accepted.map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;

    let response = SyncResponse::Refused {
        reason: reason.to_owned(),
    };
    send_framed(&mut send, &response).await?;
    finish_and_drain(&mut send, conn, TERMINAL_DRAIN).await
}
