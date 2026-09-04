//! Immutable semantic-authority facts aligned with condensed entity rows.
//!
//! This module owns truth availability rather than leaving a driver-side
//! compatibility sidecar beside [`crate::Ir`].  The facts live in cold
//! structure-of-arrays storage: ordinary declaration scans need not touch
//! provenance or availability, while callers can borrow every exact authority
//! lane without reconstructing it from optional semantic values.

use alloc::vec::Vec;

use compiler_vocabulary::CompileRecipeFact;
use heart_identity::{ContentId, SemanticScopeDomain};

use crate::{AtomId, DeclarationIdentity, SourceIdentity};

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
/// finalized [`crate::Ir`] images retain its fields in cold, aligned columns.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EntityAuthorityFacts {
    /// Exact containment truth, not inferred from the owned `parent` field.
    pub parentage: ParentageAuthority,
    /// Primary declaration source span availability.
    pub source: FactAvailability,
    /// Primary source-file identity availability for `source`.
    pub source_file: FactAvailability,
    /// Complete local member-set availability.
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
    pub ecosystem: AtomId,
    pub package: AtomId,
    pub path: AtomId,
}

/// Coordinate-free commitment to a requested package/file scope before its
/// atoms enter an image. The declaration-key identity commits the validated
/// framed scope; the owned header retains the queryable atom values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticScopeClaim {
    pub identity: ContentId<SemanticScopeDomain>,
}

/// Exact image provenance requested at a write-once admission boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageProvenanceClaim {
    pub source: SourceIdentity,
    pub recipe: CompileRecipeFact,
    pub scope: SemanticScopeClaim,
}

/// Owned compile provenance for an image that originated in an authority
/// transaction.  Manually assembled images retain an explicit unavailable
/// state rather than inventing source, recipe, or scope facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageProvenance {
    Unavailable,
    Captured {
        source: SourceIdentity,
        recipe: CompileRecipeFact,
        /// Coordinate-free scope commitment used for idempotent provenance
        /// admission and exact rebind diagnostics.
        claim: SemanticScopeClaim,
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
    pub parentage: &'ir [ParentageAuthority],
    pub source: &'ir [FactAvailability],
    pub source_file: &'ir [FactAvailability],
    pub members: &'ir [FactAvailability],
    pub semantic_type: &'ir [FactAvailability],
    pub documentation: &'ir [FactAvailability],
    pub visibility: &'ir [FactAvailability],
    pub attributes: &'ir [FactAvailability],
    pub language_extension: &'ir [FactAvailability],
}

/// Authority facts for one observed graph occurrence site.
///
/// A missing final [`crate::SourceSpan`] is ambiguous by itself: a caller
/// needs this row to know whether source-site truth was supplied. The current
/// two-state model validates `Captured` exactly when a final site exists;
/// captured-but-unprojectable evidence would require a future third state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OccurrenceAuthorityFacts {
    pub source: FactAvailability,
}

/// Borrowed cold authority lane aligned with [`crate::LinkOccurrence`] rows.
#[derive(Clone, Copy, Debug)]
pub struct OccurrenceAuthorityColumns<'ir> {
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
    Source,
    SourceFile,
    Members,
    SemanticType,
    Documentation,
    Visibility,
    Attributes,
    LanguageExtension,
    OccurrenceSource,
}

/// Exact reason an entity authority row disagreed with its owned semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityFactFault {
    /// Containment truth did not agree with the local parent relationship.
    Parentage {
        claimed: ParentageAuthority,
        local_parent: Option<DeclarationIdentity>,
    },
    /// An optional authority plane did not agree with its owned value.
    Availability {
        plane: AuthorityFactPlane,
        claimed: FactAvailability,
        present: bool,
    },
    /// Source-file authority cannot exist independently of a source span.
    SourceFileWithoutSource {
        source: FactAvailability,
        source_file: FactAvailability,
    },
}

/// Owned cold authority columns.  This is crate-private so all row insertion
/// remains coupled to the matching entity insertion in [`crate::IrBuilder`].
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
