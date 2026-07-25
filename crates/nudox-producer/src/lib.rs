//! The shared base contract for every language producer in the nudox-ir pipeline.
//!
//! # What this crate is
//!
//! Seven language frontends (Rust, Go, Java, C#, Python, TypeScript, Nix) lower
//! their oracle output into the IR via [`Lowering`].  This crate defines the
//! contract they all implement: [`Producer`], the pipeline driver [`produce`], and
//! the types those two depend on.
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
    lower::{Lowering, LoweringError},
    package::PackageId,
};

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

// ── ProducerError ─────────────────────────────────────────────────────────────

/// Everything that can go wrong during a single producer run.
///
/// Error messages name the package and — where known — the failing symbol.
/// A message that says only `"decode error"` is useless at scale; prefer
/// `"go-oracle/1: failed to decode oracle output for github.com/user/repo: <reason>"`.
#[derive(Debug)]
pub enum ProducerError {
    /// The oracle subprocess exited with a non-zero code.
    ///
    /// Carries the command that failed, the captured `stderr`, and the exit
    /// code as a string.  The exit code is a string so that signal-terminated
    /// processes (`"signal: 9"`) and numeric codes can both be represented.
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
    OracleSpawn {
        /// The command that could not be spawned.
        command: String,
        /// The underlying I/O error message.
        reason: String,
    },

    /// Deserializing the oracle's JSON stdout into the expected type failed.
    ///
    /// `package` is the [`PackageSource::name`] being processed.
    /// `reason` is the [`serde_json`] error rendered as a string.
    Decode {
        /// Package being processed when the decode failed.
        package: String,
        /// The `serde_json` error description.
        reason: String,
    },

    /// [`Lowering::finish`] reported a structural error in the declarations
    /// emitted by the producer (undeclared IDs, duplicates, or a parent-pointer
    /// cycle).
    ///
    /// This always indicates a bug in the producer's `lower` implementation.
    LoweringFailed {
        /// Package being processed when the error was detected.
        package: String,
        /// The full [`LoweringError`] description.
        detail: String,
    },

    /// The producer encountered a language construct it does not yet know how
    /// to lower.
    ///
    /// `package` and `symbol` identify where the construct was found.
    /// This is not fatal: the producer may continue and simply omit the symbol.
    /// When the producer chooses to surface it as an error, it uses this variant.
    UnsupportedConstruct {
        /// Package containing the unsupported construct.
        package: String,
        /// Symbol name or path where the unsupported construct was encountered.
        symbol: String,
        /// Short description of what the construct is.
        description: String,
    },
}

impl fmt::Display for ProducerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProducerError::OracleExit {
                command,
                code,
                stderr,
            } => {
                write!(f, "oracle `{command}` exited with {code}")?;
                if !stderr.is_empty() {
                    write!(f, ": {stderr}")?;
                }
                Ok(())
            }
            ProducerError::OracleSpawn { command, reason } => {
                write!(f, "failed to spawn oracle `{command}`: {reason}")
            }
            ProducerError::Decode { package, reason } => {
                write!(f, "failed to decode oracle output for `{package}`: {reason}")
            }
            ProducerError::LoweringFailed { package, detail } => {
                write!(
                    f,
                    "lowering error in `{package}` (producer bug): {detail}"
                )
            }
            ProducerError::UnsupportedConstruct {
                package,
                symbol,
                description,
            } => {
                write!(
                    f,
                    "unsupported construct in `{package}` at `{symbol}`: {description}"
                )
            }
        }
    }
}

impl std::error::Error for ProducerError {}

impl<Id: fmt::Debug> From<LoweringError<Id>> for ProducerError {
    fn from(err: LoweringError<Id>) -> Self {
        // The package name is not available in this blanket conversion; callers
        // that know the package name should construct LoweringFailed directly
        // (via `produce`, which always supplies it).
        ProducerError::LoweringFailed {
            package: String::from("<unknown>"),
            detail: err.to_string(),
        }
    }
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
    /// Must implement `Eq + Hash + Clone + fmt::Debug` so it can be used as a
    /// key in [`Lowering`]'s internal `IndexMap`.
    type Id: Eq + Hash + Clone + fmt::Debug;

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
/// # Errors
///
/// Any error from `invoke`, `lower`, or `Lowering::finish` propagates
/// as a typed [`ProducerError`] variant.  No error is silently swallowed.
pub fn produce<P: Producer>(
    producer: &P,
    src: &PackageSource,
    lineage: &PackageLineageId,
) -> Result<PristineIntroTable, ProducerError> {
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
        detail: err.to_string(),
    })?;

    Ok(ir_package.seal(lineage))
}
