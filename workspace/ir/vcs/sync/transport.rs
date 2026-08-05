//! iroh transport for `nudox_ir::sync`, over `index::transport`.
//!
//! The bespoke iroh endpoint + iroh-blobs provide/fetch this module used to
//! carry now lives once in `index::transport` (CONSOLIDATION-NOTES §8b): the
//! endpoint builder is [`index::transport::bind_endpoint`], the opaque
//! content-addressed blob move is [`index::transport::blob::Provider`] /
//! [`index::transport::blob::Fetcher`], and the length-prefixed postcard framing is
//! [`index::transport::frame`]. This module keeps only the IR-plane control protocol
//! (announce tip → fetch missing changes → verify → write → apply → ack) and
//! its `heart::sync::{ContentIo, ApplyHook}` wiring.

use std::sync::Arc;
use std::time::Duration;

use heart::sync::{ApplyHook, ContentIo, SyncError};
use index::transport::blob::{Fetcher, Provider, TransportHash};
use index::transport::endpoint::{AddressLookup, EndpointId, SecretKey, bind_endpoint};
use index::transport::frame::{recv_framed, send_framed};

use super::MAX_CHANGE_BYTES;
use super::types::{ChangeId, ChannelRef, IrohHash, MergeEvent, SyncAck, TipAnnouncement};

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
            vec![index::transport::blob::BLOBS_ALPN.to_vec()],
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

        let ack: SyncAck = recv_framed(&mut recv, CONTROL_FRAME_CAP).await?;

        conn.close(0u32.into(), b"done");

        Ok(ack)
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
                "unexpected ALPN: {:?}",
                alpn
            )));
        }

        let conn = accepting
            .await
            .map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;

        let remote_endpoint_id = conn.remote_id();
        if !self.is_enrolled(&remote_endpoint_id) {
            conn.close(0u32.into(), b"not enrolled");
            return Err(SyncError::RemoteRefused(format!(
                "push from non-enrolled endpoint {remote_endpoint_id}"
            )));
        }

        self.handle_connection(conn).await
    }

    async fn handle_connection(
        &self,
        conn: iroh::endpoint::Connection,
    ) -> Result<SyncAck, SyncError> {
        let (mut send, mut recv) = conn
            .accept_bi()
            .await
            .map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;

        let announcement: TipAnnouncement = recv_framed(&mut recv, CONTROL_FRAME_CAP).await?;

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

        let ack = SyncAck {
            tip: new_tip,
            applied,
        };

        send_framed(&mut send, &ack).await?;
        send.finish()
            .map_err(|e| SyncError::Transport(std::io::Error::other(e)))?;

        conn.closed().await;
        Ok(ack)
    }
}
