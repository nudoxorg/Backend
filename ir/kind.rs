#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
	function::Function,
	protocols::{TraitDef, TraitImpl},
	record::{Record, SumVariant},
	ty::Type,
};

/// The visibility of an entry in the source language.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Visibility {
	Public,
	Private,
	Protected,
	/// Module-internal (e.g., Rust `pub(crate)`, C# `internal`).
	Internal,
	/// Package-scoped (e.g., Java package-private).
	Package,
}

/// The syntactic / semantic kind of a documented API entry.
///
/// Each variant carries its data inline, so no separate lookup is required
/// to understand what shape the entry has.  Variants that carry no structured
/// data are leaf entries whose content is fully captured by their `Entry`
/// fields.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", content = "value"))]
pub enum EntryKind {
	/// A namespace, package, or module — a container for other entries.
	Module,

	/// A product type: struct, class, record, or data class.
	RecordType(Record),

	/// Unlinked, free-form documentation (prose articles, guides, etc.).
	Info,

	/// An anonymous or tagged union of concrete types.
	UnionType(Vec<Type>),

	/// A trait, protocol, or interface definition.
	TraitDef(TraitDef),

	/// A concrete implementation of a trait or protocol for a specific type.
	TraitImpl(TraitImpl),

	/// An algebraic sum type: enum, discriminated union, or sealed class.
	SumType(Vec<SumVariant>),

	/// A function, method, or lambda with its full signature.
	Function(Function),

	/// A type alias, typedef, or `using` alias.
	TypeAlias(Type),

	/// A named constant or immutable global binding.
	Constant,

	/// A mutable global or static variable.
	Variable,

	/// A macro, template, or code-generation hook.
	Macro,

	/// A built-in primitive type (integer, float, bool, …).
	PrimitiveType,

	/// A field or property of a containing type.
	Field,

	/// An event, signal, or callback definition.
	Event,
}
