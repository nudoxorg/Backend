use crate::{
    List,
    kinds::{GenericParam, Type, WherePred},
    visitor::Visitor,
};

// Generics and where-clauses are now modelled via `generics:
// List<GenericParam>` and `wheres: List<WherePred>` (bounds represented as
// `List<Type>`). Remaining deferred items: associated types, associated consts,
// const-expressions, and variance.

/// Boolean modifiers for a [`Trait`] definition.
///
/// All fields default to `false`, so existing builders that do not set `flags`
/// continue to produce a plain safe non-auto non-sealed trait.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize, Default,
)]
pub struct TraitFlags {
    /// `true` for an `unsafe trait`.
    pub is_unsafe: bool,

    /// `true` for an `auto trait` (e.g. `Send`, `Sync`).
    pub is_auto: bool,

    /// `true` when the trait is sealed (not publicly implementable).
    pub sealed: bool,
}

/// A trait, interface, or protocol definition.
///
/// The trait's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol).
/// Methods and associated items appear as child entries.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Trait {
    /// Boolean modifiers for this trait (unsafe, auto, sealed).
    pub flags: TraitFlags,

    /// The explicit super-traits (bounds) of this trait, in declaration order.
    ///
    /// An empty list denotes an unconstrained trait.
    pub supers: List<Type>,

    /// Generic parameters declared on this trait, in declaration order.
    pub generics: List<GenericParam>,

    /// Where-clause predicates for this trait, in declaration order.
    pub wheres: List<WherePred>,
}

#[bon::bon]
impl Trait {
    #[builder]
    pub fn new(
        #[builder(default)] flags: TraitFlags,
        #[builder(default, with = FromIterator::from_iter)] supers: List<Type>,
        #[builder(default, with = FromIterator::from_iter)] generics: List<GenericParam>,
        #[builder(default, with = FromIterator::from_iter)] wheres: List<WherePred>,
    ) -> Self {
        Trait {
            flags,
            supers,
            generics,
            wheres,
        }
    }
}
