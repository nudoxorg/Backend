use crate::{List, index::EntryIndex, kinds::Field, visitor::Visitor};

// FIXME: generics are not yet ported; they await the generics subsystem.

/// An algebraic sum type: a Rust `enum`, a discriminated/tagged union, or a
/// sealed class hierarchy.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Enum {
    /// The variants of this sum type, in declaration order.
    pub variants: List<EntryIndex<Variant>>,
}

#[bon::bon]
impl Enum {
    #[builder]
    pub fn new(
        #[builder(with = FromIterator::from_iter)] variants: List<EntryIndex<Variant>>,
    ) -> Self {
        Enum { variants }
    }
}

// FIXME: an explicit discriminant value (e.g. `Foo = 3`) is not yet
// represented; it awaits the const-expression subsystem.

/// A single variant of an [`Enum`].
///
/// Payload-carrying variants reference their payload as [`Field`] entries:
/// tuple variants use [`FieldKey::Positional`](crate::kinds::FieldKey), struct
/// variants use named fields, and unit variants carry none.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Variant {
    /// The variant's payload fields; empty for a unit variant.
    pub fields: List<EntryIndex<Field>>,
}

#[bon::bon]
impl Variant {
    #[builder]
    pub fn new(#[builder(with = FromIterator::from_iter)] fields: List<EntryIndex<Field>>) -> Self {
        Variant { fields }
    }
}
