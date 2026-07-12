#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use super::function;
use crate::{generics::{GenericArg, TraitRef}, parameter::Parameter, primitives::Primitive, protocols::GenericBound, record::{Record, SumVariant}};

/// Universal representation of types across languages.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum Type {
	/// A named reference to a concrete type or struct.
	/// Ex: `std::string::String`, `MyStruct`, `Vec<T>`
	TypeReference(TypeReference),

	/// A receiver/self type such as Rust `Self` or TypeScript `this`.
	SelfType,

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

	/// An inline record or object literal type.
	RecordLiteral(Box<Record>),

	/// A dynamically-sized view into a contiguous sequence.
	/// Ex: `[u8]` or `[]T`.
	Slice(Box<Type>),

	/// A fixed-size contiguous sequence.
	/// Ex: `[i32; 4]` or `std::array<int, 4>`.
	Array { r#type: Box<Type>, length: usize },

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
	RawPointer { is_mutable: bool, r#type: Box<Type> },

	/// A managed reference with optional lifetime/mutability tracking.
	/// Ex: `&'a mut T`.
	BorrowedRef { lifetime: Option<String>, is_mutable: bool, r#type: Box<Type> },

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

	/// Type-level operators such as `keyof T` or `readonly T`.
	TypeOperator(TypeOperator),

	/// Conditional types such as `T extends U ? X : Y`.
	Conditional(ConditionalType),

	/// Mapped types such as `{ [K in keyof T]: T[K] }`.
	Mapped(MappedType),

	/// Type predicates such as `value is Foo` or `asserts this is Bar`.
	Predicate(TypePredicate),

	/// A literal type carrying its exact **value**, not just its kind.
	/// Ex: `"foo"`, `42`, `true`, `10n`. (deno_doc collapsed these to the bare
	/// keyword type, discarding the value — this preserves it.)
	Literal(LiteralValue),

	/// A template-literal type with its structure preserved: interleaved literal
	/// string chunks (`quasis`) and embedded `types`.
	/// Ex: `` `prefix-${T}-suffix` ``. (deno_doc flattened these to `string`.)
	TemplateLiteral(TemplateLiteralType),

	/// A `typeof x` query — the type of a value binding, kept as a first-class
	/// query rather than a fake name reference (deno stringified it to a name).
	TypeQuery(TypeQuery),

	/// A tuple whose elements carry labels: `[first: string, ...rest: number[]]`.
	/// (deno_doc dropped the labels; a plain [`Type::Tuple`] is used when there
	/// are none.)
	NamedTuple(Vec<TupleMember>),
}

/// The kind of a literal-type value.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum LiteralKind {
	String,
	Number,
	Boolean,
	BigInt,
}

/// A literal type plus its exact source spelling (`"foo"`, `42`, `10n`, `true`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LiteralValue {
	pub kind:  LiteralKind,
	/// Exact source text of the literal, value included.
	pub value: String,
}

/// A structured template-literal type. `quasis` are the literal chunks between
/// interpolations (`n + 1` of them for `n` embedded `types`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TemplateLiteralType {
	pub quasis: Vec<String>,
	pub types:  Vec<Type>,
}

/// A `typeof x` type query: the dotted name of the queried binding plus any
/// explicit type arguments.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeQuery {
	pub name:         String,
	pub generic_args: Option<Vec<GenericArg>>,
}

/// One element of a labelled tuple.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TupleMember {
	/// The member label (`first` in `[first: string]`), if any.
	pub label:  Option<String>,
	pub r#type: Type,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeReference {
	/// The name/identifier of the type (e.g., "std::vec::Vec")
	pub identifier:   String,
	/// Arguments for the reference (e.g., the "T" in "Vec<T>")
	pub generic_args: Option<Vec<GenericArg>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct QualifiedPath {
	pub name:              String,
	pub generic_arguments: Option<Vec<GenericArg>>,
	pub self_type:         Box<Type>,
	/// The specific trait the reference is being qualified through.
	pub tr:                Option<TypeReference>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct GenericParam {
	pub name: String,

	/// For Higher-Kinded Types (HKTs), the expected kind/shape of this variable.
	pub kind: Option<Box<Type>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FunctionPointer {
	pub inputs:  Option<Vec<Parameter>>,
	pub outputs: Option<Vec<Parameter>>,

	/// Metadata like `#[unsafe]`, `extern "C"`, or async status.
	pub attributes: Option<Vec<function::Attribute>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DynTrait {
	pub traits:   Vec<PolyTrait>,
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

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeOperator {
	pub operator: String,
	pub r#type:   Box<Type>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ConditionalType {
	pub check_type:   Box<Type>,
	pub extends_type: Box<Type>,
	pub true_type:    Box<Type>,
	pub false_type:   Box<Type>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ModifierPrefix {
	Preserve,
	Add,
	Remove,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct MappedType {
	pub readonly:    Option<ModifierPrefix>,
	pub optional:    Option<ModifierPrefix>,
	pub parameter:   String,
	pub source_type: Box<Type>,
	pub name_type:   Option<Box<Type>>,
	pub value_type:  Option<Box<Type>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum PredicateSubject {
	This,
	Identifier(String),
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypePredicate {
	pub asserts: bool,
	pub subject: PredicateSubject,
	pub r#type:  Option<Box<Type>>,
}
