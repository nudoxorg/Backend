//! Canonical persistent relations used by the derived-output catalog.
//!
//! The catalog deliberately uses the version crate's authenticated
//! `PersistentTree` rather than defining a second leaf/root format.  All
//! relations are immutable path-copy trees: the primary relation provides
//! exact semantic lookup, the freshness relation provides bounded global
//! eviction, and the payload relation provides exact reachability counts.

use super::super::owner::WorkspaceError;
use super::proof::CatalogRecord;
use super::storage::CatalogDescriptor;
use super::work::CatalogWork;
use super::{MAX_CATALOG_ENTRIES, SEMANTIC_KEY_PREFIX_BYTES};
use backend_version::{
    CanonicalRelation, CoverageWitness, MapChange, Relation, RelationDecodeError, RelationState,
    prepare_delta_with_state,
};
use std::fmt;

/// Fixed-width primary lookup key.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct PrimaryKey(pub(crate) [u8; super::PRIMARY_KEY_BYTES]);

/// Fixed-width freshness/eviction key. Generation is first so the smallest
/// key is always the oldest selected result.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct FreshnessKey(pub(crate) [u8; super::FRESHNESS_KEY_BYTES]);

/// Content-addressed output or dependency-manifest object retained by one or
/// more catalog rows.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct PayloadKey(pub(crate) [u8; 32]);

/// Number of live catalog rows referencing one payload. The catalog's hard
/// bound makes `u16` sufficient while keeping each relation leaf compact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PayloadRefs(pub(crate) u16);

/// Compact value retained in the primary relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogValue {
    pub(super) request: [u8; 32],
    pub(super) output: [u8; 32],
    pub(super) output_object: [u8; 32],
    pub(super) manifest_object: [u8; 32],
    pub(super) manifest_version: [u8; 32],
    pub(super) coverage_identity: [u8; 32],
    pub(super) scope: u64,
    pub(super) read_manifest: [u8; 32],
    pub(super) authority: [u8; 32],
    pub(super) authority_epoch: u64,
    pub(super) revocation_version: u64,
    pub(super) dependency_generation: u64,
    pub(super) authority_class: u8,
}

/// Freshness values point back to the exact primary row. Keeping this
/// inverse relation checked prevents an eviction index from deleting a
/// different semantic row after corruption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FreshnessValue(pub(crate) PrimaryKey);

/// Version relation schema for exact output identity.
pub(crate) struct PrimaryRelation;

impl fmt::Debug for PrimaryRelation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrimaryRelation")
    }
}

impl Relation for PrimaryRelation {
    const DOMAIN: u8 = super::CATALOG_DOMAIN;
    const TYPE: u16 = 10;
    const VERSION: u8 = super::CATALOG_VERSION;
    type Key = PrimaryKey;
    type Value = CatalogValue;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(&key.0);
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        value.encode(output);
    }
}

impl CanonicalRelation for PrimaryRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes
            .try_into()
            .map(PrimaryKey)
            .map_err(|_| RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        CatalogValue::decode(bytes).ok_or(RelationDecodeError::Malformed)
    }
}

/// Version relation schema for bounded freshness eviction.
pub(crate) struct FreshnessRelation;

impl fmt::Debug for FreshnessRelation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FreshnessRelation")
    }
}

impl Relation for FreshnessRelation {
    const DOMAIN: u8 = super::CATALOG_DOMAIN;
    const TYPE: u16 = 11;
    const VERSION: u8 = super::CATALOG_VERSION;
    type Key = FreshnessKey;
    type Value = FreshnessValue;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(&key.0);
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&(value.0).0);
    }
}

impl CanonicalRelation for FreshnessRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes
            .try_into()
            .map(FreshnessKey)
            .map_err(|_| RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        bytes
            .try_into()
            .map(|bytes| FreshnessValue(PrimaryKey(bytes)))
            .map_err(|_| RelationDecodeError::Malformed)
    }
}

/// Version relation schema for payload lifetime. This makes payload removal
/// an O(log n) delta coupled to row eviction and avoids a catalog scan.
pub(crate) struct PayloadRefRelation;

impl fmt::Debug for PayloadRefRelation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PayloadRefRelation")
    }
}

impl Relation for PayloadRefRelation {
    const DOMAIN: u8 = super::CATALOG_DOMAIN;
    const TYPE: u16 = 12;
    const VERSION: u8 = super::CATALOG_VERSION;
    type Key = PayloadKey;
    type Value = PayloadRefs;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(&key.0);
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.0.to_be_bytes());
    }
}

impl CanonicalRelation for PayloadRefRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes
            .try_into()
            .map(PayloadKey)
            .map_err(|_| RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        let count = u16::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| RelationDecodeError::Malformed)?,
        );
        (count > 0)
            .then_some(PayloadRefs(count))
            .ok_or(RelationDecodeError::Malformed)
    }
}

impl CatalogValue {
    const BYTES: usize = 32 * 8 + 8 * 4 + 1;

    fn encode(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.request);
        out.extend_from_slice(&self.output);
        out.extend_from_slice(&self.output_object);
        out.extend_from_slice(&self.manifest_object);
        out.extend_from_slice(&self.manifest_version);
        out.extend_from_slice(&self.coverage_identity);
        out.extend_from_slice(&self.scope.to_be_bytes());
        out.extend_from_slice(&self.read_manifest);
        out.extend_from_slice(&self.authority);
        out.extend_from_slice(&self.authority_epoch.to_be_bytes());
        out.extend_from_slice(&self.revocation_version.to_be_bytes());
        out.extend_from_slice(&self.dependency_generation.to_be_bytes());
        out.push(self.authority_class);
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::BYTES {
            return None;
        }
        let mut at = 0usize;
        Some(Self {
            request: take::<32>(bytes, &mut at)?,
            output: take::<32>(bytes, &mut at)?,
            output_object: take::<32>(bytes, &mut at)?,
            manifest_object: take::<32>(bytes, &mut at)?,
            manifest_version: take::<32>(bytes, &mut at)?,
            coverage_identity: take::<32>(bytes, &mut at)?,
            scope: u64::from_be_bytes(take::<8>(bytes, &mut at)?),
            read_manifest: take::<32>(bytes, &mut at)?,
            authority: take::<32>(bytes, &mut at)?,
            authority_epoch: u64::from_be_bytes(take::<8>(bytes, &mut at)?),
            revocation_version: u64::from_be_bytes(take::<8>(bytes, &mut at)?),
            dependency_generation: u64::from_be_bytes(take::<8>(bytes, &mut at)?),
            authority_class: bytes.get(at).copied()?,
        })
    }
}

fn take<const N: usize>(bytes: &[u8], at: &mut usize) -> Option<[u8; N]> {
    let end = at.checked_add(N)?;
    let value = bytes.get(*at..end)?.try_into().ok()?;
    *at = end;
    Some(value)
}

impl CatalogRecord {
    pub(super) fn primary_key(self) -> PrimaryKey {
        let mut bytes = [0u8; super::PRIMARY_KEY_BYTES];
        let mut at = 0usize;
        bytes[at..at + 32].copy_from_slice(&self.key);
        at += 32;
        bytes[at..at + 32].copy_from_slice(&self.manifest_version);
        at += 32;
        bytes[at..at + 32].copy_from_slice(&self.authority);
        at += 32;
        bytes[at..at + 8].copy_from_slice(&self.authority_epoch.to_be_bytes());
        at += 8;
        bytes[at..at + 8].copy_from_slice(&self.revocation_version.to_be_bytes());
        at += 8;
        bytes[at..at + 32].copy_from_slice(&self.coverage_identity);
        at += 32;
        bytes[at..at + 8].copy_from_slice(&self.scope.to_be_bytes());
        at += 8;
        bytes[at..at + 32].copy_from_slice(&self.read_manifest);
        at += 32;
        // Keep the semantic identity contiguous and invert the generation
        // suffix. A lower-bound lookup at the exact semantic prefix then
        // returns the newest generation without scanning its history.
        debug_assert_eq!(at, SEMANTIC_KEY_PREFIX_BYTES);
        let descending_generation = u64::MAX - self.dependency_generation;
        bytes[at..at + 8].copy_from_slice(&descending_generation.to_be_bytes());
        debug_assert_eq!(at + 8, bytes.len());
        PrimaryKey(bytes)
    }

    pub(super) fn freshness_key(self) -> FreshnessKey {
        let primary = self.primary_key();
        let mut bytes = [0u8; super::FRESHNESS_KEY_BYTES];
        bytes[..8].copy_from_slice(&self.dependency_generation.to_be_bytes());
        bytes[8..].copy_from_slice(&primary.0);
        FreshnessKey(bytes)
    }

    pub(super) fn value(self) -> CatalogValue {
        CatalogValue {
            request: self.request,
            output: self.output,
            output_object: self.output_object,
            manifest_object: self.manifest_object,
            manifest_version: self.manifest_version,
            coverage_identity: self.coverage_identity,
            scope: self.scope,
            read_manifest: self.read_manifest,
            authority: self.authority,
            authority_epoch: self.authority_epoch,
            revocation_version: self.revocation_version,
            dependency_generation: self.dependency_generation,
            authority_class: self.authority_class,
        }
    }

    pub(super) fn payload_keys(self) -> [PayloadKey; 2] {
        [
            PayloadKey(self.output_object),
            PayloadKey(self.manifest_object),
        ]
    }

    pub(crate) fn from_pair(key: PrimaryKey, value: &CatalogValue) -> Result<Self, WorkspaceError> {
        let work_key = key.0;
        let mut at = 0usize;
        let key_bytes =
            take::<32>(&work_key, &mut at).ok_or(WorkspaceError::Corrupt("catalog primary key"))?;
        let manifest_version =
            take::<32>(&work_key, &mut at).ok_or(WorkspaceError::Corrupt("catalog primary key"))?;
        let authority =
            take::<32>(&work_key, &mut at).ok_or(WorkspaceError::Corrupt("catalog primary key"))?;
        let authority_epoch = u64::from_be_bytes(
            take::<8>(&work_key, &mut at).ok_or(WorkspaceError::Corrupt("catalog primary key"))?,
        );
        let revocation_version = u64::from_be_bytes(
            take::<8>(&work_key, &mut at).ok_or(WorkspaceError::Corrupt("catalog primary key"))?,
        );
        let coverage_identity =
            take::<32>(&work_key, &mut at).ok_or(WorkspaceError::Corrupt("catalog primary key"))?;
        let scope = u64::from_be_bytes(
            take::<8>(&work_key, &mut at).ok_or(WorkspaceError::Corrupt("catalog primary key"))?,
        );
        let read_manifest =
            take::<32>(&work_key, &mut at).ok_or(WorkspaceError::Corrupt("catalog primary key"))?;
        let descending_generation = u64::from_be_bytes(
            take::<8>(&work_key, &mut at).ok_or(WorkspaceError::Corrupt("catalog primary key"))?,
        );
        let dependency_generation = u64::MAX - descending_generation;
        if dependency_generation == 0 || dependency_generation != value.dependency_generation {
            return Err(WorkspaceError::Corrupt("catalog generation binding"));
        }
        if at != work_key.len() {
            return Err(WorkspaceError::Corrupt("catalog primary key"));
        }
        let record = Self {
            key: key_bytes,
            request: value.request,
            output: value.output,
            output_object: value.output_object,
            manifest_object: value.manifest_object,
            manifest_version,
            coverage_identity,
            scope,
            read_manifest,
            authority,
            authority_epoch,
            revocation_version,
            dependency_generation,
            authority_class: value.authority_class,
        };
        if record.primary_key() != key {
            return Err(WorkspaceError::Corrupt("catalog primary/value binding"));
        }
        Ok(record)
    }
}

/// Owner-retained catalog state. A selected catalog keeps only its checked
/// descriptor and coverage witness; relation rows remain in the shared lazy
/// node store for the whole lifetime of the owner.
#[derive(Clone, Debug)]
pub(crate) enum CatalogState {
    Empty {
        coverage: CoverageWitness,
    },
    Persisted {
        descriptor: CatalogDescriptor,
        coverage: CoverageWitness,
    },
}

pub(super) struct InitialCatalog {
    pub(super) primary: RelationState<PrimaryRelation>,
    pub(super) freshness: RelationState<FreshnessRelation>,
    pub(super) payload_refs: RelationState<PayloadRefRelation>,
    pub(super) count: usize,
}

impl CatalogState {
    pub(crate) fn empty(coverage: CoverageWitness) -> Self {
        Self::Empty { coverage }
    }

    pub(super) const fn coverage(&self) -> CoverageWitness {
        match self {
            Self::Empty { coverage } | Self::Persisted { coverage, .. } => *coverage,
        }
    }

    pub(super) fn persisted(
        descriptor: CatalogDescriptor,
        coverage: CoverageWitness,
    ) -> Result<Self, WorkspaceError> {
        if descriptor.count > MAX_CATALOG_ENTRIES {
            return Err(WorkspaceError::Bounds);
        }
        Ok(Self::Persisted {
            descriptor,
            coverage,
        })
    }

    pub(super) fn descriptor(&self) -> Option<CatalogDescriptor> {
        match self {
            Self::Persisted { descriptor, .. } => Some(*descriptor),
            Self::Empty { .. } => None,
        }
    }

    pub(super) fn persisted_parts(&self) -> Option<(CatalogDescriptor, CoverageWitness)> {
        match self {
            Self::Persisted {
                descriptor,
                coverage,
            } => Some((*descriptor, *coverage)),
            Self::Empty { .. } => None,
        }
    }

    pub(crate) fn descriptor_object_id(
        &self,
    ) -> Result<Option<backend_store::ObjectId>, WorkspaceError> {
        self.descriptor()
            .map(CatalogDescriptor::object_id)
            .transpose()
    }

    pub(super) fn initial_insert(
        &self,
        candidate: &CatalogRecord,
        work: &mut CatalogWork,
    ) -> Result<InitialCatalog, WorkspaceError> {
        let coverage = match self {
            Self::Empty { coverage } => *coverage,
            Self::Persisted { .. } => {
                return Err(WorkspaceError::Corrupt(
                    "catalog state is already persisted",
                ));
            }
        };
        let primary = RelationState::<PrimaryRelation>::empty(coverage);
        let freshness = RelationState::<FreshnessRelation>::empty(coverage);
        let mut payload_refs = RelationState::<PayloadRefRelation>::empty(coverage);
        let primary_key = candidate.primary_key();
        let value = candidate.value();
        let primary_target = update_relation(
            &primary,
            MapChange {
                key: primary_key,
                before: None,
                after: Some(value),
            },
            work,
        )?;
        let freshness_target = update_relation(
            &freshness,
            MapChange {
                key: candidate.freshness_key(),
                before: None,
                after: Some(FreshnessValue(primary_key)),
            },
            work,
        )?;
        for payload_key in candidate.payload_keys() {
            let before = payload_refs.get(&payload_key).copied();
            let count = before
                .map_or(Some(1), |refs| refs.0.checked_add(1))
                .filter(|count| usize::from(*count) <= MAX_CATALOG_ENTRIES * 2)
                .ok_or(WorkspaceError::Bounds)?;
            let after = PayloadRefs(count);
            payload_refs = update_relation(
                &payload_refs,
                MapChange {
                    key: payload_key,
                    before,
                    after: Some(after),
                },
                work,
            )?;
        }
        Ok(InitialCatalog {
            primary: primary_target,
            freshness: freshness_target,
            payload_refs,
            count: 1,
        })
    }
}

fn update_relation<R: Relation>(
    base: &RelationState<R>,
    change: MapChange<R>,
    work: &mut CatalogWork,
) -> Result<RelationState<R>, WorkspaceError> {
    let (prepared, delta_work) = prepare_delta_with_state(base, vec![change])
        .map_err(|_| WorkspaceError::Corrupt("catalog persistent update"))?;
    work.add_delta(delta_work)?;
    prepared
        .commit(base)
        .map_err(|_| WorkspaceError::Corrupt("catalog persistent commit"))
}

#[cfg(test)]
mod tests {
    use super::{CatalogRecord, CatalogState, CatalogWork, PayloadKey, PayloadRefs};
    use backend_version::{ClosedRelationScope, CoverageWitness, ScopeRoot};

    fn record(generation: u64) -> CatalogRecord {
        CatalogRecord {
            key: [1; 32],
            request: [2; 32],
            output: [3; 32],
            output_object: [4; 32],
            manifest_object: [5; 32],
            manifest_version: [6; 32],
            coverage_identity: [7; 32],
            scope: 8,
            read_manifest: [9; 32],
            authority: [10; 32],
            authority_epoch: 11,
            revocation_version: 12,
            dependency_generation: generation,
            authority_class: 13,
        }
    }

    #[test]
    fn newest_generation_is_first_for_one_semantic_prefix() {
        let older = record(1);
        let newer = record(2);
        assert!(newer.primary_key() < older.primary_key());

        let value = newer.value();
        let decoded = CatalogRecord::from_pair(newer.primary_key(), &value)
            .unwrap_or_else(|error| panic!("descending key must round-trip: {error:?}"));
        assert_eq!(decoded, newer);
    }

    #[test]
    fn initial_catalog_counts_shared_payload_identity_once_per_reference() {
        let mut candidate = record(1);
        candidate.manifest_object = candidate.output_object;
        let coverage = CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(
            ScopeRoot::from_u64(1),
        ));
        let state = CatalogState::empty(coverage);
        let mut work = CatalogWork::default();
        let initial = state
            .initial_insert(&candidate, &mut work)
            .unwrap_or_else(|error| panic!("initial catalog: {error:?}"));

        assert_eq!(initial.count, 1);
        assert_eq!(
            initial
                .payload_refs
                .get(&PayloadKey(candidate.output_object)),
            Some(&PayloadRefs(2))
        );
        assert_eq!(initial.payload_refs.iter().count(), 1);
    }
}
