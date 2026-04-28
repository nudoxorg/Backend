#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use super::function;
use crate::{function::Function, generics::{ConstExpr, Constraint, Generics, TraitRef}, kind::Visibility, parameter::Parameter, ty::Type};

// MARK: - TraitDef

/// Universal representation of traits (Rust), protocols (Swift), interfaces
/// (Java/C#/TypeScript), etc.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitDef {
	/// The name of the trait/protocol/interface
	pub name: String,

	/// Generic parameters
	pub generics: Option<Generics>,

	/// Supertraits/protocol inheritance/interface extends
	pub super_traits: Option<Vec<TraitRef>>,

	/// Associated types (Rust/Swift protocols)
	pub associated_types: Option<Vec<AssociatedType>>,

	/// Required methods
	pub required_methods: Option<Vec<TraitMethod>>,

	/// Provided/default method implementations
	pub provided_methods: Option<Vec<TraitMethod>>,

	/// Required constants/static members
	pub required_constants: Option<Vec<TraitConstant>>,

	/// Trait-level attributes
	pub attributes: Option<Vec<TraitAttribute>>,

	/// Visibility
	pub visibility: Option<Visibility>,

	/// Documentation
	pub docs: Option<String>,
}

// MARK: - Associated Types

/// Associated types in traits/protocols
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct AssociatedType {
	/// Name of the associated type
	pub name: String,

	/// Bounds/constraints on the associated type
	pub bounds: Option<Vec<GenericBound>>,

	/// Default type (if any)
	pub default_type: Option<Type>,

	/// Documentation
	pub docs: Option<String>,
}

// Assuming GenericBound is a new enum/struct
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum GenericBound {
	Trait(TraitRef),
	Lifetime(String),
}

// MARK: - Trait Methods

/// A method signature within a trait/protocol/interface
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitMethod {
	/// Method name
	pub name: String,

	/// Input parameters
	pub parameters: Option<Vec<Parameter>>,

	/// Return type
	pub return_type: Option<Box<Type>>,

	/// Generic parameters specific to this method
	pub generics: Option<Generics>,

	/// Method-level attributes
	pub attributes: Option<Vec<function::Attribute>>,

	/// Receiver type (self, &self, &mut self, etc.)
	pub receiver: Option<ReceiverKind>,

	/// Whether this method has a default implementation
	pub has_default_implementation: bool,

	/// Documentation
	pub docs: Option<String>,
}

/// Receiver/self parameter kind
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ReceiverKind {
	/// Takes ownership (self in Rust, consuming in Swift)
	Owned,

	/// Immutable reference (&self, borrowing in Swift)
	SharedRef,

	/// Mutable reference (&mut self, mutating in Swift)
	MutRef,

	/// Static/class method (no receiver)
	Static,

	/// Arbitrary receiver (arbitrary self types in Rust)
	Arbitrary,
}

// MARK: - Trait Constants

/// A constant/static member in a trait
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitConstant {
	/// Constant name
	pub name: String,

	/// Type of the constant
	pub r#type: Box<Type>,

	/// Default value (if provided)
	pub default_value: Option<ConstExpr>,

	/// Documentation
	pub docs: Option<String>,
}

// MARK: - Trait Attributes

/// Attributes that can be applied to traits
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TraitAttribute {
	/// Marker trait with no methods (e.g., Send, Sync in Rust)
	Marker,
	/// Auto trait (automatically implemented, like Send/Sync)
	Auto,
	/// Unsafe trait (requires unsafe to implement)
	Unsafe,
	/// Object-safe/dyn-compatible trait
	ObjectSafe,
	/// Sealed trait (can only be implemented in current module)
	Sealed,
	/// Functional interface (single abstract method, like Java's
	/// @FunctionalInterface)
	Functional,
	/// Custom attribute with name and optional arguments
	Custom { name: String, args: Option<Vec<String>> },
}

// MARK: - Trait Implementation

/// Represents an implementation of a trait for a type
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitImpl {
	/// The trait being implemented
	pub tr: TraitRef,

	/// The type implementing the trait
	pub for_type: Box<Type>,

	/// Generic parameters for this impl
	pub generics: Option<Generics>,

	/// Where clauses/constraints
	pub where_constraints: Option<Vec<Constraint>>,

	/// Implemented methods
	pub methods: Option<Vec<Function>>,

	/// Associated type implementations
	pub associated_types: Option<Vec<AssociatedTypeImpl>>,

	/// Associated constant implementations
	pub associated_constants: Option<Vec<TraitConstant>>,

	/// Whether this is a negative impl (Rust: impl !Trait)
	pub is_negative: bool,

	/// Whether this is a blanket impl (impl<T> Trait for T)
	pub is_blanket: bool,

	/// Whether this impl is unsafe
	pub is_unsafe: bool,

	/// Visibility of the impl block
	pub visibility: Option<Visibility>,

	/// Documentation
	pub docs: Option<String>,
}

/// Implementation of an associated type
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct AssociatedTypeImpl {
	/// Name of the associated type
	pub name: String,

	/// The concrete type
	pub r#type: Box<Type>,
}
