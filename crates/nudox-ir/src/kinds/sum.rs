use crate::{
    List,
    index::Ref,
    kinds::{AutoFact, Field, GenericParam, WherePred},
    visitor::Visitor,
};

// Generics and where-clauses are now modelled via `generics:
// List<GenericParam>` and `wheres: List<WherePred>` (bounds represented as
// `List<Type>`). Remaining deferred items: const-expressions and variance.

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

    /// Recorded auto-trait implementation facts for this enum type.
    ///
    /// Populated by oracle producers when they expose auto-trait information;
    /// empty when unavailable. Mirrors the same field on
    /// [`Record`](crate::kinds::Record).
    pub auto: List<AutoFact>,
}

#[bon::bon]
impl Enum {
    #[builder]
    pub fn new(
        #[builder(default, with = FromIterator::from_iter)] variants: List<Ref<Variant>>,
        #[builder(default, with = FromIterator::from_iter)] generics: List<GenericParam>,
        #[builder(default, with = FromIterator::from_iter)] wheres: List<WherePred>,
        #[builder(default, with = FromIterator::from_iter)] auto: List<AutoFact>,
    ) -> Self {
        Enum {
            variants,
            generics,
            wheres,
            auto,
        }
    }
}

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

    /// Explicit discriminant value, rendered as source text, if present.
    ///
    /// Examples: `"3"` for `A = 3` (Rust/C#), `"\"a\""` for `A = "a"`
    /// (TypeScript string enum). `None` means the discriminant is implicit
    /// (language-assigned or not applicable, as in Java).
    ///
    /// A rendered `String` is the right type here: the value crosses language
    /// boundaries (Rust `#[repr(u8)] A = 3`, C# `enum : byte { A = 3 }`,
    /// TypeScript `A = "a"`) where the underlying storage type varies and an
    /// integer would lose the TypeScript string-enum case. Producers already
    /// emit this form; a typed alternative would require a large sum type
    /// that adds complexity without practical benefit.
    pub discr: Option<String>,
}

#[bon::bon]
impl Variant {
    #[builder]
    pub fn new(
        #[builder(default)] form: VariantForm,
        #[builder(default, with = FromIterator::from_iter)] fields: List<Ref<Field>>,
        discr: Option<String>,
    ) -> Self {
        Variant {
            form,
            fields,
            discr,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::facts::{AutoFact, AutoState, AutoTrait};

    /// Builder + serde round-trip for `Variant.discr` (gap 2).
    #[test]
    fn variant_discr_roundtrip() {
        let v = Variant::builder()
            .form(VariantForm::Unit)
            .discr("42".to_owned())
            .build();

        assert_eq!(v.discr, Some("42".to_owned()));

        let json = serde_json::to_string(&v).expect("serialize failed");
        let rt: Variant = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(v, rt);
    }

    /// Variant with no discriminant serialises and deserialises as `None`.
    #[test]
    fn variant_discr_none_roundtrip() {
        let v = Variant::builder().build();
        assert_eq!(v.discr, None);

        let json = serde_json::to_string(&v).expect("serialize failed");
        let rt: Variant = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(v, rt);
    }

    /// `Enum.auto` builder + serde round-trip (gap 1).
    #[test]
    fn enum_auto_roundtrip() {
        let e = Enum::builder()
            .auto([
                AutoFact {
                    trait_: AutoTrait::Send,
                    state: AutoState::Yes,
                },
                AutoFact {
                    trait_: AutoTrait::Sync,
                    state: AutoState::No,
                },
            ])
            .build();

        assert_eq!(e.auto.len(), 2);
        assert_eq!(e.auto[0].trait_, AutoTrait::Send);

        let json = serde_json::to_string(&e).expect("serialize failed");
        let rt: Enum = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(e, rt);
    }
}
