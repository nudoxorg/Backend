//! Root-bound Turso projection for local query acceleration.
//!
//! The versioned workspace remains the authority. This crate owns a disposable
//! SQL projection whose sole mutable row set is bound to one immutable
//! [`backend_library::ViewRoot`].
//! Hot transitions update only changed rows in one transaction; cold recovery can
//! rebuild the projection from a certified root.

#![deny(unsafe_code)]

mod connection;
mod error;
mod graph;
mod read;
mod schema;
mod writer;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use error::ProjectionError;
pub use graph::{PackageGraphState, RootedPackageGraph};
pub use read::RootedRows;

use std::fmt;

/// Durable path used by the product composition.
pub const FILE_NAME: &str = "projection.turso";

/// Result of aligning the projection to an immutable view root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionUpdate {
    /// The database already named this exact root; no SQL mutation ran.
    Reused {
        /// Number of rows retained without work.
        rows: u64,
    },
    /// A cold or stale database was rebuilt from the supplied root.
    Rebuilt {
        /// Rows materialized from the root.
        rows: u64,
    },
    /// One checked transition advanced the existing projection.
    Advanced {
        /// Rows inserted, replaced, or removed by the transition.
        changed_rows: u64,
    },
}

/// One process-owned Turso accelerator.
///
/// The database handle keeps the process alive while the connection is borrowed
/// by one bounded writer transaction or one read snapshot at a time.
pub struct TursoProjection {
    pub(crate) _database: turso::Database,
    pub(crate) connection: turso::Connection,
}

impl fmt::Debug for TursoProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TursoProjection")
            .finish_non_exhaustive()
    }
}
