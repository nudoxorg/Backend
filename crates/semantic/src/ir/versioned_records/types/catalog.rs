use alloc::vec::Vec;

use crate::ir::{SemanticIrPlane, SemanticPlaneKind, SemanticPlaneRecordError};

use super::EXTERNAL_TARGET_TAG;
use super::ROOT_TAG;
use super::plan::TypesClosureSemantics;
use super::wire::parse_types_row_with_reference_limit;

const TYPES_FAMILY_ROOT_DOMAIN: &[u8] = b"backend.semantic.ir.types-family-root.v2\0";

/// Explicit resource ceiling for one standalone Types-family verification.
/// Aggregate verification derives this from its standard or large-package
/// policy and also enforces its stricter complete-inventory budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypesFamilyVerificationLimitsV2 {
    max_payload_bytes: u64,
    max_rows: u64,
    max_references: u64,
}

impl TypesFamilyVerificationLimitsV2 {
    /// Standard bounded local validation policy.
    pub const fn standard() -> Self {
        Self {
            max_payload_bytes: 64 * 1024 * 1024,
            max_rows: 500_000,
            max_references: 1_000_000,
        }
    }

    /// Higher bounded tier for larger semantic packages.
    pub const fn large_package() -> Self {
        Self {
            max_payload_bytes: 512 * 1024 * 1024,
            max_rows: 2_000_000,
            max_references: 8_000_000,
        }
    }

    pub(crate) const fn bounded(
        max_payload_bytes: u64,
        max_rows: u64,
        max_references: u64,
    ) -> Self {
        Self {
            max_payload_bytes,
            max_rows,
            max_references,
        }
    }
}

/// Closed row domain advertised by a checked Types-family reference catalog.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TypesRowDomainV2 {
    /// Entity-to-optional-type association row.
    EntityRoot,
    /// Any of the nine structural type/list domains.
    TypedNode,
    /// Structural semantic type.
    Type,
    /// Ordered type sequence.
    TypeList,
    /// Ordered tuple or callable parameter/result sequence.
    TupleElements,
    /// Ordered object-member sequence.
    ObjectMembers,
    /// Ordered template-literal pieces.
    TemplateParts,
    /// Ordered arbitrary-byte atom sequence.
    AtomList,
    /// Ordered type-parameter sequence.
    TypeParameters,
    /// Ordered type-parameter bounds.
    TypeParameterBounds,
    /// Ordered Rust free predicates.
    FreePredicates,
    /// Exact arbitrary-byte atom.
    Atom,
    /// Exact external target identity and payload.
    ExternalTarget,
}

/// A typed row reference extracted from a verified row.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TypesReferenceV2 {
    /// Required target row domain.
    pub domain: TypesRowDomainV2,
    /// Stable target key.
    pub key: [u8; 32],
}

/// Locally checked Types family, safe to pass to the all-family V2 proof.
///
/// The catalog contains all stable row keys and cross-family declaration
/// references. Every intra-Types reference has already been resolved and every
/// row payload has passed the independent strict decoder.
pub struct CheckedTypesFamilyV2 {
    row_keys: alloc::boxed::Box<[[u8; 32]]>,
    row_domains: alloc::boxed::Box<[TypesRowDomainV2]>,
    edge_offsets: alloc::boxed::Box<[usize]>,
    row_edges: alloc::boxed::Box<[TypesReferenceV2]>,
    root_identities: alloc::boxed::Box<[[u8; 32]]>,
    root_type_presence: alloc::boxed::Box<[([u8; 32], bool)]>,
    declaration_references: alloc::boxed::Box<[[u8; 32]]>,
    external_target_keys: alloc::boxed::Box<[[u8; 32]]>,
    local_root: [u8; 32],
    row_count: u64,
    reference_count: u64,
}

impl CheckedTypesFamilyV2 {
    /// Strictly admits the complete merged record sequence for one Types plane.
    ///
    /// Input keys must be globally strictly ascending. All typed/list/atom/
    /// external references must resolve inside the supplied family; declaration
    /// identities remain for the aggregate proof to bind to Core.
    pub fn from_records<'bytes>(
        records: impl IntoIterator<Item = ([u8; 32], u8, &'bytes [u8])>,
    ) -> Result<Self, SemanticPlaneRecordError> {
        Self::from_records_with_limits(records, TypesFamilyVerificationLimitsV2::standard())
    }

    /// Strictly admits the complete record sequence under an explicit tier.
    pub fn from_records_with_limits<'bytes>(
        records: impl IntoIterator<Item = ([u8; 32], u8, &'bytes [u8])>,
        limits: TypesFamilyVerificationLimitsV2,
    ) -> Result<Self, SemanticPlaneRecordError> {
        let mut row_keys = Vec::new();
        let mut row_domains = Vec::new();
        let mut edge_offsets = Vec::new();
        edge_offsets
            .try_reserve_exact(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        edge_offsets.push(0);
        let mut row_edges = Vec::new();
        let mut root_identities = Vec::new();
        let mut root_type_presence = Vec::new();
        let mut declaration_references = Vec::new();
        let mut previous = None;
        let mut family_hasher = blake3::Hasher::new();
        family_hasher.update(TYPES_FAMILY_ROOT_DOMAIN);
        let mut row_count = 0_u64;
        let mut payload_bytes = 0_u64;
        let mut reference_count = 0_u64;
        for (key, tag, payload) in records {
            if previous.is_some_and(|prior| prior >= key) {
                return Err(SemanticPlaneRecordError::RecordOrder);
            }
            let remaining_references = limits
                .max_references
                .checked_sub(reference_count)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            let row_owner_reference = if tag == ROOT_TAG { 1 } else { 0 };
            let row_reference_limit = remaining_references
                .checked_sub(row_owner_reference)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            let row_reference_limit = usize::try_from(row_reference_limit)
                .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
            let parsed =
                parse_types_row_with_reference_limit(key, tag, payload, row_reference_limit)?;
            let payload_length =
                u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
            payload_bytes = payload_bytes
                .checked_add(payload_length)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            if payload_bytes > limits.max_payload_bytes {
                return Err(SemanticPlaneRecordError::RowTooLarge);
            }
            family_hasher.update(&key);
            family_hasher.update(&[tag]);
            family_hasher.update(&payload_length.to_be_bytes());
            family_hasher.update(payload);
            row_count = row_count
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            if row_count > limits.max_rows {
                return Err(SemanticPlaneRecordError::RowTooLarge);
            }
            let row_references = parsed
                .references
                .len()
                .checked_add(parsed.declaration_references.len())
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            let row_references = row_references
                .checked_add(if parsed.root_identity.is_some() { 1 } else { 0 })
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            reference_count = reference_count
                .checked_add(
                    u64::try_from(row_references)
                        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
                )
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            if reference_count > limits.max_references {
                return Err(SemanticPlaneRecordError::RowTooLarge);
            }
            match (parsed.root_identity, parsed.root_type_present) {
                (Some(identity), Some(present)) => {
                    root_identities
                        .try_reserve(1)
                        .map_err(SemanticPlaneRecordError::Allocation)?;
                    root_type_presence
                        .try_reserve(1)
                        .map_err(SemanticPlaneRecordError::Allocation)?;
                    root_identities.push(identity);
                    root_type_presence.push((identity, present));
                }
                (None, None) => {}
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
            row_edges
                .try_reserve(parsed.references.len())
                .map_err(SemanticPlaneRecordError::Allocation)?;
            row_edges.extend(parsed.references.iter().copied());
            declaration_references
                .try_reserve(parsed.declaration_references.len())
                .map_err(SemanticPlaneRecordError::Allocation)?;
            declaration_references.extend(parsed.declaration_references);
            row_keys
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            row_domains
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            row_keys.push(key);
            row_domains.push(parsed.domain);
            edge_offsets
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            edge_offsets.push(row_edges.len());
            previous = Some(key);
        }
        root_identities.sort_unstable();
        if root_identities.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(SemanticPlaneRecordError::StableKeyCollision);
        }
        root_type_presence.sort_unstable_by_key(|(identity, _)| *identity);
        if root_type_presence
            .windows(2)
            .any(|pair| pair[0].0 == pair[1].0)
        {
            return Err(SemanticPlaneRecordError::StableKeyCollision);
        }
        declaration_references.sort_unstable();
        declaration_references.dedup();
        for reference in &row_edges {
            let observed = row_keys
                .binary_search(&reference.key)
                .ok()
                .and_then(|index| row_domains.get(index));
            let resolved = match reference.domain {
                TypesRowDomainV2::TypedNode => {
                    observed.is_some_and(|domain| domain.is_typed_node())
                }
                domain => observed == Some(&domain),
            };
            if !resolved {
                return Err(SemanticPlaneRecordError::ReaderReference);
            }
        }
        let external_target_count = row_domains
            .iter()
            .filter(|domain| **domain == TypesRowDomainV2::ExternalTarget)
            .count();
        let mut external_target_keys = Vec::new();
        external_target_keys
            .try_reserve_exact(external_target_count)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        for (key, domain) in row_keys.iter().zip(row_domains.iter()) {
            if *domain == TypesRowDomainV2::ExternalTarget {
                external_target_keys.push(*key);
            }
        }
        family_hasher.update(&row_count.to_be_bytes());
        let local_root = *family_hasher.finalize().as_bytes();
        Ok(Self {
            row_keys: row_keys.into_boxed_slice(),
            row_domains: row_domains.into_boxed_slice(),
            edge_offsets: edge_offsets.into_boxed_slice(),
            row_edges: row_edges.into_boxed_slice(),
            root_identities: root_identities.into_boxed_slice(),
            root_type_presence: root_type_presence.into_boxed_slice(),
            declaration_references: declaration_references.into_boxed_slice(),
            external_target_keys: external_target_keys.into_boxed_slice(),
            local_root,
            row_count,
            reference_count,
        })
    }

    /// Declares the normalized reachability semantics accepted by this catalog.
    #[must_use]
    pub const fn semantics(&self) -> TypesClosureSemantics {
        TypesClosureSemantics::NormalizedReachable
    }

    /// Sorted stable keys in the admitted Types family.
    #[must_use]
    pub fn row_keys(&self) -> &[[u8; 32]] {
        &self.row_keys
    }

    /// Declaration identities that have an entity→optional-type root row.
    #[must_use]
    pub fn root_identities(&self) -> &[[u8; 32]] {
        &self.root_identities
    }

    /// Sorted declaration identity and semantic-type-present bit from every root row.
    #[must_use]
    pub fn root_type_presence(&self) -> &[([u8; 32], bool)] {
        &self.root_type_presence
    }

    /// Declaration identities referenced by type nodes and checked against Core by V2.
    #[must_use]
    pub fn declaration_references(&self) -> &[[u8; 32]] {
        &self.declaration_references
    }

    /// Cross-family declaration identities that the aggregate must resolve.
    #[must_use]
    pub fn unresolved_references(&self) -> &[[u8; 32]] {
        &self.declaration_references
    }

    /// Exact external-target stable keys owned by this family.
    #[must_use]
    pub fn external_target_keys(&self) -> &[[u8; 32]] {
        &self.external_target_keys
    }

    /// Local canonical commitment over sorted keys, tags, and exact payloads.
    #[must_use]
    pub const fn local_root(&self) -> &[u8; 32] {
        &self.local_root
    }

    /// Number of checked rows, including roots and explicit empty lists.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Number of exact typed/list/atom/external/declaration references parsed
    /// from the admitted Types rows, before any cross-family set deduplication.
    #[must_use]
    pub const fn reference_count(&self) -> u64 {
        self.reference_count
    }

    /// Resolves one typed/list/atom/external root emitted by another family.
    pub fn require_reference(
        &self,
        reference: TypesReferenceV2,
    ) -> Result<(), SemanticPlaneRecordError> {
        self.row_index_for_reference(reference).map(|_| ())
    }

    /// Proves every Types row is reachable from mandatory roots and extension references.
    ///
    /// The bounded traversal stores one visited byte and at most one pending row index per
    /// admitted row. Adjacency was independently parsed from every record during admission.
    /// The seeds are all entity-root rows, every row in the complete external-target catalog,
    /// and each typed-family reference collected from language-extension rows.
    pub fn verify_reachable_closure(
        &self,
        extension_references: &[TypesReferenceV2],
    ) -> Result<(), SemanticPlaneRecordError> {
        let mut visited = Vec::new();
        visited
            .try_reserve_exact(self.row_keys.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        visited.resize(self.row_keys.len(), 0_u8);
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(self.row_keys.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;

        for identity in self.root_identities.iter().copied() {
            let index = self
                .row_keys
                .binary_search(&identity)
                .map_err(|_| SemanticPlaneRecordError::ReaderReference)?;
            if self.row_domains.get(index) != Some(&TypesRowDomainV2::EntityRoot) {
                return Err(SemanticPlaneRecordError::ReaderReference);
            }
            enqueue_reachable(index, &mut visited, &mut pending);
        }
        for key in self.external_target_keys.iter().copied() {
            let index = self
                .row_keys
                .binary_search(&key)
                .map_err(|_| SemanticPlaneRecordError::ReaderReference)?;
            if self.row_domains.get(index) != Some(&TypesRowDomainV2::ExternalTarget) {
                return Err(SemanticPlaneRecordError::ReaderReference);
            }
            enqueue_reachable(index, &mut visited, &mut pending);
        }
        for reference in extension_references.iter().copied() {
            let index = self.row_index_for_reference(reference)?;
            enqueue_reachable(index, &mut visited, &mut pending);
        }

        while let Some(index) = pending.pop() {
            let start = *self
                .edge_offsets
                .get(index)
                .ok_or(SemanticPlaneRecordError::RowGrammar)?;
            let next_index = index
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            let end = *self
                .edge_offsets
                .get(next_index)
                .ok_or(SemanticPlaneRecordError::RowGrammar)?;
            let edges = self
                .row_edges
                .get(start..end)
                .ok_or(SemanticPlaneRecordError::RowGrammar)?;
            for reference in edges.iter().copied() {
                let target = self.row_index_for_reference(reference)?;
                enqueue_reachable(target, &mut visited, &mut pending);
            }
        }
        if visited.iter().any(|seen| *seen == 0) {
            return Err(SemanticPlaneRecordError::ReaderReference);
        }
        Ok(())
    }

    fn row_index_for_reference(
        &self,
        reference: TypesReferenceV2,
    ) -> Result<usize, SemanticPlaneRecordError> {
        let index = self
            .row_keys
            .binary_search(&reference.key)
            .map_err(|_| SemanticPlaneRecordError::ReaderReference)?;
        let domain = self
            .row_domains
            .get(index)
            .copied()
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        let matches = match reference.domain {
            TypesRowDomainV2::TypedNode => domain.is_typed_node(),
            expected => domain == expected,
        };
        if matches {
            Ok(index)
        } else {
            Err(SemanticPlaneRecordError::ReaderReference)
        }
    }
}

fn enqueue_reachable(index: usize, visited: &mut [u8], pending: &mut Vec<usize>) {
    if visited[index] == 0 {
        visited[index] = 1;
        pending.push(index);
    }
}

/// Validates the Types row sequence from already independently admitted SPIR segments.
pub fn validate_types_family_v2<'bytes>(
    segments: impl IntoIterator<Item = crate::ir::CanonicalSemanticPlaneSegmentView<'bytes>>,
) -> Result<CheckedTypesFamilyV2, SemanticPlaneRecordError> {
    validate_types_family_v2_with_limits(segments, TypesFamilyVerificationLimitsV2::standard())
}

/// Validates a complete Types family under an explicit resource tier.
pub fn validate_types_family_v2_with_limits<'bytes>(
    segments: impl IntoIterator<Item = crate::ir::CanonicalSemanticPlaneSegmentView<'bytes>>,
    limits: TypesFamilyVerificationLimitsV2,
) -> Result<CheckedTypesFamilyV2, SemanticPlaneRecordError> {
    let mut records = Vec::new();
    let mut previous = None;
    let mut row_count = 0_u64;
    let mut payload_bytes = 0_u64;
    for segment in segments {
        if segment.kind() != SemanticPlaneKind::Ir(SemanticIrPlane::Types) {
            return Err(SemanticPlaneRecordError::PlaneKind);
        }
        for record in segment.records() {
            if previous.is_some_and(|prior| prior >= record.key()) {
                return Err(SemanticPlaneRecordError::RecordOrder);
            }
            previous = Some(record.key());
            row_count = row_count
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            let payload_length = u64::try_from(record.payload().len())
                .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
            payload_bytes = payload_bytes
                .checked_add(payload_length)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            if row_count > limits.max_rows || payload_bytes > limits.max_payload_bytes {
                return Err(SemanticPlaneRecordError::RowTooLarge);
            }
            records
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            records.push((record.key(), record.tag(), record.payload()));
        }
    }
    CheckedTypesFamilyV2::from_records_with_limits(records, limits)
}
