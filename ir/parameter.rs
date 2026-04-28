#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{generics::{ConstExpr, Kind, TypeExpr, Variance}, ty::Type};

// MARK: - Parameter

/// Represents a complete parameter — whether value-level (a typed argument)
/// or generic (type, constant, lifetime, dependent, or module).
///
/// This unified enum lets callers iterate over *all* parameter kinds in a
/// single collection without losing the type information that distinguishes
/// them.  The ordering of variants follows a rough progression from the most
/// concrete (value-level literals) to the most abstract (module-level).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Parameter {
	/// A value-level argument passed to a function or method at call-sites.
	Literal(LiteralParameter),

	/// A generic type parameter (e.g., `T`, `K extends Hashable`).
	Type(TypeParam),

	/// A generic constant parameter (e.g., Rust's `const N: usize`).
	Const(ConstParam),

	/// A lifetime / region parameter (e.g., Rust's `'a`).
	Lifetime(LifetimeParam),

	/// A dependently-typed parameter: a term value whose *type* may reference
	/// earlier parameters by name (e.g., Agda/Idris/Lean `(n : Nat)`).
	Dependent(DependentParam),

	/// A module-level parameter for ML-family functors
	/// (e.g., OCaml `(M : Map.OrderedType)`).
	Module(ModuleParam),
}

// MARK: - Literal Parameters

/// A concrete, value-level parameter in a function or method signature.
///
/// This is the most common kind of parameter across all languages —
/// a named slot that accepts a value at call-sites, optionally annotated
/// with a type, a default, and calling-convention attributes.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LiteralParameter {
	/// The binding name of the parameter (e.g., `count`, `self`).
	pub name: String,

	/// The declared type of the parameter, if present.
	/// Dynamically-typed languages may omit this.
	pub r#type: Option<Type>,

	/// Calling-convention and modifier attributes (e.g., `inout`, `consuming`).
	pub attributes: Option<Vec<ParameterAttribute>>,

	/// The default value used when the caller omits this argument.
	pub default_value: Option<ConstExpr>,

	/// Human-readable description sourced from doc-comments or schemas.
	pub description: Option<String>,
}

/// Calling-convention and modifier attributes on a [`LiteralParameter`].
///
/// These flags capture language-level modifiers that affect how a value
/// is passed into or out of a function, and cannot be inferred from the
/// type alone.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ParameterAttribute {
	/// The parameter is passed by mutable reference (Swift `inout`, C++ `&`).
	Inout,

	/// The binding is locally re-assignable but not passed by reference.
	Mutable,

	/// Ownership is transferred to the callee (Swift `consuming`).
	Consuming,

	/// A shared borrow — the callee reads without taking ownership.
	Borrowing,

	/// Isolated to a particular actor or concurrency domain.
	Isolated,

	/// Accepts zero or more trailing arguments of the same type.
	Variadic,

	/// The argument may be omitted entirely at call-sites.
	Optional,
}

// MARK: - Type Parameters

/// A generic type parameter introduced in an angle-bracket or similar list
/// (e.g., `T`, `K: Hashable`, `Output = i32`, `template<typename> class F`).
///
/// Three orthogonal fields capture the full surface of type parameters across
/// languages:
/// - `kind`   — the type-theoretic kind of the variable (is it `*`, `* -> *`…?)
/// - `origin` — where/how the parameter was introduced in the source
/// - `params` — for higher-kinded / template-template params, the inner
///   parameter list the type constructor itself accepts
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeParam {
	/// The binding name of the type variable (e.g., `T`, `Element`).
	///
	/// `None` for *anonymous* parameters — Rust's `impl Trait` in argument
	/// position introduces an unnamed type variable that should be represented
	/// this way.
	pub name: Option<String>,

	/// The type-theoretic kind of this variable.
	///
	/// An ordinary type variable has kind [`Kind::Type`] (`*`).  A type
	/// constructor like `Vec` or `F<_>` has kind `Arrow(Type, Type)` (`* -> *`).
	pub kind: Kind,

	/// The variance of the parameter with respect to subtyping.
	pub variance: Variance,

	/// The fallback type used when this parameter is not inferred or supplied.
	pub default_type: Option<TypeExpr>,

	/// For parameters that are themselves type constructors accepting
	/// generic arguments — C++'s template-template parameters
	/// (`template<typename T> class Container`) and Scala's higher-kinded
	/// type parameters (`F[_]`) — the inner parameter list that the type
	/// constructor itself accepts.
	pub params: Option<Vec<Parameter>>,

	/// The origin of this parameter within its declaration context.
	pub origin: TypeParamOrigin,
}

/// Records the *origin* of a type parameter — how and where it was declared,
/// separately from its type-theoretic kind.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TypeParamOrigin {
	/// A freely declared, named type variable (the common case).
	Free,

	/// An associated type declared inside a trait or protocol
	/// (e.g., `type Item` in a Rust trait, `associatedtype Element` in Swift).
	Associated,

	/// Introduced by pattern-matching type inference rather than an explicit
	/// declaration — e.g., TypeScript's `infer T` inside a conditional type.
	Inferred,
}

// MARK: - Const Parameters

/// A generic *constant* parameter whose value is supplied at monomorphisation
/// time (e.g., Rust `const N: usize`, C++ non-type template parameter).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ConstParam {
	/// The name of the constant variable (e.g., `N`, `SIZE`).
	pub name: String,

	/// The type of the constant (must be a primitive-like, non-generic type).
	pub r#type: TypeExpr,

	/// A compile-time default value used when the argument is elided.
	pub default_value: Option<ConstExpr>,
}

// MARK: - Lifetime Parameters

/// A lifetime / region parameter that scopes the validity of borrows
/// (e.g., Rust's `'a`, `'static`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LifetimeParam {
	/// The name of the lifetime (e.g., `"'a"`, `"'static"`).
	pub name: String,

	/// The variance of this lifetime with respect to the types that mention it.
	pub variance: Variance,
}

// MARK: - Dependent Parameters

/// A dependently-typed parameter: a value-level term whose *type* may
/// reference the names of earlier parameters.
///
/// Found in Agda, Idris, Lean (4), Coq, and any language with dependent
/// types.  In these systems the type/value boundary collapses: both
/// `(n : Nat)` and `(v : Vec A n)` are represented here, since the latter's
/// type mentions the earlier binding `n`.
///
/// Unlike [`ConstParam`], there is no restriction on which types are permitted
/// — any well-formed type expression (including ones that mention earlier
/// parameters) is valid.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DependentParam {
	/// The binding name introduced for this parameter.
	pub name: String,

	/// The type of this parameter, which may reference earlier parameters
	/// by name (e.g., `Vec A n` where `A` and `n` are earlier params).
	pub r#type: TypeExpr,

	/// A compile-time default term, if the parameter may be elided.
	pub default_value: Option<ConstExpr>,

	/// Whether this parameter is *implicit* — inferred by the type-checker
	/// without being written at call-sites.
	///
	/// In Agda/Idris, curly braces `{n : Nat}` mark implicit parameters,
	/// while parentheses `(n : Nat)` mark explicit ones.
	pub implicit: bool,
}

// MARK: - Module Parameters

/// A module-level parameter used in ML-family functors.
///
/// OCaml and Standard ML functors accept *module* arguments rather than type
/// or value arguments.  Each module argument carries a local name and an
/// optional module-type signature that the supplied module must satisfy.
///
/// Example — `module Make (Ord : Map.OrderedType) = ...` produces a
/// `ModuleParam { name: "Ord", signature: Some(TypeExpr { name:
/// "Map.OrderedType", .. }) }`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ModuleParam {
	/// The local name given to the module argument inside the functor body.
	pub name: String,

	/// The module type / signature that the supplied module must satisfy.
	/// `None` implies no explicit signature constraint (unconstrained module).
	pub signature: Option<TypeExpr>,
}
