use crate::{
    List,
    index::EntryIndex,
    kinds::{Bound, Generics, TraitRef, Type},
    visitor::Visitor,
};

/// A trait, protocol, interface, or typeclass definition.
///
/// Required and provided members — methods, associated types, and associated
/// constants — are modelled as child entries ([`Function`](crate::kinds::Function),
/// [`Alias`](crate::kinds::Alias), [`Const`](crate::kinds::Const)) reached
/// through the owning entry's children, so the trait body itself only carries
/// the cross-cutting header data.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Trait {
    /// Generic parameters and `where`-clause of the trait.
    pub generics: Generics,

    /// Supertraits and lifetime bounds (`trait Ord: Eq`, `: 'static`).
    pub super_traits: List<Bound>,

    /// Trait-level attributes (marker, auto, unsafe, …).
    pub attributes: List<TraitAttribute>,
}

#[bon::bon]
impl Trait {
    #[builder]
    pub fn new(
        #[builder(default)] generics: Generics,
        #[builder(with = FromIterator::from_iter)] super_traits: List<Bound>,
        #[builder(with = FromIterator::from_iter)] attributes: List<TraitAttribute>,
    ) -> Self {
        Trait {
            generics,
            super_traits,
            attributes,
        }
    }
}

/// A concrete implementation block: an inherent `impl` or a trait `impl`.
///
/// The implemented methods, associated types, and associated constants are the
/// entry's children.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Impl {
    /// Generic parameters and `where`-clause of the impl.
    pub generics: Generics,

    /// The trait being implemented; `None` for an inherent `impl Type { … }`.
    pub trait_ref: Option<TraitRef>,

    /// The type the impl is *for* (the `Self` type).
    pub self_ty: EntryIndex<Type>,

    /// Whether this is a positive or negative impl (`impl !Send`).
    pub polarity: ImplPolarity,

    /// Impl-level attributes (unsafe, blanket, default).
    pub attributes: List<ImplAttribute>,
}

#[bon::bon]
impl Impl {
    #[builder]
    pub fn new(
        #[builder(default)] generics: Generics,
        trait_ref: Option<TraitRef>,
        self_ty: EntryIndex<Type>,
        #[builder(default = ImplPolarity::Positive)] polarity: ImplPolarity,
        #[builder(with = FromIterator::from_iter)] attributes: List<ImplAttribute>,
    ) -> Self {
        Impl {
            generics,
            trait_ref,
            self_ty,
            polarity,
            attributes,
        }
    }
}

/// An attribute applicable to a [`Trait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum TraitAttribute {
    /// A marker trait with no members (`Send`, `Sync`).
    Marker,

    /// An auto trait, implemented automatically unless opted out.
    Auto,

    /// Implementing the trait requires `unsafe`.
    Unsafe,

    /// The trait is object-safe / dyn-compatible.
    ObjectSafe,

    /// The trait is sealed (implementable only in its defining crate).
    Sealed,

    /// A single-abstract-method / functional interface (Java `@FunctionalInterface`).
    Functional,
}

/// Whether an [`Impl`] adds or removes a trait implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum ImplPolarity {
    /// A normal implementation.
    Positive,

    /// A negative impl (`impl !Trait for T`).
    Negative,
}

/// An attribute applicable to an [`Impl`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum ImplAttribute {
    /// An `unsafe impl`.
    Unsafe,

    /// A blanket impl (`impl<T> Trait for T`).
    Blanket,

    /// A specialization-default impl (`default impl`).
    Default,
}
