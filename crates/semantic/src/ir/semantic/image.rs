use super::columns::{
    EntityColumns, GraphColumns, IrIndices, ItemColumns, LanguageExtensionColumnView,
    LanguageExtensions, LanguageExtensionsView, LinkOccurrenceColumns, PackedLinkOccurrences,
    PackedLinks, SourceColumns, SourceColumnsView, SparseColumnView, StorageColumns, VcsColumns,
};
use super::error::BuildError;
use super::ids::{
    AtomListId, DocId, EntityListId, ExternalId, FreePredicateListId, ItemKind, LinkId,
    LinkOccurrenceId, ObjectMemberListId, TemplatePartListId, TupleElementListId, TypeListId,
    TypeParameterBoundListId, TypeParameterListId,
};
use super::language_facts::SemanticImageAuthority;
use super::packed_types::{
    ComputedType, ConcreteType, FreePredicate, ObjectMember, PackedTypes, TemplatePart,
    TupleElement, TypeParameter, TypeParameterBound,
};
use super::relations::{
    Confidence, DocFragment, EntityVersion, ExternalTarget, Link, LinkKind, LinkOccurrence,
    LinkTarget, SourceSpan,
};
use super::type_model::{TypeColumns, TypeExpr, TypeState, TypedTypeId, UnknownType, Visibility};
use crate::ir::{
    AnnotationKind, AtomId, AtomInterner, AtomTable, AtomTableView, AuthorityFactFault,
    AuthorityFactPlane, CapacityError, ChannelDirection, DeclarationFamilyId, DeclarationIdentity,
    DeclarationKey, DenseId, EntityAuthorityColumns, EntityAuthorityFacts, EntityId,
    ExternalDeclarationIdentity, ExternalEntityRef, FactAvailability, ImageProvenance,
    ImageProvenanceClaim, Interner, ListId, ListInterner, ListTable, ListTableView,
    OccurrenceAuthorityColumns, OccurrenceAuthorityFacts, PackageLineage, ParentageAuthority,
    PreimageOverflow, SemanticScopeClaim, SemanticScopeFacts, SourceIdentity, StableRef, TextId,
    Type, TypeId, VariantFingerprint,
    authority::{AuthorityColumns, OccurrenceAuthorityColumn},
    columnar::{RawColumn, Slab, SlabPlan},
    interner::{HashIndex, hash},
};
use core::fmt;

mod access;

/// Immutable condensed semantic IR.
pub struct Ir {
    pub(in crate::ir::semantic) authority: SemanticImageAuthority,
    pub(in crate::ir::semantic) provenance: ImageProvenance,
    pub(in crate::ir::semantic) atoms: AtomTable,
    pub(in crate::ir::semantic) types: PackedTypes,
    pub(in crate::ir::semantic) externals: Vec<ExternalTarget>,
    pub(in crate::ir::semantic) type_lists: ListTable<TypeId>,
    pub(in crate::ir::semantic) entity_lists: ListTable<EntityId>,
    pub(in crate::ir::semantic) atom_lists: ListTable<AtomId>,
    pub(in crate::ir::semantic) docs: ListTable<DocFragment>,
    pub(in crate::ir::semantic) tuple_elements: ListTable<TupleElement>,
    pub(in crate::ir::semantic) object_members: ListTable<ObjectMember>,
    pub(in crate::ir::semantic) template_parts: ListTable<TemplatePart>,
    pub(in crate::ir::semantic) type_parameter_bounds: ListTable<TypeParameterBound>,
    pub(in crate::ir::semantic) type_parameters: ListTable<TypeParameter>,
    pub(in crate::ir::semantic) free_predicates: ListTable<FreePredicate>,
    pub(in crate::ir::semantic) items: ItemColumns,
    pub(in crate::ir::semantic) authority_facts: AuthorityColumns,
    pub(in crate::ir::semantic) sources: SourceColumns,
    pub(in crate::ir::semantic) extensions: LanguageExtensions,
    pub(in crate::ir::semantic) indices: IrIndices,
    pub(in crate::ir::semantic) kind_offsets: [u32; 16],
    pub(in crate::ir::semantic) links: PackedLinks,
    pub(in crate::ir::semantic) link_occurrences: PackedLinkOccurrences,
    pub(in crate::ir::semantic) occurrence_authority: OccurrenceAuthorityColumn,
}

/// Borrowing entity handle; all child/doc/edge views inherit one IR lifetime.
#[derive(Clone, Copy)]
pub struct ItemView<'ir> {
    ir: &'ir Ir,
    id: EntityId,
}

impl<'ir> ItemView<'ir> {
    #[must_use]
    pub const fn id(self) -> EntityId {
        self.id
    }
    #[must_use]
    pub fn kind(self) -> ItemKind {
        self.ir.items.kinds[self.id.index()]
    }
    #[must_use]
    pub fn visibility(self) -> Visibility {
        self.ir.items.visibility[self.id.index()]
    }
    #[must_use]
    pub fn parent(self) -> Option<EntityId> {
        self.ir.items.parents[self.id.index()].get()
    }
    #[must_use]
    pub fn semantic_type(self) -> Option<TypeId> {
        self.ir.items.semantic_types[self.id.index()].get()
    }
    #[must_use]
    pub fn source(self) -> Option<SourceSpan> {
        self.ir.sources.get(self.id.index())
    }
    #[must_use]
    pub fn version(self) -> EntityVersion {
        self.ir.items.versions[self.id.index()]
    }
    #[must_use]
    pub fn name(self) -> &'ir [u8] {
        self.ir
            .atom(self.ir.items.names[self.id.index()])
            .unwrap_or(&[])
    }
    #[must_use]
    pub fn members(self) -> &'ir [EntityId] {
        self.ir
            .entity_lists
            .get(self.ir.items.members[self.id.index()])
            .unwrap_or(&[])
    }
    #[must_use]
    pub fn docs(self) -> &'ir [DocFragment] {
        self.ir
            .docs
            .get(self.ir.items.docs[self.id.index()])
            .unwrap_or(&[])
    }
    #[must_use]
    pub fn attributes(self) -> &'ir [AtomId] {
        self.ir
            .atom_lists
            .get(self.ir.items.attributes[self.id.index()])
            .unwrap_or(&[])
    }
    #[must_use]
    pub fn links_from(self) -> LinkIter<'ir> {
        self.ir.links_from(self.id)
    }
    /// Iterates every authority-observed outgoing source site, including
    /// repeated uses that share a canonical semantic relation.
    #[must_use]
    pub fn link_occurrences_from(self) -> LinkOccurrenceIter<'ir> {
        self.ir.link_occurrences_from(self.id)
    }
    #[must_use]
    pub fn links_to(self) -> LinkIter<'ir> {
        self.ir.links_to(self.id)
    }
}

/// Exact-size borrowed iterator over graph links.
pub struct LinkIter<'ir> {
    ir: &'ir Ir,
    ids: &'ir [LinkId],
}

impl<'ir> Iterator for LinkIter<'ir> {
    type Item = (LinkId, Link);
    fn next(&mut self) -> Option<Self::Item> {
        let (id, rest) = self.ids.split_first()?;
        self.ids = rest;
        self.ir.link(*id).map(|link| (*id, link))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.ids.len(), Some(self.ids.len()))
    }
}
impl ExactSizeIterator for LinkIter<'_> {}
impl core::iter::FusedIterator for LinkIter<'_> {}

/// Exact-size iterator over typed authority-observed graph source sites.
pub struct LinkOccurrenceIter<'ir> {
    ir: &'ir Ir,
    ids: Option<&'ir [LinkOccurrenceId]>,
    next: usize,
    end: usize,
}

impl<'ir> Iterator for LinkOccurrenceIter<'ir> {
    type Item = (LinkOccurrenceId, LinkOccurrence);

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.next;
        if index == self.end {
            return None;
        }
        self.next += 1;
        let id = self
            .ids
            .and_then(|ids| ids.get(index).copied())
            .unwrap_or_else(|| LinkOccurrenceId::new(index as u32));
        self.ir
            .link_occurrence(id)
            .map(|occurrence| (id, occurrence))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end.saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for LinkOccurrenceIter<'_> {}
impl core::iter::FusedIterator for LinkOccurrenceIter<'_> {}

/// Exact-size borrowed iterator over entity posting lists.
pub struct ItemIdIter<'ir> {
    ir: &'ir Ir,
    ids: &'ir [EntityId],
}

impl<'ir> Iterator for ItemIdIter<'ir> {
    type Item = ItemView<'ir>;
    fn next(&mut self) -> Option<Self::Item> {
        let (id, rest) = self.ids.split_first()?;
        self.ids = rest;
        self.ir.item(*id)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.ids.len(), Some(self.ids.len()))
    }
}
impl ExactSizeIterator for ItemIdIter<'_> {}
impl core::iter::FusedIterator for ItemIdIter<'_> {}
