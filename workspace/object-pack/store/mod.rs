//! The `ObjectPackStore` trait (INDEX-PLAN §6.2) and its filesystem
//! implementation.
//!
//! A store owns *sealed* packs by [`ObjectPackId`] and answers range gets. The
//! iroh transport methods are part of the trait surface today but return
//! [`PackError::TransportNotWired`] until the sync wave lands (INDEX-PLAN §7.2).

mod filesystem;

pub use filesystem::FilesystemObjectPackStore;

use std::ops::Range;

use bytes::Bytes;
use heart::object_pack::{MemberKey, ObjectPackId};

use crate::builder::ObjectPackBuilder;
use crate::error::PackError;
use crate::outboard::MemberOutboard;

/// An opaque handle to a remote iroh endpoint: the z-base-32 public-key string
/// of the peer, matching [`heart::deployment::TrustedRemote::endpoint`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EndpointId(pub smol_str::SmolStr);

/// A content-addressed store of sealed ObjectPacks (INDEX-PLAN §6.2).
///
/// Implementations must be safe to share across threads. Every method is total
/// and returns a typed [`PackError`]; none panics on missing or corrupt input.
pub trait ObjectPackStore: Send + Sync {
    /// Seal a builder and persist the resulting pack, returning its identity.
    ///
    /// Sealing is deterministic, so re-putting an identical tree is idempotent:
    /// the same bytes land at the same id.
    fn put_pack(&self, builder: ObjectPackBuilder) -> Result<ObjectPackId, PackError>;

    /// Whether a pack with this identity is present in the store.
    fn has(&self, id: &ObjectPackId) -> bool;

    /// Range get: a byte range *within* one member of a stored pack
    /// (INDEX-PLAN §6.2 — the single-snippet serve path). Decompresses only the
    /// chunks the range covers.
    fn get_member_range(
        &self,
        id: &ObjectPackId,
        key: &MemberKey,
        range: Range<u64>,
    ) -> Result<Bytes, PackError>;

    /// Fetch one whole member of a stored pack.
    fn get_member(&self, id: &ObjectPackId, key: &MemberKey) -> Result<Bytes, PackError>;

    /// The raw sealed bytes of a stored pack (for whole-pack transfer over
    /// iroh). These are byte-identical to what [`Self::put_pack`] wrote, so a
    /// receiver can re-derive the [`ObjectPackId`] from them.
    fn read_pack_bytes(&self, id: &ObjectPackId) -> Result<Bytes, PackError>;

    /// All Bao outboards for a stored pack (empty when it has no large members).
    fn read_all_outboards(&self, id: &ObjectPackId) -> Result<Vec<MemberOutboard>, PackError>;

    /// The Bao outboard for a large member (INDEX-PLAN §6.2), if one exists.
    ///
    /// Returns `Ok(None)` when the member is below
    /// [`heart::object_pack::BAO_OUTBOARD_THRESHOLD_BYTES`] (no outboard is
    /// generated for small members). The outboard is read from the pack's
    /// sidecar, never from the frozen pack bytes.
    fn outboard(
        &self,
        id: &ObjectPackId,
        key: &MemberKey,
    ) -> Result<Option<MemberOutboard>, PackError>;

    /// Install a fully-materialised pack (bytes already sealed elsewhere, e.g.
    /// a verified iroh fetch) together with its outboards, atomically.
    ///
    /// The bytes must be a well-formed pack whose id equals `id`; the store
    /// re-derives the id and rejects a mismatch. Sidecar outboards are written
    /// beside the pack.
    fn install_pack(
        &self,
        id: &ObjectPackId,
        pack_bytes: &[u8],
        outboards: &[MemberOutboard],
    ) -> Result<(), PackError>;

    /// Begin providing a stored pack to trusted remotes over iroh.
    ///
    /// The store trait is synchronous; the actual iroh serving loop lives in
    /// [`crate::transport::ObjectPackProvider`]. This method exists on the trait
    /// only for the historical signature and returns
    /// [`PackError::TransportNotWired`] — callers use the transport module.
    fn provide_iroh(&self, id: &ObjectPackId) -> Result<(), PackError>;

    /// Fetch a pack from a remote endpoint over iroh.
    ///
    /// As with [`Self::provide_iroh`], the real fetch is async and lives in
    /// [`crate::transport::ObjectPackFetcher`]; this trait method returns
    /// [`PackError::TransportNotWired`].
    fn fetch_iroh(&self, id: &ObjectPackId, from: &EndpointId) -> Result<(), PackError>;
}
