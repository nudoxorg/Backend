//! Persistent stable-key index for normalized semantic rows.
//!
//! The index reuses `backend-version`'s canonical immutable relation tree. A
//! row is keyed by one of the seven closed semantic family codes and a
//! 32-byte stable key. Its value is only a verified content identity and
//! length; payload bytes remain in immutable payload objects. The tree owns
//! rows in leaf slabs and copies affected paths for edits, so there is no
//! `Arc` per row and no process-wide mutable interner.

use std::{mem::size_of, ops::Bound, sync::Arc};

use backend_version::{
    ObjectVersion, ObjectVersionHasher, PersistentTree, Relation, RuntimeIdentityError, Schema,
    SchemaIdentity, StateRoot, TreeChange, TreeError, TreeNodeHandle, TreeNodeView, TreeRangeIter,
    TreeWork,
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
/// Values are generated from exact bytes or admitted against exact bytes;
/// arbitrary digest bytes cannot be converted to this type.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowPayloadId(ObjectVersion<SemanticRowPayloadSchema>);

impl RowPayloadId {
    /// Returns the canonical payload digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
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
            id: self.id.0.to_bytes(),
            byte_len: self.byte_len,
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
}

impl UntrustedRowPayloadIdentity {
    /// Reads an identity claim from a fixed-width wire value.
    #[must_use]
    pub const fn from_raw(id: [u8; 32], byte_len: u64) -> Self {
        Self { id, byte_len }
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

    /// Admits the claim only when both its exact length and digest match.
    pub fn admit(self, bytes: &[u8]) -> Result<RowPayload, StableRowIndexError> {
        let actual_len = u64::try_from(bytes.len()).map_err(|_| StableRowIndexError::Overflow)?;
        if actual_len != self.byte_len {
            return Err(StableRowIndexError::PayloadLengthMismatch);
        }
        let actual = payload_id(bytes)?;
        if actual.as_bytes() != &self.id {
            return Err(StableRowIndexError::PayloadDigestMismatch);
        }
        Ok(RowPayload {
            id: actual,
            byte_len: self.byte_len,
        })
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
    Ok(RowPayloadId(
        hasher.finish_version::<SemanticRowPayloadSchema>()?,
    ))
}

#[derive(Debug, Eq, PartialEq)]
struct SemanticRowIndexRelation;

impl Relation for SemanticRowIndexRelation {
    const DOMAIN: u8 = 0x53;
    const TYPE: u16 = 0xf201;
    const VERSION: u8 = 1;

    type Key = StableRowKey;
    type Value = RowPayload;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        out.push(key.family.code());
        out.extend_from_slice(&key.key);
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
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
}

/// Ordered diff whose payload references borrow the two immutable roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableRowIndexDiff<'before, 'after> {
    before_root: StableRowIndexRoot,
    after_root: StableRowIndexRoot,
    entries: Box<[StableRowIndexDiffEntry<'before, 'after>]>,
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
}

fn diff_node<'before, 'after>(
    before: RowNodeView<'before>,
    after: RowNodeView<'after>,
    entries: &mut Vec<StableRowIndexDiffEntry<'before, 'after>>,
    counter: &mut StableRowIndexDiffCounter,
) -> Result<(), StableRowIndexError> {
    counter.visited_node()?;
    counter.visited_node()?;
    if before.id().to_bytes() == after.id().to_bytes() {
        return counter.skipped_subtree();
    }
    if before.is_empty() || after.is_empty() {
        let before_rows = collect_run_rows(&[before], counter)?;
        let after_rows = collect_run_rows(&[after], counter)?;
        return diff_rows(
            before_rows.iter().copied(),
            after_rows.iter().copied(),
            entries,
            counter,
        );
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
            diff_node_lists(&before_children, &after_children, entries, counter)
        }
        _ => {
            // Trees can have different heights after a root split or collapse.
            // Descend the taller side to the lower root's child level on both
            // sides, which exposes stable anchors below the height mismatch.
            let target_level = before.level().min(after.level()).saturating_sub(1);
            let before_frontier = collect_at_level(before, target_level, counter)?;
            let after_frontier = collect_at_level(after, target_level, counter)?;
            diff_node_lists(&before_frontier, &after_frontier, entries, counter)
        }
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
    entries: &mut Vec<StableRowIndexDiffEntry<'before, 'after>>,
    counter: &mut StableRowIndexDiffCounter,
) -> Result<(), StableRowIndexError> {
    let (mut before_index, mut after_index) = (0, 0);
    while before_index < before.len() && after_index < after.len() {
        if node_frontiers_align(before, before_index, after, after_index)? {
            diff_node(before[before_index], after[after_index], entries, counter)?;
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
            let before_rows = collect_run_rows(&before[before_run..], counter)?;
            let after_rows = collect_run_rows(&after[after_run..], counter)?;
            diff_rows(
                before_rows.iter().copied(),
                after_rows.iter().copied(),
                entries,
                counter,
            )?;
            return Ok(());
        }
        let before_rows = collect_run_rows(&before[before_run..before_index], counter)?;
        let after_rows = collect_run_rows(&after[after_run..after_index], counter)?;
        diff_rows(
            before_rows.iter().copied(),
            after_rows.iter().copied(),
            entries,
            counter,
        )?;
    }
    if before_index < before.len() || after_index < after.len() {
        let before_rows = collect_run_rows(&before[before_index..], counter)?;
        let after_rows = collect_run_rows(&after[after_index..], counter)?;
        diff_rows(
            before_rows.iter().copied(),
            after_rows.iter().copied(),
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

fn collect_run_rows<'tree>(
    nodes: &[RowNodeView<'tree>],
    counter: &mut StableRowIndexDiffCounter,
) -> Result<Vec<(&'tree StableRowKey, &'tree RowPayload)>, StableRowIndexError> {
    let capacity = nodes.iter().try_fold(0_usize, |sum, node| {
        sum.checked_add(usize::try_from(node.len()).ok()?)
    });
    let capacity = capacity.ok_or(StableRowIndexError::Overflow)?;
    let mut rows = Vec::new();
    rows.try_reserve(capacity)
        .map_err(|_| StableRowIndexError::Allocation)?;
    for node in nodes {
        collect_rows_into(*node, counter, &mut rows)?;
    }
    Ok(rows)
}

fn collect_rows_into<'tree>(
    node: RowNodeView<'tree>,
    counter: &mut StableRowIndexDiffCounter,
    rows: &mut Vec<(&'tree StableRowKey, &'tree RowPayload)>,
) -> Result<(), StableRowIndexError> {
    counter.visited_node()?;
    if let Some(entries) = node.entries() {
        rows.extend(entries.iter().map(|(key, payload)| (key, payload)));
        return Ok(());
    }
    for child in node.children() {
        collect_rows_into(child, counter, rows)?;
    }
    Ok(())
}

fn diff_rows<'before, 'after>(
    before: impl ExactSizeIterator<Item = (&'before StableRowKey, &'before RowPayload)>,
    after: impl ExactSizeIterator<Item = (&'after StableRowKey, &'after RowPayload)>,
    entries: &mut Vec<StableRowIndexDiffEntry<'before, 'after>>,
    counter: &mut StableRowIndexDiffCounter,
) -> Result<(), StableRowIndexError> {
    let maximum_changes = before
        .len()
        .checked_add(after.len())
        .ok_or(StableRowIndexError::Overflow)?;
    entries
        .try_reserve(maximum_changes)
        .map_err(|_| StableRowIndexError::Allocation)?;
    let mut before = before.peekable();
    let mut after = after.peekable();
    while let (Some((before_key, before_payload)), Some((after_key, after_payload))) =
        (before.peek().copied(), after.peek().copied())
    {
        counter.examined_rows(2)?;
        match before_key.cmp(after_key) {
            std::cmp::Ordering::Less => {
                entries.push(StableRowIndexDiffEntry::Delete {
                    key: *before_key,
                    before: before_payload,
                });
                before.next();
            }
            std::cmp::Ordering::Greater => {
                entries.push(StableRowIndexDiffEntry::Insert {
                    key: *after_key,
                    after: after_payload,
                });
                after.next();
            }
            std::cmp::Ordering::Equal => {
                if before_payload != after_payload {
                    entries.push(StableRowIndexDiffEntry::Replace {
                        key: *before_key,
                        before: before_payload,
                        after: after_payload,
                    });
                }
                before.next();
                after.next();
            }
        }
    }
    for (key, payload) in before {
        counter.examined_rows(1)?;
        entries.push(StableRowIndexDiffEntry::Delete {
            key: *key,
            before: payload,
        });
    }
    for (key, payload) in after {
        counter.examined_rows(1)?;
        entries.push(StableRowIndexDiffEntry::Insert {
            key: *key,
            after: payload,
        });
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

    /// Builds and consumes this owner-local run.
    pub fn finish(self) -> Result<StableRowIndex, StableRowIndexError> {
        StableRowIndex::from_sorted_rows(&self.rows)
    }
}

/// One pre-hashed insert, replacement, deletion, or explicit unchanged row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableRowIndexChange {
    key: StableRowKey,
    after: Option<Option<RowPayload>>,
}

impl StableRowIndexChange {
    /// Creates an insert, replacement, or deletion.
    #[must_use]
    pub const fn new(key: StableRowKey, after: Option<RowPayload>) -> Self {
        Self {
            key,
            after: Some(after),
        }
    }

    /// Creates an explicit unchanged frontier entry. It is removed before
    /// path copying and costs no tree or payload work.
    #[must_use]
    pub const fn unchanged(key: StableRowKey) -> Self {
        Self { key, after: None }
    }
}

/// A raw changed row payload supplied to the owner-specific edit scratch.
#[derive(Clone, Copy, Debug)]
pub struct StableRowPayloadChange<'bytes> {
    key: StableRowKey,
    after: Option<Option<&'bytes [u8]>>,
}

impl<'bytes> StableRowPayloadChange<'bytes> {
    /// Creates an insert/replacement from exact row bytes or a deletion.
    #[must_use]
    pub const fn new(key: StableRowKey, after: Option<&'bytes [u8]>) -> Self {
        Self {
            key,
            after: Some(after),
        }
    }

    /// Creates an explicit unchanged frontier entry without row bytes.
    #[must_use]
    pub const fn unchanged(key: StableRowKey) -> Self {
        Self { key, after: None }
    }
}

/// Work counters for one batch edit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StableRowIndexUpdateWork {
    /// Existing persistent-tree update counters.
    pub tree: TreeWork,
    /// Exact payload bytes hashed by `prepare_payload_update`.
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
            let Some(after) = change.after else {
                continue;
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

    /// Hashes only payloads explicitly included in a complete producer
    /// frontier, then prepares the path-copy update. Empty/no-op frontiers
    /// hash zero row bytes.
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
            let Some(after_bytes) = change.after else {
                continue;
            };
            let after = if let Some(bytes) = after_bytes {
                let payload = RowPayload::from_bytes(bytes)?;
                hashed_bytes = hashed_bytes
                    .checked_add(payload.byte_len)
                    .ok_or(StableRowIndexError::Overflow)?;
                Some(payload)
            } else {
                None
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
    /// references from both immutable roots.
    pub fn diff<'before, 'after>(
        &'before self,
        after: &'after Self,
    ) -> Result<StableRowIndexDiff<'before, 'after>, StableRowIndexError> {
        let mut entries = Vec::new();
        let mut counter = StableRowIndexDiffCounter::default();
        diff_node(
            self.tree.root_view(),
            after.tree.root_view(),
            &mut entries,
            &mut counter,
        )?;
        Ok(StableRowIndexDiff {
            before_root: self.root(),
            after_root: after.root(),
            entries: entries.into_boxed_slice(),
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
