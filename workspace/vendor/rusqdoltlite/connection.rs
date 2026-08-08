//! The catalog connection.
//!
//! [`Connection`] is the safe, `rusqlite`-shaped entry point over a DoltLite
//! database. It owns a `sqlite3 *` handle and exposes only the subset the
//! versioned catalog needs: parameterized `execute`, row-mapping `query_rows`,
//! and transaction scoping. Dolt version-control operations live on the
//! [`crate::DoltConnectionExtension`] trait implemented for this type.

use core::ffi::c_int;
// Only the linked build reaches the C API; without an engine the helpers below
// are compiled out along with it, so their imports go too.
#[cfg(doltlite_engine_linked)]
use core::ffi::c_char;
#[cfg(doltlite_engine_linked)]
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

    /// The **only** constructor of a live engine handle.
    ///
    /// Everything else in this crate takes a `sqlite3 *` that came from here, so
    /// the two guards below are the crate's complete admission control:
    ///
    /// 1. **Build-time.** Without the `doltlite_engine_linked` cfg — which
    ///    `build.rs` emits only after compiling and archiving a real
    ///    amalgamation — this returns
    ///    [`EngineError::EngineNotLinked`] and never reaches the C API.
    /// 2. **Runtime.** With the engine linked, the handle still has to *prove* it
    ///    is DoltLite by answering `dolt_version()` before it is handed out
    ///    ([`verify_engine_is_doltlite`](Connection::verify_engine_is_doltlite)).
    ///
    /// The second guard is not redundant. The first trusts the build; the second
    /// interrogates the library that actually answered, and so survives link
    /// order, a substituted archive, or an interposed symbol.
    fn open_with_flags(path: &str, flags: c_int) -> Result<Self, EngineError> {
        Self::open_engine_handle(path, flags)
    }

    /// No amalgamation was compiled into this build, so there is nothing to open.
    ///
    /// This is deliberately a refusal rather than a fallback. Opening would
    /// succeed — the process almost certainly contains a stock SQLite, since
    /// `index` links `rusqlite` with feature `bundled` — and would produce a
    /// catalog that reads and writes correctly while silently keeping no history
    /// at all.
    #[cfg(not(doltlite_engine_linked))]
    fn open_engine_handle(path: &str, _flags: c_int) -> Result<Self, EngineError> {
        Err(EngineError::EngineNotLinked {
            path: path.to_owned(),
            expected_amalgamation: env!("RUSQDOLTLITE_AMALGAMATION_PATH"),
        })
    }

    /// Translate the filename to a C string, call `doltlite_open_v2`, and admit
    /// the handle only once it has demonstrated DoltLite capabilities.
    #[cfg(doltlite_engine_linked)]
    fn open_engine_handle(path: &str, flags: c_int) -> Result<Self, EngineError> {
        let filename = c_string(path, "Connection::open path")?;
        let mut database: *mut sys::sqlite3 = ptr::null_mut();
        // SAFETY: `filename` is a valid NUL-terminated C string for the duration of
        // the call; `database` receives the new handle. We pass a null VFS name to
        // select the default VFS.
        let result_code =
            unsafe { sys::sqlite3_open_v2(filename.as_ptr(), &mut database, flags, ptr::null()) };
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
        // Constructed before the probe so a rejected handle is closed by `Drop`
        // rather than leaked on the error path.
        let connection = Self { database };
        connection.verify_engine_is_doltlite(path)?;
        Ok(connection)
    }

    /// Require the freshly opened library to answer a DoltLite-only function.
    ///
    /// `dolt_version()` is registered by the prolly engine and by nothing in
    /// stock SQLite, so a successful non-empty answer is *positive* evidence of
    /// the right engine — as opposed to a build flag, which is only evidence
    /// about the build. Stock SQLite answers `no such function: dolt_version`,
    /// which becomes [`EngineError::NotDoltLite`] here, at open time, instead of
    /// surfacing at the first `dolt_commit` an arbitrary distance downstream.
    #[cfg(doltlite_engine_linked)]
    fn verify_engine_is_doltlite(&self, path: &str) -> Result<(), EngineError> {
        let reject = |detail: String| EngineError::NotDoltLite {
            path: path.to_owned(),
            library_version: self.library_version(),
            detail,
        };
        match self.query_single_text("SELECT dolt_version()", &[]) {
            Ok(Some(version)) if !version.is_empty() => Ok(()),
            Ok(Some(_)) => Err(reject(
                "`SELECT dolt_version()` returned an empty string".to_owned(),
            )),
            Ok(None) => Err(reject(
                "`SELECT dolt_version()` returned NULL or no row".to_owned(),
            )),
            Err(error) => Err(reject(format!("`SELECT dolt_version()` failed: {error}"))),
        }
    }

    /// The version string of the library actually answering this connection.
    ///
    /// Reported alongside a failed capability probe so the error identifies the
    /// impostor (`3.46.0` is stock SQLite as bundled by `rusqlite`; DoltLite
    /// 0.11.x reports `3.54.0`).
    #[cfg(doltlite_engine_linked)]
    fn library_version(&self) -> String {
        // SAFETY: `sqlite3_libversion` takes no arguments and returns a pointer to
        // a static, NUL-terminated string owned by the library.
        unsafe {
            let pointer = sys::sqlite3_libversion();
            if pointer.is_null() {
                return String::from("(unreported)");
            }
            core::ffi::CStr::from_ptr(pointer)
                .to_string_lossy()
                .into_owned()
        }
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
        let mut rows = self.query_rows(sql, parameters, |row| row.get_optional_text(0))?;
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
///
/// Reachable only when an engine is linked: otherwise `open_engine_handle`
/// refuses before any string crosses the FFI boundary.
#[cfg(doltlite_engine_linked)]
fn c_string(value: &str, context: &'static str) -> Result<std::ffi::CString, EngineError> {
    std::ffi::CString::new(value).map_err(|_| EngineError::NulInterior { context })
}

/// The static English text for a SQLite primary result code.
#[cfg(doltlite_engine_linked)]
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
#[cfg(doltlite_engine_linked)]
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
