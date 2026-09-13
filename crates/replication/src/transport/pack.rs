//! Immutable backend-store pack claims at the transport boundary.

use std::collections::BTreeMap;

use crate::{ReplicationError, SparseCoverage, TransportLimits};

/// An immutable physical pack received at the replication boundary.
///
/// `backend-store` owns the pack grammar and content-addressed admission. The
/// alias keeps the wire state visible to replication callers without creating
/// a second storage identity domain here.
pub type WirePack = backend_store::WirePack;
/// A pack after `backend-store` has checked its digest and canonical records.
pub type AdmittedPack = backend_store::Pack;

/// A bounded pack claim carried by [`crate::TransportMessage::WirePack`].
///
/// The physical layout identity is represented as raw bytes so a decoder can
/// retain the claim without manufacturing a `backend_store::LayoutId`. The
/// receiver must supply its caller-owned expected layout to
/// [`Self::admit_against`], which reconstructs the store wire value and sends
/// it through [`backend_store::admit_pack`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WirePackClaim {
    /// Claimed content digest.
    pub id: [u8; 32],
    /// Claimed physical layout identity.
    pub layout: [u8; 32],
    /// Canonical encoded records.
    pub bytes: Vec<u8>,
    /// Claimed key to byte offset and length directory.
    pub locations: BTreeMap<Vec<u8>, (u32, u32)>,
}

impl WirePackClaim {
    /// Copies a backend-store wire pack into a transport claim.
    #[must_use]
    pub fn from_wire(wire: &WirePack) -> Self {
        Self {
            id: wire.id,
            layout: *wire.layout.as_bytes(),
            bytes: wire.bytes.clone(),
            locations: wire.locations.clone(),
        }
    }

    /// Returns the exact payload size used by the canonical pack codec.
    #[must_use]
    pub fn wire_size(&self) -> usize {
        pack_claim_wire_size(self)
    }

    /// Checks claim shape and allocation bounds without granting a trusted
    /// store pack.
    ///
    /// # Errors
    ///
    /// Returns a size or corruption error when a directory entry is outside
    /// the claimed record bytes or the claim exceeds `limits`.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.wire_size().saturating_add(10) > limits.max_frame
            || self.bytes.len() > limits.max_frame
            || self.locations.len() > limits.max_objects
        {
            return Err(ReplicationError::MessageTooLarge);
        }
        for (key, &(offset, length)) in &self.locations {
            if key.len() > limits.max_key_bytes {
                return Err(ReplicationError::MessageTooLarge);
            }
            let end = usize::try_from(offset)
                .ok()
                .and_then(|start| {
                    usize::try_from(length)
                        .ok()
                        .and_then(|len| start.checked_add(len))
                })
                .ok_or(ReplicationError::Overflow)?;
            if end > self.bytes.len() {
                return Err(ReplicationError::CorruptFrame);
            }
        }
        Ok(())
    }

    /// Admits this claim against the caller-owned physical layout identity.
    ///
    /// # Errors
    ///
    /// Returns an identity, size, or corruption error when the claimed layout
    /// differs from `expected_layout`, or backend-store rejects the digest,
    /// record directory, or canonical values.
    pub fn admit_against(
        self,
        expected_layout: backend_store::LayoutId,
        limits: TransportLimits,
    ) -> Result<AdmittedPack, ReplicationError> {
        self.validate(limits)?;
        if self.layout != *expected_layout.as_bytes() {
            return Err(ReplicationError::IdentityMismatch);
        }
        admit_pack(
            backend_store::WirePack {
                id: self.id,
                layout: expected_layout,
                bytes: self.bytes,
                locations: self.locations,
            },
            limits,
        )
    }
}

impl From<WirePack> for WirePackClaim {
    fn from(wire: WirePack) -> Self {
        Self {
            id: wire.id,
            layout: *wire.layout.as_bytes(),
            bytes: wire.bytes,
            locations: wire.locations,
        }
    }
}

pub(super) fn coverage_wire_size(coverage: &SparseCoverage) -> usize {
    coverage.ranges().len().saturating_mul(16)
}

fn pack_wire_size(pack: &WirePack) -> usize {
    let claim = WirePackClaim::from_wire(pack);
    claim.wire_size().saturating_add(10)
}

fn pack_claim_wire_size(pack: &WirePackClaim) -> usize {
    32usize
        .saturating_add(32)
        .saturating_add(4)
        .saturating_add(pack.bytes.len())
        .saturating_add(4)
        .saturating_add(
            pack.locations
                .keys()
                .map(|key| 4usize.saturating_add(key.len()).saturating_add(8))
                .fold(0usize, usize::saturating_add),
        )
}

fn map_store_error(error: &backend_store::StoreError) -> ReplicationError {
    match error {
        backend_store::StoreError::Bounds | backend_store::StoreError::OversizedKey => {
            ReplicationError::MessageTooLarge
        }
        backend_store::StoreError::Corrupt
        | backend_store::StoreError::WrongBase
        | backend_store::StoreError::BeforeMismatch(_)
        | backend_store::StoreError::TargetMismatch
        | backend_store::StoreError::MalformedDelta
        | backend_store::StoreError::NeedsScopedRebuild
        | backend_store::StoreError::IncompleteCoverage
        | backend_store::StoreError::StaleHead
        | backend_store::StoreError::PublicationAuthorityBusy
        | backend_store::StoreError::PreparedWithSyncPending { .. }
        | backend_store::StoreError::PublishedWithSyncPending(_)
        | backend_store::StoreError::Io(_) => ReplicationError::CorruptFrame,
    }
}

/// Admits one physical pack through the canonical `backend-store` seam.
///
/// The wire pack remains untrusted until this function checks its bounded
/// shape and `backend-store` verifies the content digest, record directory,
/// and canonical value encoding. The returned pack can then enter a store or
/// an object-closure hydrator without replication inventing a second pack
/// identity.
///
/// # Errors
///
/// Returns [`ReplicationError::MessageTooLarge`] for a budget violation or
/// [`ReplicationError::CorruptFrame`] when store admission rejects the pack.
pub fn admit_pack(
    wire: WirePack,
    limits: TransportLimits,
) -> Result<AdmittedPack, ReplicationError> {
    limits.validate()?;
    if pack_wire_size(&wire) > limits.max_frame
        || wire.locations.len() > limits.max_objects
        || wire
            .locations
            .keys()
            .any(|key| key.len() > limits.max_key_bytes)
    {
        return Err(ReplicationError::MessageTooLarge);
    }
    backend_store::admit_pack(wire, limits.max_frame).map_err(|error| map_store_error(&error))
}
