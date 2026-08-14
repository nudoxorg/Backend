//! # nudox-store — L1 of the local-first documentation engine
//!
//! This crate sits at the base of the GUI stack, directly above `nudox-ir`.
//! Its role is to:
//!
//! 1. **Hold** the live corpus of `IrView`s for every loaded package, wrapped
//!    in [`PackageView`] with all derived indexes pre-computed.
//! 2. **Feed** the corpus through the [`IrSource`] trait, which has exactly two
//!    in-tree implementations: [`FixtureSource`] (deterministic, no I/O) and
//!    [`ProducerSource`] (drives the real language producers on the Tokio
//!    blocking pool).
//! 3. **Expose** the [`Corpus`] as a cheap, `Clone`-able handle backed by an
//!    `Arc` so that the engine, the graph adapter, and the MCP layer can all
//!    share one world-view without locking.
//!
//! ## Why no `anyhow` here
//!
//! Every error type in this crate is a `thiserror` enum (§L7.4). `anyhow` is
//! allowed only in `main.rs` and tests; it must never appear in the corpus or
//! source modules so that `EngineError` can branch on error variants rather than
//! inspecting strings.
//!
//! ## Interior-mutability decision
//!
//! [`Corpus`] uses a `tokio::sync::RwLock` for the package map. The alternative
//! was `papaya::HashMap` (lock-free reads). We chose `RwLock` because:
//!
//! * The package map is written **once** during the initial load of a workspace,
//!   then read-only forever (hot-reload adds new packages but never mutates
//!   existing `Arc<PackageView>`s).
//! * A `RwLock` on the whole map lets us return `Arc<PackageView>` clones from
//!   `package()` without lifetime entanglement — the caller gets an owned `Arc`
//!   that outlives the lock guard.
//! * The GUI's critical path (search, chunk, query) takes a read lock that
//!   never contends against other readers; it only blocks during the brief
//!   window when a new package is inserted. This satisfies "reads must never
//!   block writes on the GUI's critical path" because inserts are rare and
//!   bounded in time.
//!
//! If profiling shows lock contention during large workspace loads we can
//! migrate to `papaya` with no API change — `Corpus` is opaque.

pub mod corpus;
pub mod index;
pub mod package;
pub mod source;

/// Re-exports of the most commonly used types across the L1 layer.
///
/// Import this in downstream crates with `use nudox_engine::store::prelude::*`.
pub mod prelude {
    pub use crate::store::{
        corpus::{Corpus, EntryRef},
        package::{PackageIndexes, PackageView, Provenance},
        source::{
            IrSource, LoadEvent, LoadRequest, PackageHint, ProduceStage, SourceDescriptor,
            Error,
        },
    };
}
