#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
	generics::{ConstExpr, GenericArg, Generics},
	kind::Visibility,
	ty::Type,
};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Record {
	/// Optional name of the record (e.g., "User", "Point").
	/// Anonymous records (like tuples or JS objects) may omit this.
	pub name: Option<String>,

	/// Optional generic parameters (e.g., <T, U>).
	pub generics: Option<Generics>,

	/// The fields of the record (if applicable).
	/// A (empty lack of fields implies dynamic fields, AKA classical JS and Python
	/// We avoid None for this because there ARE still fields, just unkown. So it would be Some([])
	/// None is exclusively for unit types
	// TODO: Use an Optional wrapper where this is indicated
	pub fields: Option<Vec<Field>>,

	/// The visibility of the record
	pub visibility: Visibility,
}

/// A field on a Record
/// If this record is a classical tuple, expect for the fields to have no name
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Field {
	/// Field name (None if tuple-like).
	pub name: Option<String>,

	/// Type of the field (if known).
	pub ty: Option<Box<Type>>,

	/// The default value
	/// Some languages hold default values in external stores (I.E Default impls in Rust, for which this would still be none, but in which it is assumed a developer would expect this case, and search there.)
	pub default_value: Option<ConstExpr>,

	/// The state of potential changes to the field
	pub mutability: Option<FieldAttribute>,

	/// To whom the field can be viewed by
	pub visibility: Option<Visibility>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum FieldAttribute {
	Mutable,
	Optional,
}

/// Adding sum variants here for historical reasons
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SumVariant {
	/// The variant/tag name (e.g., "Some", "None", "Ok", "Err")
	pub name: String,

	/// Associated types for this variant (None for unit variants)
	pub types: Option<Vec<Type>>, /* has to be vec because it could technically hold more than
	                               * one?? Might want to use an enum for this idk */
}
