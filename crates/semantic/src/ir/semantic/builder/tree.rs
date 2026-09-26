//! Borrowed compiler-tree ingestion.
//!
//! The tree handle publishes the final entity range before any row is copied.
//! One static walk reserves every arena, then the original slices are interned
//! in a single pass.

use super::super::columns::LanguageExtensionCounts;
use super::super::error::{BuildError, EntityRange};
use super::super::ids::{
    AtomListId, EntityListId, ExternalId, FreePredicateListId, ObjectMemberListId,
    TemplatePartListId, TreeEntityId, TupleElementListId, TypeListId, TypeParameterBoundListId,
    TypeParameterListId,
};
use super::super::packed_types::{
    ComputedType, ConcreteType, FreePredicate, ObjectMember, TemplatePart, TupleElement,
    TypeParameter, TypeParameterBound,
};
use super::super::relations::{
    DocFragment, DocInput, EntityVersion, ExternalTarget, Item, Link, LinkTarget,
};
use super::super::tree::{
    BorrowedTree, FrontendTree, TreeItemInput, TreeLinkInput, TreeLinkTarget,
};
use super::super::type_model::{
    ComputedTypeId, ConcreteTypeId, GuardedType, TypeExpr, TypeState, TypedTypeId, UnknownType,
    UnknownTypeId,
};
use super::IrBuilder;
use crate::ir::{AtomId, CapacityError, EntityId, TextId, TypeId};

#[derive(Default)]
struct BorrowedTreeCapacity {
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
        add_borrowed_tree(
            self.builder,
            BorrowedTree {
                versions: self.versions,
                items,
                links,
            },
        )
    }
}

fn transpose_tree_id(
    range: EntityRange,
    id: Option<TreeEntityId>,
) -> Result<Option<EntityId>, BuildError> {
    id.map(|id| tree_id(range, id)).transpose()
}

fn tree_id(range: EntityRange, id: TreeEntityId) -> Result<EntityId, BuildError> {
    range.get(id).ok_or(BuildError::InvalidTreeEntity {
        raw: id.raw,
        count: range.len,
    })
}

/// Reserves the final entity range for one borrowed tree before type construction.
pub(super) fn reserve_tree<'builder, 'source>(
    builder: &'builder mut IrBuilder,
    versions: &'source [EntityVersion],
) -> Result<TreeBuilder<'builder, 'source>, BuildError> {
    let start = EntityId::try_from_index(builder.items.len()).map_err(|_| CapacityError {
        space: crate::ir::CapacitySpace::Value,
        actual: builder.items.len(),
    })?;
    let len = u32::try_from(versions.len()).map_err(|_| CapacityError {
        space: crate::ir::CapacitySpace::Value,
        actual: versions.len(),
    })?;
    Ok(TreeBuilder {
        builder,
        versions,
        range: EntityRange { start, len },
    })
}

/// Condenses a whole borrowed compiler tree with one copy of each distinct atom and list.
pub(super) fn add_borrowed_tree(
    builder: &mut IrBuilder,
    tree: BorrowedTree<'_>,
) -> Result<EntityRange, BuildError> {
    add_frontend_tree(builder, &tree)
}

/// Condenses any compiler-owned tree through the static frontend adapter.
pub(super) fn add_frontend_tree<Tree: FrontendTree + ?Sized>(
    builder: &mut IrBuilder,
    tree: &Tree,
) -> Result<EntityRange, BuildError> {
    let item_count = tree.items().len();
    if tree.versions().len() != item_count {
        return Err(BuildError::TreeVersionCount {
            versions: tree.versions().len(),
            items: item_count,
        });
    }
    reserve_frontend_tree(builder, tree);
    let start = EntityId::try_from_index(builder.items.len()).map_err(|_| CapacityError {
        space: crate::ir::CapacitySpace::Value,
        actual: builder.items.len(),
    })?;
    let count = u32::try_from(item_count).map_err(|_| CapacityError {
        space: crate::ir::CapacitySpace::Value,
        actual: item_count,
    })?;
    let range = EntityRange { start, len: count };

    for (input, version) in tree.items().zip(tree.versions()) {
        let name = builder.intern_atom(input.name)?;
        let parent = transpose_tree_id(range, input.parent)?;

        builder.entity_scratch.clear();
        for member in input.members {
            builder.entity_scratch.push(tree_id(range, *member)?);
        }
        let members = builder.entity_lists.intern(&builder.entity_scratch)?;

        builder.atom_scratch.clear();
        for attribute in input.attributes {
            builder.atom_scratch.push(builder.atoms.intern(attribute)?);
        }
        let attributes = builder.atom_lists.intern(&builder.atom_scratch)?;

        builder.doc_scratch.clear();
        for fragment in input.docs {
            let fragment = match *fragment {
                DocInput::Text(text) => DocFragment::Text(builder.atoms.intern_text(text)?),
                DocInput::Code(text) => DocFragment::Code(builder.atoms.intern_text(text)?),
                DocInput::Link { label, target } => DocFragment::Link {
                    label: builder.atoms.intern_text(label)?,
                    target: match target {
                        TreeLinkTarget::Local(local) => LinkTarget::Local(tree_id(range, local)?),
                        TreeLinkTarget::External(external) => LinkTarget::External(external),
                    },
                },
                DocInput::SoftBreak => DocFragment::SoftBreak,
                DocInput::HardBreak => DocFragment::HardBreak,
            };
            builder.doc_scratch.push(fragment);
        }
        let docs = builder.docs.intern(&builder.doc_scratch)?;
        builder.add_item(
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
        builder.add_link_occurrence(
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

fn reserve_frontend_tree<Tree: FrontendTree + ?Sized>(builder: &mut IrBuilder, tree: &Tree) {
    let capacity = BorrowedTreeCapacity::measure(tree);
    let item_count = tree.items().len();
    let link_count = tree.links().len();
    builder.items.reserve_exact(item_count);
    builder.authority_facts.reserve(item_count);
    builder.sources.reserve(item_count, capacity.source_values);
    builder.extensions.reserve(item_count, capacity.extensions);
    let link_source_values = tree.links().filter(|link| link.source.is_some()).count();
    builder.links.reserve_exact(link_count, link_source_values);
    builder
        .link_occurrences
        .reserve_exact(link_count, link_source_values);
    builder.occurrence_authority.reserve(link_count);
    builder.link_index.reserve(link_count);
    builder
        .atoms
        .reserve(capacity.atom_values, capacity.atom_bytes);
    builder
        .entity_lists
        .reserve(capacity.member_lists, capacity.member_values);
    builder
        .atom_lists
        .reserve(capacity.attribute_lists, capacity.attribute_values);
    builder
        .docs
        .reserve(capacity.doc_lists, capacity.doc_values);
    builder.entity_scratch.reserve(capacity.max_members);
    builder.atom_scratch.reserve(capacity.max_attributes);
    builder.doc_scratch.reserve(capacity.max_docs);
}
