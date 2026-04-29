#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{parameter::Parameter, ty::Type};

/// A universal representation of a generic parameter list across languages.
///
/// `Generics` bundles every kind of compile-time parameter — type, constant,
/// lifetime, dependent, and module variables — together with the constraints
/// that govern them into a single, language-agnostic structure.  It mirrors
/// closely what an angle-bracket or parenthetical list encodes in languages
/// like Rust, C++, Swift, TypeScript, Scala, OCaml, Agda, and Haskell.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Generics {
	/// The parameters this generic scope introduces, in declaration order.
	pub params: Vec<Parameter>,

	/// Additional constraints that must hold over the parameters above.
	pub constraints: Vec<Constraint>,
}

/// A predicate that restricts how the generic parameters of a declaration may
/// be instantiated.
///
/// Constraints are collected separately from the parameters themselves so that
/// complex where-clauses can be represented without bloating each individual
/// parameter definition.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Constraint {
	/// A trait or protocol conformance bound (e.g., `T: Clone`).
	TraitBound { param: String, trait_ref: TraitRef },

	/// A bound on an associated type (e.g., `T::Item: Display`).
	AssociatedTypeBound { param: String, assoc_name: String, bound: TypeExpr },

	/// A higher-kinded bound constraining the *kind* of a type constructor
	/// (e.g., `F: * -> *`).
	HigherKindedBound { param: String, kind: Kind },

	/// An associated-item equality constraint (e.g., `Iterator<Item = u8>`).
	AssociatedItem { name: String, args: Option<Vec<GenericArg>>, term: Term },

	/// A lifetime outlives relation (e.g., `'a: 'b` — `'a` outlives `'b`).
	LifetimeBound { shorter: String, longer: String },

	/// A constant-expression bound (e.g., `N > 0`).
	ConstExprBound { param: String, expr: ConstExpr },

	/// A compound logical predicate over the parameters.
	/// Used for conditions that span multiple parameters or require boolean
	/// connectives (e.g., `T: Clone && U: Copy`).
	LogicalPredicate { pred: Predicate },

	/// A Haskell-style functional dependency: the `sources` parameters
	/// uniquely determine the `determined` parameters within a typeclass
	/// or multi-parameter typeclass declaration (e.g., `class C f e | f -> e`).
	FunctionalDependency { sources: Vec<String>, determined: Vec<String> },

	/// A Scala / Haskell implicit-evidence bound: an implicit or given
	/// instance of the trait must be available in scope at the call site, but
	/// is threaded through automatically rather than named explicitly.
	/// (e.g., `[T: Ordering]` in Scala 3, `(implicit ev: Ordering[T])` in Scala
	/// 2).
	ImplicitBound { param: String, trait_ref: TraitRef },
}

/// The right-hand side of an associated-type equality constraint.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Term {
	/// An exact type equality (e.g., `Output = i32`).
	Equality(Box<Type>),

	/// A set of sub-constraints the term must satisfy.
	Bound(Vec<Constraint>),
}

/// The *kind* of a type or type constructor, expressed as a structured tree.
///
/// Kinds are the types of types.  An ordinary type like `i32` has kind `*`
/// (represented by [`Kind::Type`]).  A type constructor like `Vec` has kind
/// `* -> *` (represented by `Kind::Arrow(box Type, box Type)`).  Richer kind
/// systems found in Haskell, Agda, and PureScript introduce additional base
/// kinds and kind variables.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Kind {
	/// The base kind of all ordinary types (`*` or `Type`).
	Type,

	/// The kind of typeclass or trait constraints (Haskell's `Constraint` kind).
	Constraint,

	/// The extensible row kind used in PureScript and similar systems.
	Row,

	/// A type constructor: maps one kind to another (e.g., `* -> *`).
	Arrow(Box<Kind>, Box<Kind>),

	/// A named kind variable, used in kind-polymorphic systems.
	Var(String),
}

/// A compile-time constant expression that can appear in generic bounds,
/// array lengths, default values, and dependent-type annotations.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ConstExpr {
	/// An integer literal (e.g., `4`, `-1`).
	Int(i64),

	/// A floating-point literal (e.g., `3.14`).
	Float(f64),

	/// A boolean literal (`true`, `false`).
	Bool(bool),

	/// A string literal (e.g., `"hello"`).
	Str(String),

	/// A reference to a named binding or constant (e.g., `MAX`, `N`).
	Var(String),

	/// A binary operation (e.g., `N + 1`, `SIZE * 2`).
	BinOp { op: BinOp, lhs: Box<ConstExpr>, rhs: Box<ConstExpr> },

	/// A unary operation (e.g., `-N`, `!flag`).
	UnaryOp { op: UnaryOp, operand: Box<ConstExpr> },

	/// A function or constructor call (e.g., `size_of::<T>()`, `min(A, B)`).
	Call { func: String, args: Vec<ConstExpr> },

	/// A type ascription, used in dependent-type contexts to annotate a
	/// constant with its type (e.g., `(expr : Ty)` in Agda / Idris).
	Ascription { expr: Box<ConstExpr>, ty: Box<TypeExpr> },
}

/// Binary operators that may appear inside a [`ConstExpr`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum BinOp {
	Add,
	Sub,
	Mul,
	Div,
	Rem,
	BitAnd,
	BitOr,
	BitXor,
	Shl,
	Shr,
	Eq,
	Ne,
	Lt,
	Le,
	Gt,
	Ge,
	And,
	Or,
}

/// Unary operators that may appear inside a [`ConstExpr`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum UnaryOp {
	/// Arithmetic negation (`-x`).
	Neg,

	/// Logical or bitwise negation (`!x`).
	Not,

	/// Borrow / address-of (`&x`).
	Ref,

	/// Dereference (`*x`).
	Deref,
}

/// A structured boolean predicate over generic parameters.
///
/// Predicates replace the old flat `PredicateExpr` string and allow
/// compound conditions (conjunctions, disjunctions, negations) over
/// individual [`Constraint`]s to be expressed and inspected structurally.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Predicate {
	/// A single atomic constraint.
	Atom(Box<Constraint>),

	/// All inner predicates must hold simultaneously.
	And(Vec<Predicate>),

	/// At least one inner predicate must hold.
	Or(Vec<Predicate>),

	/// The inner predicate must not hold.
	Not(Box<Predicate>),
}

/// A reference to a trait or protocol, optionally parameterised.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitRef {
	/// The unqualified name of the trait (e.g., `"Clone"`, `"Iterator"`).
	pub name: String,

	/// Type arguments supplied to the trait (e.g., `<Item = u8>`).
	pub args: Vec<TypeExpr>,
}

/// A monomorphic or generic type expression used inside constraints and
/// parameter defaults (e.g., `Vec<T>`, `Option<u8>`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeExpr {
	/// The base name of the type (e.g., `"Vec"`, `"Option"`).
	pub name: String,

	/// Recursive type arguments (e.g., `[TypeExpr { name: "u8", args: [] }]`).
	pub args: Vec<TypeExpr>,
}

/// A single argument supplied to a generic parameter position.
///
/// Generic arguments appear at call or instantiation sites
/// (e.g., `Vec<u8>`, `array<4>`, `&'a str`, `Functor(Map)`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum GenericArg {
	/// A type supplied in a type-parameter position.
	#[cfg_attr(feature = "serde", serde(rename = "type"))]
	Type(Type),

	/// A constant value supplied in a const-parameter position.
	#[cfg_attr(feature = "serde", serde(rename = "constExpr"))]
	ConstExpr(ConstExpr),

	/// A lifetime supplied in a lifetime-parameter position (e.g., `'a`).
	Lifetime(String),

	/// An inline constraint supplied as an argument (rare, but expressible).
	Constraint(Constraint),

	/// A module reference supplied to a functor parameter (OCaml, ML-family).
	/// The string holds the module path (e.g., `"Map.Make"`).
	Module(String),
}

/// The variance of a type or lifetime parameter with respect to subtyping.
///
/// Variance determines how a change in a parameter's type flows through to a
/// containing type.  Most languages only surface covariance and invariance
/// directly; the remaining variants are included for completeness.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Variance {
	/// The parameter can be widened — `F<Sub>` is a subtype of `F<Super>`.
	Covariant,

	/// The parameter can be narrowed — `F<Super>` is a subtype of `F<Sub>`.
	Contravariant,

	/// No subtyping relationship; the parameter must match exactly.
	Invariant,

	/// Subtyping is unrestricted in both directions (uncommon).
	Bivariant,
}
