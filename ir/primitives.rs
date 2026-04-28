#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// An unsigned integer width.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum IntWidth {
	W8,
	W16,
	W32,
	W64,
	W128,
	/// Machine-dependent / pointer-sized (e.g., `usize`, `isize`).
	Arch,
}

/// A floating-point width.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum FloatWidth {
	W16,
	W32,
	W64,
}

/// A language-level primitive type, independent of any target architecture.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Primitive {
	/// Signed integer (8 / 16 / 32 / 64 / 128 / arch-dependent bits).
	Int(IntWidth),

	/// Unsigned integer (8 / 16 / 32 / 64 / 128 / arch-dependent bits).
	UInt(IntWidth),

	/// Floating-point number (16 / 32 / 64 bits).
	Float(FloatWidth),

	/// Boolean logic.
	Bool,

	/// UTF-8 or platform-equivalent string type.
	String,

	/// Single Unicode code-point.
	Char,

	/// Raw binary data / byte buffer.
	Bytes,

	/// Temporal / date type.
	Date,

	/// Raw pointer-sized address (`void *`).
	Address,
}
