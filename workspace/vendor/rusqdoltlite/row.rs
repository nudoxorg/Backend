//! Typed access to a single result row.
//!
//! A [`Row`] borrows the currently-stepped prepared statement. Its typed getters
//! read column values by zero-based index and validate the column's fundamental
//! datatype before decoding, turning a wrong-type read into a structured
//! [`EngineError::TypeMismatch`] rather than a silent SQLite coercion.

use core::ffi::c_int;

use crate::error::EngineError;
use crate::sys;

/// A view over one row of a query result set.
///
/// The row is only valid for the duration of the `map` closure passed to
/// [`crate::Connection::query_rows`]; it must not outlive the underlying step.
pub struct Row<'statement> {
	/// The prepared statement positioned on the current row.
	statement: *mut sys::sqlite3_stmt,
	/// Number of columns in the result set (cached from `sqlite3_column_count`).
	column_count: usize,
	/// Ties the borrow to the owning statement's lifetime.
	_lifetime: core::marker::PhantomData<&'statement ()>,
}

impl<'statement> Row<'statement> {
	/// Wrap a statement handle that has just returned `SQLITE_ROW`.
	///
	/// # Safety
	/// The caller guarantees `statement` is non-null, points at a live prepared
	/// statement, and that the most recent `sqlite3_step` returned a row.
	pub(crate) unsafe fn new(statement: *mut sys::sqlite3_stmt) -> Self {
		// SAFETY: `statement` is a live prepared statement per the contract above;
		// `sqlite3_column_count` is always valid on such a handle.
		let column_count = unsafe { sys::sqlite3_column_count(statement) };
		Self {
			statement,
			column_count: column_count.max(0) as usize,
			_lifetime: core::marker::PhantomData,
		}
	}

	/// The number of columns available in this row.
	pub fn column_count(&self) -> usize {
		self.column_count
	}

	/// Validate a caller-supplied column index against the result width.
	fn checked_index(&self, column_index: usize) -> Result<c_int, EngineError> {
		if column_index >= self.column_count {
			return Err(EngineError::ColumnIndexOutOfRange {
				column_index,
				column_count: self.column_count,
			});
		}
		Ok(column_index as c_int)
	}

	/// The fundamental SQLite datatype of a column in this row.
	fn column_type(&self, index: c_int) -> c_int {
		// SAFETY: `index` is range-checked by the caller and the statement is live.
		unsafe { sys::sqlite3_column_type(self.statement, index) }
	}

	/// Human-readable name for a fundamental SQLite datatype code.
	fn type_name(datatype: c_int) -> &'static str {
		match datatype {
			sys::SQLITE_INTEGER => "integer",
			sys::SQLITE_FLOAT => "real",
			sys::SQLITE_TEXT => "text",
			sys::SQLITE_BLOB => "blob",
			sys::SQLITE_NULL => "null",
			_ => "unknown",
		}
	}

	/// Read a column as a 64-bit signed integer.
	///
	/// A NULL column is a type mismatch here; use [`Row::get_optional_integer`]
	/// for nullable columns.
	pub fn get_integer(&self, column_index: usize) -> Result<i64, EngineError> {
		let index = self.checked_index(column_index)?;
		let datatype = self.column_type(index);
		if datatype == sys::SQLITE_NULL {
			return Err(EngineError::TypeMismatch {
				column_index,
				expected: "integer",
				actual: "null",
			});
		}
		if datatype != sys::SQLITE_INTEGER {
			return Err(EngineError::TypeMismatch {
				column_index,
				expected: "integer",
				actual: Self::type_name(datatype),
			});
		}
		// SAFETY: index is range-checked and the column holds an integer.
		Ok(unsafe { sys::sqlite3_column_int64(self.statement, index) })
	}

	/// Read a column as an IEEE double.
	pub fn get_real(&self, column_index: usize) -> Result<f64, EngineError> {
		let index = self.checked_index(column_index)?;
		let datatype = self.column_type(index);
		if datatype == sys::SQLITE_NULL {
			return Err(EngineError::TypeMismatch {
				column_index,
				expected: "real",
				actual: "null",
			});
		}
		if datatype != sys::SQLITE_FLOAT && datatype != sys::SQLITE_INTEGER {
			return Err(EngineError::TypeMismatch {
				column_index,
				expected: "real",
				actual: Self::type_name(datatype),
			});
		}
		// SAFETY: index is range-checked; SQLite converts integer to double here.
		Ok(unsafe { sys::sqlite3_column_double(self.statement, index) })
	}

	/// Read a column as an owned UTF-8 string.
	///
	/// Returns [`EngineError::Utf8`] if the engine returned non-UTF-8 bytes.
	pub fn get_text(&self, column_index: usize) -> Result<String, EngineError> {
		let index = self.checked_index(column_index)?;
		let datatype = self.column_type(index);
		if datatype == sys::SQLITE_NULL {
			return Err(EngineError::TypeMismatch {
				column_index,
				expected: "text",
				actual: "null",
			});
		}
		let bytes = self.raw_text_bytes(index);
		match core::str::from_utf8(bytes) {
			Ok(text) => Ok(text.to_owned()),
			Err(_) => Err(EngineError::Utf8 {
				context: "Row::get_text",
			}),
		}
	}

	/// Read a column as an owned byte blob.
	pub fn get_blob(&self, column_index: usize) -> Result<Vec<u8>, EngineError> {
		let index = self.checked_index(column_index)?;
		let datatype = self.column_type(index);
		if datatype == sys::SQLITE_NULL {
			return Err(EngineError::TypeMismatch {
				column_index,
				expected: "blob",
				actual: "null",
			});
		}
		Ok(self.raw_blob_bytes(index).to_vec())
	}

	/// Read a nullable integer column, mapping SQL NULL to `None`.
	pub fn get_optional_integer(
		&self,
		column_index: usize,
	) -> Result<Option<i64>, EngineError> {
		let index = self.checked_index(column_index)?;
		if self.column_type(index) == sys::SQLITE_NULL {
			return Ok(None);
		}
		self.get_integer(column_index).map(Some)
	}

	/// Read a nullable real column, mapping SQL NULL to `None`.
	pub fn get_optional_real(&self, column_index: usize) -> Result<Option<f64>, EngineError> {
		let index = self.checked_index(column_index)?;
		if self.column_type(index) == sys::SQLITE_NULL {
			return Ok(None);
		}
		self.get_real(column_index).map(Some)
	}

	/// Read a nullable text column, mapping SQL NULL to `None`.
	pub fn get_optional_text(
		&self,
		column_index: usize,
	) -> Result<Option<String>, EngineError> {
		let index = self.checked_index(column_index)?;
		if self.column_type(index) == sys::SQLITE_NULL {
			return Ok(None);
		}
		self.get_text(column_index).map(Some)
	}

	/// Read a nullable blob column, mapping SQL NULL to `None`.
	pub fn get_optional_blob(
		&self,
		column_index: usize,
	) -> Result<Option<Vec<u8>>, EngineError> {
		let index = self.checked_index(column_index)?;
		if self.column_type(index) == sys::SQLITE_NULL {
			return Ok(None);
		}
		self.get_blob(column_index).map(Some)
	}

	/// Borrow the raw text bytes of a column (without the trailing NUL).
	fn raw_text_bytes(&self, index: c_int) -> &[u8] {
		// SAFETY: index is range-checked. `sqlite3_column_text` returns a pointer
		// valid until the next step/reset/finalize, and `sqlite3_column_bytes`
		// returns its length in bytes; we borrow it only for this call.
		unsafe {
			let pointer = sys::sqlite3_column_text(self.statement, index);
			let length = sys::sqlite3_column_bytes(self.statement, index).max(0) as usize;
			if pointer.is_null() || length == 0 {
				&[]
			} else {
				core::slice::from_raw_parts(pointer, length)
			}
		}
	}

	/// Borrow the raw blob bytes of a column.
	fn raw_blob_bytes(&self, index: c_int) -> &[u8] {
		// SAFETY: index is range-checked. `sqlite3_column_blob` returns a pointer
		// valid until the next step/reset/finalize, with length from
		// `sqlite3_column_bytes`; we borrow it only for this call.
		unsafe {
			let pointer = sys::sqlite3_column_blob(self.statement, index);
			let length = sys::sqlite3_column_bytes(self.statement, index).max(0) as usize;
			if pointer.is_null() || length == 0 {
				&[]
			} else {
				core::slice::from_raw_parts(pointer.cast::<u8>(), length)
			}
		}
	}
}
