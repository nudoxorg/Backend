#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
	parameter::{ConstParam, LifetimeParam, TypeParam},
	ty::Type,
};

// MARK: - Generics

/// A universal representation of a generic parameter list across languages.
///
/// `Generics` bundles all four flavors of compile-time parameters — type,
/// constant, and lifetime variables, plus the constraints that govern them —
/// into a single, language-agnostic structure.  It mirrors closely what an
/// angle-bracket list encodes in languages like Rust, C++, Swift, and
/// TypeScript.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Generics {
	/// The type variables introduced (e.g., `T`, `K: Hashable`).
	pub type_params: Vec<TypeParam>,

	/// The constant variables introduced (e.g., `const N: usize`).
	pub const_params: Vec<ConstParam>,

	/// The lifetime / region variables introduced (e.g., `'a`).
	pub lifetime_params: Vec<LifetimeParam>,

	/// Additional constraints relating the parameters above.
	pub constraints: Vec<Constraint>,
}

// MARK: - Constraints

/// A predicate that restricts how the generic parameters of a declaration may
/// be instantiated.
///
/// Constraints are collected separately from the parameters themselves so that
/// complex where-clauses can be represented without bloating each individual
/// parameter definition.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Constraint {
	/// A trait / protocol conformance bound (e.g., `T: Clone`).
	TraitBound { param: String, trait_ref: TraitRef },

	/// A bound on an associated type (e.g., `T::Item: Display`).
	AssociatedTypeBound { param: String, assoc_name: String, bound: TypeExpr },

	/// A higher-kinded bound constraining the *kind* of a type constructor.
	HigherKindedBound { param: String, kind: KindExpr },

	/// An associated item equality constraint (e.g., `Iterator<Item = u8>`).
	AssociatedItem { name: String, args: Option<Vec<GenericArg>>, term: Term },

	/// A lifetime outlives relation (e.g., `'a: 'b` — `'a` outlives `'b`).
	LifetimeBound { shorter: String, longer: String },

	/// A constant-expression bound (e.g., `N > 0`).
	ConstExprBound { param: String, expr: ConstExpr },

	/// An arbitrary logical predicate over the parameters (e.g., `T: Clone && U: Copy`).
	LogicalPredicate { expr: PredicateExpr },
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

// MARK: - Supporting Types

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

/// A compile-time constant expression, used in default values and bounds.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ConstExpr {
	/// Source-level text of the expression (e.g., `"1 + 1"`, `"true"`).
	pub expr: String,
}

/// The *kind* of a type constructor, expressed as a signature string.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct KindExpr {
	/// Kind signature in arrow notation (e.g., `"* -> *"`, `"* -> * -> *"`).
	pub signature: String,
}

/// A logical predicate over generic parameters.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PredicateExpr {
	/// Textual logical expression (e.g., `"T: Clone && U: Copy"`).
	pub expr: String,
}

/// A single argument supplied to a generic parameter position.
///
/// Generic arguments appear at call or instantiation sites
/// (e.g., `Vec<u8>`, `array<4>`, `&'a str`).
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
