#[cfg(feature = "facet")]
use facet::Facet;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "facet", derive(Facet), repr(C))]
pub enum Visibility {
	#[cfg_attr(feature = "serde", serde(rename = "public"))]
	Public,
	#[cfg_attr(feature = "serde", serde(rename = "private"))]
	Private,
	#[cfg_attr(feature = "serde", serde(rename = "protected"))]
	Protected,
	#[cfg_attr(feature = "serde", serde(rename = "internal"))]
	Internal,
	#[cfg_attr(feature = "serde", serde(rename = "package"))]
	Package,
}

/// Represents different kinds of code elements in a programming language or
/// API.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", content = "value"))]
#[cfg_attr(feature = "facet", derive(Facet), repr(C))]
pub enum Kind {
	/// A namespace, package, or module.
	Module,

	/// Struct, class, record, or data class.
	#[cfg_attr(feature = "serde", serde(rename = "recordType"))]
	RecordType(Record),

	/// Unlinked documentation
	Info,

	/// Union type
	#[cfg_attr(feature = "serde", serde(rename = "unionType"))]
	UnionType(Vec<Type>),

	/// Trait/protocol/interface definition
	#[cfg_attr(feature = "serde", serde(rename = "traitDef"))]
	TraitDef(TraitDef),

	/// Trait/protocol implementation
	#[cfg_attr(feature = "serde", serde(rename = "traitImpl"))]
	TraitImpl(TraitImpl),

	/// Enum, algebraic data type, discriminated union.
	#[cfg_attr(feature = "serde", serde(rename = "sumType"))]
	SumType(Vec<SumVariant>),

	/// Trait, interface, abstract base class.
	#[cfg_attr(feature = "serde", serde(rename = "interfaceType"))]
	InterfaceType,

	/// Function, method, lambda (with metadata).
	Function(Function),

	/// Type alias, typedef, using alias.
	#[cfg_attr(feature = "serde", serde(rename = "typeAlias"))]
	TypeAlias(Type),

	// value in compler output for now... Quick PAtch for now
	/// Constant or immutable global.
	Constant,

	/// Mutable global/static variable.
	Variable,

	/// Macro, template, codegen hook.
	Macro,

	/// Built‑in primitive type.
	#[cfg_attr(feature = "serde", serde(rename = "primitiveType"))]
	PrimitiveType,

	/// Field or property of a type.
	Field,

	/// Event, signal, or callback definition.
	Event,
}

// Implementing `Display` for `Kind` to replace the Swift `description` computed
// property.
use std::fmt;

use crate::{function::Function, protocols::{TraitDef, TraitImpl}, record::{Record, SumVariant}, ty::Type};

impl fmt::Display for Kind {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let description = match self {
			// A namespace, package, or module.
			Kind::Module => "Module",

			// Struct, class, record, or data class.
			Kind::RecordType(_) => "RecordType",

			// A piece of unlinked documentation
			Kind::Info => "Info",

			// Union type (C, C++, Rust, etc.).
			Kind::UnionType(_) => "UnionType",

			// Trait def
			Kind::TraitDef(_) => "TraitDef",

			// Trait impl
			Kind::TraitImpl(_) => "TraitImpl",

			// Enum, algebraic data type, discriminated union.
			Kind::SumType(_) => "SumType",

			// Trait, interface, abstract base class.
			Kind::InterfaceType => "InterfaceType",

			// Function, method, lambda (with metadata).
			Kind::Function(_) => "Function",

			// Type alias, typedef, using alias.
			Kind::TypeAlias(_) => "TypeAlias",

			// Constant or immutable global.
			Kind::Constant => "Constant",

			// Mutable global/static variable.
			Kind::Variable => "Variable",

			// Macro, template, codegen hook.
			Kind::Macro => "Macro",

			// Built-in primitive type.
			Kind::PrimitiveType => "PrimitiveType",

			// Field or property of a type.
			Kind::Field => "Field",

			// Event, signal, or callback definition.
			Kind::Event => "Event",
		};
		write!(f, "{}", description)
	}
}
