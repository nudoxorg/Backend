use super::columns::{
    IrIndices, ItemColumns, LanguageExtensionCounts, LanguageExtensions, PackedLinkOccurrences,
    PackedLinks, SourceColumns,
};
use super::error::{BuildError, EntityRange, LanguageExtensionViolation, SemanticSpace};
use super::ids::{
    AtomListId, DocId, EntityListId, External, ExternalId, FreePredicateListId, ItemKind, LinkId,
    LinkOccurrenceId, LinkOccurrenceSpace, LinkSpace, ObjectMemberListId, TemplatePartListId,
    TreeEntityId, TupleElementListId, TypeListId, TypeParameterBoundListId, TypeParameterListId,
};
use super::image::Ir;
use super::language_facts::{LanguageExtensionInput, SemanticImageAuthority};
use super::packed_types::{
    ArrayShape, CallableElementRole, ComputedType, ConcreteType, FreePredicate, LiteralType,
    ObjectMember, PackedTypes, PropertyKey, QualifiedSegments, TemplatePart, TupleElement,
    TupleElementKind, TypeInterner, TypeParameter, TypeParameterBound, TypeParameterKind,
    TypeQuery, VariadicForm, WildcardBound,
};
use super::relations::{
    Confidence, DeclarationLinkTarget, DocFragment, DocInput, EntityVersion, ExternalTarget,
    ForeignExternalTarget, ForeignTargetOrigin, Item, Link, LinkKey, LinkKind, LinkOccurrence,
    LinkTarget, SourceSpan, canonical_relation_evidence_precedes,
};
use super::tree::{BorrowedTree, FrontendTree, TreeItemInput, TreeLinkInput, TreeLinkTarget};
use super::type_model::{
    ComputedTypeId, ConcreteTypeId, GuardedType, TypeExpr, TypeState, TypedTypeId, UnknownType,
    UnknownTypeId, Visibility,
};
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
use crate::vocabulary::{CompileRecipeFact, Language, LanguageProfile, PackageUrl};
use alloc::{vec, vec::Vec};
use backend_version::{ContentId, SemanticScopeDomain};
use core::{fmt, hash::Hash, num::NonZeroU16};

pub(super) fn validate_type_parameters(
    builder: &IrBuilder,
    parameters: TypeParameterListId,
) -> Result<(), BuildError> {
    for (position, parameter) in list_or_dangling(
        &builder.type_parameters,
        parameters,
        SemanticSpace::TypeParameters,
    )?
    .iter()
    .enumerate()
    {
        atom(builder, parameter.name)?;
        for bound in list_or_dangling(
            &builder.type_parameter_bounds,
            parameter.bounds,
            SemanticSpace::TypeParameterBounds,
        )? {
            match bound {
                TypeParameterBound::Type(ty) => {
                    id(*ty, builder.types.len(), SemanticSpace::Type)?;
                }
                TypeParameterBound::Lifetime(name) => atom(builder, *name)?,
            }
        }
        if let TypeParameterKind::ConstValue { value_type } = parameter.kind {
            id(value_type, builder.types.len(), SemanticSpace::Type)?;
        }
        if !parameter.requirements.is_valid() {
            return Err(BuildError::TypeParameterRequirements {
                list: parameters.raw,
                position: u32::try_from(position).unwrap_or(u32::MAX),
                requirements: parameter.requirements,
            });
        }
        optional_id(parameter.default, builder.types.len(), SemanticSpace::Type)?;
    }
    Ok(())
}

pub(super) fn validate_free_predicates(
    builder: &IrBuilder,
    predicates: FreePredicateListId,
) -> Result<(), BuildError> {
    for predicate in list_or_dangling(
        &builder.free_predicates,
        predicates,
        SemanticSpace::FreePredicates,
    )? {
        id(predicate.subject, builder.types.len(), SemanticSpace::Type)?;
        for bound in list_or_dangling(
            &builder.type_parameter_bounds,
            predicate.bounds,
            SemanticSpace::TypeParameterBounds,
        )? {
            match bound {
                TypeParameterBound::Type(ty) => {
                    id(*ty, builder.types.len(), SemanticSpace::Type)?;
                }
                TypeParameterBound::Lifetime(name) => atom(builder, *name)?,
            }
        }
    }
    Ok(())
}

pub(super) fn validate_atom_list(builder: &IrBuilder, atoms: AtomListId) -> Result<(), BuildError> {
    for atom_id in list_or_dangling(&builder.atom_lists, atoms, SemanticSpace::AtomList)? {
        atom(builder, *atom_id)?;
    }
    Ok(())
}

pub(super) fn validate_entity_list(
    builder: &IrBuilder,
    entities: EntityListId,
) -> Result<(), BuildError> {
    for entity_id in list_or_dangling(&builder.entity_lists, entities, SemanticSpace::EntityList)? {
        id(*entity_id, builder.items.len(), SemanticSpace::Entity)?;
    }
    Ok(())
}
/// Allocation-amortized builder for the condensed semantic IR.
///
/// Frontends should prefer [`Self::add_borrowed_tree`]: it consumes their
/// existing slice-backed tree without manufacturing owned nodes or strings.
#[derive(Default)]
pub struct IrBuilder {
    authority: SemanticImageAuthority,
    provenance: ImageProvenance,
    atoms: AtomInterner,
    pub(in crate::ir::semantic) types: TypeInterner,
    externals: Interner<ExternalTarget, External>,
    type_lists: ListInterner<TypeId>,
    entity_lists: ListInterner<EntityId>,
    atom_lists: ListInterner<AtomId>,
    docs: ListInterner<DocFragment>,
    tuple_elements: ListInterner<TupleElement>,
    object_members: ListInterner<ObjectMember>,
    template_parts: ListInterner<TemplatePart>,
    type_parameter_bounds: ListInterner<TypeParameterBound>,
    type_parameters: ListInterner<TypeParameter>,
    free_predicates: ListInterner<FreePredicate>,
    items: ItemColumns,
    authority_facts: AuthorityColumns,
    sources: SourceColumns,
    extensions: LanguageExtensions,
    link_index: HashIndex,
    links: PackedLinks,
    link_occurrences: PackedLinkOccurrences,
    occurrence_authority: OccurrenceAuthorityColumn,
    entity_scratch: Vec<EntityId>,
    atom_scratch: Vec<AtomId>,
    doc_scratch: Vec<DocFragment>,
}

impl IrBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Binds this image to one language before language-specific facts arrive.
    pub fn set_language_profile(&mut self, profile: LanguageProfile) -> Result<(), BuildError> {
        let requested = SemanticImageAuthority::Language(profile);
        if self.authority != SemanticImageAuthority::Shared && self.authority != requested {
            return Err(BuildError::LanguageProfileRebind {
                existing: self.authority,
                requested: profile,
            });
        }
        self.authority = requested;
        Ok(())
    }
    /// Binds one owned compile provenance header before entity materialization.
    ///
    /// The header is unavailable for manually assembled images. A compiled
    /// image retains the exact source, recipe, and validated package/file
    /// scope in the same arena as its semantic facts, so driver scratch never
    /// remains the only source of reader-visible authority.
    pub fn set_image_provenance(
        &mut self,
        source: SourceIdentity,
        recipe: CompileRecipeFact,
        lineage: PackageLineage<'_>,
        path: &str,
    ) -> Result<(), BuildError> {
        self.set_image_provenance_inner(source, recipe, lineage, path, None)
    }

    /// Binds provenance to an exact already-parsed package coordinate.
    ///
    /// The resulting immutable image retains and authenticates the complete
    /// canonical package URL rather than only its declaration lineage.
    pub fn set_image_provenance_for_package(
        &mut self,
        source: SourceIdentity,
        recipe: CompileRecipeFact,
        coordinate: &PackageUrl,
        path: &str,
    ) -> Result<(), BuildError> {
        let lineage = PackageLineage::new(
            coordinate.package_type().as_str(),
            coordinate.lineage_name(),
        )
        .map_err(|cause| BuildError::ImageProvenanceLineage { cause })?;
        self.set_image_provenance_inner(source, recipe, lineage, path, Some(coordinate))
    }

    fn set_image_provenance_inner(
        &mut self,
        source: SourceIdentity,
        recipe: CompileRecipeFact,
        lineage: PackageLineage<'_>,
        path: &str,
        coordinate: Option<&PackageUrl>,
    ) -> Result<(), BuildError> {
        let expected = CompileRecipeFact::derive(
            recipe.profile,
            recipe.stage,
            recipe.tool,
            source.identity,
            recipe.toolchain,
        );
        if expected != recipe {
            return Err(BuildError::ImageProvenanceRecipe { source, recipe });
        }
        let scope_key = DeclarationKey::new(lineage, path, ItemKind::Module, b"_")
            .map_err(|cause| BuildError::ImageProvenanceScope { cause })?;
        let scope_preimage_len = scope_key
            .preimage_len()
            .map_err(|cause| BuildError::ImageProvenanceScopePreimage { cause })?;
        let mut scope_preimage = vec![0_u8; scope_preimage_len];
        let requested = ImageProvenanceClaim {
            source,
            recipe,
            scope: SemanticScopeClaim {
                identity: {
                    if let Some(coordinate) = coordinate {
                        SemanticScopeClaim::for_package(coordinate, path).identity
                    } else {
                        let written = scope_key
                            .write_preimage(&mut scope_preimage)
                            .map_err(|cause| BuildError::ImageProvenanceScopePreimage { cause })?;
                        ContentId::<SemanticScopeDomain>::from_canonical_bytes(
                            &scope_preimage[..written],
                        )
                    }
                },
            },
        };
        if let ImageProvenance::Captured {
            source: existing_source,
            recipe: existing_recipe,
            claim: existing_scope,
            ..
        } = self.provenance
        {
            let existing = ImageProvenanceClaim {
                source: existing_source,
                recipe: existing_recipe,
                scope: existing_scope,
            };
            if existing == requested {
                return Ok(());
            }
            return Err(BuildError::ImageProvenanceRebind {
                existing,
                requested,
            });
        }
        self.set_language_profile(recipe.profile)?;
        let scope = SemanticScopeFacts {
            ecosystem: self.intern_atom(lineage.ecosystem.as_bytes())?,
            package: self.intern_atom(lineage.name.as_bytes())?,
            path: self.intern_atom(path.as_bytes())?,
            coordinate: coordinate
                .map(|coordinate| self.intern_atom(coordinate.as_str().as_bytes()))
                .transpose()?,
        };
        self.provenance = ImageProvenance::Captured {
            source,
            recipe,
            claim: requested.scope,
            scope,
        };
        Ok(())
    }
    pub fn intern_atom(&mut self, bytes: &[u8]) -> Result<AtomId, BuildError> {
        self.atoms.intern(bytes).map_err(Into::into)
    }
    pub fn intern_text(&mut self, text: &str) -> Result<TextId, BuildError> {
        self.atoms.intern_text(text).map_err(Into::into)
    }
    /// Reserves final packed headers and hash slots for a known lowering batch.
    pub fn reserve_types(&mut self, additional: usize) {
        self.types.reserve(additional);
    }
    pub fn intern_type(&mut self, ty: TypeExpr) -> Result<TypeId, BuildError> {
        if let TypeExpr::Unknown(UnknownType {
            spelling: Some(spelling),
            ..
        }) = ty
            && self.atoms.get(spelling).is_none()
        {
            return Err(BuildError::Dangling {
                space: SemanticSpace::Atom,
                raw: spelling.raw,
            });
        }
        self.types.intern(ty).map_err(Into::into)
    }
    /// Interns a never-guarded state-indexed term and retains its proof in the ID.
    pub fn intern_guarded<State: TypeState>(
        &mut self,
        ty: GuardedType<State>,
    ) -> Result<TypedTypeId<State>, BuildError> {
        self.intern_type(State::inject(ty.node))
            .map(TypedTypeId::proven)
    }
    pub fn intern_concrete(&mut self, ty: ConcreteType) -> Result<ConcreteTypeId, BuildError> {
        self.intern_guarded(GuardedType::concrete(ty))
    }
    pub fn intern_computed(&mut self, ty: ComputedType) -> Result<ComputedTypeId, BuildError> {
        self.intern_guarded(GuardedType::computed(ty))
    }
    pub fn intern_unknown(&mut self, ty: UnknownType) -> Result<UnknownTypeId, BuildError> {
        self.intern_guarded(GuardedType::unknown(ty))
    }
    pub fn intern_types(&mut self, types: &[TypeId]) -> Result<TypeListId, BuildError> {
        self.type_lists.intern(types).map_err(Into::into)
    }
    pub fn intern_external(&mut self, target: ExternalTarget) -> Result<ExternalId, BuildError> {
        self.externals.intern(target).map_err(Into::into)
    }
    pub fn intern_docs(&mut self, docs: &[DocFragment]) -> Result<DocId, BuildError> {
        self.docs.intern(docs).map_err(Into::into)
    }
    pub fn intern_members(&mut self, members: &[EntityId]) -> Result<EntityListId, BuildError> {
        self.entity_lists.intern(members).map_err(Into::into)
    }
    pub fn intern_attributes(&mut self, attributes: &[AtomId]) -> Result<AtomListId, BuildError> {
        self.atom_lists.intern(attributes).map_err(Into::into)
    }
    pub fn intern_tuple_elements(
        &mut self,
        elements: &[TupleElement],
    ) -> Result<TupleElementListId, BuildError> {
        self.tuple_elements.intern(elements).map_err(Into::into)
    }
    pub fn intern_object_members(
        &mut self,
        members: &[ObjectMember],
    ) -> Result<ObjectMemberListId, BuildError> {
        self.object_members.intern(members).map_err(Into::into)
    }
    pub fn intern_template_parts(
        &mut self,
        parts: &[TemplatePart],
    ) -> Result<TemplatePartListId, BuildError> {
        self.template_parts.intern(parts).map_err(Into::into)
    }
    pub fn intern_type_parameter_bounds(
        &mut self,
        bounds: &[TypeParameterBound],
    ) -> Result<TypeParameterBoundListId, BuildError> {
        self.type_parameter_bounds
            .intern(bounds)
            .map_err(Into::into)
    }
    pub fn intern_type_parameters(
        &mut self,
        parameters: &[TypeParameter],
    ) -> Result<TypeParameterListId, BuildError> {
        self.type_parameters.intern(parameters).map_err(Into::into)
    }
    pub fn intern_free_predicates(
        &mut self,
        predicates: &[FreePredicate],
    ) -> Result<FreePredicateListId, BuildError> {
        self.free_predicates.intern(predicates).map_err(Into::into)
    }
    pub fn add_item(
        &mut self,
        version: EntityVersion,
        item: Item,
        extension: Option<LanguageExtensionInput<'_>>,
        authority_facts: EntityAuthorityFacts,
    ) -> Result<EntityId, BuildError> {
        if let Some(extension) = extension
            && self.authority.language() != Some(extension.language())
        {
            return Err(BuildError::LanguageProfileMismatch {
                authority: self.authority,
                extension: extension.language(),
            });
        }
        let id = EntityId::try_from_index(self.items.len()).map_err(|_| CapacityError {
            space: crate::ir::CapacitySpace::Value,
            actual: self.items.len(),
        })?;
        self.items.reserve_one();
        self.authority_facts.reserve(1);
        self.sources.push(item.source);
        self.items.push(version, item);
        self.authority_facts.push(authority_facts);
        self.extensions.push(extension)?;
        Ok(id)
    }
    pub fn add_link(&mut self, link: Link) -> Result<LinkId, BuildError> {
        let key = LinkKey {
            from: link.from,
            target: link.target,
            kind: link.kind,
        };
        let value_hash = hash(&key);
        if let Some(ordinal) = self.link_index.find(value_hash, |ordinal| {
            self.links.get(LinkId::new(ordinal)).is_some_and(|known| {
                known.from == key.from && known.target == key.target && known.kind == key.kind
            })
        }) {
            let id = LinkId::new(ordinal);
            let Some(known) = self.links.get(id) else {
                return Err(BuildError::Dangling {
                    space: SemanticSpace::Entity,
                    raw: ordinal,
                });
            };
            // Confidence is a monotonic quality lattice. Equal-confidence
            // observations join by one total source order, so the legacy
            // representative column never depends on authority emission
            // order. Exhaustive evidence remains in LinkOccurrence rows.
            if canonical_relation_evidence_precedes(link, known) {
                self.links.replace(id, link)?;
            }
            return Ok(id);
        }
        let id = LinkId::try_from_index(self.links.len()).map_err(|_| CapacityError {
            space: crate::ir::CapacitySpace::Value,
            actual: self.links.len(),
        })?;
        self.links.reserve_one(usize::from(link.source.is_some()));
        self.links.push(link)?;
        self.link_index.insert(value_hash, id.raw);
        Ok(id)
    }

    /// Records one authority-observed source site and returns its typed
    /// occurrence coordinate. The relation remains canonical and deduped,
    /// while this method preserves every observation's span and confidence.
    pub fn add_link_occurrence(
        &mut self,
        link: Link,
        authority: OccurrenceAuthorityFacts,
    ) -> Result<LinkOccurrenceId, BuildError> {
        let id = LinkOccurrenceId::try_from_index(self.link_occurrences.len()).map_err(|_| {
            CapacityError {
                space: crate::ir::CapacitySpace::Value,
                actual: self.link_occurrences.len(),
            }
        })?;
        self.link_occurrences
            .reserve_one(usize::from(link.source.is_some()));
        self.occurrence_authority.reserve(1);
        let occurrence = LinkOccurrence {
            link: self.add_link(link)?,
            confidence: link.confidence,
            source: link.source,
        };
        self.link_occurrences.push(occurrence)?;
        self.occurrence_authority.push(authority);
        Ok(id)
    }

    /// Reserves a stable entity range before type construction.
    ///
    /// The returned exclusive builder exposes the final entity coordinates, so
    /// recursive nominal types and forward graph links can name nodes before
    /// any row is copied into the condensed arenas.
    pub fn reserve_tree<'builder, 'source>(
        &'builder mut self,
        versions: &'source [EntityVersion],
    ) -> Result<TreeBuilder<'builder, 'source>, BuildError> {
        let start = EntityId::try_from_index(self.items.len()).map_err(|_| CapacityError {
            space: crate::ir::CapacitySpace::Value,
            actual: self.items.len(),
        })?;
        let len = u32::try_from(versions.len()).map_err(|_| CapacityError {
            space: crate::ir::CapacitySpace::Value,
            actual: versions.len(),
        })?;
        Ok(TreeBuilder {
            builder: self,
            versions,
            range: EntityRange { start, len },
        })
    }

    /// Condenses a whole borrowed compiler tree with one copy of each distinct atom/list.
    pub fn add_borrowed_tree(&mut self, tree: BorrowedTree<'_>) -> Result<EntityRange, BuildError> {
        self.add_frontend_tree(&tree)
    }

    /// Condenses any compiler-owned tree through a zero-allocation static
    /// adapter. Frontends yield stack rows directly from their native AST.
    pub fn add_frontend_tree<Tree: FrontendTree + ?Sized>(
        &mut self,
        tree: &Tree,
    ) -> Result<EntityRange, BuildError> {
        let item_count = tree.items().len();
        if tree.versions().len() != item_count {
            return Err(BuildError::TreeVersionCount {
                versions: tree.versions().len(),
                items: item_count,
            });
        }
        self.reserve_frontend_tree(tree);
        let start = EntityId::try_from_index(self.items.len()).map_err(|_| CapacityError {
            space: crate::ir::CapacitySpace::Value,
            actual: self.items.len(),
        })?;
        let count = u32::try_from(item_count).map_err(|_| CapacityError {
            space: crate::ir::CapacitySpace::Value,
            actual: item_count,
        })?;
        let range = EntityRange { start, len: count };

        for (input, version) in tree.items().zip(tree.versions()) {
            let name = self.intern_atom(input.name)?;
            let parent = transpose_tree_id(range, input.parent)?;

            self.entity_scratch.clear();
            for member in input.members {
                self.entity_scratch.push(tree_id(range, *member)?);
            }
            let members = self.entity_lists.intern(&self.entity_scratch)?;

            self.atom_scratch.clear();
            for attribute in input.attributes {
                self.atom_scratch.push(self.atoms.intern(attribute)?);
            }
            let attributes = self.atom_lists.intern(&self.atom_scratch)?;

            self.doc_scratch.clear();
            for fragment in input.docs {
                let fragment = match *fragment {
                    DocInput::Text(text) => DocFragment::Text(self.atoms.intern_text(text)?),
                    DocInput::Code(text) => DocFragment::Code(self.atoms.intern_text(text)?),
                    DocInput::Link { label, target } => DocFragment::Link {
                        label: self.atoms.intern_text(label)?,
                        target: match target {
                            TreeLinkTarget::Local(local) => {
                                LinkTarget::Local(tree_id(range, local)?)
                            }
                            TreeLinkTarget::External(external) => LinkTarget::External(external),
                        },
                    },
                    DocInput::SoftBreak => DocFragment::SoftBreak,
                    DocInput::HardBreak => DocFragment::HardBreak,
                };
                self.doc_scratch.push(fragment);
            }
            let docs = self.docs.intern(&self.doc_scratch)?;
            self.add_item(
                *version,
                Item {
                    name,
                    kind: input.kind,
                    visibility: input.visibility,
                    parent,
                    semantic_type: input.semantic_type,
                    members,
                    docs,
                    attributes,
                    source: input.source,
                },
                input.extension,
                input.authority,
            )?;
        }

        for input in tree.links() {
            let target = match input.target {
                TreeLinkTarget::Local(local) => LinkTarget::Local(tree_id(range, local)?),
                TreeLinkTarget::External(external) => LinkTarget::External(external),
            };
            self.add_link_occurrence(
                Link {
                    from: tree_id(range, input.from)?,
                    target,
                    kind: input.kind,
                    confidence: input.confidence,
                    source: input.source,
                },
                input.authority,
            )?;
        }
        Ok(range)
    }

    fn reserve_frontend_tree<Tree: FrontendTree + ?Sized>(&mut self, tree: &Tree) {
        let capacity = BorrowedTreeCapacity::measure(tree);
        let item_count = tree.items().len();
        let link_count = tree.links().len();
        self.items.reserve_exact(item_count);
        self.authority_facts.reserve(item_count);
        self.sources.reserve(item_count, capacity.source_values);
        self.extensions.reserve(item_count, capacity.extensions);
        let link_source_values = tree.links().filter(|link| link.source.is_some()).count();
        self.links.reserve_exact(link_count, link_source_values);
        self.link_occurrences
            .reserve_exact(link_count, link_source_values);
        self.occurrence_authority.reserve(link_count);
        self.link_index.reserve(link_count);
        self.atoms
            .reserve(capacity.atom_values, capacity.atom_bytes);
        self.entity_lists
            .reserve(capacity.member_lists, capacity.member_values);
        self.atom_lists
            .reserve(capacity.attribute_lists, capacity.attribute_values);
        self.docs.reserve(capacity.doc_lists, capacity.doc_values);
        self.entity_scratch.reserve(capacity.max_members);
        self.atom_scratch.reserve(capacity.max_attributes);
        self.doc_scratch.reserve(capacity.max_docs);
    }

    /// Validates all caller-constructible IDs and freezes compact query indices.
    pub fn finish(self) -> Result<Ir, BuildError> {
        self.validate()?;
        let links = self.links;
        let link_occurrences = self.link_occurrences;
        let (indices, kind_offsets) = IrIndices::build(
            &self.items,
            &self.items.versions,
            &self.atoms,
            &links,
            &link_occurrences,
            self.externals.as_slice(),
        )?;
        Ok(Ir {
            authority: self.authority,
            provenance: self.provenance,
            atoms: self.atoms.freeze(),
            types: self.types.freeze(),
            externals: self.externals.into_values(),
            type_lists: self.type_lists.freeze(),
            entity_lists: self.entity_lists.freeze(),
            atom_lists: self.atom_lists.freeze(),
            docs: self.docs.freeze(),
            tuple_elements: self.tuple_elements.freeze(),
            object_members: self.object_members.freeze(),
            template_parts: self.template_parts.freeze(),
            type_parameter_bounds: self.type_parameter_bounds.freeze(),
            type_parameters: self.type_parameters.freeze(),
            free_predicates: self.free_predicates.freeze(),
            items: self.items,
            authority_facts: self.authority_facts,
            sources: self.sources,
            extensions: self.extensions,
            indices,
            kind_offsets,
            links,
            link_occurrences,
            occurrence_authority: self.occurrence_authority,
        })
    }

    fn validate(&self) -> Result<(), BuildError> {
        if !self.authority_facts.aligned(self.items.len()) {
            return Err(BuildError::AuthorityRowCount {
                entities: self.items.len(),
                authority_rows: self.authority_facts.len(),
            });
        }
        if self.occurrence_authority.len() != self.link_occurrences.len() {
            return Err(BuildError::OccurrenceAuthorityRowCount {
                occurrences: self.link_occurrences.len(),
                authority_rows: self.occurrence_authority.len(),
            });
        }
        if let ImageProvenance::Captured {
            source,
            recipe,
            scope,
            ..
        } = self.provenance
        {
            let expected = CompileRecipeFact::derive(
                recipe.profile,
                recipe.stage,
                recipe.tool,
                source.identity,
                recipe.toolchain,
            );
            if expected != recipe {
                return Err(BuildError::ImageProvenanceRecipe { source, recipe });
            }
            atom(self, scope.ecosystem)?;
            atom(self, scope.package)?;
            atom(self, scope.path)?;
            if self.authority != SemanticImageAuthority::Language(recipe.profile) {
                return Err(BuildError::LanguageProfileRebind {
                    existing: self.authority,
                    requested: recipe.profile,
                });
            }
        }
        for raw in 0..self.types.len() {
            let Some(ty) = self.types.get(TypeId::new(raw as u32)) else {
                return Err(BuildError::Dangling {
                    space: SemanticSpace::Type,
                    raw: raw as u32,
                });
            };
            validate_type(self, ty)?;
        }
        for target in self.externals.as_slice() {
            match target {
                ExternalTarget::Stable { target } => {
                    // A resolved endpoint is entirely typed identity; it has
                    // no invented foreign spelling to validate.
                    let _ = target;
                }
                ExternalTarget::Foreign(target) => {
                    atom(self, target.path)?;
                    atom(self, target.display)?;
                    match target.origin {
                        ForeignTargetOrigin::Package { ecosystem, package } => {
                            atom(self, ecosystem)?;
                            atom(self, package)?;
                        }
                        ForeignTargetOrigin::Namespace {
                            ecosystem,
                            namespace,
                        } => {
                            atom(self, ecosystem)?;
                            atom(self, namespace)?;
                        }
                        ForeignTargetOrigin::Universe { ecosystem }
                        | ForeignTargetOrigin::Unspecified { ecosystem } => atom(self, ecosystem)?,
                    }
                }
                ExternalTarget::FragmentEntity { display, .. } => atom(self, *display)?,
            }
        }
        for index in 0..self.items.len() {
            let entity = EntityId::new(index as u32);
            atom(self, self.items.names[index])?;
            optional_id(
                self.items.parents[index].get(),
                self.items.len(),
                SemanticSpace::Entity,
            )?;
            optional_id(
                self.items.semantic_types[index].get(),
                self.types.len(),
                SemanticSpace::Type,
            )?;
            if let Some(source) = self.sources.get(index) {
                atom(self, source.file())?;
            }
            self.extensions.validate_entity(self, entity)?;
            let members = list_or_dangling(
                &self.entity_lists,
                self.items.members[index],
                SemanticSpace::EntityList,
            )?;
            for member in members {
                id(*member, self.items.len(), SemanticSpace::Entity)?;
            }
            let attributes = list_or_dangling(
                &self.atom_lists,
                self.items.attributes[index],
                SemanticSpace::AtomList,
            )?;
            for attribute in attributes {
                atom(self, *attribute)?;
            }
            let docs = list_or_dangling(&self.docs, self.items.docs[index], SemanticSpace::Docs)?;
            for fragment in docs {
                validate_doc(self, *fragment)?;
            }
            let facts = self
                .authority_facts
                .facts(index)
                .ok_or(BuildError::AuthorityRowCount {
                    entities: self.items.len(),
                    authority_rows: self.authority_facts.len(),
                })?;
            let local_parent = self.items.parents[index]
                .get()
                .and_then(|parent| self.items.versions.get(parent.index()).copied())
                .map(EntityVersion::identity);
            validate_entity_authority(
                entity,
                facts,
                local_parent,
                self.sources.get(index).is_some(),
                !members.is_empty(),
                self.items.semantic_types[index].get().is_some(),
                !docs.is_empty(),
                self.items.visibility[index] != Visibility::Unknown,
                !attributes.is_empty(),
                self.extensions.has_entity(entity),
            )?;
        }
        for raw in 0..self.links.len() {
            let Some(link) = self.links.get(LinkId::new(raw as u32)) else {
                return Err(BuildError::Dangling {
                    space: SemanticSpace::Entity,
                    raw: raw as u32,
                });
            };
            id(link.from, self.items.len(), SemanticSpace::Entity)?;
            validate_target(self, link.target)?;
            if let Some(source) = link.source {
                atom(self, source.file())?;
            }
        }
        for raw in 0..self.link_occurrences.len() {
            let Some(occurrence) = self.link_occurrences.get(LinkOccurrenceId::new(raw as u32))
            else {
                return Err(BuildError::Dangling {
                    space: SemanticSpace::LinkOccurrence,
                    raw: raw as u32,
                });
            };
            id(occurrence.link, self.links.len(), SemanticSpace::Link)?;
            if let Some(source) = occurrence.source {
                atom(self, source.file())?;
            }
            let authority = self.occurrence_authority.get(raw).ok_or(
                BuildError::OccurrenceAuthorityRowCount {
                    occurrences: self.link_occurrences.len(),
                    authority_rows: self.occurrence_authority.len(),
                },
            )?;
            let present = occurrence.source.is_some();
            if matches!(authority.source, FactAvailability::Captured) != present {
                return Err(BuildError::OccurrenceAuthorityFacts {
                    occurrence: LinkOccurrenceId::new(raw as u32),
                    cause: AuthorityFactFault::Availability {
                        plane: AuthorityFactPlane::OccurrenceSource,
                        claimed: authority.source,
                        present,
                    },
                });
            }
        }
        Ok(())
    }
}
/// Validates one cold authority row against the corresponding owned entity
/// facts. List capture is intentionally not inferred from list cardinality:
/// captured-empty and unavailable-empty are distinct authority observations.
pub(super) fn validate_entity_authority(
    entity: EntityId,
    facts: EntityAuthorityFacts,
    local_parent: Option<DeclarationIdentity>,
    has_source: bool,
    has_members: bool,
    has_semantic_type: bool,
    has_documentation: bool,
    has_visibility: bool,
    has_attributes: bool,
    has_extension: bool,
) -> Result<(), BuildError> {
    let parentage_matches = match facts.parentage {
        ParentageAuthority::Root => local_parent.is_none(),
        ParentageAuthority::Bound(parent) => local_parent == Some(parent),
        ParentageAuthority::UnrepresentedAuthorityOwner(_) | ParentageAuthority::Unavailable => {
            local_parent.is_none()
        }
    };
    if !parentage_matches {
        return Err(BuildError::AuthorityFacts {
            entity,
            cause: AuthorityFactFault::Parentage {
                claimed: facts.parentage,
                local_parent,
            },
        });
    }
    validate_authority_availability(entity, AuthorityFactPlane::Source, facts.source, has_source)?;
    if facts.source != facts.source_file {
        return Err(BuildError::AuthorityFacts {
            entity,
            cause: AuthorityFactFault::SourceFileWithoutSource {
                source: facts.source,
                source_file: facts.source_file,
            },
        });
    }
    validate_authority_availability(
        entity,
        AuthorityFactPlane::SourceFile,
        facts.source_file,
        has_source,
    )?;
    validate_authority_availability(
        entity,
        AuthorityFactPlane::SemanticType,
        facts.semantic_type,
        has_semantic_type,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Members,
        facts.members,
        has_members,
    )?;
    // Documentation and attributes can be Captured with empty lists. A
    // nonempty value, however, cannot claim an unavailable authority plane.
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Documentation,
        facts.documentation,
        has_documentation,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Visibility,
        facts.visibility,
        has_visibility,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Attributes,
        facts.attributes,
        has_attributes,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::LanguageExtension,
        facts.language_extension,
        has_extension,
    )
}

pub(super) fn validate_authority_availability(
    entity: EntityId,
    plane: AuthorityFactPlane,
    claimed: FactAvailability,
    present: bool,
) -> Result<(), BuildError> {
    let matches = matches!(claimed, FactAvailability::Captured) == present;
    if matches {
        return Ok(());
    }
    Err(BuildError::AuthorityFacts {
        entity,
        cause: AuthorityFactFault::Availability {
            plane,
            claimed,
            present,
        },
    })
}

pub(super) fn validate_list_authority_availability(
    entity: EntityId,
    plane: AuthorityFactPlane,
    claimed: FactAvailability,
    present: bool,
) -> Result<(), BuildError> {
    if !present || claimed == FactAvailability::Captured {
        return Ok(());
    }
    Err(BuildError::AuthorityFacts {
        entity,
        cause: AuthorityFactFault::Availability {
            plane,
            claimed,
            present,
        },
    })
}

#[derive(Default)]
pub(super) struct BorrowedTreeCapacity {
    atom_values: usize,
    atom_bytes: usize,
    member_lists: usize,
    member_values: usize,
    attribute_lists: usize,
    attribute_values: usize,
    doc_lists: usize,
    doc_values: usize,
    extensions: LanguageExtensionCounts,
    source_values: usize,
    max_members: usize,
    max_attributes: usize,
    max_docs: usize,
}

impl BorrowedTreeCapacity {
    fn measure<Tree: FrontendTree + ?Sized>(tree: &Tree) -> Self {
        let mut capacity = Self::default();
        let has_items = usize::from(tree.items().len() != 0);
        capacity.member_lists = has_items;
        capacity.attribute_lists = has_items;
        capacity.doc_lists = has_items;
        for item in tree.items() {
            capacity.atom_values = capacity.atom_values.saturating_add(1);
            capacity.atom_bytes = capacity.atom_bytes.saturating_add(item.name.len());
            capacity.member_values = capacity.member_values.saturating_add(item.members.len());
            capacity.attribute_values = capacity
                .attribute_values
                .saturating_add(item.attributes.len());
            capacity.doc_values = capacity.doc_values.saturating_add(item.docs.len());
            capacity.member_lists = capacity
                .member_lists
                .saturating_add(usize::from(!item.members.is_empty()));
            capacity.attribute_lists = capacity
                .attribute_lists
                .saturating_add(usize::from(!item.attributes.is_empty()));
            capacity.doc_lists = capacity
                .doc_lists
                .saturating_add(usize::from(!item.docs.is_empty()));
            capacity.extensions.observe(item.extension);
            capacity.source_values = capacity
                .source_values
                .saturating_add(usize::from(item.source.is_some()));
            capacity.max_members = capacity.max_members.max(item.members.len());
            capacity.max_attributes = capacity.max_attributes.max(item.attributes.len());
            capacity.max_docs = capacity.max_docs.max(item.docs.len());
            for attribute in item.attributes {
                capacity.atom_values = capacity.atom_values.saturating_add(1);
                capacity.atom_bytes = capacity.atom_bytes.saturating_add(attribute.len());
            }
            for doc in item.docs {
                let text = match doc {
                    DocInput::Text(text) | DocInput::Code(text) => Some(*text),
                    DocInput::Link { label, .. } => Some(*label),
                    DocInput::SoftBreak | DocInput::HardBreak => None,
                };
                if let Some(text) = text {
                    capacity.atom_values = capacity.atom_values.saturating_add(1);
                    capacity.atom_bytes = capacity.atom_bytes.saturating_add(text.len());
                }
            }
        }
        capacity
    }
}
/// Exclusive typestate for a borrowed tree whose final entity range is known.
///
/// Frontends may intern arbitrarily rich types through this handle using the
/// range's final IDs, then commit their original slice-backed rows in one pass.
pub struct TreeBuilder<'builder, 'source> {
    builder: &'builder mut IrBuilder,
    versions: &'source [EntityVersion],
    range: EntityRange,
}

impl TreeBuilder<'_, '_> {
    #[must_use]
    pub const fn entities(&self) -> EntityRange {
        self.range
    }
    pub fn intern_atom(&mut self, bytes: &[u8]) -> Result<AtomId, BuildError> {
        self.builder.intern_atom(bytes)
    }
    pub fn intern_text(&mut self, text: &str) -> Result<TextId, BuildError> {
        self.builder.intern_text(text)
    }
    pub fn reserve_types(&mut self, additional: usize) {
        self.builder.reserve_types(additional);
    }
    pub fn intern_type(&mut self, ty: TypeExpr) -> Result<TypeId, BuildError> {
        self.builder.intern_type(ty)
    }
    pub fn intern_guarded<State: TypeState>(
        &mut self,
        ty: GuardedType<State>,
    ) -> Result<TypedTypeId<State>, BuildError> {
        self.builder.intern_guarded(ty)
    }
    pub fn intern_concrete(&mut self, ty: ConcreteType) -> Result<ConcreteTypeId, BuildError> {
        self.builder.intern_concrete(ty)
    }
    pub fn intern_computed(&mut self, ty: ComputedType) -> Result<ComputedTypeId, BuildError> {
        self.builder.intern_computed(ty)
    }
    pub fn intern_unknown(&mut self, ty: UnknownType) -> Result<UnknownTypeId, BuildError> {
        self.builder.intern_unknown(ty)
    }
    pub fn intern_types(&mut self, types: &[TypeId]) -> Result<TypeListId, BuildError> {
        self.builder.intern_types(types)
    }
    pub fn intern_tuple_elements(
        &mut self,
        elements: &[TupleElement],
    ) -> Result<TupleElementListId, BuildError> {
        self.builder.intern_tuple_elements(elements)
    }
    pub fn intern_object_members(
        &mut self,
        members: &[ObjectMember],
    ) -> Result<ObjectMemberListId, BuildError> {
        self.builder.intern_object_members(members)
    }
    pub fn intern_template_parts(
        &mut self,
        parts: &[TemplatePart],
    ) -> Result<TemplatePartListId, BuildError> {
        self.builder.intern_template_parts(parts)
    }
    pub fn intern_type_parameter_bounds(
        &mut self,
        bounds: &[TypeParameterBound],
    ) -> Result<TypeParameterBoundListId, BuildError> {
        self.builder.intern_type_parameter_bounds(bounds)
    }
    pub fn intern_type_parameters(
        &mut self,
        parameters: &[TypeParameter],
    ) -> Result<TypeParameterListId, BuildError> {
        self.builder.intern_type_parameters(parameters)
    }
    pub fn intern_free_predicates(
        &mut self,
        predicates: &[FreePredicate],
    ) -> Result<FreePredicateListId, BuildError> {
        self.builder.intern_free_predicates(predicates)
    }
    /// Interns an entity list while the reserved tree range keeps local
    /// entity identities branded to this one transaction.
    pub fn intern_members(&mut self, members: &[EntityId]) -> Result<EntityListId, BuildError> {
        self.builder.intern_members(members)
    }
    /// Interns an atom list while the reserved tree range owns its semantic
    /// extension projection.
    pub fn intern_attributes(&mut self, attributes: &[AtomId]) -> Result<AtomListId, BuildError> {
        self.builder.intern_attributes(attributes)
    }
    pub fn intern_external(&mut self, target: ExternalTarget) -> Result<ExternalId, BuildError> {
        self.builder.intern_external(target)
    }
    pub fn commit(
        self,
        items: &[TreeItemInput<'_>],
        links: &[TreeLinkInput],
    ) -> Result<EntityRange, BuildError> {
        self.builder.add_borrowed_tree(BorrowedTree {
            versions: self.versions,
            items,
            links,
        })
    }
}

pub(super) fn transpose_tree_id(
    range: EntityRange,
    id: Option<TreeEntityId>,
) -> Result<Option<EntityId>, BuildError> {
    id.map(|id| tree_id(range, id)).transpose()
}

pub(super) fn tree_id(range: EntityRange, id: TreeEntityId) -> Result<EntityId, BuildError> {
    range.get(id).ok_or(BuildError::InvalidTreeEntity {
        raw: id.raw,
        count: range.len,
    })
}

pub(super) fn validate_type(builder: &IrBuilder, ty: TypeExpr) -> Result<(), BuildError> {
    match ty {
        TypeExpr::Concrete(concrete) => validate_concrete_type(builder, concrete),
        TypeExpr::Computed(computed) => validate_computed_type(builder, computed),
        TypeExpr::Unknown(unknown) => unknown
            .spelling
            .map(|spelling| atom(builder, spelling))
            .transpose()
            .map(|_| ()),
    }
}

pub(super) fn validate_concrete_type(
    builder: &IrBuilder,
    ty: ConcreteType,
) -> Result<(), BuildError> {
    match ty {
        ConcreteType::Builtin(_) => Ok(()),
        ConcreteType::Literal(literal) => match literal {
            LiteralType::String(value)
            | LiteralType::Number(value)
            | LiteralType::BigInt(value) => atom(builder, value),
            LiteralType::Boolean(_) | LiteralType::Null | LiteralType::Undefined => Ok(()),
        },
        ConcreteType::Nominal(entity) => id(entity, builder.items.len(), SemanticSpace::Entity),
        ConcreteType::External(external) => id(
            external,
            builder.externals.as_slice().len(),
            SemanticSpace::External,
        ),
        ConcreteType::Parameter(atom_id) => atom(builder, atom_id),
        ConcreteType::Applied {
            constructor,
            arguments,
        } => {
            id(constructor, builder.types.len(), SemanticSpace::Type)?;
            validate_type_list(builder, arguments)
        }
        ConcreteType::Tuple(list) => {
            for element in
                list_or_dangling(&builder.tuple_elements, list, SemanticSpace::TupleElements)?
            {
                if let Some(label) = element.label {
                    atom(builder, label)?;
                }
                id(element.ty, builder.types.len(), SemanticSpace::Type)?;
            }
            Ok(())
        }
        ConcreteType::Object(list) => {
            for member in
                list_or_dangling(&builder.object_members, list, SemanticSpace::ObjectMembers)?
            {
                validate_object_member(builder, *member)?;
            }
            Ok(())
        }
        ConcreteType::Union(list)
        | ConcreteType::Intersection(list)
        | ConcreteType::ImplTrait(list)
        | ConcreteType::DynTrait(list) => validate_type_list(builder, list),
        ConcreteType::Function {
            parameters,
            results,
            abi,
            variadic,
            ..
        } => {
            let parameters = list_or_dangling(
                &builder.tuple_elements,
                parameters,
                SemanticSpace::TupleElements,
            )?;
            let mut typed_rest = false;
            for (position, parameter) in parameters.iter().copied().enumerate() {
                if let Some(label) = parameter.label {
                    atom(builder, label)?;
                }
                id(parameter.ty, builder.types.len(), SemanticSpace::Type)?;
                if parameter.kind == TupleElementKind::Rest {
                    let is_final = position + 1 == parameters.len();
                    if variadic != VariadicForm::TypedLast || !is_final {
                        return Err(BuildError::CallableElement {
                            role: CallableElementRole::Parameter,
                            position,
                            kind: parameter.kind,
                        });
                    }
                    typed_rest = true;
                }
            }
            if variadic == VariadicForm::TypedLast && !typed_rest {
                return Err(BuildError::MissingTypedVariadicParameter {
                    parameter_count: parameters.len(),
                });
            }
            for (position, result) in list_or_dangling(
                &builder.tuple_elements,
                results,
                SemanticSpace::TupleElements,
            )?
            .iter()
            .copied()
            .enumerate()
            {
                if let Some(label) = result.label {
                    atom(builder, label)?;
                }
                id(result.ty, builder.types.len(), SemanticSpace::Type)?;
                if result.kind != TupleElementKind::Required {
                    return Err(BuildError::CallableElement {
                        role: CallableElementRole::Result,
                        position,
                        kind: result.kind,
                    });
                }
            }
            if let Some(abi) = abi {
                atom(builder, abi)?;
            }
            Ok(())
        }
        ConcreteType::Reference {
            target, lifetime, ..
        } => {
            id(target, builder.types.len(), SemanticSpace::Type)?;
            if let Some(lifetime) = lifetime {
                atom(builder, lifetime)?;
            }
            Ok(())
        }
        ConcreteType::CxxReference { target, .. }
        | ConcreteType::CPointer { target }
        | ConcreteType::CBlockPointer { target } => {
            id(target, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::NativeCharacter { .. } => Ok(()),
        ConcreteType::CxxMemberPointer { owner, member } => {
            id(owner, builder.types.len(), SemanticSpace::Type)?;
            id(member, builder.types.len(), SemanticSpace::Type)?;
            if !is_cxx_record_owner(builder, owner) {
                return Err(BuildError::IllegalCxxMemberPointerOwner { owner });
            }
            Ok(())
        }
        ConcreteType::CQualified { target, qualifiers } => {
            if qualifiers.is_empty() {
                return Err(BuildError::EmptyCxxQualification);
            }
            id(target, builder.types.len(), SemanticSpace::Type)?;
            let target_type = builder.types.get(target).ok_or(BuildError::Dangling {
                space: SemanticSpace::Type,
                raw: target.raw,
            })?;
            let legal = match target_type.concrete() {
                Some(ConcreteType::CPointer { .. }) => true,
                Some(ConcreteType::Function { .. }) | Some(ConcreteType::CxxReference { .. }) => {
                    !qualifiers.const_ && !qualifiers.volatile && !qualifiers.restrict
                }
                _ => !qualifiers.restrict,
            };
            if !legal {
                return Err(BuildError::IllegalCQualifierTarget { target, qualifiers });
            }
            Ok(())
        }
        ConcreteType::Pointer { target, .. }
        | ConcreteType::Slice(target)
        | ConcreteType::Optional(target) => id(target, builder.types.len(), SemanticSpace::Type),
        ConcreteType::Array { element, shape } => {
            id(element, builder.types.len(), SemanticSpace::Type)?;
            if let ArrayShape::ConstExpression(expression) = shape {
                atom(builder, expression)?;
            }
            Ok(())
        }
        ConcreteType::Wildcard(WildcardBound::Unbounded) => Ok(()),
        ConcreteType::Wildcard(WildcardBound::Extends(bound))
        | ConcreteType::Wildcard(WildcardBound::Super(bound)) => {
            id(bound, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::Annotated { target, .. } => {
            id(target, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::Inferred(spelling) => spelling
            .map(|spelling| atom(builder, spelling))
            .transpose()
            .map(|_| ()),
        ConcreteType::QualifiedPath {
            self_type,
            trait_type,
            segments,
            spelling,
        } => {
            id(self_type, builder.types.len(), SemanticSpace::Type)?;
            optional_id(trait_type, builder.types.len(), SemanticSpace::Type)?;
            if let QualifiedSegments::Captured(segments) = segments {
                let segments =
                    list_or_dangling(&builder.atom_lists, segments, SemanticSpace::AtomList)?;
                if segments.is_empty() {
                    return Err(BuildError::EmptyQualifiedPath);
                }
                for segment in segments {
                    atom(builder, *segment)?;
                }
            }
            atom(builder, spelling)
        }
        ConcreteType::Map { key, value } => {
            id(key, builder.types.len(), SemanticSpace::Type)?;
            id(value, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::Channel { element, .. } => {
            id(element, builder.types.len(), SemanticSpace::Type)
        }
    }
}

/// A C++ member-pointer owner is a class/record nominal or one generic
/// application whose constructor is such a nominal. This deliberately does
/// not accept a source spelling, enum, pointer, or unrelated type variable.
pub(super) fn is_cxx_record_owner(builder: &IrBuilder, owner: TypeId) -> bool {
    let Some(TypeExpr::Concrete(owner)) = builder.types.get(owner) else {
        return false;
    };
    let nominal = match owner {
        ConcreteType::Nominal(entity) => Some(entity),
        ConcreteType::Applied { constructor, .. } => match builder.types.get(constructor) {
            Some(TypeExpr::Concrete(ConcreteType::Nominal(entity))) => Some(entity),
            _ => None,
        },
        _ => None,
    };
    nominal.is_some_and(|entity| {
        builder
            .items
            .kinds
            .get(entity.index())
            .is_some_and(|kind| *kind == ItemKind::Record)
    })
}

pub(super) fn validate_computed_type(
    builder: &IrBuilder,
    ty: ComputedType,
) -> Result<(), BuildError> {
    let type_len = builder.types.len();
    match ty {
        ComputedType::KeyOf(ty) | ComputedType::Awaited(ty) => {
            id(ty, type_len, SemanticSpace::Type)
        }
        ComputedType::TypeOf(query) => match query {
            TypeQuery::Entity(entity) => id(entity, builder.items.len(), SemanticSpace::Entity),
            TypeQuery::Path(path) => {
                for component in
                    list_or_dangling(&builder.atom_lists, path, SemanticSpace::AtomList)?
                {
                    atom(builder, *component)?;
                }
                Ok(())
            }
            TypeQuery::External(external) => id(
                external,
                builder.externals.as_slice().len(),
                SemanticSpace::External,
            ),
        },
        ComputedType::IndexedAccess { object, index } => {
            id(object, type_len, SemanticSpace::Type)?;
            id(index, type_len, SemanticSpace::Type)
        }
        ComputedType::Conditional {
            check,
            extends,
            then_type,
            else_type,
            ..
        } => {
            for ty in [check, extends, then_type, else_type] {
                id(ty, type_len, SemanticSpace::Type)?;
            }
            Ok(())
        }
        ComputedType::Mapped {
            parameter,
            constraint,
            name_as,
            value,
            ..
        } => {
            atom(builder, parameter)?;
            id(constraint, type_len, SemanticSpace::Type)?;
            optional_id(name_as, type_len, SemanticSpace::Type)?;
            id(value, type_len, SemanticSpace::Type)
        }
        ComputedType::Infer {
            parameter,
            constraint,
        } => {
            atom(builder, parameter)?;
            optional_id(constraint, type_len, SemanticSpace::Type)
        }
        ComputedType::TemplateLiteral(parts) => {
            for part in
                list_or_dangling(&builder.template_parts, parts, SemanticSpace::TemplateParts)?
            {
                match *part {
                    TemplatePart::Bytes(bytes) => atom(builder, bytes)?,
                    TemplatePart::Placeholder(ty) => id(ty, type_len, SemanticSpace::Type)?,
                }
            }
            Ok(())
        }
        ComputedType::Import {
            specifier,
            qualifier,
            arguments,
        } => {
            atom(builder, specifier)?;
            for component in
                list_or_dangling(&builder.atom_lists, qualifier, SemanticSpace::AtomList)?
            {
                atom(builder, *component)?;
            }
            validate_type_list(builder, arguments)
        }
        ComputedType::This => Ok(()),
    }
}

pub(super) fn validate_object_member(
    builder: &IrBuilder,
    member: ObjectMember,
) -> Result<(), BuildError> {
    let type_len = builder.types.len();
    match member {
        ObjectMember::Property { key, ty, .. } => {
            validate_property_key(builder, key)?;
            id(ty, type_len, SemanticSpace::Type)
        }
        ObjectMember::Method { key, signature, .. } => {
            validate_property_key(builder, key)?;
            id(signature, type_len, SemanticSpace::Type)
        }
        ObjectMember::Index {
            parameter,
            key,
            value,
            ..
        } => {
            atom(builder, parameter)?;
            id(key, type_len, SemanticSpace::Type)?;
            id(value, type_len, SemanticSpace::Type)
        }
        ObjectMember::Call(signature) | ObjectMember::Construct(signature) => {
            id(signature, type_len, SemanticSpace::Type)
        }
    }
}

pub(super) fn validate_property_key(
    builder: &IrBuilder,
    key: PropertyKey,
) -> Result<(), BuildError> {
    match key {
        PropertyKey::Named(atom_id)
        | PropertyKey::Private(atom_id)
        | PropertyKey::Numeric(atom_id) => atom(builder, atom_id),
        PropertyKey::Computed(ty) => id(ty, builder.types.len(), SemanticSpace::Type),
    }
}

pub(super) fn validate_type_list(builder: &IrBuilder, list: TypeListId) -> Result<(), BuildError> {
    for ty in list_or_dangling(&builder.type_lists, list, SemanticSpace::TypeList)? {
        id(*ty, builder.types.len(), SemanticSpace::Type)?;
    }
    Ok(())
}

pub(super) fn validate_doc(builder: &IrBuilder, doc: DocFragment) -> Result<(), BuildError> {
    match doc {
        DocFragment::Text(text) | DocFragment::Code(text) => text_id(builder, text),
        DocFragment::Link { label, target } => {
            text_id(builder, label)?;
            validate_target(builder, target)
        }
        DocFragment::SoftBreak | DocFragment::HardBreak => Ok(()),
    }
}

pub(super) fn validate_target(builder: &IrBuilder, target: LinkTarget) -> Result<(), BuildError> {
    match target {
        LinkTarget::Local(entity) => id(entity, builder.items.len(), SemanticSpace::Entity),
        LinkTarget::External(external) => id(
            external,
            builder.externals.as_slice().len(),
            SemanticSpace::External,
        ),
    }
}

pub(super) fn atom(builder: &IrBuilder, atom: AtomId) -> Result<(), BuildError> {
    builder
        .atoms
        .get(atom)
        .map(|_| ())
        .ok_or(BuildError::Dangling {
            space: SemanticSpace::Atom,
            raw: atom.raw,
        })
}

pub(super) fn text_id(builder: &IrBuilder, text: TextId) -> Result<(), BuildError> {
    builder
        .atoms
        .text(text)
        .map(|_| ())
        .ok_or(BuildError::Dangling {
            space: SemanticSpace::Text,
            raw: text.raw,
        })
}

fn id<T: Copy>(id: DenseId<T>, len: usize, space: SemanticSpace) -> Result<(), BuildError> {
    (id.index() < len)
        .then_some(())
        .ok_or(BuildError::Dangling { space, raw: id.raw })
}

pub(super) fn optional_id<T: Copy>(
    id_: Option<DenseId<T>>,
    len: usize,
    space: SemanticSpace,
) -> Result<(), BuildError> {
    id_.map_or(Ok(()), |id_| id(id_, len, space))
}

pub(super) fn list_or_dangling<T: Copy + Eq + Hash>(
    lists: &ListInterner<T>,
    id: ListId<T>,
    space: SemanticSpace,
) -> Result<&[T], BuildError> {
    lists
        .get(id)
        .ok_or(BuildError::Dangling { space, raw: id.raw })
}
pub(super) fn declaration_link_target(target: ExternalTarget) -> DeclarationLinkTarget {
    match target {
        ExternalTarget::Stable { target } => DeclarationLinkTarget::Stable(target),
        ExternalTarget::Foreign(target) => DeclarationLinkTarget::Foreign(target.identity),
        ExternalTarget::FragmentEntity { target, .. } => {
            DeclarationLinkTarget::FragmentEntity(target)
        }
    }
}

pub(super) fn prefix_sum(values: &mut [u32]) {
    for index in 1..values.len() {
        values[index] += values[index - 1];
    }
}

pub(super) fn sort_adjacency(
    links: &PackedLinks,
    order: &mut [LinkId],
    offsets: &mut [u32],
    reverse: bool,
) {
    order.sort_unstable_by_key(|link_id| {
        let link = links
            .get(*link_id)
            .expect("adjacency IDs originate from packed links");
        if reverse {
            let target = match link.target {
                LinkTarget::Local(target) => target.raw,
                LinkTarget::External(_) => u32::MAX,
            };
            (target, 0, link.from.raw, link.kind, link_id.raw)
        } else {
            let (external, target) = match link.target {
                LinkTarget::Local(target) => (0, target.raw),
                LinkTarget::External(target) => (1, target.raw),
            };
            (link.from.raw, external, target, link.kind, link_id.raw)
        }
    });
    for link_id in order.iter().copied() {
        let link = links
            .get(link_id)
            .expect("adjacency IDs originate from packed links");
        let raw = if reverse {
            match link.target {
                LinkTarget::Local(target) => target.raw,
                LinkTarget::External(_) => continue,
            }
        } else {
            link.from.raw
        };
        offsets[raw as usize + 1] += 1;
    }
    prefix_sum(offsets);
}

/// Sorts every authority-observed occurrence by owner and canonical relation
/// without collapsing sites. Fully equal sites are intentionally an unordered
/// multiset: no emission ordinal enters a canonical storage key.
pub(super) fn sort_occurrence_adjacency(
    links: &PackedLinks,
    occurrences: &PackedLinkOccurrences,
    order: &mut [LinkOccurrenceId],
    offsets: &mut [u32],
) {
    order.sort_unstable_by_key(|occurrence_id| {
        let occurrence = occurrences
            .get(*occurrence_id)
            .expect("occurrence IDs originate from packed occurrence rows");
        let relation = links
            .get(occurrence.link)
            .expect("occurrence relations were validated before indexing");
        let (external, target) = match relation.target {
            LinkTarget::Local(target) => (0, target.raw),
            LinkTarget::External(target) => (1, target.raw),
        };
        (
            relation.from.raw,
            external,
            target,
            relation.kind,
            occurrence.source.map_or(u32::MAX, |span| span.file().raw),
            occurrence.source.map_or(u32::MAX, SourceSpan::start),
            occurrence.source.map_or(u32::MAX, SourceSpan::end),
        )
    });
    for occurrence_id in order.iter().copied() {
        let occurrence = occurrences
            .get(occurrence_id)
            .expect("occurrence IDs originate from packed occurrence rows");
        let relation = links
            .get(occurrence.link)
            .expect("occurrence relations were validated before indexing");
        offsets[relation.from.index() + 1] += 1;
    }
    prefix_sum(offsets);
}
