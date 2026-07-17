//! # nudox-ir-vcs — libpijul-backed IR versioning
//!
//! This crate deliberately does **not** reimplement patch theory. It maps a
//! package's materialized IR onto a **file-per-[`IntroId`](nudox_change::IntroId)**
//! working tree — one file `symbols/{intro_hex}` per symbol introduction — and
//! lets **libpijul** be the change engine: libpijul computes content-addressed
//! changes, their dependencies, commutation, `unrecord`, channel membership, the
//! tip Merkle state, and durable storage.
//!
//! File-per-`IntroId` is the load-bearing choice: pijul's file-level
//! independence lines up with IR symbol independence, so pijul's commutation
//! matches IR semantics (two edits to different symbols are different files →
//! they commute, exactly as two independent symbol edits should).
//!
//! - [`repo::IrRepository`] wraps a libpijul pristine + changestore + working
//!   copy + channel.
//! - [`repo::IrRepository::record_generation`] serializes a new IR state into the
//!   working tree (as **textual** per-symbol [`blob`]s, so libpijul diffs at the
//!   field/param level) and records+applies it as one libpijul change.
//! - [`repo::IrRepository::materialize`] outputs the channel and reads the tree
//!   back into a [`nudox_ir::PristineIntroTable`] container via a borrow-based
//!   [`blob::SymbolView`] scan.
//! - [`repo::IrRepository::materialize_index_incremental`] rebuilds only the
//!   symbols that changed since a prior tip; [`repo::IrRepository::checkout_symbol`]
//!   fetches a single symbol.
//! - [`repo::IrRepository::seal`] materializes then seals a
//!   [`nudox_ir_archive`] serve snapshot.

pub mod blob;
pub mod checkout;
pub mod error;
pub mod repo;
pub mod serialize;

#[cfg(test)]
mod tests;

pub use blob::{serialize_symbol_blob, BlobError, LinkView, SymbolView};
pub use checkout::MaterializedIndex;
pub use error::VcsError;
pub use repo::{ChangeHashHex, IrRepository, IrTip};
pub use serialize::{intro_hex_of, is_symbol_path, symbol_path, LinkWire};
