//! Complete, reader-independent discovery over one validated semantic image.
//!
//! Discovery is deliberately computed from [`crate::ir::SemanticReader`], so an
//! owned compile result and a durable reopened image expose the same census
//! without a renderer cache or a second fact model.

use core::fmt;

use crate::ir::{
    AtomListId, DocId, EntityId, EntityListId, FactAvailability, ParentageAuthority,
    SemanticImageFacts, SemanticReader,
};

/// Counts captured and explicitly unavailable rows in one authority plane.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AvailabilityCensus {
    pub captured: usize,
    pub unavailable: usize,
}

/// Exact containment-authority distribution for the discovered declarations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ParentageCensus {
    pub unavailable: usize,
    pub roots: usize,
    pub bound: usize,
    pub unrepresented_authority_owner: usize,
}

/// Exact availability distribution for every common declaration fact plane.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EntityAuthorityCensus {
    pub parentage: ParentageCensus,
    pub source: AvailabilityCensus,
    pub source_file: AvailabilityCensus,
    pub members: AvailabilityCensus,
    pub semantic_type: AvailabilityCensus,
    pub documentation: AvailabilityCensus,
    pub visibility: AvailabilityCensus,
    pub attributes: AvailabilityCensus,
    pub language_extension: AvailabilityCensus,
}

/// Exact row counts for the seven typed sparse language-extension planes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LanguageExtensionCensus {
    pub typescript: usize,
    pub csharp: usize,
    pub go: usize,
    pub rust: usize,
    pub python: usize,
    pub java: usize,
    pub clang: usize,
}

/// Complete structural and authority census of one semantic image.
///
/// Counts use the host address-space width because every value was observed
/// through an exact-size borrowed cursor. No saturating narrowing is applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticImageCensus {
    pub image: SemanticImageFacts,
    pub entities: usize,
    pub root_entities: usize,
    pub typed_entities: usize,
    pub source_bound_entities: usize,
    pub member_references: usize,
    pub documentation_fragments: usize,
    pub attribute_references: usize,
    pub types: usize,
    pub external_targets: usize,
    pub links: usize,
    pub link_occurrences: usize,
    pub occurrence_source_authority: AvailabilityCensus,
    pub language_extensions: LanguageExtensionCensus,
    pub entity_authority: EntityAuthorityCensus,
}

/// Exact pooled reference which a purported complete reader failed to lend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticDiscoveryReference {
    Members(EntityListId),
    Documentation(DocId),
    Attributes(AtomListId),
    OccurrenceAuthority(crate::ir::LinkOccurrenceId),
}

/// A complete semantic reader exposed a coordinate without its required row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticDiscoveryError {
    pub entity: Option<EntityId>,
    pub reference: SemanticDiscoveryReference,
}

impl fmt::Display for SemanticDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "complete semantic image is missing {:?} for entity {:?}",
            self.reference, self.entity
        )
    }
}

impl core::error::Error for SemanticDiscoveryError {}

/// One allocation-free discovery session over an owned or reopened image.
#[derive(Clone, Copy)]
pub struct SemanticImageDiscovery<'image, Reader: SemanticReader + ?Sized> {
    pub reader: &'image Reader,
}

impl<'image, Reader: SemanticReader + ?Sized> SemanticImageDiscovery<'image, Reader> {
    /// Borrows a complete semantic reader as the single discovery authority.
    #[must_use]
    pub const fn new(reader: &'image Reader) -> Self {
        Self { reader }
    }

    /// Traverses every common row, graph plane, type coordinate, external
    /// target, and language extension exactly once or retains the exact broken
    /// pooled coordinate from an invalid reader implementation.
    pub fn census(self) -> Result<SemanticImageCensus, SemanticDiscoveryError> {
        let mut entity_authority = EntityAuthorityCensus::default();
        let mut root_entities = 0;
        let mut typed_entities = 0;
        let mut source_bound_entities = 0;
        let mut member_references = 0;
        let mut documentation_fragments = 0;
        let mut attribute_references = 0;
        let mut links = 0;

        let entities = self.reader.canonical_entities();
        let entity_count = entities.len();
        for entity in entities {
            observe_parentage(&mut entity_authority.parentage, entity.authority.parentage);
            observe_availability(&mut entity_authority.source, entity.authority.source);
            observe_availability(
                &mut entity_authority.source_file,
                entity.authority.source_file,
            );
            observe_availability(&mut entity_authority.members, entity.authority.members);
            observe_availability(
                &mut entity_authority.semantic_type,
                entity.authority.semantic_type,
            );
            observe_availability(
                &mut entity_authority.documentation,
                entity.authority.documentation,
            );
            observe_availability(
                &mut entity_authority.visibility,
                entity.authority.visibility,
            );
            observe_availability(
                &mut entity_authority.attributes,
                entity.authority.attributes,
            );
            observe_availability(
                &mut entity_authority.language_extension,
                entity.authority.language_extension,
            );

            root_entities += usize::from(entity.parent.is_none());
            typed_entities += usize::from(entity.semantic_type.is_some());
            source_bound_entities += usize::from(entity.source.is_some());
            member_references += self
                .reader
                .entity_list(entity.members)
                .ok_or(SemanticDiscoveryError {
                    entity: Some(entity.id),
                    reference: SemanticDiscoveryReference::Members(entity.members),
                })?
                .len();
            documentation_fragments += self
                .reader
                .docs(entity.docs)
                .ok_or(SemanticDiscoveryError {
                    entity: Some(entity.id),
                    reference: SemanticDiscoveryReference::Documentation(entity.docs),
                })?
                .len();
            attribute_references += self
                .reader
                .atom_list(entity.attributes)
                .ok_or(SemanticDiscoveryError {
                    entity: Some(entity.id),
                    reference: SemanticDiscoveryReference::Attributes(entity.attributes),
                })?
                .len();
            links += self.reader.links_from(entity.id).len();
        }

        let occurrences = self.reader.link_occurrences();
        let link_occurrences = occurrences.len();
        let mut occurrence_source_authority = AvailabilityCensus::default();
        for (id, _) in occurrences {
            let authority = self
                .reader
                .occurrence_authority(id)
                .ok_or(SemanticDiscoveryError {
                    entity: None,
                    reference: SemanticDiscoveryReference::OccurrenceAuthority(id),
                })?;
            observe_availability(&mut occurrence_source_authority, authority.source);
        }

        let language_extensions = LanguageExtensionCensus {
            typescript: self.reader.typescript_extensions().len(),
            csharp: self.reader.csharp_extensions().len(),
            go: self.reader.go_extensions().len(),
            rust: self.reader.rust_extensions().len(),
            python: self.reader.python_extensions().len(),
            java: self.reader.java_extensions().len(),
            clang: self.reader.clang_extensions().len(),
        };

        Ok(SemanticImageCensus {
            image: self.reader.image_facts(),
            entities: entity_count,
            root_entities,
            typed_entities,
            source_bound_entities,
            member_references,
            documentation_fragments,
            attribute_references,
            types: self.reader.canonical_types().len(),
            external_targets: self.reader.canonical_externals().len(),
            links,
            link_occurrences,
            occurrence_source_authority,
            language_extensions,
            entity_authority,
        })
    }
}

fn observe_availability(census: &mut AvailabilityCensus, value: FactAvailability) {
    match value {
        FactAvailability::Captured => census.captured += 1,
        FactAvailability::Unavailable => census.unavailable += 1,
    }
}

fn observe_parentage(census: &mut ParentageCensus, value: ParentageAuthority) {
    match value {
        ParentageAuthority::Unavailable => census.unavailable += 1,
        ParentageAuthority::Root => census.roots += 1,
        ParentageAuthority::Bound(_) => census.bound += 1,
        ParentageAuthority::UnrepresentedAuthorityOwner(_) => {
            census.unrepresented_authority_owner += 1;
        }
    }
}
