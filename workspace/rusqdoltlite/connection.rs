//! The catalog connection.
//!
//! [`Connection`] is the safe, `rusqlite`-shaped entry point over a DoltLite
//! database. It owns a `sqlite3 *` handle and exposes only the subset the
//! versioned catalog needs: parameterized `execute`, row-mapping `query_rows`,
//! and transaction scoping. Dolt version-control operations live on the
//! [`crate::DoltConnectionExtension`] trait implemented for this type.

use core::ffi::{c_char, c_int};
use core::ptr;

use crate::error::EngineError;
use crate::row::Row;
use crate::statement::PreparedStatement;
use crate::sys;
use crate::transaction::Transaction;
use crate::value::Value;

/// An open connection to a DoltLite (prolly-tree, versioned SQLite) database.
///
/// The connection is opened in serialized threading mode, so it is `Send` and may
/// migrate between threads; it is **not** `Sync` (concurrent use from two threads
/// at once is a caller bug the catalog's single-writer discipline avoids).
pub struct Connection {
	/// The live database handle; non-null until `Drop`.
	database: *mut sys::sqlite3,
}

// SAFETY: the connection is opened with SQLITE_OPEN_FULLMUTEX (serialized mode),
// so the handle may be moved to and used from another thread. It is deliberately
// not `Sync`: two threads must not call into the same handle simultaneously.
unsafe impl Send for Connection {}

impl Connection {
	/// Open (creating if necessary) a database file at `path`.
	///
	/// The parent directory must already exist. A DoltLite database initializes an
	/// empty commit graph on first open ("Initialize data repository").
	pub fn open(path: &str) -> Result<Self, EngineError> {
		let flags =
			sys::SQLITE_OPEN_READWRITE | sys::SQLITE_OPEN_CREATE | sys::SQLITE_OPEN_FULLMUTEX;
		Self::open_with_flags(path, flags)
	}

	/// Open a private, in-memory database that is discarded on close.
	///
	/// Useful for the Embedded profile's tests and ephemeral catalogs.
	pub fn open_in_memory() -> Result<Self, EngineError> {
		let flags =
			sys::SQLITE_OPEN_READWRITE | sys::SQLITE_OPEN_CREATE | sys::SQLITE_OPEN_FULLMUTEX;
		Self::open_with_flags(":memory:", flags)
	}

	/// Shared open path: translate the filename to a C string and call open_v2.
	fn open_with_flags(path: &str, flags: c_int) -> Result<Self, EngineError> {
		let filename = c_string(path, "Connection::open path")?;
		let mut database: *mut sys::sqlite3 = ptr::null_mut();
		// SAFETY: `filename` is a valid NUL-terminated C string for the duration of
		// the call; `database` receives the new handle. We pass a null VFS name to
		// select the default VFS.
		let result_code = unsafe {
			sys::sqlite3_open_v2(filename.as_ptr(), &mut database, flags, ptr::null())
		};
		if result_code != sys::SQLITE_OK {
			// Even on failure SQLite may hand back a handle carrying the message.
			let message = if database.is_null() {
				engine_result_text(result_code)
			} else {
				// SAFETY: `database` is non-null and live enough to read its error.
				let text = unsafe { last_error_message(database) };
				// SAFETY: close the partially-opened handle exactly once.
				unsafe {
					sys::sqlite3_close_v2(database);
				}
				text
			};
			return Err(EngineError::CannotOpen {
				path: path.to_owned(),
				message,
			});
		}
		Ok(Self { database })
	}

	/// Execute a non-query statement, returning the number of rows changed.
	///
	/// Parameters are bound positionally to `?` placeholders in `sql`.
	pub fn execute(&self, sql: &str, parameters: &[Value]) -> Result<usize, EngineError> {
		// SAFETY: `self.database` is live for the whole call.
		let mut statement = unsafe { PreparedStatement::prepare(self.database, sql)? };
		statement.bind_all(parameters)?;
		statement.execute()
	}

	/// Run a query and map every result row into a `T`.
	///
	/// The `map` closure receives a borrowed [`Row`] valid only for that call.
	pub fn query_rows<T>(
		&self,
		sql: &str,
		parameters: &[Value],
		map: impl FnMut(&Row<'_>) -> Result<T, EngineError>,
	) -> Result<Vec<T>, EngineError> {
		// SAFETY: `self.database` is live for the whole call.
		let mut statement = unsafe { PreparedStatement::prepare(self.database, sql)? };
		statement.bind_all(parameters)?;
		statement.query_map(map)
	}

	/// Begin a transaction, borrowing the connection exclusively for its scope.
	///
	/// The returned [`Transaction`] rolls back on drop unless
	/// [`Transaction::commit`] is called.
	pub fn transaction(&mut self) -> Result<Transaction<'_>, EngineError> {
		Transaction::begin(self)
	}

	/// Convenience: run a query expected to yield exactly one text scalar.
	///
	/// Used by the Dolt extension for functions such as `dolt_commit`, which
	/// return a single-column, single-row result carrying a hash or a message.
	pub(crate) fn query_single_text(
		&self,
		sql: &str,
		parameters: &[Value],
	) -> Result<Option<String>, EngineError> {
		let mut rows =
			self.query_rows(sql, parameters, |row| row.get_optional_text(0))?;
		Ok(rows.drain(..).next().flatten())
	}
}

impl Drop for Connection {
	fn drop(&mut self) {
		// SAFETY: `database` was opened by `sqlite3_open_v2` and is closed exactly
		// once here. `sqlite3_close_v2` finalizes any statements that outlived us.
		unsafe {
			sys::sqlite3_close_v2(self.database);
		}
	}
}

/// Build a NUL-terminated C string, rejecting interior NUL bytes.
fn c_string(value: &str, context: &'static str) -> Result<std::ffi::CString, EngineError> {
	std::ffi::CString::new(value).map_err(|_| EngineError::NulInterior { context })
}

/// The static English text for a SQLite primary result code.
fn engine_result_text(result_code: c_int) -> String {
	// SAFETY: `sqlite3_errstr` accepts any integer and returns a static string.
	unsafe {
		let pointer = sys::sqlite3_errstr(result_code);
		if pointer.is_null() {
			return String::from("(unknown error)");
		}
		core::ffi::CStr::from_ptr(pointer)
			.to_string_lossy()
			.into_owned()
	}
}

/// Read the most recent error message from a live connection handle.
///
/// # Safety
/// `database` must be a live connection handle.
unsafe fn last_error_message(database: *mut sys::sqlite3) -> String {
	// SAFETY: `database` is live per the contract; the returned pointer is owned by
	// the connection and valid until the next API call.
	unsafe {
		let pointer = sys::sqlite3_errmsg(database);
		if pointer.is_null() {
			return String::from("(no error message)");
		}
		core::ffi::CStr::from_ptr(pointer as *const c_char)
			.to_string_lossy()
			.into_owned()
	}
}
