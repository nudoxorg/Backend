#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use super::function;
use crate::{generics::{GenericArg, TraitRef, TypeParam}, parameter::Parameter, primitives::Primitive, protocols::GenericBound, record::SumVariant};

/// Universal representation of types across languages.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))] // Example for tagged enum
// TODO: Add builtins
pub enum Type {
	ResolvedPath(Path),
	DynTrait(DynTrait),
	GenericParam(String),
	Primitive(Primitive),
	FunctionPointer(FunctionPointer),
	Tuple(Vec<Type>),
	Slice(Box<Type>),
	Array { ty: Box<Type>, length: usize },
	Pattern { ty: Box<Type> },
	ImplTrait(Vec<GenericBound>),
	Infer,
	RawPointer { is_mutable: bool, ty: Box<Type> },
	Union(Vec<Type>),
	Sum(Vec<SumVariant>),
	Intersection(Vec<Type>),
	BorrowedRef { lifetime: Option<String>, is_mutable: bool, ty: Box<Type> },
	QualifiedPath(QualifiedPath),
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Path {
	pub path:         String,
	pub generic_args: Option<Vec<GenericArg>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct QualifiedPath {
	pub name:              String,
	pub generic_arguments: Option<Vec<GenericArg>>,
	pub self_type:         Box<Type>,
	pub tr:                Option<Path>,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DynTrait {
	pub traits:   Vec<PolyTrait>,
	pub lifetime: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct GenericParam {
	pub name: String,

	/// For Higher-Kinded Types (HKTs), define the expected "shape" of the generic.
	pub kind: Option<Box<Type>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Primitive {
	/// Signed Integers (8, 16, 32, 64, 128, and Architecture-dependent)
	Int(IntWidth),

	/// Unsigned Integers (8, 16, 32, 64, 128, and Architecture-dependent)
	UInt(IntWidth),

	/// Floating point numbers
	Float(FloatWidth),

	/// Boolean logic
	Bool,

	/// UTF-8 or similar string types
	String,

	/// Single character type
	Char,

	/// Binary data / Byte buffers
	Bytes,

	/// Temporal/Date types
	Date,

	/// A pointer-sized address (void*)
	Address,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum IntWidth {
	W8,
	W16,
	W32,
	W64,
	W128,

	/// Machine dependent/usize
	Arch,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum FloatWidth {
	W16,
	W32,
	W64,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FunctionPointer {
	pub inputs:         Option<Vec<Parameter>>,
	pub outputs:        Option<Vec<Parameter>>,
	pub generic_params: Option<Vec<TypeParam>>,
	pub attributes:     Option<Vec<function::Attribute>>,
}

// MARK: - PolyTrait

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PolyTrait {
	pub tr:        TraitRef,
	pub lifetimes: Vec<String>,
}
