//! Immutable semantic-authority facts aligned with condensed entity rows.
//!
//! This module owns truth availability rather than leaving a driver-side
//! compatibility sidecar beside [`crate::ir::Ir`].  The facts live in cold
//! structure-of-arrays storage: ordinary declaration scans need not touch
//! provenance or availability, while callers can borrow every exact authority
//! lane without reconstructing it from optional semantic values.

use alloc::vec::Vec;

use crate::vocabulary::{CompileRecipeFact, PackageUrl};
use backend_version::{ContentId, SemanticScopeDomain};

use crate::ir::{AtomId, DeclarationIdentity, SourceIdentity};

/// Whether an authority supplied one semantic plane for an entity.
///
/// [`Self::Captured`] is independent of value cardinality: an authority can
/// prove an empty documentation, attributes, or member set. [`Self::Unavailable`]
/// means the source authority did not supply that plane; it never means the
/// corresponding owned value was observed empty.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FactAvailability {
    /// The authority supplied the plane, including a proved-empty list.
    Captured,
    /// The authority did not expose the plane.
    #[default]
    Unavailable,
}

/// Opaque identity of an authority owner which has no local declaration row.
///
/// This value intentionally cannot be converted into an [`EntityId`].  It
/// retains native ownership evidence without manufacturing a local root.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct UnrepresentedAuthorityOwner {
    bytes: [u8; 16],
}

impl UnrepresentedAuthorityOwner {
    /// Retains the exact fixed-width native identity without treating it as a
    /// local coordinate or declaration key.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self { bytes }
    }

    /// Borrows the retained opaque authority evidence for diagnostics.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.bytes
    }
}

/// Closed authority fact for one declaration's containment relationship.
///
/// A bound owner is the parent's exact local [`DeclarationIdentity`], not a
/// staging coordinate or a family-only key.  That keeps nested declarations
/// beneath overloaded parents unambiguous in the owned image.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ParentageAuthority {
    /// The authority did not expose containment for this entity.
    #[default]
    Unavailable,
    /// The authority proved this entity has no local parent.
    Root,
    /// The authority proved this exact local declaration instance is parent.
    Bound(DeclarationIdentity),
    /// The authority proved a non-local owner and retained its opaque ID.
    UnrepresentedAuthorityOwner(UnrepresentedAuthorityOwner),
}

/// All authority facts for one entity row.
///
/// The row is a public immutable value for construction and typed inspection;
/// finalized [`crate::ir::Ir`] images retain its fields in cold, aligned columns.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EntityAuthorityFacts {
    /// Exact containment truth, not inferred from the owned `parent` field.
    pub parentage: ParentageAuthority,
    /// Primary declaration source span availability.
    pub source: FactAvailability,
    /// Primary source-file identity availability for `source`.
    pub source_file: FactAvailability,
    /// Complete directly declared member inventory availability. Inherited,
    /// effective and runtime structural members are outside this plane.
    pub members: FactAvailability,
    /// Semantic type availability.
    pub semantic_type: FactAvailability,
    /// Documentation-plane availability.
    pub documentation: FactAvailability,
    /// Visibility observation availability.
    pub visibility: FactAvailability,
    /// Attribute-plane availability.
    pub attributes: FactAvailability,
    /// Language-specific extension-plane availability.
    pub language_extension: FactAvailability,
}

/// Atom-backed package/file scope retained by a compiled semantic image.
///
/// The atoms are validated against the image's single arena.  This keeps the
/// request's package lineage and relative source path queryable after driver
/// scratch and authority input have gone away.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticScopeFacts {
    /// Atom naming the package ecosystem used to interpret `package`.
    pub ecosystem: AtomId,
    /// Atom containing the package name in that ecosystem.
    pub package: AtomId,
    /// Atom containing the source path relative to the package root.
    pub path: AtomId,
    /// Exact canonical package URL when this image came from package compilation.
    pub coordinate: Option<AtomId>,
}

/// Coordinate-free commitment to a requested package/file scope before its
/// atoms enter an image. The declaration-key identity commits the validated
/// framed scope; the owned header retains the queryable atom values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticScopeClaim {
    /// Domain-separated commitment to the package coordinate and exact source path.
    pub identity: ContentId<SemanticScopeDomain>,
}

impl SemanticScopeClaim {
    /// Derives the package-aware scope commitment used by compiler-produced
    /// images. The package URL identity commits its complete canonical
    /// spelling, including version, qualifiers, and subpath; the path binds
    /// the individual source artifact within that package.
    #[must_use]
    pub(crate) fn for_package(coordinate: &PackageUrl, path: &str) -> Self {
        let mut canonical = Vec::with_capacity(33_usize.saturating_add(path.len()));
        canonical.push(1);
        canonical.extend_from_slice(coordinate.identity.as_ref());
        canonical.extend_from_slice(path.as_bytes());
        Self {
            identity: ContentId::<SemanticScopeDomain>::from_canonical_bytes(&canonical),
        }
    }
}

/// Exact image provenance requested at a write-once admission boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageProvenanceClaim {
    /// Source-content identity and exact source byte length requested for admission.
    pub source: SourceIdentity,
    /// Compiler recipe whose identity binds the source and compilation settings.
    pub recipe: CompileRecipeFact,
    /// Package/file scope commitment the image must retain.
    pub scope: SemanticScopeClaim,
}

/// Owned compile provenance for an image that originated in an authority
/// transaction.  Manually assembled images retain an explicit unavailable
/// state rather than inventing source, recipe, or scope facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageProvenance {
    /// No source-authority transaction supplied provenance for this image.
    Unavailable,
    /// Provenance retained from a successfully admitted source-authority transaction.
    Captured {
        /// Exact source identity admitted for the compiled image.
        source: SourceIdentity,
        /// Exact recipe identity admitted for the compiled image.
        recipe: CompileRecipeFact,
        /// Coordinate-free scope commitment used for idempotent provenance
        /// admission and exact rebind diagnostics.
        claim: SemanticScopeClaim,
        /// Queryable package and source-path atoms retained in the image.
        scope: SemanticScopeFacts,
    },
}

impl Default for ImageProvenance {
    fn default() -> Self {
        Self::Unavailable
    }
}

/// Borrowed cold authority lanes aligned one-for-one with entity rows.
///
/// Every field is directly inspectable.  There are deliberately no
/// convenience getters which could blur unavailable truth into an empty
/// semantic value.
#[derive(Clone, Copy, Debug)]
pub struct EntityAuthorityColumns<'ir> {
    /// Containment authority for each entity ordinal.
    pub parentage: &'ir [ParentageAuthority],
    /// Availability of the primary source span for each entity ordinal.
    pub source: &'ir [FactAvailability],
    /// Availability of source-file identity tied to the primary span.
    pub source_file: &'ir [FactAvailability],
    /// Availability of the complete local member set, including known-empty sets.
    pub members: &'ir [FactAvailability],
    /// Availability of the entity's semantic type observation.
    pub semantic_type: &'ir [FactAvailability],
    /// Availability of the entity's documentation observation.
    pub documentation: &'ir [FactAvailability],
    /// Availability of the entity's visibility observation.
    pub visibility: &'ir [FactAvailability],
    /// Availability of the entity's attribute observation.
    pub attributes: &'ir [FactAvailability],
    /// Availability of the language-profile-specific extension observation.
    pub language_extension: &'ir [FactAvailability],
}

/// Authority facts for one observed graph occurrence site.
///
/// A missing final [`crate::ir::SourceSpan`] is ambiguous by itself: a caller
/// needs this row to know whether source-site truth was supplied. The current
/// two-state model validates `Captured` exactly when a final site exists;
/// captured-but-unprojectable evidence would require a future third state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OccurrenceAuthorityFacts {
    /// Whether the source site for this graph occurrence was observed by authority.
    pub source: FactAvailability,
}

/// Borrowed cold authority lane aligned with [`crate::ir::LinkOccurrence`] rows.
#[derive(Clone, Copy, Debug)]
pub struct OccurrenceAuthorityColumns<'ir> {
    /// Source-site availability aligned with each `LinkOccurrence` ordinal.
    pub source: &'ir [FactAvailability],
}

impl OccurrenceAuthorityColumns<'_> {
    /// Number of occurrence rows sharing this source-availability lane.
    #[must_use]
    pub const fn row_count(self) -> usize {
        self.source.len()
    }
}

impl EntityAuthorityColumns<'_> {
    /// Number of entity rows shared by all authority planes.
    #[must_use]
    pub const fn row_count(self) -> usize {
        self.parentage.len()
    }
}

/// Closed authority plane named by an admission mismatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityFactPlane {
    /// Primary declaration source span.
    Source,
    /// Source-file identity associated with the primary span.
    SourceFile,
    /// Complete directly declared member inventory, excluding inherited,
    /// effective and runtime structural membership.
    Members,
    /// Semantic type assigned to the declaration.
    SemanticType,
    /// Documentation observed for the declaration.
    Documentation,
    /// Visibility observed for the declaration.
    Visibility,
    /// Attributes observed for the declaration.
    Attributes,
    /// Language-specific extension facts observed for the declaration.
    LanguageExtension,
    /// Source-site span on a graph occurrence row.
    OccurrenceSource,
}

/// Exact reason an entity authority row disagreed with its owned semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityFactFault {
    /// Containment truth did not agree with the local parent relationship.
    Parentage {
        /// Containment fact supplied by the authority.
        claimed: ParentageAuthority,
        /// Local parent derived from the owned entity row, if any.
        local_parent: Option<DeclarationIdentity>,
    },
    /// An optional authority plane did not agree with its owned value.
    Availability {
        /// Plane whose captured/unavailable state disagreed with its value.
        plane: AuthorityFactPlane,
        /// Availability state supplied by the authority.
        claimed: FactAvailability,
        /// Whether the corresponding finalized semantic value exists.
        present: bool,
    },
    /// Source-file authority cannot exist independently of a source span.
    SourceFileWithoutSource {
        /// Source-span availability supplied for the entity.
        source: FactAvailability,
        /// Source-file availability that cannot be represented without a span.
        source_file: FactAvailability,
    },
}

/// Owned cold authority columns.  This is crate-private so all row insertion
/// remains coupled to the matching entity insertion in [`crate::ir::IrBuilder`].
#[derive(Default)]
pub(crate) struct AuthorityColumns {
    parentage: Vec<ParentageAuthority>,
    source: Vec<FactAvailability>,
    source_file: Vec<FactAvailability>,
    members: Vec<FactAvailability>,
    semantic_type: Vec<FactAvailability>,
    documentation: Vec<FactAvailability>,
    visibility: Vec<FactAvailability>,
    attributes: Vec<FactAvailability>,
    language_extension: Vec<FactAvailability>,
}

impl AuthorityColumns {
    pub(crate) fn len(&self) -> usize {
        self.parentage.len()
    }

    pub(crate) fn reserve(&mut self, additional: usize) {
        self.parentage.reserve(additional);
        self.source.reserve(additional);
        self.source_file.reserve(additional);
        self.members.reserve(additional);
        self.semantic_type.reserve(additional);
        self.documentation.reserve(additional);
        self.visibility.reserve(additional);
        self.attributes.reserve(additional);
        self.language_extension.reserve(additional);
    }

    pub(crate) fn push(&mut self, facts: EntityAuthorityFacts) {
        self.parentage.push(facts.parentage);
        self.source.push(facts.source);
        self.source_file.push(facts.source_file);
        self.members.push(facts.members);
        self.semantic_type.push(facts.semantic_type);
        self.documentation.push(facts.documentation);
        self.visibility.push(facts.visibility);
        self.attributes.push(facts.attributes);
        self.language_extension.push(facts.language_extension);
    }

    pub(crate) fn facts(&self, index: usize) -> Option<EntityAuthorityFacts> {
        Some(EntityAuthorityFacts {
            parentage: *self.parentage.get(index)?,
            source: *self.source.get(index)?,
            source_file: *self.source_file.get(index)?,
            members: *self.members.get(index)?,
            semantic_type: *self.semantic_type.get(index)?,
            documentation: *self.documentation.get(index)?,
            visibility: *self.visibility.get(index)?,
            attributes: *self.attributes.get(index)?,
            language_extension: *self.language_extension.get(index)?,
        })
    }

    pub(crate) fn view(&self) -> EntityAuthorityColumns<'_> {
        EntityAuthorityColumns {
            parentage: &self.parentage,
            source: &self.source,
            source_file: &self.source_file,
            members: &self.members,
            semantic_type: &self.semantic_type,
            documentation: &self.documentation,
            visibility: &self.visibility,
            attributes: &self.attributes,
            language_extension: &self.language_extension,
        }
    }

    pub(crate) fn aligned(&self, count: usize) -> bool {
        self.parentage.len() == count
            && self.source.len() == count
            && self.source_file.len() == count
            && self.members.len() == count
            && self.semantic_type.len() == count
            && self.documentation.len() == count
            && self.visibility.len() == count
            && self.attributes.len() == count
            && self.language_extension.len() == count
    }
}

/// Owned cold authority lane for graph occurrences.
#[derive(Default)]
pub(crate) struct OccurrenceAuthorityColumn {
    source: Vec<FactAvailability>,
}

impl OccurrenceAuthorityColumn {
    pub(crate) fn len(&self) -> usize {
        self.source.len()
    }

    pub(crate) fn reserve(&mut self, additional: usize) {
        self.source.reserve(additional);
    }

    pub(crate) fn push(&mut self, facts: OccurrenceAuthorityFacts) {
        self.source.push(facts.source);
    }

    pub(crate) fn get(&self, index: usize) -> Option<OccurrenceAuthorityFacts> {
        Some(OccurrenceAuthorityFacts {
            source: *self.source.get(index)?,
        })
    }

    pub(crate) fn view(&self) -> OccurrenceAuthorityColumns<'_> {
        OccurrenceAuthorityColumns {
            source: &self.source,
        }
    }
}
