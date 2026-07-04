use crate::{arena::EntryIdx, primitive::Primitive};

#[derive(Debug, PartialEq, Eq)]
pub enum Type {
	/// A receiver/self type such as Rust `Self` or TypeScript `this`.
	SelfType,

	/// A fundamental, language-level built-in type.
	/// Ex: `i32`, `f64`, `bool`.
	Primitive(Primitive),

	/// A fixed-length, heterogeneous collection of types.
	/// Ex: `(i32, String)`. An empty vec `()` represents the Unit type.
	Tuple(Vec<EntryIdx<Type>>),

	/// A dynamically-sized view into a contiguous sequence.
	/// Ex: `[u8]` or `[]T`.
	Slice(EntryIdx<Type>),

	/// A fixed-size contiguous sequence.
	/// Ex: `[i32; 4]` or `std::array<int, 4>`.
	Array { ty: EntryIdx<Type>, length: usize },

	/// An untagged union or sum of types.
	/// Ex: `string | number`.
	Union(Vec<EntryIdx<Type>>),

	/// An intersection or combination of types.
	/// Ex: `Serializable & Cloneable`.
	Intersection(Vec<EntryIdx<Type>>),

	/// Represents a type that cannot exist (Bottom Type).
	/// Ex: `!` in Rust, `never` in TypeScript, `NoReturn` in Python.
	Never,

	/// Represents the "All" type (Top Type).
	/// Ex: `any` or `unknown` in TypeScript, `Object` in Java.
	Any,
}
