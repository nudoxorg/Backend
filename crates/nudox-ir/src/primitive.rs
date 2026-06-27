use ecow::EcoString;

use crate::{arena::EntryIdx, ty::Type};

/// A language-level primitive type, independent of any target architecture.
#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
	Integer {
		signed: bool,
		width:  Width,
	},

	Float(Width),

	Bool,

	Char,

	/// Type of a string literal (if it exists)
	///
	/// Note that this is specifically for primitive types, so this should be
	/// equivalent to `str` in Rust or `string` in C#, not Rust's `String` or
	/// C++'s `std::string`.
	// TODO: should rust string literals resolve to BorrowedRef then?
	// TODO: should C/C++ string literals resolve to `char*` or `char[]` instead?
	Str,

	/// A raw, mutable, unmanaged pointer.
	/// Ex: `*mut T`, `int*`.
	MutPointer(EntryIdx<Type>),

	/// A raw, const, unmanaged pointer.
	/// Ex: `*const T`, `int *const`.
	ConstPointer(EntryIdx<Type>),

	/// A managed reference with optional lifetime/mutability tracking.
	/// Ex: `&'a mut T`.
	Reference {
		lifetime: Option<EcoString>,
		mutable:  bool,
		ty:       EntryIdx<Type>,
	},

	/// An arbitrary primtive type, e.g. Date in JavaScript/TypeScript
	Builtin(EcoString),
}

/// A language-level primitive type, independent of any target architecture.
#[derive(Debug, Clone, PartialEq)]
pub enum Width {
	Fixed(usize),

	/// Machine-dependent / pointer-sized (e.g., `usize`, `isize`).
	/// Generally not applicable to Floats
	Arch,
}

impl Width {
	/// 8-bit width
	pub const W8: Self = Width::Fixed(8);

	/// 16-bit width
	pub const W16: Self = Width::Fixed(16);

	/// 32-bit width
	pub const W32: Self = Width::Fixed(32);

	/// 64-bit width
	pub const W64: Self = Width::Fixed(64);

	/// 80-bit width
	pub const W80: Self = Width::Fixed(80);

	/// 128-bit width
	pub const W128: Self = Width::Fixed(128);
}
