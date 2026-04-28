use std::collections::HashMap;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{entry::NudoxPath, generics::Generics, parameter::Parameter};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Function {
	pub input_parameters: Option<Vec<Parameter>>,
	pub output_parameters: Option<Vec<Parameter>>,

	pub type_links: Option<HashMap<String, i64>>,
	pub attributes: Option<Vec<Attribute>>,
	pub generics: Option<Generics>,
	pub name: String,

	/// Function-level documentation comments.
	pub documentation: Option<String>,

	/// Child entries conceptually scoped inside this function.
	///
	/// In many dynamic languages (e.g., JavaScript/TypeScript), functions are
	/// first-class objects that can act as namespaces containing static properties,
	/// nested classes, or secondary exported functions.
	pub members: Option<Vec<NudoxPath>>,

	/// Protocols or traits implemented explicitly by this function object.
	///
	/// While rare in systems languages, callable objects or first-class functions
	/// may dynamically implement interfaces at runtime.
	pub implemented_protocols: Option<Vec<NudoxPath>>,
}

/// The various attributes a function can have
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Attribute {
	/// Takes an arbitrary amount of arguments
	Variadic,

	/// Determined at compile-time
	Const,

	/// No side-effects
	Pure,

	/// Runs asynchronously
	Async,

	/// For Rust, happens within an unsafe context
	Unsafe,
}
