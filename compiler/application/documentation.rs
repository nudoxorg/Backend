//! Rustdoc-grade semantic documentation projection.
//!
//! This module owns no parallel catalogue and never makes a stringly
//! approximation of semantic state.  A session borrows a finalized
//! [`compiler_ir::SemanticReader`], so owned [`compiler_ir::Ir`] and a
//! validated reopened [`compiler_ir::SemanticImageView`] expose the identical
//! declaration, documentation, type, and graph facts.

use core::{iter::FusedIterator, ops::Deref};

use compiler_ir::{
    prepare_canonical_type, AtomId, CanonicalTypeRenderError, CanonicalTypeRenderLimits,
    DeclarationIdentity, DocFragment, DocId, EntityId, EntityListId, ExternalId, ExternalTarget,
    Link, LinkId, LinkTarget, PreparedCanonicalType, SemanticEntity, SemanticImageFacts,
    SemanticReader, TextId, TypeExpr, TypeId,
};
use thiserror::Error;

/// Immutable image facts shared by all pages in one documentation session.
///
/// The session owns neither source bytes nor semantic data.  It can therefore
/// be instantiated over an owned image or a reopened publication without a
/// second discovery/catalogue pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentationSessionView {
    /// Exact authority and provenance admitted by the underlying image.
    pub image: SemanticImageFacts,
}

/// Borrowed documentation projection over one complete semantic image.
pub struct DocumentationSession<'image, Reader: SemanticReader + ?Sized> {
    reader: &'image Reader,
    view: DocumentationSessionView,
}

impl<Reader: SemanticReader + ?Sized> Deref for DocumentationSession<'_, Reader> {
    type Target = DocumentationSessionView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<'image, Reader: SemanticReader + ?Sized> DocumentationSession<'image, Reader> {
    /// Starts a projection over a fully admitted semantic image.
    #[must_use]
    pub fn new(reader: &'image Reader) -> Self {
        Self {
            reader,
            view: DocumentationSessionView {
                image: reader.image_facts(),
            },
        }
    }

    /// Resolves one exact local declaration coordinate.
    pub fn entity(
        &self,
        id: EntityId,
    ) -> Result<DocumentationEntity<'image, Reader>, DocumentationProjectionError> {
        let entity = self
            .reader
            .entity(id)
            .ok_or(DocumentationProjectionError::MissingEntity { entity: id })?;
        documentation_entity(self.reader, entity)
    }

    /// Resolves one exact composite declaration identity.
    pub fn entity_by_identity(
        &self,
        identity: DeclarationIdentity,
    ) -> Result<DocumentationEntity<'image, Reader>, DocumentationProjectionError> {
        let entity = self
            .reader
            .entity_by_identity(identity)
            .ok_or(DocumentationProjectionError::MissingDeclarationIdentity { identity })?;
        documentation_entity(self.reader, entity)
    }

    /// Resolves one canonical graph relation without manufacturing its target.
    pub fn relation(
        &self,
        link: LinkId,
    ) -> Result<DocumentationRelation<'image>, DocumentationProjectionError> {
        let facts = self
            .reader
            .link(link)
            .ok_or(DocumentationProjectionError::MissingRelation { link })?;
        documentation_relation(self.reader, facts.from, link, facts)
    }

    /// Traverses declaration pages in the reader's canonical entity order.
    #[must_use]
    pub fn canonical_entities(&self) -> CanonicalDocumentationEntities<'image, Reader> {
        CanonicalDocumentationEntities {
            reader: self.reader,
            rows: self.reader.canonical_entities(),
        }
    }
}

/// Immutable declaration facts suitable for a documentation page.
///
/// Names intentionally remain raw bytes.  Semantic atoms may be binary, and
/// callers that need a textual policy must make that policy explicit rather
/// than having this projection silently replace or discard bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentationEntityView<'image> {
    /// Exact finalized declaration row.
    pub entity: SemanticEntity,
    /// Exact declaration-name atom bytes.
    pub name: &'image [u8],
}

/// One documentation declaration page backed by a semantic reader.
pub struct DocumentationEntity<'image, Reader: SemanticReader + ?Sized> {
    reader: &'image Reader,
    view: DocumentationEntityView<'image>,
}

impl<'image, Reader: SemanticReader + ?Sized> Deref for DocumentationEntity<'image, Reader> {
    type Target = DocumentationEntityView<'image>;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<'image, Reader: SemanticReader + ?Sized> DocumentationEntity<'image, Reader> {
    /// Traverses this declaration's explicitly retained local member order.
    ///
    /// Each yielded child is checked against its local parent coordinate.  A
    /// malformed reader cannot become a page hierarchy by merely listing an
    /// unrelated entity in a member pool.
    pub fn members(
        &self,
    ) -> Result<DocumentationMembers<'image, Reader>, DocumentationProjectionError> {
        let entity = self.view.entity;
        let members = self.reader.entity_list(entity.members).ok_or(
            DocumentationProjectionError::MissingMembers {
                entity: entity.id,
                members: entity.members,
            },
        )?;
        Ok(DocumentationMembers {
            reader: self.reader,
            owner: entity.id,
            members_id: entity.members,
            members,
        })
    }

    /// Traverses exact documentation fragments in their retained order.
    pub fn documentation(
        &self,
    ) -> Result<DocumentationFragments<'image, Reader>, DocumentationProjectionError> {
        let entity = self.view.entity;
        let fragments = self.reader.docs(entity.docs).ok_or(
            DocumentationProjectionError::MissingDocumentation {
                entity: entity.id,
                documentation: entity.docs,
            },
        )?;
        Ok(DocumentationFragments {
            reader: self.reader,
            owner: entity.id,
            documentation: entity.docs,
            next_fragment: 0,
            fragments,
        })
    }

    /// Traverses canonical graph relations emitted from this declaration.
    #[must_use]
    pub fn relations(&self) -> DocumentationRelations<'image, Reader> {
        DocumentationRelations {
            reader: self.reader,
            owner: self.view.entity.id,
            relations: self.reader.links_from(self.view.entity.id),
        }
    }

    /// Resolves the declaration's captured semantic type, when present.
    pub fn semantic_type(
        &self,
    ) -> Result<Option<DocumentationType<'image, Reader>>, DocumentationProjectionError> {
        let entity = self.view.entity;
        let Some(id) = entity.semantic_type else {
            return Ok(None);
        };
        let expression =
            self.reader
                .ty(id)
                .ok_or(DocumentationProjectionError::MissingSemanticType {
                    entity: entity.id,
                    semantic_type: id,
                })?;
        Ok(Some(DocumentationType {
            reader: self.reader,
            view: DocumentationTypeView { id, expression },
        }))
    }
}

/// One typed semantic type available to a documentation page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentationTypeView {
    /// Exact semantic type coordinate.
    pub id: TypeId,
    /// Exact tagged semantic expression.
    pub expression: TypeExpr,
}

/// A documentation type that can be rendered through the established
/// allocation-free canonical type renderer.
pub struct DocumentationType<'image, Reader: SemanticReader + ?Sized> {
    reader: &'image Reader,
    view: DocumentationTypeView,
}

impl<Reader: SemanticReader + ?Sized> Deref for DocumentationType<'_, Reader> {
    type Target = DocumentationTypeView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<'image, Reader: SemanticReader + ?Sized> DocumentationType<'image, Reader> {
    /// Validates and measures canonical type output without allocating output
    /// storage.  The returned prepared value writes atomically into a caller
    /// byte buffer.
    pub fn prepare_canonical(
        &self,
        limits: CanonicalTypeRenderLimits,
    ) -> Result<PreparedCanonicalType<'image, Reader>, DocumentationProjectionError> {
        prepare_canonical_type(self.reader, self.id, limits).map_err(|cause| {
            DocumentationProjectionError::CanonicalType {
                semantic_type: self.id,
                cause,
            }
        })
    }
}

/// Exact target behind a documentation fragment or graph relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentationTarget<'image> {
    /// A retained local declaration page.
    Local(DocumentationEntityView<'image>),
    /// An exact stable, foreign, or legacy external endpoint.
    External(ExternalTarget),
}

/// Documentation markup after text and targets have been resolved exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentationFragment<'image> {
    /// UTF-8 text admitted by the semantic document pool.
    Text(&'image str),
    /// UTF-8 code text admitted by the semantic document pool.
    Code(&'image str),
    /// A label and an exact, non-fabricated semantic target.
    Link {
        /// UTF-8 label admitted by the semantic document pool.
        label: &'image str,
        /// Exact semantic link target.
        target: DocumentationTarget<'image>,
    },
    /// A retained soft line break.
    SoftBreak,
    /// A retained hard line break.
    HardBreak,
}

/// One retained canonical graph relation with its exact target resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentationRelation<'image> {
    /// Exact canonical relation coordinate.
    pub id: LinkId,
    /// Exact relation evidence and source facts.
    pub facts: Link,
    /// Exact local or external target.
    pub target: DocumentationTarget<'image>,
}

/// Cursor over canonical documentation declaration pages.
pub struct CanonicalDocumentationEntities<'image, Reader: SemanticReader + ?Sized + 'image> {
    reader: &'image Reader,
    rows: Reader::CanonicalEntities<'image>,
}

impl<'image, Reader: SemanticReader + ?Sized + 'image> Iterator
    for CanonicalDocumentationEntities<'image, Reader>
{
    type Item = Result<DocumentationEntity<'image, Reader>, DocumentationProjectionError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.rows
            .next()
            .map(|entity| documentation_entity(self.reader, entity))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.rows.size_hint()
    }
}

impl<Reader: SemanticReader + ?Sized> ExactSizeIterator
    for CanonicalDocumentationEntities<'_, Reader>
{
}
impl<Reader: SemanticReader + ?Sized> FusedIterator for CanonicalDocumentationEntities<'_, Reader> {}

/// Cursor over one declaration's retained member sequence.
pub struct DocumentationMembers<'image, Reader: SemanticReader + ?Sized + 'image> {
    reader: &'image Reader,
    owner: EntityId,
    members_id: EntityListId,
    members: Reader::Entities<'image>,
}

impl<'image, Reader: SemanticReader + ?Sized + 'image> Iterator
    for DocumentationMembers<'image, Reader>
{
    type Item = Result<DocumentationEntity<'image, Reader>, DocumentationProjectionError>;

    fn next(&mut self) -> Option<Self::Item> {
        let member = self.members.next()?;
        let entity = match self.reader.entity(member) {
            Some(entity) => entity,
            None => {
                return Some(Err(DocumentationProjectionError::MissingMember {
                    entity: self.owner,
                    members: self.members_id,
                    member,
                }));
            }
        };
        if entity.parent != Some(self.owner) {
            return Some(Err(DocumentationProjectionError::MemberParentageMismatch {
                entity: self.owner,
                members: self.members_id,
                member,
                observed_parent: entity.parent,
            }));
        }
        Some(documentation_entity(self.reader, entity))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.members.size_hint()
    }
}

impl<Reader: SemanticReader + ?Sized> ExactSizeIterator for DocumentationMembers<'_, Reader> {}
impl<Reader: SemanticReader + ?Sized> FusedIterator for DocumentationMembers<'_, Reader> {}

/// Cursor over one declaration's exact documentation fragments.
pub struct DocumentationFragments<'image, Reader: SemanticReader + ?Sized + 'image> {
    reader: &'image Reader,
    owner: EntityId,
    documentation: DocId,
    next_fragment: usize,
    fragments: Reader::Docs<'image>,
}

impl<'image, Reader: SemanticReader + ?Sized + 'image> Iterator
    for DocumentationFragments<'image, Reader>
{
    type Item = Result<DocumentationFragment<'image>, DocumentationProjectionError>;

    fn next(&mut self) -> Option<Self::Item> {
        let fragment = self.fragments.next()?;
        let index = self.next_fragment;
        self.next_fragment = match self.next_fragment.checked_add(1) {
            Some(next) => next,
            None => {
                return Some(Err(DocumentationProjectionError::FragmentIndexOverflow {
                    entity: self.owner,
                    documentation: self.documentation,
                    fragment: index,
                }));
            }
        };
        Some(documentation_fragment(
            self.reader,
            self.owner,
            self.documentation,
            index,
            fragment,
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.fragments.size_hint()
    }
}

impl<Reader: SemanticReader + ?Sized> ExactSizeIterator for DocumentationFragments<'_, Reader> {}
impl<Reader: SemanticReader + ?Sized> FusedIterator for DocumentationFragments<'_, Reader> {}

/// Cursor over one declaration's canonical graph relations.
pub struct DocumentationRelations<'image, Reader: SemanticReader + ?Sized + 'image> {
    reader: &'image Reader,
    owner: EntityId,
    relations: Reader::Links<'image>,
}

impl<'image, Reader: SemanticReader + ?Sized + 'image> Iterator
    for DocumentationRelations<'image, Reader>
{
    type Item = Result<DocumentationRelation<'image>, DocumentationProjectionError>;

    fn next(&mut self) -> Option<Self::Item> {
        let (id, relation) = self.relations.next()?;
        if relation.from != self.owner {
            return Some(Err(DocumentationProjectionError::RelationOwnerMismatch {
                requested: self.owner,
                link: id,
                observed: relation.from,
            }));
        }
        Some(documentation_relation(
            self.reader,
            self.owner,
            id,
            relation,
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.relations.size_hint()
    }
}

impl<Reader: SemanticReader + ?Sized> ExactSizeIterator for DocumentationRelations<'_, Reader> {}
impl<Reader: SemanticReader + ?Sized> FusedIterator for DocumentationRelations<'_, Reader> {}

/// Exact semantic-projection terminal.  Every missing coordinate keeps its
/// owner and source pool rather than becoming an empty page or plain string.
#[derive(Debug, Error)]
pub enum DocumentationProjectionError {
    /// A requested local declaration coordinate was absent.
    #[error("documentation requested missing declaration {entity:?}")]
    MissingEntity {
        /// Requested local declaration coordinate.
        entity: EntityId,
    },
    /// A requested composite declaration identity was absent.
    #[error("documentation requested missing declaration identity {identity:?}")]
    MissingDeclarationIdentity {
        /// Requested exact composite declaration identity.
        identity: DeclarationIdentity,
    },
    /// A declaration named an absent raw name atom.
    #[error("documentation declaration {entity:?} names missing atom {name:?}")]
    MissingName {
        /// Declaration which carried the absent atom coordinate.
        entity: EntityId,
        /// Exact absent atom coordinate.
        name: AtomId,
    },
    /// A declaration named an absent pooled member list.
    #[error("documentation declaration {entity:?} names missing members list {members:?}")]
    MissingMembers {
        /// Owning declaration.
        entity: EntityId,
        /// Exact absent pooled member list.
        members: EntityListId,
    },
    /// A member list named an absent local declaration.
    #[error("documentation member list {members:?} for {entity:?} names absent member {member:?}")]
    MissingMember {
        /// Owning declaration.
        entity: EntityId,
        /// Source member list.
        members: EntityListId,
        /// Exact absent child coordinate.
        member: EntityId,
    },
    /// A retained member declaration disagreed with the list owner's parentage.
    #[error("documentation member {member:?} in {members:?} belongs to {observed_parent:?}, not {entity:?}")]
    MemberParentageMismatch {
        /// Owning declaration named by the member list.
        entity: EntityId,
        /// Source member list.
        members: EntityListId,
        /// Child coordinate.
        member: EntityId,
        /// Parent actually retained by the child row.
        observed_parent: Option<EntityId>,
    },
    /// A declaration named an absent pooled document list.
    #[error("documentation declaration {entity:?} names missing document list {documentation:?}")]
    MissingDocumentation {
        /// Owning declaration.
        entity: EntityId,
        /// Exact absent pooled document list.
        documentation: DocId,
    },
    /// A documentation fragment named an absent UTF-8 text atom.
    #[error("documentation fragment {fragment} in {documentation:?} for {entity:?} names missing {part:?} text {text:?}")]
    MissingDocumentationText {
        /// Owning declaration.
        entity: EntityId,
        /// Source documentation list.
        documentation: DocId,
        /// Fragment position in that list.
        fragment: usize,
        /// Exact role which carried the absent text coordinate.
        part: DocumentationTextPart,
        /// Exact absent text coordinate.
        text: TextId,
    },
    /// The projection cursor could not represent another documentation position.
    #[error("documentation fragment index overflowed after {fragment} in {documentation:?} for {entity:?}")]
    FragmentIndexOverflow {
        /// Owning declaration.
        entity: EntityId,
        /// Source documentation list.
        documentation: DocId,
        /// Last representable fragment position.
        fragment: usize,
    },
    /// A documentation fragment or graph relation named an absent local target.
    #[error("documentation {reference:?} from {entity:?} names missing local target {target:?}")]
    MissingLocalTarget {
        /// Declaration that owns the reference.
        entity: EntityId,
        /// Exact source reference.
        reference: DocumentationReference,
        /// Exact absent local target coordinate.
        target: EntityId,
    },
    /// A documentation fragment or graph relation named an absent external target.
    #[error(
        "documentation {reference:?} from {entity:?} names missing external target {target:?}"
    )]
    MissingExternalTarget {
        /// Declaration that owns the reference.
        entity: EntityId,
        /// Exact source reference.
        reference: DocumentationReference,
        /// Exact absent external target coordinate.
        target: ExternalId,
    },
    /// A requested canonical graph relation coordinate was absent.
    #[error("documentation requested missing graph relation {link:?}")]
    MissingRelation {
        /// Exact absent canonical relation coordinate.
        link: LinkId,
    },
    /// A page traversal returned a relation owned by another declaration.
    #[error(
        "documentation relation {link:?} requested from {requested:?} is owned by {observed:?}"
    )]
    RelationOwnerMismatch {
        /// Entity requested by the page traversal.
        requested: EntityId,
        /// Relation coordinate.
        link: LinkId,
        /// Relation's exact retained owner.
        observed: EntityId,
    },
    /// A declaration named an absent semantic type coordinate.
    #[error("documentation declaration {entity:?} names missing semantic type {semantic_type:?}")]
    MissingSemanticType {
        /// Owning declaration.
        entity: EntityId,
        /// Exact absent type coordinate.
        semantic_type: TypeId,
    },
    /// The shared canonical type renderer rejected the exact semantic type.
    #[error("canonical documentation type {semantic_type:?} was rejected")]
    CanonicalType {
        /// Type passed to canonical rendering.
        semantic_type: TypeId,
        /// Exact render terminal.
        #[source]
        cause: CanonicalTypeRenderError,
    },
}

/// Exact role of a missing UTF-8 document atom.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentationTextPart {
    /// A normal prose fragment.
    Text,
    /// A code fragment.
    Code,
    /// A link label.
    LinkLabel,
}

/// Exact source coordinate of a local or external documentation target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentationReference {
    /// A link embedded in a documentation fragment.
    Fragment {
        /// Documentation list containing the fragment.
        documentation: DocId,
        /// Fragment position in that list.
        fragment: usize,
    },
    /// A graph relation emitted from the declaration.
    Relation {
        /// Canonical graph relation coordinate.
        link: LinkId,
    },
}

fn documentation_entity<'image, Reader: SemanticReader + ?Sized>(
    reader: &'image Reader,
    entity: SemanticEntity,
) -> Result<DocumentationEntity<'image, Reader>, DocumentationProjectionError> {
    let name = reader
        .atom(entity.name)
        .ok_or(DocumentationProjectionError::MissingName {
            entity: entity.id,
            name: entity.name,
        })?;
    Ok(DocumentationEntity {
        reader,
        view: DocumentationEntityView { entity, name },
    })
}

fn documentation_fragment<'image, Reader: SemanticReader + ?Sized>(
    reader: &'image Reader,
    owner: EntityId,
    documentation: DocId,
    fragment: usize,
    value: DocFragment,
) -> Result<DocumentationFragment<'image>, DocumentationProjectionError> {
    match value {
        DocFragment::Text(text) => {
            let text = documentation_text(
                reader,
                owner,
                documentation,
                fragment,
                DocumentationTextPart::Text,
                text,
            )?;
            Ok(DocumentationFragment::Text(text))
        }
        DocFragment::Code(text) => {
            let text = documentation_text(
                reader,
                owner,
                documentation,
                fragment,
                DocumentationTextPart::Code,
                text,
            )?;
            Ok(DocumentationFragment::Code(text))
        }
        DocFragment::Link { label, target } => {
            let label = documentation_text(
                reader,
                owner,
                documentation,
                fragment,
                DocumentationTextPart::LinkLabel,
                label,
            )?;
            let target = documentation_target(
                reader,
                owner,
                DocumentationReference::Fragment {
                    documentation,
                    fragment,
                },
                target,
            )?;
            Ok(DocumentationFragment::Link { label, target })
        }
        DocFragment::SoftBreak => Ok(DocumentationFragment::SoftBreak),
        DocFragment::HardBreak => Ok(DocumentationFragment::HardBreak),
    }
}

fn documentation_text<'image, Reader: SemanticReader + ?Sized>(
    reader: &'image Reader,
    entity: EntityId,
    documentation: DocId,
    fragment: usize,
    part: DocumentationTextPart,
    text: TextId,
) -> Result<&'image str, DocumentationProjectionError> {
    reader
        .text(text)
        .ok_or(DocumentationProjectionError::MissingDocumentationText {
            entity,
            documentation,
            fragment,
            part,
            text,
        })
}

fn documentation_relation<'image, Reader: SemanticReader + ?Sized>(
    reader: &'image Reader,
    owner: EntityId,
    id: LinkId,
    facts: Link,
) -> Result<DocumentationRelation<'image>, DocumentationProjectionError> {
    let target = documentation_target(
        reader,
        owner,
        DocumentationReference::Relation { link: id },
        facts.target,
    )?;
    Ok(DocumentationRelation { id, facts, target })
}

fn documentation_target<'image, Reader: SemanticReader + ?Sized>(
    reader: &'image Reader,
    owner: EntityId,
    reference: DocumentationReference,
    target: LinkTarget,
) -> Result<DocumentationTarget<'image>, DocumentationProjectionError> {
    match target {
        LinkTarget::Local(target) => {
            let entity =
                reader
                    .entity(target)
                    .ok_or(DocumentationProjectionError::MissingLocalTarget {
                        entity: owner,
                        reference,
                        target,
                    })?;
            let view = documentation_entity(reader, entity)?.view;
            Ok(DocumentationTarget::Local(view))
        }
        LinkTarget::External(target) => {
            let target = reader.external(target).ok_or(
                DocumentationProjectionError::MissingExternalTarget {
                    entity: owner,
                    reference,
                    target,
                },
            )?;
            Ok(DocumentationTarget::External(target))
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroUsize;

    use compiler_ir::{
        encode_full_semantic_image, full_semantic_image_len, BorrowedTree, BuiltinType,
        ConcreteType, Confidence, CorePayloadHash, DeclarationFamilyId, DocInput,
        EntityAuthorityFacts, EntityVersion, FactAvailability, Ir, IrBuilder, ItemKind, LinkKind,
        OccurrenceAuthorityFacts, ParentageAuthority, SemanticImageView, TreeEntityId,
        TreeItemInput, TreeLinkInput, TreeLinkTarget, VariantFingerprint, Visibility,
    };

    use super::{DocumentationFragment, DocumentationSession, DocumentationTarget};

    #[derive(Debug, Eq, PartialEq)]
    enum FragmentObservation {
        Text(String),
        Code(String),
        Link {
            label: String,
            target: compiler_ir::DeclarationIdentity,
        },
        SoftBreak,
        HardBreak,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct DocumentationObservation {
        canonical_names: Vec<(compiler_ir::DeclarationIdentity, Vec<u8>)>,
        members: Vec<compiler_ir::DeclarationIdentity>,
        fragments: Vec<FragmentObservation>,
        type_output: Vec<u8>,
        relation_target: compiler_ir::DeclarationIdentity,
    }

    fn version(family: u8, variant: u8, payload: u8) -> EntityVersion {
        EntityVersion {
            family: DeclarationFamilyId::from_raw([family; 16]),
            variant: VariantFingerprint::from_raw([variant; 16]),
            core_payload: CorePayloadHash::from_raw([payload; 16]),
        }
    }

    fn root_authority() -> EntityAuthorityFacts {
        EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        }
    }

    fn child_authority(parent: compiler_ir::DeclarationIdentity) -> EntityAuthorityFacts {
        EntityAuthorityFacts {
            parentage: ParentageAuthority::Bound(parent),
            semantic_type: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        }
    }

    fn documented_image() -> Ir {
        let mut builder = IrBuilder::new();
        let string = builder
            .intern_concrete(ConcreteType::Builtin(BuiltinType::String))
            .expect("string type fixture is admitted");
        let versions = [version(1, 2, 3), version(4, 5, 6)];
        let members = [TreeEntityId::new(1)];
        let root_docs = [
            DocInput::Text("A documented module."),
            DocInput::SoftBreak,
            DocInput::Link {
                label: "member",
                target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            },
            DocInput::HardBreak,
            DocInput::Code("example::member"),
        ];
        let child_docs = [DocInput::Text("The documented member.")];
        let items = [
            TreeItemInput {
                name: b"crate",
                kind: ItemKind::Module,
                visibility: Visibility::Public,
                authority: root_authority(),
                parent: None,
                semantic_type: None,
                members: &members,
                docs: &root_docs,
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"member",
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: child_authority(versions[0].identity()),
                parent: Some(TreeEntityId::new(0)),
                semantic_type: Some(string.erase()),
                members: &[],
                docs: &child_docs,
                attributes: &[],
                source: None,
                extension: None,
            },
        ];
        let links = [TreeLinkInput {
            from: TreeEntityId::new(1),
            target: TreeLinkTarget::Local(TreeEntityId::new(0)),
            kind: LinkKind::TypeReference,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts::default(),
            source: None,
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &links,
            })
            .expect("documentation fixture is admitted");
        builder.finish().expect("documentation image is finalized")
    }

    fn observe<Reader: compiler_ir::SemanticReader + ?Sized>(
        reader: &Reader,
    ) -> DocumentationObservation {
        let session = DocumentationSession::new(reader);
        let canonical_names = session
            .canonical_entities()
            .map(|page| {
                let page = page.expect("canonical declaration has a name atom");
                (page.entity.version.identity(), page.name.to_vec())
            })
            .collect();

        let root = session
            .entity_by_identity(version(1, 2, 3).identity())
            .expect("root is present by identity");
        let members = root
            .members()
            .expect("root member pool is present")
            .map(|member| {
                member
                    .expect("member row is present and parented")
                    .entity
                    .version
                    .identity()
            })
            .collect();
        let fragments = root
            .documentation()
            .expect("root documentation is present")
            .map(
                |fragment| match fragment.expect("documentation fragment resolves") {
                    DocumentationFragment::Text(text) => FragmentObservation::Text(text.into()),
                    DocumentationFragment::Code(text) => FragmentObservation::Code(text.into()),
                    DocumentationFragment::Link { label, target } => match target {
                        DocumentationTarget::Local(entity) => FragmentObservation::Link {
                            label: label.into(),
                            target: entity.entity.version.identity(),
                        },
                        DocumentationTarget::External(_) => {
                            panic!("fixture documentation link must remain local")
                        }
                    },
                    DocumentationFragment::SoftBreak => FragmentObservation::SoftBreak,
                    DocumentationFragment::HardBreak => FragmentObservation::HardBreak,
                },
            )
            .collect();

        let child = root
            .members()
            .expect("root member pool is present")
            .next()
            .expect("one member")
            .expect("member projection is exact");
        let semantic_type = child
            .semantic_type()
            .expect("member semantic type is present")
            .expect("member carries captured type");
        let prepared = semantic_type
            .prepare_canonical(compiler_ir::CanonicalTypeRenderLimits::new(
                NonZeroUsize::new(16).expect("nonzero test bound"),
            ))
            .expect("canonical type is admitted");
        let mut type_output = vec![0; prepared.encoded_len];
        let rendered_len = prepared
            .write_into(&mut type_output)
            .expect("caller buffer has prepared length")
            .len();
        assert_eq!(rendered_len, type_output.len());

        let relation = child
            .relations()
            .next()
            .expect("child relation is retained")
            .expect("relation target resolves");
        let DocumentationTarget::Local(relation_target) = relation.target else {
            panic!("fixture relation must remain local");
        };
        DocumentationObservation {
            canonical_names,
            members,
            fragments,
            type_output,
            relation_target: relation_target.entity.version.identity(),
        }
    }

    #[test]
    fn documentation_projection_matches_owned_and_reopened_semantic_images() {
        let ir = documented_image();
        let owned = observe(&ir);

        let length = full_semantic_image_len(&ir).expect("full semantic image is planned");
        let mut bytes = vec![0; length];
        let written = encode_full_semantic_image(&ir, &mut bytes)
            .expect("full semantic image writes atomically");
        assert_eq!(written, bytes.len());
        let reopened = SemanticImageView::reopen(&bytes).expect("full semantic image reopens");

        assert_eq!(owned, observe(&reopened));
        assert_eq!(
            owned.type_output,
            b"builtin(string)".as_slice(),
            "the documentation type view uses the shared canonical renderer"
        );
    }
}
