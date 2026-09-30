//! Portable, non-publishing V3 typed-history claims and locators.
//!
//! The V3 locator binds only the canonical manifest and semantic-to-FileStore
//! object bridge. It deliberately does not contain a closure, commit, or ref;
//! the root claim binds the locator identity and exact closure without a hash
//! cycle. Commit/ref integration is deferred until V3 input authority can be
//! checked by the history catalog.

use super::v2::{create_typed_v2_locator, decode_typed_v2_locator};
use super::*;

const TYPED_V3_ROOT_DISCRIMINATOR: u8 = 3;
const TYPED_V3_ROOT_DOMAIN: &[u8] = b"backend.semantic.history-typed-v3-root-claim.v1\0";
const TYPED_V3_LOCATOR_TAG: u8 = 16;
const TYPED_V3_LOCATOR_DOMAIN: &[u8] = b"backend.semantic.history-typed-v3-locator.v1\0";
const MAX_TYPED_V3_LOCATOR_BYTES: usize = MAX_HISTORY_TYPED_V2_LOCATOR_BYTES + 128;
const MAX_TYPED_V3_ROOT_CLAIM_BYTES: usize = 1 + 4 * 32 + CHECKSUM_BYTES;

/// Portable identity of one immutable V3 manifest/object-bridge locator.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HistoryTypedV3LocatorId([u8; 32]);

impl HistoryTypedV3LocatorId {
    /// Returns the fixed-width locator identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Wraps an untrusted locator identity claim.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Portable, claim-only typed V3 semantic root.
///
/// This value records untrusted roots, one exact FileStore closure, and the
/// content-addressed V3 locator. It is not a `HistoryGenerationRoot` and does
/// not authorize history commit or ref publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryTypedV3RootClaim {
    content_root: backend_semantic::ir::UntrustedSemanticContentRootV2,
    generation_root: backend_semantic::ir::UntrustedSemanticGenerationRootV2,
    closure: ArtifactClosureClaim,
    locator: HistoryTypedV3LocatorId,
}

impl HistoryTypedV3RootClaim {
    pub(crate) fn from_verified(
        content: &backend_semantic::ir::VerifiedTypedPlaneContentV2,
        closure: ArtifactClosureClaim,
        locator: HistoryTypedV3LocatorId,
    ) -> Self {
        Self {
            content_root: backend_semantic::ir::UntrustedSemanticContentRootV2::from_wire_claim(
                *content.content_root().as_bytes(),
            ),
            generation_root:
                backend_semantic::ir::UntrustedSemanticGenerationRootV2::from_wire_claim(
                    *content.generation_root().as_bytes(),
                ),
            closure,
            locator,
        }
    }

    /// Returns the V3 content-root claim for cold verification.
    #[must_use]
    pub const fn content_root_claim(self) -> backend_semantic::ir::UntrustedSemanticContentRootV2 {
        self.content_root
    }

    /// Returns the V3 generation-root claim for cold verification.
    #[must_use]
    pub const fn generation_root_claim(
        self,
    ) -> backend_semantic::ir::UntrustedSemanticGenerationRootV2 {
        self.generation_root
    }

    /// Returns the exact FileStore closure claim bound by this root.
    #[must_use]
    pub const fn closure(self) -> ArtifactClosureClaim {
        self.closure
    }

    /// Returns the canonical V3 manifest/object-bridge locator identity.
    #[must_use]
    pub const fn locator(self) -> HistoryTypedV3LocatorId {
        self.locator
    }

    /// Encodes this claim with the reserved V3 root discriminator and a
    /// domain-separated checksum. The commit codec can embed these bytes once
    /// catalog/ref admission is ready.
    pub fn encode_portable(self) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(MAX_TYPED_V3_ROOT_CLAIM_BYTES)
            .map_err(|_| "typed V3 root claim allocation failed".to_owned())?;
        bytes.push(TYPED_V3_ROOT_DISCRIMINATOR);
        bytes.extend_from_slice(self.content_root.as_bytes());
        bytes.extend_from_slice(self.generation_root.as_bytes());
        bytes.extend_from_slice(self.closure.as_bytes());
        bytes.extend_from_slice(self.locator.as_bytes());
        let mut hasher = blake3::Hasher::new();
        hasher.update(TYPED_V3_ROOT_DOMAIN);
        hasher.update(&bytes);
        bytes.extend_from_slice(hasher.finalize().as_bytes());
        Ok(bytes)
    }

    /// Decodes an untrusted portable V3 claim. It does not validate the
    /// referenced closure or locator and cannot produce admission authority.
    pub fn decode_portable(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != MAX_TYPED_V3_ROOT_CLAIM_BYTES
            || bytes.first().copied() != Some(TYPED_V3_ROOT_DISCRIMINATOR)
        {
            return Err("typed V3 root claim discriminator or length is invalid".to_owned());
        }
        let checksum_offset = bytes.len() - CHECKSUM_BYTES;
        let mut hasher = blake3::Hasher::new();
        hasher.update(TYPED_V3_ROOT_DOMAIN);
        hasher.update(&bytes[..checksum_offset]);
        if hasher.finalize().as_bytes() != &bytes[checksum_offset..] {
            return Err("typed V3 root claim checksum failed".to_owned());
        }
        let mut offset = 1;
        let mut take_root = || {
            let end = offset + 32;
            let root = bytes[offset..end]
                .try_into()
                .map_err(|_| "typed V3 root claim field has the wrong width".to_owned());
            offset = end;
            root
        };
        let content_root =
            backend_semantic::ir::UntrustedSemanticContentRootV2::from_wire_claim(take_root()?);
        let generation_root =
            backend_semantic::ir::UntrustedSemanticGenerationRootV2::from_wire_claim(take_root()?);
        let closure = ArtifactClosureClaim::from_bytes(take_root()?);
        let locator = HistoryTypedV3LocatorId(take_root()?);
        if offset != checksum_offset {
            return Err("typed V3 root claim has trailing fields".to_owned());
        }
        Ok(Self {
            content_root,
            generation_root,
            closure,
            locator,
        })
    }
}

/// Canonical V3 locator derived from a complete producer receipt set.
///
/// Its nested V2 bridge body intentionally reuses the existing canonical map
/// validator. The outer V3 tag and hash domain are distinct, so V3 metadata
/// cannot be mistaken for a V2 history locator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TypedV3HistoryLocator {
    bridge: TypedV2HistoryLocator,
}

impl TypedV3HistoryLocator {
    /// Derives the exact locator from all durable receipts emitted by the
    /// complete typed V3 producer. Duplicate rope occurrences are reduced to
    /// one semantic object mapping; conflicting aliases fail closed.
    pub(crate) fn from_produced(
        produced: &crate::ProducedSemanticTypedPlaneV3,
    ) -> Result<Self, String> {
        use crate::{ProducedSemanticObjectIdentity, ProducedSemanticObjectKind};
        use std::collections::BTreeMap;

        let manifest = produced
            .manifest()
            .canonical_bytes()
            .map_err(|error| format!("encode typed V3 history manifest: {error}"))?;
        let expected_segments = produced
            .manifest()
            .resource_usage()
            .map_err(|error| format!("measure typed V3 history manifest: {error}"))?
            .segment_descriptors();
        TypedV2HistoryLocator::preflight_admission_counts(
            expected_segments,
            produced.segment_admissions().len(),
            0,
        )?;

        let mut segments = Vec::new();
        segments
            .try_reserve_exact(produced.segment_admissions().len())
            .map_err(|_| "typed V3 history segment map allocation failed".to_owned())?;
        for receipt in produced.segment_admissions() {
            let ProducedSemanticObjectIdentity::Segment { id, .. } = receipt.identity() else {
                return Err("typed V3 segment receipt has a non-segment identity".to_owned());
            };
            if receipt.identity().kind() != ProducedSemanticObjectKind::Segment {
                return Err("typed V3 segment receipt kind is inconsistent".to_owned());
            }
            segments.push(HistoryTypedV2SegmentObject::new(
                backend_semantic::ir::UntrustedSemanticSegmentId::from_raw(*id.as_bytes()),
                UntrustedObjectId::from_bytes(*receipt.object_id().as_bytes()),
                receipt.payload_bytes(),
            ));
        }

        let mut jumbo_by_semantic = BTreeMap::new();
        for receipt in produced.jumbo_admissions() {
            let (id, kind) = match receipt.identity() {
                ProducedSemanticObjectIdentity::JumboLeaf { id, .. } => {
                    (id, backend_semantic::ir::JumboRopeObjectKind::Leaf)
                }
                ProducedSemanticObjectIdentity::JumboInterior { id, .. } => {
                    (id, backend_semantic::ir::JumboRopeObjectKind::Interior)
                }
                ProducedSemanticObjectIdentity::Segment { .. } => {
                    return Err("typed V3 jumbo receipt has a segment identity".to_owned());
                }
            };
            let expected_kind = match kind {
                backend_semantic::ir::JumboRopeObjectKind::Leaf => {
                    ProducedSemanticObjectKind::JumboLeaf
                }
                backend_semantic::ir::JumboRopeObjectKind::Interior => {
                    ProducedSemanticObjectKind::JumboInterior
                }
            };
            if receipt.identity().kind() != expected_kind {
                return Err("typed V3 jumbo receipt kind is inconsistent".to_owned());
            }
            let mapping = HistoryTypedV2JumboObject::new(
                id,
                kind,
                UntrustedObjectId::from_bytes(*receipt.object_id().as_bytes()),
                receipt.payload_bytes(),
            );
            let key = (
                match kind {
                    backend_semantic::ir::JumboRopeObjectKind::Leaf => 0_u8,
                    backend_semantic::ir::JumboRopeObjectKind::Interior => 1_u8,
                },
                *id.as_bytes(),
            );
            if let Some(previous) = jumbo_by_semantic.insert(key, mapping) {
                if previous != mapping {
                    return Err(
                        "typed V3 repeated rope identity has conflicting durable receipts"
                            .to_owned(),
                    );
                }
            }
        }
        if segments.len().saturating_add(jumbo_by_semantic.len())
            > MAX_HISTORY_TYPED_V2_LOCATOR_OBJECTS
        {
            return Err("typed V3 history locator exceeds its object bound".to_owned());
        }
        let mut jumbo = Vec::new();
        jumbo
            .try_reserve_exact(jumbo_by_semantic.len())
            .map_err(|_| "typed V3 history rope map allocation failed".to_owned())?;
        jumbo.extend(jumbo_by_semantic.into_values());
        let bridge = TypedV2HistoryLocator::from_admission_parts(
            manifest,
            expected_segments,
            &segments,
            &jumbo,
            None,
        )?;
        if bridge.validate()? != *produced.manifest() {
            return Err("typed V3 history locator differs from producer manifest".to_owned());
        }
        Ok(Self { bridge })
    }

    pub(crate) fn from_v2_bridge(bridge: TypedV2HistoryLocator) -> Result<Self, String> {
        if bridge.lineage_edge_set.is_some() || bridge.wire_revision != HISTORY_TYPED_V2_LOCATOR_TAG
        {
            return Err("typed V3 locator cannot carry V2-only lineage metadata".to_owned());
        }
        let _ = bridge.validate()?;
        Ok(Self { bridge })
    }

    pub(crate) fn bridge(&self) -> &TypedV2HistoryLocator {
        &self.bridge
    }

    pub(crate) fn validate(
        &self,
    ) -> Result<backend_semantic::ir::SemanticTypedPlaneManifestV2, String> {
        self.bridge.validate()
    }

    pub(crate) fn identity(&self) -> Result<HistoryTypedV3LocatorId, String> {
        Ok(typed_v3_locator_identity(&self.encode_body()?))
    }

    fn encode_body(&self) -> Result<Vec<u8>, String> {
        let _ = self.validate()?;
        let (_, bridge_bytes) = create_typed_v2_locator(self.bridge.clone())?;
        let mut writer = Writer::new(MAX_TYPED_V3_LOCATOR_BYTES);
        writer.header(TYPED_V3_LOCATOR_TAG)?;
        writer.sized_bytes(
            &bridge_bytes,
            MAX_HISTORY_TYPED_V2_LOCATOR_BYTES.saturating_add(64),
        )?;
        let body = writer.finish();
        if body.len() > MAX_TYPED_V3_LOCATOR_BYTES {
            return Err("typed V3 history locator exceeds its byte bound".to_owned());
        }
        Ok(body)
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        let body = self.encode_body()?;
        let identity = typed_v3_locator_identity(&body);
        let total = body
            .len()
            .checked_add(32 + CHECKSUM_BYTES)
            .ok_or_else(|| "typed V3 history locator length overflows".to_owned())?;
        if total > MAX_TYPED_V3_LOCATOR_BYTES + 32 + CHECKSUM_BYTES {
            return Err("typed V3 history locator exceeds its byte bound".to_owned());
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(|_| "typed V3 history locator allocation failed".to_owned())?;
        bytes.extend_from_slice(identity.as_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8], expected: HistoryTypedV3LocatorId) -> Result<Self, String> {
        let body = checked_body(bytes, MAX_TYPED_V3_LOCATOR_BYTES + 32 + CHECKSUM_BYTES)?;
        if body.len() < 32 || body[..32] != *expected.as_bytes() {
            return Err("typed V3 locator identity differs from its record prefix".to_owned());
        }
        let content = &body[32..];
        if typed_v3_locator_identity(content) != expected {
            return Err("typed V3 locator identity does not match its bytes".to_owned());
        }
        let mut reader = Reader::new(content);
        reader.header(TYPED_V3_LOCATOR_TAG)?;
        let bridge_bytes =
            reader.sized_bytes(MAX_HISTORY_TYPED_V2_LOCATOR_BYTES.saturating_add(64))?;
        reader.finish()?;
        if bridge_bytes.len() < 32 {
            return Err("typed V3 nested bridge locator is truncated".to_owned());
        }
        let bridge_id = HistoryTypedV2LocatorId(
            bridge_bytes[..32]
                .try_into()
                .map_err(|_| "typed V3 nested bridge ID has the wrong width".to_owned())?,
        );
        let locator = Self::from_v2_bridge(decode_typed_v2_locator(bridge_bytes, bridge_id)?)?;
        if locator.encode_body()?.as_slice() != content {
            return Err("typed V3 history locator is not canonically encoded".to_owned());
        }
        Ok(locator)
    }
}

fn typed_v3_locator_identity(body: &[u8]) -> HistoryTypedV3LocatorId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(TYPED_V3_LOCATOR_DOMAIN);
    hasher.update(&u64::try_from(body.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(body);
    HistoryTypedV3LocatorId(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_locator_has_a_distinct_wire_tag_and_cold_round_trips_c007_bridge() {
        let fixture = crate::ir_hydration_store::positive_v2_history_fixture_for_test();
        let locator = TypedV3HistoryLocator::from_v2_bridge(fixture.locator)
            .expect("canonical c007 bridge is portable under the V3 locator tag");
        let identity = locator.identity().expect("hash V3 locator body");
        let bytes = locator.encode().expect("encode V3 locator envelope");
        assert_eq!(bytes.get(32 + 5), Some(&TYPED_V3_LOCATOR_TAG));
        let decoded = TypedV3HistoryLocator::decode(&bytes, identity)
            .expect("cold decode V3 locator envelope");
        assert_eq!(decoded, locator);
        assert_eq!(
            decoded.validate().expect("validate V3 bridge"),
            fixture.manifest
        );

        let mut swapped_identity = *identity.as_bytes();
        swapped_identity[0] ^= 0x80;
        assert!(
            TypedV3HistoryLocator::decode(
                &bytes,
                HistoryTypedV3LocatorId::from_bytes(swapped_identity),
            )
            .is_err()
        );
    }

    #[test]
    fn standalone_v3_root_claim_round_trips_and_fails_closed_on_mutation() {
        let claim = HistoryTypedV3RootClaim {
            content_root: backend_semantic::ir::UntrustedSemanticContentRootV2::from_wire_claim(
                [0x31; 32],
            ),
            generation_root:
                backend_semantic::ir::UntrustedSemanticGenerationRootV2::from_wire_claim([0x42; 32]),
            closure: ArtifactClosureClaim::from_bytes([0x53; 32]),
            locator: HistoryTypedV3LocatorId::from_bytes([0x64; 32]),
        };
        let bytes = claim.encode_portable().expect("encode portable root claim");
        assert_eq!(bytes[0], TYPED_V3_ROOT_DISCRIMINATOR);
        assert_eq!(HistoryTypedV3RootClaim::decode_portable(&bytes), Ok(claim));

        let mut mutated = bytes;
        mutated[1] ^= 1;
        assert!(HistoryTypedV3RootClaim::decode_portable(&mutated).is_err());
        mutated[0] = 2;
        assert!(HistoryTypedV3RootClaim::decode_portable(&mutated).is_err());
    }
}
