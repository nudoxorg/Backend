//! iroh transport for ir-sync.
//!
//! ## Architecture
//!
//! ```text
//! Sender (Syncer)                           Receiver (SyncService)
//! ───────────────                           ──────────────────────
//! on_merge(MergeEvent)
//!   │ add each change as iroh blob          (receiver has no blob store; fetches only)
//!   │ build TipAnnouncement
//!   │ connect remote on OUR_ALPN ──────────►│ ep.accept() → dispatch on OUR_ALPN
//!   │──── TipAnnouncement (postcard) ──────►│ parse announcement
//!   │                                        │ for each change not present:
//!   │                                        │   connect to sender on iroh_blobs::ALPN
//!   │                                        │   fetch blob by iroh::Hash (BlobsProtocol)
//!   │                                        │   enforce MAX_CHANGE_BYTES
//!   │                                        │   verify pijul hash
//!   │                                        │   write_change
//!   │                                        │ apply_hook.apply(channel, changes)
//!   │◄─── SyncAck (postcard) ──────────────│
//! ```
//!
//! **Sender Router**: serves iroh-blobs via `BlobsProtocol` on `iroh_blobs::ALPN`.
//! **Receiver endpoint**: registered for both `ALPN` (our sync protocol) and
//! `iroh_blobs::ALPN`; accepted connections are dispatched manually via
//! `ep.accept()`. For blob fetches, the receiver *connects out* to the sender's
//! blob-serving Router — it does not serve blobs itself.
//!
//! **Size cap**: each change blob is capped at [`crate::io::MAX_CHANGE_BYTES`]
//! (64 MiB). Any blob that exceeds this limit causes the entire sync to fail with
//! [`crate::types::SyncError::ChangeTooLarge`] and NOTHING is written for that
//! announcement. This cap is enforced on received bytes, never on declared sizes.

use std::sync::Arc;
use std::time::Duration;

use iroh::Endpoint;
use iroh::address_lookup::MemoryLookup;
use iroh::protocol::Router;
use iroh_blobs::{BlobsProtocol, HashAndFormat, store::mem::MemStore};

use crate::io::{ApplyHook, ChangeIo, MAX_CHANGE_BYTES};
use crate::types::{ChangeId, IrohHash, MergeEvent, SyncAck, SyncError, TipAnnouncement};

/// ALPN for the ir-sync announcement/ack protocol.
pub const ALPN: &[u8] = b"nudox/ir-sync/1";

/// Default timeout for a single sync operation.
const SYNC_TIMEOUT: Duration = Duration::from_secs(60);

/// Configuration for the remote node to push changes to.
#[derive(Clone, Debug)]
pub struct RemoteConfig {
    /// The iroh node ID of the remote.
    pub node_id: iroh::EndpointId,
}

/// Sender side: triggered by merge events, pushes changes to a remote.
///
/// Owns an iroh endpoint (via a Router that serves iroh-blobs), a blob store,
/// and a `ChangeIo` reference for reading local change files.
pub struct Syncer<C: ChangeIo> {
    /// Router wrapping the sender's endpoint; serves iroh-blobs to the receiver.
    router: Router,
    store: MemStore,
    change_io: Arc<C>,
    remote: RemoteConfig,
}

impl<C: ChangeIo + 'static> Syncer<C> {
    /// Create a syncer with a randomly generated iroh key.
    ///
    /// `address_lookup` is optional — supply in tests so the two endpoints can
    /// discover each other without a relay server.
    pub async fn new(
        change_io: Arc<C>,
        remote: RemoteConfig,
        address_lookup: Option<MemoryLookup>,
    ) -> Result<Self, SyncError> {
        Self::new_with_key(change_io, remote, address_lookup, iroh::SecretKey::generate()).await
    }

    /// Create a syncer with a specific secret key (useful in tests to pre-determine
    /// the `EndpointId` before binding).
    pub async fn new_with_key(
        change_io: Arc<C>,
        remote: RemoteConfig,
        address_lookup: Option<MemoryLookup>,
        secret_key: iroh::SecretKey,
    ) -> Result<Self, SyncError> {
        use iroh::endpoint::presets;

        let store = MemStore::new();
        let blobs = BlobsProtocol::new(&store, None);

        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .alpns(vec![iroh_blobs::ALPN.to_vec()]);

        if let Some(lookup) = address_lookup {
            // Loopback-only binding so MemoryLookup addresses are reachable in-process.
            builder = builder
                .address_lookup(lookup)
                .bind_addr("127.0.0.1:0")
                .map_err(|e| SyncError::Transport(e.to_string()))?
                .bind_addr("[::1]:0")
                .map_err(|e| SyncError::Transport(e.to_string()))?;
        }

        let ep = builder
            .bind()
            .await
            .map_err(|e| SyncError::Transport(e.to_string()))?;

        let router = Router::builder(ep)
            .accept(iroh_blobs::ALPN, blobs)
            .spawn();

        Ok(Self {
            router,
            store,
            change_io,
            remote,
        })
    }

    /// Access the underlying iroh endpoint (needed to share addresses in tests).
    pub fn endpoint(&self) -> &Endpoint {
        self.router.endpoint()
    }

    /// Called by the repo merge hook when a commit lands on a durable channel.
    ///
    /// Steps:
    /// 1. Read each change from `change_io` and add it as an iroh blob.
    /// 2. Build a `TipAnnouncement` with both hashes per change.
    /// 3. Open a connection to the remote on our ALPN.
    /// 4. Send the announcement; await a `SyncAck`.
    pub async fn on_merge(&self, event: MergeEvent) -> Result<SyncAck, SyncError> {
        tokio::time::timeout(SYNC_TIMEOUT, self.push(event))
            .await
            .unwrap_or(Err(SyncError::Timeout))
    }

    async fn push(&self, event: MergeEvent) -> Result<SyncAck, SyncError> {
        // 1. Add each change as an iroh blob; collect (ChangeId, iroh::Hash) pairs.
        let mut change_pairs: Vec<(ChangeId, IrohHash)> = Vec::new();
        for change_id in &event.new_changes {
            let bytes = self
                .change_io
                .read_change(change_id)
                .map_err(|_| SyncError::MissingChange(change_id.clone()))?;

            if bytes.len() > MAX_CHANGE_BYTES {
                return Err(SyncError::ChangeTooLarge {
                    id: change_id.clone(),
                    size: bytes.len(),
                    max: MAX_CHANGE_BYTES,
                });
            }

            let tag = self
                .store
                .add_bytes(bytes)
                .await
                .map_err(|e| SyncError::Transport(e.to_string()))?;

            change_pairs.push((change_id.clone(), IrohHash::from(tag.hash)));
        }

        // 2. Build the announcement.
        let announcement = TipAnnouncement {
            channel: event.channel,
            tip: event.tip,
            changes: change_pairs,
        };

        // 3. Connect to the remote on our ALPN.
        let conn = self
            .router
            .endpoint()
            .connect(self.remote.node_id, ALPN)
            .await
            .map_err(|e| SyncError::Transport(e.to_string()))?;

        // 4. Send announcement; await ack.
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| SyncError::Transport(e.to_string()))?;

        send_framed(&mut send, &announcement).await?;
        send.finish()
            .map_err(|e| SyncError::Transport(e.to_string()))?;

        let ack: SyncAck = recv_framed(&mut recv).await?;

        conn.close(0u32.into(), b"done");

        Ok(ack)
    }
}

// ---------------------------------------------------------------------------
// Receiver (SyncService)
// ---------------------------------------------------------------------------

/// Receiver side: accepts connections on our ALPN; for each announcement,
/// downloads the change blobs from the sender's blob store, verifies them,
/// writes them, and applies them via an `ApplyHook`.
///
/// The receiver endpoint is registered for both our custom ALPN (for sync
/// announcements) and `iroh_blobs::ALPN` (for blob fetching — even though the
/// receiver doesn't serve blobs, registering the ALPN avoids rejection). Blob
/// fetches go *out* to the sender's Router over `iroh_blobs::ALPN`.
pub struct SyncService<C: ChangeIo, A: ApplyHook> {
    /// Endpoint registered for our sync ALPN (and optionally iroh-blobs ALPN).
    endpoint: Endpoint,
    /// In-memory blob store for fetched blobs (transient; only held during sync).
    store: MemStore,
    change_io: Arc<C>,
    apply_hook: Arc<A>,
    /// The iroh node ID of the sender; used to connect for blob fetches.
    sender_endpoint_id: iroh::EndpointId,
    /// Endpoints enrolled to push IR changes to this receiver (INDEX-PLAN
    /// ID-18: the same trust gate the ObjectPack plane uses). A connection from
    /// any endpoint outside this set is refused with
    /// [`SyncError::RemoteRefused`] before any bytes are processed.
    ///
    /// Defaults to `{ sender_endpoint_id }` so existing single-sender wiring is
    /// unchanged; callers may widen it via [`SyncService::enroll`].
    enrolled: Vec<iroh::EndpointId>,
}

impl<C: ChangeIo + 'static, A: ApplyHook + 'static> SyncService<C, A> {
    /// Create and bind the receiver's iroh endpoint with a randomly generated key.
    pub async fn new(
        change_io: Arc<C>,
        apply_hook: Arc<A>,
        sender_endpoint_id: iroh::EndpointId,
        address_lookup: Option<MemoryLookup>,
    ) -> Result<Self, SyncError> {
        Self::new_with_key(
            change_io,
            apply_hook,
            sender_endpoint_id,
            address_lookup,
            iroh::SecretKey::generate(),
        )
        .await
    }

    /// Create and bind the receiver's iroh endpoint with a specific secret key
    /// (useful in tests to pre-determine the `EndpointId` before binding).
    pub async fn new_with_key(
        change_io: Arc<C>,
        apply_hook: Arc<A>,
        sender_endpoint_id: iroh::EndpointId,
        address_lookup: Option<MemoryLookup>,
        secret_key: iroh::SecretKey,
    ) -> Result<Self, SyncError> {
        use iroh::endpoint::presets;

        let store = MemStore::new();

        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .alpns(vec![ALPN.to_vec()]);

        if let Some(lookup) = address_lookup {
            // Loopback-only binding so MemoryLookup addresses are reachable in-process.
            builder = builder
                .address_lookup(lookup)
                .bind_addr("127.0.0.1:0")
                .map_err(|e| SyncError::Transport(e.to_string()))?
                .bind_addr("[::1]:0")
                .map_err(|e| SyncError::Transport(e.to_string()))?;
        }

        let endpoint = builder
            .bind()
            .await
            .map_err(|e| SyncError::Transport(e.to_string()))?;

        Ok(Self {
            endpoint,
            store,
            change_io,
            apply_hook,
            sender_endpoint_id,
            // Default trust: only the configured sender may push.
            enrolled: vec![sender_endpoint_id],
        })
    }

    /// Enroll an additional endpoint permitted to push IR changes to this
    /// receiver (INDEX-PLAN ID-18). Additive: the configured sender is always
    /// enrolled; this widens the set (e.g. an edge Remote host trusting several
    /// devices). Returns `self` for builder-style chaining.
    pub fn enroll(mut self, endpoint: iroh::EndpointId) -> Self {
        if !self.enrolled.contains(&endpoint) {
            self.enrolled.push(endpoint);
        }
        self
    }

    /// Whether `endpoint` is enrolled to push to this receiver.
    fn is_enrolled(&self, endpoint: &iroh::EndpointId) -> bool {
        self.enrolled.iter().any(|allowed| allowed == endpoint)
    }

    /// Access the underlying iroh endpoint (needed to share addresses in tests).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Accept a single incoming sync connection on our ALPN and handle it.
    ///
    /// This is the unit-testable entry point. Call it in a loop for a
    /// long-running service, or call it once per test.
    pub async fn accept_one(self) -> Result<SyncAck, SyncError> {
        // Wait for an incoming connection on our ALPN.
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or(SyncError::ConnectionFailed)?;

        let mut accepting = incoming.accept()
            .map_err(|e| SyncError::Transport(e.to_string()))?;

        // Verify ALPN.
        let alpn = accepting.alpn().await
            .map_err(|e| SyncError::Transport(e.to_string()))?;
        if alpn != ALPN {
            return Err(SyncError::RemoteRefused(format!(
                "unexpected ALPN: {:?}",
                alpn
            )));
        }

        let conn = accepting.await
            .map_err(|e| SyncError::Transport(e.to_string()))?;

        // Trust gate (INDEX-PLAN ID-18): reject pushes from endpoints that are
        // not enrolled, before any announcement bytes are read or applied.
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
            .map_err(|e| SyncError::Transport(e.to_string()))?;

        // Read the TipAnnouncement.
        let announcement: TipAnnouncement = recv_framed(&mut recv).await?;

        // For each change, check presence; if missing, fetch from sender.
        let mut applied: u64 = 0;
        let mut to_apply: Vec<ChangeId> = Vec::new();

        for (change_id, iroh_hash) in &announcement.changes {
            if self.change_io.has_change(change_id)? {
                // Already present — idempotent.
                to_apply.push(change_id.clone());
                continue;
            }

            // Fetch via iroh-blobs from the sender's Router.
            let blob_hash = iroh_blobs::Hash::from(iroh_hash.clone());
            let haf = HashAndFormat::raw(blob_hash);

            let blob_conn = self
                .endpoint
                .connect(self.sender_endpoint_id, iroh_blobs::ALPN)
                .await
                .map_err(|e| SyncError::Transport(e.to_string()))?;

            self.store
                .remote()
                .fetch(blob_conn, haf)
                .await
                .map_err(|e| SyncError::Transport(e.to_string()))?;

            // Read back the bytes and enforce size cap.
            let bytes = {
                use tokio::io::AsyncReadExt;
                let mut reader = self.store.reader(blob_hash);
                let mut buf = Vec::new();
                reader
                    .read_to_end(&mut buf)
                    .await
                    .map_err(SyncError::Io)?;
                buf
            };

            if bytes.len() > MAX_CHANGE_BYTES {
                return Err(SyncError::ChangeTooLarge {
                    id: change_id.clone(),
                    size: bytes.len(),
                    max: MAX_CHANGE_BYTES,
                });
            }

            // Verify pijul hash. MUST NOT write on failure.
            self.change_io
                .verify(change_id, &bytes)
                .map_err(SyncError::VerificationFailed)?;

            // Write to local store (verified bytes only).
            self.change_io.write_change(change_id, &bytes)?;

            to_apply.push(change_id.clone());
            applied += 1;
        }

        // Apply all changes to the channel (repo side).
        let new_tip = self
            .apply_hook
            .apply(&announcement.channel, &to_apply)?;

        let ack = SyncAck {
            tip: new_tip,
            applied,
        };

        // Send the ack.
        send_framed(&mut send, &ack).await?;
        send.finish()
            .map_err(|e| SyncError::Transport(e.to_string()))?;

        conn.closed().await;
        Ok(ack)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Send a postcard-encoded value as a length-prefixed frame.
async fn send_framed<T: serde::Serialize>(
    send: &mut iroh::endpoint::SendStream,
    value: &T,
) -> Result<(), SyncError> {
    let encoded = postcard::to_allocvec(value)
        .map_err(|e| SyncError::Codec(e.to_string()))?;
    let len_prefix = (encoded.len() as u32).to_le_bytes();
    send.write_all(&len_prefix)
        .await
        .map_err(|e| SyncError::Transport(e.to_string()))?;
    send.write_all(&encoded)
        .await
        .map_err(|e| SyncError::Transport(e.to_string()))?;
    Ok(())
}

/// Read a length-prefixed postcard frame from a QUIC recv stream.
async fn recv_framed<T: for<'de> serde::Deserialize<'de>>(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<T, SyncError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| SyncError::Transport(e.to_string()))?;
    let len = u32::from_le_bytes(len_buf) as usize;

    // Sanity cap: a single announcement frame should never exceed 16 MiB.
    const FRAME_CAP: usize = 16 * 1024 * 1024;
    if len > FRAME_CAP {
        return Err(SyncError::Transport(format!(
            "frame too large: {len} bytes"
        )));
    }

    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .map_err(|e| SyncError::Transport(e.to_string()))?;

    postcard::from_bytes(&buf).map_err(|e| SyncError::Codec(e.to_string()))
}
