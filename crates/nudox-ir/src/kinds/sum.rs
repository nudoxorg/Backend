use crate::{
    List,
    index::EntryIndex,
    kinds::{ConstExpr, Field, Generics},
    visitor::Visitor,
};

/// An algebraic sum type: a Rust `enum`, a discriminated/tagged union, or a
/// sealed class hierarchy.
///
/// The type's name lives on the owning [`Entry`](crate::entry::Entry)'s
/// [`Symbol`](crate::entry::Symbol).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Enum {
    /// Generic parameters and `where`-clause of the sum type.
    pub generics: Generics,

    /// The variants, in declaration order.
    pub variants: List<EntryIndex<Variant>>,
}

#[bon::bon]
impl Enum {
    #[builder]
    pub fn new(
        #[builder(default)] generics: Generics,
        #[builder(with = FromIterator::from_iter)] variants: List<EntryIndex<Variant>>,
    ) -> Self {
        Enum { generics, variants }
    }
}

/// A single variant of an [`Enum`].
///
/// Payload-carrying variants reference their payload as [`Field`] entries: tuple
/// variants use [`FieldKey::Positional`](crate::kinds::FieldKey), struct variants
/// use named fields, and unit variants carry none.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Variant {
    /// The variant's payload fields; empty for a unit variant.
    pub fields: List<EntryIndex<Field>>,

    /// An explicit discriminant value (`Foo = 3`), if written.
    pub discriminant: Option<ConstExpr>,
}

#[bon::bon]
impl Variant {
    #[builder]
    pub fn new(
        #[builder(with = FromIterator::from_iter)] fields: List<EntryIndex<Field>>,
        discriminant: Option<ConstExpr>,
    ) -> Self {
        Variant {
            fields,
            discriminant,
        }
    }
}
