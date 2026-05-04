use std::collections::HashSet;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{entry::NudoxPath, function::Function, module::Module, protocols::{TraitDef, TraitImpl}, record::{Record, SumVariant}, ty::Type};

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

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Symbol<T> {
	pub name:          String,
	pub path:          NudoxPath,
	pub aliases:       Option<HashSet<Vec<String>>>,
	pub visibility:    Visibility,
	pub documentation: Option<String>,
	pub inner:         T,
}

impl<T> Symbol<T> {
	pub fn placeholder(inner: T) -> Self {
		Self {
			name: String::new(),
			path: NudoxPath::Local(std::path::PathBuf::new()),
			aliases: None,
			visibility: Visibility::Public,
			documentation: None,
			inner,
		}
	}

	pub fn clone_with<U>(&self, inner: U) -> Symbol<U> {
		Symbol {
			name: self.name.clone(),
			path: self.path.clone(),
			aliases: self.aliases.clone(),
			visibility: self.visibility.clone(),
			documentation: self.documentation.clone(),
			inner,
		}
	}
}

/// The syntactic / semantic kind of a documented API entry.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", content = "value"))]
pub enum Entry {
	/// A namespace, package, or module — a container for other entries.
	Module(Symbol<Module>),

	/// A product type: struct, class, record, or data class.
	RecordType(Symbol<Record>),

	/// Unlinked, free-form documentation (prose articles, guides, etc.).
	Info(Symbol<String>),

	/// An anonymous or tagged union of concrete types.
	UnionType(Symbol<Vec<Type>>),

	/// A trait, protocol, or interface definition.
	TraitDef(Symbol<TraitDef>),

	/// A concrete implementation of a trait or protocol for a specific type.
	TraitImpl(Symbol<TraitImpl>),

	/// An algebraic sum type: enum, discriminated union, or sealed class.
	SumType(Symbol<Vec<SumVariant>>),

	/// A function, method, or lambda with its full signature.
	Function(Symbol<Function>),

	/// A type alias, typedef, or `using` alias.
	TypeAlias(Symbol<Type>),

	/// A named constant or immutable global binding.
	Constant(Symbol<()>),

	/// A mutable global or static variable.
	Variable(Symbol<()>),

	/// A macro, template, or code-generation hook.
	Macro(Symbol<()>),

	/// A built-in primitive type (integer, float, bool, …).
	PrimitiveType(Symbol<()>),

	/// A field or property of a containing type.
	Field(Symbol<()>),

	/// An event, signal, or callback definition.
	Event(Symbol<()>),
}

impl Entry {
	pub fn path(&self) -> &NudoxPath {
		match self {
			Entry::Module(s) => &s.path,
			Entry::RecordType(s) => &s.path,
			Entry::Info(s) => &s.path,
			Entry::UnionType(s) => &s.path,
			Entry::TraitDef(s) => &s.path,
			Entry::TraitImpl(s) => &s.path,
			Entry::SumType(s) => &s.path,
			Entry::Function(s) => &s.path,
			Entry::TypeAlias(s) => &s.path,
			Entry::Constant(s) => &s.path,
			Entry::Variable(s) => &s.path,
			Entry::Macro(s) => &s.path,
			Entry::PrimitiveType(s) => &s.path,
			Entry::Field(s) => &s.path,
			Entry::Event(s) => &s.path,
		}
	}

	pub fn name(&self) -> &str {
		match self {
			Entry::Module(s) => &s.name,
			Entry::RecordType(s) => &s.name,
			Entry::Info(s) => &s.name,
			Entry::UnionType(s) => &s.name,
			Entry::TraitDef(s) => &s.name,
			Entry::TraitImpl(s) => &s.name,
			Entry::SumType(s) => &s.name,
			Entry::Function(s) => &s.name,
			Entry::TypeAlias(s) => &s.name,
			Entry::Constant(s) => &s.name,
			Entry::Variable(s) => &s.name,
			Entry::Macro(s) => &s.name,
			Entry::PrimitiveType(s) => &s.name,
			Entry::Field(s) => &s.name,
			Entry::Event(s) => &s.name,
		}
	}

	pub fn kind_tag(&self) -> &'static str {
		match self {
			Entry::Module(_) => "module",
			Entry::Info(_) => "info",
			Entry::Constant(_) => "constant",
			Entry::Variable(_) => "variable",
			Entry::Macro(_) => "macro",
			Entry::PrimitiveType(_) => "primitive_type",
			Entry::Event(_) => "event",
			Entry::Field(_) => "field",
			Entry::RecordType(_) => "record",
			Entry::UnionType(_) => "union",
			Entry::TraitDef(_) => "trait_def",
			Entry::TraitImpl(_) => "trait_impl",
			Entry::SumType(_) => "sum_type",
			Entry::TypeAlias(_) => "type_alias",
			Entry::Function(_) => "function",
		}
	}

	pub fn schema_class(&self) -> &'static str {
		match self {
			Entry::Module(_) => "Module",
			Entry::RecordType(_) => "RecordType",
			Entry::Info(_) => "Info",
			Entry::UnionType(_) => "UnionType",
			Entry::TraitDef(_) => "TraitDef",
			Entry::TraitImpl(_) => "TraitImpl",
			Entry::SumType(_) => "SumType",
			Entry::Function(_) => "Function",
			Entry::TypeAlias(_) => "TypeAlias",
			Entry::Constant(_) => "Constant",
			Entry::Variable(_) => "Variable",
			Entry::Macro(_) => "Macro",
			Entry::PrimitiveType(_) => "PrimitiveType",
			Entry::Field(_) => "Field",
			Entry::Event(_) => "Event",
		}
	}
}

impl std::fmt::Display for Entry {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.kind_tag()) }
}
