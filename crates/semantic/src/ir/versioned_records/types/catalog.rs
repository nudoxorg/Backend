use alloc::vec::Vec;

use crate::ir::{SemanticIrPlane, SemanticPlaneKind, SemanticPlaneRecordError};

use super::EXTERNAL_TARGET_TAG;
use super::plan::TypesClosureSemantics;
use super::wire::parse_types_row;

const TYPES_FAMILY_ROOT_DOMAIN: &[u8] = b"backend.semantic.ir.types-family-root.v2\0";

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
    root_identities: alloc::boxed::Box<[[u8; 32]]>,
    root_type_presence: alloc::boxed::Box<[([u8; 32], bool)]>,
    declaration_references: alloc::boxed::Box<[[u8; 32]]>,
    external_target_keys: alloc::boxed::Box<[[u8; 32]]>,
    local_root: [u8; 32],
    row_count: u64,
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
        let mut row_keys = Vec::new();
        let mut row_domains = Vec::new();
        let mut local_references = Vec::new();
        let mut root_identities = Vec::new();
        let mut root_type_presence = Vec::new();
        let mut declaration_references = Vec::new();
        let mut previous = None;
        let mut family_hasher = blake3::Hasher::new();
        family_hasher.update(TYPES_FAMILY_ROOT_DOMAIN);
        let mut row_count = 0_u64;
        for (key, tag, payload) in records {
            if previous.is_some_and(|prior| prior >= key) {
                return Err(SemanticPlaneRecordError::RecordOrder);
            }
            let parsed = parse_types_row(key, tag, payload)?;
            let payload_length =
                u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
            family_hasher.update(&key);
            family_hasher.update(&[tag]);
            family_hasher.update(&payload_length.to_be_bytes());
            family_hasher.update(payload);
            row_count = row_count
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            match (parsed.root_identity, parsed.root_type_present) {
                (Some(identity), Some(present)) => {
                    root_identities.push(identity);
                    root_type_presence.push((identity, present));
                }
                (None, None) => {}
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
            local_references.extend(parsed.references);
            declaration_references.extend(parsed.declaration_references);
            row_keys
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            row_domains
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            row_keys.push(key);
            row_domains.push(parsed.domain);
            previous = Some(key);
        }
        local_references.sort_unstable();
        local_references.dedup();
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
        for reference in &local_references {
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
        let external_target_keys = row_keys
            .iter()
            .zip(row_domains.iter())
            .filter_map(|(key, domain)| {
                (*domain == TypesRowDomainV2::ExternalTarget).then_some(*key)
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        family_hasher.update(&row_count.to_be_bytes());
        let local_root = *family_hasher.finalize().as_bytes();
        Ok(Self {
            row_keys: row_keys.into_boxed_slice(),
            row_domains: row_domains.into_boxed_slice(),
            root_identities: root_identities.into_boxed_slice(),
            root_type_presence: root_type_presence.into_boxed_slice(),
            declaration_references: declaration_references.into_boxed_slice(),
            external_target_keys,
            local_root,
            row_count,
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

    /// Resolves one typed/list/atom/external root emitted by another family.
    pub fn require_reference(
        &self,
        reference: TypesReferenceV2,
    ) -> Result<(), SemanticPlaneRecordError> {
        let matches = match reference.domain {
            TypesRowDomainV2::TypedNode => self
                .domain_for(reference.key)
                .is_some_and(TypesRowDomainV2::is_typed_node),
            domain => self.domain_for(reference.key) == Some(domain),
        };
        if matches {
            Ok(())
        } else {
            Err(SemanticPlaneRecordError::ReaderReference)
        }
    }

    fn domain_for(&self, key: [u8; 32]) -> Option<TypesRowDomainV2> {
        self.row_keys
            .binary_search(&key)
            .ok()
            .and_then(|index| self.row_domains.get(index).copied())
    }
}

/// Validates the Types row sequence from already independently admitted SPIR segments.
pub fn validate_types_family_v2<'bytes>(
    segments: impl IntoIterator<Item = crate::ir::CanonicalSemanticPlaneSegmentView<'bytes>>,
) -> Result<CheckedTypesFamilyV2, SemanticPlaneRecordError> {
    let mut records = Vec::new();
    let mut previous = None;
    for segment in segments {
        if segment.kind() != SemanticPlaneKind::Ir(SemanticIrPlane::Types) {
            return Err(SemanticPlaneRecordError::PlaneKind);
        }
        for record in segment.records() {
            if previous.is_some_and(|prior| prior >= record.key()) {
                return Err(SemanticPlaneRecordError::RecordOrder);
            }
            previous = Some(record.key());
            records.push((record.key(), record.tag(), record.payload()));
        }
    }
    CheckedTypesFamilyV2::from_records(records)
}
