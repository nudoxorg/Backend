//! `nudox-engine` — the streaming facade between the IR layer and `lindsey`.
//!
//! # Dependency law (§L0)
//!
//! This crate sits at L3 in the stack:
//!
//! ```text
//! lindsey  →  nudox-engine  →  {store, graph, embed, mcp}  →  nudox-ir
//! ```
//!
//! The four former sibling crates — `nudox-store`, `nudox-graph`, `nudox-embed`
//! and `nudox-mcp` — were merged into this crate as the [`store`], [`graph`],
//! [`embed`] and [`mcp`] modules respectively. `lindsey` must not import
//! `nudox-ir`, or reach past the engine's public wire surface into its
//! `store`/`graph`/`mcp` internals. `nudox-engine` must not import `gpui`.
//! Both rules are enforced by `scripts/lint-gui-no-block.sh` and
//! `workspace/gui/tests/dependency_law.rs`.
//!
//! # Modules
//!
//! * [`wire`] — the complete type vocabulary crossing the engine↔GUI seam
//!   (§L2).  `lindsey` imports only from here.
//! * [`chunk`] — LR-3/LR-4: the one place IR becomes presentation.
//! * [`runtime`] — LR-9: Tokio multi-thread runtime + `LocalSet` for query
//!   execution; `Engine::start` → `EngineHandle`.
//! * [`doc`] — `open_symbol`: drives the chunker and emits `DocEvent`
//!   streams following the §9.3 protocol.
//! * [`search`] — name/type/semantic fan-out over `PackageIndexes` (LR-10).
//! * [`query`] — drives `CorpusAdapter` on the `LocalSet`; emits
//!   `QueryEvent` streams.
//! * [`versions`] — the version plane: holding several generations of one
//!   package, and choosing which one the corpus serves.
//! * [`timeline`] — a symbol's history across those generations, keyed on
//!   `IntroId`.
//! * [`purl`] — the one thing a *user* can type that names a package this
//!   corpus has never seen. An input parser and a rendering, not
//!   a second identity beside `PackageLineageId`.
//! * [`acquire`] — resolve → fetch → verify → extract, and the typed failure
//!   vocabulary for each. Read its module docs before assuming
//!   anything about what a fetch guarantees.
//! * [`store`] — the live corpus (`Corpus`, `PackageView`, `IrSource`) at the
//!   base of the stack, directly above `nudox-ir`.
//! * [`graph`] — the Trustfall query plane (`CorpusAdapter`) over the corpus.
//! * [`embed`] — the production ONNX embedder adapter behind `semantic::Embedder`.
//! * [`mcp`] — the MCP server `lindsey` hosts; a *view* of `EngineHandle`.
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
//! EngineHandle::resolve_project(root)   -> Result<(StreamHandle, Receiver<ProjectEvent>), Unimplemented>
//! EngineHandle::sync()                  -> Result<Receiver<SyncEvent>, Unimplemented>
//! EngineHandle::jobs()                  -> Result<Receiver<JobEvent>, Unimplemented>
//! EngineHandle::command(cmd)            -> Result<(), Unimplemented>
//! EngineHandle::index_purl(purl, gen)   -> (StreamHandle, Receiver<IndexEvent>)
//! EngineHandle::query(q, gen)           -> (StreamHandle, Receiver<QueryEvent>)
//! EngineHandle::schema()                -> &'static Schema
//! EngineHandle::runtime_handle()        -> tokio::runtime::Handle                  // sync
//! ```
//!
//! `versions` is the one synchronous query on the handle. The justification is
//! in [`versions`]: it reads data that is already fully resident, cannot arrive
//! partially, and has nothing for a `Gen` to guard, so a channel would buy a
//! task hop and no correctness.
//!
//! # The four planes that do not exist yet
//!
//! `resolve_project`, `sync`, `jobs` and `command` all return
//! [`Result<_, Unimplemented>`](Unimplemented). Until 2026-08-07 they returned
//! an immediately-`Done` stream, or a receiver that closed on the spot, which a
//! caller could not tell apart from "resolution succeeded and found nothing" or
//! "there are no jobs right now". That is the exact shape doctrine §8 names —
//! a failure converted into a success with the cause parked where nobody
//! looked. See [`EngineCapability`] and docs/LIMITATIONS.md L37.

pub mod chunk;
pub mod doc;
pub mod embed;
pub mod graph;
pub mod highlight;
pub mod mcp;
pub mod packages;
/// Per-platform directory resolution (see its module docs).
pub(crate) mod platform;
pub mod query;
pub mod runtime;
pub mod search;
pub mod semantic;
pub mod store;
pub mod typequery;
pub mod versions;
pub mod wire;

// `purl` and `acquire` moved under [`packages`]; keep the historical module
// paths resolvable for callers that reach `nudox_engine::acquire::*` /
// `nudox_engine::purl::*` (integration tests, older hosts).
pub use packages::{acquire, purl};

#[cfg(test)]
pub(crate) mod test_support;

// Re-export the primary public surface so callers can `use crate::*`
// if they prefer.
pub use packages::acquire::{Error as IndexError, IndexEvent, IndexStage, Integrity};
pub use packages::purl::{Error as PurlParseError, Purl, PurlType};
pub use query::GraphQuery;
pub use runtime::{Engine, EngineConfig, EngineHandle, StreamHandle};
pub use search::SearchQuery;
// The embedding seam. `lindsey` names these to install a host embedder, in
// exactly the way it names `Highlighter` to install a host highlighter — no IR
// type crosses the seam, so §1 is satisfied by the same argument.
pub use semantic::{
    EmbedRole, Embedder, EmbedderInfo, Error as EmbedError, SectionState, SharedEmbedder, Unavailable,
};
pub use typequery::{TypeFacet, TypeQuery};
pub use wire::{
    DocEvent, EngineError, Gen, GenerationId, HitRow, KindTag, Provenance, QueryEvent, QueryRow,
    RenderSection, SearchEvent, SearchSectionId, SectionId, SharedStr, SymbolHead, SymbolKey,
    Timeline, TimelineChange, TimelineRow, VersionEvent, VersionList, VersionRow,
};
// `PackageLoadEvent`, `PackageSpec`, and `ProducerLanguage` are defined below
// and are `pub`; they are visible to `lindsey` as `crate::PackageLoadEvent`
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
/// One variant per language module in the `nudox-languages` crate
/// (§docs/LIMITATIONS.md L2). `#[non_exhaustive]` because an eighth producer
/// arriving must not be a breaking change for `lindsey`'s `match` arms.
///
/// # Which variants actually run something
///
/// `Engine::start_with_versions` builds its registry from
/// `ProducerRegistry::with_all_available`, which registers a runner for
/// [`Rust`](Self::Rust), [`Go`](Self::Go), [`Java`](Self::Java),
/// [`CSharp`](Self::CSharp), [`TypeScript`](Self::TypeScript), and
/// [`Cpp`](Self::Cpp) (covering both C and C++, via `ClangProducer`) today.
///
/// [`Python`](Self::Python) is registered **only** when the `pyrefly` feature is
/// enabled. Without it the producer's `invoke()` returns an empty oracle rather
/// than an error, so registering it unconditionally made "we cannot document
/// this" indistinguishable from "this package has no public API". Unregistered,
/// a Python lookup correctly yields `ToolchainMissing`; passing it without the
/// feature enabled produces a `PackageLoadEvent::LoadFailed { error: "toolchain
/// missing … " }` for that package, and the rest of the corpus is unaffected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProducerLanguage {
    /// Rust, via `nudox-languages::rust`. Oracle: rust-analyzer (`ra_ap_*`,
    /// in-process). Manifest: `Cargo.toml`.
    Rust,
    /// Go, via `nudox-languages::go`. Oracle: a vendored Go subprocess oracle
    /// (`workspace/compiler/compile/go/oracle/`). Manifest: `go.mod`.
    Go,
    /// Java, via `nudox-languages::java`. Oracle: a Java oracle subprocess
    /// (schema in `nudox_languages::java::schema`). Manifest: `pom.xml` or
    /// `build.gradle`.
    Java,
    /// C#, via `nudox-languages::csharp`. Oracle: Roslyn, invoked as
    /// `dotnet <publish>/oracle.dll`. Manifest: `*.csproj`.
    CSharp,
    /// Python, via `nudox-languages::python`. Oracle: pyrefly, in-process
    /// (behind that crate's `pyrefly` feature; without it, an empty oracle).
    /// Manifest: `pyproject.toml`.
    Python,
    /// TypeScript / JavaScript, via `nudox-languages::typescript`. Oracle: OXC,
    /// in-process. Manifest: `package.json`.
    TypeScript,
    /// C and C++, via `nudox-languages::clang`. Oracle: libclang, loaded at
    /// runtime via `dlopen`/`LoadLibrary` (no build-time libclang
    /// dependency). Manifest: `CMakeLists.txt` or `compile_commands.json`.
    ///
    /// Named `Cpp` rather than `C` because the clang producer's own doc
    /// comment says its single `Language::C` registration "also covers C++
    /// files" — one variant, one producer, matching that crate's actual
    /// coverage rather than the two-way split in `nudox_ir::body::Language`.
    Cpp,
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
/// The engine converts this to a `crate::store::source::producer::PackageDescriptor`
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

/// Translate one [`PackageSpec`] into the store-owned `PackageDescriptor`.
///
/// # Why this is a function and not an inline `match`
///
/// It was an inline `match` inside `Engine::start_with_versions`, which meant
/// the *only* way to produce a descriptor was to start an engine.
/// [`EngineHandle::index_purl`] needs exactly one descriptor for a package that
/// arrives long after start-up, and copying the `match` would have created a
/// second place where an ecosystem is paired with a language — the pairing
/// `PackageDescriptor`'s private `new` and its seven named constructors exist
/// to make unmistakable, because getting it wrong builds a descriptor no
/// registered producer answers and surfaces at runtime as `ToolchainMissing`
/// rather than as a compile error.
///
/// One function, one pairing, and a new [`ProducerLanguage`] variant is a
/// compile error here rather than a silent `ToolchainMissing` there.
pub(crate) fn descriptor_for(
    spec: PackageSpec,
) -> crate::store::source::producer::PackageDescriptor {
    use crate::store::source::producer::PackageDescriptor;

    let PackageSpec {
        root,
        name,
        version,
        language,
    } = spec;
    match language {
        ProducerLanguage::Rust => PackageDescriptor::cargo(root, name, version),
        ProducerLanguage::Go => PackageDescriptor::go(root, name, version),
        ProducerLanguage::Java => PackageDescriptor::maven(root, name, version),
        ProducerLanguage::CSharp => PackageDescriptor::nuget(root, name, version),
        ProducerLanguage::Python => PackageDescriptor::pypi(root, name, version),
        ProducerLanguage::TypeScript => PackageDescriptor::npm(root, name, version),
        ProducerLanguage::Cpp => PackageDescriptor::cpp(root, name, version),
    }
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
/// `crate::store::package::PackageView` and `nudox_ir::change::PackageLineageId`
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
    /// match on `Error` variants.
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

/// Events emitted by project resolution once it exists.
///
/// **No value of this type is produced today.**
/// [`EngineHandle::resolve_project`] returns `Err(Unimplemented)` rather than a
/// stream, so this enum currently describes the shape M3 will fill in and
/// nothing more. It is kept — rather than deleted — because
/// [`EngineHandle::resolve_project`]'s `Ok` type has to name *something*, and a
/// named type with a doc comment is a better placeholder than a type parameter
/// nobody can read.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ProjectEvent {
    /// Terminal — every package under the requested root has been reported.
    Done {
        /// The generation this stream was opened for.
        generation: Gen,
    },
}

/// Events emitted by the sync plane once it exists.
///
/// **No value of this type is produced today** — see [`ProjectEvent`] for why
/// the type still exists.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum SyncEvent {
    /// Nothing is being fetched or re-indexed.
    Idle,
}

/// Events emitted by the job plane once it exists.
///
/// **No value of this type is produced today** — see [`ProjectEvent`] for why
/// the type still exists.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum JobEvent {
    /// No job is running.
    Idle,
}

/// A fire-and-forget command to the engine (cancel job, pin, retry, …).
///
/// The single [`Noop`](Self::Noop) variant is a placeholder, not a command the
/// engine honours: there is no command channel into the runtime yet, so
/// [`EngineHandle::command`] rejects every value of this type — including this
/// one. Accepting `Noop` "because doing nothing succeeds" would make the plane
/// look alive to the one caller most likely to probe it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ClientCommand {
    /// Placeholder. See the type-level docs — this is *not* accepted.
    Noop,
}

// ---------------------------------------------------------------------------
// Unbuilt capabilities (docs/LIMITATIONS.md L37)
// ---------------------------------------------------------------------------

/// A milestone in GUI-LOCAL-PLAN §L10.
///
/// Carried by [`Unimplemented`] so a caller can say *when* rather than only
/// *no*. An enum rather than a `&'static str` because a milestone is a domain
/// value that crosses a crate boundary (§L7.1), and because a UI that wants to
/// hide "M4" behind "a future release" must be able to match on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Milestone {
    /// M3 — the shell.
    M3,
    /// M4 — search, jobs, and the command channel.
    M4,
}

impl std::fmt::Display for Milestone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::M3 => f.write_str("M3"),
            Self::M4 => f.write_str("M4"),
        }
    }
}

/// A plane of the [`EngineHandle`] API whose signature exists but whose
/// behaviour does not.
///
/// # Why this is an enum and not four separate error types
///
/// All four planes fail for the same *kind* of reason — the subsystem behind
/// them has not been built — and a caller's response is the same in each case:
/// render "unavailable", not "empty". One enum keeps that decision in one
/// `match`, and adding a fifth plane (or deleting one as it lands) is a
/// compile-time event at every call site rather than a silent behaviour change.
///
/// `#[non_exhaustive]` because this list shrinks as milestones land, and
/// `lindsey` must not need a new release to keep compiling when it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EngineCapability {
    /// [`EngineHandle::resolve_project`] — discovering the packages under a
    /// workspace root and watching them for changes.
    ProjectResolution,
    /// [`EngineHandle::sync`] — the long-lived fetch/re-index progress stream.
    Sync,
    /// [`EngineHandle::jobs`] — the long-lived producer-job stream.
    Jobs,
    /// [`EngineHandle::command`] — the client → engine command channel.
    Commands,
}

impl EngineCapability {
    /// The milestone that owns building this plane (GUI-LOCAL-PLAN §L10).
    pub fn milestone(self) -> Milestone {
        match self {
            Self::ProjectResolution => Milestone::M3,
            Self::Sync | Self::Jobs | Self::Commands => Milestone::M4,
        }
    }

    /// The concrete thing that has to exist before this plane can.
    ///
    /// Recorded here rather than in a plan document because a caller reading
    /// the error is the reader most likely to want it, and because a reason
    /// that lives next to the `match` cannot drift out of step with the list
    /// of capabilities the way a prose file can.
    pub fn blocked_on(self) -> &'static str {
        match self {
            Self::ProjectResolution => {
                "a filesystem walk in the engine that discovers on-disk packages and infers their \
                 language from the manifest present. The second half of this blocker — an \
                 `EngineHandle` method that can add a package to a *running* corpus — is now built \
                 (`EngineHandle::index_purl` and the `load_one`/`drive_load` path beneath it), so \
                 what remains is discovery, not insertion"
            }
            Self::Sync => {
                "a file-watching subsystem — no watcher dependency is present in this workspace \
                 and packages are loaded once at `start*` time and never re-indexed"
            }
            Self::Jobs => {
                "a job registry in `runtime.rs`; producer work is spawned there today but is \
                 only observable as `PackageLoadEvent`, which carries no job identity"
            }
            Self::Commands => "a command channel into the engine runtime, and commands to put on it",
        }
    }
}

impl std::fmt::Display for EngineCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ProjectResolution => "project resolution",
            Self::Sync => "the sync plane",
            Self::Jobs => "the job plane",
            Self::Commands => "the command channel",
        })
    }
}

/// The engine was asked for a plane that this build does not have.
///
/// # Why a typed error and not an empty stream
///
/// This is the whole point of docs/LIMITATIONS.md L37. An immediately-closed
/// receiver and a receiver that is merely quiet are the same value to a caller,
/// so a UI wired to one renders "no jobs" — a claim — where the truth is "we
/// cannot answer". Returning `Err` moves the decision to the call site, where
/// the caller has to choose between "unavailable" and "empty" in code a
/// reviewer can see.
///
/// Only [`capability`](Self::capability) is stored; the milestone and the
/// blocker are *derived* from it (doctrine §8: never accumulate alongside what
/// you can compute), so the three can never disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, thiserror::Error)]
#[error(
    "{capability} is not implemented in this build ({} work): blocked on {}",
    .capability.milestone(),
    .capability.blocked_on(),
)]
pub struct Unimplemented {
    /// Which plane was asked for.
    pub capability: EngineCapability,
}

impl Unimplemented {
    /// The milestone that will make this call succeed.
    pub fn milestone(&self) -> Milestone {
        self.capability.milestone()
    }
}

impl EngineHandle {
    /// Open a package's API surface as a streaming [`PackageEvent`] sequence.
    ///
    /// **Stub (M4).** Returns an immediately-`Done` stream.
    ///
    /// This carries the same defect the four planes below were fixed for
    /// (docs/LIMITATIONS.md L37) — a `Done` with no sections is indistinguishable
    /// from a package with no API — and is left alone here only because it was
    /// outside the assigned scope of that fix. It has no callers.
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
    /// **Always `Err`** ([`EngineCapability::ProjectResolution`]). The engine
    /// has no filesystem walk. It *can* now add a package to a corpus that is
    /// already running — [`EngineHandle::index_purl`] does exactly that — so
    /// the remaining gap is discovery alone. Returning `Ok` with an empty
    /// stream would report "this directory contains no packages" about a
    /// directory nobody looked at.
    pub fn resolve_project(
        &self,
        _root: PathBuf,
    ) -> Result<(StreamHandle, flume::Receiver<ProjectEvent>), Unimplemented> {
        Err(Unimplemented {
            capability: EngineCapability::ProjectResolution,
        })
    }

    /// Subscribe to the long-lived sync event stream.
    ///
    /// **Always `Err`** ([`EngineCapability::Sync`]). Nothing re-indexes after
    /// `start*`, so a silent receiver would mean "everything is up to date"
    /// when the truth is "nothing is being watched".
    pub fn sync(&self) -> Result<flume::Receiver<SyncEvent>, Unimplemented> {
        Err(Unimplemented {
            capability: EngineCapability::Sync,
        })
    }

    /// Subscribe to the long-lived job event stream.
    ///
    /// **Always `Err`** ([`EngineCapability::Jobs`]). This is the root cause of
    /// docs/LIMITATIONS.md L29: the Jobs panel had nothing to render because this
    /// method handed it a receiver that closed immediately, and an empty panel
    /// read as "no jobs are running".
    pub fn jobs(&self) -> Result<flume::Receiver<JobEvent>, Unimplemented> {
        Err(Unimplemented {
            capability: EngineCapability::Jobs,
        })
    }

    /// Fire-and-forget a command to the engine (cancel job, pin, retry, …).
    ///
    /// **Always `Err`** ([`EngineCapability::Commands`]). Every value of
    /// [`ClientCommand`] is rejected, [`ClientCommand::Noop`] included — see
    /// that type's docs.
    pub fn command(&self, _cmd: ClientCommand) -> Result<(), Unimplemented> {
        Err(Unimplemented {
            capability: EngineCapability::Commands,
        })
    }

    /// A handle to the one Tokio runtime in the process (LR-9).
    ///
    /// # Why the engine hands this out at all
    ///
    /// LR-9 says the engine owns every runtime and `lindsey` links none. That
    /// rule is what keeps the GUI's foreground pure, but it also means a host
    /// that must run *one* piece of async work — starting the MCP server
    /// `lindsey` hosts (GUI-LOCAL-PLAN §L6) — has nowhere to run it. The two
    /// alternatives are both worse: a second runtime inside `nudox-mcp` breaks
    /// LR-9 outright and gives the process two thread pools competing for the
    /// same cores, and moving the MCP server into this crate would invert the
    /// dependency law (`nudox-mcp` already depends on `nudox-engine`).
    ///
    /// So: hosts borrow this runtime, they do not build one. Anything spawned
    /// on it is bounded by the engine's own lifetime, because the returned
    /// `Handle` keeps nothing alive — hold the [`EngineHandle`] for as long as
    /// the spawned work must run.
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        // `RuntimeGuard`'s inherent `handle()` yields the `&Runtime`; the
        // second `handle()` is `Runtime::handle`, which yields the cheap
        // cloneable `Handle`.
        self.runtime.handle().handle().clone()
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    use crate::runtime::{Engine, EngineConfig};
    use crate::test_support::{StaticSource, lineage};

    /// An engine over an empty corpus. None of the four planes below reads the
    /// corpus, so a source with no generations is the honest input: it removes
    /// the "maybe it was just empty" explanation from every failure here.
    fn engine() -> EngineHandle {
        engine_with(EngineConfig::default())
    }

    fn engine_with(config: EngineConfig) -> EngineHandle {
        Engine::start(config, StaticSource::versions(&lineage("none"), Vec::new()))
    }

    /// The defect docs/LIMITATIONS.md L37 records: each of the four planes used to
    /// hand back a channel that a caller could not tell apart from a real,
    /// empty answer. The invariant is that they now refuse *by type*.
    #[test]
    fn the_four_unbuilt_planes_refuse_with_a_named_capability() {
        let engine = engine();

        assert_eq!(
            engine
                .resolve_project(PathBuf::from("/nonexistent"))
                .err()
                .map(|e| e.capability),
            Some(EngineCapability::ProjectResolution),
        );
        assert_eq!(
            engine.sync().err().map(|e| e.capability),
            Some(EngineCapability::Sync),
        );
        assert_eq!(
            engine.jobs().err().map(|e| e.capability),
            Some(EngineCapability::Jobs),
        );
        assert_eq!(
            engine.command(ClientCommand::Noop).err().map(|e| e.capability),
            Some(EngineCapability::Commands),
        );

        drop(engine);
    }

    /// `Noop` is the one command a reader would expect to be accepted "because
    /// doing nothing always works". It is not, and that is deliberate: an
    /// accepted command would make the plane look alive.
    #[test]
    fn even_the_noop_command_is_rejected() {
        let engine = engine();
        let err = engine
            .command(ClientCommand::Noop)
            .expect_err("no command channel exists, so no command can be delivered");
        assert_eq!(err.capability, EngineCapability::Commands);
        assert_eq!(err.milestone(), Milestone::M4);
        drop(engine);
    }

    /// Each capability names a *distinct* blocker. A shared "not implemented"
    /// string would be doctrine §3's `"failed"` message wearing four hats.
    #[test]
    fn every_capability_names_its_own_blocker_and_milestone() {
        let all = [
            EngineCapability::ProjectResolution,
            EngineCapability::Sync,
            EngineCapability::Jobs,
            EngineCapability::Commands,
        ];

        let mut seen: Vec<&'static str> = Vec::new();
        for capability in all {
            let blocker = capability.blocked_on();
            assert!(
                !seen.contains(&blocker),
                "{capability} reuses another capability's blocker: {blocker}",
            );
            seen.push(blocker);
        }

        assert_eq!(
            EngineCapability::ProjectResolution.milestone(),
            Milestone::M3,
        );
        for capability in [
            EngineCapability::Sync,
            EngineCapability::Jobs,
            EngineCapability::Commands,
        ] {
            assert_eq!(capability.milestone(), Milestone::M4);
        }
    }

    /// The message a caller renders has to say which plane and when — a
    /// bare "unimplemented" would send the reader to the wrong subsystem.
    #[test]
    fn the_rendered_error_names_the_plane_the_milestone_and_the_blocker() {
        let err = Unimplemented {
            capability: EngineCapability::Jobs,
        };
        let text = err.to_string();
        assert!(text.contains("the job plane"), "{text}");
        assert!(text.contains("M4"), "{text}");
        assert!(text.contains("job registry"), "{text}");
    }

    /// LR-9: the engine owns the process's only runtime, and a host that
    /// borrows it must get *that* one, not a freshly built pool.
    ///
    /// The assertion is on the worker count the engine was configured with —
    /// a value no default runtime would have — because "is this a live
    /// runtime?" would pass against a `Runtime::new()` bolted on inside
    /// `runtime_handle`, which is precisely the mistake this method exists to
    /// prevent.
    #[test]
    fn the_borrowed_runtime_is_the_engines_own_configured_one() {
        let engine = engine_with(EngineConfig {
            worker_threads: Some(3),
            ..EngineConfig::default()
        });
        let handle = engine.runtime_handle();

        assert_eq!(
            handle.metrics().num_workers(),
            3,
            "the handle must belong to the runtime `EngineConfig` built, not a new one",
        );

        // And it must actually drive work: a handle to a shut-down runtime
        // reports the same worker count.
        let ran = handle.block_on(async { tokio::spawn(async { 7_u8 }).await });
        assert_eq!(ran.expect("spawned task must join"), 7);

        drop(engine);
    }
}
