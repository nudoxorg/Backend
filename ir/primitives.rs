#[cfg(feature = "facet")]
use facet::Facet;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

type Float16 = f32; // Placeholder

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "facet", derive(Facet), repr(C))]
pub enum Primitive {
	Int8(Option<i8>),
	Int16(Option<i16>),
	Int(Option<isize>),
	Int64(Option<i64>),
	Int128(Option<i128>),
	UInt8(Option<u8>),
	UInt16(Option<u16>),
	UInt(Option<usize>),
	UInt64(Option<u64>),
	UInt128(Option<u128>),
	F16(Option<Float16>),
	Float(Option<f32>),
	Double(Option<f64>),
	Bool(Option<bool>),
	String(Option<String>),
	Char(Option<char>),
	// MARK: - Special Values
	Null,
	Date(Option<String>), // Using jiff::Timestamp
	Data(Option<Vec<u8>>),
}
