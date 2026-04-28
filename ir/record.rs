#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{generics::{ConstExpr, GenericArg, Generics}, kind::Visibility, ty::Type};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Record {
	/// Optional name of the record (e.g., "User", "Point").
	/// Anonymous records (like tuples or JS objects) may omit this.
	pub name: Option<String>,

	/// Optional generic parameters (e.g., <T, U>).
	pub generics: Option<Generics>,

	/// The fields of the record (if applicable).
	/// An empty lack of fields implies dynamic fields, AKA classical JS and
	/// Python. None is exclusively for unit types.
	pub fields: Option<Vec<Field>>,

	/// The visibility of the record
	pub visibility: Visibility,

	/// For JS `__proto__`, Python base classes, or CSS mixins.
	/// This represents the delegation link.
	pub prototypes: Vec<Type>,
}

/// For defining the shape of data
/// Ex: [key: string]: number;
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct IndexSignature {
	pub key_type:   Box<Type>, // Usually String or Number
	pub value_type: Box<Type>,
}

/// Route and wrap fields that are known/declared
/// As is the commonality for static languages
/// But also be able to express fields that are not known (yet)
/// As in JS/Py
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Field {
	/// For when the exact requirements are provided
	Known(KnownField),

	/// For when the general shape of the data is known
	Pattern(IndexSignature),

	/// When nothing is known about this field.
	/// Used for gradual typing or unparsed dynamic objects.
	Unknown,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum FieldKey {
	/// Standard identifier: `name: "value"`
	Ident(String),

	/// For tuples: `(1, 2, 3)` -> indices 0, 1, 2
	Index(usize),

	/// Computed keys: `[Symbol.iterator]`, `["key" + i]`
	/// This wraps a ConstExpr or even a full Expr depending on your IR depth.
	Computed(ConstExpr),
}

/// A field on a Record
/// If this record is a classical tuple, expect for the fields to have no name
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct KnownField {
	/// Field name.
	pub key: FieldKey,

	/// Type of the field (if known).
	pub ty: Option<Box<Type>>,

	/// The default value
	/// Some languages hold default values in external stores (I.E Default impls
	/// in Rust, for which this would still be none, but in which it is assumed a
	/// developer would expect this case, and search there.)
	pub default_value: Option<ConstExpr>,

	/// The state and metadata of potential changes to the field
	pub attributes: FieldAttributes,

	/// To whom the field can be viewed by
	pub visibility: Option<Visibility>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FieldAttributes {
	/// Support for inline declarations like Java's @Override,
	/// Python decorators, or serde(default)-like attributes in Rust
	pub decorators: Vec<String>, // TODO: Use a dedicated Metadata/Expr struct

	/// For fields that are marked as mutable
	/// If fields are immutable by default in the language just mark this as
	/// false.
	pub is_mutable: bool,

	/// For fields that are marked as optional
	pub is_optional: bool,

	/// For differentiating instance properties from static class properties
	pub is_static: bool,
}

/// Adding sum variants here for historical reasons
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SumVariant {
	/// The variant/tag name (e.g., "Some", "None", "Ok", "Err")
	pub name: String,

	/// Associated data for this variant (None for unit variants)
	pub data: Option<SumField>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum SumField {
	/// For inline types that have no names (e.g., Tuple variants in Rust)
	Tuple(Vec<Type>),

	/// For internal field like structures (e.g., Struct variants in Rust)
	StructLike(Vec<Field>),
}
