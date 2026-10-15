//! Native, project-scoped TSZ parser, binder, and checker authority.
//!
//! A worker owns this cache exclusively. Source edits reparse only changed
//! files; unchanged TSZ `BindResult`s retain their arenas and are borrowed by
//! the cross-file merge. An exact repeat of a project revision reuses the
//! merged program and checker result as well. TSZ currently exposes a whole-
//! project merge/check API, so a changed project revision rebuilds those two
//! project-wide views while preserving the unchanged per-file parse/bind data.
//!
//! The source list is caller-resolved and includes project, declaration, and
//! dependency files. Project configuration, module-resolution inputs, the
//! resolved library set, package-manager identity, and compiler version belong
//! in the explicit environment fingerprint supplied by the owning project
//! authority. This module does not discover files or read ambient
//! configuration from disk.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::Utf8Span;
use sha2::{Digest, Sha256};
use thiserror::Error;
#[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
use tsz::tsz_solver::construction::TypeDatabase;
use tsz_common::ExecutionCheckpoint;

pub use tsz::binder::BinderState as TszBinderState;
pub use tsz::binder::SymbolId as TszSymbolId;
pub use tsz::checker::context::CheckerOptions as TszCheckerOptions;
pub use tsz::checker::diagnostics::Diagnostic as TszDiagnostic;
pub use tsz::checker::state::CheckerState as TszCheckerState;
pub use tsz::common::{ModuleKind as TszModuleKind, ScriptTarget as TszScriptTarget};
pub use tsz::parallel::{
    ProjectCheckerExecution as TszProjectCheckerExecution,
    ProjectFileWorkExhaustion as TszFileWorkExhaustion,
    ProjectFileFormat as TszProjectFileFormat, ProjectFileFormatInput as TszProjectFileFormatInput,
    ProjectModuleRequestKind as TszProjectModuleRequestKind,
    ProjectModuleResolution as TszProjectModuleResolution,
    ProjectModuleResolutionError as TszProjectModuleResolutionError,
    ProjectModuleResolutionTarget as TszProjectModuleResolutionTarget,
};
pub use tsz::parser::{NodeIndex as TszNodeIndex, ParseDiagnostic as TszParseDiagnostic};
pub use tsz::tsz_solver::type_handles::TypeId as TszTypeId;
pub use tsz_common::ProjectSemanticOptions as TszProjectSemanticOptions;
pub use tsz_common::options::checker::DeclarationDiagnosticPolicy as TszDeclarationDiagnosticPolicy;
pub use tsz_common::options::module_detection::ModuleDetectionKind as TszModuleDetectionKind;

#[path = "tsz_query_session.rs"]
mod query_session;
pub use query_session::{TszProjectQuerySession, TszProjectQuerySessionError};

/// Caller-computed identity for a fully resolved TypeScript project context.
///
/// Include the normalized compiler options, tsconfig inheritance and project
/// references, module-resolution inputs, package-manager/lockfile identity,
/// resolved dependency and library content, and the selected TSZ revision.
/// A change invalidates checked program authority. Parse/bind entries have a
/// narrower identity: exact source bytes, parser target, module detection, and
/// the exact library set actually consumed by binding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TszEnvironmentFingerprint([u8; 32]);

impl TszEnvironmentFingerprint {
    /// Creates an identity from the SHA-256 digest owned by the project layer.
    #[must_use]
    pub const fn from_sha256(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// Returns the underlying digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One caller-admitted project, declaration, or dependency source file.
/// `path` is the stable virtual or canonical project path used for module
/// identity; no filesystem lookup or implicit source discovery is performed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TszFileInput {
    /// Stable source path, including a supported TypeScript/JavaScript suffix.
    pub path: String,
    /// UTF-8 source text, moved into TSZ's owning parser.
    pub source: String,
}

/// A source profile TSZ's path-sensitive parser recognizes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TszSourceError {
    /// The source path has no usable filename or extension.
    InvalidPath,
    /// The source extension is outside the TS/JS/JSX/MTS/CTS/CJS set.
    UnsupportedExtension,
    /// A project or its explicit library set lists a stable path more than once.
    DuplicatePath,
    /// TSZ needs at least one source file to construct a project program.
    EmptyProject,
    /// An explicitly admitted library was not valid UTF-8.
    InvalidUtf8,
}

impl std::fmt::Display for TszSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPath => "TypeScript source path must include a filename and extension",
            Self::UnsupportedExtension => "TSZ source path extension is not supported",
            Self::DuplicatePath => "TypeScript project contains a duplicate source path",
            Self::EmptyProject => "TypeScript project requires at least one source file",
            Self::InvalidUtf8 => "TypeScript library source is not valid UTF-8",
        })
    }
}

impl std::error::Error for TszSourceError {}

/// Exact UTF-8 bytes for one caller-selected TypeScript library source.
///
/// Build this from the host's admitted path and content bytes, then pass the
/// resulting `LibFile` to [`TszProjectAuthority::update`]. No filesystem
/// lookup or ambient TypeScript library discovery occurs here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TszLibraryInput {
    path: String,
    source: String,
}

impl TszLibraryInput {
    /// Admits a canonical library path and exact UTF-8 source bytes.
    ///
    /// # Errors
    /// Returns an input error for an unsupported path or invalid UTF-8.
    pub fn from_utf8(
        path: impl Into<String>,
        source: impl Into<Vec<u8>>,
    ) -> Result<Self, TszSourceError> {
        let path = path.into();
        validate_source_path(&path)?;
        let source = String::from_utf8(source.into()).map_err(|_| TszSourceError::InvalidUtf8)?;
        Ok(Self { path, source })
    }

    /// Stable path included in the project library identity.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact UTF-8 text retained for parsing and source-token admission.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Constructs a TSZ library directly from these admitted in-memory bytes.
    #[must_use]
    pub fn into_lib_file(self) -> Arc<tsz::lib_loader::LibFile> {
        Arc::new(tsz::lib_loader::LibFile::from_source(
            self.path,
            self.source,
        ))
    }
}

/// Typed failures at the TSZ adapter boundary.
#[derive(Debug, Error)]
pub enum TszAuthorityError {
    /// The project input could not be admitted before parsing.
    #[error(transparent)]
    Source(#[from] TszSourceError),
    /// A caller queried a file index that is absent from the merged program.
    #[error("TSZ project has no bound file at index {0}")]
    MissingFile(usize),
    /// The selected project has no bound file with this exact stable path.
    #[error("TSZ project has no bound file at path {0}")]
    MissingSource(String),
    /// The engine requested a source path whose retained bytes do not match
    /// the bytes admitted to the current compile lease.
    #[error("TSZ project source bytes do not match compile source at {path}")]
    SourceMismatch {
        /// Stable source path selected by the package authority.
        path: String,
    },
    /// A compiler file projection attempted to use a query session owned by
    /// a different checked project.
    #[error("TSZ query session is bound to a different project")]
    ProjectSessionMismatch,
    /// Exact compiler module-resolution authority could not be attached to
    /// the merged program without guessing a path or weakening a request.
    #[error(transparent)]
    ModuleResolution(#[from] TszProjectModuleResolutionError),
    /// The merged TSZ project could not lend the configured project-scoped
    /// checker for one exact file query.
    #[error(transparent)]
    ProjectCheckerSession(#[from] tsz::parallel::ProjectCheckerSessionError),
    /// A complete project check could not finish under the admitted inputs or
    /// shared execution checkpoint.
    #[error(transparent)]
    ProjectCheck(#[from] tsz::parallel::ProjectProgramCheckError),
    /// The explicit private native checker workers could not be started.
    #[error("TSZ checker worker pool unavailable: {0}")]
    CheckerWorkerPoolUnavailable(String),
    /// A parse, bind, or project merge stopped under the shared execution
    /// checkpoint before a complete project could be published.
    #[error("TSZ project update stopped: {0:?}")]
    ExecutionStopped(tsz_common::ProjectExecutionStop),
    /// Files spent their per-file work allowance in every permitted round, so
    /// no complete checked project exists. The files are exact program paths,
    /// sorted by program index.
    #[error("TSZ project files spent their per-file work allowance")]
    FileWorkExhausted(Vec<TszFileWorkExhaustion>),
}

/// Effective TypeScript compiler options could not be mapped onto the
/// supported native TSZ checker option model.
#[derive(Debug, Error)]
#[error("TypeScript compiler options could not be mapped into TSZ: {message}")]
pub struct TszCompilerOptionsError {
    message: Box<str>,
}

/// Maps compiler-API-normalized options into the exact TSZ checker options.
///
/// The input is one JSON object containing a `compilerOptions` member. The
/// project bridge must first resolve the selected tsconfig with the admitted
/// TypeScript compiler API and materialize its version-specific effective
/// defaults. This function then applies the TSZ config parser's supported
/// option validation and returns the typed checker options consumed by the
/// same project update and query session.
///
/// # Errors
/// Returns a typed failure when the JSON is invalid or TSZ does not recognize
/// a checker option value.
pub fn checker_options_from_compiler_api_json(
    source: &str,
) -> Result<TszCheckerOptions, TszCompilerOptionsError> {
    let config = tsz::config::parse_tsconfig(source).map_err(|error| TszCompilerOptionsError {
        message: error.to_string().into_boxed_str(),
    })?;
    tsz::config::resolve_compiler_options(config.compiler_options.as_ref())
        .map(|resolved| {
            // Resolver options also control checker TS5097 diagnostics. Keep
            // their admitted values when constructing the project checker.
            let mut checker = resolved.checker;
            checker.allow_importing_ts_extensions = resolved.allow_importing_ts_extensions;
            checker.rewrite_relative_import_extensions =
                resolved.rewrite_relative_import_extensions;
            checker
        })
        .map_err(|error| TszCompilerOptionsError {
            message: error.to_string().into_boxed_str(),
        })
}

/// Explicit TSZ checker/binder options resolved by the project configuration layer.
#[derive(Clone, Debug)]
pub struct TszProjectOptions {
    /// Checker options after tsconfig inheritance and defaults are resolved.
    pub checker: TszCheckerOptions,
    /// Typed TSZ semantic policies used by the project merge and checker.
    ///
    /// Keep semantic policy on this project value so type-origin behavior is
    /// stable across Rayon workers and direct per-file query caches.
    pub semantic_options: TszProjectSemanticOptions,
    /// Exact compiler-resolved outcomes for observed imports and source
    /// references. Reference kinds bind their source byte positions as well as
    /// their importer, value, mode and target to this immutable recipe.
    /// Empty is an explicit declaration that the admitted program has no
    /// requests; it does not enable filename-based fallback.
    pub module_resolutions: Vec<TszProjectModuleResolution>,
    /// Actual Compiler API per-file format; missing entries remain unknown.
    pub file_formats: Vec<TszProjectFileFormatInput>,
    /// Identity for all configuration and dependency inputs described above.
    pub environment: TszEnvironmentFingerprint,
}

struct CachedBind {
    digest: [u8; 32],
    result: Arc<tsz::parallel::BindResult>,
}

/// Counts work performed by one project update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TszUpdateReport {
    /// Source files parsed and bound by TSZ during this update.
    pub parsed_and_bound: usize,
    /// Source files whose typed TSZ bind results were reused.
    pub reused_binds: usize,
    /// Removed source files evicted from the project's cache.
    pub removed_sources: usize,
    /// Whether the complete merged/check result was reused unchanged.
    pub reused_project_result: bool,
    /// Rebuild rounds after files spent their per-file work allowance.
    pub file_work_retries: u8,
}

/// Exclusive owner of one TypeScript project's persistent TSZ compile state.
///
/// Keep this value on the project worker and update it serially. The cache has
/// no global mutex; edits invalidate source digests and parse only files whose
/// text or project environment changed.
#[derive(Default)]
pub struct TszProjectAuthority {
    environment: Option<TszEnvironmentFingerprint>,
    libraries: Option<[u8; 32]>,
    binding_options: Option<[u8; 32]>,
    // Cached binders carry library declaration arena provenance. Keep the
    // original library objects whenever exact library bytes are unchanged.
    lib_files: Vec<Arc<tsz::lib_loader::LibFile>>,
    bound_sources: BTreeMap<String, CachedBind>,
    project: Option<TszProject>,
}

/// One real in-process TSZ program, including its typed parser/binder state,
/// merged cross-file symbols, and checker diagnostics.
pub struct TszProject {
    options: TszProjectOptions,
    checker_options_digest: [u8; 32],
    semantic_options_digest: [u8; 32],
    module_resolution_digest: [u8; 32],
    file_format_digest: [u8; 32],
    library_digest: [u8; 32],
    bound_sources: BTreeMap<String, Arc<tsz::parallel::BindResult>>,
    lib_files: Vec<Arc<tsz::lib_loader::LibFile>>,
    program: tsz::parallel::ProjectCheckerImage,
    query_budget: Option<tsz_common::ProjectExecutionIdentity>,
    /// Program files whose semantic check was not run because an earlier round
    /// of this update spent their per-file work allowance (or the caller
    /// already knew they would).
    file_work_skipped: BTreeSet<String>,
    /// Exact requested policy that governed the complete retained result.
    file_work_policy: Option<TszFileWorkPolicy>,
    check: tsz::parallel::CheckResult,
}

impl std::fmt::Debug for TszProject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TszProject")
            .field("source_count", &self.bound_sources.len())
            .field("library_count", &self.lib_files.len())
            .field(
                "source_paths",
                &self.bound_sources.keys().collect::<Vec<_>>(),
            )
            .field("environment", &self.options.environment)
            .field("diagnostic_count", &self.check.diagnostic_count)
            .finish()
    }
}

/// Per-file work isolation for one partitioned project update.
///
/// Every file's semantic check runs under its own `allowance` slice of the
/// project's shared work checkpoint. A file that spends its slice is skipped by
/// a rebuilt project (up to `max_retries` times), so one expensive file costs
/// its own coverage instead of the whole project. A file that stopped part-way
/// may have written partial state that every other file reads, which is why the
/// update rebuilds the merged program rather than reusing the interrupted one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TszFileWorkPolicy {
    /// Maximum work units one file's semantic check may spend.
    pub allowance: u64,
    /// Exact program paths whose semantic check must not run (known hot files).
    pub skipped: BTreeSet<String>,
    /// Rebuild rounds permitted after the first one.
    pub max_retries: u8,
    /// Whether declaration files are semantically checked for diagnostics.
    ///
    /// A caller that publishes semantic facts and never reads checker
    /// diagnostics sets this to `false`: the project then checks declaration
    /// files exactly as `skipLibCheck` would, without changing the resolved
    /// compiler options, source membership, or any query result. Lazy queries
    /// still resolve declaration types on demand.
    pub check_declaration_files: bool,
}

#[derive(Clone, Copy)]
enum TszUpdateExecution<'checkpoint> {
    Unmetered,
    Fresh(&'checkpoint dyn ExecutionCheckpoint),
    Partitions(
        TszProjectCheckerExecution<'checkpoint>,
        Option<&'checkpoint TszFileWorkPolicy>,
    ),
}

impl TszUpdateExecution<'_> {
    fn checkpoint(&self) -> Option<&dyn ExecutionCheckpoint> {
        match self {
            Self::Unmetered => None,
            Self::Fresh(checkpoint) => Some(*checkpoint),
            Self::Partitions(execution, _) => Some(execution.checkpoint),
        }
    }

    fn file_work(&self) -> Option<&TszFileWorkPolicy> {
        match self {
            Self::Partitions(_, policy) => *policy,
            Self::Unmetered | Self::Fresh(_) => None,
        }
    }
}

impl TszProjectAuthority {
    /// Creates an empty project worker cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses, binds, merges, and checks a caller-resolved TypeScript project.
    ///
    /// Unchanged files retain their TSZ arenas and bind results. A changed
    /// source causes TSZ's current project-wide merge/check operations to run
    /// again over borrowed cached results. The explicit `lib_files` must be
    /// the resolved project library set whose identity is covered by
    /// `options.environment`.
    ///
    /// Parse and checker diagnostics remain native typed TSZ diagnostics on
    /// [`TszProject`]; a successful method call means the adapter ran, not
    /// that the source has no diagnostics or that TSZ fully conforms to tsc.
    ///
    /// # Errors
    /// Returns a typed input error for an empty project, unsupported source
    /// suffix, duplicate path, or malformed path.
    pub fn update(
        &mut self,
        sources: Vec<TszFileInput>,
        options: TszProjectOptions,
        lib_files: &[Arc<tsz::lib_loader::LibFile>],
    ) -> Result<TszUpdateReport, TszAuthorityError> {
        self.update_inner(sources, options, lib_files, TszUpdateExecution::Unmetered)
    }

    /// Parses, binds, merges, and checks a caller-resolved project under one
    /// shared cancellation, deadline, and finite-work checkpoint.
    ///
    /// The same checkpoint must be passed to later query sessions opened from
    /// the returned project. If it stops, no new [`TszProject`] is published;
    /// changed projects also invalidate the prior ready result before any
    /// changed source is rebound.
    ///
    /// # Errors
    /// Returns the exact source, module-resolution, project-check, or stop
    /// reason that prevented a complete checked project.
    pub fn update_with_execution_checkpoint(
        &mut self,
        sources: Vec<TszFileInput>,
        options: TszProjectOptions,
        lib_files: &[Arc<tsz::lib_loader::LibFile>],
        checkpoint: &dyn ExecutionCheckpoint,
    ) -> Result<TszUpdateReport, TszAuthorityError> {
        self.update_inner(
            sources,
            options,
            lib_files,
            TszUpdateExecution::Fresh(checkpoint),
        )
    }

    /// Check a project with caller-admitted private checker workers and the
    /// same borrowed deadline/cancellation/work checkpoint used by later
    /// semantic query sessions. The worker bound covers checker planning,
    /// file partitions, and library checking; parse/bind/merge retain their
    /// existing cooperative execution APIs.
    ///
    /// No checked project is published after a stop or worker-start failure.
    /// Worker count is execution policy and does not alter source membership,
    /// exact resolutions, compiler options, or semantic identity.
    ///
    /// # Errors
    /// Retains exact input/project-check/stopped reasons and reports a typed
    /// worker-pool startup refusal without falling back to ambient workers.
    pub fn update_with_checker_execution(
        &mut self,
        sources: Vec<TszFileInput>,
        options: TszProjectOptions,
        lib_files: &[Arc<tsz::lib_loader::LibFile>],
        execution: TszProjectCheckerExecution<'_>,
    ) -> Result<TszUpdateReport, TszAuthorityError> {
        self.update_inner(
            sources,
            options,
            lib_files,
            TszUpdateExecution::Partitions(execution, None),
        )
    }

    /// [`Self::update_with_checker_execution`] with per-file work isolation.
    ///
    /// Each file's semantic check is bounded by `file_work.allowance`. Files
    /// that spend it (or that the policy already names) are not semantically
    /// checked in the returned project; [`TszProject::file_work_skipped`] lists
    /// them, and every other file's result is identical to a project that never
    /// contained the skipped files' checks.
    ///
    /// # Errors
    /// As [`Self::update_with_checker_execution`], plus
    /// [`TszAuthorityError::FileWorkExhausted`] when files still spend their
    /// allowance after `file_work.max_retries` rebuilds.
    pub fn update_with_checker_execution_and_file_work(
        &mut self,
        sources: Vec<TszFileInput>,
        options: TszProjectOptions,
        lib_files: &[Arc<tsz::lib_loader::LibFile>],
        execution: TszProjectCheckerExecution<'_>,
        file_work: &TszFileWorkPolicy,
    ) -> Result<TszUpdateReport, TszAuthorityError> {
        self.update_inner(
            sources,
            options,
            lib_files,
            TszUpdateExecution::Partitions(execution, Some(file_work)),
        )
    }

    fn update_inner(
        &mut self,
        sources: Vec<TszFileInput>,
        options: TszProjectOptions,
        lib_files: &[Arc<tsz::lib_loader::LibFile>],
        execution: TszUpdateExecution<'_>,
    ) -> Result<TszUpdateReport, TszAuthorityError> {
        let execution_scope = execution
            .checkpoint()
            .map(tsz_common::LatchedExecutionCheckpoint::new);
        let checkpoint = execution_scope
            .as_ref()
            .map(|scope| scope as &dyn ExecutionCheckpoint);
        if let Some(checkpoint) = checkpoint {
            checkpoint
                .checkpoint(0)
                .map_err(TszAuthorityError::ExecutionStopped)?;
        }
        let frontier = tsz_common::ProjectExecutionSpan::new(
            checkpoint,
            tsz_common::ProjectExecutionPhase::SourceFrontier,
            None,
        );
        if sources.is_empty() {
            return Err(TszSourceError::EmptyProject.into());
        }

        let mut unique_paths = BTreeSet::new();
        let mut pending = BTreeMap::new();
        for source in sources {
            validate_source_path(&source.path)?;
            if !unique_paths.insert(source.path.clone()) {
                return Err(TszSourceError::DuplicatePath.into());
            }
            let digest = source_digest(&source.path, &source.source);
            pending.insert(source.path.clone(), (digest, source.source));
        }

        let mut report = TszUpdateReport::default();
        for library in lib_files {
            validate_source_path(&library.file_name)?;
            if !unique_paths.insert(library.file_name.clone()) {
                return Err(TszSourceError::DuplicatePath.into());
            }
        }
        let library_digest = library_digest(lib_files)?;
        let libraries_changed = self.libraries != Some(library_digest);
        let environment_changed = self.environment != Some(options.environment);
        let binding_digest = binding_options_digest(&options.checker);
        let binding_options_changed = self.binding_options != Some(binding_digest);
        let checker_digest = checker_options_digest(&options.checker);
        let checker_options_changed = self
            .project
            .as_ref()
            .is_some_and(|current| current.checker_options_digest != checker_digest);
        let semantic_digest = semantic_options_digest(options.semantic_options);
        let semantic_options_changed = self
            .project
            .as_ref()
            .is_some_and(|current| current.semantic_options_digest != semantic_digest);
        let file_format_digest = file_format_digest(&options.file_formats);
        let file_formats_changed = self
            .project
            .as_ref()
            .is_some_and(|current| current.file_format_digest != file_format_digest);
        let module_resolution_digest = module_resolution_digest(&options.module_resolutions);
        let module_resolutions_changed = self
            .project
            .as_ref()
            .is_some_and(|current| current.module_resolution_digest != module_resolution_digest);
        // Parsing/binding consumes only the source, target, module detection,
        // and library binders. The full environment also includes dependency
        // source bytes and resolver reads; rebuilding every unchanged arena
        // for those changes would discard the per-source cache on every edit.
        if libraries_changed {
            self.lib_files = lib_files.to_vec();
        }
        if libraries_changed || binding_options_changed {
            self.bound_sources.clear();
            self.project = None;
        } else if environment_changed || checker_options_changed
            || semantic_options_changed || module_resolutions_changed || file_formats_changed {
            // Reuse exact source binds, but never a checked image or solver
            // cache from another environment, option set, or module authority.
            self.project = None;
        }
        self.environment = Some(options.environment);
        self.libraries = Some(library_digest);
        self.binding_options = Some(binding_digest);
        let lib_files = self.lib_files.as_slice();

        report.removed_sources = self
            .bound_sources
            .keys()
            .filter(|path| !pending.contains_key(*path))
            .count();
        self.bound_sources
            .retain(|path, _| pending.contains_key(path));

        let mut files_to_parse = Vec::new();
        for (path, (digest, source_text)) in pending {
            let is_cached = self
                .bound_sources
                .get(&path)
                .is_some_and(|cached| cached.digest == digest);
            if is_cached {
                report.reused_binds += 1;
            } else {
                files_to_parse.push((path.clone(), source_text));
                self.bound_sources.remove(&path);
            }
        }

        // A previous checked program is no longer Ready as soon as its source
        // frontier changes. Clear it before parsing so a stop never leaves a
        // stale project reachable from this owner.
        if report.removed_sources != 0 || !files_to_parse.is_empty() {
            self.project = None;
        }

        if files_to_parse.is_empty()
            && !environment_changed
            && self.project.as_ref().is_some_and(|current| {
                current.options.environment == options.environment
                    && current.checker_options_digest == checker_digest
                    && current.semantic_options_digest == semantic_digest
                    && current.module_resolution_digest == module_resolution_digest
                    && current.file_format_digest == file_format_digest
                    && current.library_digest == library_digest
                    && current.program.is_usable()
                    // Reuse requires the same exact policy as well as every
                    // source/option/resolution identity. Gapped checks rebuild.
                    && execution.file_work() == current.file_work_policy.as_ref()
                    && current.file_work_skipped.is_empty()
            })
            && report.removed_sources == 0
        {
            if let Some(checkpoint) = checkpoint {
                checkpoint
                    .checkpoint(0)
                    .map_err(TszAuthorityError::ExecutionStopped)?;
            }
            frontier.returned();
            // The immutable checked cache remains valid. Admit the current
            // request's query scope separately after its checkpoint succeeds.
            if let Some(project) = self.project.as_mut() {
                project.query_budget = checkpoint.and_then(|checkpoint| checkpoint.execution_identity());
            }
            report.reused_project_result = true;
            return Ok(report);
        }

        frontier.returned();
        self.project = None;
        report.parsed_and_bound = files_to_parse.len();
        if !files_to_parse.is_empty() {
            let parse = tsz_common::ProjectExecutionSpan::new(
                checkpoint,
                tsz_common::ProjectExecutionPhase::ParseBind,
                None,
            );
            let parsed = match checkpoint {
                Some(checkpoint) => {
                    tsz::parallel::parse_and_bind_parallel_with_libs_and_options_and_execution_checkpoint(
                        files_to_parse,
                        lib_files,
                        options.checker.target,
                        options.checker.module_detection,
                        checkpoint,
                    )
                    .map_err(TszAuthorityError::ExecutionStopped)?
                }
                None => tsz::parallel::parse_and_bind_parallel_with_libs_and_options(
                    files_to_parse,
                    lib_files,
                    options.checker.target,
                    options.checker.module_detection,
                ),
            };
            parse.returned();
            for result in parsed {
                let path = result.file_name.clone();
                let source = result
                    .arena
                    .get_source_file_at(result.source_file)
                    .map(|file| file.text.as_ref())
                    .unwrap_or_default();
                let digest = source_digest(&path, source);
                self.bound_sources.insert(
                    path,
                    CachedBind {
                        digest,
                        result: Arc::new(result),
                    },
                );
            }
        }

        let bound_sources: BTreeMap<_, _> = self
            .bound_sources
            .iter()
            .map(|(path, cached)| (path.clone(), Arc::clone(&cached.result)))
            .collect();
        let bind_refs: Vec<_> = bound_sources.values().map(Arc::as_ref).collect();
        let file_work = execution.file_work();
        let max_file_work_retries = file_work.map_or(0, |work| work.max_retries);
        let mut file_work_skipped = file_work
            .map(|work| work.skipped.clone())
            .unwrap_or_default();
        let partition_failure = |error| match error {
            tsz::parallel::ProjectPartitionCheckError::Program(error) => TszAuthorityError::ProjectCheck(error),
            tsz::parallel::ProjectPartitionCheckError::CheckerSession(error) => TszAuthorityError::ProjectCheckerSession(error),
            tsz::parallel::ProjectPartitionCheckError::WorkerPoolUnavailable(detail) => TszAuthorityError::CheckerWorkerPoolUnavailable(detail),
            tsz::parallel::ProjectPartitionCheckError::FileWorkExhausted(files) => TszAuthorityError::FileWorkExhausted(files),
        };
        // Each round merges, plans, and checks a fresh program from the bound
        // sources. A round in which files spent their allowance is discarded
        // whole: those files may have written partial state into program-wide
        // tables that every other file reads, so the next round skips them.
        let (program, check) = loop {
            let merge = tsz_common::ProjectExecutionSpan::new(
                checkpoint,
                tsz_common::ProjectExecutionPhase::Merge,
                None,
            );
            let mut program = match checkpoint {
                Some(checkpoint) => {
                    tsz::parallel::merge_bind_results_ref_with_project_semantic_options_and_execution_checkpoint(
                        &bind_refs,
                        options.semantic_options,
                        checkpoint,
                    )
                    .map_err(TszAuthorityError::ExecutionStopped)?
                }
                None => tsz::parallel::merge_bind_results_ref_with_project_semantic_options(
                    &bind_refs,
                    options.semantic_options,
                ),
            };
            if let Some(checkpoint) = checkpoint {
                checkpoint
                    .checkpoint(0)
                    .map_err(TszAuthorityError::ExecutionStopped)?;
            }
            merge.returned();
            let formats = tsz_common::ProjectExecutionSpan::new(
                checkpoint,
                tsz_common::ProjectExecutionPhase::FileFormats,
                None,
            );
            program.set_project_file_formats(&options.file_formats)?;
            formats.returned();
            program.set_project_module_resolutions_for_checker_options(&options.module_resolutions, &options.checker, checkpoint)
                .map_err(|error| match error {
                    tsz::parallel::ProjectProgramCheckError::ModuleResolution(error) => TszAuthorityError::ModuleResolution(error),
                    stopped => TszAuthorityError::ProjectCheck(stopped),
                })?;
            let checked = match execution {
                TszUpdateExecution::Partitions(execution, work) => {
                    let mut image_checker = options.checker.clone();
                    if work.is_some_and(|work| !work.check_declaration_files) {
                        image_checker.declaration_diagnostics = TszDeclarationDiagnosticPolicy::SkipSemantic;
                    }
                    let prepared = tsz::parallel::PreparedProjectCheckerImage::new_with_checker_execution(
                        program,
                        image_checker,
                        lib_files,
                        options.semantic_options,
                        TszProjectCheckerExecution { checkpoint: checkpoint.expect("partition checkpoint"), workers: execution.workers },
                    ).map_err(partition_failure)?;
                    let prepared = match work {
                        Some(work) => prepared.with_file_work(tsz::parallel::ProjectFileWork {
                            allowance: work.allowance,
                            skipped: file_work_skipped.clone(),
                        }),
                        None => prepared,
                    };
                    match prepared.check_with_partitions(
                        TszProjectCheckerExecution { checkpoint: checkpoint.expect("partition checkpoint"), workers: execution.workers },
                    ) {
                        Ok(checked) => checked,
                        Err(tsz::parallel::ProjectPartitionCheckError::FileWorkExhausted(files)) => {
                            if report.file_work_retries >= max_file_work_retries {
                                return Err(TszAuthorityError::FileWorkExhausted(files));
                            }
                            report.file_work_retries += 1;
                            file_work_skipped.extend(files.into_iter().map(|file| file.path));
                            continue;
                        }
                        Err(other) => return Err(partition_failure(other)),
                    }
                }
                TszUpdateExecution::Fresh(_) | TszUpdateExecution::Unmetered => {
                    let prepared = tsz::parallel::PreparedProjectCheckerImage::new(
                        program, options.checker.clone(), lib_files, options.semantic_options, checkpoint,
                    ).map_err(|cause| match cause {
                        tsz::parallel::ProjectCheckerSessionError::ExecutionStopped(reason) => TszAuthorityError::ProjectCheck(tsz::parallel::ProjectProgramCheckError::ExecutionStopped(reason)),
                        other => TszAuthorityError::ProjectCheckerSession(other),
                    })?;
                    prepared.check_fresh(checkpoint)
                        .map_err(|cause| match cause {
                            tsz::parallel::ProjectCheckerSessionError::ExecutionStopped(reason) => TszAuthorityError::ProjectCheck(tsz::parallel::ProjectProgramCheckError::ExecutionStopped(reason)),
                            other => TszAuthorityError::ProjectCheckerSession(other),
                        })?
                }
            };
            break checked;
        };
        if let Some(checkpoint) = checkpoint {
            checkpoint
                .checkpoint(0)
                .map_err(TszAuthorityError::ExecutionStopped)?;
        }
        self.project = Some(TszProject {
            checker_options_digest: checker_digest,
            semantic_options_digest: semantic_digest,
            module_resolution_digest,
            file_format_digest,
            library_digest,
            lib_files: lib_files.to_vec(),
            options,
            bound_sources,
            program,
            query_budget: checkpoint.and_then(|checkpoint| checkpoint.execution_identity()),
            file_work_skipped,
            file_work_policy: execution.file_work().cloned(),
            check,
        });
        Ok(report)
    }

    /// Returns the most recently updated real TSZ project, if one exists.
    #[must_use]
    pub fn project(&self) -> Option<&TszProject> {
        self.project.as_ref().filter(|project| project.program.is_usable())
    }
}

impl TszProject {
    /// TSZ's project-wide merged cross-file program.
    #[must_use]
    pub fn program(&self) -> &tsz::parallel::MergedProgram {
        &self.program
    }

    /// TSZ's typed checker result and every checker diagnostic it retained.
    #[must_use]
    pub const fn check_result(&self) -> &tsz::parallel::CheckResult {
        &self.check
    }

    /// Exact program paths whose semantic check was not run because they spent
    /// their per-file work allowance (or the caller named them in advance).
    ///
    /// They remain program members for resolution, but a query against one of
    /// them was never primed by a check and must not be treated as covered.
    #[must_use]
    pub const fn file_work_skipped(&self) -> &BTreeSet<String> {
        &self.file_work_skipped
    }

    /// Source files in deterministic path order, with their retained native binds.
    #[must_use]
    pub fn source_paths(&self) -> impl Iterator<Item = &str> {
        self.bound_sources.keys().map(String::as_str)
    }

    /// The native parse/bind result for one source path.
    #[must_use]
    pub fn bind_result(&self, path: &str) -> Option<&tsz::parallel::BindResult> {
        self.bound_sources.get(path).map(Arc::as_ref)
    }

    /// Source text retained by the TSZ parser for a source path.
    #[must_use]
    pub fn source_text(&self, path: &str) -> Option<&str> {
        if let Some(result) = self.bind_result(path) {
            return result
                .arena
                .get_source_file_at(result.source_file)
                .map(|file| file.text.as_ref());
        }
        self.lib_files.iter().find_map(|lib| {
            (lib.file_name == path)
                .then(|| lib.arena.get_source_file_at(lib.root_index))
                .flatten()
                .map(|file| file.text.as_ref())
        })
    }

    /// Returns the exact source token at a declaration-scoped TSZ node.
    ///
    /// `file` is the source-file name carried by `TypeParamOrigin::DeclScoped`
    /// and `node` is that origin's exact identifier-node index. The result is
    /// admitted only if the node is an identifier whose TSZ-decoded name is
    /// `expected`; the returned bytes are the raw spelling from the retained
    /// project or library source, including any escapes the source wrote.
    #[must_use]
    pub fn declaration_name_source(&self, file: &str, node: u32, expected: &str) -> Option<&str> {
        let node = TszNodeIndex(node);
        let (arena, source) = self.source_arena_and_text(file)?;
        let syntax_node = arena.get(node)?;
        let identifier = arena.get_identifier(syntax_node)?;
        if identifier.escaped_text != expected {
            return None;
        }
        let (start, end) = arena.pos_end_at(node)?;
        let token = source.get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?;
        (!token.is_empty()).then_some(token)
    }

    /// Returns the exact source token for a named property in one retained
    /// declaration. This is used only when TSZ's structural property has no
    /// declaration symbol (for example an inline object or type literal).
    /// The property spelling must still match a parsed identifier that is
    /// the `name` child of a property/signature node inside the exact owner
    /// span; comments, string contents, and unrelated declaration names do
    /// not qualify.
    #[must_use]
    pub fn property_name_source_in_declaration(
        &self,
        file: &str,
        declaration: (u32, u32),
        expected: &str,
    ) -> Option<&str> {
        if expected.is_empty() || declaration.0 >= declaration.1 {
            return None;
        }
        let bind = self.bind_result(file)?;
        let arena = &bind.arena;
        let source = arena.get_source_file_at(bind.source_file)?.text.as_ref();
        let mut candidates = Vec::new();
        for (raw_index, syntax_node) in arena.nodes.iter().enumerate() {
            let node = TszNodeIndex(u32::try_from(raw_index).ok()?);
            let Some(identifier) = arena.get_identifier(syntax_node) else {
                continue;
            };
            if identifier.escaped_text != expected {
                continue;
            }
            let Some(parent) = arena.get_extended(node).map(|extended| extended.parent) else {
                continue;
            };
            let Some(parent_node) = arena.get(parent) else {
                continue;
            };
            let is_property_name = arena
                .get_signature(parent_node)
                .is_some_and(|signature| signature.name == node)
                || arena
                    .get_property_decl(parent_node)
                    .is_some_and(|property| property.name == node)
                || arena
                    .get_property_assignment(parent_node)
                    .is_some_and(|property| property.name == node)
                || arena
                    .get_shorthand_property(parent_node)
                    .is_some_and(|property| property.name == node)
                || arena
                    .get_method_decl(parent_node)
                    .is_some_and(|method| method.name == node)
                || arena
                    .get_accessor(parent_node)
                    .is_some_and(|accessor| accessor.name == node);
            if !is_property_name {
                continue;
            }
            let Some((start, end)) = arena.pos_end_at(node) else {
                continue;
            };
            if start < declaration.0 || end > declaration.1 {
                continue;
            }
            let (Ok(start_index), Ok(end_index)) = (usize::try_from(start), usize::try_from(end))
            else {
                continue;
            };
            let Some(token) = source.get(start_index..end_index) else {
                continue;
            };
            if !token.is_empty() {
                candidates.push((start, end, token));
            }
        }
        candidates.sort_unstable_by_key(|(start, end, _)| (*start, *end));
        candidates.first().map(|(_, _, token)| *token)
    }

    /// Resolves a checker property name through the exact TSZ symbol's
    /// declaration-name node and returns its raw source spelling. The lookup
    /// searches only declaration nodes bound to this symbol, constrained by
    /// the symbol's file-stable declaration range.
    #[must_use]
    pub fn symbol_name_source(&self, symbol: TszSymbolId, expected: &str) -> Option<&str> {
        let symbol_data = self.program.symbols.get(symbol)?;
        for declaration in &symbol_data.stable_declarations {
            let file_index = usize::try_from(declaration.file_idx).ok()?;
            let Some(file) = self.program.files.get(file_index) else {
                continue;
            };
            if let Some(token) = symbol_name_in_arena(
                &file.arena,
                file.node_symbols.iter(),
                symbol,
                expected,
                Some((declaration.pos, declaration.end)),
                file.source_file,
            ) {
                return Some(token);
            }
        }
        None
    }

    /// Borrows one property's exact identifier token from its bound owner.
    /// Repeated declarations with identical raw spellings agree; distinct raw
    /// spellings of the same decoded name are ambiguous and return `None`.
    /// Batch consumers should use [`Self::with_property_name_sources_from_owner`]
    /// to scan an owner's direct members only once with their shared checkpoint.
    #[must_use]
    pub fn property_name_source_from_owner(
        &self,
        symbol: TszSymbolId,
        expected: &str,
    ) -> Option<&str> {
        Self::property_name_source_in_program(self.program(), symbol, expected)
    }

    fn property_name_source_in_program<'source>(
        program: &'source tsz::parallel::MergedProgram,
        symbol: TszSymbolId,
        expected: &str,
    ) -> Option<&'source str> {
        if expected.is_empty() {
            return None;
        }
        let mut spelling = None;
        Self::with_property_name_sources_in_program(
            program,
            symbol,
            |_| Ok(()),
            |name, raw| {
                if name == expected {
                    match spelling {
                        None => spelling = Some(Some(raw)),
                        Some(Some(previous)) if previous != raw => spelling = Some(None),
                        _ => {}
                    }
                }
            },
        )
        .ok()?;
        spelling.flatten()
    }

    /// Visits borrowed decoded names and raw tokens of an exact declaring owner.
    ///
    /// Only direct parsed class/interface members, or a directly bound member
    /// declaration, may lend tokens. Raw declarations must align with their
    /// stable file/spans and retain the exact declaration-node symbol binding.
    /// No nested-member or global name scan participates. Each declaration,
    /// direct member and token byte is charged before it is exposed.
    ///
    /// Callers must discard any local index populated by `consume` on error;
    /// the visitor retains no cache or partial result. Duplicate decoded names
    /// are visited separately so the caller can refuse conflicting raw tokens.
    ///
    /// # Errors
    /// Propagates the shared execution checkpoint's exact stop reason.
    pub fn with_property_name_sources_from_owner<'source>(
        &'source self,
        symbol: TszSymbolId,
        checkpoint: impl FnMut(u64) -> Result<(), tsz_common::ProjectExecutionStop>,
        consume: impl FnMut(&'source str, &'source str),
    ) -> Result<(), tsz_common::ProjectExecutionStop> {
        Self::with_property_name_sources_in_program(self.program(), symbol, checkpoint, consume)
    }

    fn with_property_name_sources_in_program<'source>(
        program: &'source tsz::parallel::MergedProgram,
        symbol: TszSymbolId,
        mut checkpoint: impl FnMut(u64) -> Result<(), tsz_common::ProjectExecutionStop>,
        mut consume: impl FnMut(&'source str, &'source str),
    ) -> Result<(), tsz_common::ProjectExecutionStop> {
        checkpoint(0)?;
        let Some(owner) = program.symbols.get(symbol) else {
            return Ok(());
        };
        if owner.declarations.len() != owner.stable_declarations.len() {
            return Ok(());
        }
        for (declaration, stable) in owner.declarations.iter().zip(&owner.stable_declarations) {
            checkpoint(1)?;
            if stable.file_idx == u32::MAX || stable.pos >= stable.end {
                continue;
            }
            let Some(file) = usize::try_from(stable.file_idx)
                .ok()
                .and_then(|index| program.files.get(index))
            else {
                continue;
            };
            let arena = &file.arena;
            if arena.pos_end_at(*declaration) != Some((stable.pos, stable.end))
                || file.node_symbols.get(&declaration.0) != Some(&symbol)
            {
                continue;
            }
            let Some(node) = arena.get(*declaration) else {
                continue;
            };
            let Some(source) = arena
                .get_source_file_at(file.source_file)
                .map(|file| file.text.as_ref())
            else {
                continue;
            };
            let members = arena
                .get_class(node)
                .map(|class| &class.members)
                .or_else(|| {
                    arena
                        .get_interface(node)
                        .map(|interface| &interface.members)
                });
            if let Some(members) = members {
                for &member in &members.nodes {
                    checkpoint(1)?;
                    if arena.get_extended(member).map(|node| node.parent) != Some(*declaration)
                        || !arena.pos_end_at(member).is_some_and(|(start, end)| {
                            stable.pos <= start && start < end && end <= stable.end
                        })
                    {
                        continue;
                    }
                    if let Some((name, raw)) = exact_property_name_tokens(arena, source, member) {
                        checkpoint(
                            u64::try_from(name.len())
                                .unwrap_or(u64::MAX)
                                .saturating_add(u64::try_from(raw.len()).unwrap_or(u64::MAX)),
                        )?;
                        consume(name, raw);
                    }
                }
            } else if let Some((name, raw)) =
                exact_property_name_tokens(arena, source, *declaration)
            {
                checkpoint(
                    u64::try_from(name.len())
                        .unwrap_or(u64::MAX)
                        .saturating_add(u64::try_from(raw.len()).unwrap_or(u64::MAX)),
                )?;
                consume(name, raw);
            }
        }
        checkpoint(0)
    }

    /// Content identity for one exact retained project or library source.
    /// The stable path is included in the digest, so moving byte-identical
    /// content to another owner does not preserve a declaration identity.
    #[must_use]
    pub fn source_content_digest(&self, path: &str) -> Option<[u8; 32]> {
        let source = self.source_text(path)?;
        Some(source_digest(path, source))
    }

    /// Library source paths in deterministic load order.
    #[must_use]
    pub fn library_paths(&self) -> impl Iterator<Item = &str> {
        self.lib_files.iter().map(|lib| lib.file_name.as_str())
    }

    /// Borrows one exact source or library arena and its retained UTF-8 bytes
    /// for syntax and coordinate projection. No filesystem lookup occurs.
    #[must_use]
    pub fn with_source_nodes<Output>(
        &self,
        path: &str,
        consume: impl FnOnce(&tsz::parser::NodeArena, &str) -> Output,
    ) -> Option<Output> {
        let (arena, source) = self.source_arena_and_text(path)?;
        Some(consume(arena, source))
    }

    fn source_arena_and_text(&self, path: &str) -> Option<(&tsz::parser::NodeArena, &str)> {
        if let Some(file) = self
            .program
            .files
            .iter()
            .find(|file| file.file_name == path)
        {
            let source = file
                .arena
                .get_source_file_at(file.source_file)?
                .text
                .as_ref();
            return Some((file.arena.as_ref(), source));
        }
        self.lib_files.iter().find_map(|lib| {
            if lib.file_name != path {
                return None;
            }
            let source = lib.arena.get_source_file_at(lib.root_index)?.text.as_ref();
            Some((lib.arena.as_ref(), source))
        })
    }

    /// Admits a TSZ diagnostic's byte range against the exact retained source.
    ///
    /// TSZ's native diagnostic offsets are UTF-8 bytes. This rejects overflow,
    /// out-of-bounds offsets, and ranges that split a UTF-8 scalar before the
    /// range can enter the backend's source-coordinate-aware IR.
    #[must_use]
    pub fn diagnostic_utf8_span(&self, diagnostic: &TszDiagnostic) -> Option<Utf8Span> {
        self.source_utf8_span(&diagnostic.file, diagnostic.start, diagnostic.length)
    }

    /// Admits a TSZ parser diagnostic's byte range against its exact source.
    #[must_use]
    pub fn parse_diagnostic_utf8_span(
        &self,
        path: &str,
        diagnostic: &TszParseDiagnostic,
    ) -> Option<Utf8Span> {
        self.source_utf8_span(path, diagnostic.start, diagnostic.length)
    }

    fn source_utf8_span(&self, path: &str, start: u32, length: u32) -> Option<Utf8Span> {
        let source = self.source_text(path)?;
        let end = start.checked_add(length)?;
        let span = Utf8Span::try_from(start..end).ok()?;
        source
            .get(usize::try_from(span.start).ok()?..usize::try_from(span.end).ok()?)
            .map(|_| span)
    }

    /// Runs a typed checker query inside a non-escaping per-file transaction.
    ///
    /// The callback can walk the TSZ arena and bound symbols, ask TSZ for
    /// `TypeId`s, and map those typed facts into the existing compiler IR while
    /// the exact program interner and arena are alive. Return values cannot
    /// borrow the temporary checker, binder, or query cache.
    ///
    /// # Errors
    /// Returns [`TszAuthorityError::MissingFile`] for an invalid merged index.
    #[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
    pub fn with_file_checker<Output>(
        &self,
        file_index: usize,
        consume: impl for<'checker> FnOnce(
            &mut TszCheckerState<'checker>,
            &TszBinderState,
            &tsz::parallel::BoundFile,
        ) -> Output,
    ) -> Result<Output, TszAuthorityError> {
        self.with_file_checker_and_types(file_index, |checker, binder, file, _types| {
            consume(checker, binder, file)
        })
    }

    /// Unmetered regression helper using the exact shared project query context.
    /// This helper is available only to unit tests or explicit semantic test support.
    /// Production projection uses [`Self::checked_query_session`] and its shared budget.
    ///
    /// Runs a typed checker query and exposes the same checker's native type
    /// database for direct structural IR mapping. All borrows remain scoped to
    /// this exact project/file transaction; no type handles escape the
    /// callback.
    ///
    /// # Errors
    /// Returns [`TszAuthorityError::MissingFile`] for an invalid merged index.
    #[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
    pub fn with_file_checker_and_types<Output>(
        &self,
        file_index: usize,
        consume: impl for<'checker> FnOnce(
            &mut TszCheckerState<'checker>,
            &TszBinderState,
            &tsz::parallel::BoundFile,
            &dyn TypeDatabase,
        ) -> Output,
    ) -> Result<Output, TszAuthorityError> {
        self.program
            .files
            .get(file_index)
            .ok_or(TszAuthorityError::MissingFile(file_index))?;
        let session = self.checked_query_session_unmetered_for_test()?;
        session
            .with_file_checker_and_types(file_index, |checker, binder, file, types| {
                // Preserve this legacy regression helper's eager file query semantics
                // while borrowing the same exact context as bounded projection.
                checker.check_source_file(file.source_file);
                consume(checker, binder, file, types)
            })
            .map_err(TszAuthorityError::ProjectCheckerSession)
    }
}

fn validate_source_path(path: &str) -> Result<(), TszSourceError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.contains(':')
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(TszSourceError::InvalidPath);
    }
    let Some((basename, extension)) = path.rsplit_once('.') else {
        return Err(TszSourceError::InvalidPath);
    };
    let filename = basename.rsplit('/').next().unwrap_or(basename);
    if path.is_empty() || filename.is_empty() || extension.is_empty() {
        return Err(TszSourceError::InvalidPath);
    }
    match extension.to_ascii_lowercase().as_str() {
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Ok(()),
        _ => Err(TszSourceError::UnsupportedExtension),
    }
}

fn source_digest(path: &str, source: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"compiler.typescript.tsz-source.v1\0");
    digest_part(&mut digest, path.as_bytes());
    digest_part(&mut digest, source.as_bytes());
    digest.finalize().into()
}

fn library_digest(lib_files: &[Arc<tsz::lib_loader::LibFile>]) -> Result<[u8; 32], TszSourceError> {
    let mut libraries = Vec::with_capacity(lib_files.len());
    let mut unique_paths = BTreeSet::new();
    for library in lib_files {
        validate_source_path(&library.file_name)?;
        if !unique_paths.insert(library.file_name.as_str()) {
            return Err(TszSourceError::DuplicatePath);
        }
        let Some(source) = library
            .arena
            .get_source_file_at(library.root_index)
            .map(|file| file.text.as_ref())
        else {
            return Err(TszSourceError::InvalidPath);
        };
        libraries.push((library.file_name.as_str(), source));
    }
    libraries.sort_unstable_by(|left, right| left.0.cmp(right.0));

    let mut digest = Sha256::new();
    digest.update(b"compiler.typescript.tsz-libraries.v2\0");
    for (path, source) in libraries {
        digest_part(&mut digest, path.as_bytes());
        digest_part(&mut digest, source.as_bytes());
    }
    Ok(digest.finalize().into())
}

fn digest_part(digest: &mut Sha256, part: &[u8]) {
    digest.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(part);
}

fn exact_property_name_tokens<'a>(
    arena: &'a tsz::parser::NodeArena,
    source: &'a str,
    member: TszNodeIndex,
) -> Option<(&'a str, &'a str)> {
    let node = arena.get(member)?;
    let name = arena
        .get_property_decl(node)
        .map(|property| property.name)
        .or_else(|| arena.get_signature(node).map(|signature| signature.name))
        .or_else(|| arena.get_method_decl(node).map(|method| method.name))
        .or_else(|| arena.get_accessor(node).map(|accessor| accessor.name))?;
    if arena.get_extended(name)?.parent != member {
        return None;
    }
    let decoded = arena
        .get_identifier(arena.get(name)?)?
        .escaped_text
        .as_str();
    let (lower, upper) = arena.pos_end_at(member)?;
    let (start, end) = arena.pos_end_at(name)?;
    if start < lower || end > upper || start >= end {
        return None;
    }
    let raw = source.get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?;
    Some((decoded, raw))
}

fn symbol_name_in_arena<'a, 'symbols, I>(
    arena: &'a tsz::parser::NodeArena,
    node_symbols: I,
    symbol: TszSymbolId,
    expected: &str,
    range: Option<(u32, u32)>,
    source_file: TszNodeIndex,
) -> Option<&'a str>
where
    I: IntoIterator<Item = (&'symbols u32, &'symbols TszSymbolId)>,
{
    let source = arena.get_source_file_at(source_file)?.text.as_ref();
    let mut candidates = Vec::new();
    for (raw, candidate_symbol) in node_symbols {
        if *candidate_symbol != symbol {
            continue;
        }
        let node = TszNodeIndex(*raw);
        let syntax_node = arena.get(node)?;
        let Some(identifier) = arena.get_identifier(syntax_node) else {
            continue;
        };
        if identifier.escaped_text != expected {
            continue;
        }
        let Some((start, end)) = arena.pos_end_at(node) else {
            continue;
        };
        if range.is_some_and(|(lower, upper)| start < lower || end > upper) {
            continue;
        }
        let (Ok(start_index), Ok(end_index)) = (usize::try_from(start), usize::try_from(end))
        else {
            continue;
        };
        let Some(token) = source.get(start_index..end_index) else {
            continue;
        };
        if !token.is_empty() {
            candidates.push((start, end, token));
        }
    }
    candidates.sort_unstable_by_key(|(start, end, _)| (*start, *end));
    candidates.first().map(|(_, _, token)| *token)
}

fn binding_options_digest(options: &TszCheckerOptions) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"tsz.parse-bind-options.v1\0");
    digest.update(format!("{:?}:{:?}", options.target, options.module_detection).as_bytes());
    digest.finalize().into()
}

fn checker_options_digest(options: &TszCheckerOptions) -> [u8; 32] {
    // TSZ's options are a broad non-serializable struct. Its Debug output is
    // a same-revision in-process cache key only; the external environment
    // fingerprint remains the durable content-addressed identity.
    Sha256::digest(format!("{options:?}").as_bytes()).into()
}

fn semantic_options_digest(options: TszProjectSemanticOptions) -> [u8; 32] {
    Sha256::digest(format!("{options:?}").as_bytes()).into()
}

fn file_format_digest(formats: &[TszProjectFileFormatInput]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"compiler.typescript.tsz-file-formats.v1\0");
    digest_part(&mut digest, format!("{formats:?}").as_bytes());
    digest.finalize().into()
}

fn module_resolution_digest(resolutions: &[TszProjectModuleResolution]) -> [u8; 32] {
    // DTO Debug is a same-revision cache key only. The caller's durable
    // environment fingerprint remains responsible for resolved compiler
    // options, source closure, and resolver/toolchain identity.
    let mut digest = Sha256::new();
    digest.update(b"compiler.typescript.tsz-module-resolutions.v1\0");
    digest_part(&mut digest, format!("{resolutions:?}").as_bytes());
    digest.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TszResolutionModeOverride;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};
    use tsz::parser::node::NodeAccess;

    fn options() -> TszProjectOptions {
        let mut checker = TszCheckerOptions::default();
        checker.no_lib = true;
        TszProjectOptions {
            checker,
            semantic_options: TszProjectSemanticOptions::structural(),
            module_resolutions: Vec::new(),
            file_formats: Vec::new(),
            environment: TszEnvironmentFingerprint::from_sha256([0x5a; 32]),
        }
    }

    #[test]
    fn compiler_api_options_preserve_explicit_checker_overrides() {
        let source = r#"{"compilerOptions":{"target":"ES2022","module":"NodeNext","moduleResolution":"NodeNext","strict":true,"noImplicitAny":false,"strictNullChecks":true,"noUncheckedIndexedAccess":true,"skipLibCheck":true,"lib":["ES2022"]}}"#;
        let checker = checker_options_from_compiler_api_json(source)
            .expect("compiler-API options should resolve through the typed TSZ config model");

        assert!(checker.strict);
        assert!(!checker.no_implicit_any);
        assert!(checker.strict_null_checks);
        assert!(checker.no_unchecked_indexed_access);
        assert_eq!(
            checker.declaration_diagnostics,
            tsz_common::options::checker::DeclarationDiagnosticPolicy::SkipSemantic
        );
        let full =
            checker_options_from_compiler_api_json(r#"{"compilerOptions":{"skipLibCheck":false}}"#)
                .unwrap();
        assert_eq!(
            full.declaration_diagnostics,
            tsz_common::options::checker::DeclarationDiagnosticPolicy::Check
        );
        assert!(checker.module_explicitly_set);
    }

    #[test]
    fn compiler_api_options_preserve_vite_flags_and_raw_isolated_modules() {
        let parse = |raw_isolated| {
            checker_options_from_compiler_api_json(&format!(
            r#"{{"compilerOptions":{{"target":"ES2022","module":"ESNext","moduleResolution":"Bundler","verbatimModuleSyntax":true,"isolatedModules":{raw_isolated},"erasableSyntaxOnly":true,"allowImportingTsExtensions":true,"noEmit":true}}}}"#
        )).expect("modeled Vite checker flags")
        };
        let implied = parse(false);
        let explicit = parse(true);
        assert!(implied.verbatim_module_syntax && implied.isolated_modules);
        assert!(implied.isolated_modules_from_verbatim);
        assert!(explicit.isolated_modules && !explicit.isolated_modules_from_verbatim);
        assert!(implied.erasable_syntax_only && implied.allow_importing_ts_extensions);
        assert_ne!(
            checker_options_digest(&implied),
            checker_options_digest(&explicit)
        );
    }

    #[test]
    fn admitted_erasable_syntax_option_changes_actual_checker_diagnostics() {
        let run = |erasable| {
            let mut authority = TszProjectAuthority::new();
            let mut policy = options();
            policy.checker = checker_options_from_compiler_api_json(
                &serde_json::json!({
                    "compilerOptions": {
                        "target": "ES2022",
                        "module": "ESNext",
                        "noLib": true,
                        "erasableSyntaxOnly": erasable
                    }
                })
                .to_string(),
            )
            .expect("checker options");
            authority
                .update(
                    vec![input("src/value.ts", "export enum Value { A, B }")],
                    policy,
                    &[],
                )
                .expect("actual checker");
            authority
                .project()
                .unwrap()
                .check_result()
                .file_results
                .iter()
                .flat_map(|file| &file.diagnostics)
                .map(|d| d.code)
                .collect::<Vec<_>>()
        };
        assert!(run(true).contains(&1294));
        assert!(!run(false).contains(&1294));
    }

    fn input(path: &str, source: &str) -> TszFileInput {
        TszFileInput {
            path: path.to_owned(),
            source: source.to_owned(),
        }
    }

    fn execution_budget<'cancel>(
        cancelled: &'cancel AtomicBool,
        maximum_work_units: u64,
    ) -> tsz_common::ProjectExecutionBudget<'cancel> {
        tsz_common::ProjectExecutionBudget::new(
            Instant::now() + Duration::from_secs(30),
            cancelled,
            maximum_work_units,
        )
    }

    #[test]
    fn stopped_owned_query_image_is_hidden_and_exact_update_rebuilds_retained_binds() {
        use std::sync::atomic::Ordering;
        struct OneShotStop(AtomicBool);
        impl ExecutionCheckpoint for OneShotStop {
            fn checkpoint(&self, _work: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
                if self.0.swap(false, Ordering::AcqRel) { Err(tsz_common::ProjectExecutionStop::Cancelled) } else { Ok(()) }
            }
        }
        let sources = vec![input("src/value.ts", "export const value: number = 1;")];
        let mut authority = TszProjectAuthority::new();
        authority.update(sources.clone(), options(), &[]).unwrap();
        let one_shot = OneShotStop(AtomicBool::new(false));
        {
            let session = authority.project().unwrap().checked_query_session(&one_shot).unwrap();
            let rejected = session.with_file_checker_and_types(0, |_, _, _, database| {
                one_shot.0.store(true, Ordering::Release);
                assert_eq!(database.union(vec![TszTypeId::NUMBER, TszTypeId::STRING]), TszTypeId::UNKNOWN);
                "discard this partial query"
            });
            assert!(matches!(rejected, Err(TszProjectQuerySessionError::ExecutionStopped(tsz_common::ProjectExecutionStop::Cancelled))));
        }
        assert!(authority.project().is_none(), "stopped image is no longer a ready project");
        let cancelled = AtomicBool::new(false);
        let fresh_budget = execution_budget(&cancelled, 1_000_000);
        let rebuilt = authority.update_with_execution_checkpoint(sources.clone(), options(), &[], &fresh_budget).unwrap();
        assert_eq!(rebuilt.parsed_and_bound, 0);
        assert_eq!(rebuilt.reused_binds, 1);
        assert!(!rebuilt.reused_project_result);
        let project = authority.project().unwrap();
        let session = project.checked_query_session(&fresh_budget).unwrap();
        let value = session.with_file_checker_and_types(0, |checker, binder, _, _| {
            let ty = checker.get_type_of_symbol(binder.file_locals.get("value").unwrap());
            checker.format_type(ty)
        }).unwrap();
        assert_eq!(value, "number");
        drop(session);
        let repeated = authority.update_with_execution_checkpoint(sources, options(), &[], &fresh_budget).unwrap();
        assert!(repeated.reused_project_result);
    }

    #[test]
    fn new_admitted_budget_reuses_exact_checked_image_without_rebuilding_views() {
        let cancelled = AtomicBool::new(false);
        let first = execution_budget(&cancelled, 1_000_000);
        let other = execution_budget(&cancelled, 1_000_000);
        let sources = vec![input("src/value.ts", "export const value: number = 1;")];
        let mut authority = TszProjectAuthority::new();
        authority.update_with_execution_checkpoint(sources.clone(), options(), &[], &first).unwrap();
        assert!(matches!(authority.project().unwrap().checked_query_session(&other), Err(TszProjectQuerySessionError::ExecutionCheckpointMismatch)));
        assert!(authority.project().is_some(), "wrong caller cannot poison the admitted image");
        let checked_store = Arc::clone(&authority.project().unwrap().program.definition_store);
        let admitted = authority.update_with_execution_checkpoint(sources.clone(), options(), &[], &other).unwrap();
        assert_eq!(admitted.parsed_and_bound, 0);
        assert_eq!(admitted.reused_binds, 1);
        assert!(admitted.reused_project_result);
        assert!(Arc::ptr_eq(&checked_store, &authority.project().unwrap().program.definition_store),
            "a later admitted scope must not rebuild the checked program");
        assert!(matches!(authority.project().unwrap().checked_query_session(&first), Err(TszProjectQuerySessionError::ExecutionCheckpointMismatch)),
            "the current request still rejects a scope from its previous admission");
        let session = authority.project().unwrap().checked_query_session(&other).unwrap();
        let value = session.with_file_checker_and_types(0, |checker, binder, _, _| {
            let ty = checker.get_type_of_symbol(binder.file_locals.get("value").unwrap());
            checker.format_type(ty)
        }).unwrap();
        assert_eq!(value, "number");
        drop(session);
        assert!(authority.update_with_execution_checkpoint(sources, options(), &[], &other).unwrap().reused_project_result);
    }

    #[test]
    fn checked_query_session_reuses_the_project_execution_checkpoint() {
        let cancelled = AtomicBool::new(false);
        let budget = execution_budget(&cancelled, 1_000_000);
        let mut authority = TszProjectAuthority::new();
        authority
            .update_with_execution_checkpoint(
                vec![input("src/value.ts", "export const value: number = 1;")],
                options(),
                &[],
                &budget,
            )
            .expect("the bounded project should parse, bind, merge, and check");

        let project = authority.project().expect("checked project exists");
        let session = project
            .checked_query_session(&budget)
            .expect("query session borrows the same project budget");
        let file_index = session
            .file_index("src/value.ts")
            .expect("the exact source path is present");
        let observed = session
            .with_file_checker_and_types(file_index, |checker, binder, _file, _database| {
                let symbol = binder
                    .file_locals
                    .get("value")
                    .expect("the exported declaration is bound");
                let ty = checker.get_type_of_symbol(symbol);
                checker.format_type(ty)
            })
            .expect("the shared full-program checker lends the file");
        assert_eq!(observed, "number");
    }

    #[test]
    fn stopped_changed_project_is_not_left_ready() {
        let cancelled = AtomicBool::new(false);
        let initial_budget = execution_budget(&cancelled, 1_000_000);
        let mut authority = TszProjectAuthority::new();
        authority
            .update_with_execution_checkpoint(
                vec![input("src/value.ts", "export const value: number = 1;")],
                options(),
                &[],
                &initial_budget,
            )
            .expect("the initial project should be ready");
        assert!(authority.project().is_some());

        let exhausted_budget = execution_budget(&cancelled, 0);
        let stopped = authority.update_with_execution_checkpoint(
            vec![input("src/value.ts", "export const value: number = 2;")],
            options(),
            &[],
            &exhausted_budget,
        );
        assert!(matches!(
            stopped,
            Err(TszAuthorityError::ExecutionStopped(
                tsz_common::ProjectExecutionStop::WorkBudgetExhausted
            ))
        ));
        assert!(authority.project().is_none());
    }

    #[test]
    fn native_checker_queries_bind_same_named_exports_by_project_path() {
        let mut authority = TszProjectAuthority::new();
        let report = authority
            .update(
                vec![
                    input("packages/a/index.ts", "export let shared: number = 1;"),
                    input("packages/b/index.ts", "export let shared: string = 'b';"),
                ],
                options(),
                &[],
            )
            .expect("TSZ should build the project");
        assert_eq!(report.parsed_and_bound, 2);

        let project = authority.project().expect("project result exists");
        let mut observed = BTreeMap::new();
        for (index, file) in project.program().files.iter().enumerate() {
            let path = file.file_name.clone();
            let (symbol, display) = project
                .with_file_checker(index, |checker, binder, _bound_file| {
                    let symbol = binder
                        .file_locals
                        .get("shared")
                        .expect("same-named exported symbol is bound");
                    let type_id = checker.get_type_of_symbol(symbol);
                    (symbol, checker.format_type(type_id))
                })
                .expect("file checker exists");
            observed.insert(path, (symbol, display));
        }

        let (number_symbol, number_type) = observed
            .get("packages/a/index.ts")
            .expect("first file is retained");
        let (string_symbol, string_type) = observed
            .get("packages/b/index.ts")
            .expect("second file is retained");
        assert_ne!(number_symbol, string_symbol);
        assert_eq!(number_type, "number");
        assert_eq!(string_type, "string");
    }

    #[test]
    fn changed_source_reuses_unchanged_native_bind_and_exact_revision_result() {
        let mut authority = TszProjectAuthority::new();
        let initial = vec![
            input("src/a.ts", "export const a: number = 1;"),
            input("src/b.ts", "export const b: string = 'b';"),
        ];
        authority
            .update(initial.clone(), options(), &[])
            .expect("initial TSZ project builds");
        let previous_b = Arc::clone(
            authority
                .project()
                .expect("initial project exists")
                .bound_sources
                .get("src/b.ts")
                .expect("unchanged file has native bind"),
        );

        let report = authority
            .update(
                vec![
                    input("src/a.ts", "export const a: number = 2;"),
                    initial[1].clone(),
                ],
                options(),
                &[],
            )
            .expect("edited project builds");
        assert_eq!(report.parsed_and_bound, 1);
        assert_eq!(report.reused_binds, 1);
        let project = authority.project().expect("updated project exists");
        assert!(Arc::ptr_eq(
            &previous_b,
            project
                .bound_sources
                .get("src/b.ts")
                .expect("unchanged native bind survives")
        ));

        let exact = authority
            .update(
                vec![
                    input("src/a.ts", "export const a: number = 2;"),
                    initial[1].clone(),
                ],
                options(),
                &[],
            )
            .expect("exact project revision is reusable");
        assert!(exact.reused_project_result);
        assert_eq!(exact.parsed_and_bound, 0);
    }

    #[test]
    fn dependency_environment_edit_retains_exact_binds_and_replaces_checked_types() {
        fn observed(authority: &TszProjectAuthority) -> String {
            let project = authority.project().unwrap();
            let session = project.checked_query_session_unmetered_for_test().unwrap();
            let index = session.file_index("src/main.ts").unwrap();
            session.with_file_checker_and_types(index, |checker, binder, _, _| {
                let ty = checker.get_type_of_symbol(binder.file_locals.get("actual").unwrap());
                checker.format_type(ty)
            }).unwrap()
        }
        let main = input("src/main.ts", "import { value } from './dep'; export const actual = value;");
        let mut recipe = options();
        recipe.module_resolutions.push(TszProjectModuleResolution {
            importer_path: "src/main.ts".into(),
            specifier: "./dep".into(),
            request_kind: TszProjectModuleRequestKind::EsmImport,
            resolution_mode: None,
            target: TszProjectModuleResolutionTarget::File { path: "src/dep.ts".into() },
        });
        let mut authority = TszProjectAuthority::new();
        authority.update(vec![main.clone(), input("src/dep.ts", "export const value: number = 1;")], recipe.clone(), &[]).unwrap();
        assert_eq!(observed(&authority), "number");
        let previous_main = Arc::clone(authority.project().unwrap().bound_sources.get("src/main.ts").unwrap());

        // The real engine includes dependency source content in its full
        // environment. That changes checked authority without changing the
        // parser/binder inputs of this unchanged importer.
        recipe.environment = TszEnvironmentFingerprint::from_sha256([0x71; 32]);
        let changed = vec![main.clone(), input("src/dep.ts", "export const value: string = 'changed';")];
        let report = authority.update(changed.clone(), recipe.clone(), &[]).unwrap();
        assert_eq!((report.parsed_and_bound, report.reused_binds), (1, 1));
        assert!(!report.reused_project_result);
        assert!(Arc::ptr_eq(&previous_main, authority.project().unwrap().bound_sources.get("src/main.ts").unwrap()));
        assert_eq!(observed(&authority), "string");
        let mut fresh = TszProjectAuthority::new();
        fresh.update(changed.clone(), recipe.clone(), &[]).unwrap();
        assert_eq!(observed(&authority), observed(&fresh));
        let diagnostics = |authority: &TszProjectAuthority| authority.project().unwrap().check_result().file_results.iter()
            .map(|file| (file.file_name.clone(), file.diagnostics.clone())).collect::<Vec<_>>();
        assert_eq!(diagnostics(&authority), diagnostics(&fresh));

        recipe.checker.strict_null_checks = !recipe.checker.strict_null_checks;
        recipe.environment = TszEnvironmentFingerprint::from_sha256([0x72; 32]);
        let options_changed = authority.update(changed.clone(), recipe.clone(), &[]).unwrap();
        assert_eq!((options_changed.parsed_and_bound, options_changed.reused_binds), (0, 2));
        assert!(!options_changed.reused_project_result);
        assert_eq!(observed(&authority), "string");

        recipe.checker.module_detection = TszModuleDetectionKind::Force;
        let binding_changed = authority.update(changed, recipe, &[]).unwrap();
        assert_eq!((binding_changed.parsed_and_bound, binding_changed.reused_binds), (2, 0));
        assert!(!binding_changed.reused_project_result);
    }

    #[test]
    fn semantic_option_change_rechecks_without_rebinding_and_reaches_direct_queries() {
        let sources = vec![input(
            "src/labels.ts",
            "export type Labels<T> = { [K in keyof T]: T[K] };",
        )];
        let mut authority = TszProjectAuthority::new();
        authority
            .update(sources.clone(), options(), &[])
            .expect("structural project builds");

        let mut declaration_scoped = options();
        declaration_scoped.semantic_options = TszProjectSemanticOptions::declaration_scoped();
        let report = authority
            .update(sources, declaration_scoped, &[])
            .expect("declaration-scoped project rebuilds");
        assert_eq!(report.parsed_and_bound, 0);
        assert_eq!(report.reused_binds, 1);
        assert!(!report.reused_project_result);

        let project = authority.project().expect("updated project exists");
        let observed = project
            .with_file_checker_and_types(0, |_checker, _binder, _file, database| {
                database.project_semantic_options()
            })
            .expect("direct project query exists");
        assert_eq!(observed, TszProjectSemanticOptions::declaration_scoped());

        let exact = authority
            .update(
                vec![input(
                    "src/labels.ts",
                    "export type Labels<T> = { [K in keyof T]: T[K] };",
                )],
                TszProjectOptions {
                    checker: options().checker,
                    semantic_options: TszProjectSemanticOptions::declaration_scoped(),
                    module_resolutions: Vec::new(),
                    file_formats: Vec::new(),
                    environment: TszEnvironmentFingerprint::from_sha256([0x5a; 32]),
                },
                &[],
            )
            .expect("exact semantic project revision is reusable");
        assert!(exact.reused_project_result);
    }

    #[test]
    fn exact_module_resolutions_are_attached_and_change_project_identity_only() {
        let sources = vec![
            input(
                "src/main.ts",
                "import { value } from './value'; export const result = value;",
            ),
            input("src/value.ts", "export const value = 42;"),
        ];
        let resolution = |target| TszProjectModuleResolution {
            importer_path: "src/main.ts".to_owned(),
            specifier: "./value".to_owned(),
            request_kind: TszProjectModuleRequestKind::EsmImport,
            resolution_mode: None,
            target,
        };
        let mut initial_options = options();
        initial_options.module_resolutions =
            vec![resolution(TszProjectModuleResolutionTarget::File {
                path: "src/value.ts".to_owned(),
            })];
        let mut authority = TszProjectAuthority::new();
        let initial = authority
            .update(sources.clone(), initial_options, &[])
            .expect("exact file resolution builds");
        assert_eq!(initial.parsed_and_bound, 2);
        assert!(
            authority
                .project()
                .expect("initial project exists")
                .program()
                .project_module_resolution_outcomes
                .is_some()
        );

        let mut external_options = options();
        external_options.module_resolutions =
            vec![resolution(TszProjectModuleResolutionTarget::External {
                identity: "npm:fixture/value@1".to_owned(),
            })];
        let changed = authority
            .update(sources.clone(), external_options.clone(), &[])
            .expect("changed exact outcome rechecks the same binds");
        assert_eq!(changed.parsed_and_bound, 0);
        assert_eq!(changed.reused_binds, 2);
        assert!(!changed.reused_project_result);

        let repeated = authority
            .update(sources.clone(), external_options, &[])
            .expect("exact module-resolution revision reuses the project");
        assert!(repeated.reused_project_result);

        let mut invalid_options = options();
        invalid_options.module_resolutions =
            vec![resolution(TszProjectModuleResolutionTarget::File {
                path: "src/not-admitted.ts".to_owned(),
            })];
        assert!(matches!(
            authority.update(sources, invalid_options, &[]),
            Err(TszAuthorityError::ModuleResolution(
                TszProjectModuleResolutionError::TargetNotInProgram { .. }
            ))
        ));
    }

    #[test]
    fn captured_reference_outcomes_own_diagnostics_partitions_queries_and_identity() {
        let disk = std::env::temp_dir().join(format!(
            "tsz-reference-{}-{}.d.ts",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let cleanup = Cleanup(disk);
        std::fs::write(&cleanup.0, "interface AmbientDiskCannotOverrideCapture {}").unwrap();
        let path = cleanup.0.to_str().unwrap();
        let source = format!(
            "#!/usr/bin/env node\n/// <reference path='{path}' />\n/// <reference types='missing-ambient' resolution-mode='require' />\nexport const value = 1;"
        );
        let path_offset = source.find(path).unwrap() as u32;
        let types_offset = source.find("missing-ambient").unwrap() as u32;
        let sources = vec![
            input("src/main.ts", &source),
            input("types/admitted.d.ts", "interface CapturedType {}"),
        ];
        let mut recipe = options();
        recipe.module_resolutions = vec![
            TszProjectModuleResolution {
                importer_path: "src/main.ts".into(),
                specifier: path.into(),
                request_kind: TszProjectModuleRequestKind::ReferencePath {
                    offset: path_offset,
                },
                resolution_mode: None,
                target: TszProjectModuleResolutionTarget::Unresolved,
            },
            TszProjectModuleResolution {
                importer_path: "src/main.ts".into(),
                specifier: "missing-ambient".into(),
                request_kind: TszProjectModuleRequestKind::ReferenceTypes {
                    offset: types_offset,
                },
                resolution_mode: Some(TszResolutionModeOverride::Require),
                target: TszProjectModuleResolutionTarget::Unresolved,
            },
        ];
        let codes = |project: &TszProject| {
            project
                .check_result()
                .file_results
                .iter()
                .flat_map(|file| &file.diagnostics)
                .filter(|diagnostic| matches!(diagnostic.code, 6053 | 2688))
                .map(|diagnostic| (diagnostic.code, diagnostic.start))
                .collect::<Vec<_>>()
        };
        let mut authority = TszProjectAuthority::new();
        authority
            .update(sources.clone(), recipe.clone(), &[])
            .unwrap();
        let expected = codes(authority.project().unwrap());
        assert!(expected.contains(&(6053, path_offset)));
        assert!(expected.contains(&(2688, types_offset)));
        for workers in [1, 2] {
            let cancelled = AtomicBool::new(false);
            let budget = execution_budget(&cancelled, 1_000_000);
            let mut partitioned = TszProjectAuthority::new();
            partitioned
                .update_with_checker_execution(
                    sources.clone(),
                    recipe.clone(),
                    &[],
                    TszProjectCheckerExecution {
                        checkpoint: &budget,
                        workers: std::num::NonZeroUsize::new(workers).unwrap(),
                    },
                )
                .unwrap();
            let project = partitioned.project().unwrap();
            assert_eq!(codes(project), expected);
            let session = project.checked_query_session(&budget).unwrap();
            let index = project
                .program()
                .files
                .iter()
                .position(|file| file.file_name == "src/main.ts")
                .unwrap();
            session
                .with_file_checker_and_types(index, |checker, _, _, _| {
                    let key = (
                        index,
                        path.to_owned(),
                        None,
                        tsz::checker::context::ResolutionRequestKind::ReferencePath {
                            offset: path_offset,
                        },
                    );
                    assert_eq!(
                        checker
                            .ctx
                            .project_module_resolution_outcomes
                            .as_ref()
                            .unwrap()
                            .get(&key),
                        Some(&tsz::checker::context::ResolvedModuleRequestOutcome::Unresolved)
                    );
                    assert_eq!(
                        checker.ctx.resolve_import_target_from_file(index, path),
                        None
                    );
                })
                .unwrap();
        }
        std::fs::remove_file(&cleanup.0).unwrap();
        for request in &mut recipe.module_resolutions {
            request.target = TszProjectModuleResolutionTarget::File {
                path: "types/admitted.d.ts".into(),
            };
        }
        let report = authority.update(sources, recipe, &[]).unwrap();
        assert_eq!(report.parsed_and_bound, 0);
        assert!(!report.reused_project_result);
        assert!(
            codes(authority.project().unwrap()).is_empty(),
            "published captured files need no ambient disk counterpart"
        );
    }

    #[test]
    fn captured_reference_inputs_validate_exact_source_kind_offset_mode_and_target() {
        let source = "/// <reference types='environment' resolution-mode='require' />\nexport const sample = 'environment';";
        let mut recipe = options();
        let valid = TszProjectModuleResolution {
            importer_path: "src/main.ts".into(),
            specifier: "environment".into(),
            request_kind: TszProjectModuleRequestKind::ReferenceTypes {
                offset: source.find("environment").unwrap() as u32,
            },
            resolution_mode: Some(TszResolutionModeOverride::Require),
            target: TszProjectModuleResolutionTarget::Unresolved,
        };
        for invalid in [
            TszProjectModuleResolution {
                request_kind: TszProjectModuleRequestKind::ReferenceTypes {
                    offset: source.rfind("environment").unwrap() as u32,
                },
                ..valid.clone()
            },
            TszProjectModuleResolution {
                request_kind: TszProjectModuleRequestKind::ReferencePath {
                    offset: source.find("environment").unwrap() as u32,
                },
                ..valid.clone()
            },
            TszProjectModuleResolution {
                resolution_mode: Some(TszResolutionModeOverride::Import),
                ..valid.clone()
            },
            TszProjectModuleResolution {
                target: TszProjectModuleResolutionTarget::External {
                    identity: "ambient".into(),
                },
                ..valid.clone()
            },
        ] {
            recipe.module_resolutions = vec![invalid];
            let mut authority = TszProjectAuthority::new();
            assert!(matches!(
                authority.update(vec![input("src/main.ts", source)], recipe.clone(), &[]),
                Err(TszAuthorityError::ModuleResolution(
                    TszProjectModuleResolutionError::InvalidReference { .. }
                ))
            ));
        }
    }

    #[test]
    fn captured_reference_maps_require_complete_exact_coverage_except_no_resolve() {
        let source = "/// <reference path='./provided.d.ts' />\n/// <reference types='environment' />\nexport {};";
        let path = TszProjectModuleResolution {
            importer_path: "src/main.ts".into(),
            specifier: "./provided.d.ts".into(),
            request_kind: TszProjectModuleRequestKind::ReferencePath {
                offset: source.find("./provided.d.ts").unwrap() as u32,
            },
            resolution_mode: None,
            target: TszProjectModuleResolutionTarget::File {
                path: "src/provided.d.ts".into(),
            },
        };
        let types = TszProjectModuleResolution {
            importer_path: "src/main.ts".into(),
            specifier: "environment".into(),
            request_kind: TszProjectModuleRequestKind::ReferenceTypes {
                offset: source.find("environment").unwrap() as u32,
            },
            resolution_mode: None,
            target: TszProjectModuleResolutionTarget::Unresolved,
        };
        let sources = vec![
            input("src/main.ts", source),
            input("src/provided.d.ts", "interface Present {}"),
        ];
        for rows in [
            vec![],
            vec![path.clone()],
            vec![path.clone(), types.clone(), path.clone()],
            vec![
                path.clone(),
                types.clone(),
                TszProjectModuleResolution {
                    request_kind: TszProjectModuleRequestKind::ReferenceTypes { offset: 0 },
                    ..types.clone()
                },
            ],
        ] {
            let mut recipe = options();
            recipe.module_resolutions = rows;
            let mut authority = TszProjectAuthority::new();
            assert!(matches!(
                authority.update(sources.clone(), recipe, &[]),
                Err(TszAuthorityError::ModuleResolution(_))
            ));
            assert!(authority.project().is_none());
        }
        let mut recipe = options();
        recipe.module_resolutions = vec![path, types];
        let mut authority = TszProjectAuthority::new();
        authority.update(sources.clone(), recipe, &[]).unwrap();
        let codes = authority
            .project()
            .unwrap()
            .check_result()
            .file_results
            .iter()
            .flat_map(|file| &file.diagnostics)
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert!(!codes.contains(&6053));
        assert!(codes.contains(&2688));
        let mut no_resolve = options();
        no_resolve.checker.no_resolve = true;
        authority.update(sources, no_resolve, &[]).unwrap();
        assert!(
            !authority
                .project()
                .unwrap()
                .check_result()
                .file_results
                .iter()
                .flat_map(|file| &file.diagnostics)
                .any(|diagnostic| matches!(diagnostic.code, 6053 | 2688))
        );
    }
    #[test]
    fn compiler_file_format_changes_recheck_with_shared_map_and_reuse_binds() {
        use tsz::checker::context::{ResolutionModeOverride, ResolutionRequestKind};
        let syntax_target = |checker: &TszCheckerState<'_>,
                             main: usize,
                             expected_mode: ResolutionModeOverride| {
            let arena = checker.ctx.arena;
            let import = arena
                .source_files
                .first()
                .unwrap()
                .statements
                .nodes
                .iter()
                .find_map(|&statement| {
                    arena
                        .get(statement)
                        .and_then(|node| arena.get_import_decl(node))
                })
                .expect("actual import declaration");
            let specifier = arena.get_literal_text(import.module_specifier).unwrap();
            assert_eq!(specifier, "./value");
            assert!(import.attributes.is_none());
            let kind = ResolutionRequestKind::EsmImport;
            let mode = checker
                .ctx
                .resolution_mode_for_project_request(specifier, kind, None);
            assert_eq!(mode, Some(expected_mode));
            // None is an exact unadmitted key, not an implicit-format selector.
            assert_eq!(
                checker
                    .ctx
                    .resolve_import_target_from_file_for_request(main, specifier, None, kind),
                None
            );
            let target = checker
                .ctx
                .resolve_import_target_from_file_for_request(main, specifier, mode, kind)
                .unwrap();
            let opposite = match expected_mode {
                ResolutionModeOverride::Import => ResolutionModeOverride::Require,
                ResolutionModeOverride::Require => ResolutionModeOverride::Import,
            };
            let opposite_target = checker
                .ctx
                .resolve_import_target_from_file_for_request(main, specifier, Some(opposite), kind)
                .unwrap();
            assert_ne!(
                opposite_target, target,
                "the other admitted mode cannot borrow this syntax target"
            );
            target
        };
        let sources = vec![
            input(
                "src/main.ts",
                "import { value } from './value'; export const result: number = value;",
            ),
            input("src/es.ts", "export const value = 'esm';"),
            input("src/cjs.ts", "export const value = 42;"),
        ];
        let mut recipe = options();
        recipe.checker.module = TszModuleKind::NodeNext;
        recipe.module_resolutions = vec![
            (ResolutionModeOverride::Import, "src/es.ts"),
            (ResolutionModeOverride::Require, "src/cjs.ts"),
        ]
        .into_iter()
        .map(|(mode, path)| TszProjectModuleResolution {
            importer_path: "src/main.ts".into(),
            specifier: "./value".into(),
            request_kind: TszProjectModuleRequestKind::EsmImport,
            resolution_mode: Some(mode),
            target: TszProjectModuleResolutionTarget::File { path: path.into() },
        })
        .collect();
        let mut authority = TszProjectAuthority::new();
        for (format, expected, expected_error) in [
            (TszProjectFileFormat::EsModule, "src/es.ts", true),
            (TszProjectFileFormat::CommonJs, "src/cjs.ts", false),
        ] {
            let expected_mode = if format == TszProjectFileFormat::EsModule {
                ResolutionModeOverride::Import
            } else {
                ResolutionModeOverride::Require
            };
            recipe.file_formats = vec![TszProjectFileFormatInput {
                path: "src/main.ts".into(),
                format,
            }];
            let report = authority
                .update(sources.clone(), recipe.clone(), &[])
                .unwrap();
            if format == TszProjectFileFormat::CommonJs {
                assert_eq!(report.parsed_and_bound, 0);
                assert_eq!(report.reused_binds, 3);
                assert!(!report.reused_project_result);
            }
            let project = authority.project().unwrap();
            let main = project
                .program()
                .files
                .iter()
                .position(|file| file.file_name == "src/main.ts")
                .unwrap();
            project
                .with_file_checker_and_types(main, |checker, _, _, _| {
                    assert_eq!(
                        checker.ctx.file_is_esm,
                        Some(format == TszProjectFileFormat::EsModule)
                    );
                    assert!(Arc::ptr_eq(
                        checker.ctx.file_is_esm_map.as_ref().unwrap(),
                        &project.program().file_is_esm_map
                    ));
                    let target = syntax_target(checker, main, expected_mode);
                    assert_eq!(project.program().files[target].file_name, expected);
                })
                .unwrap();
            let cancelled = AtomicBool::new(false);
            let budget = execution_budget(&cancelled, 1_000_000);
            let session = project.checked_query_session(&budget).unwrap();
            session
                .with_file_checker_and_types(main, |checker, _, _, _| {
                    assert_eq!(
                        checker.ctx.file_is_esm,
                        Some(format == TszProjectFileFormat::EsModule)
                    );
                    assert!(Arc::ptr_eq(
                        checker.ctx.file_is_esm_map.as_ref().unwrap(),
                        &project.program().file_is_esm_map
                    ));
                    let target = syntax_target(checker, main, expected_mode);
                    assert_eq!(project.program().files[target].file_name, expected);
                })
                .unwrap();
            let actual_error = project
                .check_result()
                .file_results
                .iter()
                .filter(|file| file.file_name == "src/main.ts")
                .flat_map(|file| &file.diagnostics)
                .any(|diagnostic| diagnostic.code == 2322);
            assert_eq!(actual_error, expected_error);
        }
        assert!(
            authority
                .update(sources.clone(), recipe.clone(), &[])
                .unwrap()
                .reused_project_result
        );
        recipe.file_formats[0].format = TszProjectFileFormat::Unknown;
        let report = authority.update(sources, recipe, &[]).unwrap();
        assert_eq!(report.reused_binds, 3);
        assert!(!report.reused_project_result);
        let project = authority.project().unwrap();
        let main = project
            .program()
            .files
            .iter()
            .position(|file| file.file_name == "src/main.ts")
            .unwrap();
        project
            .with_file_checker_and_types(main, |checker, _, _, _| {
                assert_eq!(checker.ctx.file_is_esm, None);
                assert!(checker.ctx.file_is_esm_map.as_ref().unwrap().is_empty());
                assert_eq!(
                    checker.ctx.resolve_import_target_from_file_for_request(
                        main,
                        "./value",
                        None,
                        ResolutionRequestKind::EsmImport
                    ),
                    None
                );
                assert!(
                    checker
                        .ctx
                        .resolve_import_target_from_file_for_request(
                            main,
                            "./value",
                            Some(ResolutionModeOverride::Import),
                            ResolutionRequestKind::EsmImport
                        )
                        .is_some()
                );
            })
            .unwrap();
        assert!(
            matches!(project.with_file_checker_and_types(usize::MAX, |_, _, _, _| ()), Err(TszAuthorityError::MissingFile(index)) if index == usize::MAX)
        );
        let cancelled = AtomicBool::new(false);
        let budget = execution_budget(&cancelled, 1_000_000);
        let session = project.checked_query_session(&budget).unwrap();
        session
            .with_file_checker_and_types(main, |checker, _, _, _| {
                assert_eq!(checker.ctx.file_is_esm, None);
                assert!(Arc::ptr_eq(
                    checker.ctx.file_is_esm_map.as_ref().unwrap(),
                    &project.program().file_is_esm_map
                ));
                assert_eq!(
                    checker.ctx.resolve_import_target_from_file_for_request(
                        main,
                        "./value",
                        None,
                        ResolutionRequestKind::EsmImport
                    ),
                    None
                );
                assert!(
                    checker
                        .ctx
                        .resolve_import_target_from_file_for_request(
                            main,
                            "./value",
                            Some(ResolutionModeOverride::Import),
                            ResolutionRequestKind::EsmImport
                        )
                        .is_some()
                );
            })
            .unwrap();
        assert!(matches!(
            session.with_file_checker_and_types(usize::MAX, |_, _, _, _| ()),
            Err(TszProjectQuerySessionError::FileIndexOutOfRange {
                file_index: usize::MAX,
                ..
            })
        ));
    }

    #[test]
    fn compiler_file_format_admission_rejects_wrong_duplicate_and_noncanonical_paths() {
        let sources = vec![
            input("src/main.ts", "export {};"),
            input("other/main.ts", "export {};"),
        ];
        let mut authority = TszProjectAuthority::new();
        let row = |path: &str| TszProjectFileFormatInput {
            path: path.into(),
            format: TszProjectFileFormat::Unknown,
        };
        for (formats, expected) in [
            (
                vec![row("src/../src/main.ts")],
                TszProjectModuleResolutionError::InvalidVirtualPath {
                    path: "src/../src/main.ts".into(),
                },
            ),
            (
                vec![row("main.ts")],
                TszProjectModuleResolutionError::ImporterNotInProgram {
                    path: "main.ts".into(),
                },
            ),
            (
                vec![row("src/main.ts"), row("src/main.ts")],
                TszProjectModuleResolutionError::DuplicateProgramPath {
                    path: "src/main.ts".into(),
                },
            ),
        ] {
            let mut recipe = options();
            recipe.file_formats = formats;
            match authority.update(sources.clone(), recipe, &[]) {
                Err(TszAuthorityError::ModuleResolution(actual)) => assert_eq!(actual, expected),
                other => panic!("expected exact format admission failure, got {other:?}"),
            }
        }
        let mut recipe = options();
        recipe.file_formats = vec![TszProjectFileFormatInput {
            path: "other/main.ts".into(),
            format: TszProjectFileFormat::EsModule,
        }];
        authority.update(sources, recipe, &[]).unwrap();
        let map = &authority.project().unwrap().program().file_is_esm_map;
        assert_eq!(map.get("other/main.ts"), Some(&true));
        assert_eq!(map.get("src/main.ts"), None);
    }

    #[test]
    fn tsz_diagnostics_admit_exact_utf8_source_ranges() {
        let source = "const café = 1;\nconst value: number = 'wrong';";
        let mut authority = TszProjectAuthority::new();
        authority
            .update(vec![input("src/unicode.ts", source)], options(), &[])
            .expect("TSZ should report diagnostics instead of rejecting the run");
        let project = authority.project().expect("project result exists");
        let diagnostics: Vec<_> = project
            .check_result()
            .file_results
            .iter()
            .flat_map(|file| file.diagnostics.iter())
            .filter(|diagnostic| diagnostic.code == 2322)
            .collect();
        assert!(
            !diagnostics.is_empty(),
            "TSZ should report the assignment error"
        );
        let diagnostic = diagnostics[0];
        let span = project
            .diagnostic_utf8_span(diagnostic)
            .expect("diagnostic is a valid UTF-8 byte range");
        let expected_start = source.find("value").expect("declaration offset");
        assert_eq!(span.start as usize, expected_start);
        assert_eq!(span.end as usize, expected_start + "value".len());
    }

    #[test]
    fn declaration_name_source_uses_the_exact_retained_identifier_node() {
        let source = "export type Labels<T> = { [K in keyof T]: T[K] };";
        let mut authority = TszProjectAuthority::new();
        authority
            .update(vec![input("src/labels.ts", source)], options(), &[])
            .expect("TSZ should build the project");
        let project = authority.project().expect("project result exists");
        let file = project
            .program()
            .files
            .iter()
            .find(|file| file.file_name == "src/labels.ts")
            .expect("exact source file is retained");
        let mapped_key = file
            .arena
            .nodes
            .iter()
            .enumerate()
            .find_map(|(raw, node)| {
                let identifier = file.arena.get_identifier(node)?;
                (identifier.escaped_text == "K")
                    .then(|| u32::try_from(raw).ok())
                    .flatten()
            })
            .expect("mapped binder identifier is present in the source arena");

        assert_eq!(
            project.declaration_name_source("src/labels.ts", mapped_key, "K"),
            Some("K")
        );
        assert_eq!(
            project.declaration_name_source("src/labels.ts", mapped_key, "T"),
            None,
            "a valid node with a mismatched expected name is not admitted"
        );
        assert_eq!(
            project.declaration_name_source("other.ts", mapped_key, "K"),
            None,
            "an unadmitted source path is not admitted"
        );
        assert_eq!(
            project.declaration_name_source("src/labels.ts", u32::MAX, "K"),
            None,
            "an out-of-range authority node is rejected"
        );
        assert_eq!(
            project.source_content_digest("src/labels.ts"),
            Some(source_digest("src/labels.ts", source)),
            "the admitted path and exact retained source have a stable identity"
        );
    }

    #[test]
    fn anonymous_property_name_source_requires_a_parsed_member_in_owner_span() {
        let source = "export type Recursive = { next: Recursive; café: string };";
        let mut authority = TszProjectAuthority::new();
        authority
            .update(vec![input("src/recursive.ts", source)], options(), &[])
            .expect("TSZ should build the project");
        let project = authority.project().expect("project result exists");
        let declaration = (0, u32::try_from(source.len()).expect("source length fits"));

        assert_eq!(
            project.property_name_source_in_declaration("src/recursive.ts", declaration, "next"),
            Some("next"),
            "a type-literal property is borrowed from its exact parsed identifier node"
        );
        assert_eq!(
            project.property_name_source_in_declaration("src/recursive.ts", declaration, "café"),
            Some("café"),
            "UTF-8 property spans preserve their exact source bytes"
        );
        assert_eq!(
            project.property_name_source_in_declaration(
                "src/recursive.ts",
                declaration,
                "Recursive"
            ),
            None,
            "the declaration's own name is not evidence for a structural property"
        );
        assert_eq!(
            project.property_name_source_in_declaration("missing.ts", declaration, "next"),
            None,
            "an unadmitted file cannot lend property spelling"
        );
    }

    #[test]
    fn property_owner_batch_preserves_ambiguity_and_stops_before_publishing_an_index() {
        let mut authority = TszProjectAuthority::new();
        authority.update(vec![input("input.ts", concat!(
            r"export class Owner { same!: string; s\u0061me!: number; café!: string; ",
            "identical(value: string): void; identical(value: number): void; identical(value: unknown): void {} }\n",
            "export class Foreign { same!: boolean; foreignOnly!: string; }",
        ))], options(), &[]).expect("actual conflicting member declarations");
        let project = authority.project().unwrap();
        let file = &project.program().files[0];
        let owner_symbol = |expected: &str| {
            file.arena
                .nodes
                .iter()
                .enumerate()
                .find_map(|(raw, node)| {
                    let class = file.arena.get_class(node)?;
                    let name = file.arena.get_identifier(file.arena.get(class.name)?)?;
                    (name.escaped_text == expected)
                        .then(|| file.node_symbols.get(&u32::try_from(raw).ok()?).copied())
                        .flatten()
                })
                .expect("actual bound owner")
        };
        let owner = owner_symbol("Owner");
        let foreign = owner_symbol("Foreign");
        assert_ne!(owner, foreign);
        assert_eq!(
            project.property_name_source_from_owner(owner, "same"),
            None,
            "two raw spellings of a decoded name are ambiguous within this owner"
        );
        assert_eq!(
            project.property_name_source_from_owner(owner, "identical"),
            Some("identical"),
            "repeated identical overload spelling is unambiguous"
        );
        assert_eq!(
            project.property_name_source_from_owner(foreign, "same"),
            Some("same")
        );
        assert_eq!(
            project.property_name_source_from_owner(owner, "foreignOnly"),
            None
        );
        let mut charged = 0;
        let mut tokens = Vec::new();
        project
            .with_property_name_sources_from_owner(
                owner,
                |work| {
                    charged += work;
                    Ok(())
                },
                |name, raw| tokens.push((name, raw)),
            )
            .unwrap();
        assert_eq!(
            tokens.len(),
            6,
            "one visit per direct parsed member, including overloads"
        );
        let expected = 1
            + tokens.len() as u64
            + tokens
                .iter()
                .map(|(name, raw)| (name.len() + raw.len()) as u64)
                .sum::<u64>();
        assert_eq!(
            charged, expected,
            "one owner scan charges declarations, direct members and retained token bytes"
        );
        for stop in [
            tsz_common::ProjectExecutionStop::Cancelled,
            tsz_common::ProjectExecutionStop::Deadline,
            tsz_common::ProjectExecutionStop::WorkBudgetExhausted,
        ] {
            let mut consumed = 0;
            let mut build = || {
                let mut remaining = 15_u64;
                let mut local = Vec::new();
                project.with_property_name_sources_from_owner(
                    owner,
                    |work| {
                        remaining = remaining.checked_sub(work).ok_or(stop)?;
                        Ok(())
                    },
                    |name, raw| {
                        consumed += 1;
                        local.push((name, raw));
                    },
                )?;
                Ok::<_, tsz_common::ProjectExecutionStop>(local)
            };
            assert_eq!(
                build(),
                Err(stop),
                "no prefix index is returned on a typed stop"
            );
            assert_eq!(
                consumed, 1,
                "the stop occurs during a scan after its first actual token"
            );
            let mut recovered = Vec::new();
            project
                .with_property_name_sources_from_owner(
                    owner,
                    |_| Ok(()),
                    |name, raw| recovered.push((name, raw)),
                )
                .unwrap();
            assert_eq!(
                recovered, tokens,
                "a stopped visitor has no persistent partial cache"
            );
        }
    }

    #[test]
    fn property_owner_spelling_borrows_only_exact_direct_bound_members() {
        let mut authority = TszProjectAuthority::new();
        let mut project_options = options();
        project_options
            .module_resolutions
            .push(TszProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "../types".to_owned(),
                request_kind: TszProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: TszProjectModuleResolutionTarget::File {
                    path: "types.ts".to_owned(),
                },
            });
        authority.update(vec![
            input("types.ts", concat!(
                "export class Owner { label!: string; café!: string; v\\u0061lue!: number; nested!: { nestedOnly: string }; method(): void { class Inner { nestedClassOnly!: string; } interface InnerFace { nestedInterfaceOnly: string } const localOnly = 0; } }\n",
                "export interface Unbound { unbound: string; nested: { interfaceNested: string } }\n",
                "export declare const nonMember: string;",
            )),
            input("foreign.d.ts", r"export declare class Owner { l\u0061bel: string; foreignOnly: string; }"),
            input("src/main.ts", "import type { Owner } from '../types'; export declare const value: Owner;"),
        ], project_options, &[]).expect("real cross-file owner project");
        let (symbol, foreign_symbol, stable, declaration) = {
            let project = authority.project().unwrap();
            let owner_symbol =
                |path: &str, expected: &str| {
                    let file = project
                        .program()
                        .files
                        .iter()
                        .find(|file| file.file_name == path)
                        .unwrap();
                    file.arena
                        .nodes
                        .iter()
                        .enumerate()
                        .find_map(|(raw, node)| {
                            let name = file.arena.get_class(node).map(|class| class.name).or_else(
                                || {
                                    file.arena
                                        .get_interface(node)
                                        .map(|interface| interface.name)
                                },
                            )?;
                            let name = file.arena.get_identifier(file.arena.get(name)?)?;
                            (name.escaped_text == expected)
                                .then(|| file.node_symbols.get(&u32::try_from(raw).ok()?).copied())
                                .flatten()
                        })
                        .expect("owner declaration itself has a real binder symbol")
                };
            let symbol = owner_symbol("types.ts", "Owner");
            let foreign_symbol = owner_symbol("foreign.d.ts", "Owner");
            let interface = owner_symbol("types.ts", "Unbound");
            assert_ne!(symbol, foreign_symbol);
            assert_eq!(
                project.symbol_name_source(symbol, "label"),
                None,
                "the class symbol does not bind its property's identifier"
            );
            for (name, raw) in [
                ("label", "label"),
                ("café", "café"),
                ("value", r"v\u0061lue"),
            ] {
                let token = project
                    .property_name_source_from_owner(symbol, name)
                    .expect("exact owner member");
                assert_eq!(token, raw);
                let source = project.source_text("types.ts").unwrap();
                assert!(token.as_ptr() >= source.as_ptr());
                assert!(
                    token.as_bytes().as_ptr_range().end <= source.as_bytes().as_ptr_range().end,
                    "spelling borrows the original retained UTF-8 bytes"
                );
            }
            assert_eq!(
                project.property_name_source_from_owner(interface, "unbound"),
                Some("unbound"),
                "an explicitly supplied bound interface owner may lend its direct member"
            );
            assert_eq!(
                project.property_name_source_from_owner(foreign_symbol, "label"),
                Some(r"l\u0061bel")
            );
            for name in [
                "Owner",
                "nonMember",
                "nestedOnly",
                "nestedClassOnly",
                "nestedInterfaceOnly",
                "localOnly",
                "foreignOnly",
                "missing",
                "",
            ] {
                assert_eq!(
                    project.property_name_source_from_owner(symbol, name),
                    None,
                    "wrong member {name}"
                );
            }
            assert_eq!(
                project.property_name_source_from_owner(interface, "interfaceNested"),
                None
            );
            assert_eq!(
                project.property_name_source_from_owner(foreign_symbol, "café"),
                None
            );
            let file = project
                .program()
                .files
                .iter()
                .find(|file| file.file_name == "types.ts")
                .unwrap();
            let member_symbol = file
                .node_symbols
                .iter()
                .find_map(|(&raw, &symbol)| {
                    let node = file.arena.get(TszNodeIndex(raw))?;
                    let identifier = file.arena.get_identifier(node)?;
                    (identifier.escaped_text == "label").then_some(symbol)
                })
                .expect("class member has an actual bound name symbol");
            assert_ne!(member_symbol, symbol);
            assert_eq!(
                project.property_name_source_from_owner(member_symbol, "label"),
                Some("label")
            );
            assert_eq!(
                project.property_name_source_from_owner(member_symbol, "café"),
                None
            );
            assert!(
                !file.node_symbols.keys().any(|&raw| file
                    .arena
                    .get(TszNodeIndex(raw))
                    .and_then(|node| file.arena.get_identifier(node))
                    .is_some_and(|name| name.escaped_text == "unbound")),
                "interface member names have no member SymbolId to invent"
            );
            let data = project.program().symbols.get(symbol).unwrap();
            assert_eq!(data.declarations.len(), 1);
            assert_eq!(data.stable_declarations.len(), 1);
            (
                symbol,
                foreign_symbol,
                data.stable_declarations[0],
                data.declarations[0],
            )
        };
        // Corrupt only captured provenance for negative controls: no replacement
        // symbol or positive authority is synthesized by the fixture.
        // Rebuild an owned unsealed model from the same real binds. The checked
        // image remains immutable; corrupt only this detached negative control.
        let project = authority.project().unwrap();
        let binds: Vec<_> = project.bound_sources.values().map(Arc::as_ref).collect();
        let mut program = tsz::parallel::merge_bind_results_ref_with_project_semantic_options(
            &binds, project.options.semantic_options,
        );
        program.set_project_file_formats(&project.options.file_formats).unwrap();
        program.set_project_module_resolutions(&project.options.module_resolutions).unwrap();
        assert_eq!(
            TszProject::property_name_source_in_program(&program, symbol, "label"),
            Some("label"),
            "the detached native model preserves the real positive binding before corruption"
        );
        program
            .symbols
            .get_mut(symbol)
            .unwrap()
            .stable_declarations
            .clear();
        assert_eq!(
            TszProject::property_name_source_in_program(&program, symbol, "label"),
            None,
            "a missing stable partner is refused before zipped lookup"
        );
        program
            .symbols
            .get_mut(symbol)
            .unwrap()
            .stable_declarations
            .push(stable);
        program
            .symbols
            .get_mut(symbol)
            .unwrap()
            .stable_declarations[0]
            .file_idx = program
            .symbols
            .get(foreign_symbol)
            .unwrap()
            .stable_declarations[0]
            .file_idx;
        assert_eq!(
            TszProject::property_name_source_in_program(&program, symbol, "label"),
            None,
            "same-spelled foreign source cannot replace this symbol's declaration"
        );
        program
            .symbols
            .get_mut(symbol)
            .unwrap()
            .stable_declarations[0] = stable;
        program
            .symbols
            .get_mut(symbol)
            .unwrap()
            .stable_declarations[0]
            .end -= 1;
        assert_eq!(
            TszProject::property_name_source_in_program(&program, symbol, "label"),
            None,
            "a stale declaration span cannot lend spelling"
        );
        program
            .symbols
            .get_mut(symbol)
            .unwrap()
            .stable_declarations[0] = stable;
        let file = &mut program.files[stable.file_idx as usize];
        Arc::make_mut(&mut file.node_symbols).insert(declaration.0, foreign_symbol);
        assert_eq!(
            TszProject::property_name_source_in_program(&program, symbol, "label"),
            None,
            "even matching file/span needs the exact declaration-node symbol binding"
        );
    }

    #[test]
    fn symbol_name_source_uses_the_exact_cross_file_declaration_binding() {
        let mut authority = TszProjectAuthority::new();
        let mut project_options = options();
        project_options
            .module_resolutions
            .push(TszProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "../types".to_owned(),
                request_kind: TszProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: TszProjectModuleResolutionTarget::File {
                    path: "types.d.ts".to_owned(),
                },
            });
        authority
            .update(
                vec![
                    input(
                        "types.d.ts",
                        "export declare class ExternalSettings { label: string }\nexport interface UnboundSettings { unbound: string }",
                    ),
                    input("foreign.d.ts", r"export declare class ForeignSettings { l\u0061bel: string }"),
                    input(
                        "src/main.ts",
                        "import type { ExternalSettings } from '../types';\nexport const settings: ExternalSettings = { label: 'ready' };",
                    ),
                ],
                project_options,
                &[],
            )
            .expect("TSZ should build the cross-file project");
        let project = authority.project().expect("project result exists");
        let bound_property = |path: &str| {
            let file = project
                .program()
                .files
                .iter()
                .find(|file| file.file_name == path)
                .expect("declaration dependency is retained");
            file.node_symbols
                .iter()
                .find_map(|(&raw, &symbol)| {
                    let node = file.arena.get(TszNodeIndex(raw))?;
                    let identifier = file.arena.get_identifier(node)?;
                    (identifier.escaped_text == "label").then_some(symbol)
                })
                .expect("a class property-name node has an actual binder symbol")
        };
        let symbol = bound_property("types.d.ts");
        let foreign_symbol = bound_property("foreign.d.ts");
        assert_ne!(symbol, foreign_symbol);
        assert_eq!(project.symbol_name_source(symbol, "label"), Some("label"));
        assert_eq!(project.symbol_name_source(symbol, "ExternalSettings"), None);
        assert_eq!(project.symbol_name_source(symbol, "unbound"), None);
        assert_eq!(
            project.symbol_name_source(foreign_symbol, "label"),
            Some(r"l\u0061bel")
        );
        let file = project
            .program()
            .files
            .iter()
            .find(|file| file.file_name == "types.d.ts")
            .unwrap();
        let interface_range = file
            .arena
            .nodes
            .iter()
            .find_map(|node| {
                let interface = file.arena.get_interface(node)?;
                let name = file
                    .arena
                    .get(interface.name)
                    .and_then(|name| file.arena.get_identifier(name))?;
                (name.escaped_text == "UnboundSettings").then_some((node.pos, node.end))
            })
            .expect("exact unbound interface declaration");
        assert_eq!(
            project.property_name_source_in_declaration("types.d.ts", interface_range, "unbound"),
            Some("unbound")
        );
        assert_eq!(
            project.property_name_source_in_declaration("foreign.d.ts", interface_range, "unbound"),
            None
        );
        assert!(
            !file.node_symbols.keys().any(|&raw| {
                file.arena
                    .get(TszNodeIndex(raw))
                    .and_then(|node| file.arena.get_identifier(node))
                    .is_some_and(|identifier| identifier.escaped_text == "unbound")
            }),
            "an interface property has no binder symbol and cannot lend one to the class property"
        );
    }

    #[test]
    fn library_sources_are_exact_authority_and_invalidate_cached_project() {
        let mut authority = TszProjectAuthority::new();
        let mut first_options = options();
        first_options.checker.no_lib = true;
        let first_library = TszLibraryInput::from_utf8(
            "lib.fixture.d.ts",
            b"declare type LibraryLabel = string;".to_vec(),
        )
        .expect("host bytes form an in-memory TSZ library")
        .into_lib_file();
        let first = authority
            .update(
                vec![input("src/main.ts", "export const value = 1;")],
                first_options.clone(),
                &[Arc::clone(&first_library)],
            )
            .expect("explicit library source is admitted");
        assert_eq!(first.parsed_and_bound, 1);
        let project = authority.project().expect("project result exists");
        let lib_source = "declare type LibraryLabel = string;";
        let lib = first_library
            .arena
            .nodes
            .iter()
            .enumerate()
            .find_map(|(raw, node)| {
                let identifier = first_library.arena.get_identifier(node)?;
                (identifier.escaped_text == "LibraryLabel")
                    .then(|| u32::try_from(raw).ok())
                    .flatten()
            })
            .expect("library binder identifier exists");
        assert_eq!(
            project.declaration_name_source("lib.fixture.d.ts", lib, "LibraryLabel"),
            Some("LibraryLabel")
        );
        assert_eq!(
            project.source_content_digest("lib.fixture.d.ts"),
            Some(source_digest("lib.fixture.d.ts", lib_source))
        );

        let repeated = authority
            .update(
                vec![input("src/main.ts", "export const value = 1;")],
                first_options.clone(),
                &[Arc::clone(&first_library)],
            )
            .expect("unchanged explicit library set is reused");
        assert!(repeated.reused_project_result);

        let changed_library = TszLibraryInput::from_utf8(
            "lib.fixture.d.ts",
            b"declare type LibraryLabel = number;".to_vec(),
        )
        .expect("changed host bytes form a new in-memory TSZ library")
        .into_lib_file();
        let changed = authority
            .update(
                vec![input("src/main.ts", "export const value = 1;")],
                first_options,
                &[changed_library],
            )
            .expect("changed library bytes invalidate the project result");
        assert_eq!(changed.parsed_and_bound, 1);
        assert_eq!(changed.reused_binds, 0);
        assert!(!changed.reused_project_result);
    }

    #[test]
    fn equivalent_readmitted_libraries_preserve_cached_declaration_arena_provenance() {
        let library = || TszLibraryInput::from_utf8(
            "lib.fixture.d.ts", b"declare type LibraryLabel = string;".to_vec(),
        ).unwrap().into_lib_file();
        let original = library();
        let first = vec![
            input("src/a.ts", "export const a: LibraryLabel = 'a';"),
            input("src/b.ts", "export const b: LibraryLabel = 'b';"),
        ];
        let mut authority = TszProjectAuthority::new();
        authority.update(first.clone(), options(), &[Arc::clone(&original)]).unwrap();
        let unchanged = Arc::clone(authority.project().unwrap().bound_sources.get("src/b.ts").unwrap());
        // The production bridge constructs these objects again after each
        // fresh admission. Equal bytes do not make their arena objects equal.
        let readmitted = library();
        assert!(!Arc::ptr_eq(&original, &readmitted));
        let changed = vec![input("src/a.ts", "export const a: LibraryLabel = 'edited';"), first[1].clone()];
        let mut recipe = options();
        recipe.environment = TszEnvironmentFingerprint::from_sha256([0x73; 32]);
        let report = authority.update(changed.clone(), recipe.clone(), &[Arc::clone(&readmitted)]).unwrap();
        assert_eq!((report.parsed_and_bound, report.reused_binds), (1, 1));
        let project = authority.project().unwrap();
        assert!(Arc::ptr_eq(&project.lib_files[0], &original));
        assert!(Arc::ptr_eq(project.bound_sources.get("src/b.ts").unwrap(), &unchanged));
        let observed = |authority: &TszProjectAuthority| {
            let project = authority.project().unwrap();
            let session = project.checked_query_session_unmetered_for_test().unwrap();
            let index = session.file_index("src/b.ts").unwrap();
            session.with_file_checker_and_types(index, |checker, binder, _, _| {
                let ty = checker.get_type_of_symbol(binder.file_locals.get("b").unwrap());
                let ty = checker.resolve_lazy_type(ty);
                checker.format_type(ty)
            }).unwrap()
        };
        let mut cold = TszProjectAuthority::new();
        cold.update(changed, recipe, &[readmitted]).unwrap();
        assert_eq!(observed(&authority), observed(&cold));
        assert_eq!(observed(&authority), "string");
    }

    #[test]
    fn in_memory_library_input_rejects_invalid_utf8_and_untyped_paths() {
        assert_eq!(
            TszLibraryInput::from_utf8("lib.fixture.d.ts", vec![0xff]),
            Err(TszSourceError::InvalidUtf8)
        );
        assert_eq!(
            TszLibraryInput::from_utf8("lib.fixture.json", b"{}".to_vec()),
            Err(TszSourceError::UnsupportedExtension)
        );
        for path in [
            "../lib.fixture.d.ts",
            "/absolute/lib.fixture.d.ts",
            "./lib.fixture.d.ts",
            "src//lib.fixture.d.ts",
            "C:/lib.fixture.d.ts",
        ] {
            assert_eq!(
                TszLibraryInput::from_utf8(path, b"declare const value: string;".to_vec()),
                Err(TszSourceError::InvalidPath),
                "un-normalized or absolute path {path:?} must not enter project identity"
            );
        }
    }

    #[test]
    fn project_source_paths_cannot_alias_library_source_paths() {
        let mut authority = TszProjectAuthority::new();
        let same_path = Arc::new(tsz::lib_loader::LibFile::from_source(
            "src/library.d.ts".to_owned(),
            "declare type Duplicate = string;".to_owned(),
        ));
        let duplicate = authority.update(
            vec![input(
                "src/library.d.ts",
                "declare type Duplicate = number;",
            )],
            options(),
            &[same_path],
        );
        assert!(matches!(
            duplicate,
            Err(TszAuthorityError::Source(TszSourceError::DuplicatePath))
        ));

        let duplicate_library = Arc::new(tsz::lib_loader::LibFile::from_source(
            "lib.duplicate.d.ts".to_owned(),
            "declare type Duplicate = string;".to_owned(),
        ));
        let duplicate = authority.update(
            vec![input("src/main.ts", "export const value = 1;")],
            options(),
            &[Arc::clone(&duplicate_library), duplicate_library],
        );
        assert!(matches!(
            duplicate,
            Err(TszAuthorityError::Source(TszSourceError::DuplicatePath))
        ));
    }

    #[test]
    fn ts_and_javascript_profiles_parse_through_tsz_without_file_discovery() {
        let paths = [
            "src/a.ts",
            "src/a.tsx",
            "src/a.mts",
            "src/a.cts",
            "src/a.js",
            "src/a.jsx",
            "src/a.mjs",
            "src/a.cjs",
        ];
        for path in paths {
            validate_source_path(path).expect("supported TS/JS path");
        }
        let mut authority = TszProjectAuthority::new();
        let inputs = paths
            .iter()
            .map(|path| input(path, "export const value = 1;"))
            .collect();
        let report = authority
            .update(inputs, options(), &[])
            .expect("TSZ parses all supported profiles");
        assert_eq!(report.parsed_and_bound, paths.len());
        let project = authority.project().expect("project result exists");
        for path in paths {
            assert!(
                project
                    .bind_result(path)
                    .expect("source is retained")
                    .parse_diagnostics
                    .is_empty(),
                "TSZ parser accepts {path}"
            );
        }
        assert_eq!(
            validate_source_path("src/a.vue"),
            Err(TszSourceError::UnsupportedExtension)
        );
        assert_eq!(
            validate_source_path("src/.ts"),
            Err(TszSourceError::InvalidPath)
        );
    }

    #[test]
    fn checker_execution_policy_preserves_exact_sources_resolutions_and_diagnostics() {
        use std::num::NonZeroUsize;
        fn fixture() -> (Vec<TszFileInput>, TszProjectOptions) {
            let mut options = options();
            options.module_resolutions.push(TszProjectModuleResolution {
                importer_path: "src/consumer.ts".into(),
                specifier: "./value".into(),
                request_kind: TszProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: TszProjectModuleResolutionTarget::File {
                    path: "src/value.ts".into(),
                },
            });
            (
                vec![
                    input("src/value.ts", "export class Value { field = 1; }"),
                    input(
                        "src/consumer.ts",
                        "import { Value } from './value'; export const invalid: string = new Value().field;",
                    ),
                ],
                options,
            )
        }
        let (sources, opts) = fixture();
        let mut fresh = TszProjectAuthority::new();
        fresh.update(sources, opts, &[]).unwrap();
        let expected = fresh.project().unwrap();
        assert!(
            expected
                .check_result()
                .file_results
                .iter()
                .flat_map(|file| &file.diagnostics)
                .any(|diagnostic| diagnostic.code == 2322)
        );
        for workers in [1, 2, 4] {
            let (sources, opts) = fixture();
            let cancelled = AtomicBool::new(false);
            let budget = execution_budget(&cancelled, 1_000_000);
            let mut bounded = TszProjectAuthority::new();
            bounded
                .update_with_checker_execution(
                    sources,
                    opts,
                    &[],
                    TszProjectCheckerExecution {
                        checkpoint: &budget,
                        workers: NonZeroUsize::new(workers).unwrap(),
                    },
                )
                .unwrap();
            let actual = bounded.project().unwrap();
            assert!(!Arc::ptr_eq(
                &expected.program().definition_store,
                &actual.program().definition_store
            ));
            assert_eq!(
                actual.check_result().diagnostic_count,
                expected.check_result().diagnostic_count
            );
            assert_eq!(
                actual.check_result().file_results.len(),
                expected.check_result().file_results.len()
            );
            for (actual, expected) in actual
                .check_result()
                .file_results
                .iter()
                .zip(&expected.check_result().file_results)
            {
                assert_eq!(actual.file_name, expected.file_name);
                assert_eq!(actual.file_idx, expected.file_idx);
                assert_eq!(actual.diagnostics, expected.diagnostics);
            }
            for path in expected.source_paths() {
                assert_eq!(actual.source_text(path), expected.source_text(path));
                assert_eq!(
                    actual.source_content_digest(path),
                    expected.source_content_digest(path)
                );
            }
            let session = actual.checked_query_session(&budget).unwrap();
            let index = session.file_index("src/value.ts").unwrap();
            let field = session
                .with_file_checker_and_types(index, |checker, binder, _file, _types| {
                    checker.get_type_of_symbol(binder.file_locals.get("Value").unwrap())
                })
                .unwrap();
            assert_ne!(
                field,
                TszTypeId::UNKNOWN,
                "bounded project retains native query authority"
            );
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn checker_execution_one_shot_stop_after_merge_never_publishes_project() {
        use std::sync::atomic::Ordering;
        struct StopOnPrivateWorker {
            emitted: AtomicBool,
            entered: AtomicBool,
        }
        impl ExecutionCheckpoint for StopOnPrivateWorker {
            fn checkpoint(&self, _work: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
                let private = std::thread::current()
                    .name()
                    .is_some_and(|name| name.starts_with("tsz-checker-"));
                if private {
                    self.entered.store(true, Ordering::Release);
                    if !self.emitted.swap(true, Ordering::AcqRel) {
                        return Err(tsz_common::ProjectExecutionStop::Cancelled);
                    }
                }
                Ok(())
            }
        }
        let source = StopOnPrivateWorker {
            emitted: AtomicBool::new(false),
            entered: AtomicBool::new(false),
        };
        let mut authority = TszProjectAuthority::new();
        authority
            .update(
                vec![input("src/value.ts", "export const value = 1;")],
                options(),
                &[],
            )
            .unwrap();
        let stopped = authority.update_with_checker_execution(
            vec![input("src/value.ts", "export const value = 2;")],
            options(),
            &[],
            TszProjectCheckerExecution {
                checkpoint: &source,
                workers: std::num::NonZeroUsize::new(2).unwrap(),
            },
        );
        assert!(
            source.entered.load(Ordering::Acquire),
            "stop must occur in real private checker planning after parse/bind/merge"
        );
        assert!(matches!(
            stopped,
            Err(TszAuthorityError::ProjectCheck(
                tsz::parallel::ProjectProgramCheckError::ExecutionStopped(
                    tsz_common::ProjectExecutionStop::Cancelled
                )
            ))
        ));
        assert_eq!(
            source.checkpoint(0),
            Ok(()),
            "source really stops only once"
        );
        assert!(
            authority.project().is_none(),
            "stopped changed project cannot publish or retain prior Ready"
        );
    }

    fn hot_file_fixture() -> (Vec<TszFileInput>, TszProjectOptions) {
        let hot = (0..20_000)
            .map(|n| format!("export const v{n}: number = {n};"))
            .collect::<Vec<_>>()
            .join("\n");
        (
            vec![
                input("src/a.ts", "export const a: number = 1; const bad: number = 'x';"),
                input("src/hot.ts", &hot),
                input("src/b.ts", "export const b: string = 'b'; const bad: string = 2;"),
            ],
            options(),
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_work_policy_rebuilds_without_the_hot_file_and_keeps_every_other_result() {
        use std::num::NonZeroUsize;
        let (sources, opts) = hot_file_fixture();
        let cancelled = AtomicBool::new(false);
        let budget = execution_budget(&cancelled, 50_000_000);
        let execution = TszProjectCheckerExecution {
            checkpoint: &budget,
            workers: NonZeroUsize::new(2).unwrap(),
        };
        let mut isolated = TszProjectAuthority::new();
        let report = isolated
            .update_with_checker_execution_and_file_work(
                sources,
                opts,
                &[],
                execution,
                &TszFileWorkPolicy {
                    allowance: 5_000,
                    skipped: BTreeSet::new(),
                    max_retries: 2,
                    check_declaration_files: true,
                },
            )
            .unwrap();
        assert_eq!(report.file_work_retries, 1, "one rebuilt round without the hot file");
        let project = isolated.project().unwrap();
        assert_eq!(
            project.file_work_skipped().iter().map(String::as_str).collect::<Vec<_>>(),
            ["src/hot.ts"]
        );
        assert!(project.source_paths().any(|path| path == "src/hot.ts"), "still a program member");
        assert_eq!(budget.stop_reason(), None);

        // The surviving files are exactly what a project without the hot file reports.
        let (mut sources, opts) = hot_file_fixture();
        sources.retain(|source| source.path != "src/hot.ts");
        let mut baseline = TszProjectAuthority::new();
        baseline.update(sources, opts, &[]).unwrap();
        for name in ["src/a.ts", "src/b.ts"] {
            let diagnostics = |project: &TszProject| {
                project
                    .check_result()
                    .file_results
                    .iter()
                    .find(|file| file.file_name == name)
                    .unwrap()
                    .diagnostics
                    .clone()
            };
            let actual = diagnostics(isolated.project().unwrap());
            assert!(!actual.is_empty(), "{name} keeps its real diagnostics");
            assert_eq!(actual, diagnostics(baseline.project().unwrap()), "{name}");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_work_policy_can_skip_declaration_diagnostics_without_changing_application_facts() {
        use std::num::NonZeroUsize;
        let diagnostics = |check_declaration_files| {
            let cancelled = AtomicBool::new(false);
            let budget = execution_budget(&cancelled, 50_000_000);
            let mut authority = TszProjectAuthority::new();
            authority
                .update_with_checker_execution_and_file_work(
                    vec![
                        input("src/types.d.ts", "export interface Broken { missing: DoesNotExist; }"),
                        input(
                            "src/main.ts",
                            "import type { Broken } from './types'; export const bad: number = 'x'; export type B = Broken;",
                        ),
                    ],
                    {
                        let mut opts = options();
                        opts.module_resolutions.push(TszProjectModuleResolution {
                            importer_path: "src/main.ts".into(),
                            specifier: "./types".into(),
                            request_kind: TszProjectModuleRequestKind::EsmImport,
                            resolution_mode: None,
                            target: TszProjectModuleResolutionTarget::File {
                                path: "src/types.d.ts".into(),
                            },
                        });
                        opts
                    },
                    &[],
                    TszProjectCheckerExecution {
                        checkpoint: &budget,
                        workers: NonZeroUsize::new(2).unwrap(),
                    },
                    &TszFileWorkPolicy {
                        allowance: 10_000_000,
                        skipped: BTreeSet::new(),
                        max_retries: 0,
                        check_declaration_files,
                    },
                )
                .unwrap();
            let project = authority.project().unwrap();
            assert!(project.file_work_skipped().is_empty());
            let codes = |name: &str| {
                project
                    .check_result()
                    .file_results
                    .iter()
                    .find(|file| file.file_name == name)
                    .unwrap()
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.code)
                    .collect::<BTreeSet<_>>()
            };
            (codes("src/types.d.ts"), codes("src/main.ts"))
        };
        let (declaration_checked, application_checked) = diagnostics(true);
        let (declaration_skipped, application_skipped) = diagnostics(false);
        assert!(declaration_checked.contains(&2304), "declaration diagnostics are produced when asked for");
        assert!(declaration_skipped.is_empty(), "and omitted when the caller reads only facts");
        assert!(application_checked.contains(&2322));
        assert_eq!(application_checked, application_skipped, "application diagnostics are unchanged");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_work_policy_without_retries_names_the_hot_file_and_publishes_nothing() {
        use std::num::NonZeroUsize;
        let (sources, opts) = hot_file_fixture();
        let cancelled = AtomicBool::new(false);
        let budget = execution_budget(&cancelled, 50_000_000);
        let mut authority = TszProjectAuthority::new();
        let refused = authority.update_with_checker_execution_and_file_work(
            sources,
            opts,
            &[],
            TszProjectCheckerExecution {
                checkpoint: &budget,
                workers: NonZeroUsize::new(2).unwrap(),
            },
            &TszFileWorkPolicy {
                allowance: 5_000,
                skipped: BTreeSet::new(),
                max_retries: 0,
                check_declaration_files: true,
            },
        );
        let Err(TszAuthorityError::FileWorkExhausted(files)) = refused else {
            panic!("an unrecoverable hot file must be a typed error");
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/hot.ts");
        assert_eq!(files[0].allowance, 5_000);
        assert!(authority.project().is_none());
        assert_eq!(budget.stop_reason(), None, "the program budget survives a hot file");
    }

    #[test]
    fn checker_execution_precancelled_admission_keeps_typed_stop() {
        let cancelled = AtomicBool::new(true);
        let budget = execution_budget(&cancelled, 1_000_000);
        let mut authority = TszProjectAuthority::new();
        let stopped = authority.update_with_checker_execution(
            vec![input("../invalid.ts", "not admitted")],
            options(),
            &[],
            TszProjectCheckerExecution {
                checkpoint: &budget,
                workers: std::num::NonZeroUsize::new(2).unwrap(),
            },
        );
        assert!(matches!(
            stopped,
            Err(TszAuthorityError::ExecutionStopped(
                tsz_common::ProjectExecutionStop::Cancelled
            ))
        ));
        assert!(authority.project().is_none());
    }
}

#[cfg(test)]
mod declaration_policy_tests {
    use super::*;

    #[test]
    fn resolved_declaration_policy_rechecks_diagnostics_without_removing_sources_or_syntax() {
        let declaration = "export interface Broken { missing: DoesNotExist; }";
        // noLib makes required globals a program obligation independent of
        // the declaration member under test. Supply this unit program's own
        // globals explicitly; these are source inputs, not a substitute SDK.
        let globals = "interface Object {} interface Function {} interface CallableFunction {} \
            interface NewableFunction {} interface String {} interface Number {} \
            interface Boolean {} interface RegExp {} interface IArguments {} \
            interface Array<T> { length: number; [n: number]: T; }";
        let sources = vec![
            TszFileInput {
                path: "types/dep.d.ts".into(),
                source: declaration.into(),
            },
            TszFileInput {
                path: "types/globals.d.ts".into(),
                source: globals.into(),
            },
        ];
        let mut options = TszProjectOptions {
            checker: checker_options_from_compiler_api_json(
                r#"{"compilerOptions":{"skipLibCheck":true,"noLib":true}}"#,
            )
            .unwrap(),
            semantic_options: TszProjectSemanticOptions::declaration_scoped(),
            module_resolutions: Vec::new(),
            file_formats: Vec::new(),
            environment: TszEnvironmentFingerprint::from_sha256([0x67; 32]),
        };
        let mut authority = TszProjectAuthority::new();
        authority
            .update(sources.clone(), options.clone(), &[])
            .unwrap();
        assert!(
            authority.project().unwrap().check_result().file_results[0]
                .diagnostics
                .is_empty()
        );
        assert_eq!(
            authority.project().unwrap().source_text("types/dep.d.ts"),
            Some(declaration)
        );
        assert_eq!(
            authority.project().unwrap().source_text("types/globals.d.ts"),
            Some(globals)
        );
        assert_eq!(authority.project().unwrap().program.files.len(), 2);
        options.checker = checker_options_from_compiler_api_json(
            r#"{"compilerOptions":{"skipLibCheck":false,"noLib":true}}"#,
        )
        .unwrap();
        let report = authority.update(sources, options.clone(), &[]).unwrap();
        assert!(
            !report.reused_project_result,
            "typed policy participates in checker cache identity"
        );
        assert_eq!(
            authority.project().unwrap().check_result().file_results[0]
                .diagnostics
                .iter()
                .map(|d| d.code)
                .collect::<Vec<_>>(),
            [2304]
        );
        options.checker = checker_options_from_compiler_api_json(
            r#"{"compilerOptions":{"skipLibCheck":true,"noLib":true}}"#,
        )
        .unwrap();
        let invalid = vec![
            TszFileInput {
                path: "types/dep.d.ts".into(),
                source: "export interface Broken { value: ; }".into(),
            },
            TszFileInput {
                path: "types/globals.d.ts".into(),
                source: globals.into(),
            },
        ];
        authority.update(invalid, options, &[]).unwrap();
        assert!(
            !authority
                .project()
                .unwrap()
                .bind_result("types/dep.d.ts")
                .unwrap()
                .parse_diagnostics
                .is_empty(),
            "skipLibCheck cannot suppress parser diagnostics or remove syntax authority"
        );
    }
}
