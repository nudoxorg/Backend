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
    apply::{IntroCollision, PristineIntroTable},
    body::Language,
    change::{PackageLineageId, PackageName},
    foreign::ForeignResolver,
    lower::Lowering,
    package::{Escalation, ForcedDisambiguation, PackageId, SealOutcome, SealReport},
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
/// docs/AGENTS-DOCTRINE.md §8 prescribes — instead of being flattened into a message
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
/// carry one entry, and the second is the failure docs/AGENTS-DOCTRINE.md §8 calls
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

// ── Identity contract ─────────────────────────────────────────────────────────

/// How strictly [`enforce_identity_contract`] treats a declaration whose
/// identity survived only by its **ordinal position** among its colliding peers.
///
/// # Why this tier is a knob and dropped declarations are not
///
/// A declaration in [`SealReport::collisions`] is *gone*: `seal` ran out of
/// disambiguators, `PristineIntroTable::try_insert_live` refused it, and the
/// table that ships is missing a public symbol. There is no policy under which
/// that is acceptable, so it is unconditional.
///
/// `Disambiguator::Ordinal` is a weaker failure of the same kind. The
/// declaration survives, but its [`IntroId`] is a function of *where the
/// producer happened to emit it*, which `nudox_ir::intro` states plainly:
/// "an `Ordinal` id is not stable across a producer reordering its output. A
/// report is the mitigation; it is not stability." Every consumer that caches a
/// key — the graph, the MCP tools, cross-version lineage — is therefore holding
/// something that can move for no source-level reason.
///
/// # What the corpus actually says, measured 2026-08-08
///
/// `nudox-store`'s `every_corpus_declaration_gets_a_distinct_content_derived_identity`
/// over 79 provisioned package versions and 446,947 declarations:
///
/// ```text
/// dropped declarations                  0
/// unmapped local references             0
/// ordinal-keyed groups             32,339   in 49 of the 79 packages
/// ```
///
/// So the unconditional half of this gate is green on the whole corpus today —
/// the escalation ladder always reaches a distinct id — and **this knob is the
/// only reason the run is not red**. It is holding back a real, measured,
/// 32,339-group defect, not a hypothetical one.
///
/// By declaration kind, the ordinal-keyed groups are:
///
/// ```text
/// Param 13,601   Field 3,845   Const 3,809   Function 3,395   Module 2,772
/// Reexport 1,480   Static 1,335   Trait 1,310   Alias 600   Record 129
/// ```
///
/// There are **two** independent causes, and only the first is about spans:
///
/// 1. **Degenerate spans.** A producer that writes `0..0` on every declaration
///    collapses the `Span` tier onto `Ordinal` for every collision it makes.
///    Four producers do this, not three — and the largest was missing from the
///    list this comment first carried:
///    `typescript/src/emit.rs` (fourteen `span: 0..0` literals; only the
///    `decl.span_start..decl.span_end` path at the bottom of the file is real),
///    `go/src/lower/mod.rs`, `csharp/src/lower.rs`,
///    `python/src/emit/mod.rs`. TypeScript alone accounts for roughly
///    24,000 of the 32,339 groups.
///
/// 2. **The ancestor path is built from parent *names*, so overloads share it.**
///    This one has nothing to do with spans, and clang and Rust — both of which
///    emit real byte offsets — hit it hard. Two overloads of `CLI::App::parse`
///    are distinguished by `Disambiguator::FnOverload`, but their *parameters*
///    are not: each param's key is `(Param, [.., App, parse], "args")`, which
///    is byte-identical across the overload set, and a parameter has no
///    signature skeleton to fall back to. That is why `Param` is the single
///    largest kind above, and it is docs/LIMITATIONS.md L23's residue at corpus
///    scale ("memchr's own return-type `Param` is overwritten by a sibling
///    function's").
///
/// **Flip [`Self::CURRENT`] to [`Self::Reject`] when that measurement reads
/// zero.** Fixing the four degenerate-span producers is necessary and not
/// sufficient: it converts cause 1 into `Escalation::Span`, which is injective
/// but still not version-stable, and it does nothing at all for cause 2. Cause
/// 2 wants a disambiguator that keys a child on its *parent's* identity rather
/// than on its parent's name.
///
/// The knob exists so the drop-a-declaration half of this gate could land hard
/// on day one instead of waiting for four producers and a sealer change, not so
/// that ordinal identity is a permanent state of affairs.
///
/// [`IntroId`]: nudox_ir::change::IntroId
/// [`SealReport::collisions`]: nudox_ir::package::SealReport::collisions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrdinalPolicy {
    /// An ordinal-keyed group travels in [`SealReport::forced`] and does not
    /// fail the run.
    ///
    /// This is *not* "ignored": `forced` is a field on [`Produced`], carrying
    /// the kind, path, name and group size of every group that needed the
    /// tier, and [`enforce_identity_contract`] still lists them in the failure
    /// message when the run fails for a dropped declaration anyway.
    ///
    /// [`SealReport::forced`]: nudox_ir::package::SealReport::forced
    Report,

    /// An ordinal-keyed group fails the run as
    /// [`ProducerError::IdentityNotInjective`].
    Reject,
}

impl OrdinalPolicy {
    /// The single policy every pipeline in this workspace runs under.
    ///
    /// A constant rather than a parameter on purpose: a parameter would let one
    /// pipeline pick `Report` while another picked `Reject`, which is precisely
    /// the per-call-site exemption that [`enforce_yield_contract`] was made
    /// `pub` to prevent. There is one place to flip, and flipping it is a diff
    /// a reviewer sees.
    pub const CURRENT: Self = Self::Report;
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

    /// Sealing could not give every declaration a distinct, content-derived
    /// identity.
    ///
    /// Carried rather than logged because [`IntroId`] is the key the graph, the
    /// MCP tools and cross-version lineage are all built on: a table that lost a
    /// declaration here is not a degraded table, it is a **wrong** one. The
    /// symbol is not marked missing, not rendered greyed out, not counted
    /// anywhere — it simply is not in the product, and every consumer sees a
    /// package whose public API is smaller than its source says. `seal` already
    /// records the fact in [`SealReport`]; before this variant existed the only
    /// production reader of that record was a `tracing::warn!` in
    /// `nudox_store::source::producer` with no subscriber installed, which is
    /// exactly what [`SealReport`]'s own doc calls insufficient — "a `warn!`
    /// stops no caller".
    ///
    /// # The two halves, and why they are one variant
    ///
    /// `lost` is unconditional: `seal` exhausted its escalation ladder and
    /// [`PristineIntroTable::try_insert_live`] rejected the declaration.
    ///
    /// `order_dependent` is gated on [`OrdinalPolicy::CURRENT`] — the
    /// declaration survives, but only because it was the *n*-th of its
    /// colliding group, so its identity moves when the producer reorders its
    /// output. See [`OrdinalPolicy`] for what has to change before that becomes
    /// unconditional too.
    ///
    /// They share a variant because they share a cause and a fix: something
    /// upstream erased the bytes that told two declarations apart. The
    /// canonical instance is `result/cli11-v2.7.2/include/CLI/App.hpp`,
    /// where `parse(std::vector<std::string>&)` and
    /// `parse(std::vector<std::string>&&)` both lower to the identical mutable
    /// reference — `workspace/compiler/languages/clang/src/lower.rs` says so in
    /// its own comment, "No dedicated RValueRef in the IR; model as mutable
    /// reference" — so they mint one base key, one skeleton, one `IntroId`.
    ///
    /// Recover by making the lowering preserve whatever distinguishes the two
    /// declarations in the source. Do **not** recover by widening this gate:
    /// the escalation ladder is already the widening, and this variant is what
    /// says the ladder ran out.
    ///
    /// [`IntroId`]: nudox_ir::change::IntroId
    /// [`SealReport`]: nudox_ir::package::SealReport
    /// [`PristineIntroTable::try_insert_live`]: nudox_ir::apply::PristineIntroTable::try_insert_live
    #[error(
        "sealing `{package}` did not give every declaration a distinct identity: {} \
         declaration(s) were dropped because another minted the same IntroId, and {} \
         group(s) survive only by ordinal position. A dropped declaration is absent from \
         the table, the graph, and every MCP answer, with nothing downstream able to tell \
         it from a symbol the package never had. Fix the lowering that erased what \
         distinguished them.\n{}",
        .lost.len(),
        .order_dependent.len(),
        render_identity_failure(.lost, .order_dependent),
    )]
    IdentityNotInjective {
        /// Package whose sealed table is not injective over its declarations.
        package: String,
        /// Declarations `seal` could not insert, each carrying the `IntroId`
        /// both sides minted and *both* symbols' file and span — enough to open
        /// the two source lines that collided without any further search.
        ///
        /// The whole [`IntroCollision`] is kept rather than a count or a
        /// rendered string: it owns the only surviving copy of the rejected
        /// [`Entry`], and discarding it here would be the `map_err(|_| …)`
        /// mistake at the one place the data still exists.
        ///
        /// [`Entry`]: nudox_ir::entry::Entry
        lost: Vec<IntroCollision>,
        /// Groups that reached `Disambiguator::Ordinal`, so their identity is a
        /// function of the producer's emission order rather than of their
        /// content. Empty unless [`OrdinalPolicy::CURRENT`] is
        /// [`OrdinalPolicy::Reject`], in which case a non-empty list is by
        /// itself enough to fail the run.
        order_dependent: Vec<ForcedDisambiguation>,
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

/// Render the body of [`ProducerError::IdentityNotInjective`]'s message.
///
/// One line per finding, because the reader's next action is to open two source
/// locations and see what the lowering flattened, and a count cannot tell them
/// which.
///
/// The rendering is capped; the *data* is not. Both vectors stay whole on the
/// variant, so a caller that wants all of them has them — only the `Display`
/// text is bounded, so a producer that loses four hundred declarations does not
/// bury the rest of the run's output.
fn render_identity_failure(
    lost: &[IntroCollision],
    order_dependent: &[ForcedDisambiguation],
) -> String {
    /// How many findings of each kind the message spells out in full.
    const SHOWN: usize = 25;

    let mut out = String::new();
    for collision in lost.iter().take(SHOWN) {
        out.push_str("\n  dropped: ");
        out.push_str(&collision.to_string());
    }
    if lost.len() > SHOWN {
        out.push_str(&format!(
            "\n  … and {} further dropped declaration(s)",
            lost.len() - SHOWN
        ));
    }
    for forced in order_dependent.iter().take(SHOWN) {
        out.push_str(&format!(
            "\n  order-dependent: {:?} `{}{}` — {} declarations shared one key",
            forced.kind,
            forced
                .segments
                .iter()
                .map(|s| format!("{s}::"))
                .collect::<String>(),
            forced.name,
            forced.group,
        ));
    }
    if order_dependent.len() > SHOWN {
        out.push_str(&format!(
            "\n  … and {} further order-dependent group(s)",
            order_dependent.len() - SHOWN
        ));
    }
    out
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
    let outcome = enforce_identity_contract(src, outcome)?;

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
    // alongside them (docs/AGENTS-DOCTRINE.md §8: "the count is derived from the
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

/// Hold a sealed package to the one property its identity scheme has to have:
/// **every declaration got a distinct, content-derived [`IntroId`]**.
///
/// # Why this exists at all
///
/// `seal` is infallible by design, and rightly so — a package that cannot be
/// sealed cannot be *shown*, and its report already carries every observation a
/// caller needs. What was missing was anyone holding the caller to reading it.
/// `produce` returned `Ok` no matter how many declarations `seal` had just
/// dropped, and the only production reader of the report was a `tracing::warn!`
/// with no subscriber installed. The whole defect class this workspace keeps
/// finding — docs/AGENTS-DOCTRINE.md §8's `map_err(|_| …)`, the `--no-deps` fallback,
/// the yield contract — is a real failure converted into a success with the
/// cause parked somewhere a caller need not look. This is that shape, wearing
/// `SealReport`'s hat.
///
/// # Why this is `pub` rather than a private step inside [`produce`]
///
/// The same reason [`enforce_yield_contract`] is, and it is not a hypothetical:
/// `nudox_producer_rust::produce_with_occurrences` repeats
/// `invoke → lower → finish → seal` verbatim. A gate written inside `produce`'s
/// body would simply not exist there, and the one producer with a bespoke
/// pipeline would be the one producer whose tables were allowed to lose
/// declarations. Every pipeline calls this function, so a reviewer looking at a
/// new one asks "does it call this?" instead of re-deriving the rule.
///
/// Call it immediately after `seal`, on the outcome, before anything reads the
/// table: a table that is missing declarations must not be indexed, cached, or
/// rendered first.
///
/// # Why it takes and returns the whole [`SealOutcome`]
///
/// So it cannot be called and ignored. Taking `&SealReport` would let a caller
/// invoke it, drop the `Result`, and carry on with the table it already has;
/// taking the outcome by value means the only way to reach the table is through
/// the `?`.
///
/// # Errors
///
/// [`ProducerError::IdentityNotInjective`] when `seal` dropped a declaration
/// outright, and — once [`OrdinalPolicy::CURRENT`] is
/// [`OrdinalPolicy::Reject`] — when a declaration's identity depends on the
/// producer's emission order.
///
/// [`IntroId`]: nudox_ir::change::IntroId
pub fn enforce_identity_contract(
    src: &PackageSource,
    outcome: SealOutcome,
) -> Result<SealOutcome, ProducerError> {
    enforce_identity_contract_under(src, outcome, OrdinalPolicy::CURRENT)
}

/// [`enforce_identity_contract`] with the ordinal policy supplied rather than
/// read from [`OrdinalPolicy::CURRENT`].
///
/// `pub(crate)` and not one step further. Doctrine §8's rule for a guard is
/// "verify it by mutation: break the verdict deliberately and confirm the suite
/// goes red", and a guard whose strict half is unreachable from any test until
/// three producers in three languages are fixed is a guard nobody has watched
/// fail. This seam lets the test suite watch it fail today. Widening it to
/// `pub` would hand every pipeline the per-call-site exemption that
/// [`OrdinalPolicy::CURRENT`] exists to deny them.
pub(crate) fn enforce_identity_contract_under(
    src: &PackageSource,
    outcome: SealOutcome,
    ordinals: OrdinalPolicy,
) -> Result<SealOutcome, ProducerError> {
    // Derived from the report's own rows, never tallied alongside them
    // (docs/AGENTS-DOCTRINE.md §8). `seal` re-mints *whole groups* and never just
    // the losers — `package/seal.rs` says so where it does it — so
    // `SealReport::forced` carries exactly one row per colliding group and
    // filtering it cannot double-count.
    let order_dependent: Vec<ForcedDisambiguation> = match ordinals {
        OrdinalPolicy::Reject => outcome
            .report
            .forced
            .iter()
            .filter(|forced| match forced.escalated_to {
                Escalation::Ordinal => true,
                // `Span` is not stable across versions either, and that is a
                // real defect — see the schema's own account of the tiers. It
                // is not *this* gate's defect: a span-keyed id is at least a
                // function of the declaration, so the table is injective and
                // nothing was lost. Version stability is tracked separately by
                // `unchanged_declarations_keep_their_intro_id_across_versions`.
                Escalation::Span => false,
            })
            .cloned()
            .collect(),
        OrdinalPolicy::Report => Vec::new(),
    };

    if outcome.report.collisions.is_empty() && order_dependent.is_empty() {
        return Ok(outcome);
    }

    // Destructured rather than cloned: `IntroCollision` owns the only copy of
    // the entry that did not make it into the table.
    let SealOutcome { report, .. } = outcome;
    Err(ProducerError::IdentityNotInjective {
        package: src.name.as_str().to_owned(),
        lost: report.collisions,
        order_dependent,
    })
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
    /// docs/AGENTS-DOCTRINE.md §8 states the rule this field implements: a degraded
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
