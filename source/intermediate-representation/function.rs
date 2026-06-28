use rustc_hash::FxHashMap as HashMap;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{entry::NudoxPath, generics::Generics, parameter::Parameter, protocols::ReceiverKind};

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Function {
	pub input_parameters:  Option<Vec<Parameter>>,
	pub output_parameters: Option<Vec<Parameter>>,

	pub type_links:  Option<HashMap<String, i64>>,
	pub attributes:  Option<Vec<Attribute>>,
	pub generics:    Option<Generics>,
	pub receiver:    Option<ReceiverKind>,
	pub overloads:   Option<Vec<Function>>,
	pub implemented: bool,

	/// Child entries conceptually scoped inside this function.
	///
	/// In many dynamic languages (e.g., JavaScript/TypeScript), functions are
	/// first-class objects that can act as namespaces containing static
	/// properties, nested classes, or secondary exported functions.
	pub members: Option<Vec<NudoxPath>>,

	/// Protocols or traits implemented explicitly by this function object.
	///
	/// While rare in systems languages, callable objects or first-class functions
	/// may dynamically implement interfaces at runtime.
	pub implemented_protocols: Option<Vec<NudoxPath>>,

	/// The tree-sitter-parsed function body, yoked to its source text
	/// with pre-resolved IR references.
	///
	/// Carries the raw source, the parsed syntax tree, and a list of
	/// [`ResolvedReference`]s that link identifiers in the body to
	/// [`NudoxPath`] entries in the IR index.
	///
	/// Resolution happens in the compiler after parsing; see
	/// [`crate::syntax::FunctionBody`] for construction.
	#[cfg_attr(feature = "serde", serde(skip))]
	pub body: Option<crate::syntax::ParsedBody>,
}

impl PartialEq for Function {
	fn eq(&self, other: &Self) -> bool {
		self.input_parameters == other.input_parameters
			&& self.output_parameters == other.output_parameters
			&& self.type_links == other.type_links
			&& self.attributes == other.attributes
			&& self.generics == other.generics
			&& self.receiver == other.receiver
			&& self.overloads == other.overloads
			&& self.implemented == other.implemented
			&& self.members == other.members
			&& self.implemented_protocols == other.implemented_protocols
			&& match (&self.body, &other.body) {
				(Some(a), Some(b)) => a.get() == b.get(),
				(None, None) => true,
				_ => false,
			}
	}
}

impl Eq for Function {}

/// The various attributes a function can have
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Attribute {
	/// Takes an arbitrary amount of arguments
	Variadic,

	/// Can suspend and resume execution between yields.
	Generator,

	/// Determined at compile-time
	Const,

	/// No side-effects
	Pure,

	/// Runs asynchronously
	Async,

	/// For Rust, happens within an unsafe context
	Unsafe,
}
