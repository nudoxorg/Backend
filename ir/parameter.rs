#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
	generics::{ConstExpr, TypeExpr, Variance},
	ty::Type,
};

/// Represents a complete parameter — whether value-level (a typed argument),
/// or generic (a type, constant, or lifetime parameter).
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
}

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

/// A generic type parameter introduced in an angle-bracket list
/// (e.g., `T`, `K: Hashable`, `Output = i32`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeParam {
	/// The name of the type variable (e.g., `T`, `Element`).
	pub name: String,

	/// Classifies the parameter as a plain type, a higher-kinded type,
	/// or an associated-type slot.
	pub kind: TypeKind,

	/// The variance of the parameter with respect to subtyping.
	pub variance: Variance,

	/// The fallback type used when this parameter is not inferred or supplied.
	pub default_type: Option<TypeExpr>,
}

/// Classifies how a type parameter is used within a generic signature.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TypeKind {
	/// An ordinary unconstrained type slot (e.g., `T`).
	Type,

	/// A type constructor that itself accepts type arguments (e.g., `F<_>`).
	HigherKinded,

	/// An associated type declared inside a trait or protocol.
	Associated,
}

/// A generic *constant* parameter whose value is supplied at monomorphisation
/// time (e.g., Rust `const N: usize`, C++ NTTP).
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
