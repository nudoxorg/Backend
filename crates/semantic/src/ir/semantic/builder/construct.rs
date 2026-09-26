//! IrBuilder construction, interning, and finish.

use super::super::columns::IrIndices;
use super::super::error::{BuildError, EntityRange, SemanticSpace};
use super::super::ids::{
    AtomListId, DocId, EntityListId, ExternalId, FreePredicateListId, ItemKind, LinkId,
    LinkOccurrenceId, ObjectMemberListId, TemplatePartListId, TupleElementListId, TypeListId,
    TypeParameterBoundListId, TypeParameterListId,
};
use super::super::image::Ir;
use super::super::language_facts::{LanguageExtensionInput, SemanticImageAuthority};
use super::super::packed_types::{
    ComputedType, ConcreteType, FreePredicate, ObjectMember, TemplatePart, TupleElement,
    TypeParameter, TypeParameterBound,
};
use super::super::relations::{
    DocFragment, DocInput, EntityVersion, ExternalTarget, ForeignTargetOrigin, Item, Link, LinkKey,
    LinkOccurrence, LinkTarget, canonical_relation_evidence_precedes,
};
use super::super::tree::{BorrowedTree, FrontendTree, TreeLinkTarget};
use super::super::type_model::{
    ComputedTypeId, ConcreteTypeId, GuardedType, TypeExpr, TypeState, TypedTypeId, UnknownType,
    UnknownTypeId, Visibility,
};
use super::{
    BorrowedTreeCapacity, IrBuilder, TreeBuilder, atom, id, list_or_dangling, optional_id,
    transpose_tree_id, tree_id, validate_doc, validate_entity_authority, validate_target,
    validate_type,
};
use crate::ir::interner::hash;
use crate::ir::{
    AtomId, AuthorityFactFault, AuthorityFactPlane, CapacityError, DeclarationKey,
    EntityAuthorityFacts, EntityId, FactAvailability, ImageProvenance, ImageProvenanceClaim,
    OccurrenceAuthorityFacts, PackageLineage, SemanticScopeClaim, SemanticScopeFacts,
    SourceIdentity, TextId, TypeId,
};
use crate::vocabulary::{CompileRecipeFact, LanguageProfile, PackageUrl};
use alloc::vec;
use backend_version::{ContentId, SemanticScopeDomain};

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
