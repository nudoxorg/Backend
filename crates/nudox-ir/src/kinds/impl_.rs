use crate::{
    List,
    kinds::{GenericParam, Type, WherePred},
    visitor::Visitor,
};

// Generics and where-clauses are now modelled via `generics:
// List<GenericParam>` and `wheres: List<WherePred>` (bounds represented as
// `List<Type>`). Remaining deferred items: associated item lists,
// const-expressions, and variance.

/// Boolean modifiers for an [`Impl`] block.
///
/// Both fields default to `false`, so existing builders that do not set `flags`
/// continue to produce a normal positive non-blanket impl.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize, Default,
)]
pub struct ImplFlags {
    /// `true` for a negative impl (`impl !Trait for T`).
    pub negative: bool,

    /// `true` for a blanket impl (`impl<T> Trait for T`).
    pub blanket: bool,
}

/// A trait implementation or inherent impl block.
///
/// The impl's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol).
/// Impl methods and associated items appear as child entries.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Impl {
    /// Boolean modifiers for this impl (negative, blanket).
    pub flags: ImplFlags,

    /// The trait being implemented, if this is a trait impl.
    ///
    /// `None` denotes an inherent impl block.
    pub of: Option<Type>,

    /// The concrete type this impl is for (`Self`).
    pub self_ty: Type,

    /// Generic parameters declared on this impl block, in declaration order.
    pub generics: List<GenericParam>,

    /// Where-clause predicates for this impl block, in declaration order.
    pub wheres: List<WherePred>,
}

#[bon::bon]
impl Impl {
    #[builder]
    pub fn new(
        #[builder(default)] flags: ImplFlags,
        of: Option<Type>,
        self_ty: Type,
        #[builder(default, with = FromIterator::from_iter)] generics: List<GenericParam>,
        #[builder(default, with = FromIterator::from_iter)] wheres: List<WherePred>,
    ) -> Self {
        Impl {
            flags,
            of,
            self_ty,
            generics,
            wheres,
        }
    }
}
