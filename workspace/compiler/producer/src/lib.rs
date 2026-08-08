//! The shared base contract for every language producer in the nudox-ir pipeline.
//!
//! # What this crate is
//!
//! Seven language frontends (Rust, Go, Java, C#, Python, TypeScript, and
//! C/C++ via clang) lower their oracle output into the IR via [`Lowering`].
//! This crate defines the contract they all implement: [`Producer`], the
//! pipeline driver [`produce`], and the types those two depend on.
//!
//! # What this crate is NOT
//!
//! This crate contains no sandbox, daemon, cage, or execution-engine concerns.
//! Sealing inputs, worker pools, cancel tokens, threat tiers, and resource
//! profiles belong to the execution plane; they are not imported here.
//!
//! # Memory discipline
//!
//! Borrowing from [`Producer::Oracle`] output is the intended pattern.
//! [`Producer::lower`] takes `&Self::Oracle` so producers can build
//! [`Symbol`]s and kind bodies by borrowing slices, strings, and identifiers
//! directly from the deserialized oracle document rather than cloning them.
//! Prefer `&str` over `String` in intermediate structures; use the IR's
//! `Box<[T]>` (`List<T>`) convention for finished collections where the count is
//! known; always use `with_capacity` when collecting into a `Vec`.

pub mod oracle;

#[cfg(test)]
mod tests;

use std::{
    fmt,
    hash::Hash,
    path::{Path, PathBuf},
};

use nudox_ir::{
    apply::PristineIntroTable,
    body::Language,
    change::{PackageLineageId, PackageName},
    foreign::ForeignResolver,
    lower::Lowering,
    package::{PackageId, SealReport},
};
use thiserror::Error;

// ── PackageSource ─────────────────────────────────────────────────────────────

/// Where the sources for one package live and what it is called.
///
/// A [`Producer`] receives a `&PackageSource` from the caller and uses it
/// to locate files on disk and to name the root module in [`Lowering`].
/// Keep this small: only fields that producers actually need to do their
/// job.  Execution policy (sandboxing, resource limits, toolchain paths)
/// belongs to the execution plane, not here.
#[derive(Debug, Clone)]
pub struct PackageSource {
    /// Absolute path to the package root (the directory that contains the
    /// language manifest: `go.mod`, `*.csproj`, `Cargo.toml`, …).
    pub root: PathBuf,

    /// The package name as it appears in the ecosystem registry (e.g.
    /// `"github.com/user/repo"` for Go, `"MyLib"` for NuGet).
    ///
    /// Used as the root-module [`Symbol`] name when constructing the
    /// [`Lowering`] and as the human-readable label in error messages.
    ///
    /// [`Symbol`]: nudox_ir::entry::Symbol
    pub name: PackageName,

    /// The ecosystem-specific version string (`"1.2.3"`, `"v0.4.0-beta"`, …).
    ///
    /// Stored for diagnostics and for the manifest generation stamp; not
    /// interpreted by this crate.
    pub version: String,
}

impl PackageSource {
    /// Construct a [`PackageSource`] from its components.
    pub fn new(
        root: impl Into<PathBuf>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            root: root.into(),
            name: PackageName::new(name),
            version: version.into(),
        }
    }

    /// Borrow the root path.
    #[inline]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

// ── ProducerId ────────────────────────────────────────────────────────────────

/// A stable, versioned producer identity string.
///
/// The convention is `"<lang>-<tier>/<version>"` — e.g. `"go-oracle/1"`,
/// `"csharp-roslyn/2"`.  The version suffix must be bumped whenever the
/// producer's output shape changes so that cached IR can be invalidated.
///
/// This identifier becomes part of the job key once CAS wiring lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProducerId(pub &'static str);

impl fmt::Display for ProducerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

// ── Yield contract ────────────────────────────────────────────────────────────

/// Why a producer contributes nothing beyond the root module in *this* build.
///
/// It is a type rather than a `String` field on [`YieldContract`] so the reason
/// can occupy a `#[source]` slot on [`ProducerError::YieldContractOutgrown`] and
/// be reached by walking `std::error::Error::source` — the diagnosis path
/// AGENTS-DOCTRINE.md §8 prescribes — instead of being flattened into a message
/// the moment it crosses a layer. It is the [`YieldContract`] analogue of
/// `nudox_producer_rust::NoDepsFallback`.
///
/// `blocker` is prose because the set of reasons is open (an unbuilt oracle, an
/// absent toolchain, a dependency conflict) and because the reader's next action
/// is always to go read it, never to branch on it. What is *not* prose is the
/// fact of the degradation: that is [`YieldContract::RootOnly`], and it is
/// enforced.
#[derive(Debug, Clone, Error)]
#[error("`{producer}` cannot analyse sources in this build: {blocker}")]
pub struct DegradedYield {
    producer: ProducerId,
    blocker: String,
}

impl DegradedYield {
    /// Declare that `producer` contributes nothing, naming what stops it.
    ///
    /// `blocker` must name the *real, current* obstruction concretely enough
    /// that a reader can check whether it still holds — "pyrefly feature off"
    /// is useless, "`src/context.rs` is absent and pyrefly pins `blake3 =1.8.2`
    /// against iroh's `^1.8.3`" is actionable. This string is the only channel
    /// through which the degradation explains itself.
    pub fn new(producer: ProducerId, blocker: impl Into<String>) -> Self {
        Self {
            producer,
            blocker: blocker.into(),
        }
    }

    /// The producer that declared the degradation.
    pub fn producer(&self) -> ProducerId {
        self.producer
    }

    /// What stops this producer from contributing declarations.
    pub fn blocker(&self) -> &str {
        &self.blocker
    }
}

/// What a producer guarantees it contributes to the sink **beyond the root
/// module [`produce`] synthesizes for it**.
///
/// # Why this is a type and not a length check
///
/// [`produce`] builds the root [`Symbol`] itself and hands it to
/// [`Lowering::new`] before `lower` is ever called, so a producer that does
/// nothing at all still seals a table containing exactly one entry. A
/// `table.is_empty()` guard therefore catches nothing: it cannot distinguish
/// "this producer analysed the package and found no public API" from "this
/// producer never read a byte of `src` and returned `Ok`". Both are `Ok`, both
/// carry one entry, and the second is the failure AGENTS-DOCTRINE.md §8 calls
/// "a real failure converted into a success with the cause parked somewhere a
/// caller need not look" — the same defect class as the `--no-deps` fallback,
/// wearing a producer's hat.
///
/// The question with a real answer is *"did the producer contribute
/// declarations of its own?"*, and the only party who knows what the answer
/// ought to be is the producer. So the producer states it, once, on its
/// definition — and [`produce`] holds it to the statement in **both**
/// directions ([`ProducerError::NoDeclarationsContributed`] and
/// [`ProducerError::YieldContractOutgrown`]). A producer that is degraded today
/// and repaired tomorrow fails loudly on the day of the repair rather than
/// keeping a stale claim forever.
///
/// # Why the default is [`Declarations`](Self::Declarations)
///
/// [`Producer::yield_contract`] defaults to the strong variant so that a newly
/// written producer gets the loud behaviour *by omission*. Opting out of it has
/// to be typed out, with a reason, in the producer's own source — where review
/// sees it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum YieldContract {
    /// The producer analyses its [`PackageSource`] and declares entries from
    /// what it finds there. A run that ends with only the synthesized root is a
    /// failure, not an empty package.
    Declarations,

    /// The producer cannot analyse its [`PackageSource`] in this build and will
    /// contribute nothing. Carries the blocker so a reader can act on it rather
    /// than merely notice it.
    RootOnly(DegradedYield),
}

impl YieldContract {
    /// The declared degradation, when the producer has declared one.
    ///
    /// This exists so callers outside this crate can ask "is this producer
    /// inert, and if so why?" *totally*, without a wildcard arm. The enum is
    /// `#[non_exhaustive]` because a future producer could report a third kind
    /// of partial yield (declarations from a subset of source files, say);
    /// making every external call site write `_ => {}` today would silently
    /// absorb that variant everywhere instead of forcing a decision. Routing the
    /// question through this projection puts the decision in one reviewable
    /// place — the same shape, and the same reasoning, as
    /// `nudox_producer_rust::DependencyResolution::no_deps`.
    ///
    /// [`enforce_yield_contract`] matches the variants directly rather than
    /// going through this: it lives in this crate, where `#[non_exhaustive]`
    /// does not apply, so a new variant *must* break the one enforcement point.
    pub fn degraded(&self) -> Option<&DegradedYield> {
        match self {
            Self::Declarations => None,
            Self::RootOnly(degraded) => Some(degraded),
        }
    }

    /// Whether this producer has declared that it contributes nothing.
    ///
    /// The predicate half of [`Self::degraded`], for callers that only branch
    /// and never report.
    pub fn is_degraded(&self) -> bool {
        match self {
            Self::Declarations => false,
            Self::RootOnly(_) => true,
        }
    }
}

// ── ProducerError ─────────────────────────────────────────────────────────────

/// Everything that can go wrong during a single producer run.
///
/// Error messages name the package and — where known — the failing symbol.
/// A message that says only `"decode error"` is useless at scale; prefer
/// `"go-oracle/1: failed to decode oracle output for github.com/user/repo: <reason>"`.
#[derive(Debug, Error)]
pub enum ProducerError {
    /// The oracle subprocess exited with a non-zero code.
    ///
    /// Carries the command that failed, the captured `stderr`, and the exit
    /// code as a string.  The exit code is a string so that signal-terminated
    /// processes (`"signal: 9"`) and numeric codes can both be represented.
    // The command, code and stderr have **no** `#[source]`, so unlike
    // `OracleSpawn`/`Decode` there is no error chain to walk for them. Left out
    // of the message they were unreachable from every diagnostic surface — the
    // terse-`Display`-plus-walk-the-chain rule only works when a chain exists.
    #[error("oracle `{command}` exited with status {code}: {stderr}")]
    OracleExit {
        /// The command line label (binary + first argument) for diagnostics.
        command: String,
        /// Exit code or signal description.
        code: String,
        /// The captured stderr bytes, lossily decoded.
        stderr: String,
    },

    /// The oracle subprocess could not be spawned at all (binary missing, I/O
    /// error before the process started, …).
    #[error("oracle spawn failed")]
    OracleSpawn {
        /// The command that could not be spawned.
        command: String,
        /// The underlying I/O error.
        #[source]
        reason: std::io::Error,
    },

    /// Deserializing the oracle's JSON stdout into the expected type failed.
    // `reason` stays a `#[source]`; only `package` is added here, because it is
    // the one field with no other route to a reader. This is what this enum's
    // own doc comment above already promises.
    #[error("failed to decode oracle output for {package}")]
    Decode {
        /// Package being processed when the decode failed.
        package: String,
        /// The serde_json error.
        #[source]
        reason: serde_json::Error,
    },

    /// [`Lowering::finish`] reported a structural error in the declarations
    /// emitted by the producer (undeclared IDs, duplicates, or a parent-pointer
    /// cycle).
    ///
    /// This always indicates a bug in the producer's `lower` implementation.
    ///
    /// The source is boxed because `LoweringError<P::Id>` is generic over the
    /// producer's id type — it cannot be named concretely here without making
    /// `ProducerError` generic.  The original `LoweringError` is preserved in
    /// the `#[source]` chain.
    #[error("lowering failed")]
    LoweringFailed {
        /// Package being processed when the error was detected.
        package: String,
        /// The concrete `LoweringError<P::Id>`.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The oracle was built from an incomplete dependency graph, so a lowering
    /// from it would be silently partial.
    ///
    /// This is neither a load failure nor a producer bug — the oracle exists and
    /// the walk would succeed. It is the case where the *environment* could not
    /// give the producer everything the package declares: a build tool that
    /// resolved no dependencies, a toolchain that could not fetch them, an
    /// offline cache missing an entry. The resulting table looks complete and is
    /// not, which is why it needs a variant of its own rather than being folded
    /// into [`ProducerError::LoweringFailed`] (whose contract is "the producer
    /// has a bug") or [`ProducerError::UnsupportedConstruct`] (which flattens
    /// its cause into a string and ends the `#[source]` chain there).
    ///
    /// Recover by making the dependencies resolvable, or — if a
    /// dependency-free lowering is genuinely wanted — by opting in through
    /// whatever the producer offers for that (the Rust producer uses
    /// `LoadedWorkspace::accept_degraded_dependencies`).
    #[error("dependency graph unresolved")]
    DependenciesUnresolved {
        /// Package whose dependencies did not resolve.
        package: String,
        /// The producer-specific resolution failure, kept whole so callers can
        /// walk it — this is the only thing that says *which* dependency and
        /// *why*.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The oracle was built without the output of the package's own build
    /// scripts, so every conditionally-compiled item in it is wrong.
    ///
    /// [`ProducerError::DependenciesUnresolved`]'s sibling, and deliberately
    /// not folded into it: an unresolved dependency graph makes declarations
    /// *missing*, which a reader can at least recognise as a gap, while a
    /// missing build-script `cfg` makes them **wrong** — the gated item vanishes
    /// and whatever the package wrote behind `#[cfg(not(...))]` is presented in
    /// its place as though the author had chosen it. Merging the two would mean
    /// a caller who decided how to handle a partial table had silently also
    /// decided how to handle a counterfeit one.
    ///
    /// Like `DependenciesUnresolved` this is an *environment* outcome, not a
    /// producer bug: the producer's walk is fine and would happily lower the
    /// wrong graph it was handed. Recover by fixing whatever stopped the build
    /// scripts running, or — if a lowering with no build-script cfgs is
    /// genuinely wanted — by opting in through whatever the producer offers for
    /// that (the Rust producer uses
    /// `LoadedWorkspace::accept_missing_build_script_cfgs`).
    #[error("build scripts did not run")]
    BuildScriptsFailed {
        /// Package whose build-script output is missing.
        package: String,
        /// The producer-specific failure, kept whole so callers can walk it —
        /// this is the only thing that says what the build tool actually said.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The producer promised declarations ([`YieldContract::Declarations`]) and
    /// contributed none: the lowering holds nothing but the root module
    /// [`produce`] synthesized for it.
    ///
    /// This is the one failure that has no natural error site — the producer
    /// returned `Ok` from both `invoke` and `lower`, `Lowering::finish`
    /// validated a structurally perfect table, and `seal` sealed it. Everything
    /// succeeded; nothing happened. It is its own variant because the reader's
    /// question ("did we look at this package, or only appear to?") is answered
    /// by neither [`ProducerError::LoweringFailed`] (whose contract is "the
    /// producer has a bug" — this producer may be flawless and merely inert) nor
    /// [`ProducerError::DependenciesUnresolved`] (which is specifically about an
    /// incomplete dependency graph behind an otherwise working oracle).
    ///
    /// A package with a genuinely empty public API lands here too, and that is
    /// deliberate: it is indistinguishable at this seam from a producer that
    /// never ran, so it must be surfaced rather than counted as a success. The
    /// caller that knows the difference is the one that should decide.
    ///
    /// `source` is optional because the generic gate in [`produce`] has, by
    /// construction, no cause to offer — the absence of one *is* the failure. A
    /// producer that detects the same condition earlier and knows why (the Rust
    /// producer's "no documented packages found", which used to be mistyped as a
    /// `NotFound` I/O error) fills it in.
    #[error(
        "`{producer}` contributed no declarations for `{package}`: the lowering holds only the \
         root module `produce` synthesized, which is exactly what a producer that never read a \
         byte of the package also yields. Fix the oracle so it declares what it finds — or, if \
         this producer genuinely cannot analyse sources in this build, override \
         `Producer::yield_contract` to return `YieldContract::RootOnly` naming the blocker"
    )]
    NoDeclarationsContributed {
        /// Package that came back with nothing.
        package: String,
        /// The producer that declared it would contribute declarations.
        producer: ProducerId,
        /// The producer-specific reason, when it knows one.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// The producer declared [`YieldContract::RootOnly`] and then contributed
    /// declarations anyway — the degradation it advertises no longer holds.
    ///
    /// This is the direction that keeps a declared degradation from rotting. A
    /// `RootOnly` declaration is a claim about the present, and the day the
    /// blocker behind it is removed the claim becomes a lie that silently
    /// discredits real output: downstream tooling that trusts the declaration
    /// would keep reporting the producer as inert while it was working. Failing
    /// here means the repair cannot land without the claim being retracted in
    /// the same change.
    ///
    /// Recover by deleting the [`Producer::yield_contract`] override, or by
    /// narrowing the `cfg` that selects it, so the producer's output is credited
    /// honestly.
    #[error(
        "`{producer}` declares `YieldContract::RootOnly` but contributed {contributed} \
         declarations for `{package}`: the degradation it advertises no longer holds, so its \
         real output would be reported as an inert stub. Remove the `Producer::yield_contract` \
         override, or narrow the `cfg` that selects it"
    )]
    YieldContractOutgrown {
        /// Package whose lowering contradicted the declaration.
        package: String,
        /// The producer whose declaration is now stale.
        producer: ProducerId,
        /// How many declarations it contributed beyond the synthesized root.
        /// Derived from the lowered entries, never tallied alongside them.
        contributed: usize,
        /// The now-false degradation, kept whole so a reader can see which
        /// blocker was claimed and check whether it still exists.
        #[source]
        declared: DegradedYield,
    },

    /// The producer encountered a language construct it does not yet know how
    /// to lower.
    ///
    /// `package` and `symbol` identify where the construct was found.
    /// This is not fatal: the producer may continue and simply omit the symbol.
    /// When the producer chooses to surface it as an error, it uses this variant.
    #[error("unsupported construct")]
    UnsupportedConstruct {
        /// Package containing the unsupported construct.
        package: String,
        /// Symbol name or path where the unsupported construct was encountered.
        symbol: String,
        /// Short description of what the construct is.
        description: String,
    },
}

// ── Producer trait ────────────────────────────────────────────────────────────

/// The contract every language frontend implements.
///
/// A producer is stateless by design: it carries only compile-time constants
/// and immutable configuration, not per-run state.  The two-phase structure
/// (`invoke` then `lower`) keeps deserialized oracle data in `Self::Oracle` —
/// owned by the caller across the `lower` call — so producers can borrow from
/// it freely without cloning.
///
/// # Type parameters
///
/// * `Self::Id` — the producer's native symbol identifier (Go object path,
///   C# `DocId`, rustdoc numeric id, …).  The [`Lowering`] sink is generic
///   over this type, so producers declare directly in their own id-space;
///   they never invent a path scheme or build an intermediate tree.
///
/// * `Self::Oracle` — the deserialized oracle output, owned across `lower`.
///   For subprocess producers (Go, C#, Java) this is the top-level JSON
///   document struct.  For library producers (Python, TypeScript) it may be
///   an in-process analysis result.
///
/// # Borrowing from Oracle
///
/// `lower` takes `&Self::Oracle` rather than consuming it.  Produce borrowed
/// `&str` slices, `&[T]` slices, and paths directly from oracle fields where
/// possible; clone only when the IR type requires an owned value.
pub trait Producer {
    /// The producer's native symbol identifier.
    ///
    /// `Eq + Hash + Clone + fmt::Debug` is what [`Lowering`] needs to use the
    /// value as a key in its internal `IndexMap`.
    ///
    /// `Send + Sync + 'static` is what the *error* path needs, and it is a
    /// contract of this trait rather than an incidental bound: a producer runs
    /// on a `spawn_blocking` thread (see `nudox_store::source::producer`), so a
    /// [`LoweringError<Self::Id>`] must be able to travel back to the thread
    /// that scheduled it. [`ProducerError::LoweringFailed`] erases that error
    /// into a `Box<dyn Error + Send + Sync>`; without these bounds the erasure
    /// is impossible and the failure has nowhere to go but a panic. Stating the
    /// bounds here fails the *producer definition* rather than one call site.
    ///
    /// [`LoweringError<Self::Id>`]: nudox_ir::lower::LoweringError
    type Id: Eq + Hash + Clone + fmt::Debug + Send + Sync + 'static;

    /// The deserialized oracle output.
    type Oracle;

    /// Stable versioned identity string.
    ///
    /// Convention: `"<lang>-<tier>/<version>"` — e.g. `"go-oracle/1"`.
    /// Bump the version suffix whenever the output shape changes.
    const ID: ProducerId;

    /// The source language this producer lowers.
    ///
    /// Used to label body facts and to select language-specific defaults.
    /// The value is `nudox_ir::body::Language`, not `heart::Language` —
    /// nudox-ir deliberately carries its own minimal `Language` enum to
    /// avoid depending on `heart`.
    const LANGUAGE: Language;

    /// Run the language's oracle over `src` and deserialize its output.
    ///
    /// For subprocess producers (Go, C#, Java) this should call
    /// [`oracle::run_json`] and return the deserialized output.
    /// For library producers that run in-process this should perform the
    /// equivalent analysis.
    ///
    /// This method **must not** emit declarations into any IR sink; that is
    /// `lower`'s job.  The split allows the caller to hold the oracle output
    /// alive across the `lower` call without a borrow conflict.
    fn invoke(&self, src: &PackageSource) -> Result<Self::Oracle, ProducerError>;

    /// What this producer guarantees it contributes to the sink beyond the root
    /// module [`produce`] synthesizes for it.
    ///
    /// Defaulted to [`YieldContract::Declarations`] on purpose. A producer that
    /// says nothing is claiming it does real work, and [`produce`] will fail it
    /// if it does not — so the loud behaviour is what a newly written producer
    /// gets *by omission*, and going quiet costs an override with a written
    /// reason. The reverse default (silence means "inert is fine") is how a stub
    /// comes to be counted as a success for months.
    ///
    /// Override it only where the producer genuinely cannot analyse its input
    /// in the build being compiled, and scope the override to exactly those
    /// builds. Putting the `#[cfg]` on the whole method is the cleanest way to
    /// do that — the capable build then has no override at all and picks up the
    /// strong default automatically, with no second place to keep in sync. That
    /// is what `nudox_producer_python::PythonProducer` does with
    /// `#[cfg(not(feature = "pyrefly"))]`.
    ///
    /// [`produce`] enforces the declaration in both directions, so an override
    /// left in place after its blocker is gone fails as
    /// [`ProducerError::YieldContractOutgrown`] the first time the producer
    /// declares anything. The claim cannot outlive the condition it describes.
    ///
    /// Takes `&self` rather than being an associated constant because
    /// [`DegradedYield`] owns its reason string, and because a producer whose
    /// capability depends on runtime configuration (a resolved oracle path, a
    /// discovered toolchain) must be able to answer from its own state.
    fn yield_contract(&self) -> YieldContract {
        YieldContract::Declarations
    }

    /// Emit declarations into the sink.
    ///
    /// One pass; no intermediate tree.  Call [`Lowering::declare`] for each
    /// symbol and [`Lowering::refer`] for forward references.  The sink is
    /// order-independent: parents need not be declared before children.
    ///
    /// Borrow from `oracle` as much as possible to avoid unnecessary clones.
    /// The oracle is held alive by the caller for the duration of this call.
    fn lower(
        &self,
        oracle: &Self::Oracle,
        out: &mut Lowering<Self::Id>,
    ) -> Result<(), ProducerError>;
}

// ── produce ───────────────────────────────────────────────────────────────────

/// Drive a producer end to end: `invoke → lower → finish → seal`.
///
/// This is the whole pipeline in about ten lines.  [`Lowering::finish`]
/// already validates undeclared IDs, duplicates, and parent-pointer cycles;
/// [`LoweringError`] is surfaced as [`ProducerError::LoweringFailed`] with
/// the package name attached.
///
/// [`LoweringError`]: nudox_ir::lower::LoweringError
///
/// # The yield contract
///
/// Between `finish` and `seal`, `produce` holds the producer to its
/// [`Producer::yield_contract`]. This is the only place that check exists, and
/// it is why "the oracle never ran" cannot arrive downstream wearing the same
/// shape as "the package is small" — see [`YieldContract`] for the full
/// argument.
///
/// # Errors
///
/// Any error from `invoke`, `lower`, or `Lowering::finish` propagates
/// as a typed [`ProducerError`] variant.  No error is silently swallowed.
/// A lowering that disagrees with the producer's declared [`YieldContract`]
/// fails as [`ProducerError::NoDeclarationsContributed`] or
/// [`ProducerError::YieldContractOutgrown`] — an `Ok` return therefore means
/// "this producer contributed what it said it would", not merely "nothing threw".
pub fn produce<P: Producer>(
    producer: &P,
    src: &PackageSource,
    lineage: &PackageLineageId,
    imports: &dyn ForeignResolver,
) -> Result<Produced, ProducerError> {
    let oracle = producer.invoke(src)?;

    // Root symbol name = the package name; span and metadata are left at
    // their defaults.  Producers that need a richer root can build it via
    // the lower() call before the sink is consumed.
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

    let mut sink: Lowering<P::Id> = Lowering::new(pkg_id, root_sym);
    producer.lower(&oracle, &mut sink)?;

    let ir_package = sink.finish().map_err(|err| ProducerError::LoweringFailed {
        package: src.name.as_str().to_owned(),
        source: Box::new(err),
    })?;

    // Checked before `seal` so a root-only run does not pay for
    // content-addressing a table nobody should trust.
    let contract = enforce_yield_contract(producer, src, &ir_package)?;

    let outcome = ir_package.seal(lineage, imports);
    Ok(Produced {
        table: outcome.table,
        report: outcome.report,
        contract,
    })
}

/// Hold `producer` to its [`Producer::yield_contract`] against a finished
/// lowering, and hand back the contract to attach to the output.
///
/// # Why this is `pub` rather than a private step inside [`produce`]
///
/// [`produce`] is *not* the only pipeline in this workspace. A producer that
/// needs something `produce` cannot return writes its own — today that is
/// `nudox_producer_rust::produce_with_occurrences`, which repeats
/// `invoke → lower → finish → seal` verbatim in order to also return occurrence
/// facts. A gate that lived inside `produce`'s body would simply not exist on
/// that path, and the one producer with a bespoke pipeline would be the one
/// producer exempt from the contract. Extracting it means every pipeline calls
/// the same function, and a reviewer looking at a new pipeline can ask one
/// question ("does it call this?") instead of re-deriving the rule.
///
/// Call it after [`Lowering::finish`] and before `seal`: a lowering that is
/// about to be rejected should not pay to be content-addressed first.
///
/// # Errors
///
/// [`ProducerError::NoDeclarationsContributed`] when the producer promised
/// declarations and the lowering holds only the synthesized root;
/// [`ProducerError::YieldContractOutgrown`] when it declared
/// [`YieldContract::RootOnly`] and contributed anyway.
pub fn enforce_yield_contract<P: Producer>(
    producer: &P,
    src: &PackageSource,
    lowered: &nudox_ir::package::IrPackage<P::Id>,
) -> Result<YieldContract, ProducerError> {
    // `contributed` is *derived from the entries themselves*, not tallied
    // alongside them (AGENTS-DOCTRINE.md §8: "the count is derived from the
    // repaired values, never accumulated alongside them"). It is exact rather
    // than `len() - 1`: `PackageInfo::export_idx_to_id` maps export index 0 —
    // and only export index 0 — to `None`, and index 0 is precisely the root
    // `Symbol` the pipeline synthesized before `lower` was called. So "entries
    // carrying a producer id" *is* "declarations this producer contributed",
    // with no arithmetic that a future change to the root's representation
    // could quietly invalidate.
    let contributed = lowered.iter().filter(|(id, _)| id.is_some()).count();

    // Matched directly rather than through `YieldContract::degraded()`:
    // `#[non_exhaustive]` does not apply within the defining crate, so a future
    // variant breaks this arm and forces whoever adds it to say what enforcement
    // should do about it.
    let contract = producer.yield_contract();
    match &contract {
        YieldContract::Declarations if contributed == 0 => {
            Err(ProducerError::NoDeclarationsContributed {
                package: src.name.as_str().to_owned(),
                producer: P::ID,
                source: None,
            })
        }
        YieldContract::RootOnly(declared) if contributed > 0 => {
            Err(ProducerError::YieldContractOutgrown {
                package: src.name.as_str().to_owned(),
                producer: P::ID,
                contributed,
                declared: declared.clone(),
            })
        }
        YieldContract::Declarations | YieldContract::RootOnly(_) => Ok(contract),
    }
}

/// A sealed package plus what `seal` observed while sealing it.
///
/// The report is a **field**, not a log line, because a `warn!` stops no
/// caller. The canonical example on this program is `cargo metadata` failing in
/// 20 of 22 fixtures: upstream converted the failure into an `Ok` with the
/// cause parked where nobody looked, and every cost-per-entry number in the
/// corpus was taken through it.
#[derive(Debug)]
pub struct Produced {
    /// The materialized, content-addressed table.
    pub table: PristineIntroTable,
    /// Unlinked cross-package references, forced disambiguations, and any
    /// residual identity collision. See [`SealReport`].
    pub report: SealReport,
    /// The [`YieldContract`] this table was produced under.
    ///
    /// # Why this is here and not only in the error path
    ///
    /// [`produce`] rejects a producer whose output *contradicts* its
    /// declaration, but a producer that declares [`YieldContract::RootOnly`]
    /// and contributes nothing is consistent, so it returns `Ok`. Without this
    /// field that `Ok` would be byte-indistinguishable from a real one-symbol
    /// package, and every consumer that tallies successes would count it as a
    /// success — which is precisely the register finding this whole contract
    /// exists to answer ("26 of 154 corpus entries return Ok with a stub table
    /// and are counted as successes").
    ///
    /// AGENTS-DOCTRINE.md §8 states the rule this field implements: a degraded
    /// case may travel only if it is *typed* — "the output carries which repair
    /// happened, in-band, on the thing repaired … a mandatory field with no
    /// `Default` on the value that crosses the seam, so the repaired case
    /// cannot be constructed without answering the question". [`YieldContract`]
    /// deliberately has no `Default` impl, and this field is not `Option`, so
    /// there is no way to build a `Produced` that declines to say.
    ///
    /// The counting rule is the same one `report` follows: derive the tally
    /// from this field on the values you actually have, never accumulate it
    /// alongside them.
    pub contract: YieldContract,
}
