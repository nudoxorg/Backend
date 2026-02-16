#[cfg(feature = "facet")]
use facet::Facet;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{generics::ConstExpr, ty::Type};

/// Represents a parameter in a function or method.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "facet", derive(Facet))]
pub struct Parameter {
	pub name:          String,
	pub ty:            Option<Type>, // Renamed 'type' to 'ty'
	pub attributes:    Option<Vec<ParameterAttribute>>,
	pub default_value: Option<ConstExpr>,
	pub description:   Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "facet", derive(Facet), repr(C))]
pub enum ParameterAttribute {
	Inout,
	Mutable,
	Consuming,
	Borrowing,
	Isolated,
	Variadic,
	Optional,
}
