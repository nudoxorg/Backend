//! Arena builder for the condensed semantic image.
//!
//! Interning, borrowed-tree ingestion, row admission, and compressed link
//! layout are separate. Callers still construct an [`IrBuilder`] and finish
//! it into an immutable image.

use super::columns::{
    IrIndices, ItemColumns, LanguageExtensions, PackedLinkOccurrences, PackedLinks, SourceColumns,
};
use super::error::{BuildError, EntityRange, SemanticSpace};
use super::ids::{
    AtomListId, DocId, EntityListId, External, ExternalId, FreePredicateListId, ItemKind, LinkId,
    LinkOccurrenceId, ObjectMemberListId, TemplatePartListId, TupleElementListId, TypeListId,
    TypeParameterBoundListId, TypeParameterListId,
};
use super::image::Ir;
use super::language_facts::{LanguageExtensionInput, SemanticImageAuthority};
use super::packed_types::{
    ComputedType, ConcreteType, FreePredicate, ObjectMember, TemplatePart, TupleElement,
    TypeInterner, TypeParameter, TypeParameterBound,
};
use super::relations::{
    DocFragment, EntityVersion, ExternalTarget, Item, Link, LinkKey, LinkOccurrence,
    canonical_relation_evidence_precedes,
};
use super::tree::{BorrowedTree, FrontendTree};
use super::type_model::{
    ComputedTypeId, ConcreteTypeId, GuardedType, TypeExpr, TypeState, TypedTypeId, UnknownType,
    UnknownTypeId,
};
use crate::ir::{
    AtomId, AtomInterner, CapacityError, DeclarationKey, EntityAuthorityFacts, EntityId,
    ImageProvenance, ImageProvenanceClaim, Interner, ListInterner, OccurrenceAuthorityFacts,
    PackageLineage, SemanticScopeClaim, SemanticScopeFacts, SourceIdentity, TextId, TypeId,
    authority::{AuthorityColumns, OccurrenceAuthorityColumn},
    interner::{HashIndex, hash},
};
use crate::vocabulary::{CompileRecipeFact, LanguageProfile, PackageUrl};
use alloc::{vec, vec::Vec};
use backend_version::{ContentId, SemanticScopeDomain};
mod adjacency;
mod check;
mod tree;

pub(super) use adjacency::{
    declaration_link_target, prefix_sum, sort_adjacency, sort_occurrence_adjacency,
};
pub(super) use check::{
    atom, optional_id, validate_atom_list, validate_entity_list, validate_free_predicates,
    validate_type_list, validate_type_parameters,
};
pub use tree::TreeBuilder;

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
        tree::reserve_tree(self, versions)
    }

    /// Condenses a whole borrowed compiler tree with one copy of each distinct atom/list.
    pub fn add_borrowed_tree(&mut self, tree: BorrowedTree<'_>) -> Result<EntityRange, BuildError> {
        tree::add_borrowed_tree(self, tree)
    }

    /// Condenses any compiler-owned tree through a zero-allocation static
    /// adapter. Frontends yield stack rows directly from their native AST.
    pub fn add_frontend_tree<Tree: FrontendTree + ?Sized>(
        &mut self,
        tree: &Tree,
    ) -> Result<EntityRange, BuildError> {
        tree::add_frontend_tree(self, tree)
    }

    /// Validates all caller-constructible IDs and freezes compact query indices.
    pub fn finish(self) -> Result<Ir, BuildError> {
        check::validate(&self)?;
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
}
