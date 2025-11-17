#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Represents a parameter in a function or method.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Parameter {
    pub name: String,
    pub ty: Option<Box<Type>>, // Renamed 'type' to 'ty'
    pub attributes: Option<Vec<ParameterAttribute>>,
    pub default_value: Option<ConstExpr>,
    pub description: Option<String>,
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
