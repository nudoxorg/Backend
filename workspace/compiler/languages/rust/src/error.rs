//! Error types for the Rust producer.
//!
//! These are internal; the `Producer` trait boundary converts them into
//! `ProducerError` variants.  Keeping them distinct gives better diagnostics
//! during development.

use thiserror::Error;

use crate::ra::loaded::{BuildScriptFailure, NoDepsFallback};

/// Why a load that succeeded still has nothing for the walk to lower.
///
/// A separate type, like [`NoDepsFallback`], so it can occupy a `#[source]`
/// slot: `nudox_producer::ProducerError::NoDeclarationsContributed` is the
/// generic backstop for "this producer contributed nothing", and its cause slot
/// is where a producer that knows *why* says so. Without that, the reason would
/// be flattened into a message at the trait boundary and the reader would be
/// left with the backstop's own text, which by construction cannot name a cause.
///
/// Both variants used to be `Error::Load(io::Error(NotFound, "…"))`. That was
/// wrong in the direction docs/AGENTS-DOCTRINE.md §8 warns about specifically — "the
/// producer's package name is a key, not a label … If a lowering fails on a
/// crate that obviously exists, check the name before you check the code" — a
/// mistyped name is exactly the common cause of [`Self::NoPackageMatched`], and
/// dressing it as a `NotFound` I/O error sends the reader to look for a missing
/// *file*.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum NothingToDocument {
    /// `documented_package_names` matched no package in the loaded workspace.
    ///
    /// Almost always the name, not the code: `PackageSource::name` is matched
    /// against `cargo metadata`, so a checkout of one crate pointed at another
    /// crate's name resolves to zero packages.
    #[error(
        "no package in the loaded workspace is named `{requested}`; `PackageSource::name` is \
         matched against `cargo metadata`'s package names, so check the name against the \
         checkout's `Cargo.toml` before suspecting the lowering"
    )]
    NoPackageMatched {
        /// The `PackageSource::name` that matched nothing.
        requested: String,
    },

    /// Package names resolved, but the crate graph holds no local `hir::Crate`
    /// for any of them — the packages exist in the manifest and produced no
    /// buildable library target the walk can enter.
    #[error(
        "no local `hir::Crate` in the loaded crate graph corresponds to {packages:?}; the \
         manifest names them but rust-analyzer built no library target for any of them"
    )]
    NoCrateForPackages {
        /// The documented package set that produced no crates.
        packages: Vec<String>,
    },
}

/// Any failure that can occur during workspace load or HIR lowering.
#[derive(Debug, Error)]
pub enum Error {
    /// `ra_ap_load_cargo` / cargo-metadata / crate-graph construction failed.
    #[error("workspace load failed")]
    Load(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The workspace loaded, but from `--no-deps` cargo metadata, and the
    /// caller did not accept that.
    ///
    /// This is not a load failure and not a lowering bug: the walk would
    /// succeed and return a table that *looks* complete while missing every
    /// dependency and every `cfg(feature = "…")` item, `default` included. It
    /// is its own variant precisely so that a caller can tell "I could not read
    /// this package" apart from "I can only read part of this package, and here
    /// is the resolution failure to fix".
    ///
    /// Recover by making the dependencies resolvable (vendor them, warm the
    /// cargo cache, drop `--offline`) — or, if a dependency-free lowering is
    /// genuinely what you want, by calling
    /// [`LoadedWorkspace::accept_degraded_dependencies`].
    ///
    /// [`LoadedWorkspace::accept_degraded_dependencies`]:
    ///     crate::LoadedWorkspace::accept_degraded_dependencies
    #[error(
        "dependency graph unresolved for `{package}`: rust-analyzer loaded it from `--no-deps` \
         cargo metadata, so no dependency is in the crate graph and every Cargo feature \
         (`default` included) evaluates false; lowering it would silently omit every \
         `cfg(feature = ...)` item. Fix the resolution failure, or call \
         `LoadedWorkspace::accept_degraded_dependencies` to lower it anyway"
    )]
    DependenciesUnresolved {
        /// The package whose dependencies did not resolve.
        package: String,
        /// The `cargo metadata` failure upstream demoted into a tuple field.
        #[source]
        cause: NoDepsFallback,
    },

    /// The workspace loaded, but its build scripts did not deliver their
    /// `cargo:rustc-cfg` output, and the caller did not accept that.
    ///
    /// [`Self::DependenciesUnresolved`]'s sibling, one layer down, and with a
    /// nastier failure mode. A missing dependency graph makes items *disappear*;
    /// a missing `cargo:rustc-cfg` makes items disappear **and replaces them
    /// with their `#[cfg(not(...))]` counterparts**, so the table does not merely
    /// shrink — it describes a different crate. `log 0.4.17` lowered its private
    /// `#[cfg(not(has_atomics))]` `AtomicUsize` shim as public structure while
    /// `set_logger` and `set_boxed_logger` were absent; `nom 5.1.3` lost eight
    /// public parsers. See docs/LIMITATIONS.md L50.
    ///
    /// Recover by making the build-script `cargo check` succeed — which for a
    /// crates.io tarball usually means not asking cargo to resolve targets the
    /// tarball excluded — or, if a lowering with no build-script cfgs is
    /// genuinely what you want, by calling
    /// [`LoadedWorkspace::accept_missing_build_script_cfgs`].
    ///
    /// [`LoadedWorkspace::accept_missing_build_script_cfgs`]:
    ///     crate::LoadedWorkspace::accept_missing_build_script_cfgs
    #[error(
        "build scripts did not run for `{package}`: no `cargo:rustc-cfg` this package's \
         `build.rs` emits reached the crate graph, so every `#[cfg(...)]` item behind one \
         evaluates false and its `#[cfg(not(...))]` counterpart is lowered in its place. The \
         resulting table is not a smaller description of this crate, it is a description of a \
         different one. Fix the build-script `cargo check`, or call \
         `LoadedWorkspace::accept_missing_build_script_cfgs` to lower it anyway"
    )]
    BuildScriptsFailed {
        /// The package whose build-script output is missing.
        package: String,
        /// Why the output is missing — the cargo diagnostic upstream demoted
        /// into an `Option<&str>` field.
        #[source]
        cause: BuildScriptFailure,
    },

    /// The workspace loaded and its dependencies resolved, but there is
    /// nothing in it for the walk to lower, so this run would contribute no
    /// declarations at all.
    ///
    /// Distinct from [`Self::Load`] (nothing loaded) and from
    /// [`Self::LoweringBug`] (something loaded and the walk mishandled it):
    /// here the load succeeded and the walk is never reached. It is detected
    /// early, before any HIR work, purely so the failure names its own cause —
    /// `nudox_producer::produce`'s yield-contract gate would catch the same run
    /// at the end regardless, but only as "this producer contributed nothing",
    /// with no cause to chain. This variant is that cause.
    #[error("nothing to document")]
    NothingToDocument {
        /// The package this run was for.
        package: String,
        /// Which of the two ways the documented set came out empty.
        #[source]
        cause: NothingToDocument,
    },

    /// Salsa emitted a `Cancelled` panic during prime or walk — retryable.
    #[error("rust-analyzer analysis cancelled")]
    Cancelled,

    /// The proc-macro server was unavailable; macro-generated items may be missing.
    /// This is a soft-degradation warning rather than a hard failure.
    #[error("proc-macro server degraded")]
    ProcMacroDegraded(String),

    /// A Rust HIR construct that the producer does not yet know how to lower.
    ///
    /// Carries the canonical path of the offending symbol (when known) and a
    /// short description.  The symbol is omitted from the output rather than
    /// aborting the whole package.
    #[error("unsupported construct")]
    UnsupportedConstruct { path: String, description: String },

    /// An item lowering produced a structural problem (duplicate id, cycle, …).
    ///
    /// This indicates a bug in the producer's `lower` implementation.
    #[error("lowering bug")]
    LoweringBug(String),
}

/// Backward-compatible alias.
pub type RustProducerError = Error;
