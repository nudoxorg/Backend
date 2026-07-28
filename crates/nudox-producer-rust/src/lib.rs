//! Rust language producer for the nudox-ir pipeline.
//!
//! This crate drives rust-analyzer **in-process** via the `ra_ap_*` crates.
//! It implements [`nudox_producer::Producer`] and lowers Rust HIR directly
//! into [`nudox_ir`] via [`nudox_ir::lower::Lowering`].
//!
//! # Architecture
//!
//! ```text
//! invoke()                  lower()
//!   │                          │
//!   ▼                          ▼
//! load(root)           walk crate graph
//!   │                  ├── module_entries
//!   ▼                  ├── functions / ADTs / traits
//! LoadedWorkspace      ├── impls
//!   (owns db+vfs)      └── declare/refer into Lowering<RaId>
//! ```
//!
//! # Oracle lifetime
//!
//! `Self::Oracle = LoadedWorkspace`.  The workspace owns the salsa `RootDatabase`
//! and `Vfs`.  `lower` borrows HIR out of the owned database across its entire
//! call, which is safe because `lower` takes `&Self::Oracle` and the oracle is
//! kept alive by `nudox_producer::produce` until `lower` returns.
//!
//! # Self::Id
//!
//! We use `RaId` — a canonical path string (`SmolStr`) — as the producer-local
//! identity.  A pure salsa id like `hir::Function`'s internal `InFile<...>`
//! is not stable across runs (it encodes `FileId` integers which are assigned
//! during load).  The canonical path string is stable across runs, is already
//! computed for every item, and naturally distinguishes same-named items in
//! different modules.  Overloads with the same name (two `impl` blocks providing
//! `fn new`) get a disambiguating suffix: `path::to::Type::new` plus a `#N`
//! counter that is incremented per (parent-path, name) pair.  See [`RaId`].
//!
//! # Lifetime problem
//!
//! `ra_ap_hir::Type<'db>` borrows from the salsa db with lifetime `'db`.  The
//! `Lowering<RaId>` sink only accepts owned `nudox_ir::kinds::Type` values
//! (no lifetime parameters).  So every HIR type must be fully lowered to an
//! owned `ir::Type` before being handed to `declare`.  This is acceptable
//! because the IR type tree is small compared to the full HIR graph, and the
//! old code already cloned type strings extensively.
//!
//! The salsa `RootDatabase` is not `Send`, not `Sync`, and the `Crate::all`
//! iteration + type resolution must stay on the thread that called `attach_db`.
//! `lower()` therefore runs single-threaded on the calling thread.  That is
//! consistent with the existing `ra::generate_ir` design.

pub mod error;

mod ra;

use nudox_ir::body::Language;
use nudox_producer::{PackageSource, Producer, ProducerError, ProducerId};

pub use self::error::RustProducerError;
pub use self::ra::loaded::LoadedWorkspace;

// ── RaId — producer-local item identity ──────────────────────────────────────

/// Producer-local stable identity for a Rust HIR item.
///
/// A canonical path string such as `"my_crate::Foo::bar"`, optionally suffixed
/// with `#N` when two items share the same canonical path (e.g. two inherent
/// `impl` blocks both providing `fn new`).
///
/// **Why not salsa ids?**  `hir::Function`, `hir::Struct`, etc. wrap
/// `InFile<FunctionId>` which embeds a `FileId` integer assigned during the
/// current load.  Those integers are not stable across distinct invocations of
/// `load()`.  The canonical path is stable.
///
/// **Why `SmolStr`?**  Most Rust paths are short (< 23 bytes) and fit inline.
/// The old code used `SmolStr` for `PathKey` for the same reason.
pub type RaId = smol_str::SmolStr;

// ── RustProducer ─────────────────────────────────────────────────────────────

/// In-process rust-analyzer HIR walk → nudox-ir.
///
/// `invoke` loads the Cargo workspace once and returns a [`LoadedWorkspace`]
/// that owns the salsa database.  `lower` borrows HIR from that database,
/// walks every module of every documented crate, and emits declarations into
/// the [`nudox_ir::lower::Lowering`] sink.
///
/// The `direct_repo` flag controls whether private items and workspace-local
/// library members are included (matching the old `document_private` +
/// workspace-member BFS from `ra::generate_ir`).
/// # Why there is no `name` field
///
/// There used to be one, and `lower` read it. But `lower` is handed only the
/// oracle, so the name had to be configured on the producer *value* before
/// `invoke` — and a [`ProducerRegistry`] holds one producer per *language*,
/// so it cannot know which package it is about to be asked for. It registered
/// `name: String::new()`, an empty name makes `documented_package_names` return
/// nothing, and every package produced through the registry failed with
/// "no documented packages found for ``".
///
/// The name now travels on [`LoadedWorkspace`], taken from the `PackageSource`
/// that `invoke` was given, so it provably describes the package being lowered.
///
/// [`ProducerRegistry`]: nudox_store::source::producer::ProducerRegistry
#[derive(Debug, Clone, Default)]
pub struct RustProducer {
    /// If `true`: document private items + pull in local workspace library deps.
    pub direct_repo: bool,
}

impl Producer for RustProducer {
    type Id = RaId;
    type Oracle = LoadedWorkspace;

    const ID: ProducerId = ProducerId("rust-ra/1");
    const LANGUAGE: Language = Language::Rust;

    fn invoke(&self, src: &PackageSource) -> Result<LoadedWorkspace, ProducerError> {
        ra::load(src, self.direct_repo).map_err(|e| ProducerError::OracleSpawn {
            command: "ra_ap_load_cargo::load_workspace".to_owned(),
            reason: e.to_string(),
        })
    }

    fn lower(
        &self,
        oracle: &LoadedWorkspace,
        out: &mut nudox_ir::lower::Lowering<RaId>,
    ) -> Result<(), ProducerError> {
        ra::lower_workspace(oracle, &oracle.package_name, out).map_err(|e| {
            ProducerError::UnsupportedConstruct {
                package: oracle.package_name.clone(),
                symbol: String::new(),
                description: e.to_string(),
            }
        })
    }
}
