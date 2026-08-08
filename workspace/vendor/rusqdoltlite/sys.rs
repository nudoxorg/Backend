//! Raw, hand-written FFI declarations for the vendored DoltLite engine.
//!
//! DoltLite exposes the standard SQLite C API under its own name, so the Rust
//! functions below keep their familiar `sqlite3_*` spelling. **The symbols they
//! bind to do not.** Each carries an explicit
//! `#[link_name = "doltlite_…"]`, and `build.rs` compiles the amalgamation with
//! matching `-Dsqlite3_x=doltlite_x` renames.
//!
//! That indirection is the whole point of this module. Declaring plain
//! `sqlite3_open_v2` with no `#[link]` — which is what this file used to do —
//! means the extern resolves against *any* SQLite in the final binary. `index`
//! links one: `rusqlite`'s `bundled` feature statically embeds a complete stock
//! SQLite. So when the DoltLite amalgamation was absent, every call in this crate
//! quietly went to stock SQLite, the build stayed green, and the "sovereign
//! versioned catalog" ran with no prolly-tree pager and no `dolt_*` functions.
//! Nothing failed until `dolt_commit` reported "no such function" at runtime.
//!
//! `doltlite_open_v2` has exactly one definition in the world: the archive
//! `build.rs` produces. There is nothing else for it to bind to, so the
//! substitution is not merely detected — it is unconstructible.
//!
//! When the amalgamation is *not* present, `build.rs` withholds the
//! `doltlite_engine_linked` cfg and the `doltlite_api!` macro emits diverging
//! Rust stubs instead of externs. Those stubs are unreachable: `Connection` is
//! the sole owner of a `sqlite3 *`, its only constructor is
//! [`Connection::open_with_flags`](crate::Connection), and that constructor
//! returns [`EngineError::EngineNotLinked`](crate::EngineError) before touching
//! the C API. Emitting stubs rather than externs also keeps the crate free of
//! dangling references, so the failure stays a typed error instead of degrading
//! into a linker diagnostic that names neither this crate nor DoltLite.
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

/// Declare the crate's entire C surface once, in a form that cannot drift
/// between the linked and unlinked builds.
///
/// Each entry names the Rust function, its signature, and the **exact** symbol
/// `build.rs` renamed it to. Expands to:
///
/// - under `doltlite_engine_linked`: an `unsafe extern "C"` block bound to
///   `libdoltlite.a`, every item carrying its `#[link_name]`;
/// - otherwise: diverging Rust stubs with identical signatures, so the rest of
///   the crate still type-checks while referencing no C symbol whatsoever.
///
/// One invocation means the two shapes cannot disagree about the surface, and
/// the `link_name` literals sit next to the declarations they apply to instead of
/// in a build script the reader has to go find.
macro_rules! doltlite_api {
    ($(
        $(#[$attribute:meta])*
        fn $name:ident($($argument:ident: $argument_type:ty),* $(,)?) $(-> $return_type:ty)?
            = $symbol:literal;
    )*) => {
        #[cfg(doltlite_engine_linked)]
        #[link(name = "doltlite", kind = "static")]
        unsafe extern "C" {
            $(
                $(#[$attribute])*
                #[link_name = $symbol]
                pub fn $name($($argument: $argument_type),*) $(-> $return_type)?;
            )*
        }

        $(
            $(#[$attribute])*
            ///
            /// # Unreachable in this build
            /// The DoltLite amalgamation was not compiled, so this is a stub.
            /// `Connection::open` refuses before any C call can be made, and it is
            /// the only way to obtain the handle these functions take.
            #[cfg(not(doltlite_engine_linked))]
            pub unsafe fn $name($($argument: $argument_type),*) $(-> $return_type)? {
                let _ = ($($argument,)*);
                unreachable!(
                    "rusqdoltlite: `{}` was called with no DoltLite engine linked. \
                     `Connection::open` is the only constructor of an engine handle and it \
                     returns `EngineError::EngineNotLinked` in this build, so reaching here \
                     means the guard in `connection.rs` was bypassed.",
                    $symbol
                );
            }
        )*
    };
}

doltlite_api! {
    /// Open (or create) a database connection with explicit flags and VFS.
    fn sqlite3_open_v2(
        filename: *const c_char,
        db_handle_out: *mut *mut sqlite3,
        flags: c_int,
        vfs_name: *const c_char,
    ) -> c_int = "doltlite_open_v2";

    /// Close a database connection, finalizing any dangling statements.
    fn sqlite3_close_v2(db: *mut sqlite3) -> c_int = "doltlite_close_v2";

    /// Compile the next SQL statement from a UTF-8 text buffer.
    fn sqlite3_prepare_v2(
        db: *mut sqlite3,
        sql: *const c_char,
        sql_byte_length: c_int,
        statement_out: *mut *mut sqlite3_stmt,
        sql_tail_out: *mut *const c_char,
    ) -> c_int = "doltlite_prepare_v2";

    /// Advance a prepared statement to the next row (or completion).
    fn sqlite3_step(statement: *mut sqlite3_stmt) -> c_int = "doltlite_step";

    /// Reset a prepared statement so it can be re-executed.
    fn sqlite3_reset(statement: *mut sqlite3_stmt) -> c_int = "doltlite_reset";

    /// Clear all parameter bindings on a prepared statement.
    fn sqlite3_clear_bindings(statement: *mut sqlite3_stmt) -> c_int
        = "doltlite_clear_bindings";

    /// Destroy a prepared statement.
    fn sqlite3_finalize(statement: *mut sqlite3_stmt) -> c_int = "doltlite_finalize";

    /// Number of columns in the current result row.
    fn sqlite3_column_count(statement: *mut sqlite3_stmt) -> c_int
        = "doltlite_column_count";

    /// Fundamental datatype of a result column in the current row.
    fn sqlite3_column_type(statement: *mut sqlite3_stmt, column_index: c_int) -> c_int
        = "doltlite_column_type";

    /// Read a result column as a 64-bit signed integer.
    fn sqlite3_column_int64(statement: *mut sqlite3_stmt, column_index: c_int) -> i64
        = "doltlite_column_int64";

    /// Read a result column as an IEEE double.
    fn sqlite3_column_double(statement: *mut sqlite3_stmt, column_index: c_int) -> f64
        = "doltlite_column_double";

    /// Read a result column as a UTF-8 text pointer (not necessarily NUL-safe).
    fn sqlite3_column_text(statement: *mut sqlite3_stmt, column_index: c_int) -> *const u8
        = "doltlite_column_text";

    /// Read a result column as a raw blob pointer.
    fn sqlite3_column_blob(statement: *mut sqlite3_stmt, column_index: c_int) -> *const c_void
        = "doltlite_column_blob";

    /// Byte length of the current text/blob column value.
    fn sqlite3_column_bytes(statement: *mut sqlite3_stmt, column_index: c_int) -> c_int
        = "doltlite_column_bytes";

    /// Bind a NULL to a `?`-parameter (1-based index).
    fn sqlite3_bind_null(statement: *mut sqlite3_stmt, parameter_index: c_int) -> c_int
        = "doltlite_bind_null";

    /// Bind a 64-bit signed integer to a parameter.
    fn sqlite3_bind_int64(
        statement: *mut sqlite3_stmt,
        parameter_index: c_int,
        value: i64,
    ) -> c_int = "doltlite_bind_int64";

    /// Bind an IEEE double to a parameter.
    fn sqlite3_bind_double(
        statement: *mut sqlite3_stmt,
        parameter_index: c_int,
        value: f64,
    ) -> c_int = "doltlite_bind_double";

    /// Bind a UTF-8 text buffer to a parameter with an explicit byte length.
    ///
    /// The explicit length is what makes embedded NUL bytes representable.
    fn sqlite3_bind_text(
        statement: *mut sqlite3_stmt,
        parameter_index: c_int,
        text: *const c_char,
        text_byte_length: c_int,
        destructor: sqlite3_destructor_type,
    ) -> c_int = "doltlite_bind_text";

    /// Bind a raw blob buffer to a parameter with an explicit byte length.
    fn sqlite3_bind_blob(
        statement: *mut sqlite3_stmt,
        parameter_index: c_int,
        blob: *const c_void,
        blob_byte_length: c_int,
        destructor: sqlite3_destructor_type,
    ) -> c_int = "doltlite_bind_blob";

    /// Number of rows changed by the most recent statement on this connection.
    fn sqlite3_changes(db: *mut sqlite3) -> c_int = "doltlite_changes";

    /// The primary result code of the most recent failed API call.
    fn sqlite3_extended_errcode(db: *mut sqlite3) -> c_int
        = "doltlite_extended_errcode";

    /// A human-readable message for the most recent error on this connection.
    fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char = "doltlite_errmsg";

    /// The English-language text describing a primary result code.
    fn sqlite3_errstr(result_code: c_int) -> *const c_char = "doltlite_errstr";

    /// Free memory previously returned by the SQLite allocator.
    fn sqlite3_free(pointer: *mut c_void) = "doltlite_free";

    /// The engine's library version string (`"3.54.0"` for DoltLite 0.11.x).
    ///
    /// Reported in [`EngineError::NotDoltLite`](crate::EngineError) so a failed
    /// capability probe says *which* engine answered, not merely that one did.
    fn sqlite3_libversion() -> *const c_char = "doltlite_libversion";
}
