use crate::{
    List,
    kinds::{GenericParam, Sealed, TriState, Type, WherePred},
    visitor::Visitor,
};

// Generics and where-clauses are now modelled via `generics:
// List<GenericParam>` and `wheres: List<WherePred>` (bounds represented as
// `List<Type>`). Remaining deferred items: associated types, associated consts,
// const-expressions, and variance.

/// Modifier flags for a [`Trait`] definition.
///
/// The `Default` impl matches the old model's default: `dyn_compat: Unknown`,
/// `sealed: None`, `is_unsafe: false`, `is_auto: false`. Builders that do not
/// set `flags` continue to produce a plain, safe, non-auto, unsealed trait.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize, Default,
)]
pub struct TraitFlags {
    /// `true` for an `unsafe trait`.
    pub is_unsafe: bool,

    /// `true` for an `auto trait` (e.g. `Send`, `Sync`).
    pub is_auto: bool,

    /// Object-safety / dyn-compatibility of this trait.
    ///
    /// `Unknown` when the oracle has not determined it; `Yes` means the trait
    /// is dyn-compatible (`dyn Trait` is valid), `No` means it is not.
    pub dyn_compat: TriState,

    /// How thoroughly this trait is sealed against external implementation.
    ///
    /// `None` means no sealing evidence was found (trait may still be open);
    /// `PubApi` means it is sealed via a public-supertrait-on-private-bound
    /// pattern; `Full` means only the defining crate can implement it.
    pub sealed: Sealed,
}

/// A trait, interface, or protocol definition.
///
/// The trait's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol).
/// Methods and associated items appear as child entries.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `TraitFlags::default()` matches the old model: `dyn_compat = Unknown`,
    /// `sealed = None` (gap 6).
    #[test]
    fn trait_flags_default() {
        let flags = TraitFlags::default();
        assert!(!flags.is_unsafe);
        assert!(!flags.is_auto);
        assert_eq!(flags.dyn_compat, TriState::Unknown);
        assert_eq!(flags.sealed, Sealed::None);
    }

    /// Builder + serde round-trip for fully-populated `TraitFlags` (gap 6).
    #[test]
    fn trait_flags_roundtrip() {
        let flags = TraitFlags {
            is_unsafe: true,
            is_auto: false,
            dyn_compat: TriState::Yes,
            sealed: Sealed::PubApi,
        };
        let t = Trait::builder().flags(flags).build();
        assert_eq!(t.flags.dyn_compat, TriState::Yes);
        assert_eq!(t.flags.sealed, Sealed::PubApi);

        let json = serde_json::to_string(&t).expect("serialize failed");
        let rt: Trait = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(t, rt);
    }

    /// `Sealed::Full` round-trips cleanly.
    #[test]
    fn sealed_full_roundtrip() {
        let flags = TraitFlags {
            sealed: Sealed::Full,
            ..TraitFlags::default()
        };
        let json = serde_json::to_string(&flags).unwrap();
        assert_eq!(flags, serde_json::from_str::<TraitFlags>(&json).unwrap());
    }
}
