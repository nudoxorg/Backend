use super::ids::{AtomListId, DocId, EntityListId, ExternalId, ItemKind, LinkId};
use super::tree::TreeLinkTarget;
use super::type_model::Visibility;
use crate::ir::{
    AtomId, DeclarationFamilyId, DeclarationIdentity, DenseId, EntityId,
    ExternalDeclarationIdentity, ExternalEntityRef, StableRef, TextId, TypeId, VariantFingerprint,
};
use backend_version::ContentId;
use core::fmt;

/// A graph target, local or self-describing across a package boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LinkTarget {
    Local(EntityId),
    External(ExternalId),
}

/// Exact retained origin facts for an unresolved foreign declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ForeignTargetOrigin {
    Package {
        ecosystem: AtomId,
        package: AtomId,
    },
    Namespace {
        ecosystem: AtomId,
        namespace: AtomId,
    },
    Universe {
        ecosystem: AtomId,
    },
    /// A producer supplied an ecosystem/path but no stronger foreign-origin
    /// classification. This is not silently promoted to a package.
    Unspecified {
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
    Stable { target: StableRef },
    /// Self-describing unresolved foreign authority and path facts.
    Foreign(ForeignExternalTarget),
    /// Legacy external fragment ordinal retained from type facts that do not
    /// yet supply a declaration identity. It remains distinct from both a
    /// resolved [`StableRef`] and an unresolved foreign key.
    FragmentEntity {
        target: ExternalEntityRef,
        display: AtomId,
    },
}

/// Documentation is UTF-8 by construction; only its IDs carry that promise.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DocFragment {
    Text(TextId),
    Code(TextId),
    Link { label: TextId, target: LinkTarget },
    SoftBreak,
    HardBreak,
}

/// Borrowed documentation accepted from a frontend without an intermediate string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocInput<'source> {
    Text(&'source str),
    Code(&'source str),
    Link {
        label: &'source str,
        target: TreeLinkTarget,
    },
    SoftBreak,
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
    #[must_use]
    pub const fn file(self) -> AtomId {
        self.file
    }
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }
    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }
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
    Local(DeclarationIdentity),
    Stable(StableRef),
    Foreign(ExternalDeclarationIdentity),
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
    pub declaration_shape: CorePayloadPlane,
    pub type_structure: CorePayloadPlane,
    pub ordered_product_children: CorePayloadPlane,
    pub ordered_local_members: CorePayloadPlane,
    pub documentation: CorePayloadPlane,
    pub visibility: CorePayloadPlane,
    pub language_extension: CorePayloadPlane,
    pub source_provenance: CorePayloadPlane,
    pub occurrences: CorePayloadPlane,
    pub opaque_parentage: CorePayloadPlane,
}

/// Canonical hash of the currently admitted core semantic declaration
/// payload. It is intentionally not an authority-complete payload hash.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CorePayloadHash([u8; 16]);

impl CorePayloadHash {
    /// Width of one canonical compact payload digest.
    pub const BYTES: usize = core::mem::size_of::<Self>();

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

    #[must_use]
    pub const fn from_raw(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        let hash = blake3::hash(bytes);
        let bytes = hash.as_bytes();
        Self([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ])
    }
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
    pub name: AtomId,
    pub kind: ItemKind,
    pub visibility: Visibility,
    pub parent: Option<EntityId>,
    pub semantic_type: Option<TypeId>,
    pub members: EntityListId,
    pub docs: DocId,
    pub attributes: AtomListId,
    pub source: Option<SourceSpan>,
}
/// Kind of an extrinsic graph edge.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LinkKind {
    Calls,
    MethodCall,
    TypeReference,
    Reads,
    Writes,
    Imports,
    Implements,
    Overrides,
    Reexports,
    Inherits,
    Documents,
}

/// Resolution confidence forms a monotonic quality lattice.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Confidence {
    Syntactic,
    Heuristic,
    Indexed,
    Imported,
    Compiler,
}

/// One directed graph edge.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Link {
    pub from: EntityId,
    pub target: LinkTarget,
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
    pub link: LinkId,
    pub confidence: Confidence,
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
/// equal-confidence captured sites sort by `(file, start, end)`. The complete
/// site set is deliberately held in [`LinkOccurrence`], never inferred here.
pub(super) fn canonical_relation_evidence_precedes(candidate: Link, known: Link) -> bool {
    match candidate.confidence.cmp(&known.confidence) {
        core::cmp::Ordering::Greater => true,
        core::cmp::Ordering::Less => false,
        core::cmp::Ordering::Equal => {
            source_evidence_key(candidate.source) < source_evidence_key(known.source)
        }
    }
}

pub(super) fn source_evidence_key(source: Option<SourceSpan>) -> (u8, u32, u32, u32) {
    match source {
        // A captured coordinate is more specific than universal absence.
        Some(span) => (0, span.file().raw, span.start(), span.end()),
        None => (1, u32::MAX, u32::MAX, u32::MAX),
    }
}
