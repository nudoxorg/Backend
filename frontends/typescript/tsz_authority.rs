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
use tsz::tsz_solver::construction::TypeDatabase;

pub use tsz::binder::BinderState as TszBinderState;
pub use tsz::binder::SymbolId as TszSymbolId;
pub use tsz::checker::context::CheckerOptions as TszCheckerOptions;
pub use tsz::checker::diagnostics::Diagnostic as TszDiagnostic;
pub use tsz::checker::state::CheckerState as TszCheckerState;
pub use tsz_common::ProjectSemanticOptions as TszProjectSemanticOptions;
pub use tsz::common::{ModuleKind as TszModuleKind, ScriptTarget as TszScriptTarget};
pub use tsz::parser::{NodeIndex as TszNodeIndex, ParseDiagnostic as TszParseDiagnostic};
pub use tsz::parallel::{
    ProjectModuleRequestKind as TszProjectModuleRequestKind,
    ProjectModuleResolution as TszProjectModuleResolution,
    ProjectModuleResolutionError as TszProjectModuleResolutionError,
    ProjectModuleResolutionTarget as TszProjectModuleResolutionTarget,
};
pub use tsz::tsz_solver::type_handles::TypeId as TszTypeId;
pub use tsz_common::options::module_detection::ModuleDetectionKind as TszModuleDetectionKind;

#[cfg(feature = "tsz-semantic-session-test-support")]
#[path = "tsz_query_session.rs"]
mod query_session;
#[cfg(feature = "tsz-semantic-session-test-support")]
pub use query_session::{TszProjectQuerySession, TszProjectQuerySessionError};

/// Caller-computed identity for a fully resolved TypeScript project context.
///
/// Include the normalized compiler options, tsconfig inheritance and project
/// references, module-resolution inputs, package-manager/lockfile identity,
/// resolved dependency and library content, and the selected TSZ revision.
/// A change invalidates all parse/bind cache entries because TSZ binding may
/// depend on those inputs.
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
    /// Exact compiler module-resolution authority could not be attached to
    /// the merged program without guessing a path or weakening a request.
    #[error(transparent)]
    ModuleResolution(#[from] TszProjectModuleResolutionError),
    /// The merged TSZ project could not lend the configured project-scoped
    /// checker for one exact file query.
    #[error(transparent)]
    ProjectCheckerSession(#[from] tsz::parallel::ProjectCheckerSessionError),
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
    /// Exact compiler-resolved outcome for every observed program import.
    /// Empty is an explicit declaration that the admitted program has no
    /// requests; it does not enable filename-based fallback.
    pub module_resolutions: Vec<TszProjectModuleResolution>,
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
    library_digest: [u8; 32],
    bound_sources: BTreeMap<String, Arc<tsz::parallel::BindResult>>,
    lib_files: Vec<Arc<tsz::lib_loader::LibFile>>,
    program: tsz::parallel::MergedProgram,
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
        let environment_changed =
            self.environment != Some(options.environment) || libraries_changed;
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
        let module_resolution_digest = module_resolution_digest(&options.module_resolutions);
        let module_resolutions_changed = self
            .project
            .as_ref()
            .is_some_and(|current| current.module_resolution_digest != module_resolution_digest);
        if environment_changed || checker_options_changed {
            self.bound_sources.clear();
            self.project = None;
            self.environment = Some(options.environment);
            self.libraries = Some(library_digest);
        } else if semantic_options_changed || module_resolutions_changed {
            // Semantic options and exact resolution outcomes affect merged
            // identities/checking, but not parser/binder output. Reuse exact
            // source binds while rebuilding the merged/checker project.
            self.project = None;
        }

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

        if files_to_parse.is_empty()
            && !environment_changed
            && self.project.as_ref().is_some_and(|current| {
                current.options.environment == options.environment
                    && current.checker_options_digest == checker_digest
                    && current.semantic_options_digest == semantic_digest
                    && current.module_resolution_digest == module_resolution_digest
                    && current.library_digest == library_digest
            })
            && report.removed_sources == 0
        {
            report.reused_project_result = true;
            return Ok(report);
        }

        report.parsed_and_bound = files_to_parse.len();
        if !files_to_parse.is_empty() {
            let parsed = tsz::parallel::parse_and_bind_parallel_with_libs_and_options(
                files_to_parse,
                lib_files,
                options.checker.target,
                options.checker.module_detection,
            );
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
        let mut program = tsz::parallel::merge_bind_results_ref_with_project_semantic_options(
            &bind_refs,
            options.semantic_options,
        );
        program.set_project_module_resolutions(&options.module_resolutions)?;
        let check = tsz::parallel::check_files_parallel_with_project_semantic_options(
            &program,
            &options.checker,
            lib_files,
            options.semantic_options,
        );
        self.project = Some(TszProject {
            checker_options_digest: checker_digest,
            semantic_options_digest: semantic_digest,
            module_resolution_digest,
            library_digest,
            lib_files: lib_files.to_vec(),
            options,
            bound_sources,
            program,
            check,
        });
        Ok(report)
    }

    /// Returns the most recently updated real TSZ project, if one exists.
    #[must_use]
    pub fn project(&self) -> Option<&TszProject> {
        self.project.as_ref()
    }
}

impl TszProject {
    /// TSZ's project-wide merged cross-file program.
    #[must_use]
    pub const fn program(&self) -> &tsz::parallel::MergedProgram {
        &self.program
    }

    /// TSZ's typed checker result and every checker diagnostic it retained.
    #[must_use]
    pub const fn check_result(&self) -> &tsz::parallel::CheckResult {
        &self.check
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

    /// Runs a typed checker query and exposes the same checker's native type
    /// database for direct structural IR mapping. All borrows remain scoped to
    /// this exact project/file transaction; no type handles escape the
    /// callback.
    ///
    /// # Errors
    /// Returns [`TszAuthorityError::MissingFile`] for an invalid merged index.
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
        let file = self
            .program
            .files
            .get(file_index)
            .ok_or(TszAuthorityError::MissingFile(file_index))?;
        let binder = tsz::parallel::create_binder_from_bound_file(file, &self.program, file_index);
        let query_cache =
            tsz::tsz_solver::construction::QueryCache::new(&self.program.type_interner)
                .with_definition_store(&self.program.definition_store)
                .with_project_semantic_options(self.options.semantic_options);
        let mut checker = TszCheckerState::new_with_shared_def_store(
            &file.arena,
            &binder,
            &query_cache,
            file.file_name.clone(),
            self.options.checker.clone(),
            Arc::clone(&self.program.definition_store),
        );
        checker.check_source_file(file.source_file);
        let types = checker.ctx.types.as_type_database();
        Ok(consume(&mut checker, &binder, file, types))
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

fn checker_options_digest(options: &TszCheckerOptions) -> [u8; 32] {
    // TSZ's options are a broad non-serializable struct. Its Debug output is
    // a same-revision in-process cache key only; the external environment
    // fingerprint remains the durable content-addressed identity.
    Sha256::digest(format!("{options:?}").as_bytes()).into()
}

fn semantic_options_digest(options: TszProjectSemanticOptions) -> [u8; 32] {
    Sha256::digest(format!("{options:?}").as_bytes()).into()
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

    fn options() -> TszProjectOptions {
        let mut checker = TszCheckerOptions::default();
        checker.no_lib = true;
        TszProjectOptions {
            checker,
            semantic_options: TszProjectSemanticOptions::structural(),
            module_resolutions: Vec::new(),
            environment: TszEnvironmentFingerprint::from_sha256([0x5a; 32]),
        }
    }

    fn input(path: &str, source: &str) -> TszFileInput {
        TszFileInput {
            path: path.to_owned(),
            source: source.to_owned(),
        }
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
        initial_options.module_resolutions = vec![resolution(
            TszProjectModuleResolutionTarget::File {
                path: "src/value.ts".to_owned(),
            },
        )];
        let mut authority = TszProjectAuthority::new();
        let initial = authority
            .update(sources.clone(), initial_options, &[])
            .expect("exact file resolution builds");
        assert_eq!(initial.parsed_and_bound, 2);
        assert!(authority
            .project()
            .expect("initial project exists")
            .program()
            .project_module_resolution_outcomes
            .is_some());

        let mut external_options = options();
        external_options.module_resolutions = vec![resolution(
            TszProjectModuleResolutionTarget::External {
                identity: "npm:fixture/value@1".to_owned(),
            },
        )];
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
        invalid_options.module_resolutions = vec![resolution(
            TszProjectModuleResolutionTarget::File {
                path: "src/not-admitted.ts".to_owned(),
            },
        )];
        assert!(matches!(
            authority.update(sources, invalid_options, &[]),
            Err(TszAuthorityError::ModuleResolution(
                TszProjectModuleResolutionError::TargetNotInProgram { .. }
            ))
        ));
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
    fn symbol_name_source_uses_the_exact_cross_file_declaration_binding() {
        let mut authority = TszProjectAuthority::new();
        authority
            .update(
                vec![
                    input(
                        "types.d.ts",
                        "export interface ExternalSettings { label: string }",
                    ),
                    input(
                        "src/main.ts",
                        "import type { ExternalSettings } from '../types';\nexport const settings: ExternalSettings = { label: 'ready' };",
                    ),
                ],
                options(),
                &[],
            )
            .expect("TSZ should build the cross-file project");
        let project = authority.project().expect("project result exists");
        let file = project
            .program()
            .files
            .iter()
            .find(|file| file.file_name == "types.d.ts")
            .expect("declaration dependency is retained");
        let symbol = file
            .node_symbols
            .iter()
            .find_map(|(&raw, &symbol)| {
                let node = file.arena.get(TszNodeIndex(raw))?;
                let identifier = file.arena.get_identifier(node)?;
                (identifier.escaped_text == "label").then_some(symbol)
            })
            .expect("the exact property-name node is bound to a symbol");

        assert_eq!(project.symbol_name_source(symbol, "label"), Some("label"));
        assert_eq!(project.symbol_name_source(symbol, "ExternalSettings"), None);
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
}
