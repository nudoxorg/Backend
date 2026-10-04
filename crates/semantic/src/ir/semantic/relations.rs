use super::ids::{AtomListId, DocId, EntityListId, ExternalId, ItemKind, LinkId};
use super::tree::TreeLinkTarget;
use super::type_model::Visibility;
use crate::ir::{
    AtomId, AtomInterner, DeclarationFamilyId, DeclarationIdentity, EntityId,
    ExternalDeclarationIdentity, ExternalEntityRef, StableRef, TextId, TypeId, VariantFingerprint,
};
use core::mem::size_of;

/// A graph target, local or self-describing across a package boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LinkTarget {
    /// Entity stored in the same semantic image.
    Local(EntityId),
    /// Endpoint stored in the image's external-target pool.
    External(ExternalId),
}

/// Exact retained origin facts for an unresolved foreign declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ForeignTargetOrigin {
    /// The remote declaration belongs to a named package in an ecosystem.
    Package {
        /// Ecosystem name as admitted by the producer.
        ecosystem: AtomId,
        /// Canonical package identifier within that ecosystem.
        package: AtomId,
    },
    /// The remote declaration belongs to a namespace without package identity.
    Namespace {
        /// Ecosystem name as admitted by the producer.
        ecosystem: AtomId,
        /// Canonical namespace identifier within that ecosystem.
        namespace: AtomId,
    },
    /// The producer identified only the containing ecosystem-wide universe.
    Universe {
        /// Ecosystem name as admitted by the producer.
        ecosystem: AtomId,
    },
    /// A producer supplied an ecosystem/path but no stronger foreign-origin
    /// classification. This is not silently promoted to a package.
    Unspecified {
        /// Ecosystem name in which the remote path was observed.
        ecosystem: AtomId,
    },
}

/// Everything a producer knows about an unresolved foreign declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ForeignExternalTarget {
    /// Separately branded foreign key plus honest variant availability.
    pub identity: ExternalDeclarationIdentity,
    /// Exact native/package/namespace authority supplied by the producer.
    pub origin: ForeignTargetOrigin,
    /// Canonical remote path.
    pub path: AtomId,
    /// Source display spelling.
    pub display: AtomId,
    /// Expected declaration kind, when known.
    pub kind: Option<ItemKind>,
}

/// Cross-fragment graph target. Resolved declaration endpoints are never
/// represented as an unresolved foreign path and an unresolved key can never
/// manufacture a fragment/declaration pair.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExternalTarget {
    /// Exact resolved endpoint in a known external fragment.
    Stable {
        /// Stable reference to the target declaration in its fragment.
        target: StableRef,
    },
    /// Self-describing unresolved foreign authority and path facts.
    Foreign(ForeignExternalTarget),
    /// Legacy external fragment ordinal retained from type facts that do not
    /// yet supply a declaration identity. It remains distinct from both a
    /// resolved [`StableRef`] and an unresolved foreign key.
    FragmentEntity {
        /// External fragment and entity ordinals for the legacy target.
        target: ExternalEntityRef,
        /// Source spelling used to display the unresolved legacy endpoint.
        display: AtomId,
    },
}

/// Documentation is UTF-8 by construction; only its IDs carry that promise.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DocFragment {
    /// UTF-8 prose stored in the text pool.
    Text(TextId),
    /// UTF-8 code content stored in the text pool.
    Code(TextId),
    /// UTF-8 link label and its local or external destination.
    Link { label: TextId, target: LinkTarget },
    /// A soft documentation line break.
    SoftBreak,
    /// A hard documentation line break.
    HardBreak,
}

/// Borrowed documentation accepted from a frontend without an intermediate string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocInput<'source> {
    /// Borrowed UTF-8 prose.
    Text(&'source str),
    /// Borrowed UTF-8 code content.
    Code(&'source str),
    /// Borrowed link label and a tree-local destination.
    Link {
        /// Text displayed for the link.
        label: &'source str,
        /// Destination resolved while this borrowed tree is admitted.
        target: TreeLinkTarget,
    },
    /// A soft documentation line break.
    SoftBreak,
    /// A hard documentation line break.
    HardBreak,
}

/// Half-open source byte range. The file itself is universally interned bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceSpan {
    file: AtomId,
    start: u32,
    end: u32,
}

impl SourceSpan {
    /// Creates a valid half-open source range.
    #[must_use]
    pub const fn new(file: AtomId, start: u32, end: u32) -> Option<Self> {
        if start <= end {
            Some(Self { file, start, end })
        } else {
            None
        }
    }
    /// Interned path or other file identity for this range.
    #[must_use]
    pub const fn file(self) -> AtomId {
        self.file
    }
    /// Inclusive starting byte offset within `file`.
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }
    /// Exclusive ending byte offset within `file`.
    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }
    /// Number of source bytes in this half-open range.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }
    /// Whether the range covers zero bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// Exact identity used to order a graph endpoint. Local endpoints always
/// retain both family and variant; foreign endpoints retain unavailable
/// variant knowledge explicitly rather than borrowing a local convention.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DeclarationLinkTarget {
    /// Local endpoint with both declaration-family and variant identity.
    Local(DeclarationIdentity),
    /// Resolved endpoint in a known external fragment.
    Stable(StableRef),
    /// Foreign declaration key whose variant availability remains explicit.
    Foreign(ExternalDeclarationIdentity),
    /// Legacy external fragment ordinal without declaration identity.
    FragmentEntity(ExternalEntityRef),
}

/// Whether one semantic plane participates in [`CorePayloadHash`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CorePayloadPlane {
    /// Included in the current canonical core-payload contract.
    Included,
    /// Deliberately excluded until the plane has one durable authority
    /// contract; absence here must never be reported as semantic parity.
    ExcludedPendingAuthority,
}

/// Honest coverage of the current core declaration payload.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CorePayloadCoverage {
    /// Whether declaration kind and shape contribute to the hash.
    pub declaration_shape: CorePayloadPlane,
    /// Whether semantic type structure contributes to the hash.
    pub type_structure: CorePayloadPlane,
    /// Whether ordered tuple/function children contribute to the hash.
    pub ordered_product_children: CorePayloadPlane,
    /// Whether ordered members whose targets are local contribute to the hash.
    pub ordered_local_members: CorePayloadPlane,
    /// Whether documentation fragments contribute to the hash.
    pub documentation: CorePayloadPlane,
    /// Whether visibility facts contribute to the hash.
    pub visibility: CorePayloadPlane,
    /// Whether language-owned extension facts contribute to the hash.
    pub language_extension: CorePayloadPlane,
    /// Whether source file and byte-range facts contribute to the hash.
    pub source_provenance: CorePayloadPlane,
    /// Whether graph occurrence evidence contributes to the hash.
    pub occurrences: CorePayloadPlane,
    /// Whether opaque parentage facts contribute to the hash.
    pub opaque_parentage: CorePayloadPlane,
}

/// Canonical hash of the currently admitted core semantic declaration
/// payload. It is intentionally not an authority-complete payload hash.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CorePayloadHash([u8; 16]);

impl CorePayloadHash {
    /// Width of one canonical compact payload digest.
    pub const BYTES: usize = size_of::<Self>();

    /// Exact plane coverage of every value minted by this type.
    pub const COVERAGE: CorePayloadCoverage = CorePayloadCoverage {
        declaration_shape: CorePayloadPlane::Included,
        type_structure: CorePayloadPlane::Included,
        ordered_product_children: CorePayloadPlane::Included,
        ordered_local_members: CorePayloadPlane::Included,
        documentation: CorePayloadPlane::ExcludedPendingAuthority,
        visibility: CorePayloadPlane::ExcludedPendingAuthority,
        language_extension: CorePayloadPlane::ExcludedPendingAuthority,
        source_provenance: CorePayloadPlane::ExcludedPendingAuthority,
        occurrences: CorePayloadPlane::ExcludedPendingAuthority,
        opaque_parentage: CorePayloadPlane::ExcludedPendingAuthority,
    };

    /// Wraps an already-computed 16-byte digest without re-hashing it.
    #[must_use]
    pub const fn from_raw(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
    /// Hashes canonical payload bytes with BLAKE3 and keeps the first 16 bytes.
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        let hash = blake3::hash(bytes);
        let bytes = hash.as_bytes();
        Self([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ])
    }
    /// Returns the compact digest bytes in their stored order.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Cold VCS columns aligned with one hot entity row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EntityVersion {
    /// Stable declaration family.  VCS can match a singleton family across a
    /// signature edit without pretending that an overload group is singular.
    pub family: DeclarationFamilyId,
    /// Structural fingerprint distinguishing current instances in one family.
    pub variant: VariantFingerprint,
    /// Current core-only semantic payload hash; see [`CorePayloadHash::COVERAGE`].
    pub core_payload: CorePayloadHash,
}

impl EntityVersion {
    /// The exact local declaration instance key. A family alone is a range,
    /// never a singular graph or index key.
    #[must_use]
    pub const fn identity(self) -> DeclarationIdentity {
        DeclarationIdentity {
            family: self.family,
            variant: self.variant,
        }
    }
}

/// One compact entity row. All variable-size data is an interned typed-list ID.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Item {
    /// Interned source spelling of this declaration's name.
    pub name: AtomId,
    /// Cross-language declaration category.
    pub kind: ItemKind,
    /// Language-independent visibility fact, or `Unknown` when unavailable.
    pub visibility: Visibility,
    /// Optional parent entity in this image's entity-ID space.
    pub parent: Option<EntityId>,
    /// Optional semantic type coordinate in this image's type arena.
    pub semantic_type: Option<TypeId>,
    /// Interned, declaration-ordered child entity coordinates.
    pub members: EntityListId,
    /// Interned sequence of prose, code, links, or break fragments.
    pub docs: DocId,
    /// Interned source attributes represented as atom coordinates.
    pub attributes: AtomListId,
    /// Optional source file and half-open byte range.
    pub source: Option<SourceSpan>,
}
/// Kind of an extrinsic graph edge.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LinkKind {
    /// A call from the source entity to the target entity.
    Calls,
    /// A call resolved specifically as a method invocation.
    MethodCall,
    /// A use of the target as a type.
    TypeReference,
    /// A read of the target's value.
    Reads,
    /// A write to the target's value.
    Writes,
    /// An import of the target declaration or module.
    Imports,
    /// An implementation relation, such as a type implementing an interface.
    Implements,
    /// A declaration replacing or specializing another declaration.
    Overrides,
    /// A public name re-exported from the target.
    Reexports,
    /// A nominal inheritance relation.
    Inherits,
    /// A documentation reference to the target.
    Documents,
}

/// Resolution confidence forms a monotonic quality lattice.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Confidence {
    /// Inferred directly from parsed syntax without name resolution.
    Syntactic,
    /// Inferred by a heuristic rule rather than an authoritative resolver.
    Heuristic,
    /// Resolved using an index of declarations or symbols.
    Indexed,
    /// Imported from a producer or persisted semantic fragment.
    Imported,
    /// Reported by a compiler or equivalent language authority.
    Compiler,
}

/// One directed graph edge.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Link {
    /// Source entity coordinate in the owning image.
    pub from: EntityId,
    /// Local entity coordinate or external-target pool coordinate.
    pub target: LinkTarget,
    /// Semantic relation encoded by this directed edge.
    pub kind: LinkKind,
    /// Strongest confidence observed for this canonical relation.
    pub confidence: Confidence,
    /// Optional representative source for compatibility queries.
    ///
    /// This is never the exhaustive evidence set. Consumers that need source
    /// truth must use [`LinkOccurrence`] rows, which retain every site.
    pub source: Option<SourceSpan>,
}

/// One authority-observed use site of a canonical [`Link`].
///
/// `link` names the deduplicated semantic relation. `confidence` and
/// `source` are intentionally site-local evidence: replacing them with the
/// relation's strongest observation would lose repeated references such as a
/// parameter and result both naming the same symbol.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LinkOccurrence {
    /// Deduplicated relation row observed at this use site.
    pub link: LinkId,
    /// Confidence of this particular observation.
    pub confidence: Confidence,
    /// Optional half-open source range for this particular use site.
    pub source: Option<SourceSpan>,
}

/// Evidence-independent identity used to intern exactly one row per logical link.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct LinkKey {
    pub(super) from: EntityId,
    pub(super) target: LinkTarget,
    pub(super) kind: LinkKind,
}

/// Selects the deterministic compatibility evidence stored beside one
/// canonical relation. Captured sites outrank an uncaptured representative;
/// equal-confidence captured sites sort by `(path bytes, start, end)`. The
/// complete site set is deliberately held in [`LinkOccurrence`], never
/// inferred here.
pub(super) fn canonical_relation_evidence_precedes(
    candidate: Link,
    known: Link,
    atoms: &AtomInterner,
) -> bool {
    match candidate.confidence.cmp(&known.confidence) {
        core::cmp::Ordering::Greater => true,
        core::cmp::Ordering::Less => false,
        core::cmp::Ordering::Equal => {
            source_evidence_key(candidate.source, atoms) < source_evidence_key(known.source, atoms)
        }
    }
}

pub(super) fn source_evidence_key<'atoms>(
    source: Option<SourceSpan>,
    atoms: &'atoms AtomInterner,
) -> (u8, &'atoms [u8], u32, u32) {
    match source {
        // Compare path bytes rather than image-local atom coordinates so the
        // representative survives interning and authority emission reorder.
        Some(span) => (
            0,
            atoms.get(span.file()).unwrap_or(&[]),
            span.start(),
            span.end(),
        ),
        None => (1, &[], u32::MAX, u32::MAX),
    }
}
