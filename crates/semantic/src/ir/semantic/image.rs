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

impl Ir {
    /// Number of rows shared by every entity-aligned column family.
    #[must_use]
    pub fn entity_count(&self) -> usize {
        self.items.len()
    }

    /// Returns the image-level source, recipe, and scope authority when this
    /// image was built by one compiled authority transaction.
    #[must_use]
    pub const fn image_provenance(&self) -> ImageProvenance {
        self.provenance
    }

    /// Borrows cold per-entity authority facts aligned with the entity rows.
    #[must_use]
    pub fn entity_authority_columns(&self) -> EntityAuthorityColumns<'_> {
        self.authority_facts.view()
    }

    /// Borrows cold authority source availability aligned with every observed
    /// graph occurrence. The current two-state lane is exact: `Captured`
    /// means `LinkOccurrence::source` is present, while `Unavailable` means
    /// no source site was supplied.
    #[must_use]
    pub fn occurrence_authority_columns(&self) -> OccurrenceAuthorityColumns<'_> {
        self.occurrence_authority.view()
    }

    /// Borrows every canonical column as typed slices, with no semantic
    /// conversion, hashing, directory construction, or allocation.
    #[must_use]
    pub fn storage_columns(&self) -> StorageColumns<'_> {
        StorageColumns {
            authority: self.authority,
            provenance: self.provenance,
            atoms: self.atoms.view(),
            types: self.types.columns(),
            externals: &self.externals,
            type_lists: self.type_lists.view(),
            entity_lists: self.entity_lists.view(),
            atom_lists: self.atom_lists.view(),
            docs: self.docs.view(),
            tuple_elements: self.tuple_elements.view(),
            object_members: self.object_members.view(),
            template_parts: self.template_parts.view(),
            type_parameter_bounds: self.type_parameter_bounds.view(),
            type_parameters: self.type_parameters.view(),
            free_predicates: self.free_predicates.view(),
            entities: self.entity_columns(),
            entity_authority: self.entity_authority_columns(),
            occurrence_authority: self.occurrence_authority_columns(),
            sources: self.source_columns(),
            language_extensions: self.language_extensions(),
            graph: self.graph_columns(),
            vcs: self.vcs_columns(),
            kind_entities: &self.indices.kind,
            kind_offsets: &self.kind_offsets,
            name_entities: &self.indices.name,
        }
    }

    /// Borrows the dense entity SoA with no projection or validation work.
    #[must_use]
    pub fn entity_columns(&self) -> EntityColumns<'_> {
        EntityColumns {
            names: &self.items.names,
            kinds: &self.items.kinds,
            visibility: &self.items.visibility,
            parents: &self.items.parents,
            semantic_types: &self.items.semantic_types,
            members: &self.items.members,
            docs: &self.items.docs,
            attributes: &self.items.attributes,
        }
    }

    /// Borrows cold source lanes without touching them during ordinary scans.
    #[must_use]
    pub fn source_columns(&self) -> SourceColumnsView<'_> {
        SourceColumnsView {
            files: &self.sources.files,
            starts: &self.sources.starts,
            ends: &self.sources.ends,
            rows: self.sources.rows,
        }
    }

    /// Borrows precomputed graph tables and both CSR directions.
    #[must_use]
    pub fn graph_columns(&self) -> GraphColumns<'_> {
        let mut columns = self.links.view(self.link_occurrences.view());
        columns.outgoing = &self.indices.outgoing;
        columns.outgoing_offsets = &self.indices.outgoing_offsets;
        columns.incoming = &self.indices.incoming;
        columns.incoming_offsets = &self.indices.incoming_offsets;
        columns.occurrences.outgoing = &self.indices.occurrence_outgoing;
        columns.occurrences.outgoing_offsets = &self.indices.occurrence_outgoing_offsets;
        columns
    }

    /// Borrows the exact stable-order lanes already used by IR-VCS.
    #[must_use]
    pub fn vcs_columns(&self) -> VcsColumns<'_> {
        VcsColumns {
            versions: &self.items.versions,
            declaration_instances: &self.indices.instances,
            stable_links: &self.indices.canonical_links,
        }
    }

    #[must_use]
    pub fn atom(&self, id: AtomId) -> Option<&[u8]> {
        self.atoms.get(id)
    }
    #[must_use]
    pub fn text(&self, id: TextId) -> Option<&str> {
        self.atoms.text(id)
    }
    #[must_use]
    pub fn ty(&self, id: TypeId) -> Option<TypeExpr> {
        self.types.get(id)
    }
    #[must_use]
    pub fn typed_type<State: TypeState>(&self, id: TypedTypeId<State>) -> Option<State::Node> {
        State::project(self.ty(id.erase())?)
    }
    #[must_use]
    pub fn types(&self, id: TypeListId) -> Option<&[TypeId]> {
        self.type_lists.get(id)
    }
    #[must_use]
    pub fn atom_list(&self, id: AtomListId) -> Option<&[AtomId]> {
        self.atom_lists.get(id)
    }
    /// Borrows one canonical local-member list by its typed pool coordinate.
    #[must_use]
    pub(crate) fn entity_list(&self, id: EntityListId) -> Option<&[EntityId]> {
        self.entity_lists.get(id)
    }
    /// Borrows one canonical documentation list by its typed pool coordinate.
    #[must_use]
    pub(crate) fn documentation(&self, id: DocId) -> Option<&[DocFragment]> {
        self.docs.get(id)
    }
    #[must_use]
    pub fn tuple_elements(&self, id: TupleElementListId) -> Option<&[TupleElement]> {
        self.tuple_elements.get(id)
    }
    #[must_use]
    pub fn object_members(&self, id: ObjectMemberListId) -> Option<&[ObjectMember]> {
        self.object_members.get(id)
    }
    #[must_use]
    pub fn template_parts(&self, id: TemplatePartListId) -> Option<&[TemplatePart]> {
        self.template_parts.get(id)
    }
    #[must_use]
    pub fn type_parameters(&self, id: TypeParameterListId) -> Option<&[TypeParameter]> {
        self.type_parameters.get(id)
    }
    #[must_use]
    pub fn type_parameter_bounds(
        &self,
        id: TypeParameterBoundListId,
    ) -> Option<&[TypeParameterBound]> {
        self.type_parameter_bounds.get(id)
    }
    #[must_use]
    pub fn free_predicates(&self, id: FreePredicateListId) -> Option<&[FreePredicate]> {
        self.free_predicates.get(id)
    }
    #[must_use]
    pub fn external(&self, id: ExternalId) -> Option<&ExternalTarget> {
        self.externals.get(id.index())
    }
    #[must_use]
    pub fn version(&self, id: EntityId) -> Option<EntityVersion> {
        self.items.versions.get(id.index()).copied()
    }
    /// Projects every aligned immutable semantic row for one local entity.
    ///
    /// This is the reader boundary's row view: consumers receive the hot
    /// declaration fields, cold authority facts, source truth, and exact
    /// declaration version together rather than joining parallel lanes by
    /// convention.
    #[must_use]
    pub(crate) fn semantic_entity(&self, id: EntityId) -> Option<crate::ir::SemanticEntity> {
        let index = id.index();
        Some(crate::ir::SemanticEntity {
            id,
            name: *self.items.names.get(index)?,
            kind: *self.items.kinds.get(index)?,
            visibility: *self.items.visibility.get(index)?,
            parent: self.items.parents.get(index)?.get(),
            semantic_type: self.items.semantic_types.get(index)?.get(),
            members: *self.items.members.get(index)?,
            docs: *self.items.docs.get(index)?,
            attributes: *self.items.attributes.get(index)?,
            source: self.sources.get(index),
            authority: self.authority_facts.facts(index)?,
            version: *self.items.versions.get(index)?,
        })
    }

    /// Projects only the self-contained portable declaration facts.
    ///
    /// Unlike [`Self::semantic_entity`], this deliberately carries no pooled
    /// coordinate.  A subordinate core image can therefore expose identity,
    /// authority, and source truth without claiming its omitted type/list,
    /// documentation, graph, or extension planes exist.
    pub(crate) fn core_semantic_entity(
        &self,
        id: EntityId,
    ) -> Option<crate::ir::CoreSemanticEntity> {
        let item = self.item(id)?;
        let authority = self.authority_facts.facts(id.index())?;
        Some(crate::ir::CoreSemanticEntity {
            id,
            name: *self.items.names.get(id.index())?,
            kind: item.kind(),
            visibility: item.visibility(),
            parent: item.parent(),
            authority,
            source: item.source(),
            version: item.version(),
        })
    }
    /// Binary-searches the exact canonical declaration-instance index.
    #[must_use]
    pub fn find_declaration(&self, identity: DeclarationIdentity) -> Option<ItemView<'_>> {
        let index = self
            .indices
            .instances
            .binary_search_by_key(&identity, |id| self.items.versions[id.index()].identity())
            .ok()?;
        self.item(self.indices.instances[index])
    }
    /// Iterates every current instance in one declaration family.
    #[must_use]
    pub fn family_items(&self, family: DeclarationFamilyId) -> ItemIdIter<'_> {
        let start = self
            .indices
            .instances
            .partition_point(|id| self.items.versions[id.index()].family < family);
        let end = start
            + self.indices.instances[start..]
                .partition_point(|id| self.items.versions[id.index()].family == family);
        ItemIdIter {
            ir: self,
            ids: &self.indices.instances[start..end],
        }
    }
    /// Iterates declarations in canonical exact-instance order.
    #[must_use]
    pub fn canonical_items(&self) -> ItemIdIter<'_> {
        ItemIdIter {
            ir: self,
            ids: &self.indices.instances,
        }
    }
    /// Iterates one declaration kind through its Trustfall-friendly posting index.
    #[must_use]
    pub fn items_of_kind(&self, kind: ItemKind) -> ItemIdIter<'_> {
        let raw = kind as usize;
        let ids = self
            .indices
            .kind
            .get(self.kind_offsets[raw] as usize..self.kind_offsets[raw + 1] as usize)
            .unwrap_or(&[]);
        ItemIdIter { ir: self, ids }
    }
    /// Binary-searches the precomputed raw-byte name index.
    #[must_use]
    pub fn items_named(&self, name: &[u8]) -> ItemIdIter<'_> {
        let start = self
            .indices
            .name
            .partition_point(|id| self.atom(self.items.names[id.index()]).unwrap_or(&[]) < name);
        let end = self.indices.name[start..]
            .partition_point(|id| self.atom(self.items.names[id.index()]).unwrap_or(&[]) == name)
            + start;
        ItemIdIter {
            ir: self,
            ids: &self.indices.name[start..end],
        }
    }
    #[must_use]
    pub fn item(&self, id: EntityId) -> Option<ItemView<'_>> {
        (id.index() < self.items.len()).then_some(ItemView { ir: self, id })
    }
    /// Borrows all typed language-extension planes without widening hot entity rows.
    #[must_use]
    pub fn language_extensions(&self) -> LanguageExtensionsView<'_> {
        self.extensions.view(self.authority)
    }

    /// Produces bounds proved by this validated IR's shared semantic columns.
    #[must_use]
    pub fn language_extension_common_bounds(
        &self,
    ) -> Option<crate::ir::ValidatedLanguageExtensionCommonBounds> {
        let columns = self.storage_columns();
        Some(
            crate::ir::ValidatedLanguageExtensionCommonBounds::from_validated(
                crate::ir::LanguageExtensionCommonBounds {
                    atoms: u32::try_from(columns.atoms.ranges.len()).ok()?,
                    types: u32::try_from(columns.types.headers.len()).ok()?,
                    entities: u32::try_from(columns.entities.names.len()).ok()?,
                    type_lists: u32::try_from(columns.type_lists.ranges.len()).ok()?,
                    entity_lists: u32::try_from(columns.entity_lists.ranges.len()).ok()?,
                    atom_lists: u32::try_from(columns.atom_lists.ranges.len()).ok()?,
                    free_predicates: u32::try_from(columns.free_predicates.ranges.len()).ok()?,
                    type_parameters: crate::ir::TypeParameterListBounds::ExactRanges {
                        count: u32::try_from(columns.type_parameters.ranges.len()).ok()?,
                    },
                },
            ),
        )
    }
    #[must_use]
    pub fn items(&self) -> impl ExactSizeIterator<Item = ItemView<'_>> {
        (0..self.items.len()).map(|raw| ItemView {
            ir: self,
            id: EntityId::new(raw as u32),
        })
    }
    #[must_use]
    pub fn link(&self, id: LinkId) -> Option<Link> {
        self.links.get(id)
    }
    /// Returns one authority-observed graph site without widening it into its
    /// deduplicated relation.
    #[must_use]
    pub fn link_occurrence(&self, id: LinkOccurrenceId) -> Option<LinkOccurrence> {
        self.link_occurrences.get(id)
    }
    /// Iterates every source occurrence in authority emission order.
    #[must_use]
    pub fn link_occurrences(&self) -> LinkOccurrenceIter<'_> {
        LinkOccurrenceIter {
            ir: self,
            ids: None,
            next: 0,
            end: self.link_occurrences.len(),
        }
    }
    /// Iterates source occurrences from one entity through the precomputed
    /// occurrence CSR index. This never scans unrelated owners or reparses
    /// the compact occurrence lane.
    #[must_use]
    pub fn link_occurrences_from(&self, entity: EntityId) -> LinkOccurrenceIter<'_> {
        let range = self
            .indices
            .occurrence_outgoing_offsets
            .get(entity.index())
            .zip(
                self.indices
                    .occurrence_outgoing_offsets
                    .get(entity.index() + 1),
            );
        let ids = range
            .and_then(|(start, end)| {
                self.indices
                    .occurrence_outgoing
                    .get(*start as usize..*end as usize)
            })
            .unwrap_or(&[]);
        LinkOccurrenceIter {
            ir: self,
            ids: Some(ids),
            next: 0,
            end: ids.len(),
        }
    }
    #[must_use]
    pub(crate) fn canonical_link_ids(&self) -> &[LinkId] {
        &self.indices.canonical_links
    }
    #[must_use]
    pub fn links_from(&self, entity: EntityId) -> LinkIter<'_> {
        self.adjacent(entity, false)
    }
    #[must_use]
    pub fn links_to(&self, entity: EntityId) -> LinkIter<'_> {
        self.adjacent(entity, true)
    }
    fn adjacent(&self, entity: EntityId, reverse: bool) -> LinkIter<'_> {
        let (ids, offsets) = if reverse {
            (&self.indices.incoming, &self.indices.incoming_offsets)
        } else {
            (&self.indices.outgoing, &self.indices.outgoing_offsets)
        };
        let range = offsets
            .get(entity.index())
            .zip(offsets.get(entity.index() + 1));
        let ids = range
            .and_then(|(start, end)| ids.get(*start as usize..*end as usize))
            .unwrap_or(&[]);
        LinkIter { ir: self, ids }
    }
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
