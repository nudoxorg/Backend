use crate::{List, kinds::Type, visitor::Visitor};

/// A single generic parameter on a declaration.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum GenericParam {
    /// A lifetime parameter, e.g. `'a`.
    Lifetime { name: String },

    /// A type parameter with zero or more bounds and an optional default.
    ///
    /// Bounds are represented as `List<Type>` — each bound is a reference to a
    /// trait modelled as a `Type`. This is a deliberate simplification: a full
    /// trait-bound would carry generic arguments and associated-type bindings,
    /// which are deferred to the resolution/IR-unification plane.
    Type {
        name: String,
        bounds: List<crate::kinds::Type>,
        default: Option<crate::kinds::Type>,
    },

    /// A const generic parameter, e.g. `const N: usize`.
    ///
    /// The value expression for const generics is deferred (no const-expression
    /// subsystem yet); only the parameter name and its type are stored.
    Const {
        name: String,
        ty: crate::kinds::Type,
    },
}

/// A single `where`-clause predicate: `target: bound + bound + ...`.
///
/// As with [`GenericParam`], bounds are modelled as `List<Type>` — a
/// deliberate simplification pending the resolution/IR-unification plane.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct WherePred {
    /// The type or lifetime being constrained.
    pub target: Type,
    /// The bounds imposed on `target`.
    pub bounds: List<Type>,
}
