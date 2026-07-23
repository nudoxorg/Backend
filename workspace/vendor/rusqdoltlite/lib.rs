//! `rusqdoltlite` — a safe Rust binding over the vendored DoltLite engine.
//!
//! DoltLite (`workspace/vendor/doltlite`) is a SQLite fork whose B-tree pager is
//! replaced by a content-addressed prolly-tree store, giving a SQL database
//! Git-like version control: `dolt_commit`, `dolt_branch`, `dolt_checkout`,
//! `dolt_merge`, `dolt_gc`, and the `dolt_log` / `dolt_conflicts` virtual tables.
//! This crate is the catalog engine facade for INDEX-PLAN §4 (ID-1) — the only
//! product mode; there is no plain-SQLite fallback.
//!
//! ## Surface
//! - [`Connection`] — open a database, [`Connection::execute`] parameterized
//!   writes, [`Connection::query_rows`] mapping reads, and
//!   [`Connection::transaction`] scoping.
//! - [`Value`] / [`Row`] — the bind-parameter and typed-column vocabulary.
//! - [`DoltConnectionExtension`] — the version-control operations, returning the
//!   validated newtypes [`CommitHash`] and [`BranchName`] and the typed
//!   [`MergeOutcome`].
//! - [`EngineError`] — the crate's single, structured error taxonomy.
//!
//! The engine is compiled from the vendored C amalgamation by `build.rs`; the raw
//! FFI subset lives in the private [`sys`] module and is never exposed.

mod connection;
mod dolt;
mod dolt_types;
mod error;
mod row;
mod statement;
mod sys;
mod transaction;
mod value;

pub use connection::Connection;
pub use dolt::DoltConnectionExtension;
pub use dolt_types::{BranchName, CommitHash, MergeOutcome};
pub use error::EngineError;
pub use row::Row;
pub use transaction::Transaction;
pub use value::Value;
