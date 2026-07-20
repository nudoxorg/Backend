//! iroh transport for ObjectPack (INDEX-PLAN §7.2, ID-18).
//!
//! ## Model
//!
//! Two roles, mirroring `ir-sync`'s Syncer/SyncService split and reusing its
//! conventions:
//!
//! - [`ObjectPackProvider`] (server): binds an iroh endpoint on ALPN
//!   [`OBJECT_PACK_ALPN`] (`nudox/object-pack/1`) and serves reads out of a
//!   local [`ObjectPackStore`]. It enforces an **enrollment gate** (ID-18): a
//!   request from an endpoint not on its trusted list is rejected with a typed
//!   [`PackError::NotEnrolled`], and *nothing* is served.
//! - [`ObjectPackFetcher`] (client): connects to a provider and either pulls a
//!   whole pack (then BLAKE3-verifies it against its TOC and installs it
//!   atomically into a local store) or requests a single member range —
//!   Bao-verified when the member carries an outboard, whole-member otherwise.
//!
//! ## Why a bespoke framed protocol (not iroh-blobs)
//!
//! `ir-sync` moves opaque change blobs, so it leans on iroh-blobs' BLAKE3 blob
//! store. ObjectPack needs **content-typed** verification: a whole pack is
//! verified by re-deriving its [`ObjectPackId`] from the served bytes' TOC, and
//! a member *range* is verified with a Bao slice against the member's outboard
//! root. We therefore frame typed request/response messages over a QUIC
//! bi-stream (same length-prefixed postcard framing `ir-sync` uses) and do the
//! verification ourselves.
//!
//! ## ALPN / auth conventions (reused from ir-sync)
//!
//! - ALPN string family `nudox/<plane>/<version>` — here `nudox/object-pack/1`.
//! - Endpoints built with `presets::Minimal`; tests pass a `MemoryLookup` for
//!   in-process discovery with loopback binding and no relay.
//! - Trust is **content-addressing + enrollment**, never transport authority:
//!   every payload is verified after receipt, and provides are gated on the
//!   trusted-remote list.
//!
//! [`ObjectPackId`]: heart::object_pack::ObjectPackId

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use heart::deployment::TrustedRemote;
use heart::object_pack::{MemberKey, ObjectPackId};
use iroh::Endpoint;
use iroh::address_lookup::MemoryLookup;
use serde::{Deserialize, Serialize};

use crate::pack::error::PackError;
use crate::pack::outboard::{MemberOutboard, verify_bao_range};
use crate::pack::reader::ObjectPackReader;
use crate::pack::store::{EndpointId, ObjectPackStore};

/// ALPN for the ObjectPack member/pack transfer protocol (INDEX-PLAN §7.2).
pub const OBJECT_PACK_ALPN: &[u8] = b"nudox/object-pack/1";

/// Default timeout for a single provide/fetch exchange.
const OBJECT_PACK_TIMEOUT: Duration = Duration::from_secs(120);

/// Hard cap on a single framed message (matches ir-sync's frame discipline).
/// Whole-pack payloads can be large, so this is generous but still bounds a
/// hostile peer's declared length.
const FRAME_CAP_BYTES: usize = 512 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

/// A request from a fetcher to a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackRequest {
    /// Pull the whole sealed pack (bytes) plus all its outboards, so the
    /// fetcher can install it locally.
    WholePack {
        /// The pack to fetch.
        id: ObjectPackId,
    },
    /// Pull a single member's uncompressed byte range `[start, end)`. When the
    /// member has an outboard, the provider returns a Bao-verified slice;
    /// otherwise it returns the whole member's uncompressed bytes and the
    /// fetcher checks them against the member's TOC digest.
    MemberRange {
        /// The pack the member lives in.
        id: ObjectPackId,
        /// Which member.
        key: MemberKey,
        /// Inclusive start of the requested uncompressed byte range.
        start: u64,
        /// Exclusive end of the requested uncompressed byte range.
        end: u64,
    },
}

/// A provider's response to a [`PackRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackResponse {
    /// The whole pack bytes plus its outboards.
    WholePack {
        /// Sealed pack bytes.
        pack_bytes: Vec<u8>,
        /// All member outboards (sub-threshold members contribute none).
        outboards: Vec<MemberOutboard>,
    },
    /// A Bao-verified encoding of the requested range, with the outboard root
    /// and member length needed to verify it.
    VerifiedRange {
        /// BLAKE3 root of the member (equals its TOC content digest).
        root_hash: heart::content::ContentHash,
        /// Total uncompressed length of the member.
        uncompressed_length: u64,
        /// The Bao slice for the requested range.
        encoded: Vec<u8>,
    },
    /// The whole member's uncompressed bytes (used when no outboard exists).
    WholeMember {
        /// BLAKE3 digest the fetcher checks the bytes against.
        content: heart::content::ContentHash,
        /// The member's uncompressed bytes.
        bytes: Vec<u8>,
    },
    /// The request was refused. Carries a human-readable reason.
    Refused {
        /// Why the provider refused (e.g. not enrolled, absent pack).
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Trusted-remote policy (ID-18)
// ---------------------------------------------------------------------------

/// A provide destination derived from a [`TrustedRemote`], with the capability
/// gate the plan requires (INDEX-PLAN ID-18): a device may only `provide` to a
/// remote whose `can_provide` flag is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvideTarget {
    /// The remote's endpoint identity.
    pub endpoint: EndpointId,
    /// Human-chosen handle (for logs / errors).
    pub name: String,
}

impl ProvideTarget {
    /// Derive a provide target from a trusted remote, or reject when the remote
    /// is not authorized to receive provides from this device.
    ///
    /// # Errors
    ///
    /// [`PackError::NotEnrolled`] when `remote.can_provide` is false.
    pub fn from_trusted_remote(remote: &TrustedRemote) -> Result<Self, PackError> {
        if !remote.can_provide {
            return Err(PackError::NotEnrolled {
                detail: format!(
                    "remote {:?} is not authorized to receive provides (can_provide = false)",
                    remote.name
                ),
            });
        }
        Ok(ProvideTarget {
            endpoint: EndpointId(remote.endpoint.clone().into()),
            name: remote.name.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Provider (server)
// ---------------------------------------------------------------------------

/// Serves ObjectPack reads out of a local store over iroh, gated by an
/// enrollment allow-list of fetcher endpoints (INDEX-PLAN ID-18).
pub struct ObjectPackProvider<S: ObjectPackStore> {
    endpoint: Endpoint,
    store: Arc<S>,
    /// Endpoints permitted to fetch from this provider. A request from any
    /// other endpoint is refused with [`PackError::NotEnrolled`].
    enrolled: Vec<iroh::EndpointId>,
}

impl<S: ObjectPackStore + 'static> ObjectPackProvider<S> {
    /// Bind a provider endpoint with a randomly generated key.
    pub async fn new(
        store: Arc<S>,
        enrolled: Vec<iroh::EndpointId>,
        address_lookup: Option<MemoryLookup>,
    ) -> Result<Self, PackError> {
        Self::new_with_key(store, enrolled, address_lookup, iroh::SecretKey::generate()).await
    }

    /// Bind a provider endpoint with a specific secret key (tests pre-determine
    /// the `EndpointId` before binding).
    pub async fn new_with_key(
        store: Arc<S>,
        enrolled: Vec<iroh::EndpointId>,
        address_lookup: Option<MemoryLookup>,
        secret_key: iroh::SecretKey,
    ) -> Result<Self, PackError> {
        use iroh::endpoint::presets;

        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .alpns(vec![OBJECT_PACK_ALPN.to_vec()]);

        if let Some(lookup) = address_lookup {
            builder = builder
                .address_lookup(lookup)
                .bind_addr("127.0.0.1:0")
                .map_err(|error| PackError::Transport { detail: error.to_string() })?
                .bind_addr("[::1]:0")
                .map_err(|error| PackError::Transport { detail: error.to_string() })?;
        }

        let endpoint = builder
            .bind()
            .await
            .map_err(|error| PackError::Transport { detail: error.to_string() })?;

        Ok(Self { endpoint, store, enrolled })
    }

    /// The provider's iroh endpoint (tests share its address).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Whether a connecting endpoint is enrolled to fetch from this provider.
    fn is_enrolled(&self, endpoint: &iroh::EndpointId) -> bool {
        self.enrolled.iter().any(|allowed| allowed == endpoint)
    }

    /// Accept and handle exactly one incoming fetch connection.
    ///
    /// Returns the served [`PackResponse`] discriminant on success, or a typed
    /// [`PackError`] (including [`PackError::NotEnrolled`] when the peer is not
    /// on the allow-list — in which case a `Refused` is also sent so the peer
    /// learns why).
    pub async fn accept_one(&self) -> Result<(), PackError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| PackError::Transport { detail: "endpoint closed".to_owned() })?;

        let accepting = incoming
            .accept()
            .map_err(|error| PackError::Transport { detail: error.to_string() })?;

        let connection = accepting
            .await
            .map_err(|error| PackError::Transport { detail: error.to_string() })?;

        let remote_endpoint_id = connection.remote_id();

        let (mut send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|error| PackError::Transport { detail: error.to_string() })?;

        // Enrollment gate (ID-18): reject and inform non-enrolled peers.
        if !self.is_enrolled(&remote_endpoint_id) {
            let refused = PackResponse::Refused {
                reason: "endpoint is not enrolled to fetch from this provider".to_owned(),
            };
            let _ = send_framed(&mut send, &refused).await;
            let _ = send.finish();
            connection.close(0u32.into(), b"not enrolled");
            return Err(PackError::NotEnrolled {
                detail: format!("fetch from non-enrolled endpoint {remote_endpoint_id}"),
            });
        }

        let request: PackRequest = recv_framed(&mut recv).await?;
        let response = self.serve(&request);

        send_framed(&mut send, &response).await?;
        send.finish()
            .map_err(|error| PackError::Transport { detail: error.to_string() })?;
        connection.closed().await;
        Ok(())
    }

    /// Build the response for a request out of the local store.
    fn serve(&self, request: &PackRequest) -> PackResponse {
        match self.serve_inner(request) {
            Ok(response) => response,
            Err(error) => PackResponse::Refused { reason: error.to_string() },
        }
    }

    fn serve_inner(&self, request: &PackRequest) -> Result<PackResponse, PackError> {
        match request {
            PackRequest::WholePack { id } => {
                if !self.store.has(id) {
                    return Err(PackError::MemberNotFound {
                        // No dedicated "pack not found"; reuse a typed miss.
                        key: MemberKey::Meta { name: "<whole-pack>".into() },
                    });
                }
                let pack_bytes = self.store.read_pack_bytes(id)?.to_vec();
                let outboards = self.store.read_all_outboards(id)?;
                Ok(PackResponse::WholePack { pack_bytes, outboards })
            }
            PackRequest::MemberRange { id, key, start, end } => {
                match self.store.outboard(id, key)? {
                    Some(outboard) => {
                        // Verified range streaming via Bao.
                        let member = self.store.get_member(id, key)?;
                        let encoded = outboard.encode_range(&member, *start, *end)?;
                        Ok(PackResponse::VerifiedRange {
                            root_hash: outboard.root_hash,
                            uncompressed_length: outboard.uncompressed_length,
                            encoded: encoded.to_vec(),
                        })
                    }
                    None => {
                        // No outboard (sub-threshold member): whole-member fetch.
                        let member = self.store.get_member(id, key)?;
                        let content = heart::content::ContentHash::of_bytes(&member);
                        Ok(PackResponse::WholeMember { content, bytes: member.to_vec() })
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fetcher (client)
// ---------------------------------------------------------------------------

/// Connects to a provider and pulls packs / member ranges, verifying every
/// payload before it is installed or returned.
pub struct ObjectPackFetcher<S: ObjectPackStore> {
    endpoint: Endpoint,
    store: Arc<S>,
}

impl<S: ObjectPackStore + 'static> ObjectPackFetcher<S> {
    /// Bind a fetcher endpoint with a randomly generated key.
    pub async fn new(
        store: Arc<S>,
        address_lookup: Option<MemoryLookup>,
    ) -> Result<Self, PackError> {
        Self::new_with_key(store, address_lookup, iroh::SecretKey::generate()).await
    }

    /// Bind a fetcher endpoint with a specific secret key.
    pub async fn new_with_key(
        store: Arc<S>,
        address_lookup: Option<MemoryLookup>,
        secret_key: iroh::SecretKey,
    ) -> Result<Self, PackError> {
        use iroh::endpoint::presets;

        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .alpns(vec![OBJECT_PACK_ALPN.to_vec()]);

        if let Some(lookup) = address_lookup {
            builder = builder
                .address_lookup(lookup)
                .bind_addr("127.0.0.1:0")
                .map_err(|error| PackError::Transport { detail: error.to_string() })?
                .bind_addr("[::1]:0")
                .map_err(|error| PackError::Transport { detail: error.to_string() })?;
        }

        let endpoint = builder
            .bind()
            .await
            .map_err(|error| PackError::Transport { detail: error.to_string() })?;

        Ok(Self { endpoint, store })
    }

    /// The fetcher's iroh endpoint (tests share its address).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Fetch a whole pack from `provider`, verify it against `id`, and install
    /// it atomically into the local store.
    ///
    /// The install goes through [`ObjectPackStore::install_pack`], which
    /// re-derives the id from the received bytes and refuses a mismatch
    /// (tempfile + rename gives all-or-nothing durability).
    pub async fn fetch_whole_pack(
        &self,
        id: &ObjectPackId,
        provider: iroh::EndpointId,
    ) -> Result<(), PackError> {
        let response = tokio::time::timeout(
            OBJECT_PACK_TIMEOUT,
            self.request(provider, &PackRequest::WholePack { id: *id }),
        )
        .await
        .map_err(|_| PackError::Transport { detail: "fetch timed out".to_owned() })??;

        match response {
            PackResponse::WholePack { pack_bytes, outboards } => {
                // Verify identity before install: re-derive id from the bytes.
                let reader = ObjectPackReader::open_bytes(Bytes::from(pack_bytes.clone()))?;
                if reader.id() != *id {
                    return Err(PackError::FetchedIdMismatch);
                }
                self.store.install_pack(id, &pack_bytes, &outboards)
            }
            PackResponse::Refused { reason } => {
                Err(PackError::Transport { detail: format!("provider refused: {reason}") })
            }
            _ => Err(PackError::Transport {
                detail: "provider returned an unexpected response to WholePack".to_owned(),
            }),
        }
    }

    /// Fetch a single member's uncompressed range `[start, end)` from
    /// `provider`, verifying it (Bao when an outboard exists, whole-member
    /// digest otherwise) before returning the exact requested bytes.
    pub async fn fetch_member_range(
        &self,
        id: &ObjectPackId,
        key: &MemberKey,
        start: u64,
        end: u64,
        provider: iroh::EndpointId,
    ) -> Result<Bytes, PackError> {
        let request = PackRequest::MemberRange {
            id: *id,
            key: key.clone(),
            start,
            end,
        };
        let response = tokio::time::timeout(OBJECT_PACK_TIMEOUT, self.request(provider, &request))
            .await
            .map_err(|_| PackError::Transport { detail: "fetch timed out".to_owned() })??;

        match response {
            PackResponse::VerifiedRange { root_hash, uncompressed_length, encoded } => {
                verify_bao_range(&root_hash, uncompressed_length, &encoded, start, end)
            }
            PackResponse::WholeMember { content, bytes } => {
                let actual = heart::content::ContentHash::of_bytes(&bytes);
                if actual != content {
                    return Err(PackError::MemberHashMismatch { key: key.clone() });
                }
                if start > end || end > bytes.len() as u64 {
                    return Err(PackError::RangeOutOfBounds {
                        start,
                        end,
                        member_length: bytes.len() as u64,
                    });
                }
                Ok(Bytes::copy_from_slice(&bytes[start as usize..end as usize]))
            }
            PackResponse::Refused { reason } => {
                Err(PackError::Transport { detail: format!("provider refused: {reason}") })
            }
            _ => Err(PackError::Transport {
                detail: "provider returned an unexpected response to MemberRange".to_owned(),
            }),
        }
    }

    /// Open a bi-stream to `provider`, send `request`, await the response.
    async fn request(
        &self,
        provider: iroh::EndpointId,
        request: &PackRequest,
    ) -> Result<PackResponse, PackError> {
        let connection = self
            .endpoint
            .connect(provider, OBJECT_PACK_ALPN)
            .await
            .map_err(|error| PackError::Transport { detail: error.to_string() })?;

        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| PackError::Transport { detail: error.to_string() })?;

        send_framed(&mut send, request).await?;
        send.finish()
            .map_err(|error| PackError::Transport { detail: error.to_string() })?;

        let response: PackResponse = recv_framed(&mut recv).await?;
        connection.close(0u32.into(), b"done");
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// Trusted-remote background provide (ID-18)
// ---------------------------------------------------------------------------

/// Provide a pack to a trusted remote in the background (INDEX-PLAN §7.3).
///
/// This is a **caller-scheduled** entry point: it performs one provide-side
/// accept for one incoming fetch of `id`, gated on the remote being authorized
/// (`can_provide`) and enrolled. It does **not** spawn a runtime or a loop —
/// scheduling policy lives with the caller (server loops / GUI settings), per
/// the plan's "no runtime spawning policy here" note.
///
/// The flow is provider-passive: the trusted remote *fetches* from us over
/// [`ObjectPackProvider`]; this function derives the [`ProvideTarget`] (enforcing
/// the capability gate) and services one accept. Returns the target that was
/// served.
///
/// # Errors
///
/// - [`PackError::NotEnrolled`] when `remote.can_provide` is false.
/// - Any provider-side [`PackError`] from the accept.
pub async fn provide_to_trusted<S: ObjectPackStore + 'static>(
    provider: &ObjectPackProvider<S>,
    remote: &TrustedRemote,
    _id: &ObjectPackId,
) -> Result<ProvideTarget, PackError> {
    let target = ProvideTarget::from_trusted_remote(remote)?;
    provider.accept_one().await?;
    Ok(target)
}

// ---------------------------------------------------------------------------
// Framing (identical discipline to ir-sync)
// ---------------------------------------------------------------------------

/// Send a postcard-encoded value as a length-prefixed frame.
async fn send_framed<T: Serialize>(
    send: &mut iroh::endpoint::SendStream,
    value: &T,
) -> Result<(), PackError> {
    let encoded = postcard::to_allocvec(value)
        .map_err(|error| PackError::Codec { detail: error.to_string() })?;
    let length_prefix = (encoded.len() as u64).to_le_bytes();
    send.write_all(&length_prefix)
        .await
        .map_err(|error| PackError::Transport { detail: error.to_string() })?;
    send.write_all(&encoded)
        .await
        .map_err(|error| PackError::Transport { detail: error.to_string() })?;
    Ok(())
}

/// Read a length-prefixed postcard frame.
async fn recv_framed<T: for<'de> Deserialize<'de>>(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<T, PackError> {
    let mut length_buffer = [0u8; 8];
    recv.read_exact(&mut length_buffer)
        .await
        .map_err(|error| PackError::Transport { detail: error.to_string() })?;
    let length = u64::from_le_bytes(length_buffer) as usize;
    if length > FRAME_CAP_BYTES {
        return Err(PackError::Transport {
            detail: format!("frame too large: {length} bytes"),
        });
    }
    let mut buffer = vec![0u8; length];
    recv.read_exact(&mut buffer)
        .await
        .map_err(|error| PackError::Transport { detail: error.to_string() })?;
    postcard::from_bytes(&buffer).map_err(|error| PackError::Codec { detail: error.to_string() })
}
