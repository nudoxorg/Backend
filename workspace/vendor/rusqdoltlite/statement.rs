//! Internal prepared-statement wrapper.
//!
//! [`PreparedStatement`] owns a `sqlite3_stmt` and drives the compile → bind →
//! step lifecycle. It is not part of the public API; [`crate::Connection`] uses
//! it to implement `execute` and `query_rows`. Finalization is handled on drop
//! so an early return never leaks a statement.

use core::ffi::{c_char, c_int};
use core::ptr;

use crate::error::{byte_length_to_c_int, EngineError};
use crate::row::Row;
use crate::sys;
use crate::value::Value;

/// An owned, compiled SQL statement bound to a connection.
pub(crate) struct PreparedStatement {
	/// The compiled statement handle; non-null for the lifetime of this value.
	statement: *mut sys::sqlite3_stmt,
	/// Borrowed connection handle used to fetch error messages on failure.
	database: *mut sys::sqlite3,
	/// The original SQL text, retained for error reporting.
	sql: String,
}

impl PreparedStatement {
	/// Compile a single SQL statement against a connection.
	///
	/// # Safety
	/// `database` must be a live connection handle for the lifetime of the
	/// returned statement.
	pub(crate) unsafe fn prepare(
		database: *mut sys::sqlite3,
		sql: &str,
	) -> Result<Self, EngineError> {
		let mut statement: *mut sys::sqlite3_stmt = ptr::null_mut();
		let sql_byte_length = byte_length_to_c_int(sql.len(), "SQL text")?;
		// SAFETY: `database` is live per the contract; we pass the SQL text with an
		// explicit, range-checked byte length so interior NULs are impossible to
		// misinterpret and an over-2-GiB length cannot wrap negative, and ignore
		// the unused tail pointer.
		let result_code = unsafe {
			sys::sqlite3_prepare_v2(
				database,
				sql.as_ptr().cast::<c_char>(),
				sql_byte_length,
				&mut statement,
				ptr::null_mut(),
			)
		};
		if result_code != sys::SQLITE_OK || statement.is_null() {
			// SAFETY: `database` is live; reading its last error is always valid.
			let message = unsafe { last_error_message(database) };
			return Err(EngineError::from_sql(result_code, message, sql));
		}
		Ok(Self {
			statement,
			database,
			sql: sql.to_owned(),
		})
	}

	/// Bind the positional parameters (1-based on the C side, 0-based here).
	pub(crate) fn bind_all(&mut self, parameters: &[Value]) -> Result<(), EngineError> {
		for (offset, parameter) in parameters.iter().enumerate() {
			let index = (offset + 1) as c_int;
			self.bind_one(index, parameter)?;
		}
		Ok(())
	}

	/// Bind a single parameter at a 1-based index.
	fn bind_one(&mut self, index: c_int, parameter: &Value) -> Result<(), EngineError> {
		let result_code = match parameter {
			// SAFETY: the statement handle is live and `index` is a valid 1-based
			// parameter slot for every bind call below.
			Value::Null => unsafe { sys::sqlite3_bind_null(self.statement, index) },
			Value::Integer(integer) => unsafe {
				sys::sqlite3_bind_int64(self.statement, index, *integer)
			},
			Value::Real(real) => unsafe {
				sys::sqlite3_bind_double(self.statement, index, *real)
			},
			Value::Text(text) => {
				let text_byte_length =
					byte_length_to_c_int(text.len(), "text bind parameter")?;
				// SAFETY: the statement handle is live and `index` is a valid 1-based
				// parameter slot. SQLITE_TRANSIENT: the engine copies the bytes, so the
				// `text` buffer need not outlive this call. The explicit, range-checked
				// length carries any interior NUL bytes verbatim and cannot wrap
				// negative.
				unsafe {
					sys::sqlite3_bind_text(
						self.statement,
						index,
						text.as_ptr().cast::<c_char>(),
						text_byte_length,
						sys::sqlite_transient(),
					)
				}
			}
			Value::Blob(blob) => {
				let blob_byte_length =
					byte_length_to_c_int(blob.len(), "blob bind parameter")?;
				// SAFETY: the statement handle is live and `index` is a valid 1-based
				// parameter slot. SQLITE_TRANSIENT copies the bytes; the explicit,
				// range-checked length cannot wrap negative.
				unsafe {
					sys::sqlite3_bind_blob(
						self.statement,
						index,
						blob.as_ptr().cast::<core::ffi::c_void>(),
						blob_byte_length,
						sys::sqlite_transient(),
					)
				}
			}
		};
		if result_code != sys::SQLITE_OK {
			// SAFETY: the connection handle is live.
			let message = unsafe { last_error_message(self.database) };
			return Err(EngineError::from_sql(result_code, message, &self.sql));
		}
		Ok(())
	}

	/// Execute a statement that yields no rows, returning the affected count.
	///
	/// Stepping to `SQLITE_ROW` here is tolerated (a `SELECT` used with
	/// `execute` simply discards its rows) but the common case is `SQLITE_DONE`.
	pub(crate) fn execute(&mut self) -> Result<usize, EngineError> {
		loop {
			// SAFETY: the statement handle is live.
			let result_code = unsafe { sys::sqlite3_step(self.statement) };
			match result_code {
				sys::SQLITE_ROW => continue,
				sys::SQLITE_DONE => break,
				_ => {
					// SAFETY: the connection handle is live.
					let message = unsafe { last_error_message(self.database) };
					return Err(EngineError::from_sql(result_code, message, &self.sql));
				}
			}
		}
		// SAFETY: the connection handle is live.
		let changed = unsafe { sys::sqlite3_changes(self.database) };
		Ok(changed.max(0) as usize)
	}

	/// Step the statement and map every result row with `map`.
	pub(crate) fn query_map<T, F>(&mut self, mut map: F) -> Result<Vec<T>, EngineError>
	where
		F: FnMut(&Row<'_>) -> Result<T, EngineError>,
	{
		let mut collected = Vec::new();
		loop {
			// SAFETY: the statement handle is live.
			let result_code = unsafe { sys::sqlite3_step(self.statement) };
			match result_code {
				sys::SQLITE_ROW => {
					// SAFETY: `sqlite3_step` just returned a row, so the statement is
					// positioned on valid column data for the duration of the borrow.
					let row = unsafe { Row::new(self.statement) };
					collected.push(map(&row)?);
				}
				sys::SQLITE_DONE => break,
				_ => {
					// SAFETY: the connection handle is live.
					let message = unsafe { last_error_message(self.database) };
					return Err(EngineError::from_sql(result_code, message, &self.sql));
				}
			}
		}
		Ok(collected)
	}
}

impl Drop for PreparedStatement {
	fn drop(&mut self) {
		// SAFETY: `statement` was produced by `sqlite3_prepare_v2` and has not been
		// finalized; finalizing it exactly once here releases its resources.
		unsafe {
			sys::sqlite3_finalize(self.statement);
		}
	}
}

/// Read the human-readable message for the most recent error on a connection.
///
/// # Safety
/// `database` must be a live connection handle.
unsafe fn last_error_message(database: *mut sys::sqlite3) -> String {
	// SAFETY: `database` is live per the contract; `sqlite3_errmsg` returns a
	// NUL-terminated string owned by the connection, valid until the next API call.
	unsafe {
		let pointer = sys::sqlite3_errmsg(database);
		if pointer.is_null() {
			return String::from("(no error message)");
		}
		core::ffi::CStr::from_ptr(pointer)
			.to_string_lossy()
			.into_owned()
	}
}
