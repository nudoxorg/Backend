#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{entry::NudoxPath, function::Function, generics::{ConstExpr, Generics}, kind::Visibility, ty::Type};

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
	pub fields: Vec<Field>,

	/// Callable signatures attached directly to the record shape.
	pub call_signatures: Option<Vec<Function>>,

	/// Constructor signatures attached directly to the record shape.
	pub constructors: Option<Vec<Function>>,

	/// Inline methods declared directly on the record shape.
	pub methods: Option<Vec<Function>>,

	/// Index signatures such as `[key: string]: number`.
	pub index_signatures: Option<Vec<IndexSignature>>,

	/// Base or implemented types explicitly referenced by this record.
	pub super_types: Option<Vec<Type>>,

	/// Child entries conceptually scoped to this Record.
	///
	/// Explicit struct fields are tracked via the `fields` array inline. However,
	/// a Record might also encapsulate full nested types (e.g., Java nested
	/// classes), static namespaces, or separated explicit methods (e.g., Rust
	/// inherent `impl` blocks).
	pub members: Option<Vec<NudoxPath>>,

	/// Protocols, traits, or interfaces this Record explicitly implements.
	pub implemented_protocols: Option<Vec<NudoxPath>>,
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
	pub r#type: Option<Box<Type>>,

	/// The default value
	/// Some languages hold default values in external stores (I.E Default impls
	/// in Rust, for which this would still be none, but in which it is assumed a
	/// developer would expect this case, and search there.)
	pub default_value: Option<ConstExpr>,

	/// The state and metadata of potential changes to the field
	pub attributes: FieldAttributes,

	/// To whom the field can be viewed by
	pub visibility: Option<Visibility>,

	/// Field-level documentation strings.
	pub documentation: Option<String>,
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

/// Algebraic sum / enum / sealed hierarchy as a first-class IR surface.
///
/// Historical `Entry::SumType(Symbol<Vec<SumVariant>>)` could only carry
/// variants — methods, supers, generics, and underlying types were lost or
/// dual-emitted only as free-floating entries. This container keeps that
/// structure on the sum itself (mirroring [`Record`]).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SumType {
	/// Enum / ADT variants in declaration order.
	pub variants: Vec<SumVariant>,

	/// Declaration-site type parameters (`enum E<T> { … }`, Java/C# generic enums).
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
	pub generics: Option<Generics>,

	/// Methods attached to the sum (Rust `impl Enum`, Java enum methods, …).
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
	pub methods: Option<Vec<Function>>,

	/// Protocols / interfaces / traits this sum implements.
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
	pub implemented_protocols: Option<Vec<NudoxPath>>,

	/// Super types (extends / implements / sealed permits targets as types).
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
	pub super_types: Option<Vec<Type>>,

	/// Child entry paths (methods, nested types, dual-emitted functions).
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
	pub members: Option<Vec<NudoxPath>>,

	/// Underlying representation (Go iota `type T int`, C# enum `: byte`, …).
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
	pub underlying: Option<Type>,
}

impl SumType {
	/// Build a sum that only has variants (legacy producers / inline ADTs).
	pub fn from_variants(variants: Vec<SumVariant>) -> Self {
		Self {
			variants,
			generics: None,
			methods: None,
			implemented_protocols: None,
			super_types: None,
			members: None,
			underlying: None,
		}
	}
}

impl From<Vec<SumVariant>> for SumType {
	fn from(variants: Vec<SumVariant>) -> Self {
		Self::from_variants(variants)
	}
}

/// Adding sum variants here for historical reasons
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SumVariant {
	/// The variant/tag name (e.g., "Some", "None", "Ok", "Err")
	pub name: String,

	/// Associated data for this variant (None for unit variants)
	pub data: Option<SumField>,

	/// Variant-level documentation strings.
	pub documentation: Option<String>,

	/// Explicit discriminant / constant value when known (`Foo = 1`, iota value).
	#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
	pub discriminant: Option<ConstExpr>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum SumField {
	/// For inline types that have no names (e.g., Tuple variants in Rust)
	Tuple(Vec<Type>),

	/// For internal field like structures (e.g., Struct variants in Rust)
	StructLike(Vec<Field>),
}
