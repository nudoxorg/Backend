use super::ids::{AtomListId, DocId, EntityListId, ExternalId, ItemKind, LinkId};
use super::tree::TreeLinkTarget;
use super::type_model::Visibility;
use crate::ir::{
    AnonymousCallableFamilyMultiplicity, AtomId, AtomInterner, DeclarationFamilyId,
    DeclarationIdentity, EntityId, ExternalDeclarationIdentity, ExternalEntityRef, StableRef,
    TextId, TypeId, VariantFingerprint,
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
    Link {
        /// Text-pool coordinate for the displayed label.
        label: TextId,
        /// Local entity or external-target coordinate opened by the link.
        target: LinkTarget,
    },
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
    /// Tagged source-written name or anonymous-callable structural anchor.
    pub name: ItemName,
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

/// The stored name lane for a semantic entity.
///
/// Anonymous callables have no symbol name. Their bounded structural anchor
/// occupies the same typed atom coordinate only when this tag says so.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ItemName {
    /// Exact source-written declaration spelling.
    Named(AtomId),
    /// Versioned structural anchor for one anonymous callable instance.
    AnonymousCallable(AtomId),
}

impl ItemName {
    /// Atom coordinate backing either typed name variant.
    #[must_use]
    pub const fn atom(self) -> AtomId {
        match self {
            Self::Named(atom) | Self::AnonymousCallable(atom) => atom,
        }
    }

    /// Atom coordinate only when this row carries a source-written name.
    #[must_use]
    pub const fn named_atom(self) -> Option<AtomId> {
        match self {
            Self::Named(atom) => Some(atom),
            Self::AnonymousCallable(_) => None,
        }
    }

    /// Anchor coordinate only when this row is an anonymous callable.
    #[must_use]
    pub const fn anonymous_callable_anchor(self) -> Option<AtomId> {
        match self {
            Self::Named(_) => None,
            Self::AnonymousCallable(atom) => Some(atom),
        }
    }
}

/// Typed name returned by semantic item readers. Anonymous callable anchors
/// are structural evidence and are never presented as source identifier text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemNameView<'ir> {
    /// Exact source-written declaration spelling.
    Named(&'ir [u8]),
    /// Versioned structural anchor bytes for one anonymous callable.
    AnonymousCallable {
        /// Borrowed typed anchor and stable-family multiplicity.
        anchor: AnonymousCallableAnchorView<'ir>,
    },
}

/// Borrowed, version-checked view of one encoded anonymous callable anchor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnonymousCallableAnchorView<'ir> {
    encoded: &'ir [u8],
}

impl<'ir> AnonymousCallableAnchorView<'ir> {
    pub(crate) const fn new(encoded: &'ir [u8]) -> Self {
        Self { encoded }
    }

    /// Validates and borrows one complete encoded anonymous-callable anchor.
    #[must_use]
    pub fn try_from_encoded(encoded: &'ir [u8]) -> Option<Self> {
        let view = Self::new(encoded);
        view.is_well_formed().then_some(view)
    }

    /// Decodes the bounded typed route while borrowing text cells from the
    /// validated anchor storage.
    #[must_use]
    pub fn steps(self) -> Option<Vec<crate::ir::CallableAnchorStep<'ir>>> {
        if !self.is_well_formed() {
            return None;
        }
        let route_count = usize::try_from(read_anchor_u32(self.encoded, 8)?).ok()?;
        let mut steps = Vec::new();
        steps.try_reserve_exact(route_count).ok()?;
        let mut cursor = 12_usize;
        for _ in 0..route_count {
            let role = match self.encoded.get(cursor).copied()? {
                0 => crate::ir::CallableChildRole::CallArgument,
                1 => crate::ir::CallableChildRole::VariableInitializer,
                2 => crate::ir::CallableChildRole::PropertyValue,
                3 => crate::ir::CallableChildRole::ConditionalConsequent,
                4 => crate::ir::CallableChildRole::ConditionalAlternate,
                5 => crate::ir::CallableChildRole::ArrayElement,
                6 => crate::ir::CallableChildRole::ObjectMemberValue,
                7 => crate::ir::CallableChildRole::SignatureParameterType,
                8 => crate::ir::CallableChildRole::TypeExpression,
                9 => crate::ir::CallableChildRole::TypeAliasValue,
                10 => crate::ir::CallableChildRole::CallSignatureMember,
                11 => crate::ir::CallableChildRole::ConstructSignatureMember,
                _ => return None,
            };
            let parent_tag = self.encoded.get(cursor.checked_add(1)?)?;
            cursor = cursor.checked_add(2)?;
            let parent = match *parent_tag {
                0 => crate::ir::CallableParentShape::Call,
                1 | 2 | 5 | 6 | 8 => {
                    let length = usize::try_from(read_anchor_u32(self.encoded, cursor)?).ok()?;
                    let start = cursor.checked_add(4)?;
                    let end = start.checked_add(length)?;
                    let token = core::str::from_utf8(self.encoded.get(start..end)?)
                        .ok()?
                        .as_bytes();
                    cursor = end;
                    match *parent_tag {
                        1 => crate::ir::CallableParentShape::VariableBinding(token),
                        2 => crate::ir::CallableParentShape::PropertyName(token),
                        5 => crate::ir::CallableParentShape::ObjectMember(token),
                        6 => crate::ir::CallableParentShape::SignatureParameter(token),
                        _ => crate::ir::CallableParentShape::TypeAliasName(token),
                    }
                }
                3 => crate::ir::CallableParentShape::Conditional,
                4 => crate::ir::CallableParentShape::ArrayLiteral,
                7 => {
                    let kind = decode_callable_type_container(*self.encoded.get(cursor)?)?;
                    cursor = cursor.checked_add(1)?;
                    crate::ir::CallableParentShape::TypeContainer(kind)
                }
                _ => return None,
            };
            steps.push(crate::ir::CallableAnchorStep {
                child_role: role,
                parent,
            });
        }
        (cursor == self.encoded.len()).then_some(steps)
    }

    /// Verifies the complete bounded storage grammar without allocating.
    #[must_use]
    pub(crate) fn is_well_formed(self) -> bool {
        let encoded = self.encoded;
        let Some(magic) = encoded.get(..4) else {
            return false;
        };
        let legacy = magic == b"NAC\x01";
        if !legacy && magic != b"NAC\x02" {
            return false;
        }
        let Some(multiplicity) = read_anchor_u32(encoded, 4) else {
            return false;
        };
        let Some(route_count) = read_anchor_u32(encoded, 8) else {
            return false;
        };
        if multiplicity == 0 || route_count == 0 || route_count > 32 {
            return false;
        }
        let mut cursor = 12_usize;
        let mut text_bytes = 0_usize;
        for _ in 0..route_count {
            let Some(role) = encoded.get(cursor).copied() else {
                return false;
            };
            let Some(parent) = encoded.get(cursor.saturating_add(1)).copied() else {
                return false;
            };
            let compatible = matches!(
                (role, parent),
                (0, 0)
                    | (1, 1)
                    | (2, 2)
                    | (3 | 4, 3)
                    | (5, 4)
                    | (6, 5)
                    | (7, 6)
                    | (8, 7)
                    | (9, 8)
                    | (10, 7)
                    | (11, 7)
            );
            if !compatible || (legacy && (role > 6 || parent > 5)) {
                return false;
            }
            cursor = cursor.saturating_add(2);
            if matches!(parent, 1 | 2 | 5 | 6 | 8) {
                let Some(length) = read_anchor_u32(encoded, cursor) else {
                    return false;
                };
                let Ok(length) = usize::try_from(length) else {
                    return false;
                };
                if length == 0 {
                    return false;
                }
                text_bytes = text_bytes.saturating_add(length);
                if text_bytes > crate::ir_vocabulary::MAX_ANONYMOUS_CALLABLE_ANCHOR_BYTES {
                    return false;
                }
                let start = cursor.saturating_add(4);
                let Some(end) = start.checked_add(length) else {
                    return false;
                };
                let Some(token) = encoded.get(start..end) else {
                    return false;
                };
                if core::str::from_utf8(token).is_err() {
                    return false;
                }
                cursor = end;
            } else if parent == 7 {
                let Some(kind) = encoded.get(cursor).copied() else {
                    return false;
                };
                if kind > 11 || (matches!(role, 10 | 11) && !matches!(kind, 10 | 11)) {
                    return false;
                }
                cursor = cursor.saturating_add(1);
            }
        }
        cursor == encoded.len()
    }

    /// Exact versioned bytes retained for this structural anchor.
    #[must_use]
    pub const fn encoded_bytes(self) -> &'ir [u8] {
        self.encoded
    }

    /// Current multiplicity of the stable structural callable family.
    #[must_use]
    pub fn family_multiplicity(self) -> Option<AnonymousCallableFamilyMultiplicity> {
        if !self.is_well_formed() {
            return None;
        }
        if !matches!(self.encoded.get(..4)?, b"NAC\x01" | b"NAC\x02") {
            return None;
        }
        let count = u32::from_le_bytes(self.encoded.get(4..8)?.try_into().ok()?);
        AnonymousCallableFamilyMultiplicity::new(count)
    }
}

fn decode_callable_type_container(value: u8) -> Option<crate::ir::CallableTypeContainerKind> {
    Some(match value {
        0 => crate::ir::CallableTypeContainerKind::Array,
        1 => crate::ir::CallableTypeContainerKind::Tuple,
        2 => crate::ir::CallableTypeContainerKind::Union,
        3 => crate::ir::CallableTypeContainerKind::Intersection,
        4 => crate::ir::CallableTypeContainerKind::Parenthesized,
        5 => crate::ir::CallableTypeContainerKind::Optional,
        6 => crate::ir::CallableTypeContainerKind::Rest,
        7 => crate::ir::CallableTypeContainerKind::Function,
        8 => crate::ir::CallableTypeContainerKind::Constructor,
        9 => crate::ir::CallableTypeContainerKind::TypeReference,
        10 => crate::ir::CallableTypeContainerKind::TypeLiteral,
        11 => crate::ir::CallableTypeContainerKind::Interface,
        _ => return None,
    })
}

fn read_anchor_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

impl<'ir> ItemNameView<'ir> {
    /// Returns source-written bytes only for named declarations.
    #[must_use]
    pub const fn named_bytes(self) -> Option<&'ir [u8]> {
        match self {
            Self::Named(bytes) => Some(bytes),
            Self::AnonymousCallable { .. } => None,
        }
    }

    /// Returns the anonymous structural anchor, if this row is callable and
    /// has no source-written declaration name.
    #[must_use]
    pub const fn anonymous_anchor(self) -> Option<AnonymousCallableAnchorView<'ir>> {
        match self {
            Self::Named(_) => None,
            Self::AnonymousCallable { anchor } => Some(anchor),
        }
    }
}

impl PartialEq<&[u8]> for ItemNameView<'_> {
    fn eq(&self, other: &&[u8]) -> bool {
        matches!(self, Self::Named(bytes) if *bytes == *other)
    }
}

impl<const N: usize> PartialEq<&[u8; N]> for ItemNameView<'_> {
    fn eq(&self, other: &&[u8; N]) -> bool {
        matches!(self, Self::Named(bytes) if *bytes == other.as_slice())
    }
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
