use crate::{
    List,
    index::EntryIndex,
    kinds::{ConstExpr, Trait, Type},
    visitor::Visitor,
};

/// A generic parameter introduced by a declaration — the `T`, `'a`, or
/// `const N: usize` in `<...>`.
///
/// Every parameter is a first-class entry so that uses elsewhere can reference
/// it by [`EntryIndex`]; the parameter's name lives on the owning entry's
/// [`Symbol`](crate::entry::Symbol).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Generic {
    /// A type parameter (`T`, `K: Hashable`, higher-kinded `F<_>`).
    Type(TypeParam),

    /// A lifetime / region parameter (`'a`).
    Lifetime(LifetimeParam),

    /// A const parameter (`const N: usize`, C++ non-type template parameter).
    Const(ConstParam),
}

/// A generic *type* parameter.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct TypeParam {
    /// Variance with respect to subtyping.
    pub variance: Variance,

    /// The type-theoretic kind (`*`, `* -> *`, …).
    pub kind: TypeKind,

    /// Bounds this parameter must satisfy (`T: Clone + 'a`).
    pub bounds: List<Bound>,

    /// The default supplied when the argument is elided (`T = i32`).
    pub default: Option<EntryIndex<Type>>,

    /// How the parameter was introduced.
    pub origin: TypeParamOrigin,
}

/// A generic *lifetime* parameter.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct LifetimeParam {
    /// Variance with respect to the types that mention it.
    pub variance: Variance,

    /// Other lifetimes this one is required to outlive (`'a: 'b`).
    pub outlives: List<EntryIndex<Generic>>,
}

/// A generic *const* parameter, monomorphised to a value.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct ConstParam {
    /// The type of the constant (`usize` in `const N: usize`).
    pub ty: EntryIndex<Type>,

    /// A compile-time default used when the argument is elided.
    pub default: Option<ConstExpr>,
}

/// How a [`TypeParam`] was introduced into scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum TypeParamOrigin {
    /// A freely-declared, named type variable (the common case).
    Free,

    /// An associated type declared inside a trait/protocol (`type Item`).
    Associated,

    /// Introduced by inference rather than declaration (TS `infer T`).
    Inferred,
}

/// The variance of a type or lifetime parameter with respect to subtyping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Variance {
    /// `F<Sub>` is a subtype of `F<Super>`.
    Covariant,

    /// `F<Super>` is a subtype of `F<Sub>`.
    Contravariant,

    /// No subtyping; the parameter must match exactly.
    Invariant,

    /// Subtyping is unrestricted in both directions.
    Bivariant,
}

/// The *kind* of a type or type constructor (`*`, `* -> *`, …).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum TypeKind {
    /// The base kind of all ordinary types (`*`).
    Type,

    /// The kind of typeclass/trait constraints (Haskell's `Constraint`).
    Constraint,

    /// The extensible row kind (PureScript).
    Row,

    /// A type constructor: maps one kind to another (`* -> *`).
    Arrow(Box<TypeKind>, Box<TypeKind>),

    /// A named kind variable, for kind-polymorphic systems.
    Var(String),
}

/// A reference to a [`Trait`], optionally parameterised.
///
/// Used for bounds, supertraits, `dyn` trait objects, and impl headers.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct TraitRef {
    /// The trait being referenced.
    pub def: EntryIndex<Trait>,

    /// Type / const / lifetime arguments supplied to the trait.
    pub args: List<GenericArg>,

    /// Higher-ranked lifetime binders (`for<'a> Trait<'a>`).
    pub for_lifetimes: List<EntryIndex<Generic>>,
}

/// A single argument in a generic-argument position (`Vec<u8>`, `array<4>`).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum GenericArg {
    /// A type argument.
    Type(EntryIndex<Type>),

    /// A const argument.
    Const(ConstExpr),

    /// A lifetime argument.
    Lifetime(EntryIndex<Generic>),

    /// An associated-item binding (`Item = u8`).
    Binding {
        name: String,
        value: EntryIndex<Type>,
    },
}

/// A bound a parameter or associated type must satisfy.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Bound {
    /// A trait/interface conformance bound (`T: Clone`).
    Trait(TraitRef),

    /// A lifetime bound (`T: 'a`).
    Lifetime(EntryIndex<Generic>),
}

/// A `where`-clause predicate that restricts how parameters may be
/// instantiated.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Constraint {
    /// A type must satisfy a bound (`T: Clone`, `T::Item: Display`).
    Bound { ty: EntryIndex<Type>, bound: Bound },

    /// An associated-type equality (`Iterator<Item = u8>`, `T::Item == u8`).
    Equality {
        lhs: EntryIndex<Type>,
        rhs: EntryIndex<Type>,
    },

    /// A lifetime outlives relation (`'a: 'b`).
    Outlives {
        shorter: EntryIndex<Generic>,
        longer: EntryIndex<Generic>,
    },

    /// A const-expression predicate (`N > 0`).
    ConstEval(ConstExpr),
}

/// A complete generic-parameter list plus its `where`-clause.
///
/// The empty `Generics` (via [`Default`]) denotes a non-generic declaration.
#[derive(Debug, Default, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Generics {
    /// The parameters introduced, in declaration order.
    pub params: List<EntryIndex<Generic>>,

    /// Additional `where`-clause constraints over those parameters.
    pub constraints: List<Constraint>,
}

#[bon::bon]
impl Generics {
    #[builder]
    pub fn new(
        #[builder(with = FromIterator::from_iter)] params: List<EntryIndex<Generic>>,
        #[builder(with = FromIterator::from_iter)] constraints: List<Constraint>,
    ) -> Self {
        Generics {
            params,
            constraints,
        }
    }
}
