//! # nudox-ir-vcs — libpijul-backed IR versioning
//!
//! This crate deliberately does **not** reimplement patch theory. It maps a
//! package's materialized IR onto a **file-per-[`IntroId`](nudox_ir::change::IntroId)**
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
//!   [`archive`] serve snapshot.

// ── Folded-in IR data planes (formerly sibling crates) ──────────────────────
/// Sealed, mmap-able IR-only `PackageArchive` (formerly the `nudox-ir-archive`
/// crate): header/index/seal/section/view.
pub mod archive;
/// Matcher-free structural delta over IR tables (formerly `nudox-ir-diff`):
/// `PackageDelta`, `IrOp`, diff/apply.
pub mod diff;
/// IR-only API-surface projection + semver classification (formerly
/// `nudox-semver`): surface/classify/report/law-packs.
pub mod semver;
/// Guest→host IR streaming protocol (formerly the `ir-stream` crate): the frame
/// grammar + sink/receiver state machines. The host-side driver is [`stream`].
pub mod protocol;

pub mod ascii;
pub mod blob;
pub mod checkout;
pub mod checkpoint;
pub mod continuity;
pub mod error;
pub mod f1;
pub mod refs;
pub mod repo;
pub mod serialize;
pub mod serve_cache;
pub mod session;
pub mod stream;
pub mod subst;
pub mod version;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ref_probes;

#[cfg(test)]
mod size_tests;

#[cfg(test)]
mod record_shape_tests;

pub use ascii::AsciiError;
pub use blob::{serialize_symbol_blob, BlobError, LinkView, SymbolView};
pub use checkout::MaterializedIndex;
pub use checkpoint::{
    Checkpoint, CheckpointCache, CheckpointConfig, Retention, ServeStrategy, Served,
};
pub use continuity::compute_sigma;
pub use error::VcsError;
pub use f1::{
    compute_api_surface_hash, serialize_f1, ContinuityOp, ContinuitySummary, F1Error, F1View,
    RenameEdge,
};
pub use refs::{BranchName, Ref, RefKind, ResolvedRef, TagName};
pub use repo::{ChangeHashHex, IrRepository, IrTip, VersionDiff};
pub use serialize::{intro_hex_of, is_symbol_path, symbol_path, LinkWire};
pub use serve_cache::{ServeCache, ServeSource, ServedArchive};
pub use session::{FinishReport, GenerationMeta, RecordingSession, StageReport, StagedEntry};
pub use stream::{record_stream, ProgressSnapshot, SourceDigestEntry, StreamPolicy, StreamedRecording};
pub use version::{VersionLabel, VersionState};
