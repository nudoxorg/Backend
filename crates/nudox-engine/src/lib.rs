//! `nudox-engine` — the streaming facade between the IR layer and `lindsey`.
//!
//! # Dependency law (§L0)
//!
//! This crate sits at L3 in the stack:
//!
//! ```text
//! lindsey  →  nudox-engine  →  {nudox-graph, nudox-store}  →  nudox-ir
//! ```
//!
//! `lindsey` must not import `nudox-ir`, `nudox-store`, or `nudox-graph`
//! directly.  `nudox-engine` must not import `gpui`.  Both rules are enforced
//! by `scripts/lint-gui-no-block.sh`.
//!
//! # Modules
//!
//! * [`wire`]    — the complete type vocabulary crossing the engine↔GUI seam
//!                (§L2).  `lindsey` imports only from here.
//! * [`chunk`]   — LR-3/LR-4: the one place IR becomes presentation.
//! * [`runtime`] — LR-9: Tokio multi-thread runtime + `LocalSet` for query
//!                 execution; `Engine::start` → `EngineHandle`.
//! * [`doc`]     — `open_symbol`: drives the chunker and emits `DocEvent`
//!                 streams following the §9.3 protocol.
//! * [`search`]  — name/type/semantic fan-out over `PackageIndexes` (LR-10).
//! * [`query`]   — drives `CorpusAdapter` on the `LocalSet`; emits
//!                 `QueryEvent` streams.
//! * [`versions`] — the version plane: holding several generations of one
//!                 package, and choosing which one the corpus serves.
//! * [`timeline`] — a symbol's history across those generations, keyed on
//!                 `IntroId`.
//!
//! # `EngineHandle` public surface (§7.1 + §L5)
//!
//! ```text
//! EngineHandle::search(q, gen)          -> (StreamHandle, Receiver<SearchEvent>)
//! EngineHandle::open_symbol(key, gen)   -> (StreamHandle, Receiver<DocEvent>)
//! EngineHandle::open_package(id, gen)   -> (StreamHandle, Receiver<PackageEvent>)  // stub
//! EngineHandle::packages()              -> Receiver<PackageLoadEvent>
//! EngineHandle::versions(pkg)           -> VersionList                             // sync
//! EngineHandle::select_version(pkg, v, gen) -> Receiver<VersionEvent>
//! EngineHandle::resolve_project(root)   -> (StreamHandle, Receiver<ProjectEvent>)  // stub
//! EngineHandle::sync()                  -> Receiver<SyncEvent>                     // stub
//! EngineHandle::jobs()                  -> Receiver<JobEvent>                      // stub
//! EngineHandle::command(cmd)            -> ()                                      // stub
//! EngineHandle::query(q, gen)           -> (StreamHandle, Receiver<QueryEvent>)
//! EngineHandle::schema()                -> &'static Schema
//! ```
//!
//! `versions` is the one synchronous query on the handle. The justification is
//! in [`versions`]: it reads data that is already fully resident, cannot arrive
//! partially, and has nothing for a `Gen` to guard, so a channel would buy a
//! task hop and no correctness.

pub mod wire;
pub mod chunk;
pub mod runtime;
pub mod doc;
pub mod highlight;
pub mod search;
pub mod query;
pub mod timeline;
pub mod versions;

#[cfg(test)]
pub(crate) mod test_support;

// Re-export the primary public surface so callers can `use nudox_engine::*`
// if they prefer.
pub use runtime::{Engine, EngineConfig, EngineHandle, StreamHandle};
pub use search::SearchQuery;
pub use query::GraphQuery;
pub use wire::{
    DocEvent, EngineError, Gen, GenerationId, HitRow, KindTag, Provenance, QueryEvent,
    QueryRow, RenderSection, SearchEvent, SearchSectionId, SectionId, SharedStr, SymbolHead,
    SymbolKey, Timeline, TimelineChange, TimelineRow, VersionEvent, VersionList, VersionRow,
};
// `PackageLoadEvent`, `PackageSpec`, and `ProducerLanguage` are defined below
// and are `pub`; they are visible to `lindsey` as `nudox_engine::PackageLoadEvent`
// etc. without any additional re-export.

// ---------------------------------------------------------------------------
// Types crossing the engine↔GUI seam for package loading
//
// These are declared here (not in `runtime.rs`) so the runtime module stays
// focused on runtime machinery.  Types that `lindsey` must name without
// depending on `nudox-store` or `nudox-ir` live here.
// ---------------------------------------------------------------------------

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// ProducerLanguage
// ---------------------------------------------------------------------------

/// The source language for a package passed to [`Engine::start_with_producer`].
///
/// This is the engine's own re-export of the language concept so that
/// `lindsey` can specify a language without importing `nudox-ir`.  It mirrors
/// `nudox_ir::body::Language` but is defined here at the seam (§L0) to keep
/// the dependency law intact.
///
/// Only `Rust` is currently effective: `ProducerRegistry::with_rust_pilot`
/// registers only the Rust producer.  Passing any other variant causes a
/// `PackageLoadEvent::LoadFailed { error: "toolchain missing … " }` for that
/// package; the rest of the corpus is unaffected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProducerLanguage {
    /// The Rust language, produced via `ra_ap_*` in-process.
    Rust,
}

// ---------------------------------------------------------------------------
// PackageSpec
// ---------------------------------------------------------------------------

/// A caller-supplied description of one on-disk package to load.
///
/// Passed to [`Engine::start_with_producer`].  Contains only `std`-owned types
/// so that `lindsey` (which must not name `nudox-store` or `nudox-ir`, §L0)
/// can construct one without crossing the dependency boundary.
///
/// The engine converts this to a `nudox_store::source::producer::PackageDescriptor`
/// internally — the translation is a single `match` in `start_with_producer`
/// and is invisible to the caller.
///
/// # Example (from `lindsey`)
///
/// ```rust,ignore
/// Engine::start_with_producer(config, vec![
///     PackageSpec {
///         root: PathBuf::from("/path/to/my-crate"),
///         name: "my-crate".to_owned(),
///         version: "0.1.0".to_owned(),
///         language: ProducerLanguage::Rust,
///     },
/// ])
/// ```
#[derive(Clone, Debug)]
pub struct PackageSpec {
    /// Absolute path to the package root (the directory containing the language
    /// manifest: `Cargo.toml`, `go.mod`, `*.csproj`, …).
    pub root: PathBuf,
    /// Package name as it appears in the ecosystem registry.
    pub name: String,
    /// Version string (e.g. `"1.2.3"`) — stored for diagnostics, not parsed.
    pub version: String,
    /// The source language.  Determines which producer is used.
    pub language: ProducerLanguage,
}

// ---------------------------------------------------------------------------
// PackageVersionSpec / PackageHistorySpec
// ---------------------------------------------------------------------------

/// One on-disk version of a package.
///
/// A component of [`PackageHistorySpec`]. Carries only the two things that
/// differ between generations of the same package — where it is unpacked and
/// what it calls itself — because everything else (name, ecosystem, language)
/// is a property of the *lineage* and would be a lie to vary per version.
#[derive(Clone, Debug)]
pub struct PackageVersionSpec {
    /// Absolute path to this version's package root (the directory containing
    /// `Cargo.toml`, `go.mod`, `*.csproj`, …).
    ///
    /// Every version needs its own root. Two versions of a crate are two
    /// source trees; there is no way to ask a producer for "the same directory
    /// but at 0.7.9".
    pub root: PathBuf,
    /// The version string (e.g. `"0.8.1"`).
    ///
    /// Shown verbatim in [`crate::wire::VersionRow::version`] and in every
    /// [`crate::wire::TimelineRow`]. It is parsed *only* for ordering, and a
    /// string that does not parse is still loaded and still listed — it just
    /// sorts below the ones that do (see `crate::versions`).
    pub version: String,
}

/// Several versions of one package, to be loaded together.
///
/// Passed to [`Engine::start_with_versions`]. Like [`PackageSpec`] it contains
/// only `std`-owned types so `lindsey` can construct one without naming
/// `nudox-store` or `nudox-ir` (§L0).
///
/// # Why this rather than a `Vec<PackageSpec>`
///
/// A list of `PackageSpec`s can already express "load axum 0.7.9 and axum
/// 0.8.1" — they would produce the same `PackageLineageId` and the engine would
/// load both. But nothing in the type says they are the same package, so
/// nothing stops a caller from giving two generations different names, or the
/// same version twice, and the resulting corpus would be quietly wrong. Making
/// the lineage a property of the outer struct and the version a property of the
/// inner one puts the invariant in the type: one name, one language, N roots.
///
/// # Example (from `lindsey`)
///
/// ```rust,ignore
/// Engine::start_with_versions(config, vec![
///     PackageHistorySpec {
///         name: "axum".to_owned(),
///         language: ProducerLanguage::Rust,
///         versions: vec![
///             PackageVersionSpec { root: "/src/axum-0.7.9".into(), version: "0.7.9".into() },
///             PackageVersionSpec { root: "/src/axum-0.8.1".into(), version: "0.8.1".into() },
///         ],
///     },
/// ])
/// ```
///
/// A one-element `versions` list is exactly equivalent to the corresponding
/// [`PackageSpec`], and is what `start_with_producer` builds internally.
#[derive(Clone, Debug)]
pub struct PackageHistorySpec {
    /// Package name as it appears in the ecosystem registry.  Shared by every
    /// version, because it is what makes them one lineage.
    pub name: String,
    /// The source language.  Determines which producer runs, for all versions.
    pub language: ProducerLanguage,
    /// The versions to load, in any order.
    ///
    /// Order here is not precedence: the engine sorts them (see
    /// `crate::versions`) so the newest becomes current regardless of the order
    /// they are listed in or the order their producers happen to finish.
    ///
    /// An empty list loads nothing and is not an error — it is the honest
    /// encoding of "this package has no versions to load", which a caller
    /// filtering a manifest can legitimately produce.
    pub versions: Vec<PackageVersionSpec>,
}

impl From<PackageSpec> for PackageHistorySpec {
    /// Every single-version request is a one-generation history.
    ///
    /// This is what lets `start_with_producer` delegate to
    /// `start_with_versions` instead of maintaining a second load path.
    fn from(spec: PackageSpec) -> Self {
        Self {
            name: spec.name,
            language: spec.language,
            versions: vec![PackageVersionSpec {
                root: spec.root,
                version: spec.version,
            }],
        }
    }
}

// ---------------------------------------------------------------------------
// PackageLoadEvent
// ---------------------------------------------------------------------------

/// An event emitted by [`EngineHandle::packages`] describing the outcome of
/// loading one package.
///
/// `#[non_exhaustive]` so that new variants (e.g. a `Reloaded` event when
/// hot-reload lands in M5) can be added without breaking existing `match`
/// arms in `lindsey`.
///
/// All payload strings are [`SharedStr`] — `triomphe::Arc<str>` under the
/// hood — so the drain loop in the GUI never allocates: it clones Arc handles,
/// not heap buffers (GUI-PLAN §2.2.5).
///
/// The type is intentionally **not** a re-export of any store or IR type.
/// `nudox_store::package::PackageView` and `nudox_ir::change::PackageLineageId`
/// must not cross the §L0 boundary; this event carries only the fields the
/// GUI actually needs to render its status bar and package list.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum PackageLoadEvent {
    /// A package was fully produced and inserted into the corpus.
    ///
    /// Emitted once per package, after `Corpus::insert` returns.  After this
    /// event the package is searchable via `EngineHandle::search`.
    Loaded {
        /// The package's registry name (e.g. `"serde"`, `"react"`).
        name: SharedStr,
        /// The ecosystem this package belongs to (`"cargo"`, `"npm"`, …).
        ecosystem: SharedStr,
        /// The version string, if it was available at discovery time.
        version: Option<String>,
        /// The number of public API symbols in the produced IR.  Useful for
        /// the status bar's "N symbols" count.  This is the symbol count at
        /// the time of insertion; it does not change after that.
        symbol_count: u64,
        /// The package's root entry — its crate / top-level module.
        ///
        /// Carried on the event rather than exposed as a separate query,
        /// because every consumer that wants it wants it *at this moment*: a
        /// package list needs somewhere to navigate the instant a row appears,
        /// and a second round trip to ask "what is this package's root" would
        /// re-derive something the engine held while building the event.
        ///
        /// `None` only when the package has no unparented entry, which means a
        /// malformed IR rather than an empty package — every producer declares
        /// a root module.
        root: Option<SymbolKey>,
    },

    /// A package failed to load; the remaining packages are unaffected.
    ///
    /// Corresponds to `LoadEvent::Failed` in `nudox-store`.  The error is
    /// flattened to a `SharedStr` at the boundary so `lindsey` never needs to
    /// match on `SourceError` variants.
    LoadFailed {
        /// Name of the package that failed (may be partial if the oracle
        /// crashed early).
        name: SharedStr,
        /// The ecosystem the package belongs to.
        ecosystem: SharedStr,
        /// Human-readable error message suitable for a status-bar tooltip or
        /// an error banner.
        error: SharedStr,
    },
}

// ---------------------------------------------------------------------------
// PackageEvent (open-package-page stream — distinct from PackageLoadEvent)
// ---------------------------------------------------------------------------

/// Events emitted by [`EngineHandle::open_package`] for the API-surface view
/// of one loaded package.
///
/// **Stub (M4).** Only `Done` is emitted today.  Future variants will carry
/// symbol-summary sections analogous to `DocEvent::Section`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum PackageEvent {
    /// Terminal — the API-surface stream for this package is complete.
    Done { generation: Gen },
}

/// Placeholder project event — emitted by the `resolve_project` stub.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ProjectEvent {
    /// Stub — project resolution not implemented yet (M3 work).
    Done { generation: Gen },
}

/// Placeholder sync event — emitted by the `sync` stub.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum SyncEvent {
    /// Stub.
    Idle,
}

/// Placeholder job event — emitted by the `jobs` stub.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum JobEvent {
    /// Stub.
    Idle,
}

/// A fire-and-forget command to the engine (cancel job, pin, retry, etc.).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ClientCommand {
    /// No-op stub.
    Noop,
}

impl EngineHandle {
    /// Open a package's API surface as a streaming [`PackageEvent`] sequence.
    ///
    /// **Stub (M4).** Returns an immediately-`Done` stream.
    pub fn open_package(
        &self,
        _id: nudox_ir::change::PackageLineageId,
        generation: Gen,
    ) -> (StreamHandle, flume::Receiver<PackageEvent>) {
        let (tx, rx) = flume::bounded::<PackageEvent>(32);
        let (_, cancel_fn) = Self::make_cancel();
        let handle = StreamHandle::new(generation, cancel_fn);
        let _ = tx.try_send(PackageEvent::Done { generation });
        (handle, rx)
    }

    /// Resolve a project workspace and stream package discovery events.
    ///
    /// **Stub (M3).** Returns an immediately-`Done` stream.
    pub fn resolve_project(
        &self,
        _root: PathBuf,
    ) -> (StreamHandle, flume::Receiver<ProjectEvent>) {
        let generation = Gen(0);
        let (tx, rx) = flume::bounded::<ProjectEvent>(32);
        let (_, cancel_fn) = Self::make_cancel();
        let handle = StreamHandle::new(generation, cancel_fn);
        let _ = tx.try_send(ProjectEvent::Done { generation });
        (handle, rx)
    }

    /// Subscribe to the long-lived sync event stream.
    ///
    /// **Stub (M4).** Returns a receiver that immediately closes.
    pub fn sync(&self) -> flume::Receiver<SyncEvent> {
        let (_, rx) = flume::bounded::<SyncEvent>(256);
        rx
    }

    /// Subscribe to the long-lived job event stream.
    ///
    /// **Stub (M4).** Returns a receiver that immediately closes.
    pub fn jobs(&self) -> flume::Receiver<JobEvent> {
        let (_, rx) = flume::bounded::<JobEvent>(256);
        rx
    }

    /// Fire-and-forget a command to the engine (cancel job, pin, retry, …).
    ///
    /// **Stub (M4).** Currently a no-op.
    pub fn command(&self, _cmd: ClientCommand) {
        // No-op until M4 wires up the command channel.
    }
}
