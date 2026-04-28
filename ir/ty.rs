#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use super::function;
use crate::{
	generics::{GenericArg, TraitRef, TypeParam},
	parameter::{LiteralParameter, Parmeter},
	primitives::Primitive,
	protocols::GenericBound,
	record::SumVariant,
};

/// Universal representation of types across languages.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum Type {
	/// A named reference to a concrete type or struct.
	/// Ex: `std::string::String`, `MyStruct`, `Vec<T>`
	TypeReference(TypeReference),

	/// A dynamically dispatched trait object or interface.
	/// Ex: `dyn std::fmt::Display` in Rust, or `Runnable` in Java.
	DynTrait(DynTrait),

	/// A generic type parameter or a Higher-Kinded Type (HKT) variable.
	/// Ex: `T` in `Box<T>`, or `F<_>` in Scala.
	GenericParam(GenericParam),

	/// A fundamental, language-level built-in type.
	/// Ex: `i32`, `f64`, `bool`.
	// NOTE: Most literals should resolve to this
	Primitive(Primitive),

	/// A function signature or pointer to a function.
	/// Ex: `fn(i32) -> bool` or `(a: number) => string`.
	FunctionPointer(FunctionPointer),

	/// A fixed-length, heterogeneous collection of types.
	/// Ex: `(i32, String)`. An empty vec `()` represents the Unit type.
	Tuple(Vec<Type>),

	/// A dynamically-sized view into a contiguous sequence.
	/// Ex: `[u8]` or `[]T`.
	Slice(Box<Type>),

	/// A fixed-size contiguous sequence.
	/// Ex: `[i32; 4]` or `std::array<int, 4>`.
	Array { ty: Box<Type>, length: usize },

	/// An abstract type bound by traits (Existential types).
	/// Ex: `impl Iterator<Item = u8>`.
	ImplTrait(Vec<GenericBound>),

	/// A placeholder for the compiler to fill.
	/// Ex: `_` in Rust or `var`/`auto` in some contexts.
	Infer,

	/// Represents a type that cannot exist (Bottom Type).
	/// Ex: `!` in Rust, `never` in TypeScript, `NoReturn` in Python.
	Never,

	/// Represents the "All" type (Top Type).
	/// Ex: `any` or `unknown` in TypeScript, `Object` in Java.
	Any,

	/// A raw, unmanaged pointer.
	/// Ex: `*mut T`, `int*`.
	RawPointer { is_mutable: bool, ty: Box<Type> },

	/// A managed reference with optional lifetime/mutability tracking.
	/// Ex: `&'a mut T`.
	BorrowedRef { lifetime: Option<String>, is_mutable: bool, ty: Box<Type> },

	/// An untagged union or sum of types.
	/// Ex: `string | number`.
	Union(Vec<Type>),

	/// An intersection or combination of types.
	/// Ex: `Serializable & Cloneable`.
	Intersection(Vec<Type>),

	/// A tagged union or ADT (Algebraic Data Type).
	/// Ex: `enum Option<T> { Some(T), None }`.
	Sum(Vec<SumVariant>),

	/// A fully qualified reference for projecting associated types.
	/// Ex: `<T as Trait>::AssocType`.
	QualifiedPath(QualifiedPath),

	/// Variadic types or parameter packs.
	/// Ex: `...T` in TypeScript or `Args...` in C++.
	Variadic(Box<Type>),
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeReference {
	/// The name/identifier of the type (e.g., "std::vec::Vec")
	pub identifier: String,
	/// Arguments for the reference (e.g., the "T" in "Vec<T>")
	pub generic_args: Option<Vec<GenericArg>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct QualifiedPath {
	pub name: String,
	pub generic_arguments: Option<Vec<GenericArg>>,
	pub self_type: Box<Type>,
	/// The specific trait the reference is being qualified through.
	pub tr: Option<TypeReference>,
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
	pub inputs: Option<Vec<Parmeter>>,
	pub outputs: Option<Vec<Parmeter>>,

	/// Metadata like `#[unsafe]`, `extern "C"`, or async status.
	pub attributes: Option<Vec<function::Attribute>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DynTrait {
	pub traits: Vec<PolyTrait>,
	pub lifetime: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PolyTrait {
	// TODO: Make this a literal reference
	pub trait_ref: TraitRef,

	/// For Higher-Rank Trait Bounds (HRTBs) like `for<'a> Trait<'a>`
	// TODO: Type this too fr
	pub lifetimes: Vec<String>,
}
