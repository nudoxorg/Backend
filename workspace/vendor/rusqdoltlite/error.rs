//! Engine error taxonomy for the rusqdoltlite crate.
//!
//! All fallible operations return `Result<_, EngineError>`. The variants map
//! closely to SQLite primary result codes, with additional Dolt-specific and
//! type-safety variants layered on top.

use crate::sys;

/// The single error type returned by every fallible operation in this crate.
///
/// Callers should match on the variant to distinguish recoverable conditions
/// (e.g. [`EngineError::Busy`]) from programming mistakes
/// (e.g. [`EngineError::Internal`]).
#[derive(Debug)]
pub enum EngineError {
	/// `sqlite3_open_v2` failed; the database file could not be opened or
	/// created.
	CannotOpen {
		/// Filesystem path (or URI) that was passed to the open call.
		path: String,
		/// Human-readable message returned by the engine.
		message: String,
	},

	/// A `sqlite3_prepare_v2` or `sqlite3_step` call failed for a reason that
	/// does not map to a more specific variant.  The offending SQL text is
	/// included to aid diagnostics.
	Sql {
		/// The SQLite primary result code (e.g. `SQLITE_ERROR = 1`).
		primary_code: i32,
		/// Human-readable message returned by the engine via `sqlite3_errmsg`.
		extended_message: String,
		/// The SQL statement that triggered the failure.
		sql: String,
	},

	/// `SQLITE_BUSY` or `SQLITE_LOCKED`: a concurrent writer holds a lock that
	/// prevents this operation from proceeding.
	Busy {
		/// Human-readable message returned by the engine.
		message: String,
	},

	/// `SQLITE_CONSTRAINT`: a uniqueness, foreign-key, check, or NOT NULL
	/// constraint was violated.
	Constraint {
		/// Human-readable message returned by the engine.
		message: String,
	},

	/// A typed row-getter was asked to extract a column value as a type that
	/// does not match the value actually stored in that column.
	TypeMismatch {
		/// Zero-based column index.
		column_index: usize,
		/// The Rust type the caller requested (e.g. `"i64"`).
		expected: &'static str,
		/// The SQLite storage class actually present (e.g. `"text"`).
		actual: &'static str,
	},

	/// A column index supplied to a row-getter exceeds the number of columns
	/// returned by the statement.
	ColumnIndexOutOfRange {
		/// Zero-based column index that was requested.
		column_index: usize,
		/// Total number of columns available in this result row.
		column_count: usize,
	},

	/// A `&str` value destined for a C API that requires NUL-termination
	/// contained an interior NUL byte.
	///
	/// Note: value TEXT bind parameters are length-bound and are therefore
	/// exempt.  This variant is only raised for identifiers, paths, and similar
	/// items passed through `CString` construction.
	NulInterior {
		/// Short description of what the string was being used for
		/// (e.g. `"branch name"`, `"database path"`).
		context: &'static str,
	},

	/// The engine returned a byte sequence where UTF-8 text was expected, but
	/// the bytes were not valid UTF-8.
	Utf8 {
		/// Short description of the column or context where the bad bytes
		/// appeared (e.g. `"schema name column"`).
		context: &'static str,
	},

	/// A byte length destined for a SQLite C API that takes a signed 32-bit
	/// count (`c_int`) exceeded [`i32::MAX`] (about 2 GiB).
	///
	/// Silently truncating the cast would wrap the length to a negative value,
	/// which SQLite interprets as a NUL-terminated string (for text) or as
	/// undefined behaviour (for blobs); this crate refuses the operation with a
	/// typed error instead.
	LengthOverflow {
		/// Short description of the buffer whose length overflowed
		/// (e.g. `"SQL text"`, `"text bind parameter"`, `"blob bind parameter"`).
		context: &'static str,
		/// The offending length in bytes.
		byte_length: usize,
	},

	/// A `dolt_*` SQL function signalled an application-level error (e.g.
	/// "nothing to commit", "merge source not found").
	Dolt {
		/// The Dolt operation that failed (e.g. `"dolt_commit"`).
		operation: String,
		/// The error message text returned by the function.
		message: String,
	},

	/// The supplied branch name is not a legal Dolt branch identifier.
	InvalidBranchName {
		/// The invalid name as supplied by the caller.
		name: String,
		/// Explanation of which validation rule was violated.
		reason: String,
	},

	/// The supplied commit hash is not a valid Dolt commit hash.
	InvalidCommitHash {
		/// The invalid value as supplied by the caller.
		value: String,
		/// Explanation of which validation rule was violated.
		reason: String,
	},

	/// `SQLITE_CORRUPT` or `SQLITE_NOTADB`: the database file is damaged or is
	/// not a SQLite/Dolt database at all.
	Corrupt {
		/// Human-readable message returned by the engine.
		message: String,
	},

	/// `SQLITE_MISUSE` or any other result code not covered by a more specific
	/// variant.  This indicates a bug in this crate or its caller.
	Internal {
		/// Human-readable description of the unexpected condition.
		message: String,
	},
}

impl EngineError {
	/// Construct the most appropriate [`EngineError`] variant for a SQLite
	/// result code.
	///
	/// Maps well-known codes to dedicated variants; everything else becomes
	/// [`EngineError::Sql`].
	pub fn from_sql(primary_code: i32, engine_message: String, sql: &str) -> Self {
		match primary_code {
			c if c == sys::SQLITE_BUSY || c == sys::SQLITE_LOCKED => {
				EngineError::Busy { message: engine_message }
			}
			c if c == sys::SQLITE_CONSTRAINT => {
				EngineError::Constraint { message: engine_message }
			}
			c if c == sys::SQLITE_CORRUPT || c == sys::SQLITE_NOTADB => {
				EngineError::Corrupt { message: engine_message }
			}
			c if c == sys::SQLITE_MISUSE => {
				EngineError::Internal { message: engine_message }
			}
			_ => EngineError::Sql {
				primary_code,
				extended_message: engine_message,
				sql: sql.to_string(),
			},
		}
	}
}

impl std::fmt::Display for EngineError {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			EngineError::CannotOpen { path, message } => {
				write!(formatter, "cannot open database at {path:?}: {message}")
			}
			EngineError::Sql { primary_code, extended_message, sql } => {
				write!(
					formatter,
					"SQL error (code {primary_code}): {extended_message} — in statement: {sql}"
				)
			}
			EngineError::Busy { message } => {
				write!(formatter, "database is busy or locked: {message}")
			}
			EngineError::Constraint { message } => {
				write!(formatter, "constraint violation: {message}")
			}
			EngineError::TypeMismatch { column_index, expected, actual } => {
				write!(
					formatter,
					"type mismatch at column {column_index}: expected {expected}, got {actual}"
				)
			}
			EngineError::ColumnIndexOutOfRange { column_index, column_count } => {
				write!(
					formatter,
					"column index {column_index} is out of range; statement returned {column_count} column(s)"
				)
			}
			EngineError::NulInterior { context } => {
				write!(formatter, "interior NUL byte in {context}")
			}
			EngineError::Utf8 { context } => {
				write!(formatter, "non-UTF-8 bytes in {context}")
			}
			EngineError::LengthOverflow { context, byte_length } => {
				write!(
					formatter,
					"{context} is {byte_length} bytes, which exceeds the {} byte limit of the engine's 32-bit length API",
					i32::MAX
				)
			}
			EngineError::Dolt { operation, message } => {
				write!(formatter, "dolt operation {operation:?} failed: {message}")
			}
			EngineError::InvalidBranchName { name, reason } => {
				write!(formatter, "invalid branch name {name:?}: {reason}")
			}
			EngineError::InvalidCommitHash { value, reason } => {
				write!(formatter, "invalid commit hash {value:?}: {reason}")
			}
			EngineError::Corrupt { message } => {
				write!(formatter, "database is corrupt or not a valid database: {message}")
			}
			EngineError::Internal { message } => {
				write!(formatter, "internal engine error (misuse or unexpected result code): {message}")
			}
		}
	}
}

impl std::error::Error for EngineError {}

/// Convert a Rust byte length into the signed 32-bit count SQLite's C API
/// expects, refusing lengths that would wrap to a negative value.
///
/// SQLite's `sqlite3_prepare_v2`, `sqlite3_bind_text`, and `sqlite3_bind_blob`
/// all take the byte length as a `c_int`. A value above [`i32::MAX`] would wrap
/// to a negative number, which the engine reinterprets (NUL-termination for
/// text, undefined behaviour for blobs) — a truncation or memory-safety hazard.
/// This helper turns that case into a typed [`EngineError::LengthOverflow`].
pub(crate) fn byte_length_to_c_int(
	byte_length: usize,
	context: &'static str,
) -> Result<core::ffi::c_int, EngineError> {
	if byte_length > i32::MAX as usize {
		return Err(EngineError::LengthOverflow { context, byte_length });
	}
	Ok(byte_length as core::ffi::c_int)
}

#[cfg(test)]
mod tests {
	use super::{byte_length_to_c_int, EngineError};

	#[test]
	fn byte_length_within_range_passes_through() {
		assert_eq!(byte_length_to_c_int(0, "x").unwrap(), 0);
		assert_eq!(byte_length_to_c_int(1_024, "x").unwrap(), 1_024);
		assert_eq!(
			byte_length_to_c_int(i32::MAX as usize, "x").unwrap(),
			i32::MAX
		);
	}

	#[test]
	fn byte_length_above_i32_max_is_a_typed_error() {
		let over = i32::MAX as usize + 1;
		match byte_length_to_c_int(over, "blob bind parameter") {
			Err(EngineError::LengthOverflow { context, byte_length }) => {
				assert_eq!(context, "blob bind parameter");
				assert_eq!(byte_length, over);
			}
			other => panic!("expected LengthOverflow, got {other:?}"),
		}
	}

	#[test]
	fn byte_length_overflow_never_wraps_to_negative() {
		// The historic bug: `(len as c_int)` on a >2 GiB length yields a negative
		// count. Assert the helper rejects rather than producing such a value.
		for over in [
			i32::MAX as usize + 1,
			u32::MAX as usize,
			usize::from(u16::MAX) << 20,
		] {
			assert!(byte_length_to_c_int(over, "x").is_err());
		}
	}
}
