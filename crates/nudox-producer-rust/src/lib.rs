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

use std::collections::HashMap;

use nudox_ir::{
    apply::PristineIntroTable,
    body::Language,
    change::{IntroId, PackageLineageId, StableRef},
    entry::EntryInner,
    kind::Kind,
    lower::Lowering,
    package::PackageId,
    vocab::{Confidence, Occurrence, ReferenceKind, RelSpan},
};
use nudox_producer::{PackageSource, Producer, ProducerError, ProducerId};

use crate::ra::ctx::{PendingOcc, PendingTarget};

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
        out: &mut Lowering<RaId>,
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

// ── produce_with_occurrences ──────────────────────────────────────────────────

/// Run the Rust producer end-to-end and also resolve occurrence facts.
///
/// This is a Rust-producer-specific extension to the generic [`produce`]
/// function.  In addition to the sealed [`PristineIntroTable`], it returns a
/// `Vec<(IntroId, Occurrence)>` that the caller can feed to
/// [`IrView::add_occurrence`] to populate the forward-reference map.
///
/// # How occurrences are collected
///
/// During `lower_workspace`, the [`LowerCtx`] accumulates pre-seal occurrence
/// facts (`PendingOcc` values).  Each fact records the owner's `RaId`, the
/// target's canonical path, the `ReferenceKind`, and an optional `RelSpan`.
///
/// After sealing, we build a canonical-path → `IntroId` reverse map by
/// reconstructing each entry's path from the parent chain.  Same-package
/// targets are resolved to `IntroId`; cross-package targets become
/// [`StableRef::Foreign`]-based occurrences (future work — currently skipped
/// because the foreign package's `PackageLineageId` is not known here).
///
/// Confidence is always [`Confidence::Oracle`] — every fact is backed by
/// rust-analyzer's full type-inference pass.
///
/// [`produce`]: nudox_producer::produce
/// [`IrView::add_occurrence`]: nudox_ir::view::IrView::add_occurrence
pub fn produce_with_occurrences(
    producer: &RustProducer,
    src: &PackageSource,
    lineage: &PackageLineageId,
) -> Result<(PristineIntroTable, Vec<(IntroId, Occurrence)>), ProducerError> {
    let oracle = producer.invoke(src)?;

    let pkg_id = PackageId::path(src.root());
    let root_sym = nudox_ir::entry::Symbol {
        name: src.name.as_str().to_owned(),
        visibility: nudox_ir::entry::Visibility::Public,
        documentation: String::new(),
        source: src.root.clone(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };

    let mut sink: Lowering<RaId> = Lowering::new(pkg_id, root_sym);

    let pending_occs = ra::lower_workspace_with_occs(&oracle, &oracle.package_name, &mut sink)
        .map_err(|e| ProducerError::UnsupportedConstruct {
            package: oracle.package_name.clone(),
            symbol: String::new(),
            description: e.to_string(),
        })?;

    let ir_package = sink.finish().map_err(|err| ProducerError::LoweringFailed {
        package: src.name.as_str().to_owned(),
        detail: err.to_string(),
    })?;

    let table = ir_package.seal(lineage);

    // Resolve pending occurrences against the sealed table.
    let resolved = resolve_occurrences(&table, lineage, pending_occs);

    Ok((table, resolved))
}

// ── Occurrence resolution helpers ─────────────────────────────────────────────

/// Build a `canonical_path_with_ns_tag → IntroId` reverse map from a sealed
/// [`PristineIntroTable`].
///
/// The map reconstructs each entry's canonical path by walking up the parent
/// chain, then applies the same namespace tag (`!v` for functions/consts/statics,
/// `!m` for macros, bare for everything else) that [`item::id_of`] uses during
/// the lowering walk.  This ensures a `RaId` like `axum::serve!v` maps to the
/// correct `IntroId` for the `serve` function.
fn build_path_map(table: &PristineIntroTable) -> HashMap<String, IntroId> {
    let mut map = HashMap::with_capacity(table.len());

    for (intro, entry) in table.iter() {
        // Reconstruct the canonical path by walking up the parent chain.
        let mut parts: Vec<String> = vec![entry.sym().name.clone()];
        let mut cur = intro;
        while let Some(parent_id) = table.parent_of(cur) {
            match table.get(parent_id) {
                Some(parent_entry) => {
                    parts.push(parent_entry.sym().name.clone());
                    cur = parent_id;
                }
                None => break,
            }
        }
        parts.reverse();

        // Apply the namespace tag based on the entry's kind.
        let ns_tag = match entry.kind() {
            EntryInner::Owned(Kind::Function(_)) => "!v",
            EntryInner::Owned(Kind::Const(_)) => "!v",
            EntryInner::Owned(Kind::Static(_)) => "!v",
            // Macros are lowered as Module entries with !m; we cannot distinguish
            // them from regular modules in the sealed table, so skip the tag.
            _ => "",
        };

        let path = if ns_tag.is_empty() {
            parts.join("::")
        } else {
            format!("{}{}",  parts.join("::"), ns_tag)
        };

        // If multiple entries have the same reconstructed path (e.g. two impls
        // both contribute a member with the same path after sealing strips the
        // impl skeleton), the first wins.  This is conservative.
        map.entry(path).or_insert(intro);
    }

    map
}

/// Resolve `pending_occs` into concrete `(owner IntroId, Occurrence)` pairs.
///
/// Cross-package targets are currently skipped (their `PackageLineageId` is not
/// available here; a future pass can add them via the foreign package's registry
/// lookup).
fn resolve_occurrences(
    table: &PristineIntroTable,
    _lineage: &PackageLineageId,
    pending_occs: Vec<PendingOcc>,
) -> Vec<(IntroId, Occurrence)> {
    if pending_occs.is_empty() {
        return Vec::new();
    }

    let path_map = build_path_map(table);
    let mut resolved = Vec::with_capacity(pending_occs.len());

    for occ in pending_occs {
        // Resolve the owner.
        let Some(&owner_intro) = path_map.get(occ.owner.as_str()) else {
            // Owner was not lowered (e.g. a private function filtered by
            // document_private=false).  Skip.
            continue;
        };

        // Resolve the target.
        match &occ.target {
            PendingTarget::Local(target_ra_id) => {
                let Some(&target_intro) = path_map.get(target_ra_id.as_str()) else {
                    // Target was not in the local table (e.g. filtered out).
                    continue;
                };
                let stable_ref = StableRef::new(_lineage.clone(), target_intro);
                let occurrence = Occurrence::new(
                    stable_ref,
                    occ.kind,
                    Confidence::Oracle,
                    occ.span.unwrap_or(RelSpan::new(0, 0)),
                );
                resolved.push((owner_intro, occurrence));
            }
            PendingTarget::Foreign(_path) => {
                // Cross-package targets require the foreign package's lineage id.
                // This is not available here; skip for now.
                // TODO: look up the foreign package's lineage via the registry.
            }
        }
    }

    resolved
}
