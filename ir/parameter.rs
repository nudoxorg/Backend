#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
	generics::{ConstExpr, TypeParam},
	ty::Type,
};

/// Represents a parameter in a function or method.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LiteralParameter {
	pub name: String,
	pub r#type: Option<Type>,
	pub attributes: Option<Vec<ParameterAttribute>>,
	pub default_value: Option<ConstExpr>,
	pub description: Option<String>,
}

pub enum Parmeter {
	Literal(LiteralParameter),
	Generic(TypeParam),
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ParameterAttribute {
	Inout,
	Mutable,
	Consuming,
	Borrowing,
	Isolated,
	Variadic,
	Optional,
}
