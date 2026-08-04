//! Raw, hand-written FFI declarations for the vendored DoltLite engine.
//!
//! DoltLite exposes the standard SQLite C API under its own name; every symbol
//! here is a `sqlite3_*` function compiled into the static library by `build.rs`.
//! We declare only the subset the catalog binding needs (open, prepare, bind,
//! step, column, finalize, close, plus a handful of helpers) rather than pulling
//! in a build-time `bindgen` dependency.
//!
//! Everything in this module is `unsafe` by nature; the safe wrappers in the
//! sibling modules are the only supported entry points.
//!
//! `dead_code` is allowed here on purpose: this module declares the stable
//! result-code space and a coherent slice of the SQLite C API for documentation
//! and forward use, even where the current safe layer does not yet call every
//! symbol.
#![allow(non_camel_case_types, dead_code)]

use core::ffi::{c_char, c_int, c_void};

/// Opaque database-connection handle (`sqlite3 *`).
#[repr(C)]
pub struct sqlite3 {
    _opaque: [u8; 0],
}

/// Opaque prepared-statement handle (`sqlite3_stmt *`).
#[repr(C)]
pub struct sqlite3_stmt {
    _opaque: [u8; 0],
}

/// Destructor sentinel type used by the text/blob binding functions.
pub type sqlite3_destructor_type = Option<unsafe extern "C" fn(*mut c_void)>;

// --- Result codes (primary) ------------------------------------------------

/// Operation succeeded.
pub const SQLITE_OK: c_int = 0;
/// Generic error (details available from `sqlite3_errmsg`).
pub const SQLITE_ERROR: c_int = 1;
/// The database file is locked.
pub const SQLITE_BUSY: c_int = 5;
/// A table in the database is locked.
pub const SQLITE_LOCKED: c_int = 6;
/// A memory allocation failed.
pub const SQLITE_NOMEM: c_int = 7;
/// Attempt to write to a read-only database.
pub const SQLITE_READONLY: c_int = 8;
/// A disk I/O error occurred.
pub const SQLITE_IOERR: c_int = 10;
/// The database disk image is malformed.
pub const SQLITE_CORRUPT: c_int = 11;
/// Insertion failed because the database is full.
pub const SQLITE_FULL: c_int = 13;
/// Unable to open the database file.
pub const SQLITE_CANTOPEN: c_int = 14;
/// The database schema changed underneath a prepared statement.
pub const SQLITE_SCHEMA: c_int = 17;
/// Abort due to a constraint violation.
pub const SQLITE_CONSTRAINT: c_int = 19;
/// The library was used incorrectly (a caller-side bug).
pub const SQLITE_MISUSE: c_int = 21;
/// A bind index was out of range.
pub const SQLITE_RANGE: c_int = 25;
/// A file was opened that is not a database file.
pub const SQLITE_NOTADB: c_int = 26;
/// `sqlite3_step` produced another result row.
pub const SQLITE_ROW: c_int = 100;
/// `sqlite3_step` finished executing the statement.
pub const SQLITE_DONE: c_int = 101;

// --- Fundamental datatypes (column / value types) --------------------------

/// 64-bit signed integer column value.
pub const SQLITE_INTEGER: c_int = 1;
/// IEEE floating-point column value.
pub const SQLITE_FLOAT: c_int = 2;
/// UTF-8 text column value.
pub const SQLITE_TEXT: c_int = 3;
/// Binary blob column value.
pub const SQLITE_BLOB: c_int = 4;
/// SQL NULL column value.
pub const SQLITE_NULL: c_int = 5;

// --- Text encodings --------------------------------------------------------

/// UTF-8 text encoding selector.
pub const SQLITE_UTF8: c_int = 1;

// --- open_v2 flags ---------------------------------------------------------

/// Open the database for reading and writing.
pub const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
/// Create the database if it does not already exist.
pub const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
/// The connection uses a full mutex (safe for use across threads serially).
pub const SQLITE_OPEN_FULLMUTEX: c_int = 0x0001_0000;

/// Destructor sentinel: the bound bytes are static and must not be copied.
///
/// SQLite's public header defines this as the null destructor pointer.
pub const SQLITE_STATIC: sqlite3_destructor_type = None;

/// Destructor sentinel: SQLite must make its own private copy of the bound bytes.
///
/// The C header spells this `((sqlite3_destructor_type)-1)`. We reconstruct the
/// same non-null sentinel pointer value.
///
/// # Safety
/// The returned pointer is never dereferenced by SQLite; it is compared against
/// the reserved `-1` sentinel to decide the copy behaviour.
#[inline]
pub fn sqlite_transient() -> sqlite3_destructor_type {
    // SAFETY: transmuting the reserved -1 sentinel into the destructor pointer
    // type reproduces the exact `SQLITE_TRANSIENT` macro from `doltlite.h`. The
    // value is a sentinel and is never invoked or dereferenced by the engine.
    unsafe { core::mem::transmute::<isize, sqlite3_destructor_type>(-1) }
}

unsafe extern "C" {
    /// Open (or create) a database connection with explicit flags and VFS.
    pub fn sqlite3_open_v2(
        filename: *const c_char,
        db_handle_out: *mut *mut sqlite3,
        flags: c_int,
        vfs_name: *const c_char,
    ) -> c_int;

    /// Close a database connection, finalizing any dangling statements.
    pub fn sqlite3_close_v2(db: *mut sqlite3) -> c_int;

    /// Compile the next SQL statement from a UTF-8 text buffer.
    pub fn sqlite3_prepare_v2(
        db: *mut sqlite3,
        sql: *const c_char,
        sql_byte_length: c_int,
        statement_out: *mut *mut sqlite3_stmt,
        sql_tail_out: *mut *const c_char,
    ) -> c_int;

    /// Advance a prepared statement to the next row (or completion).
    pub fn sqlite3_step(statement: *mut sqlite3_stmt) -> c_int;

    /// Reset a prepared statement so it can be re-executed.
    pub fn sqlite3_reset(statement: *mut sqlite3_stmt) -> c_int;

    /// Clear all parameter bindings on a prepared statement.
    pub fn sqlite3_clear_bindings(statement: *mut sqlite3_stmt) -> c_int;

    /// Destroy a prepared statement.
    pub fn sqlite3_finalize(statement: *mut sqlite3_stmt) -> c_int;

    /// Number of columns in the current result row.
    pub fn sqlite3_column_count(statement: *mut sqlite3_stmt) -> c_int;

    /// Fundamental datatype of a result column in the current row.
    pub fn sqlite3_column_type(statement: *mut sqlite3_stmt, column_index: c_int) -> c_int;

    /// Read a result column as a 64-bit signed integer.
    pub fn sqlite3_column_int64(statement: *mut sqlite3_stmt, column_index: c_int) -> i64;

    /// Read a result column as an IEEE double.
    pub fn sqlite3_column_double(statement: *mut sqlite3_stmt, column_index: c_int) -> f64;

    /// Read a result column as a UTF-8 text pointer (not necessarily NUL-safe).
    pub fn sqlite3_column_text(statement: *mut sqlite3_stmt, column_index: c_int) -> *const u8;

    /// Read a result column as a raw blob pointer.
    pub fn sqlite3_column_blob(statement: *mut sqlite3_stmt, column_index: c_int) -> *const c_void;

    /// Byte length of the current text/blob column value.
    pub fn sqlite3_column_bytes(statement: *mut sqlite3_stmt, column_index: c_int) -> c_int;

    /// Bind a NULL to a `?`-parameter (1-based index).
    pub fn sqlite3_bind_null(statement: *mut sqlite3_stmt, parameter_index: c_int) -> c_int;

    /// Bind a 64-bit signed integer to a parameter.
    pub fn sqlite3_bind_int64(
        statement: *mut sqlite3_stmt,
        parameter_index: c_int,
        value: i64,
    ) -> c_int;

    /// Bind an IEEE double to a parameter.
    pub fn sqlite3_bind_double(
        statement: *mut sqlite3_stmt,
        parameter_index: c_int,
        value: f64,
    ) -> c_int;

    /// Bind a UTF-8 text buffer to a parameter with an explicit byte length.
    ///
    /// The explicit length is what makes embedded NUL bytes representable.
    pub fn sqlite3_bind_text(
        statement: *mut sqlite3_stmt,
        parameter_index: c_int,
        text: *const c_char,
        text_byte_length: c_int,
        destructor: sqlite3_destructor_type,
    ) -> c_int;

    /// Bind a raw blob buffer to a parameter with an explicit byte length.
    pub fn sqlite3_bind_blob(
        statement: *mut sqlite3_stmt,
        parameter_index: c_int,
        blob: *const c_void,
        blob_byte_length: c_int,
        destructor: sqlite3_destructor_type,
    ) -> c_int;

    /// Number of rows changed by the most recent statement on this connection.
    pub fn sqlite3_changes(db: *mut sqlite3) -> c_int;

    /// The primary result code of the most recent failed API call.
    pub fn sqlite3_extended_errcode(db: *mut sqlite3) -> c_int;

    /// A human-readable message for the most recent error on this connection.
    pub fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;

    /// The English-language text describing a primary result code.
    pub fn sqlite3_errstr(result_code: c_int) -> *const c_char;

    /// Free memory previously returned by the SQLite allocator.
    pub fn sqlite3_free(pointer: *mut c_void);
}
