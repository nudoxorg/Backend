//! Bind-parameter and result-column value type for the rusqdoltlite safe API.
//!
//! [`Value`] is the currency type passed to statement bind methods and returned
//! from row accessors when the caller does not want a typed extraction.

/// A dynamically-typed SQLite value corresponding to one of the five SQLite
/// storage classes.
///
/// All bind-parameter and untyped row-accessor methods accept or return this
/// type.  For typed extraction use the dedicated column getter methods on `Row`
/// instead.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
	/// The SQL `NULL` storage class — absence of a value.
	Null,

	/// The `INTEGER` storage class, stored as a signed 64-bit integer.
	Integer(i64),

	/// The `REAL` storage class, stored as an IEEE 754 double-precision float.
	Real(f64),

	/// The `TEXT` storage class, stored as a UTF-8 string.
	Text(String),

	/// The `BLOB` storage class, stored as an arbitrary byte sequence.
	Blob(Vec<u8>),
}

impl Value {
	/// Returns `true` if this value is [`Value::Null`].
	pub fn is_null(&self) -> bool {
		matches!(self, Value::Null)
	}

	/// Returns the SQLite storage-class name for this value.
	///
	/// The returned string is one of `"null"`, `"integer"`, `"real"`,
	/// `"text"`, or `"blob"`.
	pub fn type_name(&self) -> &'static str {
		match self {
			Value::Null => "null",
			Value::Integer(_) => "integer",
			Value::Real(_) => "real",
			Value::Text(_) => "text",
			Value::Blob(_) => "blob",
		}
	}
}

impl From<i64> for Value {
	fn from(integer: i64) -> Self { Value::Integer(integer) }
}

impl From<i32> for Value {
	/// Widens the 32-bit integer to `i64` without loss.
	fn from(integer: i32) -> Self { Value::Integer(i64::from(integer)) }
}

impl From<f64> for Value {
	fn from(real: f64) -> Self { Value::Real(real) }
}

impl From<String> for Value {
	fn from(text: String) -> Self { Value::Text(text) }
}

impl From<&str> for Value {
	fn from(text: &str) -> Self { Value::Text(text.to_string()) }
}

impl From<Vec<u8>> for Value {
	fn from(blob: Vec<u8>) -> Self { Value::Blob(blob) }
}

impl From<&[u8]> for Value {
	fn from(blob: &[u8]) -> Self { Value::Blob(blob.to_vec()) }
}

impl<InnerValue: Into<Value>> From<Option<InnerValue>> for Value {
	/// Maps `None` to [`Value::Null`] and `Some(v)` to `v.into()`.
	fn from(option: Option<InnerValue>) -> Self {
		match option {
			None => Value::Null,
			Some(inner) => inner.into(),
		}
	}
}
