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
