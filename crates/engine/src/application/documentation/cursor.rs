//! Documentation page cursors over an admitted semantic image.

use core::iter::FusedIterator;

use backend_semantic::ir::{
    DocFragment, DocId, EntityId, EntityListId, Link, LinkId, LinkTarget, SemanticEntity,
    SemanticReader, TextId,
};

use super::{
    DocumentationEntity, DocumentationEntityView, DocumentationFragment,
    DocumentationProjectionError, DocumentationReference, DocumentationRelation,
    DocumentationTarget, DocumentationTextPart,
};

/// Cursor over canonical documentation declaration pages.
pub struct CanonicalDocumentationEntities<'image, Reader: SemanticReader + ?Sized + 'image> {
    pub(super) reader: &'image Reader,
    pub(super) rows: Reader::CanonicalEntities<'image>,
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
    pub(super) reader: &'image Reader,
    pub(super) owner: EntityId,
    pub(super) members_id: EntityListId,
    pub(super) members: Reader::Entities<'image>,
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
    pub(super) reader: &'image Reader,
    pub(super) owner: EntityId,
    pub(super) documentation: DocId,
    pub(super) next_fragment: usize,
    pub(super) fragments: Reader::Docs<'image>,
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
    pub(super) reader: &'image Reader,
    pub(super) owner: EntityId,
    pub(super) relations: Reader::Links<'image>,
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

pub(super) fn documentation_entity<'image, Reader: SemanticReader + ?Sized>(
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

pub(super) fn documentation_relation<'image, Reader: SemanticReader + ?Sized>(
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
        LinkTarget::External(target_id) => {
            let target = reader.external(target_id).ok_or(
                DocumentationProjectionError::MissingExternalTarget {
                    entity: owner,
                    reference,
                    target: target_id,
                },
            )?;
            Ok(DocumentationTarget::External {
                id: target_id,
                target,
            })
        }
    }
}
