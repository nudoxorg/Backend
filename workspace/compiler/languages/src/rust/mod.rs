//! Rust language producer for the nudox-ir pipeline.
//!
//! This crate drives rust-analyzer **in-process** via the `ra_ap_*` crates.
//! It implements [`crate::Producer`] and lowers Rust HIR directly
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
//! kept alive by `crate::produce` until `lower` returns.
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

mod ra;

pub mod error;

use crate::{PackageSource, Producer, ProducerError, ProducerId};
use nudox_ir::{body::Language, lower::Lowering, vocab::Confidence};
use std::time::Instant;

use crate::rust::ra::ctx::PendingTarget;

pub use self::error::Error;
pub use self::ra::loaded::{
    BuildScriptExecution, BuildScriptFailure, DependencyResolution, LoadCompleteness, LoadProfile,
    LoadedWorkspace, NoDepsFallback, ProcMacroAvailability,
};

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
            reason: std::io::Error::other(e),
        })
    }

    fn lower(
        &self,
        oracle: &LoadedWorkspace,
        out: &mut Lowering<RaId>,
    ) -> Result<(), ProducerError> {
        let started = Instant::now();
        let occurrences = ra::lower_workspace(oracle, &oracle.package_name, out)
            .map_err(|e| producer_error_for(&oracle.package_name, e))?;
        for occurrence in occurrences {
            match occurrence.target {
                PendingTarget::Local(target) => {
                    out.record_occurrence(
                        occurrence.owner,
                        target,
                        occurrence.kind,
                        Confidence::Oracle,
                        occurrence.span,
                    );
                }
                PendingTarget::Foreign(target) => {
                    out.record_foreign_occurrence(
                        occurrence.owner,
                        target,
                        occurrence.kind,
                        Confidence::Oracle,
                        occurrence.span,
                    );
                }
            }
        }
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        tracing::info!(
            phase = "rust_producer_lower",
            package = %oracle.package_name,
            elapsed_ms,
            "rust producer phase complete"
        );
        if cfg!(debug_assertions) {
            eprintln!(
                "rust_producer phase=lower elapsed_ms={elapsed_ms:.1} package={}",
                oracle.package_name
            );
        }
        Ok(())
    }
}

// ── Error mapping across the Producer boundary ────────────────────────────────

/// Translate a [`Error`] into the language-agnostic
/// [`ProducerError`] without destroying its cause.
///
/// [`Error::DependenciesUnresolved`] maps to the `ProducerError`
/// variant of the same meaning, which keeps the `cargo metadata` failure in a
/// `#[source]` slot so `std::error::Error::source` still reaches
/// [`NoDepsFallback::cargo_diagnostic`] — the one string that says which
/// dependency failed and why. It must not become `UnsupportedConstruct`, which
/// flattens its cause into a `description` and ends the chain, nor
/// `LoweringFailed`, whose contract is "the producer has a bug".
///
/// [`Error::BuildScriptsFailed`] maps to
/// [`ProducerError::BuildScriptsFailed`] on the same principle and for a
/// stronger reason: its cause is the only place the build tool's own words
/// survive, and the catch-all below would render it with `to_string()`, whose
/// output for this variant is deliberately about consequences and names no
/// diagnostic at all.
///
/// [`Error::NothingToDocument`] maps to
/// [`ProducerError::NoDeclarationsContributed`] for the same reason in the
/// other direction: that is the *generic* variant `produce`'s yield-contract
/// gate would raise for this run anyway, at the end, with `source: None`
/// because the gate has no cause to offer. Mapping here rather than into the
/// `UnsupportedConstruct` catch-all means the run fails with the same typed
/// variant either way — so a caller matching on it is not sensitive to whether
/// the Rust producer happened to detect the emptiness early — while filling in
/// the `#[source]` the backstop cannot. Anything else would make the early,
/// better-diagnosed path the *less* recognisable one.
///
/// [`Error::ProcMacroDegraded`] maps to [`ProducerError::ProcMacroDegraded`]
/// rather than `UnsupportedConstruct`. A correct `invoke` never constructs that
/// error — unavailability lives on [`LoadCompleteness`] — but if the leftover
/// variant is raised, it must stay a typed environment outcome, not a hard
/// construct failure.
fn producer_error_for(package: &str, err: Error) -> ProducerError {
    match err {
        e @ Error::DependenciesUnresolved { .. } => ProducerError::DependenciesUnresolved {
            package: package.to_owned(),
            source: Box::new(e),
        },
        e @ Error::BuildScriptsFailed { .. } => ProducerError::BuildScriptsFailed {
            package: package.to_owned(),
            source: Box::new(e),
        },
        e @ Error::NothingToDocument { .. } => ProducerError::NoDeclarationsContributed {
            package: package.to_owned(),
            producer: <RustProducer as Producer>::ID,
            source: Some(Box::new(e)),
        },
        Error::ProcMacroDegraded(diagnostic) => ProducerError::ProcMacroDegraded {
            package: package.to_owned(),
            diagnostic,
        },
        other => ProducerError::UnsupportedConstruct {
            package: package.to_owned(),
            symbol: String::new(),
            description: other.to_string(),
        },
    }
}

#[cfg(test)]
mod l51_proc_macro_degraded {
    use super::*;

    /// docs/LIMITATIONS.md L51 / docs/ISSUES.md: `Error::ProcMacroDegraded` is
    /// declared as a soft warning but `producer_error_for`'s `other =>` arm
    /// would mislabel it as `UnsupportedConstruct` — a hard failure of the
    /// whole package. Soft proc-macro unavailability belongs on
    /// `LoadCompleteness`, and this mapping must not flatten it.
    #[test]
    fn proc_macro_degraded_is_not_mapped_to_unsupported_construct() {
        let err = producer_error_for(
            "demo",
            Error::ProcMacroDegraded("proc-macro server unavailable".into()),
        );
        assert!(
            !matches!(err, ProducerError::UnsupportedConstruct { .. }),
            "L51: ProcMacroDegraded fell into other => UnsupportedConstruct; \
             got {err:?}"
        );
    }
}

#[cfg(test)]
mod medium_crates {
    use super::RustProducer;
    use crate::{produce, PackageSource};
    use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
    use nudox_ir::foreign::Unlinked;

    /// serde 1.0.229 and serde_core 1.0.229, each as its own package.
    /// Crates live under `/tmp/medium/crates`.
    #[test]
    #[ignore = "extract serde 1.0.229 and serde_core 1.0.229 under /tmp/medium/crates"]
    fn serde_1_0_229_and_serde_core_seal() {
        for (dir, name) in [
            ("serde-1.0.229", "serde"),
            ("serde_core-1.0.229", "serde_core"),
        ] {
            let root = std::path::PathBuf::from("/tmp/medium/crates").join(dir);
            let source = PackageSource::new(&root, name, "1.0.229");
            let lineage =
                PackageLineageId::new(EcosystemId::new("crates"), PackageName::new(name));
            let produced = produce(&RustProducer::default(), &source, &lineage, &Unlinked)
                .unwrap_or_else(|error| panic!("{name} failed to seal: {error}"));
            assert!(
                produced.table.iter().any(|(_, entry)| {
                    entry.sym().name == "Serialize" || entry.sym().name == "Serializer"
                }),
                "{name} must declare Serialize or Serializer"
            );
            if name == "serde" {
                let points_at_core = produced.table.iter().any(|(_, entry)| {
                    matches!(
                        entry.kind(),
                        nudox_ir::entry::EntryInner::Reference(nudox_ir::index::Ref::Foreign { key, .. })
                            if key.origin.lineage().is_some_and(|lineage| lineage.name.as_str() == "serde_core")
                    )
                });
                assert!(
                    points_at_core,
                    "serde must name serde_core as a foreign package, not as an undeclared local"
                );
            }
        }
    }
}
