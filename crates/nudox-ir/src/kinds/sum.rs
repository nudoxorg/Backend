use crate::{
    List,
    index::Ref,
    kinds::{Field, GenericParam, WherePred},
    visitor::Visitor,
};

// Generics and where-clauses are now modelled via `generics:
// List<GenericParam>` and `wheres: List<WherePred>` (bounds represented as
// `List<Type>`). Remaining deferred items: explicit discriminant values,
// const-expressions, and variance.

/// An algebraic sum type: a Rust `enum`, a discriminated/tagged union, or a
/// sealed class hierarchy.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Enum {
    /// The variants of this sum type, in declaration order.
    pub variants: List<Ref<Variant>>,

    /// Generic parameters declared on this enum, in declaration order.
    pub generics: List<GenericParam>,

    /// Where-clause predicates for this enum, in declaration order.
    pub wheres: List<WherePred>,
}

#[bon::bon]
impl Enum {
    #[builder]
    pub fn new(
        #[builder(default, with = FromIterator::from_iter)] variants: List<Ref<Variant>>,
        #[builder(default, with = FromIterator::from_iter)] generics: List<GenericParam>,
        #[builder(default, with = FromIterator::from_iter)] wheres: List<WherePred>,
    ) -> Self {
        Enum {
            variants,
            generics,
            wheres,
        }
    }
}

// FIXME: an explicit discriminant value (e.g. `Foo = 3`) is not yet
// represented; it awaits the const-expression subsystem.

/// The syntactic form of an enum [`Variant`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize, Default,
)]
pub enum VariantForm {
    /// A unit variant carrying no payload (`Foo::Bar`).
    #[default]
    Unit,

    /// A tuple variant (`Foo::Bar(i32, i32)`).
    Tuple,

    /// A struct variant (`Foo::Bar { x: i32 }`).
    Struct,
}

/// A single variant of an [`Enum`].
///
/// Payload-carrying variants reference their payload as [`Field`] entries:
/// tuple variants use [`FieldKey::Positional`](crate::kinds::record::FieldKey),
/// struct variants use named fields, and unit variants carry none.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Variant {
    /// The syntactic form of this variant.
    pub form: VariantForm,

    /// The variant's payload fields; empty for a unit variant.
    pub fields: List<Ref<Field>>,
}

#[bon::bon]
impl Variant {
    #[builder]
    pub fn new(
        #[builder(default)] form: VariantForm,
        #[builder(default, with = FromIterator::from_iter)] fields: List<Ref<Field>>,
    ) -> Self {
        Variant { form, fields }
    }
}
