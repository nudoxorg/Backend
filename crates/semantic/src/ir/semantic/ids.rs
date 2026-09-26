use super::packed_types::{
    FreePredicate, ObjectMember, TemplatePart, TupleElement, TypeParameter, TypeParameterBound,
};
use super::relations::DocFragment;
use crate::ir::{AtomId, DenseId, EntityId, ListId, TypeId};

/// Marker for a cross-package graph target.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum External {}
/// Marker for a graph-link row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LinkSpace {}
/// Marker for one observed occurrence of a canonical graph relation.
///
/// A relation is unique by `(from, target, kind)`; an occurrence is not.
/// Multiple written reference sites can prove the same relation with distinct
/// spans and confidence, and remain independently queryable through this ID.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LinkOccurrenceSpace {}
/// Marker for an entity coordinate local to one borrowed tree submission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TreeEntity {}

/// Dense ID of an external symbol descriptor.
pub type ExternalId = DenseId<External>;
/// Dense ID of a graph link.
pub type LinkId = DenseId<LinkSpace>;
/// Dense ID of one authority-observed graph occurrence site.
pub type LinkOccurrenceId = DenseId<LinkOccurrenceSpace>;
/// Entity coordinate local to a [`BorrowedTree`].
pub type TreeEntityId = DenseId<TreeEntity>;
/// Interned sequence of semantic types.
pub type TypeListId = ListId<TypeId>;
/// Interned sequence of child entities.
pub type EntityListId = ListId<EntityId>;
/// Interned sequence of atoms.
pub type AtomListId = ListId<AtomId>;
/// Interned sequence of documentation fragments.
pub type DocId = ListId<DocFragment>;
/// Interned sequence of TypeScript/Rust tuple elements with labels and modifiers.
pub type TupleElementListId = ListId<TupleElement>;
/// Interned sequence of structural object members.
pub type ObjectMemberListId = ListId<ObjectMember>;
/// Interned sequence of template-literal pieces.
pub type TemplatePartListId = ListId<TemplatePart>;
/// Interned sequence of generic parameter declarations.
pub type TypeParameterListId = ListId<TypeParameter>;
/// Interned source-ordered bounds of one generic parameter.
///
/// A bound is deliberately not just a type ID: Rust lifetime bounds occupy
/// the same written sequence as trait bounds, and a renderer/discovery view
/// must never recover their order from language-specific side tables.
pub type TypeParameterBoundListId = ListId<TypeParameterBound>;
/// Interned sequence of Rust free predicates.
///
/// A free predicate names a subject that is *not* a declared generic
/// parameter (`Vec<T>: Clone`, `T::Item: Clone`, `Self: Sized`, a trait
/// supertrait) together with its ordered bounds.  The bounds reuse the shared
/// [`TypeParameterBound`] lane; only the row table is Rust-specific.
pub type FreePredicateListId = ListId<FreePredicate>;

/// Compatibility spelling for the one frozen declaration-kind vocabulary.
///
/// The underlying type and every discriminant come from
/// `crate::ir_vocabulary::EntityKind`; `TypeAlias` remains only as that
/// type's narrow associated compatibility constant.
pub type ItemKind = crate::ir_vocabulary::EntityKind;
