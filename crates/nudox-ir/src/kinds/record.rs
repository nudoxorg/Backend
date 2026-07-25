use crate::{List, index::EntryIndex, kinds::Type, visitor::Visitor};

// FIXME: generics, inline methods/constructors, and index signatures are not
// yet ported. Methods are expected to become child Function entries once the
// builder grows nested-item support; generics await their own subsystem.

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// The fields of the record, in declaration order.
    ///
    /// An empty list denotes a record with no statically-known fields — e.g. a
    /// dynamic object in JavaScript/Python, or a unit struct.
    pub fields: List<EntryIndex<Field>>,

    /// Base classes, implemented interfaces, or otherwise explicitly-named
    /// super-types of this record.
    pub super_types: List<Type>,
}

#[bon::bon]
impl Record {
    #[builder]
    pub fn new(
        #[builder(default, with = FromIterator::from_iter)] fields: List<EntryIndex<Field>>,
        #[builder(default, with = FromIterator::from_iter)] super_types: List<Type>,
    ) -> Self {
        Record {
            fields,
            super_types,
        }
    }
}

// FIXME: `default_value` (a `ConstExpr`) and source-level decorators are not
// yet ported; both await the const-expression / attribute subsystems.

/// A field or property of a containing type.
///
/// The field's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Field {
    /// How the field is keyed within its record.
    pub key: FieldKey,

    /// The declared type of the field, if known.
    ///
    /// Gradually-typed and dynamic languages may omit this.
    pub ty: Option<Type>,

    /// Field-level modifiers (mutability, optionality, storage).
    pub attributes: List<FieldAttribute>,
}

#[bon::bon]
impl Field {
    #[builder]
    pub fn new(
        key: FieldKey,
        ty: Option<Type>,
        #[builder(default, with = FromIterator::from_iter)] attributes: List<FieldAttribute>,
    ) -> Self {
        Field {
            key,
            ty,
            attributes,
        }
    }
}

// FIXME: computed keys (`[Symbol.iterator]`, `["k" + i]`) are not yet
// representable; they await the const-expression subsystem.

/// How a [`Field`] is keyed within its record.
///
/// The textual name (when present) lives on the entry's `Symbol`; this only
/// records the *shape* of the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum FieldKey {
    /// A named field, keyed by the entry's `Symbol` name (`point.x`).
    Named,

    /// A positional field in a tuple or tuple-struct (`pair.0`).
    Positional(usize),
}

/// A modifier applied to a [`Field`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum FieldAttribute {
    /// The field may be reassigned after construction.
    Mutable,

    /// The field may be absent (`foo?: T`).
    Optional,

    /// A static/class-level member rather than a per-instance one.
    Static,
}
