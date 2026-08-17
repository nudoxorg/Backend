//! `Record` and `Field` kinds with forms, keys, and attributes.
use crate::{
    List,
    index::Ref,
    kinds::{AutoFact, GenericParam, Type, WherePred},
    visitor::Visitor,
};

// Generics and where-clauses are now modelled via `generics:
// List<GenericParam>` and `wheres: List<WherePred>` (bounds represented as
// `List<Type>`). Remaining deferred items: inline methods/constructors, index
// signatures, const-expressions, variance, and associated-item lists.

/// The syntactic form of a record type.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize, Default,
)]
pub enum RecordForm {
    /// A named-field struct (`struct Foo { x: i32 }`).
    #[default]
    Struct,

    /// A tuple struct (`struct Foo(i32, i32)`).
    Tuple,

    /// A unit struct (`struct Foo`).
    Unit,

    /// An untagged union whose fields share storage (C/C++ `union`, Rust
    /// `union`). Distinct from [`RecordForm::Struct`]: the fields are
    /// alternatives rather than co-resident members, which changes both layout
    /// and semantics — collapsing it to `Struct` misreports the type.
    Union,
}

#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// The syntactic form of this record type.
    pub form: RecordForm,

    /// The fields of the record, in declaration order.
    ///
    /// An empty list denotes a record with no statically-known fields — e.g. a
    /// dynamic object in JavaScript/Python, or a unit struct.
    pub fields: List<Ref<Field>>,

    /// Base classes, implemented interfaces, or otherwise explicitly-named
    /// super-types of this record.
    pub super_types: List<Type>,

    /// Generic parameters declared on this record, in declaration order.
    pub generics: List<GenericParam>,

    /// Where-clause predicates for this record, in declaration order.
    pub wheres: List<WherePred>,

    /// Recorded auto-trait implementation facts for this record type.
    ///
    /// Populated by oracle producers (rustdoc, ra) when they expose auto-trait
    /// information; empty when unavailable. Downstream consumers use these for
    /// thread-safety and unwind-safety reasoning without re-deriving bounds.
    pub auto: List<AutoFact>,
}

#[bon::bon]
impl Record {
    #[builder]
    pub fn new(
        #[builder(default)] form: RecordForm,
        #[builder(default, with = FromIterator::from_iter)] fields: List<Ref<Field>>,
        #[builder(default, with = FromIterator::from_iter)] super_types: List<Type>,
        #[builder(default, with = FromIterator::from_iter)] generics: List<GenericParam>,
        #[builder(default, with = FromIterator::from_iter)] wheres: List<WherePred>,
        #[builder(default, with = FromIterator::from_iter)] auto: List<AutoFact>,
    ) -> Self {
        Record {
            form,
            fields,
            super_types,
            generics,
            wheres,
            auto,
        }
    }
}

// FIXME: `default_value` (a `ConstExpr`) is not yet ported; it awaits the
// const-expression subsystem.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::facts::{AutoFact, AutoState, AutoTrait};

    /// Builder + serde round-trip for `Record.auto` (gap 1).
    #[test]
    fn record_auto_roundtrip() {
        let r = Record::builder()
            .auto([
                AutoFact {
                    trait_: AutoTrait::Send,
                    state: AutoState::Yes,
                },
                AutoFact {
                    trait_: AutoTrait::Unpin,
                    state: AutoState::Cond,
                },
            ])
            .build();

        assert_eq!(r.auto.len(), 2);
        assert_eq!(r.auto[1].state, AutoState::Cond);

        let json = serde_json::to_string(&r).expect("serialize failed");
        let rt: Record = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(r, rt);
    }
}

/// A field or property of a containing type.
///
/// The field's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol).
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
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
