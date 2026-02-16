use std::collections::HashMap;

#[cfg(feature = "facet")]
use facet::Facet;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{generics::Generics, kind::Visibility, parameter::Parameter};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "facet", derive(Facet))]
pub struct Function {
	pub input_parameters:  Option<Vec<Parameter>>,
	pub output_parameters: Option<Vec<Parameter>>,
	pub type_links:        Option<HashMap<String, i64>>,
	pub attributes:        Option<Vec<Attribute>>,
	pub generics:          Option<Generics>,
	pub name:              String,
	/// Is there a function body?
	pub implemented:       bool,
	pub visibility:        Option<Visibility>,
}
/// The various attributes a function can have
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "facet", derive(Facet), repr(C))]
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
