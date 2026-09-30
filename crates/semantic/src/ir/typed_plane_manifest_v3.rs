//! Persistent V3 typed-plane row-index catalog.
//!
//! V2 writes a flat vector of c004 segment descriptors into c005. V3 replaces
//! that vector with seven typed roots over immutable ordered row-reference
//! trees. Each leaf binds a stable row key to both the semantic payload claim
//! and its exact payload-store object ID. The tree nodes are the canonical
//! persisted pages supplied by `backend_version`; unchanged pages keep their
//! IDs across generations, and a cold family/range read uses `LazyTree` to
//! load only the selected path and page.
//!
//! This module owns the index grammar and structural admission only. A root
//! catalog does not establish row-payload correctness or semantic
//! cross-family closure. The aggregate semantic verifier must still read and
//! admit every payload and check all cross-family references before
//! publication. V2 remains the current publication format until that adapter
//! is completed.
//!
//! Cold range and closure reads have bounded logical live buffers, but this
//! is not a low-memory verification cutover: the aggregate semantic verifier
//! and this module's sorted bulk builder still retain O(rows) input
//! descriptors while constructing their complete indexes. In particular,
//! the existing large-package tier allows 2,000,000 rows, so full generation
//! admission at that limit still has a substantial row-run allocation (and
//! may overlap it with tree construction); the cold zipper does not change
//! that producer/aggregate memory profile.

use std::{ops::Bound, vec::Vec};

use backend_version::{
    CanonicalRelation, CanonicalRootAdmissionError, CheckedCanonicalRoot, DEFAULT_CUT_POLICY,
    IdContext, LazyPreparedUpdate, LazyTree, LazyTreeUpdateBudget, LazyTreeWork, NodeError,
    PersistedTreeRoot, PersistentTree, Relation, RelationDecodeError, Schema, SchemaIdentity,
    TreeChange, TreeNodeLoader, TreeRangeIter, UntrustedId,
};
use blake3::Hasher;
use thiserror::Error;

use crate::vocabulary::LanguageProfile;

use super::row_index::{RowFamily, StableRowIndexError, StableRowKey, UntrustedRowPayloadIdentity};

const MAGIC: [u8; 4] = *b"STPI";
/// V3 structural index-catalog wire revision.
pub const SEMANTIC_TYPED_PLANE_INDEX_CATALOG_V3_WIRE_REVISION: u16 = 1;
const FAMILY_COUNT: usize = 7;
const HEADER_BYTES: usize = 4 + 2 + 32 + 1;
const FAMILY_DESCRIPTOR_BYTES: usize = 1 + 1 + 2 + 8 + 32 + 32;
/// The V3 catalog has fixed size; row and descriptor metadata live in pages.
pub const MAX_SEMANTIC_TYPED_PLANE_INDEX_CATALOG_V3_BYTES: usize =
    HEADER_BYTES + FAMILY_COUNT * FAMILY_DESCRIPTOR_BYTES;
/// Maximum cold range page returned by the typed V3 index API.
pub const MAX_TYPED_PLANE_V3_PAGE_ROWS: usize = 256;
/// Maximum descriptor nodes one cold range page may admit.
pub const MAX_TYPED_PLANE_V3_PAGE_NODES: usize = 8_192;
/// Maximum canonical relation-node size accepted at a cold root boundary.
pub const MAX_TYPED_PLANE_V3_NODE_BYTES: usize = DEFAULT_CUT_POLICY.max_encoded_bytes();
/// Maximum descriptor nodes admitted during one cold closure walk.
pub const MAX_TYPED_PLANE_V3_CLOSURE_NODES: u64 = 1_000_000;
/// Maximum row references admitted during one cold closure walk.
pub const MAX_TYPED_PLANE_V3_CLOSURE_ROWS: u64 = 10_000_000;

const FAMILY_ROOT_DOMAIN: &[u8] = b"backend.semantic.typed-plane-family-root.v3\0";
const CATALOG_ROOT_DOMAIN: &[u8] = b"backend.semantic.typed-plane-index-catalog.v3\0";

/// One content-addressed object ID for an exact semantic row payload.
///
/// The ID remains a storage claim until the row's payload bytes are fetched
/// from the immutable closure and checked against its semantic payload claim.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UntrustedSemanticRowObjectIdV3([u8; 32]);

impl UntrustedSemanticRowObjectIdV3 {
    /// Retains the exact 32-byte object identity from the row-index wire.
    #[must_use]
    pub const fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the untrusted claimed object-identity bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Semantic payload identity claim and its untrusted physical-object claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticRowPayloadClaimV3 {
    payload: UntrustedRowPayloadIdentity,
    object: UntrustedSemanticRowObjectIdV3,
}

impl SemanticRowPayloadClaimV3 {
    /// Constructs one row reference from untrusted wire/storage claims.
    #[must_use]
    pub const fn from_untrusted_claims(
        payload: UntrustedRowPayloadIdentity,
        object: UntrustedSemanticRowObjectIdV3,
    ) -> Self {
        Self { payload, object }
    }

    /// Returns the semantic payload identity claim.
    #[must_use]
    pub const fn payload(self) -> UntrustedRowPayloadIdentity {
        self.payload
    }

    /// Returns the physical immutable-object claim.
    #[must_use]
    pub const fn object(self) -> UntrustedSemanticRowObjectIdV3 {
        self.object
    }
}

/// Canonical persisted row-reference relation used by V3 tree pages.
///
/// One family tree is a `backend_version::PersistentTree` relation keyed by
/// `(family, stable key)`. Values are fixed-width payload and object claims;
/// neither value is promoted to verified semantic data by node admission.
#[derive(Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneRowRelationV3;

impl Relation for SemanticTypedPlaneRowRelationV3 {
    const DOMAIN: u8 = 0x53;
    const TYPE: u16 = 0xf204;
    const VERSION: u8 = 1;

    type Key = StableRowKey;
    type Value = SemanticRowPayloadClaimV3;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        output.push(key.family().code());
        output.extend_from_slice(&key.key());
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.payload.to_fixed_wire());
        output.extend_from_slice(&value.object.0);
    }
}

impl CanonicalRelation for SemanticTypedPlaneRowRelationV3 {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        if bytes.len() != 33 {
            return Err(RelationDecodeError::Malformed);
        }
        let family = RowFamily::from_code(bytes[0]).ok_or(RelationDecodeError::Malformed)?;
        let key = bytes[1..]
            .try_into()
            .map_err(|_| RelationDecodeError::Malformed)?;
        Ok(StableRowKey::new(family, key))
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        if bytes.len() != 74 {
            return Err(RelationDecodeError::Malformed);
        }
        let payload = UntrustedRowPayloadIdentity::from_fixed_wire(&bytes[..42])
            .map_err(|_| RelationDecodeError::Malformed)?;
        let object = UntrustedSemanticRowObjectIdV3(
            bytes[42..]
                .try_into()
                .map_err(|_| RelationDecodeError::Malformed)?,
        );
        Ok(SemanticRowPayloadClaimV3 { payload, object })
    }
}

/// FileStore schema for the fixed-size V3 family-root catalog.
pub struct SemanticTypedPlaneIndexCatalogV3Schema;

impl Schema for SemanticTypedPlaneIndexCatalogV3Schema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc008;
    const VERSION: u8 = SEMANTIC_TYPED_PLANE_INDEX_CATALOG_V3_WIRE_REVISION as u8;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Exact schema identity for the V3 typed-plane root catalog.
pub const SEMANTIC_TYPED_PLANE_INDEX_CATALOG_V3_SCHEMA: SchemaIdentity = SchemaIdentity::new(
    SemanticTypedPlaneIndexCatalogV3Schema::DOMAIN,
    SemanticTypedPlaneIndexCatalogV3Schema::TYPE,
    SemanticTypedPlaneIndexCatalogV3Schema::VERSION,
);

type V3RowTree = PersistentTree<SemanticTypedPlaneRowRelationV3>;
type V3RowTreeIter<'tree> = TreeRangeIter<'tree, SemanticTypedPlaneRowRelationV3>;

/// Bounded construction policy for one family row tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneRowTreeLimitsV3 {
    /// Maximum rows accepted by the sorted builder.
    pub max_rows: u64,
    /// Maximum sum of semantic row-payload byte claims.
    pub max_payload_bytes: u64,
}

impl SemanticTypedPlaneRowTreeLimitsV3 {
    /// Creates an explicit row and payload bound.
    #[must_use]
    pub const fn new(max_rows: u64, max_payload_bytes: u64) -> Self {
        Self {
            max_rows,
            max_payload_bytes,
        }
    }
}

/// One family tree builder with owner-local mutable scratch.
///
/// Rows must arrive in strict stable-key order. The builder owns a packed
/// vector of fixed-width descriptors and creates immutable tree slabs in one
/// canonical bulk build. It does not allocate an `Arc` per row or use a
/// process-wide row mutex.
pub struct SemanticTypedPlaneRowTreeBuilderV3 {
    family: RowFamily,
    profile: Option<LanguageProfile>,
    limits: SemanticTypedPlaneRowTreeLimitsV3,
    rows: Vec<(StableRowKey, SemanticRowPayloadClaimV3)>,
    payload_bytes: u64,
}

impl SemanticTypedPlaneRowTreeBuilderV3 {
    /// Starts one closed family tree. Only LanguageExtensions carries a
    /// language profile.
    pub fn new(
        family: RowFamily,
        profile: Option<LanguageProfile>,
        limits: SemanticTypedPlaneRowTreeLimitsV3,
    ) -> Result<Self, SemanticTypedPlaneIndexV3Error> {
        if matches!(family, RowFamily::LanguageExtensions) != profile.is_some() {
            return Err(SemanticTypedPlaneIndexV3Error::FamilyProfile);
        }
        Ok(Self {
            family,
            profile,
            limits,
            rows: Vec::new(),
            payload_bytes: 0,
        })
    }

    /// Appends one untrusted row reference after checking family, order, and
    /// declared resource limits.
    pub fn push(
        &mut self,
        key: StableRowKey,
        payload: SemanticRowPayloadClaimV3,
    ) -> Result<(), SemanticTypedPlaneIndexV3Error> {
        if key.family() != self.family {
            return Err(SemanticTypedPlaneIndexV3Error::RowFamilyMismatch);
        }
        if self
            .rows
            .last()
            .is_some_and(|(previous, _)| *previous >= key)
        {
            return Err(SemanticTypedPlaneIndexV3Error::UnsortedOrDuplicate);
        }
        let next_count = u64::try_from(self.rows.len())
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or(SemanticTypedPlaneIndexV3Error::Overflow)?;
        let next_bytes = self
            .payload_bytes
            .checked_add(payload.payload.byte_len())
            .ok_or(SemanticTypedPlaneIndexV3Error::Overflow)?;
        if next_count > self.limits.max_rows {
            return Err(SemanticTypedPlaneIndexV3Error::RowLimit);
        }
        if next_bytes > self.limits.max_payload_bytes {
            return Err(SemanticTypedPlaneIndexV3Error::PayloadByteLimit);
        }
        self.rows
            .try_reserve(1)
            .map_err(|_| SemanticTypedPlaneIndexV3Error::Allocation)?;
        self.rows.push((key, payload));
        self.payload_bytes = next_bytes;
        Ok(())
    }

    /// Current descriptor scratch allocation in bytes.
    ///
    /// This is the row vector's actual capacity multiplied by its inline slot
    /// size. It excludes allocator metadata and the immutable tree allocated
    /// during `finish`; the sorted builder intentionally retains all rows
    /// until that bulk build begins.
    #[must_use]
    pub fn buffered_descriptor_capacity_bytes(&self) -> usize {
        self.rows
            .capacity()
            .saturating_mul(std::mem::size_of::<(StableRowKey, SemanticRowPayloadClaimV3)>())
    }

    /// Finishes a deterministic canonical bulk build.
    pub fn finish(self) -> Result<SemanticTypedPlaneFamilyIndexV3, SemanticTypedPlaneIndexV3Error> {
        let tree = V3RowTree::from_sorted_items_owned(self.rows)?;
        Ok(SemanticTypedPlaneFamilyIndexV3 {
            family: self.family,
            profile: self.profile,
            tree,
        })
    }
}

/// Immutable structural family index that lends range entries and canonical
/// node pages to a persistence adapter. Its rows are not payload-admitted.
pub struct SemanticTypedPlaneFamilyIndexV3 {
    family: RowFamily,
    profile: Option<LanguageProfile>,
    tree: V3RowTree,
}

impl SemanticTypedPlaneFamilyIndexV3 {
    /// Family whose rows are indexed by this tree.
    #[must_use]
    pub const fn family(&self) -> RowFamily {
        self.family
    }

    /// Closed language profile for LanguageExtensions.
    #[must_use]
    pub const fn profile(&self) -> Option<LanguageProfile> {
        self.profile
    }

    /// Exact authenticated row count from the canonical root.
    #[must_use]
    pub fn row_count(&self) -> u64 {
        self.tree.row_count()
    }

    /// Canonical root bytes for this family relation.
    #[must_use]
    pub fn tree_root(&self) -> [u8; 32] {
        self.tree.root().commitment().to_bytes()
    }

    /// Borrows a canonical half-open stable-key range `[start, end)`.
    pub fn range(
        &self,
        start: Option<StableRowKey>,
        end: Option<StableRowKey>,
    ) -> Result<SemanticTypedPlaneRangeCursorV3<'_>, SemanticTypedPlaneIndexV3Error> {
        if start.is_some_and(|key| key.family() != self.family)
            || end.is_some_and(|key| key.family() != self.family)
            || start.zip(end).is_some_and(|(start, end)| start > end)
        {
            return Err(SemanticTypedPlaneIndexV3Error::InvalidRange);
        }
        let bounds = (
            start.map_or(Bound::Unbounded, Bound::Included),
            end.map_or(Bound::Unbounded, Bound::Excluded),
        );
        Ok(SemanticTypedPlaneRangeCursorV3 {
            family: self.family,
            inner: self.tree.range(bounds),
        })
    }

    /// Borrows every canonical page in the immutable tree closure. A storage
    /// adapter can persist these exact bytes by their returned content IDs.
    pub fn node_closure(&self) -> impl Iterator<Item = SemanticTypedPlaneNodeV3<'_>> + '_ {
        self.tree
            .node_closure()
            .map(|node| SemanticTypedPlaneNodeV3 {
                id: node.id().to_bytes(),
                bytes: node.canonical_bytes(),
                row_count: node.row_count(),
                level: node.level(),
            })
    }

    fn descriptor(&self) -> SemanticTypedPlaneFamilyRootV3 {
        SemanticTypedPlaneFamilyRootV3::new(
            self.family,
            self.profile,
            self.row_count(),
            self.tree_root(),
        )
    }
}

/// One borrowed node page and its immutable content identity.
#[derive(Clone, Copy, Debug)]
pub struct SemanticTypedPlaneNodeV3<'node> {
    /// Canonical node identity used as the object-store key.
    pub id: [u8; 32],
    /// Exact canonical node bytes.
    pub bytes: &'node [u8],
    /// Authenticated row count below this node.
    pub row_count: u64,
    /// Tree level, where leaves are zero.
    pub level: u16,
}

/// One borrowed ordered row reference.
#[derive(Clone, Copy, Debug)]
pub struct SemanticTypedPlaneIndexEntryV3<'entry> {
    /// Stable family and row key.
    pub key: &'entry StableRowKey,
    /// Semantic payload and physical-object claims.
    pub reference: &'entry SemanticRowPayloadClaimV3,
}

/// Borrowed half-open range cursor over one retained family index.
pub struct SemanticTypedPlaneRangeCursorV3<'tree> {
    family: RowFamily,
    inner: V3RowTreeIter<'tree>,
}

impl<'tree> Iterator for SemanticTypedPlaneRangeCursorV3<'tree> {
    type Item = SemanticTypedPlaneIndexEntryV3<'tree>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|(key, reference)| SemanticTypedPlaneIndexEntryV3 { key, reference })
    }
}

/// Root claim for one family row tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneFamilyRootV3 {
    family: RowFamily,
    profile: Option<LanguageProfile>,
    row_count: u64,
    tree_root: [u8; 32],
    family_root: [u8; 32],
}

impl SemanticTypedPlaneFamilyRootV3 {
    /// Creates an untrusted family-root claim and checks its domain-separated
    /// family commitment. Tree bytes and row payloads are admitted separately.
    pub fn from_untrusted_claims(
        family: RowFamily,
        profile: Option<LanguageProfile>,
        row_count: u64,
        tree_root: [u8; 32],
        family_root: [u8; 32],
    ) -> Result<Self, SemanticTypedPlaneIndexV3Error> {
        if matches!(family, RowFamily::LanguageExtensions) != profile.is_some() {
            return Err(SemanticTypedPlaneIndexV3Error::FamilyProfile);
        }
        let descriptor = Self::new(family, profile, row_count, tree_root);
        if descriptor.family_root != family_root {
            return Err(SemanticTypedPlaneIndexV3Error::RootMismatch);
        }
        Ok(descriptor)
    }

    fn new(
        family: RowFamily,
        profile: Option<LanguageProfile>,
        row_count: u64,
        tree_root: [u8; 32],
    ) -> Self {
        let family_root = family_root(family, profile, row_count, &tree_root);
        Self {
            family,
            profile,
            row_count,
            tree_root,
            family_root,
        }
    }

    /// Closed row family.
    #[must_use]
    pub const fn family(self) -> RowFamily {
        self.family
    }

    /// Language profile, present only for LanguageExtensions.
    #[must_use]
    pub const fn profile(self) -> Option<LanguageProfile> {
        self.profile
    }

    /// Authenticated family row count claim.
    #[must_use]
    pub const fn row_count(self) -> u64 {
        self.row_count
    }

    /// Canonical row-tree root claim.
    #[must_use]
    pub const fn tree_root(self) -> [u8; 32] {
        self.tree_root
    }

    /// Family commitment binding family, profile, count, and tree root.
    #[must_use]
    pub const fn family_root(self) -> [u8; 32] {
        self.family_root
    }
}

/// Bounded fixed-size V3 structural index catalog of exactly seven family roots.
///
/// It contains no per-row or per-segment vector. This c008 value is not a
/// generation root, compiler-completeness certificate, or publication
/// authority. Its enclosing generation contract must authenticate the catalog;
/// callers then admit each tree closure and every row payload independently.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneIndexCatalogV3 {
    families: [SemanticTypedPlaneFamilyRootV3; FAMILY_COUNT],
    root: [u8; 32],
}

impl SemanticTypedPlaneIndexCatalogV3 {
    /// Creates a claim-only catalog from seven canonical family descriptors.
    pub fn from_untrusted_claims(
        families: [SemanticTypedPlaneFamilyRootV3; FAMILY_COUNT],
    ) -> Result<Self, SemanticTypedPlaneIndexV3Error> {
        validate_family_roots(&families)?;
        let root = catalog_root(&families);
        Ok(Self { families, root })
    }

    /// Decodes exactly one bounded canonical V3 root catalog.
    pub fn decode(bytes: &[u8]) -> Result<Self, SemanticTypedPlaneIndexV3Error> {
        if bytes.len() > MAX_SEMANTIC_TYPED_PLANE_INDEX_CATALOG_V3_BYTES {
            return Err(SemanticTypedPlaneIndexV3Error::CatalogTooLarge);
        }
        let mut reader = V3Reader::new(bytes);
        if reader.take(4)? != MAGIC {
            return Err(SemanticTypedPlaneIndexV3Error::Magic);
        }
        if reader.u16()? != SEMANTIC_TYPED_PLANE_INDEX_CATALOG_V3_WIRE_REVISION {
            return Err(SemanticTypedPlaneIndexV3Error::Version);
        }
        let claimed_root = reader.array32()?;
        if reader.u8()? as usize != FAMILY_COUNT {
            return Err(SemanticTypedPlaneIndexV3Error::FamilyCount);
        }
        let mut families = Vec::new();
        families
            .try_reserve_exact(FAMILY_COUNT)
            .map_err(|_| SemanticTypedPlaneIndexV3Error::Allocation)?;
        for _ in 0..FAMILY_COUNT {
            let family = RowFamily::from_code(reader.u8()?)
                .ok_or(SemanticTypedPlaneIndexV3Error::FamilyTag)?;
            let has_profile = reader.u8()?;
            let profile_bytes: [u8; 2] = reader
                .take(2)?
                .try_into()
                .map_err(|_| SemanticTypedPlaneIndexV3Error::Truncated)?;
            let profile = match (has_profile, family) {
                (0, RowFamily::LanguageExtensions) => {
                    return Err(SemanticTypedPlaneIndexV3Error::FamilyProfile);
                }
                (0, _) if profile_bytes == [0, 0] => None,
                (1, RowFamily::LanguageExtensions) => Some(
                    LanguageProfile::try_from(profile_bytes)
                        .map_err(|_| SemanticTypedPlaneIndexV3Error::FamilyProfile)?,
                ),
                _ => return Err(SemanticTypedPlaneIndexV3Error::FamilyProfile),
            };
            let row_count = reader.u64()?;
            let tree_root = reader.array32()?;
            let claimed_family_root = reader.array32()?;
            let descriptor =
                SemanticTypedPlaneFamilyRootV3::new(family, profile, row_count, tree_root);
            if descriptor.family_root != claimed_family_root {
                return Err(SemanticTypedPlaneIndexV3Error::RootMismatch);
            }
            families.push(descriptor);
        }
        reader.finish()?;
        let families: [SemanticTypedPlaneFamilyRootV3; FAMILY_COUNT] = families
            .try_into()
            .map_err(|_| SemanticTypedPlaneIndexV3Error::FamilyCount)?;
        validate_family_roots(&families)?;
        let root = catalog_root(&families);
        if root != claimed_root {
            return Err(SemanticTypedPlaneIndexV3Error::RootMismatch);
        }
        Ok(Self { families, root })
    }

    /// Returns canonical fixed-size catalog bytes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(MAX_SEMANTIC_TYPED_PLANE_INDEX_CATALOG_V3_BYTES);
        output.extend_from_slice(&MAGIC);
        output
            .extend_from_slice(&SEMANTIC_TYPED_PLANE_INDEX_CATALOG_V3_WIRE_REVISION.to_be_bytes());
        output.extend_from_slice(&self.root);
        output.push(FAMILY_COUNT as u8);
        for family in self.families {
            output.push(family.family.code());
            match family.profile {
                Some(profile) => {
                    output.push(1);
                    output.extend_from_slice(&<[u8; 2]>::from(profile));
                }
                None => {
                    output.push(0);
                    output.extend_from_slice(&[0, 0]);
                }
            }
            output.extend_from_slice(&family.row_count.to_be_bytes());
            output.extend_from_slice(&family.tree_root);
            output.extend_from_slice(&family.family_root);
        }
        output
    }

    /// Exact descriptor for one family.
    #[must_use]
    pub const fn family(&self, family: RowFamily) -> SemanticTypedPlaneFamilyRootV3 {
        self.families[family.code() as usize - 1]
    }

    /// Root commitment over the seven ordered family roots.
    #[must_use]
    pub const fn root(&self) -> [u8; 32] {
        self.root
    }

    /// Ordered family descriptors, including explicit empty roots.
    #[must_use]
    pub const fn families(&self) -> &[SemanticTypedPlaneFamilyRootV3; FAMILY_COUNT] {
        &self.families
    }

    /// Checks all seven local tree roots and exact family counts against the
    /// supplied immutable trees. This is structural admission only; semantic
    /// row payloads and cross-family references still require aggregate proof.
    pub fn admit_indexes(
        &self,
        indexes: &[SemanticTypedPlaneFamilyIndexV3; FAMILY_COUNT],
    ) -> Result<(), SemanticTypedPlaneIndexV3Error> {
        let observed = SemanticTypedPlaneIndexCatalogV3::from_indexes(indexes)?;
        if observed != *self {
            return Err(SemanticTypedPlaneIndexV3Error::RootMismatch);
        }
        Ok(())
    }

    /// Opens one family tree selected from this exact seven-family catalog.
    ///
    /// The catalog remains an untrusted structural claim until authenticated
    /// by its enclosing generation contract. This API binds family selection
    /// to one of its seven ordered descriptors.
    pub fn open_family<'loader, L: TreeNodeLoader<SemanticTypedPlaneRowRelationV3>>(
        &self,
        loader: &'loader L,
        family: RowFamily,
        root_bytes: &[u8],
    ) -> Result<LazySemanticTypedPlaneFamilyIndexV3<'loader, L>, SemanticTypedPlaneIndexV3Error>
    where
        L::Error: std::fmt::Display,
    {
        LazySemanticTypedPlaneFamilyIndexV3::open(loader, self.family(family), root_bytes)
    }

    fn from_indexes(
        indexes: &[SemanticTypedPlaneFamilyIndexV3; FAMILY_COUNT],
    ) -> Result<Self, SemanticTypedPlaneIndexV3Error> {
        let mut families =
            [SemanticTypedPlaneFamilyRootV3::new(RowFamily::Core, None, 0, [0; 32]); FAMILY_COUNT];
        for (index, family_index) in indexes.iter().enumerate() {
            let expected = RowFamily::ALL[index];
            if family_index.family != expected
                || matches!(expected, RowFamily::LanguageExtensions)
                    != family_index.profile.is_some()
            {
                return Err(SemanticTypedPlaneIndexV3Error::FamilyOrder);
            }
            families[index] = family_index.descriptor();
        }
        Self::from_untrusted_claims(families)
    }
}

/// Seven immutable structural family indexes and their bounded V3 catalog.
///
/// This value contains no generation or compiler-authority proof.
pub struct SemanticTypedPlaneIndexV3 {
    families: [SemanticTypedPlaneFamilyIndexV3; FAMILY_COUNT],
    catalog: SemanticTypedPlaneIndexCatalogV3,
}

impl SemanticTypedPlaneIndexV3 {
    /// Joins seven family trees in canonical order and derives their catalog.
    pub fn from_families(
        families: [SemanticTypedPlaneFamilyIndexV3; FAMILY_COUNT],
    ) -> Result<Self, SemanticTypedPlaneIndexV3Error> {
        let catalog = SemanticTypedPlaneIndexCatalogV3::from_indexes(&families)?;
        Ok(Self { families, catalog })
    }

    /// Claim-only catalog object for history metadata and generation roots.
    #[must_use]
    pub const fn catalog(&self) -> &SemanticTypedPlaneIndexCatalogV3 {
        &self.catalog
    }

    /// Borrows the immutable tree for a closed family.
    #[must_use]
    pub const fn family(&self, family: RowFamily) -> &SemanticTypedPlaneFamilyIndexV3 {
        &self.families[family.code() as usize - 1]
    }

    /// Validates an untrusted catalog claim against all seven exact trees.
    pub fn admit_catalog(
        &self,
        claim: &SemanticTypedPlaneIndexCatalogV3,
    ) -> Result<(), SemanticTypedPlaneIndexV3Error> {
        claim.admit_indexes(&self.families)
    }
}

/// Bounded cold view of one persisted family row tree.
pub struct LazySemanticTypedPlaneFamilyIndexV3<
    'loader,
    L: TreeNodeLoader<SemanticTypedPlaneRowRelationV3>,
> {
    family: RowFamily,
    profile: Option<LanguageProfile>,
    tree: LazyTree<'loader, SemanticTypedPlaneRowRelationV3, L>,
    loader: &'loader L,
}

impl<'loader, L: TreeNodeLoader<SemanticTypedPlaneRowRelationV3>>
    LazySemanticTypedPlaneFamilyIndexV3<'loader, L>
{
    /// Admits exact root-node bytes against the catalog's typed root/count.
    pub(crate) fn open(
        loader: &'loader L,
        descriptor: SemanticTypedPlaneFamilyRootV3,
        root_bytes: &[u8],
    ) -> Result<Self, SemanticTypedPlaneIndexV3Error>
    where
        L::Error: std::fmt::Display,
    {
        if root_bytes.len() > MAX_TYPED_PLANE_V3_NODE_BYTES {
            return Err(SemanticTypedPlaneIndexV3Error::NodeTooLarge);
        }
        let claim = UntrustedId::from_wire(
            &descriptor.tree_root,
            IdContext::relation::<SemanticTypedPlaneRowRelationV3>(),
        )
        .map_err(|_| SemanticTypedPlaneIndexV3Error::RootMismatch)?;
        let root = PersistedTreeRoot::admit(claim, root_bytes)
            .map_err(SemanticTypedPlaneIndexV3Error::RootAdmission)?;
        if root.evidence().row_count() != descriptor.row_count {
            return Err(SemanticTypedPlaneIndexV3Error::RowCountMismatch);
        }
        if root
            .evidence()
            .node()
            .first_key()
            .is_some_and(|key| key.family() != descriptor.family)
        {
            return Err(SemanticTypedPlaneIndexV3Error::RowFamilyMismatch);
        }
        Ok(Self {
            family: descriptor.family,
            profile: descriptor.profile,
            tree: LazyTree::from_admitted(loader, root),
            loader,
        })
    }

    /// Reads at most `limit` row references from an inclusive start key and
    /// exclusive end key. Authenticated child anchors prune subtrees outside
    /// either bound, so a narrow end does not load following pages.
    pub fn page(
        &self,
        start: Option<StableRowKey>,
        end: Option<StableRowKey>,
        limit: usize,
    ) -> Result<SemanticTypedPlaneIndexPageV3, SemanticTypedPlaneIndexV3Error>
    where
        L::Error: std::fmt::Display,
    {
        if limit > MAX_TYPED_PLANE_V3_PAGE_ROWS {
            return Err(SemanticTypedPlaneIndexV3Error::PageTooLarge);
        }
        if start.is_some_and(|key| key.family() != self.family)
            || end.is_some_and(|key| key.family() != self.family)
            || start.zip(end).is_some_and(|(start, end)| start > end)
        {
            return Err(SemanticTypedPlaneIndexV3Error::InvalidRange);
        }
        if limit == 0 || start.zip(end).is_some_and(|(start, end)| start == end) {
            return Ok(SemanticTypedPlaneIndexPageV3::empty());
        }
        self.range_page(
            start.unwrap_or_else(|| StableRowKey::new(self.family, [0; 32])),
            false,
            end,
            limit,
        )
    }

    /// Reads the next bounded page after a previously returned continuation
    /// key. Unlike [`Self::page`], `after` is exclusive, so the last row of the
    /// preceding page is not repeated.
    pub fn page_after(
        &self,
        after: Option<StableRowKey>,
        end: Option<StableRowKey>,
        limit: usize,
    ) -> Result<SemanticTypedPlaneIndexPageV3, SemanticTypedPlaneIndexV3Error>
    where
        L::Error: std::fmt::Display,
    {
        if limit > MAX_TYPED_PLANE_V3_PAGE_ROWS {
            return Err(SemanticTypedPlaneIndexV3Error::PageTooLarge);
        }
        if after.is_some_and(|key| key.family() != self.family)
            || end.is_some_and(|key| key.family() != self.family)
        {
            return Err(SemanticTypedPlaneIndexV3Error::InvalidRange);
        }
        if after.zip(end).is_some_and(|(after, end)| after >= end) || limit == 0 {
            return Ok(SemanticTypedPlaneIndexPageV3::empty());
        }
        self.range_page(
            after.unwrap_or_else(|| StableRowKey::new(self.family, [0; 32])),
            after.is_some(),
            end,
            limit,
        )
    }

    fn range_page(
        &self,
        start: StableRowKey,
        start_exclusive: bool,
        end: Option<StableRowKey>,
        limit: usize,
    ) -> Result<SemanticTypedPlaneIndexPageV3, SemanticTypedPlaneIndexV3Error>
    where
        L::Error: std::fmt::Display,
    {
        let mut pending = Vec::new();
        pending
            .try_reserve(1)
            .map_err(|_| SemanticTypedPlaneIndexV3Error::Allocation)?;
        pending.push(PendingDescriptorNodeV3::Root(self.tree.root().clone()));
        let mut entries = Vec::new();
        entries
            .try_reserve(limit.min(MAX_TYPED_PLANE_V3_PAGE_ROWS))
            .map_err(|_| SemanticTypedPlaneIndexV3Error::Allocation)?;
        let mut loaded_nodes = 0_usize;
        let mut visited_nodes = 0_usize;
        let mut examined_rows = 0_usize;
        let mut peak_logical_live_buffer_bytes = 0_usize;
        let mut queued_root_bytes = match pending.first() {
            Some(PendingDescriptorNodeV3::Root(root)) => root.bytes().len(),
            _ => 0,
        };
        peak_logical_live_buffer_bytes = peak_logical_live_buffer_bytes.max(
            logical_live_buffer_bytes(&pending, entries.capacity(), queued_root_bytes, 0, 0, 0),
        );

        while let Some(pending_node) = pending.pop() {
            visited_nodes = visited_nodes
                .checked_add(1)
                .ok_or(SemanticTypedPlaneIndexV3Error::Overflow)?;
            if visited_nodes > MAX_TYPED_PLANE_V3_PAGE_NODES {
                return Err(SemanticTypedPlaneIndexV3Error::PageNodeLimit);
            }
            let node = match pending_node {
                PendingDescriptorNodeV3::Root(root) => {
                    queued_root_bytes = 0;
                    root
                }
                PendingDescriptorNodeV3::Child {
                    claim,
                    first_key,
                    row_count,
                    level,
                } => {
                    loaded_nodes = loaded_nodes
                        .checked_add(1)
                        .ok_or(SemanticTypedPlaneIndexV3Error::Overflow)?;
                    let loaded = self.loader.load(claim).map_err(|error| {
                        SemanticTypedPlaneIndexV3Error::LazyTree(error.to_string())
                    })?;
                    if loaded.root().as_bytes() != claim.as_bytes()
                        || loaded.row_count() != row_count
                        || loaded.node().level() != level
                        || loaded.node().first_key() != Some(&first_key)
                    {
                        return Err(SemanticTypedPlaneIndexV3Error::Node(
                            NodeError::AnchorMismatch,
                        ));
                    }
                    loaded
                }
            };
            peak_logical_live_buffer_bytes =
                peak_logical_live_buffer_bytes.max(logical_live_buffer_bytes(
                    &pending,
                    entries.capacity(),
                    0,
                    node.bytes().len(),
                    0,
                    0,
                ));

            if node.node().level() == 0 {
                let rows = node
                    .leaf_entries()
                    .map_err(SemanticTypedPlaneIndexV3Error::Node)?;
                examined_rows = examined_rows
                    .checked_add(rows.len())
                    .ok_or(SemanticTypedPlaneIndexV3Error::Overflow)?;
                peak_logical_live_buffer_bytes =
                    peak_logical_live_buffer_bytes.max(logical_live_buffer_bytes(
                        &pending,
                        entries.capacity(),
                        0,
                        node.bytes().len(),
                        rows.capacity(),
                        0,
                    ));
                for (key, value) in rows {
                    if key < start || (start_exclusive && key == start) {
                        continue;
                    }
                    if end.is_some_and(|end| key >= end) {
                        break;
                    }
                    if key.family() != self.family {
                        return Err(SemanticTypedPlaneIndexV3Error::RowFamilyMismatch);
                    }
                    entries.push((key, value));
                    if entries.len() == limit {
                        let next = entries.last().map(|(key, _)| *key);
                        return Ok(SemanticTypedPlaneIndexPageV3 {
                            entries,
                            next,
                            work: SemanticTypedPlanePageWorkV3 {
                                loaded_nodes,
                                visited_nodes,
                                examined_rows,
                                peak_logical_live_buffer_bytes,
                            },
                        });
                    }
                }
            } else {
                let children = node
                    .child_summaries()
                    .map_err(SemanticTypedPlaneIndexV3Error::Node)?;
                peak_logical_live_buffer_bytes =
                    peak_logical_live_buffer_bytes.max(logical_live_buffer_bytes(
                        &pending,
                        entries.capacity(),
                        0,
                        node.bytes().len(),
                        0,
                        children.capacity(),
                    ));
                for index in (0..children.len()).rev() {
                    let child = &children[index];
                    let before_end = end.is_none_or(|end| child.first_key < end);
                    let after_start = children
                        .get(index + 1)
                        .is_none_or(|next| start < next.first_key);
                    if !before_end || !after_start {
                        continue;
                    }
                    if pending.len() >= MAX_TYPED_PLANE_V3_PAGE_NODES {
                        return Err(SemanticTypedPlaneIndexV3Error::PageNodeLimit);
                    }
                    let bytes = child.commitment.to_bytes();
                    let claim = UntrustedId::from_wire(
                        &bytes,
                        IdContext::relation::<SemanticTypedPlaneRowRelationV3>(),
                    )
                    .map_err(|_| SemanticTypedPlaneIndexV3Error::RootMismatch)?;
                    pending
                        .try_reserve(1)
                        .map_err(|_| SemanticTypedPlaneIndexV3Error::Allocation)?;
                    pending.push(PendingDescriptorNodeV3::Child {
                        claim,
                        first_key: child.first_key,
                        row_count: child.row_count,
                        level: child.level,
                    });
                    peak_logical_live_buffer_bytes =
                        peak_logical_live_buffer_bytes.max(logical_live_buffer_bytes(
                            &pending,
                            entries.capacity(),
                            0,
                            node.bytes().len(),
                            0,
                            children.capacity(),
                        ));
                }
            }
        }
        Ok(SemanticTypedPlaneIndexPageV3 {
            entries,
            next: None,
            work: SemanticTypedPlanePageWorkV3 {
                loaded_nodes,
                visited_nodes,
                examined_rows,
                peak_logical_live_buffer_bytes,
            },
        })
    }

    /// Prepares a bounded sorted multi-key path-copy edit. The returned
    /// immutable nodes are the exact storage writes; publication must persist
    /// them and their row-payload object closure before advancing history.
    pub fn prepare_update_bounded(
        &self,
        changes: &[TreeChange<SemanticTypedPlaneRowRelationV3>],
        budget: LazyTreeUpdateBudget,
    ) -> Result<SemanticTypedPlanePreparedUpdateV3, SemanticTypedPlaneIndexV3Error>
    where
        L::Error: std::fmt::Display,
    {
        if changes
            .iter()
            .any(|change| change.key.family() != self.family)
        {
            return Err(SemanticTypedPlaneIndexV3Error::RowFamilyMismatch);
        }
        let update = self
            .tree
            .prepare_update_bounded(changes, budget)
            .map_err(|error| SemanticTypedPlaneIndexV3Error::LazyTree(error.to_string()))?;
        Ok(SemanticTypedPlanePreparedUpdateV3 {
            family: self.family,
            profile: self.profile,
            update,
        })
    }

    /// Walks every authenticated node and leaf reference under this family
    /// root. The caller's mark sink receives exact node bytes and row-payload
    /// object IDs, so it can build the commit's GC closure without a duplicate
    /// bridge vector. The walk checks every child identity, level, anchor, and
    /// row count, then compares the exact row census with the root claim.
    /// Callback effects are provisional until this method returns `Ok`; a
    /// sink that can trigger reclamation must stage marks and publish them only
    /// after the complete walk succeeds.
    pub fn visit_closure(
        &self,
        limits: SemanticTypedPlaneClosureLimitsV3,
        mut visit_node: impl FnMut([u8; 32], &[u8]),
        mut visit_row: impl FnMut(StableRowKey, SemanticRowPayloadClaimV3),
    ) -> Result<SemanticTypedPlaneIndexClosureV3, SemanticTypedPlaneIndexV3Error>
    where
        L::Error: std::fmt::Display,
    {
        let root = self.tree.root().clone();
        let expected_rows = root.row_count();
        let mut pending = Vec::new();
        pending
            .try_reserve(1)
            .map_err(|_| SemanticTypedPlaneIndexV3Error::Allocation)?;
        pending.push(PendingDescriptorNodeV3::Root(root));
        let mut node_count = 0_u64;
        let mut row_count = 0_u64;
        let mut node_bytes = 0_u64;
        let mut peak_logical_live_buffer_bytes = match pending.first() {
            Some(PendingDescriptorNodeV3::Root(root)) => {
                logical_live_buffer_bytes(&pending, 0, root.bytes().len(), 0, 0, 0)
            }
            _ => 0,
        };
        while let Some(pending_node) = pending.pop() {
            node_count = node_count
                .checked_add(1)
                .ok_or(SemanticTypedPlaneIndexV3Error::Overflow)?;
            let node = match pending_node {
                PendingDescriptorNodeV3::Root(root) => root,
                PendingDescriptorNodeV3::Child {
                    claim,
                    first_key,
                    row_count,
                    level,
                } => {
                    let loaded = self.loader.load(claim).map_err(|error| {
                        SemanticTypedPlaneIndexV3Error::LazyTree(error.to_string())
                    })?;
                    if loaded.root().as_bytes() != claim.as_bytes()
                        || loaded.row_count() != row_count
                        || loaded.node().level() != level
                        || loaded.node().first_key() != Some(&first_key)
                    {
                        return Err(SemanticTypedPlaneIndexV3Error::Node(
                            NodeError::AnchorMismatch,
                        ));
                    }
                    loaded
                }
            };
            peak_logical_live_buffer_bytes = peak_logical_live_buffer_bytes.max(
                logical_live_buffer_bytes(&pending, 0, 0, node.bytes().len(), 0, 0),
            );
            let bytes = node.bytes();
            let byte_len =
                u64::try_from(bytes.len()).map_err(|_| SemanticTypedPlaneIndexV3Error::Overflow)?;
            node_bytes = node_bytes
                .checked_add(byte_len)
                .ok_or(SemanticTypedPlaneIndexV3Error::Overflow)?;
            if node_count > limits.max_nodes
                || node_count > MAX_TYPED_PLANE_V3_CLOSURE_NODES
                || byte_len > u64::try_from(MAX_TYPED_PLANE_V3_NODE_BYTES).unwrap_or(u64::MAX)
                || node_bytes > limits.max_node_bytes
            {
                return Err(SemanticTypedPlaneIndexV3Error::ClosureLimit);
            }
            visit_node(node.root().to_bytes(), bytes);
            if node.node().level() == 0 {
                let rows = node
                    .leaf_entries()
                    .map_err(SemanticTypedPlaneIndexV3Error::Node)?;
                peak_logical_live_buffer_bytes =
                    peak_logical_live_buffer_bytes.max(logical_live_buffer_bytes(
                        &pending,
                        0,
                        0,
                        node.bytes().len(),
                        rows.capacity(),
                        0,
                    ));
                for (key, value) in rows {
                    if key.family() != self.family {
                        return Err(SemanticTypedPlaneIndexV3Error::RowFamilyMismatch);
                    }
                    row_count = row_count
                        .checked_add(1)
                        .ok_or(SemanticTypedPlaneIndexV3Error::Overflow)?;
                    if row_count > limits.max_rows || row_count > MAX_TYPED_PLANE_V3_CLOSURE_ROWS {
                        return Err(SemanticTypedPlaneIndexV3Error::ClosureLimit);
                    }
                    visit_row(key, value);
                }
            } else {
                let children = node
                    .child_summaries()
                    .map_err(SemanticTypedPlaneIndexV3Error::Node)?;
                peak_logical_live_buffer_bytes =
                    peak_logical_live_buffer_bytes.max(logical_live_buffer_bytes(
                        &pending,
                        0,
                        0,
                        node.bytes().len(),
                        0,
                        children.capacity(),
                    ));
                let child_capacity = children.capacity();
                for child in children.into_iter().rev() {
                    if pending.len() >= limits.max_pending_nodes {
                        return Err(SemanticTypedPlaneIndexV3Error::ClosureLimit);
                    }
                    let child_bytes = child.commitment.to_bytes();
                    let claim = UntrustedId::from_wire(
                        &child_bytes,
                        IdContext::relation::<SemanticTypedPlaneRowRelationV3>(),
                    )
                    .map_err(|_| SemanticTypedPlaneIndexV3Error::RootMismatch)?;
                    pending
                        .try_reserve(1)
                        .map_err(|_| SemanticTypedPlaneIndexV3Error::Allocation)?;
                    pending.push(PendingDescriptorNodeV3::Child {
                        claim,
                        first_key: child.first_key,
                        row_count: child.row_count,
                        level: child.level,
                    });
                    peak_logical_live_buffer_bytes =
                        peak_logical_live_buffer_bytes.max(logical_live_buffer_bytes(
                            &pending,
                            0,
                            0,
                            node.bytes().len(),
                            0,
                            child_capacity,
                        ));
                }
            }
        }
        if row_count != expected_rows {
            return Err(SemanticTypedPlaneIndexV3Error::RowCountMismatch);
        }
        Ok(SemanticTypedPlaneIndexClosureV3 {
            family_root: self.tree.root().root().to_bytes(),
            node_count,
            row_count,
            node_bytes,
            peak_logical_live_buffer_bytes,
        })
    }
}

/// One bounded lazy family edit and its exact changed canonical pages.
///
/// The storage adapter persists `changed_pages()` and row objects before it
/// publishes `target_descriptor()` in the next catalog. Unchanged child
/// commitments remain reachable through the target pages and are not copied.
pub struct SemanticTypedPlanePreparedUpdateV3 {
    family: RowFamily,
    profile: Option<LanguageProfile>,
    update: LazyPreparedUpdate<SemanticTypedPlaneRowRelationV3>,
}

impl SemanticTypedPlanePreparedUpdateV3 {
    /// Target family-root descriptor for the next seven-root catalog.
    #[must_use]
    pub fn target_descriptor(&self) -> SemanticTypedPlaneFamilyRootV3 {
        SemanticTypedPlaneFamilyRootV3::new(
            self.family,
            self.profile,
            self.update.target().row_count(),
            self.update.target().root().to_bytes(),
        )
    }

    /// Exact content-addressed pages emitted by path-copy update.
    pub fn changed_pages(&self) -> impl Iterator<Item = SemanticTypedPlaneNodeV3<'_>> + '_ {
        self.update
            .changed_nodes()
            .iter()
            .map(|node| SemanticTypedPlaneNodeV3 {
                id: node.commitment().to_bytes(),
                bytes: node.as_bytes(),
                row_count: node.row_count(),
                level: node.level(),
            })
    }

    /// Authenticated update work and byte counters from the lazy tree kernel.
    #[must_use]
    pub const fn work(&self) -> LazyTreeWork {
        self.update.work()
    }

    /// Checked target root handoff for cold reopening after storage admission.
    #[must_use]
    pub fn target_root(&self) -> PersistedTreeRoot<SemanticTypedPlaneRowRelationV3> {
        self.update.target_root()
    }

    /// Consumes the adapter wrapper and returns the backend-version update.
    #[must_use]
    pub fn into_backend_update(self) -> LazyPreparedUpdate<SemanticTypedPlaneRowRelationV3> {
        self.update
    }
}

/// One bounded cold row page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneIndexPageV3 {
    entries: Vec<(StableRowKey, SemanticRowPayloadClaimV3)>,
    next: Option<StableRowKey>,
    work: SemanticTypedPlanePageWorkV3,
}

impl SemanticTypedPlaneIndexPageV3 {
    fn empty() -> Self {
        Self {
            entries: Vec::new(),
            next: None,
            work: SemanticTypedPlanePageWorkV3::default(),
        }
    }

    /// Rows in ascending stable-key order.
    #[must_use]
    pub fn entries(&self) -> &[(StableRowKey, SemanticRowPayloadClaimV3)] {
        &self.entries
    }

    /// Last returned key when a later page may exist.
    #[must_use]
    pub const fn next(&self) -> Option<StableRowKey> {
        self.next
    }

    /// Exact descriptor-node and leaf-row work for this cold range page.
    #[must_use]
    pub const fn work(&self) -> SemanticTypedPlanePageWorkV3 {
        self.work
    }
}

/// Measured bounded work for one cold range page.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemanticTypedPlanePageWorkV3 {
    /// Child descriptor nodes fetched through the loader; the admitted root is excluded.
    pub loaded_nodes: usize,
    /// Total nodes examined, including the already admitted root.
    pub visited_nodes: usize,
    /// Rows decoded from touched leaves, including keys outside the exact range.
    pub examined_rows: usize,
    /// Peak additional logical live buffer bytes accounted by this traversal.
    ///
    /// Counts actual `Vec` capacities, active canonical page bytes, and
    /// inline element slots; the returned page's output capacity is included.
    /// Excludes the retained `LazyTree` root, decoder re-encoding scratch,
    /// allocator overhead, loader/cache state, and process RSS.
    pub peak_logical_live_buffer_bytes: usize,
}

enum PendingDescriptorNodeV3 {
    Root(CheckedCanonicalRoot<SemanticTypedPlaneRowRelationV3>),
    Child {
        claim: UntrustedId<SemanticTypedPlaneRowRelationV3>,
        first_key: StableRowKey,
        row_count: u64,
        level: u16,
    },
}

/// Estimates the operation's explicitly owned live buffers from their actual
/// vector capacities and the active canonical page's exact byte length.
///
/// This is an accounted logical buffer counter, not a complete heap or RSS
/// measurement: decoder re-encoding scratch, allocator metadata, loader
/// internals, callback state, the retained `LazyTree` root, and unrelated
/// storage are outside its accounting boundary.
fn logical_live_buffer_bytes(
    pending: &Vec<PendingDescriptorNodeV3>,
    output_capacity: usize,
    queued_root_bytes: usize,
    active_node_bytes: usize,
    leaf_row_capacity: usize,
    branch_child_capacity: usize,
) -> usize {
    let mut bytes = pending
        .capacity()
        .saturating_mul(std::mem::size_of::<PendingDescriptorNodeV3>());
    bytes = bytes.saturating_add(
        output_capacity
            .saturating_mul(std::mem::size_of::<(StableRowKey, SemanticRowPayloadClaimV3)>()),
    );
    bytes = bytes.saturating_add(queued_root_bytes);
    if active_node_bytes != 0 {
        bytes = bytes.saturating_add(active_node_bytes);
        bytes = bytes.saturating_add(std::mem::size_of::<
            CheckedCanonicalRoot<SemanticTypedPlaneRowRelationV3>,
        >());
    }
    bytes = bytes.saturating_add(
        leaf_row_capacity
            .saturating_mul(std::mem::size_of::<(StableRowKey, SemanticRowPayloadClaimV3)>()),
    );
    bytes.saturating_add(branch_child_capacity.saturating_mul(std::mem::size_of::<
        backend_version::CommittedChild<SemanticTypedPlaneRowRelationV3>,
    >()))
}

/// Structural tree and catalog admission failures for V3.
#[derive(Debug, Error)]
pub enum SemanticTypedPlaneIndexV3Error {
    /// Catalog magic or fixed revision was unsupported.
    #[error("invalid V3 typed-plane index catalog header")]
    Magic,
    /// Catalog wire revision was unsupported.
    #[error("unsupported V3 typed-plane index catalog revision")]
    Version,
    /// Catalog size exceeds its fixed bound.
    #[error("V3 typed-plane index catalog exceeds its byte bound")]
    CatalogTooLarge,
    /// Catalog has a missing or unexpected family entry.
    #[error("V3 typed-plane index catalog must contain seven family roots")]
    FamilyCount,
    /// Family code was outside the closed seven-family set.
    #[error("V3 typed-plane index catalog has an unknown family code")]
    FamilyTag,
    /// Family order or profile assignment was not canonical.
    #[error("V3 typed-plane family order or profile is invalid")]
    FamilyProfile,
    /// Family slots were not in canonical order.
    #[error("V3 typed-plane family roots are not in canonical order")]
    FamilyOrder,
    /// Descriptor or catalog root did not match its exact canonical fields.
    #[error("V3 typed-plane index root claim did not match")]
    RootMismatch,
    /// A persisted relation root failed canonical typed admission.
    #[error("V3 typed-plane row-tree root failed admission: {0}")]
    RootAdmission(#[source] CanonicalRootAdmissionError),
    /// A loaded tree root count differed from its family descriptor.
    #[error("V3 typed-plane row-tree count did not match its family descriptor")]
    RowCountMismatch,
    /// One row key named a family other than its containing family tree.
    #[error("V3 typed-plane row key crossed its family root")]
    RowFamilyMismatch,
    /// A requested stable-key range was reversed or crossed family roots.
    #[error("invalid V3 typed-plane row-key range")]
    InvalidRange,
    /// The requested cold row page exceeds the bounded page limit.
    #[error("V3 typed-plane row page exceeds its maximum")]
    PageTooLarge,
    /// A cold bounded range would exceed its explicit descriptor-page work cap.
    #[error("V3 typed-plane row range exceeds its descriptor-page bound")]
    PageNodeLimit,
    /// A canonical node exceeds the bounded root-node size.
    #[error("V3 typed-plane root node exceeds its maximum size")]
    NodeTooLarge,
    /// The bounded lazy tree rejected the page or edit.
    #[error("V3 typed-plane lazy row tree failed: {0}")]
    LazyTree(String),
    /// A leaf or branch node failed its canonical grammar or child checks.
    #[error("V3 typed-plane tree node failed validation: {0}")]
    Node(#[source] NodeError),
    /// A complete closure traversal exceeded an explicit node/row/byte bound.
    #[error("V3 typed-plane tree closure exceeded its bound")]
    ClosureLimit,
    /// A sorted builder repeated or reordered a stable row key.
    #[error("V3 typed-plane row keys must be strictly increasing")]
    UnsortedOrDuplicate,
    /// Builder row count exceeded its explicit maximum.
    #[error("V3 typed-plane row count exceeded its bound")]
    RowLimit,
    /// Builder payload byte sum exceeded its explicit maximum.
    #[error("V3 typed-plane payload bytes exceeded their bound")]
    PayloadByteLimit,
    /// Fixed-width or accumulated row accounting overflowed.
    #[error("V3 typed-plane row accounting overflowed")]
    Overflow,
    /// Bounded scratch allocation failed.
    #[error("V3 typed-plane scratch allocation failed")]
    Allocation,
    /// Existing backend-version tree construction failed.
    #[error(transparent)]
    Tree(#[from] backend_version::TreeError),
    /// The wire ended before a fixed-width field was complete.
    #[error("truncated V3 typed-plane index catalog")]
    Truncated,
    /// The wire included bytes beyond its final canonical family descriptor.
    #[error("trailing bytes in V3 typed-plane index catalog")]
    TrailingBytes,
    /// One untrusted payload claim used an invalid fixed-width encoding.
    #[error(transparent)]
    PayloadClaim(#[from] StableRowIndexError),
}

/// A compact claim that a descriptor tree has passed structural closure
/// verification. It does not prove semantic payloads or cross-family closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneIndexClosureV3 {
    family_root: [u8; 32],
    node_count: u64,
    row_count: u64,
    node_bytes: u64,
    peak_logical_live_buffer_bytes: usize,
}

impl SemanticTypedPlaneIndexClosureV3 {
    /// Family tree root whose complete node closure was traversed.
    #[must_use]
    pub const fn family_root(self) -> [u8; 32] {
        self.family_root
    }

    /// Exact row count found below the family root.
    #[must_use]
    pub const fn row_count(self) -> u64 {
        self.row_count
    }

    /// Number of descriptor-tree nodes in the verified closure.
    #[must_use]
    pub const fn node_count(self) -> u64 {
        self.node_count
    }

    /// Number of row-payload object references found in leaf pages.
    #[must_use]
    pub const fn row_reference_count(self) -> u64 {
        self.row_count
    }

    /// Total canonical bytes across descriptor-tree pages.
    #[must_use]
    pub const fn node_bytes(self) -> u64 {
        self.node_bytes
    }

    /// Peak additional logical live buffer bytes accounted by the closure walk.
    ///
    /// Counts actual `Vec` capacities, active canonical page bytes, and
    /// inline element slots. Excludes decoder re-encoding scratch, allocator
    /// overhead, loader/cache state, callback state, the retained `LazyTree`
    /// root, and process RSS.
    #[must_use]
    pub const fn peak_logical_live_buffer_bytes(self) -> usize {
        self.peak_logical_live_buffer_bytes
    }
}

/// Resource bounds for one cold tree-closure admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneClosureLimitsV3 {
    /// Maximum descriptor nodes to visit.
    pub max_nodes: u64,
    /// Maximum row references to visit.
    pub max_rows: u64,
    /// Maximum sum of canonical descriptor-node bytes.
    pub max_node_bytes: u64,
    /// Maximum pending sibling nodes held by the traversal stack.
    pub max_pending_nodes: usize,
}

impl SemanticTypedPlaneClosureLimitsV3 {
    /// Creates explicit node, row, byte, and scratch bounds.
    #[must_use]
    pub const fn new(
        max_nodes: u64,
        max_rows: u64,
        max_node_bytes: u64,
        max_pending_nodes: usize,
    ) -> Self {
        Self {
            max_nodes,
            max_rows,
            max_node_bytes,
            max_pending_nodes,
        }
    }
}

fn validate_family_roots(
    families: &[SemanticTypedPlaneFamilyRootV3; FAMILY_COUNT],
) -> Result<(), SemanticTypedPlaneIndexV3Error> {
    for (index, family) in families.iter().enumerate() {
        let expected = RowFamily::ALL[index];
        if family.family != expected
            || matches!(expected, RowFamily::LanguageExtensions) != family.profile.is_some()
        {
            return Err(SemanticTypedPlaneIndexV3Error::FamilyOrder);
        }
        if family_root(
            family.family,
            family.profile,
            family.row_count,
            &family.tree_root,
        ) != family.family_root
        {
            return Err(SemanticTypedPlaneIndexV3Error::RootMismatch);
        }
    }
    Ok(())
}

fn family_root(
    family: RowFamily,
    profile: Option<LanguageProfile>,
    row_count: u64,
    tree_root: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(FAMILY_ROOT_DOMAIN);
    hasher.update(&SEMANTIC_TYPED_PLANE_INDEX_CATALOG_V3_WIRE_REVISION.to_be_bytes());
    hasher.update(&[family.code()]);
    match profile {
        Some(profile) => {
            hasher.update(&[1]);
            hasher.update(&<[u8; 2]>::from(profile));
        }
        None => {
            hasher.update(&[0, 0, 0]);
        }
    }
    hasher.update(&row_count.to_be_bytes());
    hasher.update(tree_root);
    *hasher.finalize().as_bytes()
}

fn catalog_root(families: &[SemanticTypedPlaneFamilyRootV3; FAMILY_COUNT]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(CATALOG_ROOT_DOMAIN);
    hasher.update(&SEMANTIC_TYPED_PLANE_INDEX_CATALOG_V3_WIRE_REVISION.to_be_bytes());
    for family in families {
        hasher.update(&family.family_root);
    }
    *hasher.finalize().as_bytes()
}

struct V3Reader<'bytes> {
    remaining: &'bytes [u8],
}

#[cfg(test)]
mod tests;

impl<'bytes> V3Reader<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn take(&mut self, size: usize) -> Result<&'bytes [u8], SemanticTypedPlaneIndexV3Error> {
        if self.remaining.len() < size {
            return Err(SemanticTypedPlaneIndexV3Error::Truncated);
        }
        let (head, tail) = self.remaining.split_at(size);
        self.remaining = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, SemanticTypedPlaneIndexV3Error> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(SemanticTypedPlaneIndexV3Error::Truncated)
    }

    fn u16(&mut self) -> Result<u16, SemanticTypedPlaneIndexV3Error> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| SemanticTypedPlaneIndexV3Error::Truncated)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, SemanticTypedPlaneIndexV3Error> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| SemanticTypedPlaneIndexV3Error::Truncated)?,
        ))
    }

    fn array32(&mut self) -> Result<[u8; 32], SemanticTypedPlaneIndexV3Error> {
        self.take(32)?
            .try_into()
            .map_err(|_| SemanticTypedPlaneIndexV3Error::Truncated)
    }

    fn finish(self) -> Result<(), SemanticTypedPlaneIndexV3Error> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(SemanticTypedPlaneIndexV3Error::TrailingBytes)
        }
    }
}
