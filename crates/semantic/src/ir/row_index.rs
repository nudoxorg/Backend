//! Persistent stable-key index for normalized semantic rows.
//!
//! The index reuses `backend-version`'s canonical immutable relation tree. A
//! row is keyed by one of the seven closed semantic family codes and a
//! 32-byte stable key. Its value is only a verified content identity and
//! length; payload bytes remain in immutable payload objects. The tree owns
//! rows in leaf slabs and copies affected paths for edits, so there is no
//! `Arc` per row and no process-wide mutable interner.

use std::{fmt, mem::size_of, ops::Bound, sync::Arc};

use backend_version::{
    ObjectVersion, ObjectVersionHasher, PersistentTree, Relation, RuntimeIdentityError, Schema,
    SchemaIdentity, StateRoot, TreeChange, TreeError, TreeNodeChildren, TreeNodeHandle,
    TreeNodeView, TreeRangeIter, TreeWork,
};
use thiserror::Error;

/// One of the seven normalized semantic row families, in canonical order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum RowFamily {
    /// Canonical declaration and entity facts.
    Core = 1,
    /// Reachable type facts.
    Types = 2,
    /// Stable graph relations.
    Relations = 3,
    /// Occurrence-site facts.
    Occurrences = 4,
    /// Documentation facts.
    Documentation = 5,
    /// Source identity and span facts.
    SourceProvenance = 6,
    /// Facts selected by a closed language profile.
    LanguageExtensions = 7,
}

impl RowFamily {
    /// All families in their commitment and census order.
    pub const ALL: [Self; 7] = [
        Self::Core,
        Self::Types,
        Self::Relations,
        Self::Occurrences,
        Self::Documentation,
        Self::SourceProvenance,
        Self::LanguageExtensions,
    ];

    /// Returns the stable one-byte family code.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decodes a family code from an untrusted boundary.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Core),
            2 => Some(Self::Types),
            3 => Some(Self::Relations),
            4 => Some(Self::Occurrences),
            5 => Some(Self::Documentation),
            6 => Some(Self::SourceProvenance),
            7 => Some(Self::LanguageExtensions),
            _ => None,
        }
    }

    const fn index(self) -> usize {
        self.code() as usize - 1
    }
}

/// Ordered key for one semantic row.
///
/// The fixed-width encoding is byte-order preserving: family code first,
/// followed by the producer's stable row key. Family order therefore remains
/// the same in the Rust tree and in the canonical byte grammar.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StableRowKey {
    family: RowFamily,
    key: [u8; 32],
}

impl StableRowKey {
    /// Creates a key in one admitted semantic family.
    #[must_use]
    pub const fn new(family: RowFamily, key: [u8; 32]) -> Self {
        Self { family, key }
    }

    /// Returns this row's family.
    #[must_use]
    pub const fn family(self) -> RowFamily {
        self.family
    }

    /// Returns the stable 32-byte row key.
    #[must_use]
    pub const fn key(self) -> [u8; 32] {
        self.key
    }

    const fn family_floor(family: RowFamily) -> Self {
        Self::new(family, [0; 32])
    }
}

/// Content identity of a semantic row payload.
///
/// The raw and tagged preimages have separate schema domains. This prevents
/// `[tag, payload]` as an untagged value from aliasing a tagged row, and keeps
/// the tag available when a persisted identity is read back and rechecked.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowPayloadId(RowPayloadIdentity);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum RowPayloadIdentity {
    Raw(ObjectVersion<SemanticRowPayloadSchema>),
    Tagged {
        tag: u8,
        version: ObjectVersion<SemanticTaggedRowPayloadSchema>,
    },
}

impl RowPayloadId {
    /// Returns the canonical payload digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        match &self.0 {
            RowPayloadIdentity::Raw(version) => version.as_bytes(),
            RowPayloadIdentity::Tagged { version, .. } => version.as_bytes(),
        }
    }
}

/// Version identity schema for exact normalized-row payload bytes.
struct SemanticRowPayloadSchema;

impl Schema for SemanticRowPayloadSchema {
    const DOMAIN: u8 = 0x53;
    const TYPE: u16 = 0xf202;
    const VERSION: u8 = 1;

    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Distinct domain for the canonical tag followed by row payload bytes.
struct SemanticTaggedRowPayloadSchema;

impl Schema for SemanticTaggedRowPayloadSchema {
    const DOMAIN: u8 = 0x53;
    const TYPE: u16 = 0xf203;
    const VERSION: u8 = 1;

    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Verified identity and exact byte length for one immutable payload.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RowPayload {
    id: RowPayloadId,
    byte_len: u64,
}

impl RowPayload {
    /// Hashes and records one exact canonical row payload.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StableRowIndexError> {
        let byte_len = u64::try_from(bytes.len()).map_err(|_| StableRowIndexError::Overflow)?;
        Ok(Self {
            id: payload_id(bytes)?,
            byte_len,
        })
    }

    /// Hashes a canonical row tag and payload as one value without building a
    /// temporary tagged buffer. The stored byte length remains the exact
    /// payload length; the content identity also changes when the tag changes.
    pub fn from_tagged_bytes(tag: u8, bytes: &[u8]) -> Result<Self, StableRowIndexError> {
        let byte_len = u64::try_from(bytes.len()).map_err(|_| StableRowIndexError::Overflow)?;
        let hashed_len = bytes
            .len()
            .checked_add(1)
            .ok_or(StableRowIndexError::Overflow)?;
        let schema = SchemaIdentity::new(
            SemanticTaggedRowPayloadSchema::DOMAIN,
            SemanticTaggedRowPayloadSchema::TYPE,
            SemanticTaggedRowPayloadSchema::VERSION,
        );
        let mut hasher = ObjectVersionHasher::new(schema, hashed_len)?;
        hasher.update(&[tag])?;
        hasher.update(bytes)?;
        Ok(Self {
            id: RowPayloadId(RowPayloadIdentity::Tagged {
                tag,
                version: hasher.finish_version::<SemanticTaggedRowPayloadSchema>()?,
            }),
            byte_len,
        })
    }

    /// Returns the content identity.
    #[must_use]
    pub const fn id(self) -> RowPayloadId {
        self.id
    }

    /// Returns the payload's exact byte length.
    #[must_use]
    pub const fn byte_len(self) -> u64 {
        self.byte_len
    }

    /// Returns an untrusted claim suitable for a wire or storage boundary.
    #[must_use]
    pub const fn claim(self) -> UntrustedRowPayloadIdentity {
        UntrustedRowPayloadIdentity {
            id: *self.id.as_bytes(),
            byte_len: self.byte_len,
            encoding: match self.id.0 {
                RowPayloadIdentity::Raw(_) => RowPayloadEncoding::Raw,
                RowPayloadIdentity::Tagged { tag, .. } => RowPayloadEncoding::Tagged(tag),
            },
        }
    }

    /// Rechecks this identity against bytes read from immutable storage.
    pub fn verify_bytes(self, bytes: &[u8]) -> Result<(), StableRowIndexError> {
        self.claim().admit(bytes).map(|_| ())
    }
}

/// Untrusted payload identity read from an index or object descriptor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct UntrustedRowPayloadIdentity {
    id: [u8; 32],
    byte_len: u64,
    encoding: RowPayloadEncoding,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum RowPayloadEncoding {
    Raw,
    Tagged(u8),
}

impl UntrustedRowPayloadIdentity {
    /// Reads an identity claim from a fixed-width wire value.
    #[must_use]
    pub const fn from_raw(id: [u8; 32], byte_len: u64) -> Self {
        Self {
            id,
            byte_len,
            encoding: RowPayloadEncoding::Raw,
        }
    }

    /// Reads an untrusted tagged payload claim. The tag is part of the exact
    /// hashed preimage and is rechecked before the claim is admitted.
    #[must_use]
    pub const fn from_tagged_raw(tag: u8, id: [u8; 32], byte_len: u64) -> Self {
        Self {
            id,
            byte_len,
            encoding: RowPayloadEncoding::Tagged(tag),
        }
    }

    /// Returns the untrusted digest bytes.
    #[must_use]
    pub const fn id_bytes(self) -> [u8; 32] {
        self.id
    }

    /// Returns the claimed payload length.
    #[must_use]
    pub const fn byte_len(self) -> u64 {
        self.byte_len
    }

    /// Encodes this untrusted payload claim in the fixed V3 row-index value
    /// grammar. The encoding byte and tag stay explicit so cold tree admission
    /// cannot turn a raw digest into a tagged identity or vice versa.
    #[must_use]
    pub fn to_fixed_wire(self) -> [u8; 42] {
        let mut output = [0; 42];
        match self.encoding {
            RowPayloadEncoding::Raw => {
                output[0] = 0;
                output[1] = 0;
            }
            RowPayloadEncoding::Tagged(tag) => {
                output[0] = 1;
                output[1] = tag;
            }
        }
        output[2..34].copy_from_slice(&self.id);
        output[34..].copy_from_slice(&self.byte_len.to_be_bytes());
        output
    }

    /// Reads one fixed-width V3 payload claim without admitting its digest.
    /// The exact row bytes must still be loaded and checked with [`Self::admit`].
    pub fn from_fixed_wire(bytes: &[u8]) -> Result<Self, StableRowIndexError> {
        let bytes: &[u8; 42] = bytes
            .try_into()
            .map_err(|_| StableRowIndexError::MalformedPayloadClaim)?;
        let encoding = match bytes[0] {
            0 if bytes[1] == 0 => RowPayloadEncoding::Raw,
            1 => RowPayloadEncoding::Tagged(bytes[1]),
            _ => return Err(StableRowIndexError::MalformedPayloadClaim),
        };
        let id = bytes[2..34]
            .try_into()
            .map_err(|_| StableRowIndexError::MalformedPayloadClaim)?;
        let byte_len = u64::from_be_bytes(
            bytes[34..]
                .try_into()
                .map_err(|_| StableRowIndexError::MalformedPayloadClaim)?,
        );
        Ok(Self {
            id,
            byte_len,
            encoding,
        })
    }

    /// Returns the tag carried by a tagged row identity, if present.
    #[must_use]
    pub const fn tag(self) -> Option<u8> {
        match self.encoding {
            RowPayloadEncoding::Raw => None,
            RowPayloadEncoding::Tagged(tag) => Some(tag),
        }
    }

    /// Admits the claim only when both its exact length and digest match.
    pub fn admit(self, bytes: &[u8]) -> Result<RowPayload, StableRowIndexError> {
        let actual_len = u64::try_from(bytes.len()).map_err(|_| StableRowIndexError::Overflow)?;
        if actual_len != self.byte_len {
            return Err(StableRowIndexError::PayloadLengthMismatch);
        }
        let actual = match self.encoding {
            RowPayloadEncoding::Raw => RowPayload::from_bytes(bytes)?,
            RowPayloadEncoding::Tagged(tag) => RowPayload::from_tagged_bytes(tag, bytes)?,
        };
        if actual.id().as_bytes() != &self.id {
            return Err(StableRowIndexError::PayloadDigestMismatch);
        }
        Ok(actual)
    }
}

fn payload_id(bytes: &[u8]) -> Result<RowPayloadId, StableRowIndexError> {
    let schema = SchemaIdentity::new(
        SemanticRowPayloadSchema::DOMAIN,
        SemanticRowPayloadSchema::TYPE,
        SemanticRowPayloadSchema::VERSION,
    );
    let mut hasher = ObjectVersionHasher::new(schema, bytes.len())?;
    hasher.update(bytes)?;
    Ok(RowPayloadId(RowPayloadIdentity::Raw(
        hasher.finish_version::<SemanticRowPayloadSchema>()?,
    )))
}

#[derive(Debug, Eq, PartialEq)]
struct SemanticRowIndexRelation;

impl Relation for SemanticRowIndexRelation {
    const DOMAIN: u8 = 0x53;
    const TYPE: u16 = 0xf201;
    const VERSION: u8 = 2;

    type Key = StableRowKey;
    type Value = RowPayload;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        out.push(key.family.code());
        out.extend_from_slice(&key.key);
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        match value.id.0 {
            RowPayloadIdentity::Raw(_) => out.push(0),
            RowPayloadIdentity::Tagged { tag, .. } => {
                out.push(1);
                out.push(tag);
            }
        }
        out.extend_from_slice(value.id.as_bytes());
        out.extend_from_slice(&value.byte_len.to_be_bytes());
    }
}

type RowTree = PersistentTree<SemanticRowIndexRelation>;
type RowTreeIter<'a> = TreeRangeIter<'a, SemanticRowIndexRelation>;
type RowNodeView<'a> = TreeNodeView<'a, SemanticRowIndexRelation>;

/// Admitted stable root of one complete semantic row index.
///
/// This root is minted only by the checked local builder or by record
/// admission, which reconstructs the complete sorted tree and compares its
/// commitment to an untrusted claim.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StableRowIndexRoot {
    root: StateRoot<SemanticRowIndexRelation>,
    row_count: u64,
}

impl StableRowIndexRoot {
    /// Returns the admitted typed root commitment bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.root.to_bytes()
    }

    /// Returns the authenticated exact row count.
    #[must_use]
    pub fn row_count(self) -> u64 {
        self.row_count
    }
}

/// An untrusted stable row-index root claim read from a manifest or peer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct UntrustedStableRowIndexRoot([u8; 32]);

impl UntrustedStableRowIndexRoot {
    /// Reads a raw root claim without admitting it.
    #[must_use]
    pub const fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the raw claimed bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Rebuilds a complete index from verified, strictly ordered records and
    /// admits the claim only when the reconstructed root matches exactly.
    pub fn admit_records(
        self,
        records: &[(StableRowKey, RowPayload)],
    ) -> Result<StableRowIndex, StableRowIndexError> {
        let index = StableRowIndex::from_sorted_rows(records)?;
        if index.root().as_bytes() != self.0 {
            return Err(StableRowIndexError::RootMismatch);
        }
        Ok(index)
    }
}

/// Immutable ordered index over stable semantic row identities.
#[derive(Debug)]
pub struct StableRowIndex {
    tree: RowTree,
}

impl Clone for StableRowIndex {
    fn clone(&self) -> Self {
        Self {
            tree: self.tree.clone(),
        }
    }
}

impl StableRowIndex {
    /// Builds a canonical index from strictly ordered rows.
    pub fn from_sorted_rows(
        rows: &[(StableRowKey, RowPayload)],
    ) -> Result<Self, StableRowIndexError> {
        let tree = RowTree::from_sorted_items(rows)?;
        Ok(Self { tree })
    }

    /// Builds a canonical index and returns tree work and resident-memory
    /// accounting for this generation.
    pub fn from_sorted_rows_measured(
        rows: &[(StableRowKey, RowPayload)],
    ) -> Result<(Self, StableRowIndexBuildWork), StableRowIndexError> {
        let (tree, tree_work) = RowTree::from_sorted_items_with_work(rows)?;
        let index = Self { tree };
        let resident = index.memory_usage()?;
        Ok((
            index,
            StableRowIndexBuildWork {
                tree: tree_work,
                resident,
            },
        ))
    }

    /// Returns an owner-specific sorted-row builder with no shared mutable
    /// state or global lock.
    #[must_use]
    pub fn builder() -> StableRowIndexBuilder {
        StableRowIndexBuilder::default()
    }

    /// Returns this generation's admitted root identity.
    #[must_use]
    pub fn root(&self) -> StableRowIndexRoot {
        StableRowIndexRoot {
            root: self.tree.root().commitment(),
            row_count: self.tree.row_count(),
        }
    }

    /// Returns the authenticated aggregate row count.
    #[must_use]
    pub fn row_count(&self) -> u64 {
        self.tree.row_count()
    }

    /// Looks up one exact family/key pair.
    #[must_use]
    pub fn get(&self, key: &StableRowKey) -> Option<RowPayload> {
        self.tree.get(key).copied()
    }

    /// Creates a borrowed cursor over the half-open ordered range `[start,end)`.
    ///
    /// The cursor retains the admitted root and exact range cardinality. Each
    /// bounded edge seeks by the existing tree's subtree row counts, then
    /// lends rows from immutable leaf slabs.
    pub fn range(
        &self,
        range: StableRowRange,
    ) -> Result<StableRowRangeCursor<'_>, StableRowIndexError> {
        validate_range(range)?;
        let inner = self.tree.range(range.bounds());
        let remaining = inner.len();
        Ok(StableRowRangeCursor {
            root: self.root(),
            range,
            inner,
            remaining,
        })
    }

    /// Computes an exact census by seeking the seven disjoint family ranges.
    /// It does not scan row payloads or trust a cached family count.
    pub fn census(&self) -> Result<StableRowIndexCensus, StableRowIndexError> {
        let mut counts = [0_u64; 7];
        for family in RowFamily::ALL {
            counts[family.index()] = u64::try_from(self.family_range_len(family)?)
                .map_err(|_| StableRowIndexError::Overflow)?;
        }
        let total = counts
            .iter()
            .try_fold(0_u64, |sum, count| sum.checked_add(*count))
            .ok_or(StableRowIndexError::Overflow)?;
        if total != self.row_count() {
            return Err(StableRowIndexError::CensusMismatch);
        }
        Ok(StableRowIndexCensus {
            root: self.root(),
            counts,
        })
    }

    /// Creates a serializable claim for the exact family census.
    pub fn census_proof(&self) -> Result<UntrustedStableRowIndexCensus, StableRowIndexError> {
        let census = self.census()?;
        Ok(UntrustedStableRowIndexCensus {
            root: UntrustedStableRowIndexRoot(census.root.as_bytes()),
            counts: census.counts,
        })
    }

    /// Checks a census claim against this complete admitted root. Family
    /// counts are recomputed from authenticated key ranges, so omitted or
    /// reassigned rows fail even when the aggregate count happens to match.
    pub fn admit_census(
        &self,
        proof: UntrustedStableRowIndexCensus,
    ) -> Result<StableRowIndexCensus, StableRowIndexError> {
        if proof.root.0 != self.root().as_bytes() {
            return Err(StableRowIndexError::RootMismatch);
        }
        let expected = self.census()?;
        if proof.counts != expected.counts {
            return Err(StableRowIndexError::CensusMismatch);
        }
        Ok(expected)
    }

    /// Materializes a root-bound range claim from this admitted index.
    ///
    /// A proof is checked by [`Self::admit_range_proof`] against the complete
    /// owner-held tree. It cannot be admitted from a digest alone; portable
    /// stateless multiproofs are a separate wire protocol.
    pub fn range_proof(
        &self,
        range: StableRowRange,
    ) -> Result<StableRowRangeProof, StableRowIndexError> {
        let cursor = self.range(range)?;
        let mut entries = Vec::new();
        entries
            .try_reserve(cursor.len())
            .map_err(|_| StableRowIndexError::Allocation)?;
        entries.extend(cursor.map(|entry| StableRowRangeProofEntry {
            key: *entry.key,
            payload: entry.payload.claim(),
        }));
        Ok(StableRowRangeProof {
            root: UntrustedStableRowIndexRoot(self.root().as_bytes()),
            range,
            entries: entries.into_boxed_slice(),
        })
    }

    /// Admits an exact range claim against this complete admitted tree and
    /// returns the borrowed cursor that owns the authenticated rows.
    pub fn admit_range_proof(
        &self,
        proof: &StableRowRangeProof,
    ) -> Result<StableRowRangeCursor<'_>, StableRowIndexError> {
        if proof.root.0 != self.root().as_bytes() {
            return Err(StableRowIndexError::RootMismatch);
        }
        validate_range(proof.range)?;
        if proof
            .entries
            .windows(2)
            .any(|window| window[0].key >= window[1].key)
        {
            return Err(StableRowIndexError::UnsortedOrDuplicate);
        }
        if proof
            .entries
            .iter()
            .any(|entry| !proof.range.contains(entry.key))
        {
            return Err(StableRowIndexError::RangeProofMismatch);
        }

        let mut actual = self.tree.range(proof.range.bounds());
        for claimed in proof.entries.iter() {
            let Some((key, payload)) = actual.next() else {
                return Err(StableRowIndexError::RangeProofMismatch);
            };
            if key != &claimed.key || payload.claim() != claimed.payload {
                return Err(StableRowIndexError::RangeProofMismatch);
            }
        }
        if actual.next().is_some() {
            return Err(StableRowIndexError::RangeProofMismatch);
        }
        self.range(proof.range)
    }

    /// Accounts exact encoded node and row bytes plus an explicit estimate of
    /// in-memory tree metadata. Payload bytes are referenced, not retained.
    pub fn memory_usage(&self) -> Result<StableRowIndexMemoryUsage, StableRowIndexError> {
        let mut usage = StableRowIndexMemoryUsage::default();
        for node in self.tree.node_closure() {
            usage.nodes = usage
                .nodes
                .checked_add(1)
                .ok_or(StableRowIndexError::Overflow)?;
            usage.canonical_node_bytes = usage
                .canonical_node_bytes
                .checked_add(
                    u64::try_from(node.canonical_bytes().len())
                        .map_err(|_| StableRowIndexError::Overflow)?,
                )
                .ok_or(StableRowIndexError::Overflow)?;
            usage.tree_node_header_estimate_bytes = usage
                .tree_node_header_estimate_bytes
                .checked_add(usize_to_u64(size_of::<NodeHeaderEstimate>())?)
                .ok_or(StableRowIndexError::Overflow)?;
            usage.canonical_node_metadata_estimate_bytes = usage
                .canonical_node_metadata_estimate_bytes
                .checked_add(usize_to_u64(size_of::<
                    backend_version::CanonicalNode<SemanticRowIndexRelation>,
                >())?)
                .ok_or(StableRowIndexError::Overflow)?;
            usage.level_count_storage_estimate_bytes = usage
                .level_count_storage_estimate_bytes
                .checked_add(usize_to_u64(
                    usize::from(node.level())
                        .checked_add(1)
                        .and_then(|count| count.checked_mul(size_of::<usize>()))
                        .ok_or(StableRowIndexError::Overflow)?,
                )?)
                .ok_or(StableRowIndexError::Overflow)?;

            if let Some(entries) = node.entries() {
                usage.rows = usage
                    .rows
                    .checked_add(
                        u64::try_from(entries.len()).map_err(|_| StableRowIndexError::Overflow)?,
                    )
                    .ok_or(StableRowIndexError::Overflow)?;
                usage.row_slot_storage_bytes = usage
                    .row_slot_storage_bytes
                    .checked_add(usize_to_u64(
                        entries
                            .len()
                            .checked_mul(size_of::<(StableRowKey, RowPayload)>())
                            .ok_or(StableRowIndexError::Overflow)?,
                    )?)
                    .ok_or(StableRowIndexError::Overflow)?;
                for (_, payload) in entries {
                    usage.referenced_payload_bytes = usage
                        .referenced_payload_bytes
                        .checked_add(payload.byte_len)
                        .ok_or(StableRowIndexError::Overflow)?;
                }
            }

            let children = node.child_count();
            usage.child_handle_storage_bytes = usage
                .child_handle_storage_bytes
                .checked_add(usize_to_u64(
                    children
                        .checked_mul(size_of::<TreeNodeHandle<SemanticRowIndexRelation>>())
                        .ok_or(StableRowIndexError::Overflow)?,
                )?)
                .ok_or(StableRowIndexError::Overflow)?;
            if children != 0 {
                usage.child_offset_storage_bytes = usage
                    .child_offset_storage_bytes
                    .checked_add(usize_to_u64(
                        children
                            .checked_add(1)
                            .and_then(|count| count.checked_mul(size_of::<usize>()))
                            .ok_or(StableRowIndexError::Overflow)?,
                    )?)
                    .ok_or(StableRowIndexError::Overflow)?;
            }
        }
        usage.estimated_resident_bytes = usage
            .canonical_node_bytes
            .checked_add(usage.tree_node_header_estimate_bytes)
            .and_then(|sum| sum.checked_add(usage.canonical_node_metadata_estimate_bytes))
            .and_then(|sum| sum.checked_add(usage.row_slot_storage_bytes))
            .and_then(|sum| sum.checked_add(usage.child_handle_storage_bytes))
            .and_then(|sum| sum.checked_add(usage.child_offset_storage_bytes))
            .and_then(|sum| sum.checked_add(usage.level_count_storage_estimate_bytes))
            .ok_or(StableRowIndexError::Overflow)?;
        Ok(usage)
    }

    fn family_range_len(&self, family: RowFamily) -> Result<usize, StableRowIndexError> {
        let start = StableRowKey::family_floor(family);
        let end = RowFamily::from_code(family.code() + 1)
            .map(StableRowKey::family_floor)
            .map_or(Bound::Unbounded, Bound::Excluded);
        let rows: RowTreeIter<'_> = self.tree.range((Bound::Included(start), end));
        Ok(rows.len())
    }
}

/// One ordered semantic row change between two admitted roots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StableRowIndexDiffEntry<'before, 'after> {
    /// A key is present only in the after root.
    Insert {
        /// Inserted stable key.
        key: StableRowKey,
        /// Borrowed payload identity in the after root.
        after: &'after RowPayload,
    },
    /// A key remains present, but its payload identity changed.
    Replace {
        /// Replaced stable key.
        key: StableRowKey,
        /// Borrowed prior payload identity.
        before: &'before RowPayload,
        /// Borrowed replacement payload identity.
        after: &'after RowPayload,
    },
    /// A key is present only in the before root.
    Delete {
        /// Deleted stable key.
        key: StableRowKey,
        /// Borrowed payload identity in the before root.
        before: &'before RowPayload,
    },
}

/// Work counters for a borrowed two-root row diff.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StableRowIndexDiffWork {
    /// Individual node views inspected by pair comparison or frontier expansion.
    pub visited_nodes: u64,
    /// Equal subtree commitments skipped without visiting descendant rows.
    pub skipped_equal_subtrees: u64,
    /// Row records whose keys and values were compared in divergent leaf runs.
    pub row_records_examined: u64,
    /// Payload bodies decoded while diffing. This is always zero because the
    /// diff borrows already typed in-memory index values.
    pub decoded_rows: u64,
    /// Largest combined child-iterator stack capacity used by a divergent
    /// run merge. Row-reference buffers are never materialized.
    pub peak_run_cursor_scratch_bytes: u64,
}

/// One exact equal subtree found while comparing two live admitted indexes.
///
/// This borrow-scoped structural certificate is useful to a caller that can
/// retain the same row trees across generations: the subtree commitment,
/// key envelope, and census are equal in both borrowed roots. It does not
/// prove that either row index completely represents a semantic reader, nor
/// is it a portable proof that can be detached from these indexes.
#[derive(Clone, Copy)]
pub struct StableRowIndexUnchangedSubtree<'before, 'after> {
    before: &'before StableRowIndex,
    after: &'after StableRowIndex,
    before_root: StableRowIndexRoot,
    after_root: StableRowIndexRoot,
    first_key: Option<StableRowKey>,
    last_key: Option<StableRowKey>,
    row_count: u64,
    subtree_commitment: [u8; 32],
}

impl StableRowIndexUnchangedSubtree<'_, '_> {
    /// Returns the exact prior index root that scopes this certificate.
    #[must_use]
    pub const fn before_root(self) -> StableRowIndexRoot {
        self.before_root
    }

    /// Returns the exact target index root that scopes this certificate.
    #[must_use]
    pub const fn after_root(self) -> StableRowIndexRoot {
        self.after_root
    }

    /// Returns the first key in the equal subtree.
    #[must_use]
    pub const fn first_key(self) -> Option<StableRowKey> {
        self.first_key
    }

    /// Returns the last key in the equal subtree.
    #[must_use]
    pub const fn last_key(self) -> Option<StableRowKey> {
        self.last_key
    }

    /// Returns the exact number of rows in the equal subtree.
    #[must_use]
    pub const fn row_count(self) -> u64 {
        self.row_count
    }

    /// Returns the equal subtree's authenticated commitment.
    #[must_use]
    pub const fn subtree_commitment(self) -> [u8; 32] {
        self.subtree_commitment
    }

    /// Returns whether this certificate is scoped to these exact borrowed
    /// index instances, including instances with equal content roots.
    #[must_use]
    pub fn is_scoped_to(self, before: &StableRowIndex, after: &StableRowIndex) -> bool {
        core::ptr::eq(self.before, before) && core::ptr::eq(self.after, after)
    }
}

impl fmt::Debug for StableRowIndexUnchangedSubtree<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StableRowIndexUnchangedSubtree")
            .field("before_root", &self.before_root)
            .field("after_root", &self.after_root)
            .field("first_key", &self.first_key)
            .field("last_key", &self.last_key)
            .field("row_count", &self.row_count)
            .field("subtree_commitment", &self.subtree_commitment)
            .finish()
    }
}

impl PartialEq for StableRowIndexUnchangedSubtree<'_, '_> {
    fn eq(&self, other: &Self) -> bool {
        self.before_root == other.before_root
            && self.after_root == other.after_root
            && self.first_key == other.first_key
            && self.last_key == other.last_key
            && self.row_count == other.row_count
            && self.subtree_commitment == other.subtree_commitment
    }
}

impl Eq for StableRowIndexUnchangedSubtree<'_, '_> {}

/// Ordered diff and exact skipped-subtree facts borrowing two immutable roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableRowIndexDiff<'before, 'after> {
    before_root: StableRowIndexRoot,
    after_root: StableRowIndexRoot,
    entries: Box<[StableRowIndexDiffEntry<'before, 'after>]>,
    unchanged_subtrees: Box<[StableRowIndexUnchangedSubtree<'before, 'after>]>,
    work: StableRowIndexDiffWork,
}

impl<'before, 'after> StableRowIndexDiff<'before, 'after> {
    /// Admitted root that supplies prior payload references.
    #[must_use]
    pub const fn before_root(&self) -> StableRowIndexRoot {
        self.before_root
    }

    /// Admitted root that supplies target payload references.
    #[must_use]
    pub const fn after_root(&self) -> StableRowIndexRoot {
        self.after_root
    }

    /// Ordered insert, replace, and delete records.
    #[must_use]
    pub fn entries(&self) -> &[StableRowIndexDiffEntry<'before, 'after>] {
        &self.entries
    }

    /// Exact equal subtrees skipped while producing this pair's row diff.
    ///
    /// Each item borrows both complete indexes, so it cannot be used after
    /// either owner is dropped or mistaken for a semantic-reader completeness
    /// proof. Subtrees whose boundaries diverged may instead be compared row
    /// by row and therefore have no compact certificate here.
    #[must_use]
    pub fn unchanged_subtrees(&self) -> &[StableRowIndexUnchangedSubtree<'before, 'after>] {
        &self.unchanged_subtrees
    }

    /// Structural traversal and row-comparison counters.
    #[must_use]
    pub const fn work(&self) -> StableRowIndexDiffWork {
        self.work
    }
}

#[derive(Default)]
struct StableRowIndexDiffCounter {
    work: StableRowIndexDiffWork,
}

impl StableRowIndexDiffCounter {
    fn visited_node(&mut self) -> Result<(), StableRowIndexError> {
        self.work.visited_nodes = self
            .work
            .visited_nodes
            .checked_add(1)
            .ok_or(StableRowIndexError::Overflow)?;
        Ok(())
    }

    fn skipped_subtree(&mut self) -> Result<(), StableRowIndexError> {
        self.work.skipped_equal_subtrees = self
            .work
            .skipped_equal_subtrees
            .checked_add(1)
            .ok_or(StableRowIndexError::Overflow)?;
        Ok(())
    }

    fn examined_rows(&mut self, count: usize) -> Result<(), StableRowIndexError> {
        self.work.row_records_examined = self
            .work
            .row_records_examined
            .checked_add(usize_to_u64(count)?)
            .ok_or(StableRowIndexError::Overflow)?;
        Ok(())
    }

    fn traversed_nodes(&mut self, count: u64) -> Result<(), StableRowIndexError> {
        self.work.visited_nodes = self
            .work
            .visited_nodes
            .checked_add(count)
            .ok_or(StableRowIndexError::Overflow)?;
        Ok(())
    }
}

fn diff_node<'before, 'after>(
    before: RowNodeView<'before>,
    after: RowNodeView<'after>,
    before_index: &'before StableRowIndex,
    after_index: &'after StableRowIndex,
    entries: &mut Vec<StableRowIndexDiffEntry<'before, 'after>>,
    unchanged_subtrees: &mut Vec<StableRowIndexUnchangedSubtree<'before, 'after>>,
    counter: &mut StableRowIndexDiffCounter,
) -> Result<(), StableRowIndexError> {
    counter.visited_node()?;
    counter.visited_node()?;
    if before.id().to_bytes() == after.id().to_bytes() {
        if before.canonical_bytes() != after.canonical_bytes()
            || before.row_count() != after.row_count()
            || before.len() != after.len()
            || before.first_key() != after.first_key()
            || last_key_in_node(before) != last_key_in_node(after)
        {
            return Err(StableRowIndexError::Tree(TreeError::InvalidRoot));
        }
        unchanged_subtrees
            .try_reserve(1)
            .map_err(|_| StableRowIndexError::Allocation)?;
        unchanged_subtrees.push(StableRowIndexUnchangedSubtree {
            before: before_index,
            after: after_index,
            before_root: before_index.root(),
            after_root: after_index.root(),
            first_key: before.first_key().copied(),
            last_key: last_key_in_node(before).copied(),
            row_count: before.row_count(),
            subtree_commitment: before.id().to_bytes(),
        });
        return counter.skipped_subtree();
    }
    if before.is_empty() || after.is_empty() {
        return diff_run_rows(&[before], &[after], entries, counter);
    }

    match (before.entries(), after.entries()) {
        (Some(before_rows), Some(after_rows)) => diff_rows(
            before_rows.iter().map(|(key, payload)| (key, payload)),
            after_rows.iter().map(|(key, payload)| (key, payload)),
            entries,
            counter,
        ),
        (None, None) if before.level() == after.level() => {
            let before_children = collect_children(before)?;
            let after_children = collect_children(after)?;
            diff_node_lists(
                &before_children,
                &after_children,
                before_index,
                after_index,
                entries,
                unchanged_subtrees,
                counter,
            )
        }
        _ => {
            // Trees can have different heights after a root split or collapse.
            // Descend the taller side to the lower root's child level on both
            // sides, which exposes stable anchors below the height mismatch.
            let target_level = before.level().min(after.level()).saturating_sub(1);
            let before_frontier = collect_at_level(before, target_level, counter)?;
            let after_frontier = collect_at_level(after, target_level, counter)?;
            diff_node_lists(
                &before_frontier,
                &after_frontier,
                before_index,
                after_index,
                entries,
                unchanged_subtrees,
                counter,
            )
        }
    }
}

fn last_key_in_node<'tree>(mut node: RowNodeView<'tree>) -> Option<&'tree StableRowKey> {
    loop {
        if let Some(entries) = node.entries() {
            return entries.last().map(|(key, _)| key);
        }
        node = node.children().last()?;
    }
}

fn collect_children<'tree>(
    node: RowNodeView<'tree>,
) -> Result<Vec<RowNodeView<'tree>>, StableRowIndexError> {
    let mut children = Vec::new();
    children
        .try_reserve(node.child_count())
        .map_err(|_| StableRowIndexError::Allocation)?;
    children.extend(node.children());
    Ok(children)
}

fn collect_at_level<'tree>(
    node: RowNodeView<'tree>,
    target_level: u16,
    counter: &mut StableRowIndexDiffCounter,
) -> Result<Vec<RowNodeView<'tree>>, StableRowIndexError> {
    let mut frontier = Vec::new();
    collect_at_level_into(node, target_level, counter, &mut frontier)?;
    Ok(frontier)
}

fn collect_at_level_into<'tree>(
    node: RowNodeView<'tree>,
    target_level: u16,
    counter: &mut StableRowIndexDiffCounter,
    frontier: &mut Vec<RowNodeView<'tree>>,
) -> Result<(), StableRowIndexError> {
    counter.visited_node()?;
    if node.level() == target_level {
        frontier
            .try_reserve(1)
            .map_err(|_| StableRowIndexError::Allocation)?;
        frontier.push(node);
        return Ok(());
    }
    for child in node.children() {
        collect_at_level_into(child, target_level, counter, frontier)?;
    }
    Ok(())
}

fn diff_node_lists<'before, 'after>(
    before: &[RowNodeView<'before>],
    after: &[RowNodeView<'after>],
    before_tree: &'before StableRowIndex,
    after_tree: &'after StableRowIndex,
    entries: &mut Vec<StableRowIndexDiffEntry<'before, 'after>>,
    unchanged_subtrees: &mut Vec<StableRowIndexUnchangedSubtree<'before, 'after>>,
    counter: &mut StableRowIndexDiffCounter,
) -> Result<(), StableRowIndexError> {
    let (mut before_index, mut after_index) = (0, 0);
    while before_index < before.len() && after_index < after.len() {
        if node_frontiers_align(before, before_index, after, after_index)? {
            diff_node(
                before[before_index],
                after[after_index],
                before_tree,
                after_tree,
                entries,
                unchanged_subtrees,
                counter,
            )?;
            before_index += 1;
            after_index += 1;
            continue;
        }

        let (before_run, after_run) = (before_index, after_index);
        while before_index < before.len() && after_index < after.len() {
            let before_anchor = before[before_index]
                .first_key()
                .ok_or(StableRowIndexError::Tree(TreeError::InvalidRoot))?;
            let after_anchor = after[after_index]
                .first_key()
                .ok_or(StableRowIndexError::Tree(TreeError::InvalidRoot))?;
            match before_anchor.cmp(after_anchor) {
                std::cmp::Ordering::Less => before_index += 1,
                std::cmp::Ordering::Greater => after_index += 1,
                std::cmp::Ordering::Equal => {
                    if node_frontiers_align(before, before_index, after, after_index)? {
                        break;
                    }
                    // The nodes begin together but end at different sibling
                    // boundaries. Keep them in the row-merge run so rows do
                    // not become a false delete/insert pair across a split.
                    before_index += 1;
                    after_index += 1;
                }
            }
        }
        if before_index == before.len() || after_index == after.len() {
            // There is no later shared anchor. Merge the complete remaining
            // key ranges once; splitting them here would turn equal tail rows
            // into a delete followed by an insert.
            diff_run_rows(&before[before_run..], &after[after_run..], entries, counter)?;
            return Ok(());
        }
        diff_run_rows(
            &before[before_run..before_index],
            &after[after_run..after_index],
            entries,
            counter,
        )?;
    }
    if before_index < before.len() || after_index < after.len() {
        diff_run_rows(
            &before[before_index..],
            &after[after_index..],
            entries,
            counter,
        )?;
    }
    Ok(())
}

fn node_frontiers_align<'before, 'after>(
    before: &[RowNodeView<'before>],
    before_index: usize,
    after: &[RowNodeView<'after>],
    after_index: usize,
) -> Result<bool, StableRowIndexError> {
    let before_node = before[before_index];
    let after_node = after[after_index];
    let before_anchor = before_node
        .first_key()
        .ok_or(StableRowIndexError::Tree(TreeError::InvalidRoot))?;
    let after_anchor = after_node
        .first_key()
        .ok_or(StableRowIndexError::Tree(TreeError::InvalidRoot))?;
    if before_anchor != after_anchor {
        return Ok(false);
    }
    if before_node.id().to_bytes() == after_node.id().to_bytes() {
        return Ok(true);
    }
    let before_next = before
        .get(before_index + 1)
        .and_then(|node| node.first_key());
    let after_next = after.get(after_index + 1).and_then(|node| node.first_key());
    Ok(before_next == after_next)
}

/// Streams a divergent run without copying its row references. The frame
/// stack is bounded by tree height; the former merge path allocated one slot
/// per row on both sides even when the runs ultimately matched.
struct RunRowIter<'roots, 'tree> {
    roots: &'roots [RowNodeView<'tree>],
    next_root: usize,
    frames: Vec<TreeNodeChildren<'tree, SemanticRowIndexRelation>>,
    leaf: Option<std::slice::Iter<'tree, (StableRowKey, RowPayload)>>,
    remaining: usize,
    visited_nodes: u64,
}

impl<'roots, 'tree> RunRowIter<'roots, 'tree> {
    fn new(roots: &'roots [RowNodeView<'tree>]) -> Result<Self, StableRowIndexError> {
        let mut remaining = 0_usize;
        let mut maximum_level = 0_usize;
        for root in roots {
            remaining = remaining
                .checked_add(root.len())
                .ok_or(StableRowIndexError::Overflow)?;
            maximum_level = maximum_level.max(usize::from(root.level()));
        }
        let mut frames = Vec::new();
        if !roots.is_empty() {
            frames
                .try_reserve_exact(maximum_level.saturating_add(1))
                .map_err(|_| StableRowIndexError::Allocation)?;
        }
        Ok(Self {
            roots,
            next_root: 0,
            frames,
            leaf: None,
            remaining,
            visited_nodes: 0,
        })
    }

    fn descend(&mut self, node: RowNodeView<'tree>) {
        self.visited_nodes += 1;
        if let Some(entries) = node.entries() {
            self.leaf = Some(entries.iter());
        } else {
            self.frames.push(node.children());
        }
    }
}

impl<'tree> Iterator for RunRowIter<'_, 'tree> {
    type Item = (&'tree StableRowKey, &'tree RowPayload);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(leaf) = self.leaf.as_mut() {
                if let Some((key, payload)) = leaf.next() {
                    self.remaining -= 1;
                    return Some((key, payload));
                }
                self.leaf = None;
            }
            if let Some(frame) = self.frames.last_mut() {
                if let Some(child) = frame.next() {
                    self.descend(child);
                } else {
                    self.frames.pop();
                }
            } else if let Some(root) = self.roots.get(self.next_root).copied() {
                self.next_root += 1;
                self.descend(root);
            } else {
                return None;
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for RunRowIter<'_, '_> {
    fn len(&self) -> usize {
        self.remaining
    }
}

fn diff_run_rows<'before, 'after>(
    before_nodes: &[RowNodeView<'before>],
    after_nodes: &[RowNodeView<'after>],
    entries: &mut Vec<StableRowIndexDiffEntry<'before, 'after>>,
    counter: &mut StableRowIndexDiffCounter,
) -> Result<(), StableRowIndexError> {
    let mut before = RunRowIter::new(before_nodes)?;
    let mut after = RunRowIter::new(after_nodes)?;
    let frame_slots = before
        .frames
        .capacity()
        .checked_add(after.frames.capacity())
        .ok_or(StableRowIndexError::Overflow)?;
    let scratch_bytes = frame_slots
        .checked_mul(size_of::<TreeNodeChildren<'_, SemanticRowIndexRelation>>())
        .ok_or(StableRowIndexError::Overflow)?;
    counter.work.peak_run_cursor_scratch_bytes = counter
        .work
        .peak_run_cursor_scratch_bytes
        .max(usize_to_u64(scratch_bytes)?);
    diff_rows(&mut before, &mut after, entries, counter)?;
    counter.traversed_nodes(before.visited_nodes)?;
    counter.traversed_nodes(after.visited_nodes)?;
    Ok(())
}

fn push_diff_entry<'before, 'after>(
    entries: &mut Vec<StableRowIndexDiffEntry<'before, 'after>>,
    entry: StableRowIndexDiffEntry<'before, 'after>,
) -> Result<(), StableRowIndexError> {
    entries
        .try_reserve(1)
        .map_err(|_| StableRowIndexError::Allocation)?;
    entries.push(entry);
    Ok(())
}

fn diff_rows<'before, 'after>(
    before: impl ExactSizeIterator<Item = (&'before StableRowKey, &'before RowPayload)>,
    after: impl ExactSizeIterator<Item = (&'after StableRowKey, &'after RowPayload)>,
    entries: &mut Vec<StableRowIndexDiffEntry<'before, 'after>>,
    counter: &mut StableRowIndexDiffCounter,
) -> Result<(), StableRowIndexError> {
    let mut before = before.peekable();
    let mut after = after.peekable();
    while let (Some((before_key, before_payload)), Some((after_key, after_payload))) =
        (before.peek().copied(), after.peek().copied())
    {
        counter.examined_rows(2)?;
        match before_key.cmp(after_key) {
            std::cmp::Ordering::Less => {
                push_diff_entry(
                    entries,
                    StableRowIndexDiffEntry::Delete {
                        key: *before_key,
                        before: before_payload,
                    },
                )?;
                before.next();
            }
            std::cmp::Ordering::Greater => {
                push_diff_entry(
                    entries,
                    StableRowIndexDiffEntry::Insert {
                        key: *after_key,
                        after: after_payload,
                    },
                )?;
                after.next();
            }
            std::cmp::Ordering::Equal => {
                if before_payload != after_payload {
                    push_diff_entry(
                        entries,
                        StableRowIndexDiffEntry::Replace {
                            key: *before_key,
                            before: before_payload,
                            after: after_payload,
                        },
                    )?;
                }
                before.next();
                after.next();
            }
        }
    }
    for (key, payload) in before {
        counter.examined_rows(1)?;
        push_diff_entry(
            entries,
            StableRowIndexDiffEntry::Delete {
                key: *key,
                before: payload,
            },
        )?;
    }
    for (key, payload) in after {
        counter.examined_rows(1)?;
        push_diff_entry(
            entries,
            StableRowIndexDiffEntry::Insert {
                key: *key,
                after: payload,
            },
        )?;
    }
    Ok(())
}

fn usize_to_u64(value: usize) -> Result<u64, StableRowIndexError> {
    u64::try_from(value).map_err(|_| StableRowIndexError::Overflow)
}

/// Owner-specific mutable scratch for one sorted row run.
#[derive(Default)]
pub struct StableRowIndexBuilder {
    rows: Vec<(StableRowKey, RowPayload)>,
}

impl StableRowIndexBuilder {
    /// Adds one already content-verified row in strictly increasing order.
    pub fn push(
        &mut self,
        key: StableRowKey,
        payload: RowPayload,
    ) -> Result<(), StableRowIndexError> {
        if self
            .rows
            .last()
            .is_some_and(|(previous, _)| *previous >= key)
        {
            return Err(StableRowIndexError::UnsortedOrDuplicate);
        }
        self.rows
            .try_reserve(1)
            .map_err(|_| StableRowIndexError::Allocation)?;
        self.rows.push((key, payload));
        Ok(())
    }

    /// Number of row records in the local scratch run.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the local scratch run is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Logical live bytes reserved by the current sorted row run.
    ///
    /// This multiplies the vector's actual capacity by the inline slot size.
    /// It excludes allocator metadata and any other aggregate-verifier
    /// buffers; the builder itself retains this complete row run until
    /// `finish` consumes it.
    #[must_use]
    pub fn buffered_row_slot_capacity_bytes(&self) -> usize {
        self.rows
            .capacity()
            .saturating_mul(size_of::<(StableRowKey, RowPayload)>())
    }

    /// Builds and consumes this owner-local run.
    pub fn finish(self) -> Result<StableRowIndex, StableRowIndexError> {
        Ok(StableRowIndex {
            tree: RowTree::from_sorted_items_owned(self.rows)?,
        })
    }
}

/// One row action, independent of how its replacement value is represented.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StableRowAction<T> {
    /// Inserts or replaces the row with this value.
    Put(T),
    /// Removes the row if present.
    Delete,
    /// Carries a proven unchanged frontier entry without touching the tree.
    Unchanged,
}

/// One pre-hashed insert, replacement, deletion, or explicit unchanged row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableRowIndexChange {
    key: StableRowKey,
    action: StableRowAction<RowPayload>,
}

impl StableRowIndexChange {
    /// Creates one explicit pre-hashed action.
    #[must_use]
    pub const fn new(key: StableRowKey, action: StableRowAction<RowPayload>) -> Self {
        Self { key, action }
    }

    /// Creates an insert or replacement.
    #[must_use]
    pub const fn put(key: StableRowKey, payload: RowPayload) -> Self {
        Self::new(key, StableRowAction::Put(payload))
    }

    /// Creates a deletion.
    #[must_use]
    pub const fn delete(key: StableRowKey) -> Self {
        Self::new(key, StableRowAction::Delete)
    }

    /// Creates an explicit unchanged frontier entry. It is removed before
    /// path copying and costs no tree or payload work.
    #[must_use]
    pub const fn unchanged(key: StableRowKey) -> Self {
        Self::new(key, StableRowAction::Unchanged)
    }
}

/// Canonical row tag and borrowed payload bytes for one sparse replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableRowTaggedBytes<'bytes> {
    tag: u8,
    payload: &'bytes [u8],
}

impl<'bytes> StableRowTaggedBytes<'bytes> {
    /// Borrows one exact canonical row value without copying it.
    #[must_use]
    pub const fn new(tag: u8, payload: &'bytes [u8]) -> Self {
        Self { tag, payload }
    }
}

/// The same action algebra used by pre-hashed rows, carrying tagged bytes.
pub type StableRowPayloadAction<'bytes> = StableRowAction<StableRowTaggedBytes<'bytes>>;

/// One ordered raw row action supplied to the owner-specific edit scratch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableRowPayloadChange<'bytes> {
    key: StableRowKey,
    action: StableRowPayloadAction<'bytes>,
}

impl<'bytes> StableRowPayloadChange<'bytes> {
    /// Creates one explicit insert, replacement, delete, or unchanged action.
    #[must_use]
    pub const fn new(key: StableRowKey, action: StableRowPayloadAction<'bytes>) -> Self {
        Self { key, action }
    }

    /// Creates an insert/replacement from an exact tagged row payload.
    #[must_use]
    pub const fn put(key: StableRowKey, tag: u8, payload: &'bytes [u8]) -> Self {
        Self::new(
            key,
            StableRowAction::Put(StableRowTaggedBytes::new(tag, payload)),
        )
    }

    /// Creates a deletion.
    #[must_use]
    pub const fn delete(key: StableRowKey) -> Self {
        Self::new(key, StableRowAction::Delete)
    }

    /// Creates an explicit unchanged frontier entry without row bytes.
    #[must_use]
    pub const fn unchanged(key: StableRowKey) -> Self {
        Self::new(key, StableRowAction::Unchanged)
    }
}

/// Work counters for one batch edit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StableRowIndexUpdateWork {
    /// Existing persistent-tree update counters.
    pub tree: TreeWork,
    /// Exact tagged row bytes hashed by `prepare_payload_update`, including one tag byte per put.
    pub row_payload_hash_bytes: u64,
    /// Number of input records that changed their committed value or presence.
    pub changed_keys: u64,
}

/// A prepared immutable row-index generation.
#[derive(Debug)]
pub struct PreparedStableRowIndexUpdate {
    index: StableRowIndex,
    work: StableRowIndexUpdateWork,
}

impl PreparedStableRowIndexUpdate {
    /// Exact path-copy and payload-hash work performed for this update.
    #[must_use]
    pub const fn work(&self) -> StableRowIndexUpdateWork {
        self.work
    }

    /// Borrows the complete target generation before publication.
    #[must_use]
    pub const fn index(&self) -> &StableRowIndex {
        &self.index
    }

    /// Publishes the prepared index to the caller.
    #[must_use]
    pub fn commit(self) -> StableRowIndex {
        self.index
    }
}

impl StableRowIndex {
    /// Prepares a sorted batch whose row payload identities were computed by
    /// the producer. The index itself hashes zero payload bytes in this mode.
    pub fn prepare_update(
        &self,
        changes: &[StableRowIndexChange],
    ) -> Result<PreparedStableRowIndexUpdate, StableRowIndexError> {
        if changes
            .windows(2)
            .any(|window| window[0].key >= window[1].key)
        {
            return Err(StableRowIndexError::UnsortedOrDuplicate);
        }
        let mut changed = Vec::new();
        changed
            .try_reserve(changes.len())
            .map_err(|_| StableRowIndexError::Allocation)?;
        for change in changes {
            let before = self.tree.get(&change.key).copied();
            let after = match change.action {
                StableRowAction::Put(payload) => Some(payload),
                StableRowAction::Delete => None,
                StableRowAction::Unchanged => continue,
            };
            if before == after {
                continue;
            }
            changed.push(TreeChange {
                key: change.key,
                after,
            });
        }
        self.prepare_tree_changes(changed, 0)
    }

    /// Hashes only tagged payloads explicitly included in the caller-supplied
    /// sparse frontier, then prepares the path-copy update. Empty frontiers
    /// and `Unchanged` actions hash zero row bytes; each `Put` hashes its
    /// payload and tag even when its identity already matches. Keys must be
    /// strictly increasing.
    ///
    /// Ordering does not prove that the frontier is complete: the caller is
    /// responsible for including every changed row. At present, V2 aggregate
    /// admission builds its index from all decoded seven-family records. No
    /// production call site currently supplies a frontier to this API.
    pub fn prepare_payload_update(
        &self,
        changes: &[StableRowPayloadChange<'_>],
    ) -> Result<PreparedStableRowIndexUpdate, StableRowIndexError> {
        if changes
            .windows(2)
            .any(|window| window[0].key >= window[1].key)
        {
            return Err(StableRowIndexError::UnsortedOrDuplicate);
        }
        let mut changed = Vec::new();
        changed
            .try_reserve(changes.len())
            .map_err(|_| StableRowIndexError::Allocation)?;
        let mut hashed_bytes = 0_u64;
        for change in changes {
            let after = match change.action {
                StableRowAction::Put(tagged) => {
                    let payload = RowPayload::from_tagged_bytes(tagged.tag, tagged.payload)?;
                    let tagged_len = payload
                        .byte_len
                        .checked_add(1)
                        .ok_or(StableRowIndexError::Overflow)?;
                    hashed_bytes = hashed_bytes
                        .checked_add(tagged_len)
                        .ok_or(StableRowIndexError::Overflow)?;
                    Some(payload)
                }
                StableRowAction::Delete => None,
                StableRowAction::Unchanged => continue,
            };
            if self.tree.get(&change.key).copied() == after {
                continue;
            }
            changed.push(TreeChange {
                key: change.key,
                after,
            });
        }
        self.prepare_tree_changes(changed, hashed_bytes)
    }

    /// Diffs two admitted roots in stable key order.
    ///
    /// Equal subtree commitments are skipped before their rows are visited.
    /// Child subtrees are paired only when their key intervals align; if a
    /// split changed an interval boundary, the divergent run is merged by row
    /// key up to the next shared boundary. The returned changes borrow payload
    /// references from both immutable roots; equal structural subtrees are
    /// retained as borrow-scoped certificates on the result.
    pub fn diff<'before, 'after>(
        &'before self,
        after: &'after Self,
    ) -> Result<StableRowIndexDiff<'before, 'after>, StableRowIndexError> {
        let mut entries = Vec::new();
        let mut unchanged_subtrees = Vec::new();
        let mut counter = StableRowIndexDiffCounter::default();
        diff_node(
            self.tree.root_view(),
            after.tree.root_view(),
            self,
            after,
            &mut entries,
            &mut unchanged_subtrees,
            &mut counter,
        )?;
        Ok(StableRowIndexDiff {
            before_root: self.root(),
            after_root: after.root(),
            entries: entries.into_boxed_slice(),
            unchanged_subtrees: unchanged_subtrees.into_boxed_slice(),
            work: counter.work,
        })
    }

    fn prepare_tree_changes(
        &self,
        changes: Vec<TreeChange<SemanticRowIndexRelation>>,
        row_payload_hash_bytes: u64,
    ) -> Result<PreparedStableRowIndexUpdate, StableRowIndexError> {
        if changes.is_empty() {
            return Ok(PreparedStableRowIndexUpdate {
                index: self.clone(),
                work: StableRowIndexUpdateWork {
                    row_payload_hash_bytes,
                    ..StableRowIndexUpdateWork::default()
                },
            });
        }
        let count = u64::try_from(changes.len()).map_err(|_| StableRowIndexError::Overflow)?;
        // The generic persistent-tree batch splice is useful when a frontier
        // describes one dense key interval. Semantic frontiers can instead
        // contain sparse edits across the whole image; applying each sorted
        // edit through the canonical single-key path keeps work proportional
        // to k root paths instead of rebuilding everything between the first
        // and last changed leaves.
        let mut tree = self.tree.clone();
        let mut tree_work = TreeWork::default();
        for change in &changes {
            let prepared = tree.prepare_update(std::slice::from_ref(change))?;
            tree_work = add_tree_work(tree_work, prepared.work())?;
            tree = prepared.commit();
        }
        Ok(PreparedStableRowIndexUpdate {
            index: StableRowIndex { tree },
            work: StableRowIndexUpdateWork {
                tree: tree_work,
                row_payload_hash_bytes,
                changed_keys: count,
            },
        })
    }
}

fn add_tree_work(left: TreeWork, right: TreeWork) -> Result<TreeWork, StableRowIndexError> {
    Ok(TreeWork {
        rows: left
            .rows
            .checked_add(right.rows)
            .ok_or(StableRowIndexError::Overflow)?,
        nodes: left
            .nodes
            .checked_add(right.nodes)
            .ok_or(StableRowIndexError::Overflow)?,
        visited_nodes: left
            .visited_nodes
            .checked_add(right.visited_nodes)
            .ok_or(StableRowIndexError::Overflow)?,
        reused_nodes: left
            .reused_nodes
            .checked_add(right.reused_nodes)
            .ok_or(StableRowIndexError::Overflow)?,
        copied_nodes: left
            .copied_nodes
            .checked_add(right.copied_nodes)
            .ok_or(StableRowIndexError::Overflow)?,
        encoded_bytes: left
            .encoded_bytes
            .checked_add(right.encoded_bytes)
            .ok_or(StableRowIndexError::Overflow)?,
    })
}

/// Half-open ordered key range `[start,end)`; either edge may be unbounded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableRowRange {
    start: Option<StableRowKey>,
    end: Option<StableRowKey>,
}

impl StableRowRange {
    /// Creates a half-open range. Equal bounds describe an empty range.
    pub fn new(
        start: Option<StableRowKey>,
        end: Option<StableRowKey>,
    ) -> Result<Self, StableRowIndexError> {
        let range = Self { start, end };
        validate_range(range)?;
        Ok(range)
    }

    /// Selects exactly one closed family.
    #[must_use]
    pub fn family(family: RowFamily) -> Self {
        let start = StableRowKey::family_floor(family);
        let end = RowFamily::from_code(family.code() + 1).map(StableRowKey::family_floor);
        Self {
            start: Some(start),
            end,
        }
    }

    /// Returns the inclusive start key, if bounded.
    #[must_use]
    pub const fn start(self) -> Option<StableRowKey> {
        self.start
    }

    /// Returns the exclusive end key, if bounded.
    #[must_use]
    pub const fn end(self) -> Option<StableRowKey> {
        self.end
    }

    fn bounds(self) -> (Bound<StableRowKey>, Bound<StableRowKey>) {
        (
            self.start.map_or(Bound::Unbounded, Bound::Included),
            self.end.map_or(Bound::Unbounded, Bound::Excluded),
        )
    }

    fn contains(self, key: StableRowKey) -> bool {
        self.start.is_none_or(|start| key >= start) && self.end.is_none_or(|end| key < end)
    }
}

fn validate_range(range: StableRowRange) -> Result<(), StableRowIndexError> {
    if range
        .start
        .zip(range.end)
        .is_some_and(|(start, end)| start > end)
    {
        return Err(StableRowIndexError::InvalidRange);
    }
    Ok(())
}

/// One borrowed key/payload pair yielded from an admitted range.
#[derive(Clone, Copy, Debug)]
pub struct StableRowIndexEntry<'index> {
    /// Stable family/key identity.
    pub key: &'index StableRowKey,
    /// Verified content-addressed row payload identity and length.
    pub payload: &'index RowPayload,
}

/// Ordered borrowed cursor whose range and total count are root-bound.
pub struct StableRowRangeCursor<'index> {
    root: StableRowIndexRoot,
    range: StableRowRange,
    inner: RowTreeIter<'index>,
    remaining: usize,
}

impl StableRowRangeCursor<'_> {
    /// Admitted root that authenticates every cursor result.
    #[must_use]
    pub const fn root(&self) -> StableRowIndexRoot {
        self.root
    }

    /// Half-open range represented by this cursor.
    #[must_use]
    pub const fn range(&self) -> StableRowRange {
        self.range
    }
}

impl<'index> Iterator for StableRowRangeCursor<'index> {
    type Item = StableRowIndexEntry<'index>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let (key, payload) = self.inner.next()?;
        self.remaining -= 1;
        Some(StableRowIndexEntry { key, payload })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for StableRowRangeCursor<'_> {
    fn len(&self) -> usize {
        self.remaining
    }
}

impl std::iter::FusedIterator for StableRowRangeCursor<'_> {}

/// Exact row count for each normalized semantic family at one admitted root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableRowIndexCensus {
    root: StableRowIndexRoot,
    counts: [u64; 7],
}

impl StableRowIndexCensus {
    /// Root whose authenticated ordered key space was counted.
    #[must_use]
    pub const fn root(self) -> StableRowIndexRoot {
        self.root
    }

    /// Returns the exact count for one family.
    #[must_use]
    pub const fn count(self, family: RowFamily) -> u64 {
        self.counts[family.index()]
    }

    /// Returns counts in [`RowFamily::ALL`] order.
    #[must_use]
    pub const fn counts(self) -> [u64; 7] {
        self.counts
    }
}

/// Untrusted exact-census claim from a manifest or peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UntrustedStableRowIndexCensus {
    root: UntrustedStableRowIndexRoot,
    counts: [u64; 7],
}

impl UntrustedStableRowIndexCensus {
    /// Reads an untrusted root and seven counts without admitting them.
    #[must_use]
    pub const fn from_raw(root: [u8; 32], counts: [u64; 7]) -> Self {
        Self {
            root: UntrustedStableRowIndexRoot(root),
            counts,
        }
    }

    /// Returns the claimed family counts.
    #[must_use]
    pub const fn counts(self) -> [u64; 7] {
        self.counts
    }
}

/// One range-proof row whose value remains an untrusted payload claim until
/// its proof is checked against the complete admitted root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableRowRangeProofEntry {
    key: StableRowKey,
    payload: UntrustedRowPayloadIdentity,
}

impl StableRowRangeProofEntry {
    /// Reads one untrusted key and payload reference.
    #[must_use]
    pub const fn from_raw(key: StableRowKey, payload: UntrustedRowPayloadIdentity) -> Self {
        Self { key, payload }
    }
}

/// Untrusted page result bound to one row-index root and half-open range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableRowRangeProof {
    root: UntrustedStableRowIndexRoot,
    range: StableRowRange,
    entries: Box<[StableRowRangeProofEntry]>,
}

impl StableRowRangeProof {
    /// Reads a page proof without admitting its root, ordering, or contents.
    #[must_use]
    pub fn from_raw(
        root: [u8; 32],
        range: StableRowRange,
        entries: Vec<StableRowRangeProofEntry>,
    ) -> Self {
        Self {
            root: UntrustedStableRowIndexRoot(root),
            range,
            entries: entries.into_boxed_slice(),
        }
    }

    /// Exact number of claimed rows in the page.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the claimed page has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Build counters and exact resident-byte model for a newly built root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableRowIndexBuildWork {
    /// Canonical persistent-tree construction counters.
    pub tree: TreeWork,
    /// Memory accounting over the retained generation.
    pub resident: StableRowIndexMemoryUsage,
}

/// Explicit memory accounting for the row index.
///
/// Canonical bytes, row slots, child references, child offsets, and level
/// counters are counted from the retained tree. Metadata sizes and the final
/// total are estimates that exclude allocator headers, alignment slack, and
/// vector capacity beyond current length. Referenced payload bytes are
/// reported separately because payload bodies are stored outside this index.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StableRowIndexMemoryUsage {
    /// Rows retained by leaf slabs.
    pub rows: u64,
    /// Canonical leaf and branch node count.
    pub nodes: u64,
    /// Exact bytes in canonical node encodings.
    pub canonical_node_bytes: u64,
    /// Exact element-slot bytes for key and payload-reference values.
    pub row_slot_storage_bytes: u64,
    /// Exact child-handle element-slot bytes.
    pub child_handle_storage_bytes: u64,
    /// Exact subtree-offset element-slot bytes.
    pub child_offset_storage_bytes: u64,
    /// Estimated level-count element-slot bytes.
    pub level_count_storage_estimate_bytes: u64,
    /// Estimated inline tree-node structure bytes.
    pub tree_node_header_estimate_bytes: u64,
    /// Estimated `CanonicalNode` structure bytes, excluding its encoded data.
    pub canonical_node_metadata_estimate_bytes: u64,
    /// Approximate retained index bytes from the fields above.
    pub estimated_resident_bytes: u64,
    /// Sum of external payload lengths named by the index.
    pub referenced_payload_bytes: u64,
}

#[repr(C)]
struct NodeHeaderEstimate {
    canonical: Arc<backend_version::CanonicalNode<SemanticRowIndexRelation>>,
    leaf_items: Option<Arc<[(StableRowKey, RowPayload)]>>,
    children: Option<Arc<[TreeNodeHandle<SemanticRowIndexRelation>]>>,
    child_entry_offsets: Option<Arc<[usize]>>,
    len: usize,
    level_counts: Arc<[usize]>,
}

/// Errors from stable-row identity admission, index construction, updates,
/// range claims, and exact census verification.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum StableRowIndexError {
    /// Input keys were not strictly increasing or repeated a key.
    #[error("stable row keys must be strictly increasing")]
    UnsortedOrDuplicate,
    /// A half-open range had its start after its end.
    #[error("stable row range start exceeds its end")]
    InvalidRange,
    /// A root claim did not match the exact reconstructed root.
    #[error("stable row index root claim did not match")]
    RootMismatch,
    /// A census omitted, duplicated, or assigned a row to the wrong family.
    #[error("stable row index census did not match the exact ordered rows")]
    CensusMismatch,
    /// A range proof omitted, added, moved, or changed a row.
    #[error("stable row range proof did not match the authenticated range")]
    RangeProofMismatch,
    /// Payload bytes did not match the claimed exact length.
    #[error("stable row payload length did not match its claim")]
    PayloadLengthMismatch,
    /// Payload bytes did not match the claimed content identity.
    #[error("stable row payload digest did not match its claim")]
    PayloadDigestMismatch,
    /// A fixed-width persisted row-payload claim had an invalid encoding tag.
    #[error("stable row payload claim has a malformed encoding")]
    MalformedPayloadClaim,
    /// Checked row, count, or resident-byte arithmetic overflowed.
    #[error("stable row index arithmetic overflow")]
    Overflow,
    /// Caller-owned scratch could not reserve the requested capacity.
    #[error("stable row index scratch allocation failed")]
    Allocation,
    /// Existing backend-version canonical-tree validation failed.
    #[error(transparent)]
    Tree(#[from] TreeError),
    /// Existing backend-version typed content identity validation failed.
    #[error(transparent)]
    Identity(#[from] RuntimeIdentityError),
}

#[cfg(test)]
mod tests;
