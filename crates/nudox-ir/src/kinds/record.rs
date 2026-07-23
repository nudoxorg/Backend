use crate::{
    List,
    index::EntryIndex,
    kinds::{ConstExpr, Generics, Type},
    visitor::Visitor,
};

/// A product type: struct, class, record, data class, or object type.
///
/// Inline methods, constructors, and nested items are modelled as child
/// entries; this carries the record's fields, super-types, and generics.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// Generic parameters and `where`-clause of the record.
    pub generics: Generics,

    /// The fields, in declaration order. Empty denotes a record with no
    /// statically-known fields (a dynamic object, or a unit struct).
    pub fields: List<EntryIndex<Field>>,

    /// Base classes and implemented interfaces named directly by the record.
    pub super_types: List<EntryIndex<Type>>,

    /// TypeScript-style index signatures (`{ [key: string]: number }`).
    ///
    /// Call and construct signatures (`{ (x): y }`, `{ new(x): y }`) are modelled
    /// as child [`Function`](crate::kinds::Function) entries instead.
    pub index_signatures: List<IndexSignature>,
}

#[bon::bon]
impl Record {
    #[builder]
    pub fn new(
        #[builder(default)] generics: Generics,
        #[builder(with = FromIterator::from_iter)] fields: List<EntryIndex<Field>>,
        #[builder(with = FromIterator::from_iter)] super_types: List<EntryIndex<Type>>,
        #[builder(with = FromIterator::from_iter)] index_signatures: List<IndexSignature>,
    ) -> Self {
        Record {
            generics,
            fields,
            super_types,
            index_signatures,
        }
    }
}

/// A TypeScript index signature describing dynamic keyed access
/// (`{ [key: string]: number }`).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct IndexSignature {
    /// The key type (usually `string` or `number`).
    pub key: EntryIndex<Type>,

    /// The value type produced by keyed access.
    pub value: EntryIndex<Type>,
}

/// A field or property of a containing type.
///
/// The field's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Field {
    /// How the field is keyed within its record.
    pub key: FieldKey,

    /// The declared type of the field, if known.
    pub ty: Option<EntryIndex<Type>>,

    /// Field-level modifiers (mutability, optionality, storage).
    pub attributes: List<FieldAttribute>,

    /// The field's default / initializer value, if any.
    pub default: Option<ConstExpr>,
}

#[bon::bon]
impl Field {
    #[builder]
    pub fn new(
        key: FieldKey,
        ty: Option<EntryIndex<Type>>,
        #[builder(with = FromIterator::from_iter)] attributes: List<FieldAttribute>,
        default: Option<ConstExpr>,
    ) -> Self {
        Field {
            key,
            ty,
            attributes,
            default,
        }
    }
}

/// How a [`Field`] is keyed within its record.
///
/// The textual name (when present) lives on the entry's `Symbol`; this records
/// the *shape* of the key.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum FieldKey {
    /// A named field, keyed by the entry's `Symbol` name (`point.x`).
    Named,

    /// A positional field in a tuple or tuple-struct (`pair.0`).
    Positional(usize),

    /// A computed key (`[Symbol.iterator]`, `["k" + i]`).
    Computed(ConstExpr),
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

    /// A read-only field (`readonly`, `const` member, `final`).
    ReadOnly,

    /// A transient / non-serialized field.
    Transient,
}
