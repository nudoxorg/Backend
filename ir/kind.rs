#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Visibility {
	Public,
	Private,
	Protected,
	Internal,
	Package,
}

/// Represents different kinds of code elements in a programming language or
/// API.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", content = "value"))]
pub enum Kind {
	/// A namespace, package, or module.
	Module,

	/// Struct, class, record, or data class.
	RecordType(Record),

	/// Unlinked documentation
	Info,

	/// Union type
	UnionType(Vec<Type>),

	/// Trait/protocol/interface definition
	TraitDef(TraitDef),

	/// Trait/protocol implementation
	TraitImpl(TraitImpl),

	/// Enum, algebraic data type, discriminated union.
	SumType(Vec<SumVariant>),

	/// Trait, interface, abstract base class.
	InterfaceType,

	/// Function, method, lambda (with metadata).
	Function(Function),

	/// Type alias, typedef, using alias.
	TypeAlias(Type),

	// value in compler output for now... Quick PAtch for now
	/// Constant or immutable global.
	Constant,

	/// Mutable global/static variable.
	Variable,

	/// Macro, template, codegen hook.
	Macro,

	/// Built‑in primitive type.
	PrimitiveType,

	/// Field or property of a type.
	Field,

	/// Event, signal, or callback definition.
	Event,
}
