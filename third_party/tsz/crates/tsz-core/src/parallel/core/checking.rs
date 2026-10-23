fn extract_skeletons_for_merge(results: &[&BindResult]) -> Vec<FileSkeleton> {
    const PARALLEL_SKELETON_FILE_THRESHOLD: usize = 128;

    if results.len() < PARALLEL_SKELETON_FILE_THRESHOLD {
        return results.iter().map(|r| extract_skeleton(r)).collect();
    }

    extract_skeletons_for_merge_large(results)
}

#[cfg(not(target_arch = "wasm32"))]
fn extract_skeletons_for_merge_large(results: &[&BindResult]) -> Vec<FileSkeleton> {
    ensure_rayon_global_pool();
    results.par_iter().map(|r| extract_skeleton(r)).collect()
}

#[cfg(target_arch = "wasm32")]
fn extract_skeletons_for_merge_large(results: &[&BindResult]) -> Vec<FileSkeleton> {
    results.iter().map(|r| extract_skeleton(r)).collect()
}

/// Full pipeline: Parse → Bind (parallel) → Merge (sequential)
///
/// This is the main entry point for multi-file compilation.
/// Lib files are automatically loaded and merged during binding.
pub fn compile_files(files: Vec<(String, String)>) -> MergedProgram {
    let lib_files = resolve_default_lib_files(ScriptTarget::ESNext)
        .unwrap_or_else(|err| panic!("failed to resolve default lib files: {err}"));
    compile_files_with_libs(files, &lib_files)
}

/// Full pipeline with explicit lib files.
///
/// Callers are responsible for providing the resolved lib file paths.
pub fn compile_files_with_libs(
    files: Vec<(String, String)>,
    lib_files: &[PathBuf],
) -> MergedProgram {
    let lib_paths: Vec<&Path> = lib_files.iter().map(PathBuf::as_path).collect();
    let bind_results = parse_and_bind_parallel_with_lib_files(files, &lib_paths);
    merge_bind_results(bind_results)
}

// =============================================================================
// Parallel Type Checking
// =============================================================================

/// Complete native-record ordering for exact deduplication. The shared TSC
/// comparator intentionally omits presentation fields; its equal rows still
/// need stable category/related-presentation ties before equal records group.
fn compare_native_diagnostic_records(
    left: &Diagnostic,
    right: &Diagnostic,
) -> std::cmp::Ordering {
    left.compare(right)
        .then_with(|| (left.category as u8).cmp(&(right.category as u8)))
        .then_with(|| {
            left.related_information
                .iter()
                .zip(&right.related_information)
                .map(|(left, right)| {
                    (left.category as u8)
                        .cmp(&(right.category as u8))
                        .then_with(|| left.depth.cmp(&right.depth))
                        .then_with(|| (left.kind as u8).cmp(&(right.kind as u8)))
                })
                .find(|ordering| ordering.is_ne())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

#[cfg(test)]
#[test]
fn parallel_native_diagnostic_records_preserve_comparator_ties() {
    use tsz_common::diagnostics::{DiagnosticCategory, RelatedInformationKind};
    let mut base = Diagnostic::error("src/main.ts", 4, 1, "same", 2304);
    base.related_information.push(Diagnostic::related_message(
        2304,
        "src/main.ts",
        5,
        1,
        "related",
    ));
    let mut category = base.clone();
    category.category = DiagnosticCategory::Warning;
    let mut related_category = base.clone();
    related_category.related_information[0].category = DiagnosticCategory::Error;
    let mut depth = base.clone();
    depth.related_information[0].depth = 1;
    let mut kind = base.clone();
    kind.related_information[0].kind = RelatedInformationKind::LocationPointer;
    let unique = [base.clone(), category, related_category, depth, kind];
    assert!(unique.iter().all(|record| record.compare(&base).is_eq()));
    let mut rows = vec![
        unique[0].clone(),
        unique[1].clone(),
        unique[0].clone(),
        unique[2].clone(),
        unique[3].clone(),
        unique[1].clone(),
        unique[4].clone(),
        unique[0].clone(),
        unique[2].clone(),
    ];
    let mut reversed = rows.iter().rev().cloned().collect::<Vec<_>>();
    ParallelCheckPlan::sort_and_dedup(&mut rows);
    ParallelCheckPlan::sort_and_dedup(&mut reversed);
    assert_eq!(
        rows, reversed,
        "full record order is independent of arrival order"
    );
    assert_eq!(rows.len(), unique.len());
    for expected in unique {
        assert_eq!(rows.iter().filter(|actual| **actual == expected).count(), 1);
    }
}

use crate::checker::context::{CheckerOptions, LibContext};
use crate::checker::diagnostics::Diagnostic;
use crate::checker::state::CheckerState;
use crate::lib_loader::LibFile;
use crate::parser::syntax_kind_ext;
use crate::tsz_solver::TypeId;
use tsz_common::{ExecutionCheckpoint as _, ProjectSemanticOptions};

/// Compiler-owned `SourceFile.impliedNodeFormat`. Unknown never supplies a mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFileFormat {
    Unknown,
    CommonJs,
    EsModule,
}

/// Format evidence for an exact member of the admitted compiler program.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectFileFormatInput {
    pub path: String,
    pub format: ProjectFileFormat,
}

/// Syntactic form of an import or source reference supplied by an admitted
/// compiler program. Reference offsets locate the attribute value in UTF-8
/// source bytes; these kinds never participate in generic module lookup.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProjectModuleRequestKind {
    EsmImport,
    DynamicImport,
    CjsRequire,
    EsmReExport,
    ReferencePath {
        offset: u32,
    },
    ReferenceTypes {
        offset: u32,
    },
    /// The compiler bridge saw a request kind TSZ does not model. Keeping it
    /// explicit makes the unsupported case fail closed instead of falling
    /// through to heuristic filename resolution.
    Unsupported {
        syntax_kind: String,
    },
}

/// Exact result for one importer/specifier/request-kind/mode tuple.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProjectModuleResolutionTarget {
    /// Resolved to another file in this TSZ program. `path` uses the same
    /// normalized virtual path as the corresponding `MergedProgram` file.
    File { path: String },
    /// Resolved successfully outside this program. `identity` is a stable,
    /// compiler-derived external target identity, not merely a guessed path.
    External { identity: String },
    /// The compiler resolver authoritatively found no target.
    Unresolved,
}

/// A compiler-owned module-resolution observation for one program request.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectModuleResolution {
    pub importer_path: String,
    pub specifier: String,
    pub request_kind: ProjectModuleRequestKind,
    pub resolution_mode: Option<crate::checker::context::ResolutionModeOverride>,
    pub target: ProjectModuleResolutionTarget,
}

/// A project-resolution input could not be applied without weakening its
/// authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectModuleResolutionError {
    InvalidVirtualPath {
        path: String,
    },
    DuplicateProgramPath {
        path: String,
    },
    ImporterNotInProgram {
        path: String,
    },
    TargetNotInProgram {
        path: String,
    },
    EmptySpecifier,
    EmptyExternalIdentity,
    InvalidReference {
        importer_path: String,
        offset: u32,
    },
    UnsupportedRequestKind {
        syntax_kind: String,
    },
    DuplicateRequest {
        importer_path: String,
        specifier: String,
    },
}

impl std::fmt::Display for ProjectModuleResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidVirtualPath { path } => {
                write!(formatter, "invalid normalized project path: {path:?}")
            }
            Self::DuplicateProgramPath { path } => {
                write!(formatter, "duplicate project program path: {path:?}")
            }
            Self::ImporterNotInProgram { path } => {
                write!(
                    formatter,
                    "module importer is not in the project program: {path:?}"
                )
            }
            Self::TargetNotInProgram { path } => {
                write!(
                    formatter,
                    "module target is not in the project program: {path:?}"
                )
            }
            Self::EmptySpecifier => formatter.write_str("module specifier must not be empty"),
            Self::EmptyExternalIdentity => {
                formatter.write_str("external module identity must not be empty")
            }
            Self::InvalidReference { importer_path, offset } => write!(
                formatter,
                "invalid captured reference at {importer_path:?}:{offset}"
            ),
            Self::UnsupportedRequestKind { syntax_kind } => write!(
                formatter,
                "unsupported TypeScript module request kind: {syntax_kind}"
            ),
            Self::DuplicateRequest {
                importer_path,
                specifier,
            } => write!(
                formatter,
                "duplicate module resolution request from {importer_path:?} for {specifier:?}"
            ),
        }
    }
}

impl std::error::Error for ProjectModuleResolutionError {}

fn validate_project_virtual_path(path: &str) -> Result<(), ProjectModuleResolutionError> {
    let components: Vec<&str> = path.split('/').collect();
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || components
            .iter()
            .any(|component| component.is_empty() || *component == "." || *component == "..")
    {
        return Err(ProjectModuleResolutionError::InvalidVirtualPath {
            path: path.to_owned(),
        });
    }
    Ok(())
}

/// Result of type checking a single function body
#[derive(Debug)]
pub struct FunctionCheckResult {
    /// Function node index within its file
    pub function_idx: NodeIndex,
    /// File index in the program
    pub file_idx: usize,
    /// Inferred return type
    pub return_type: TypeId,
    /// Diagnostics produced
    pub diagnostics: Vec<Diagnostic>,
}

/// Result of type checking all function bodies in a file
#[derive(Debug)]
pub struct FileCheckResult {
    /// File index
    pub file_idx: usize,
    /// File name
    pub file_name: String,
    /// Function check results
    pub function_results: Vec<FunctionCheckResult>,
    /// File-level diagnostics
    pub diagnostics: Vec<Diagnostic>,
}

use super::diagnostics::{
    add_parallel_global_augmentation_member_conflict_diagnostics,
    add_reexported_module_augmentation_enum_conflict_diagnostics,
    affected_lib_extension_interface_names, affected_lib_interface_names,
    build_lib_bound_file_for_interface_checks, lib_file_contains_affected_interface,
    suppress_parallel_import_shadowing_namespace_type_diagnostics,
    suppress_parallel_ts2339_cascade_diagnostics,
};

/// Result of parallel type checking
#[derive(Debug)]
pub struct CheckResult {
    /// Per-file check results
    pub file_results: Vec<FileCheckResult>,
    /// Total functions checked
    pub function_count: usize,
    /// Total diagnostics
    pub diagnostic_count: usize,
}

/// Why a reusable project checker session could not be created or queried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectCheckerSessionError {
    /// The merged program has not been bound to compiler-owned module
    /// resolution outcomes, so queries could otherwise guess by filename.
    MissingProjectModuleResolutions,
    ModuleResolution(ProjectModuleResolutionError),
    /// A prepared check or query request used a budget outside its admission.
    ExecutionCheckpointMismatch,
    /// The requested file index is outside the exact merged program.
    FileIndexOutOfRange { file_index: usize, file_count: usize },
    /// The one shared cancellation/deadline/work budget stopped this session.
    ExecutionStopped(tsz_common::ProjectExecutionStop),
    /// One file query spent its whole per-file work allowance. Its output is
    /// partial and was discarded, and the checked image may carry state that
    /// the interrupted query wrote: the caller must stop using this image for
    /// further queries and rebuild it.
    FileWorkExhausted { file_index: usize, allowance: u64 },
}

impl std::fmt::Display for ProjectCheckerSessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingProjectModuleResolutions => {
                formatter.write_str("project checker session requires exact module resolutions")
            }
            Self::ModuleResolution(error) => write!(formatter, "{error}"),
            Self::ExecutionCheckpointMismatch => formatter.write_str(
                "project checker scope does not match its admitted execution budget",
            ),
            Self::FileIndexOutOfRange {
                file_index,
                file_count,
            } => write!(
                formatter,
                "project checker file index {file_index} is outside {file_count} files"
            ),
            Self::ExecutionStopped(reason) => {
                write!(formatter, "project checker session stopped: {reason:?}")
            }
            Self::FileWorkExhausted {
                file_index,
                allowance,
            } => write!(
                formatter,
                "project checker file {file_index} spent its {allowance} unit work allowance"
            ),
        }
    }
}

impl std::error::Error for ProjectCheckerSessionError {}

impl From<ProjectModuleResolutionError> for ProjectCheckerSessionError {
    fn from(error: ProjectModuleResolutionError) -> Self { Self::ModuleResolution(error) }
}

/// Reusable, read-only project checking context for exact semantic queries.
///
/// The session owns one `ParallelCheckPlan`, so binders, arenas, module
/// resolution outcomes, library contexts, and shared query caches are built
/// once. Per-file checker state is lent only for the callback duration.
/// `TypeId` is a copyable arena-local handle rather than a branded value, so
/// callers must also keep any returned IDs tied to the exact `MergedProgram`
/// used to create this session.
pub struct ProjectCheckerSession<'a> {
    plan: ParallelCheckPlan<'a>,
    /// Per-file query work allowance; `None` leaves each query on the shared
    /// program checkpoint alone.
    file_work: Option<u64>,
}

impl<'a> ProjectCheckerSession<'a> {
    /// Build the shared project context for a program with exact compiler
    /// module-resolution outcomes attached.
    pub fn new(
        program: &'a MergedProgram,
        checker_options: &'a CheckerOptions,
        lib_files: &[Arc<LibFile>],
        project_semantic_options: ProjectSemanticOptions,
        execution_checkpoint: &'a dyn tsz_common::ExecutionCheckpoint,
    ) -> Result<Self, ProjectCheckerSessionError> {
        execution_checkpoint
            .checkpoint(0)
            .map_err(ProjectCheckerSessionError::ExecutionStopped)?;
        let project_module_resolution_outcomes = program
            .project_module_resolution_outcomes
            .clone()
            .ok_or(ProjectCheckerSessionError::MissingProjectModuleResolutions)?;
        validate_project_reference_outcomes_with_poll(program, Some(checker_options.no_resolve),
            &project_module_resolution_outcomes,
            |work| execution_checkpoint.checkpoint(work).map_err(ProjectCheckerSessionError::ExecutionStopped))?;
        let plan = ParallelCheckPlan::build(
            program,
            checker_options,
            lib_files,
            project_semantic_options,
            Some(project_module_resolution_outcomes),
            Some(execution_checkpoint),
        ).map_err(ProjectCheckerSessionError::ExecutionStopped)?;
        plan.checkpoint(0)
            .map_err(ProjectCheckerSessionError::ExecutionStopped)?;
        plan.prime_module_augmentation_bodies();
        plan.checkpoint(0)
            .map_err(ProjectCheckerSessionError::ExecutionStopped)?;
        Ok(Self {
            plan,
            file_work: None,
        })
    }

    /// Run every file query under its own work allowance of `allowance` units.
    ///
    /// A query that spends its allowance returns
    /// [`ProjectCheckerSessionError::FileWorkExhausted`] and its output is
    /// discarded. The interrupted query may have written partial state into
    /// the image's program-wide tables, so a session must not be used again
    /// after that error: rebuild the image first.
    #[must_use]
    pub fn with_file_work_allowance(mut self, allowance: u64) -> Self {
        self.file_work = Some(allowance);
        self
    }

    /// Lend the exact configured checker for one file and its type database.
    ///
    /// The higher-ranked callback and owned `Output` prevent checker state,
    /// binders, or type-database borrows from escaping this session call.
    pub fn with_file_checker_and_types<Output>(
        &self,
        file_index: usize,
        consume: impl for<'checker> FnOnce(
            &mut CheckerState<'checker>,
            &BinderState,
            &BoundFile,
            &dyn tsz_solver::construction::TypeDatabase,
        ) -> Output,
    ) -> Result<Output, ProjectCheckerSessionError> {
        let file = self
            .plan
            .program
            .files
            .get(file_index)
            .ok_or(ProjectCheckerSessionError::FileIndexOutOfRange {
                file_index,
                file_count: self.plan.program.files.len(),
            })?;
        let query_span = tsz_common::ProjectExecutionSpan::new(
            self.plan.execution_control(), tsz_common::ProjectExecutionPhase::FileQuery,
            Some(tsz_common::ProjectExecutionFile { index: file_index, path: &file.file_name }),
        );
        self.plan.checkpoint(0)
            .map_err(ProjectCheckerSessionError::ExecutionStopped)?;
        let Some(allowance) = self.file_work else {
            let output = self
                .plan
                .with_bound_file_checker_and_types(file_index, file, consume);
            self.plan.checkpoint(0)
                .map_err(ProjectCheckerSessionError::ExecutionStopped)?;
            query_span.returned();
            return Ok(output);
        };
        let parent = self.plan.execution_control().ok_or(
            ProjectCheckerSessionError::ExecutionStopped(
                tsz_common::ProjectExecutionStop::Cancelled,
            ),
        )?;
        let slice = tsz_common::FileWorkSlice::new(parent, allowance);
        let output = self.plan.with_bound_file_checker_and_types_over(
            file_index, file, Some(&slice), consume,
        );
        // A program-wide stop wins over this file's own exhaustion.
        self.plan.checkpoint(0)
            .map_err(ProjectCheckerSessionError::ExecutionStopped)?;
        if slice.is_exhausted() {
            // The parent program still has work, but this image can retain
            // partial shared solver writes. Poison only this image so neither
            // another session nor a fresh budget can reuse those writes.
            if let Some(stop) = self.plan.execution_scope.as_ref().and_then(|scope| scope.image_stop) {
                let _ = stop.compare_exchange(0, 3, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire);
            }
            return Err(ProjectCheckerSessionError::FileWorkExhausted {
                file_index,
                allowance,
            });
        }
        query_span.returned();
        Ok(output)
    }

    /// Lend the checker and bound file when the caller does not need the
    /// underlying type database directly.
    pub fn with_file_checker<Output>(
        &self,
        file_index: usize,
        consume: impl for<'checker> FnOnce(
            &mut CheckerState<'checker>,
            &BinderState,
            &BoundFile,
        ) -> Output,
    ) -> Result<Output, ProjectCheckerSessionError> {
        self.with_file_checker_and_types(file_index, |checker, binder, file, _types| {
            consume(checker, binder, file)
        })
    }
}

/// Collect all function declarations from a source file
fn collect_functions(arena: &NodeArena, source_file: NodeIndex) -> Vec<NodeIndex> {
    let mut functions = Vec::new();

    let Some(sf) = arena.get_source_file_at(source_file) else {
        return functions;
    };

    for &stmt_idx in &sf.statements.nodes {
        collect_functions_from_node(arena, stmt_idx, &mut functions);
    }

    functions
}

/// Recursively collect functions from a node
fn collect_functions_from_node(
    arena: &NodeArena,
    node_idx: NodeIndex,
    functions: &mut Vec<NodeIndex>,
) {
    let Some(node) = arena.get(node_idx) else {
        return;
    };

    match node.kind {
        k if k == syntax_kind_ext::FUNCTION_DECLARATION
            || k == syntax_kind_ext::FUNCTION_EXPRESSION
            || k == syntax_kind_ext::ARROW_FUNCTION =>
        {
            functions.push(node_idx);
            // Also collect nested functions in the body
            if let Some(func) = arena.get_function(node)
                && func.body.is_some()
            {
                collect_functions_from_node(arena, func.body, functions);
            }
        }
        k if k == syntax_kind_ext::METHOD_DECLARATION => {
            functions.push(node_idx);
            // Also collect nested functions in the body
            if let Some(method) = arena.get_method_decl(node)
                && method.body.is_some()
            {
                collect_functions_from_node(arena, method.body, functions);
            }
        }
        k if k == syntax_kind_ext::CLASS_DECLARATION => {
            if let Some(class) = arena.get_class(node) {
                for &member_idx in &class.members.nodes {
                    collect_functions_from_node(arena, member_idx, functions);
                }
            }
        }
        k if k == syntax_kind_ext::BLOCK => {
            if let Some(block) = arena.get_block(node) {
                for &stmt_idx in &block.statements.nodes {
                    collect_functions_from_node(arena, stmt_idx, &mut *functions);
                }
            }
        }
        k if k == syntax_kind_ext::VARIABLE_STATEMENT => {
            // Variable statement contains a declaration list which contains declarations
            if let Some(var_stmt) = arena.get_variable(node) {
                // var_stmt.declarations contains the VARIABLE_DECLARATION_LIST node(s)
                for &decl_list_idx in &var_stmt.declarations.nodes {
                    if let Some(decl_list_node) = arena.get(decl_list_idx) {
                        // The declaration list also uses VariableData
                        if let Some(decl_list) = arena.get_variable(decl_list_node) {
                            // Now decl_list.declarations contains the actual VARIABLE_DECLARATION nodes
                            for &decl_idx in &decl_list.declarations.nodes {
                                if let Some(decl_node) = arena.get(decl_idx)
                                    && let Some(decl) = arena.get_variable_declaration(decl_node)
                                    && decl.initializer.is_some()
                                {
                                    collect_functions_from_node(arena, decl.initializer, functions);
                                }
                            }
                        }
                    }
                }
            }
        }
        k if k == syntax_kind_ext::EXPORT_DECLARATION => {
            // Export declarations may contain function/class declarations
            if let Some(export) = arena.get_export_decl(node)
                && export.export_clause.is_some()
            {
                collect_functions_from_node(arena, export.export_clause, functions);
            }
        }
        _ => {}
    }
}

/// Type check function bodies in parallel
///
/// After binding is complete and symbols are merged, function bodies
/// can be type-checked in parallel because:
/// 1. Each function body only uses local variables and global symbols
/// 2. Local type inference doesn't modify global state
/// 3. Each function is independent
///
/// # Arguments
/// * `program` - The merged program with global symbols
///
/// # Returns
/// `CheckResult` with diagnostics from all functions
///
/// This is a reusable core/test-harness entry point, not the production CLI
/// diagnostic scheduler. CLI semantic-diagnostic behavior is owned by
/// `crates/tsz-cli/src/driver/check.rs::collect_diagnostics`, which also owns
/// file scheduling, cache/watch invalidation, checker reuse, and diagnostic
/// ordering.
pub fn check_functions_parallel(
    program: &MergedProgram,
) -> Result<CheckResult, ProjectModuleResolutionError> {
    check_functions_parallel_with_project_semantic_options(
        program,
        ProjectSemanticOptions::structural(),
    )
}

/// Check function bodies with immutable semantic policies for this project.
/// Every Rayon worker receives a `QueryCache` carrying the same policy value.
pub fn check_functions_parallel_with_project_semantic_options(
    program: &MergedProgram,
    project_semantic_options: ProjectSemanticOptions,
) -> Result<CheckResult, ProjectModuleResolutionError> {
    if let Some(outcomes) = program.project_module_resolution_outcomes.as_ref() {
        validate_project_reference_outcomes_with_poll(program, Some(false), outcomes, |_| Ok(()))?;
    }
    ensure_rayon_global_pool();

    let file_names: Vec<String> = program
        .files
        .iter()
        .map(|file| file.file_name.clone())
        .collect();
    let (resolved_module_paths, resolved_modules) =
        if let Some(outcomes) = program.project_module_resolution_outcomes.as_ref() {
            (
                unambiguous_project_file_path_map(outcomes),
                FxHashSet::default(),
            )
        } else {
            crate::checker::module_resolution::build_module_resolution_maps(&file_names)
        };
    let resolved_module_paths = Arc::new(resolved_module_paths);

    let shared_binders: Vec<Arc<BinderState>> = program
        .files
        .iter()
        .enumerate()
        .map(|(file_idx, file)| {
            Arc::new(create_binder_from_bound_file_with_program_owners(
                file, program, file_idx,
            ))
        })
        .collect();
    let all_binders = Arc::new(shared_binders.clone());
    let all_arenas = Arc::new(
        program
            .files
            .iter()
            .map(|file| Arc::clone(&file.arena))
            .collect::<Vec<_>>(),
    );
    // PERF: Build arena-pointer -> file-index reverse lookup map first (O(F)),
    // then map each symbol to its file index in O(1) per symbol.
    // Total: O(S + F) instead of the previous O(S * F) nested iteration.
    let arena_to_file_idx: FxHashMap<usize, usize> = all_arenas
        .iter()
        .enumerate()
        .map(|(idx, arena)| (Arc::as_ptr(arena) as usize, idx))
        .collect();
    let symbol_file_targets: Vec<(tsz_binder::SymbolId, usize)> = program
        .symbol_arenas
        .iter()
        .filter_map(|(sym_id, arena)| {
            arena_to_file_idx
                .get(&(Arc::as_ptr(arena) as usize))
                .map(|&file_idx| (*sym_id, file_idx))
        })
        .collect();

    // Pre-compute the symbol->file index as a shared read-only map.
    // Each checker gets an Arc clone (O(1)) instead of O(N) per-checker insertion.
    let global_symbol_file_index: Arc<FxHashMap<tsz_binder::SymbolId, usize>> = Arc::new(
        symbol_file_targets
            .iter()
            .copied()
            .collect::<FxHashMap<_, _>>(),
    );

    // First, collect all functions from all files (sequential)
    let mut all_functions: Vec<(usize, NodeIndex)> = Vec::new();

    for (file_idx, file) in program.files.iter().enumerate() {
        let functions = collect_functions(&file.arena, file.source_file);
        for func_idx in functions {
            all_functions.push((file_idx, func_idx));
        }
    }

    let function_count = all_functions.len();

    // Check functions in parallel
    // Note: We need to be careful here - CheckerState holds mutable references
    // For now, we group by file and check each file's functions together
    let file_results: Vec<FileCheckResult> = maybe_parallel_iter!(program.files)
        .enumerate()
        .map(|(file_idx, file)| {
            let functions = collect_functions(&file.arena, file.source_file);

            let binder = Arc::clone(&shared_binders[file_idx]);

            // Create a per-thread QueryCache for memoized evaluate_type/is_subtype_of calls.
            // Each thread gets its own cache using RefCell/Cell (no atomic overhead).
            // Attach the shared DefinitionStore so generic-call inference can resolve
            // cross-arena declaration identity (issue #14344, `TSZ_XARENA_BASE_DECL`).
            let query_cache = tsz_solver::construction::QueryCache::new(&program.type_interner)
                .with_project_semantic_options(project_semantic_options)
                .with_definition_store(&program.definition_store);

            // Create checker for this file, using the shared type interner
            let compiler_options = crate::checker::context::CheckerOptions::default();
            let mut checker = CheckerState::new_with_shared_def_store(
                &file.arena,
                binder.as_ref(),
                &query_cache,
                file.file_name.clone(),
                compiler_options, // default options for internal operations
                std::sync::Arc::clone(&program.definition_store),
            );
            checker.ctx.file_is_esm = program.file_is_esm_map.get(&checker.ctx.file_name).copied();
            checker.ctx.file_is_esm_map = Some(Arc::clone(&program.file_is_esm_map));
            checker.ctx.set_all_arenas(Arc::clone(&all_arenas));
            checker.ctx.set_all_binders(Arc::clone(&all_binders));
            checker.ctx.set_current_file_idx(file_idx);
            checker
                .ctx
                .set_resolved_module_paths(Arc::clone(&resolved_module_paths));
            checker.ctx.set_resolved_modules(resolved_modules.clone());
            if let Some(outcomes) = program.project_module_resolution_outcomes.as_ref() {
                checker
                    .ctx
                    .set_project_module_resolution_outcomes(Arc::clone(outcomes));
            }
            checker
                .ctx
                .set_global_symbol_file_index(Arc::clone(&global_symbol_file_index));

            let mut function_results = Vec::new();

            for func_idx in functions {
                // Check the function
                let return_type = checker.get_type_of_node(func_idx);

                function_results.push(FunctionCheckResult {
                    function_idx: func_idx,
                    file_idx,
                    return_type,
                    diagnostics: Vec::new(), // Diagnostics are collected at file level
                });
            }

            // Collect diagnostics from checker
            let diagnostics = std::mem::take(&mut checker.ctx.diagnostics);

            FileCheckResult {
                file_idx,
                file_name: file.file_name.clone(),
                function_results,
                diagnostics,
            }
        })
        .collect();

    let diagnostic_count: usize = file_results.iter().map(|r| r.diagnostics.len()).sum();

    Ok(CheckResult {
        file_results,
        function_count,
        diagnostic_count,
    })
}

/// Type check full source files in parallel using Rayon.
///
/// Each file gets its own `CheckerState` with file-local mutable state, sharing
/// only thread-safe structures (`Arc`-wrapped arenas/binders, `DashMap`-backed
/// `TypeInterner` and `DefinitionStore`). Per-thread `QueryCache` instances use
/// `RefCell`/`Cell` for zero-overhead single-threaded caching within each file.
///
/// Diagnostics are sorted by `(start, code)` within each file and deduplicated
/// by `(start, code)` after collection, ensuring deterministic output regardless
/// of thread scheduling.
///
/// This helper remains lower-level reusable infrastructure. Production CLI
/// diagnostics flow through `collect_diagnostics` in `tsz-cli` so the driver can
/// preserve CLI diagnostics, cache invalidation, and file-session reuse policy.
/// Feature and fidelity fixes for CLI checking should usually start in that
/// scheduler before changing this helper.
///
/// The body is intentionally thin: [`ParallelCheckPlan::build`] computes the
/// shared scheduling inputs once, [`ParallelCheckPlan::run_file_checks`] and
/// [`ParallelCheckPlan::run_lib_checks`] perform the per-file/per-lib work, and
/// [`ParallelCheckPlan::aggregate`] folds the results into the final
/// `CheckResult`.
pub fn check_files_parallel(
    program: &MergedProgram,
    checker_options: &CheckerOptions,
    lib_files: &[Arc<LibFile>],
) -> Result<CheckResult, ProjectModuleResolutionError> {
    check_files_parallel_with_project_semantic_options(
        program,
        checker_options,
        lib_files,
        ProjectSemanticOptions::structural(),
    )
}

/// Check a complete program while carrying one immutable semantic policy into
/// every file, module-augmentation, and library query cache created by Rayon.
pub fn check_files_parallel_with_project_semantic_options(
    program: &MergedProgram,
    checker_options: &CheckerOptions,
    lib_files: &[Arc<LibFile>],
    project_semantic_options: ProjectSemanticOptions,
) -> Result<CheckResult, ProjectModuleResolutionError> {
    if let Some(outcomes) = program.project_module_resolution_outcomes.as_ref() {
        validate_project_reference_outcomes_with_poll(
            program, Some(checker_options.no_resolve), outcomes, |_| Ok(()),
        )?;
    }
    Ok(check_files_parallel_with_optional_project_inputs(
        program,
        checker_options,
        lib_files,
        project_semantic_options,
        program.project_module_resolution_outcomes.clone(),
    ))
}

/// Check a complete program using exact compiler-owned module resolutions.
///
/// Supplying this map puts the checker in authoritative project mode: every
/// request is resolved only by its importer/specifier/kind/mode tuple. Missing
/// module entries and `Unresolved` entries remain unresolved; `External` entries
/// are known successful resolutions without an in-program file. Source reference
/// rows require exact coverage unless `noResolve` is enabled. No filename or
/// basename guessing is allowed in this mode.
pub fn check_files_parallel_with_project_inputs(
    program: &MergedProgram,
    checker_options: &CheckerOptions,
    lib_files: &[Arc<LibFile>],
    project_semantic_options: ProjectSemanticOptions,
    resolutions: &[ProjectModuleResolution],
) -> Result<CheckResult, ProjectModuleResolutionError> {
    let outcomes = build_project_module_resolution_outcomes_with_poll(
        program, resolutions, Some(checker_options.no_resolve), |_| Ok(()),
        |path, _| validate_project_virtual_path(path),
    )?;
    Ok(check_files_parallel_with_optional_project_inputs(
        program,
        checker_options,
        lib_files,
        project_semantic_options,
        Some(Arc::new(outcomes)),
    ))
}

/// Check a complete project under exact compiler-owned resolutions and the
/// caller's shared cooperative work checkpoint.
pub fn check_files_parallel_with_project_inputs_and_execution_checkpoint(
    program: &MergedProgram,
    checker_options: &CheckerOptions,
    lib_files: &[Arc<LibFile>],
    project_semantic_options: ProjectSemanticOptions,
    resolutions: &[ProjectModuleResolution],
    execution_checkpoint: &dyn tsz_common::ExecutionCheckpoint,
) -> Result<CheckResult, ProjectProgramCheckError> {
    let execution_scope = tsz_common::LatchedExecutionCheckpoint::new(execution_checkpoint);
    let execution_checkpoint = &execution_scope as &dyn tsz_common::ExecutionCheckpoint;
    execution_checkpoint
        .checkpoint(0)
        .map_err(ProjectProgramCheckError::ExecutionStopped)?;
    let outcomes = build_project_module_resolution_outcomes_with_poll(
        program,
        resolutions,
        Some(checker_options.no_resolve),
        |work| {
            execution_checkpoint
                .checkpoint(work)
                .map_err(ProjectProgramCheckError::ExecutionStopped)
        },
        |path, poll| validate_project_virtual_path_with_poll(path, poll),
    )?;
    let plan = ParallelCheckPlan::build(
        program,
        checker_options,
        lib_files,
        project_semantic_options,
        Some(Arc::new(outcomes)),
        Some(execution_checkpoint),
    )
    .map_err(ProjectProgramCheckError::ExecutionStopped)?;
    let poll = || {
        execution_checkpoint
            .checkpoint(0)
            .map_err(ProjectProgramCheckError::ExecutionStopped)
    };
    poll()?;
    ensure_rayon_global_pool();
    plan.prime_module_augmentation_bodies();
    poll()?;
    let mut file_results = plan.run_file_checks();
    poll()?;
    plan.run_lib_checks_with_execution_checkpoint(&mut file_results)
        .map_err(ProjectProgramCheckError::ExecutionStopped)?;
    let result = plan
        .aggregate_with_poll(file_results, |work| execution_checkpoint.checkpoint(work))
        .map_err(ProjectProgramCheckError::ExecutionStopped)?;
    poll()?;
    Ok(result)
}

/// Typed failure while checking an admitted project program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectProgramCheckError {
    /// The compiler-owned resolution closure could not be applied to this program.
    ModuleResolution(ProjectModuleResolutionError),
    /// The caller's cooperative execution control stopped checking.
    ExecutionStopped(tsz_common::ProjectExecutionStop),
}

impl std::fmt::Display for ProjectProgramCheckError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModuleResolution(error) => write!(formatter, "{error}"),
            Self::ExecutionStopped(reason) => {
                write!(formatter, "project check stopped: {reason:?}")
            }
        }
    }
}

impl std::error::Error for ProjectProgramCheckError {}

fn build_project_module_resolution_outcomes(
    program: &MergedProgram,
    resolutions: &[ProjectModuleResolution],
) -> Result<crate::checker::context::ResolvedModuleRequestOutcomeMap, ProjectModuleResolutionError>
{
    build_project_module_resolution_outcomes_with_poll(
        program,
        resolutions,
        None,
        |_| Ok(()),
        |path, _poll| validate_project_virtual_path(path),
    )
}

/// Project legacy post-processing's `(file, specifier)` view only from exact
/// compiler outcomes that agree across every request mode/kind. Conflicting,
/// external, and unresolved requests are deliberately omitted.
fn unambiguous_project_file_path_map(
    outcomes: &crate::checker::context::ResolvedModuleRequestOutcomeMap,
) -> FxHashMap<(usize, String), usize> {
    let mut by_specifier: FxHashMap<(usize, String), Option<usize>> = FxHashMap::default();
    let mut conflicts = FxHashSet::default();
    for ((file_idx, specifier, _, kind), outcome) in outcomes {
        if kind.is_reference() {
            continue;
        }
        let key = (*file_idx, specifier.clone());
        let target = match outcome {
            crate::checker::context::ResolvedModuleRequestOutcome::File(target_idx) => {
                Some(*target_idx)
            }
            crate::checker::context::ResolvedModuleRequestOutcome::External { .. }
            | crate::checker::context::ResolvedModuleRequestOutcome::Unresolved => None,
        };
        if let Some(existing) = by_specifier.get(&key) {
            if *existing != target {
                conflicts.insert(key);
            }
        } else {
            by_specifier.insert(key, target);
        }
    }
    by_specifier
        .into_iter()
        .filter_map(|(key, target)| {
            (!conflicts.contains(&key))
                .then_some(target.map(|target_idx| (key, target_idx)))
                .flatten()
        })
        .collect()
}

fn check_files_parallel_with_optional_project_inputs<'a>(
    program: &'a MergedProgram,
    checker_options: &'a CheckerOptions,
    lib_files: &[Arc<LibFile>],
    project_semantic_options: ProjectSemanticOptions,
    project_module_resolution_outcomes: Option<
        Arc<crate::checker::context::ResolvedModuleRequestOutcomeMap>,
    >,
) -> CheckResult {
    // Ensure Rayon global pool has adequate stack size for deep type-checking recursion.
    ensure_rayon_global_pool();

    let plan = ParallelCheckPlan::build(
        program,
        checker_options,
        lib_files,
        project_semantic_options,
        project_module_resolution_outcomes,
        None,
    )
    .expect("unmetered project plan cannot observe an execution stop");
    plan.prime_module_augmentation_bodies();
    let mut file_results = plan.run_file_checks();
    plan.run_lib_checks(&mut file_results);
    plan.aggregate(file_results)
}

/// Stable key for matching a lib diagnostic against the baseline lib run.
///
/// Lib files are checked twice (once standalone for the baseline, once with
/// user augmentations applied); diagnostics that appear in both runs are
/// pre-existing lib noise and are filtered out. The fingerprint
/// `(file_name, start, code, message_text)` identifies "the same diagnostic"
/// across the two runs.
fn lib_diagnostic_fingerprint(file_name: &str, diag: &Diagnostic) -> (String, u32, u32, String) {
    (
        file_name.to_owned(),
        diag.start,
        diag.code,
        diag.message_text.clone(),
    )
}

/// Shared, read-only scheduling inputs for one parallel file check.
///
/// `ParallelCheckPlan` captures everything the per-file and per-lib workers
/// need — resolved module maps, shared binders/arenas, lib contexts, the shared
/// query cache, and the affected-lib interface sets — computed once from the
/// `MergedProgram` and `CheckerOptions`. Holding these as named fields keeps
/// [`check_files_parallel`] a thin orchestrator and makes the
/// scheduling/diagnostics boundary explicit: `build` produces the plan, the
/// `check_one_*` workers consume it, and [`ParallelCheckPlan::aggregate`] folds
/// the per-file results into the final [`CheckResult`].
fn project_plan_checkpoint(
    checkpoint: Option<&dyn tsz_common::ExecutionCheckpoint>,
    work_units: u64,
) -> Result<(), tsz_common::ProjectExecutionStop> {
    checkpoint.map_or(Ok(()), |checkpoint| checkpoint.checkpoint(work_units))
}

/// Private index authority, owned together with its exact immutable program.
/// It is never publicly attachable to another program.
struct SealedProjectCheckerIndexes {
    tables: crate::checker::context::SealedProgramGlobalIndexes,
}

impl SealedProjectCheckerIndexes {
    fn build(
        binders: Arc<Vec<Arc<BinderState>>>,
        arenas: Arc<Vec<Arc<NodeArena>>>,
        declared_modules: Option<Arc<crate::checker::context::GlobalDeclaredModules>>,
    ) -> Self {
        Self {
            tables: crate::checker::context::SealedProgramGlobalIndexes::build(
                binders,
                arenas,
                declared_modules,
            ),
        }
    }

    fn install_into(&self, ctx: &mut crate::checker::context::CheckerContext<'_>) {
        // The sealed table owns its exact views; no caller-provided filename,
        // mutable program map, query result, or alternate owner can rebind them.
        self.tables.install_into(ctx);
    }
}

struct ParallelCheckPlan<'a> {
    program: &'a MergedProgram,
    checker_options: &'a CheckerOptions,
    project_semantic_options: ProjectSemanticOptions,
    project_module_resolution_outcomes:
        Option<Arc<crate::checker::context::ResolvedModuleRequestOutcomeMap>>,
    execution_scope: Option<ProjectCheckerExecutionScope<'a>>,
    views: Arc<ProjectCheckerViews>,
}

impl std::ops::Deref for ParallelCheckPlan<'_> {
    type Target = ProjectCheckerViews;
    fn deref(&self) -> &Self::Target { &self.views }
}

struct ProjectCheckerViews {
    checker_indexes: SealedProjectCheckerIndexes,
    resolved_module_paths: Arc<FxHashMap<(usize, String), usize>>,
    resolved_modules: Arc<FxHashSet<String>>,
    checker_lib_files: Vec<Arc<LibFile>>,
    lib_contexts: Arc<Vec<LibContext>>,
    /// Prebuilt union of lib `file_locals` names, shared (`Arc`) into every
    /// per-file checker so identifier resolution can skip the `O(num_lib_files)`
    /// `file_locals` scan for names no lib declares. Built once here rather than
    /// per file (it is `O(total lib symbols)`).
    lib_file_local_names: Option<Arc<FxHashSet<String>>>,
    all_binders: Arc<Vec<Arc<BinderState>>>,
    all_arenas: Arc<Vec<Arc<NodeArena>>>,
    global_symbol_file_index: Arc<FxHashMap<tsz_binder::SymbolId, usize>>,
    shared_declared_modules: Option<Arc<crate::checker::context::GlobalDeclaredModules>>,
    shared_query_cache: Option<tsz_solver::construction::SharedQueryCache>,
    /// Type-parameter extraction results shared only within this exact image.
    cross_file_type_params: crate::checker::context::CrossFileTypeParamsCache,
    affected_lib_interfaces: FxHashSet<String>,
    affected_lib_extension_interfaces: FxHashSet<String>,
}

/// Immutable compiler-owned data, shared only within one exact program.
/// Mutable solver state is confined to the existing project-scoped caches and
/// their stop latch; neither views nor options can be rebound after sealing.
struct ProjectCheckerImageContents {
    program: Arc<MergedProgram>,
    checker_options: CheckerOptions,
    semantics: ProjectSemanticOptions,
    views: Arc<ProjectCheckerViews>,
    stopped: std::sync::atomic::AtomicU8,
}

/// A sealed program that has not yet completed its full check. No query API is
/// exposed on this state. Checking consumes the builder, and only a complete
/// result can produce [`ProjectCheckerImage`].
pub struct PreparedProjectCheckerImage {
    contents: ProjectCheckerImageContents,
    execution_identity: Option<tsz_common::ProjectExecutionIdentity>,
    file_work: Option<ProjectFileWork>,
}

/// A fully checked exact native program and reusable views/cache. Persistent
/// validity depends on the exact program and its stop latch, not a past budget.
/// Each query session borrows a new caller-admitted checkpoint for its lifetime.
pub struct ProjectCheckerImage {
    contents: ProjectCheckerImageContents,
}

impl std::ops::Deref for ProjectCheckerImage {
    type Target = MergedProgram;
    fn deref(&self) -> &Self::Target {
        &self.contents.program
    }
}

struct ProjectCheckerExecutionScope<'a> {
    checkpoint: tsz_common::LatchedExecutionCheckpoint<'a>,
    image_stop: Option<&'a std::sync::atomic::AtomicU8>,
}

impl<'a> ProjectCheckerExecutionScope<'a> {
    fn new(checkpoint: &'a dyn tsz_common::ExecutionCheckpoint, image_stop: Option<&'a std::sync::atomic::AtomicU8>) -> Self {
        Self { checkpoint: tsz_common::LatchedExecutionCheckpoint::new(checkpoint), image_stop }
    }
    fn decode_stop(value: u8) -> Option<tsz_common::ProjectExecutionStop> {
        use tsz_common::ProjectExecutionStop::*;
        match value { 1 => Some(Cancelled), 2 => Some(Deadline), 3 => Some(WorkBudgetExhausted), _ => None }
    }
}

impl tsz_common::ExecutionCheckpoint for ProjectCheckerExecutionScope<'_> {
    fn execution_identity(&self) -> Option<tsz_common::ProjectExecutionIdentity> { self.checkpoint.execution_identity() }

    fn checkpoint(&self, work: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
        use std::sync::atomic::Ordering;
        if let Some(stop) = self.image_stop.and_then(|stop| Self::decode_stop(stop.load(Ordering::Acquire))) {
            return Err(stop);
        }
        let result = self.checkpoint.checkpoint(work);
        if let (Err(reason), Some(stop)) = (result, self.image_stop) {
            use tsz_common::ProjectExecutionStop::*;
            let value = match reason { Cancelled => 1, Deadline => 2, WorkBudgetExhausted => 3 };
            let _ = stop.compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire);
        }
        // A concurrent worker can poison the image after this worker's poll.
        self.image_stop.and_then(|stop| Self::decode_stop(stop.load(Ordering::Acquire))).map_or(result, Err)
    }
    fn observe(&self, event: tsz_common::ProjectExecutionEvent<'_>) { self.checkpoint.observe(event); }
}

impl PreparedProjectCheckerImage {
    /// Seal one exact program. Metered callers supply their existing checkpoint;
    /// absence preserves the explicit unmetered test/legacy entry point.
    pub fn new(
        program: MergedProgram,
        checker_options: CheckerOptions,
        lib_files: &[Arc<LibFile>],
        semantics: ProjectSemanticOptions,
        checkpoint: Option<&dyn tsz_common::ExecutionCheckpoint>,
    ) -> Result<Self, ProjectCheckerSessionError> {
        let outcomes = program.project_module_resolution_outcomes.clone()
            .ok_or(ProjectCheckerSessionError::MissingProjectModuleResolutions)?;
        validate_project_reference_outcomes_with_poll(&program, Some(checker_options.no_resolve), &outcomes,
            |work| checkpoint.map_or(Ok(()), |checkpoint| checkpoint.checkpoint(work)).map_err(ProjectCheckerSessionError::ExecutionStopped))?;
        let plan_span = tsz_common::ProjectExecutionSpan::new(checkpoint, tsz_common::ProjectExecutionPhase::CheckerPlan, None);
        let views = ParallelCheckPlan::build(&program, &checker_options, lib_files, semantics, Some(outcomes), checkpoint)
            .map_err(ProjectCheckerSessionError::ExecutionStopped)?.views;
        plan_span.returned();
        Ok(Self {
            contents: ProjectCheckerImageContents {
                program: Arc::new(program),
                checker_options,
                semantics,
                views,
                stopped: std::sync::atomic::AtomicU8::new(0),
            },
            execution_identity: checkpoint.and_then(|checkpoint| checkpoint.execution_identity()),
            file_work: None,
        })
    }

    /// Isolate each file's semantic check under its own work allowance and skip
    /// the files the policy names. Applies to [`Self::check_with_partitions`].
    #[must_use]
    pub fn with_file_work(mut self, file_work: ProjectFileWork) -> Self {
        self.file_work = Some(file_work);
        self
    }

    /// Build shared views inside the same caller-bounded private worker policy
    /// used for file checking. Library cloning never enters an ambient pool.
    pub fn new_with_checker_execution(
        program: MergedProgram,
        checker_options: CheckerOptions,
        lib_files: &[Arc<LibFile>],
        semantics: ProjectSemanticOptions,
        execution: ProjectCheckerExecution<'_>,
    ) -> Result<Self, ProjectPartitionCheckError> {
        execution.checkpoint.checkpoint(0).map_err(|reason| ProjectPartitionCheckError::Program(ProjectProgramCheckError::ExecutionStopped(reason)))?;
        let workers = execution.workers.get().min(program.files.len().max(1));
        let build = || Self::new(program, checker_options, lib_files, semantics, Some(execution.checkpoint))
            .map_err(|error| match error {
                ProjectCheckerSessionError::ExecutionStopped(reason) => ProjectPartitionCheckError::Program(ProjectProgramCheckError::ExecutionStopped(reason)),
                other => ProjectPartitionCheckError::CheckerSession(other),
            });
        run_on_private_checker_workers(workers, build)
    }

    fn accepts_checkpoint(
        &self,
        checkpoint: Option<&dyn tsz_common::ExecutionCheckpoint>,
    ) -> bool {
        self.execution_identity == checkpoint.and_then(|checkpoint| checkpoint.execution_identity())
    }

    /// Complete every admitted source before exposing a queryable image. The
    /// build and check stages must share their originally admitted budget.
    pub fn check_with_partitions(
        self,
        execution: ProjectCheckerExecution<'_>,
    ) -> Result<(ProjectCheckerImage, CheckResult), ProjectPartitionCheckError> {
        if !self.accepts_checkpoint(Some(execution.checkpoint)) {
            return Err(ProjectPartitionCheckError::CheckerSession(
                ProjectCheckerSessionError::ExecutionCheckpointMismatch,
            ));
        }
        let result = {
            let plan = self.contents.plan(Some(execution.checkpoint));
            check_prepared_project_partitions(
                &plan,
                execution.workers,
                self.file_work.as_ref(),
                &|_| {},
                &|_| {},
            )?
        };
        Ok((
            ProjectCheckerImage { contents: self.contents },
            result,
        ))
    }

    /// Explicit fresh-file execution for legacy and test callers. A failed
    /// check consumes and discards all prepared data, including cached sentinels.
    pub fn check_fresh(
        self,
        checkpoint: Option<&dyn tsz_common::ExecutionCheckpoint>,
    ) -> Result<(ProjectCheckerImage, CheckResult), ProjectCheckerSessionError> {
        if !self.accepts_checkpoint(checkpoint) {
            return Err(ProjectCheckerSessionError::ExecutionCheckpointMismatch);
        }
        let result = {
            let plan = self.contents.plan(checkpoint);
            let poll = |work| {
                plan.checkpoint(work)
                    .map_err(ProjectCheckerSessionError::ExecutionStopped)
            };
            poll(0)?;
            ensure_rayon_global_pool();
            plan.prime_module_augmentation_bodies();
            poll(0)?;
            let mut results = plan.run_file_checks();
            poll(0)?;
            plan.run_lib_checks_with_execution_checkpoint(&mut results)
                .map_err(ProjectCheckerSessionError::ExecutionStopped)?;
            let result = plan.aggregate_with_poll(results, |work| poll(work))?;
            poll(0)?;
            result
        };
        Ok((
            ProjectCheckerImage { contents: self.contents },
            result,
        ))
    }
}

impl ProjectCheckerImageContents {
    fn plan<'a>(
        &'a self,
        checkpoint: Option<&'a dyn tsz_common::ExecutionCheckpoint>,
    ) -> ParallelCheckPlan<'a> {
        ParallelCheckPlan {
            program: &self.program,
            checker_options: &self.checker_options,
            project_semantic_options: self.semantics,
            project_module_resolution_outcomes: self.program.project_module_resolution_outcomes.clone(),
            execution_scope: checkpoint.map(|checkpoint| {
                ProjectCheckerExecutionScope::new(checkpoint, Some(&self.stopped))
            }),
            views: Arc::clone(&self.views),
        }
    }
}

impl ProjectCheckerImage {
    /// A stopped image cannot lend another session, even under a fresh budget.
    pub fn is_usable(&self) -> bool {
        self.contents.stopped.load(std::sync::atomic::Ordering::Acquire) == 0
    }

    /// Borrow fully checked immutable data under a caller-admitted scope. The
    /// session retains that exact checkpoint; it has no API to swap budgets.
    /// No views are rebuilt and full checking/augmentation priming is not repeated.
    pub fn query_session<'a>(
        &'a self,
        checkpoint: &'a dyn tsz_common::ExecutionCheckpoint,
    ) -> Result<ProjectCheckerSession<'a>, ProjectCheckerSessionError> {
        let plan = self.contents.plan(Some(checkpoint));
        plan.checkpoint(0)
            .map_err(ProjectCheckerSessionError::ExecutionStopped)?;
        Ok(ProjectCheckerSession {
            plan,
            file_work: None,
        })
    }
}

impl<'a> ParallelCheckPlan<'a> {
    /// Compute every shared input the parallel workers need, exactly once.
    fn build(
        program: &'a MergedProgram,
        checker_options: &'a CheckerOptions,
        lib_files: &[Arc<LibFile>],
        project_semantic_options: ProjectSemanticOptions,
        project_module_resolution_outcomes: Option<
            Arc<crate::checker::context::ResolvedModuleRequestOutcomeMap>,
        >,
        execution_checkpoint: Option<&'a dyn tsz_common::ExecutionCheckpoint>,
    ) -> Result<Self, tsz_common::ProjectExecutionStop> {
        let execution_scope = execution_checkpoint.map(|checkpoint| ProjectCheckerExecutionScope::new(checkpoint, None));
        let execution_checkpoint = execution_scope
            .as_ref()
            .map(|scope| scope as &dyn tsz_common::ExecutionCheckpoint);
        project_plan_checkpoint(execution_checkpoint, 0)?;
        let mut file_names = Vec::with_capacity(program.files.len());
        for file in &program.files {
            project_plan_checkpoint(execution_checkpoint, 1)?;
            file_names.push(file.file_name.clone());
        }
        let (resolved_module_paths, resolved_modules) =
            if let Some(outcomes) = project_module_resolution_outcomes.as_ref() {
                (
                    unambiguous_project_file_path_map(outcomes),
                    FxHashSet::default(),
                )
            } else {
                crate::checker::module_resolution::build_module_resolution_maps(&file_names)
            };
        project_plan_checkpoint(execution_checkpoint, 0)?;
        let resolved_module_paths = Arc::new(resolved_module_paths);
        let resolved_modules = Arc::new(resolved_modules);

        let library_span = tsz_common::ProjectExecutionSpan::new(
            execution_checkpoint,
            tsz_common::ProjectExecutionPhase::LibraryViews,
            None,
        );
        let should_clone_libs_in_parallel = program.files.len() > 1;
        let checker_lib_files =
            clone_lib_files_for_checker(lib_files, should_clone_libs_in_parallel);

        project_plan_checkpoint(execution_checkpoint, 0)?;
        // Create fresh checker lib contexts from cloned lib files (contains both arena and binder).
        // Wrapped in Arc so that per-file checkers and child delegations share
        // the same Vec with O(1) clone cost (single atomic refcount increment).
        let mut lib_contexts_vec = Vec::with_capacity(checker_lib_files.len());
        for lib in &checker_lib_files {
            project_plan_checkpoint(execution_checkpoint, 1)?;
            lib_contexts_vec.push(LibContext {
                arena: Arc::clone(&lib.arena),
                binder: Arc::clone(&lib.binder),
            });
        }
        let lib_contexts = Arc::new(lib_contexts_vec);

        // Build the lib `file_locals` name index once and share it (Arc) into
        // every per-file checker, so type/value-position identifier resolution
        // can skip the per-identifier `O(num_lib_files)` lib scan for names no
        // lib declares (the common case for a project's own symbols).
        let lib_file_local_names =
            tsz_checker::context::build_lib_file_local_names_with_poll(&lib_contexts, |work| {
                project_plan_checkpoint(execution_checkpoint, work)
            })?;

        library_span.returned();
        let binder_span = tsz_common::ProjectExecutionSpan::new(
            execution_checkpoint,
            tsz_common::ProjectExecutionPhase::BinderViews,
            None,
        );
        // The binder constructor reads the program's existing shared maps.
        // The legacy with_shared wrapper no longer consumes SharedBinderData,
        // so do not allocate a redundant merged augmentation copy here.
        project_plan_checkpoint(execution_checkpoint, 0)?;
        let mut all_binders_vec = Vec::with_capacity(program.files.len());
        for (file_idx, file) in program.files.iter().enumerate() {
            project_plan_checkpoint(execution_checkpoint, 1)?;
            all_binders_vec.push(Arc::new(create_binder_from_bound_file_with_program_owners(
                file, program, file_idx,
            )));
        }
        let all_binders = Arc::new(all_binders_vec);
        binder_span.returned();
        let owner_span = tsz_common::ProjectExecutionSpan::new(
            execution_checkpoint,
            tsz_common::ProjectExecutionPhase::OwnershipIndex,
            None,
        );
        let mut all_arenas_vec = Vec::with_capacity(program.files.len());
        for file in &program.files {
            project_plan_checkpoint(execution_checkpoint, 1)?;
            all_arenas_vec.push(Arc::clone(&file.arena));
        }
        let all_arenas = Arc::new(all_arenas_vec);
        // PERF: Build arena-pointer -> file-index reverse lookup map first (O(F)),
        // then map each symbol to its file index in O(1) per symbol.
        // Total: O(S + F) instead of the previous O(S * F) nested iteration.
        let mut arena_to_file_idx = FxHashMap::default();
        for (idx, arena) in all_arenas.iter().enumerate() {
            project_plan_checkpoint(execution_checkpoint, 1)?;
            arena_to_file_idx.insert(Arc::as_ptr(arena) as usize, idx);
        }
        let mut symbol_file_targets = Vec::with_capacity(program.symbol_arenas.len());
        for (sym_id, arena) in program.symbol_arenas.iter() {
            project_plan_checkpoint(execution_checkpoint, 1)?;
            if let Some(&file_idx) = arena_to_file_idx.get(&(Arc::as_ptr(arena) as usize)) {
                symbol_file_targets.push((*sym_id, file_idx));
            }
        }

        // Pre-compute the symbol->file index as a shared read-only map.
        // Each checker gets an Arc clone (O(1)) instead of O(N) per-checker insertion.
        let mut global_symbol_file_index = FxHashMap::default();
        for (symbol, file_index) in symbol_file_targets {
            project_plan_checkpoint(execution_checkpoint, 1)?;
            global_symbol_file_index.insert(symbol, file_index);
        }
        let global_symbol_file_index = Arc::new(global_symbol_file_index);

        // Pre-compute skeleton-derived declared modules ONCE and share via Arc.
        // Previously this was computed per-file inside the closure, rebuilding the
        // same FxHashSet/Vec on every file (O(N_files * N_modules) total work).
        let shared_declared_modules: Option<Arc<crate::checker::context::GlobalDeclaredModules>> =
            program.skeleton_index.as_ref().map(|skel| {
                let (exact, patterns) = skel.build_declared_module_sets();
                Arc::new(
                    crate::checker::context::GlobalDeclaredModules::from_skeleton(exact, patterns),
                )
            });

        project_plan_checkpoint(execution_checkpoint, 0)?;
        owner_span.returned();
        let indexes_span = tsz_common::ProjectExecutionSpan::new(
            execution_checkpoint,
            tsz_common::ProjectExecutionPhase::SharedCheckerIndexes,
            None,
        );
        let checker_indexes = SealedProjectCheckerIndexes::build(
            Arc::clone(&all_binders),
            Arc::clone(&all_arenas),
            shared_declared_modules.clone(),
        );
        project_plan_checkpoint(execution_checkpoint, 0)?;
        indexes_span.returned();
        // Initialize per-file delegation locks for parallel correctness.
        program
            .definition_store
            .init_file_locks(program.files.len());

        project_plan_checkpoint(execution_checkpoint, 0)?;
        // Create a shared cross-file query cache for multi-file projects.
        // In projects like ts-toolbelt (242 files), the same type evaluations and
        // subtype checks are performed across many files. The shared cache uses
        // DashMap for thread-safe concurrent access and eliminates redundant
        // computation across parallel file checkers.
        let shared_query_cache = if program.files.len() > 1 {
            Some(tsz_solver::construction::SharedQueryCache::new())
        } else {
            None
        };

        project_plan_checkpoint(execution_checkpoint, 0)?;
        let affected_span = tsz_common::ProjectExecutionSpan::new(
            execution_checkpoint,
            tsz_common::ProjectExecutionPhase::AffectedLibraries,
            None,
        );
        let affected_lib_interfaces = affected_lib_interface_names(program, &checker_lib_files);
        project_plan_checkpoint(execution_checkpoint, 0)?;
        let affected_lib_extension_interfaces = if affected_lib_interfaces.is_empty() {
            FxHashSet::default()
        } else {
            affected_lib_extension_interface_names(
                program,
                &checker_lib_files,
                &affected_lib_interfaces,
            )
        };

        project_plan_checkpoint(execution_checkpoint, 0)?;
        affected_span.returned();
        Ok(Self {
            program,
            checker_options,
            project_semantic_options,
            project_module_resolution_outcomes,
            execution_scope,
            views: Arc::new(ProjectCheckerViews {
            checker_indexes,
            resolved_module_paths,
            resolved_modules,
            checker_lib_files,
            lib_contexts,
            lib_file_local_names,
            all_binders,
            all_arenas,
            global_symbol_file_index,
            shared_declared_modules,
            shared_query_cache,
            cross_file_type_params: Default::default(),
            affected_lib_interfaces,
            affected_lib_extension_interfaces,
            }),
        })
    }

    fn execution_control(&self) -> Option<&dyn tsz_common::ExecutionCheckpoint> {
        self.execution_scope
            .as_ref()
            .map(|scope| scope as &dyn tsz_common::ExecutionCheckpoint)
    }

    fn checkpoint(&self, work_units: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
        self.execution_control()
            .map_or(Ok(()), |scope| scope.checkpoint(work_units))
    }

    /// Build a per-worker `QueryCache`, attaching the shared cross-file cache
    /// (L2) when this is a multi-file project.
    ///
    /// Each thread gets its own `RefCell`/`Cell`-backed cache (no atomic
    /// overhead) for memoized `evaluate_type` / `is_subtype_of` calls.
    fn make_query_cache(&self) -> tsz_solver::construction::QueryCache<'_> {
        self.make_query_cache_with(self.execution_control())
    }

    /// Build a query cache whose every checkpoint poll, and therefore every
    /// guard on persistent cache writes, goes through `checkpoint`.
    fn make_query_cache_with<'x>(
        &'x self,
        checkpoint: Option<&'x dyn tsz_common::ExecutionCheckpoint>,
    ) -> tsz_solver::construction::QueryCache<'x> {
        let cache = if let Some(ref shared) = self.shared_query_cache {
            tsz_solver::construction::QueryCache::new_with_shared(
                &self.program.type_interner,
                shared,
            )
        } else {
            tsz_solver::construction::QueryCache::new(&self.program.type_interner)
        };
        // Attach the shared DefinitionStore so generic-call inference can resolve
        // cross-arena declaration identity (issue #14344, `TSZ_XARENA_BASE_DECL`).
        let cache = cache
            .with_project_semantic_options(self.project_semantic_options)
            .with_definition_store(&self.program.definition_store);
        match checkpoint {
            Some(checkpoint) => cache.with_execution_checkpoint(checkpoint),
            None => cache,
        }
    }

    fn prime_module_augmentation_bodies(&self) {
        if self.program.files.is_empty() {
            return;
        }

        let file = &self.program.files[0];
        if self
            .execution_control()
            .is_some_and(|checkpoint| checkpoint.checkpoint(1).is_err())
        {
            return;
        }
        self.with_bound_file_checker_and_types(0, file, |checker, _binder, _file, _types| {
            checker.prime_module_augmentation_bodies();
        });
        if let Some(checkpoint) = self.execution_control() {
            let _ = checkpoint.checkpoint(0);
        }
    }

    /// Build a `FileCheckResult` for a lib file at `lib_idx` with the given
    /// diagnostics. Lib results are indexed after the user files.
    fn lib_file_result(
        &self,
        lib_idx: usize,
        lib_file: &Arc<LibFile>,
        diagnostics: Vec<Diagnostic>,
    ) -> FileCheckResult {
        FileCheckResult {
            file_idx: self.program.files.len() + lib_idx,
            file_name: lib_file.file_name.clone(),
            function_results: Vec::new(),
            diagnostics,
        }
    }

    /// Sort by the diagnostic comparator and remove only identical records.
    /// Distinct missing globals can share position zero and TS2318, while
    /// different member faults can share an anchor and diagnostic code.
    fn sort_and_dedup(diagnostics: &mut Vec<Diagnostic>) {
        diagnostics.sort_by(compare_native_diagnostic_records);
        diagnostics.dedup();
    }

    /// Lend a project-configured checker with all shared program context.
    fn with_bound_file_checker_and_types<Output>(
        &self,
        file_idx: usize,
        file: &BoundFile,
        consume: impl for<'checker> FnOnce(
            &mut CheckerState<'checker>,
            &BinderState,
            &BoundFile,
            &dyn tsz_solver::construction::TypeDatabase,
        ) -> Output,
    ) -> Output {
        self.with_bound_file_checker_and_types_over(
            file_idx,
            file,
            self.execution_control(),
            consume,
        )
    }

    /// Like [`Self::with_bound_file_checker_and_types`], but every checkpoint
    /// poll of the file query goes through `checkpoint` instead of the plan scope.
    fn with_bound_file_checker_and_types_over<Output>(
        &self,
        file_idx: usize,
        file: &BoundFile,
        checkpoint: Option<&dyn tsz_common::ExecutionCheckpoint>,
        consume: impl for<'checker> FnOnce(
            &mut CheckerState<'checker>,
            &BinderState,
            &BoundFile,
            &dyn tsz_solver::construction::TypeDatabase,
        ) -> Output,
    ) -> Output {
        tsz_common::with_ambient_checkpoint(checkpoint, || {
            let query_cache = self.make_query_cache_with(checkpoint);
            let mut checker = self.configured_file_checker(file_idx, file, &query_cache);

            consume(
                &mut checker,
                self.all_binders[file_idx].as_ref(),
                file,
                &query_cache,
            )
        })
    }

    /// Construct one checker from the immutable plan. Both fresh-file and
    /// partition-owned execution install exactly the same project context.
    fn configured_file_checker<'checker>(
        &'checker self,
        file_idx: usize,
        file: &'checker BoundFile,
        query_cache: &'checker tsz_solver::construction::QueryCache<'_>,
    ) -> CheckerState<'checker> {
        let binder = self.all_binders[file_idx].as_ref();
        let mut checker = CheckerState::with_options_and_shared_def_store(
            &file.arena,
            binder,
            query_cache,
            file.file_name.clone(),
            self.checker_options,
            std::sync::Arc::clone(&self.program.definition_store),
        );
        self.checker_indexes.install_into(&mut checker.ctx);

        // Use pre-computed skeleton-derived declared modules (shared via Arc::clone).
        if let Some(ref modules) = self.shared_declared_modules {
            checker
                .ctx
                .set_declared_modules_from_skeleton(Arc::clone(modules));
        }

        // Exact ordered binder views were installed with the sealed indexes.
        checker.ctx.set_current_file_idx(file_idx);
        checker
            .ctx
            .set_resolved_module_paths(Arc::clone(&self.resolved_module_paths));
        checker
            .ctx
            .set_resolved_modules(Arc::clone(&self.resolved_modules));
        checker.ctx.file_is_esm = self
            .program
            .file_is_esm_map
            .get(&checker.ctx.file_name)
            .copied();
        checker.ctx.file_is_esm_map = Some(Arc::clone(&self.program.file_is_esm_map));
        if let Some(outcomes) = self.project_module_resolution_outcomes.as_ref() {
            checker
                .ctx
                .set_project_module_resolution_outcomes(Arc::clone(outcomes));
        }
        checker
            .ctx
            .set_global_symbol_file_index(Arc::clone(&self.global_symbol_file_index));
        checker.ctx.share_owner_symbol_type_results = true;
        checker.ctx.cross_file_type_params_cache = Some(Arc::clone(&self.cross_file_type_params));

        if !self.lib_contexts.is_empty() {
            checker
                .ctx
                .set_lib_contexts_shared(Arc::clone(&self.lib_contexts));
            checker
                .ctx
                .set_lib_file_local_names(self.lib_file_local_names.clone());
            checker
                .ctx
                .set_actual_lib_file_count(self.lib_contexts.len());
        }

        checker
    }

    /// The AST declaration bit belongs to the admitted parsed source. The
    /// policy belongs to resolved compiler options, never a directory/name filter.
    fn checks_file_semantics(&self, file: &BoundFile) -> bool {
        self.checker_options.declaration_diagnostics
            != tsz_common::options::checker::DeclarationDiagnosticPolicy::SkipSemantic
            || !file
                .arena
                .get_source_file_at(file.source_file)
                .is_some_and(|source| source.is_declaration_file)
    }

    fn checks_library_semantics(&self, lib: &LibFile) -> bool {
        self.checker_options.declaration_diagnostics
            != tsz_common::options::checker::DeclarationDiagnosticPolicy::SkipSemantic
            || !lib
                .arena
                .get_source_file_at(lib.root_index)
                .is_some_and(|source| source.is_declaration_file)
    }

    /// Check a single user file, returning its sorted, deduplicated diagnostics.
    fn check_one_file(&self, file_idx: usize, file: &BoundFile) -> FileCheckResult {
        if !self.checks_file_semantics(file) {
            return Self::user_file_result(file_idx, file, Vec::new());
        }
        let diagnostics = self.with_bound_file_checker_and_types(
            file_idx,
            file,
            |checker, _binder, file, _types| {
                checker.check_source_file(file.source_file);
                std::mem::take(&mut checker.ctx.diagnostics)
            },
        );

        Self::user_file_result(file_idx, file, diagnostics)
    }

    fn user_file_result(
        file_idx: usize,
        file: &BoundFile,
        mut diagnostics: Vec<Diagnostic>,
    ) -> FileCheckResult {
        // Sort diagnostics by position for deterministic output within each file.
        diagnostics.sort_by(compare_native_diagnostic_records);

        suppress_parallel_ts2339_cascade_diagnostics(file.arena.as_ref(), &mut diagnostics);

        // Preserve distinct messages, locations, lengths and related evidence
        // even when two diagnostics have the same anchor and code.
        diagnostics.dedup();

        FileCheckResult {
            file_idx,
            file_name: file.file_name.clone(),
            function_results: Vec::new(),
            diagnostics,
        }
    }

    /// Check a single lib file against the merged program's affected interfaces.
    fn check_one_lib(&self, lib_idx: usize, lib_file: &Arc<LibFile>) -> FileCheckResult {
        if !self.checks_library_semantics(lib_file) {
            return self.lib_file_result(lib_idx, lib_file, Vec::new());
        }
        if !lib_file_contains_affected_interface(lib_file.as_ref(), &self.affected_lib_interfaces) {
            return self.lib_file_result(lib_idx, lib_file, Vec::new());
        }

        tsz_common::with_ambient_checkpoint(self.execution_control(), || {
            let query_cache = self.make_query_cache();

            let lib_bound_file = build_lib_bound_file_for_interface_checks(
                self.program,
                lib_file,
                &self.affected_lib_interfaces,
            );
            let mut binder =
                create_binder_from_bound_file(&lib_bound_file, self.program, self.program.files.len());
            // PERF: `build_lib_bound_file_for_interface_checks` always seeds
            // `lib_bound_file.semantic_defs` as empty, so the previous
            // clone-then-overlay collapsed to a deep clone of `program.semantic_defs`
            // and Arc-wrapping the result. With `program.semantic_defs` now
            // `Arc`-shared, the fast path is one atomic refcount bump plus a
            // potential `Arc::make_mut` only when the per-lib map actually
            // contributes entries (currently never).
            if lib_bound_file.semantic_defs.is_empty() {
                binder.semantic_defs = Arc::clone(&self.program.semantic_defs);
            } else {
                let mut composed_semantic_defs = (*self.program.semantic_defs).clone();
                for (sym_id, entry) in lib_bound_file.semantic_defs.iter() {
                    composed_semantic_defs.insert(*sym_id, entry.clone());
                }
                binder.semantic_defs = Arc::new(composed_semantic_defs);
            }

            let mut checker = CheckerState::with_options(
                &lib_bound_file.arena,
                &binder,
                &query_cache,
                lib_bound_file.file_name.clone(),
                self.checker_options,
            );
            self.checker_indexes.install_into(&mut checker.ctx);
            if let Some(ref modules) = self.shared_declared_modules {
                checker
                    .ctx
                    .set_declared_modules_from_skeleton(Arc::clone(modules));
            }
            // Exact ordered binder views were installed with the sealed indexes.
            checker
                .ctx
                .set_resolved_module_paths(Arc::clone(&self.resolved_module_paths));
            checker
                .ctx
                .set_resolved_modules(Arc::clone(&self.resolved_modules));
            checker.ctx.file_is_esm = self
                .program
                .file_is_esm_map
                .get(&checker.ctx.file_name)
                .copied();
            checker.ctx.file_is_esm_map = Some(Arc::clone(&self.program.file_is_esm_map));
            if let Some(outcomes) = self.project_module_resolution_outcomes.as_ref() {
                checker
                    .ctx
                    .set_project_module_resolution_outcomes(Arc::clone(outcomes));
            }
            checker
                .ctx
                .set_global_symbol_file_index(Arc::clone(&self.global_symbol_file_index));

            let other_lib_contexts: Vec<LibContext> = self
                .lib_contexts
                .iter()
                .enumerate()
                .filter(|(idx, _)| *idx != lib_idx)
                .map(|(_, ctx)| ctx.clone())
                .collect();
            checker.ctx.set_lib_contexts(other_lib_contexts);
            checker
                .ctx
                .set_actual_lib_file_count(self.lib_contexts.len());
            checker.prime_boxed_types();

            checker.check_source_file_interfaces_only_filtered_post_merge(
                lib_bound_file.source_file,
                &self.affected_lib_interfaces,
                &self.affected_lib_extension_interfaces,
            );

            let mut diagnostics = std::mem::take(&mut checker.ctx.diagnostics);
            Self::sort_and_dedup(&mut diagnostics);

            self.lib_file_result(lib_idx, lib_file, diagnostics)
        })
    }

    fn make_lib_baseline_query_cache(&self) -> tsz_solver::construction::QueryCache<'_> {
        // Baseline remains isolated from the augmented/shared L2 cache, but
        // must observe the same execution budget as the augmented lib run.
        let query_cache = tsz_solver::construction::QueryCache::new(&self.program.type_interner)
            .with_project_semantic_options(self.project_semantic_options)
            .with_definition_store(&self.program.definition_store);
        match self.execution_control() {
            Some(checkpoint) => query_cache.with_execution_checkpoint(checkpoint),
            None => query_cache,
        }
    }

    /// Check a lib file in isolation to capture its pre-existing diagnostics.
    ///
    /// The resulting diagnostics form the baseline that [`check_one_lib`] output
    /// is filtered against, so only diagnostics introduced by user augmentations
    /// survive.
    fn check_one_lib_baseline(&self, lib_idx: usize, lib_file: &Arc<LibFile>) -> FileCheckResult {
        if !self.checks_library_semantics(lib_file) {
            return self.lib_file_result(lib_idx, lib_file, Vec::new());
        }

        if !lib_file_contains_affected_interface(lib_file.as_ref(), &self.affected_lib_interfaces) {
            return self.lib_file_result(lib_idx, lib_file, Vec::new());
        }

        tsz_common::with_ambient_checkpoint(self.execution_control(), || {
            let query_cache = self.make_lib_baseline_query_cache();

            let mut checker = CheckerState::with_options(
                &lib_file.arena,
                lib_file.binder.as_ref(),
                &query_cache,
                lib_file.file_name.clone(),
                self.checker_options,
            );
            checker.ctx.file_is_esm = self
                .program
                .file_is_esm_map
                .get(&checker.ctx.file_name)
                .copied();
            checker.ctx.file_is_esm_map = Some(Arc::clone(&self.program.file_is_esm_map));
            let other_lib_contexts: Vec<LibContext> = self
                .lib_contexts
                .iter()
                .enumerate()
                .filter(|(idx, _)| *idx != lib_idx)
                .map(|(_, ctx)| ctx.clone())
                .collect();
            checker.ctx.set_lib_contexts(other_lib_contexts);
            checker
                .ctx
                .set_actual_lib_file_count(self.lib_contexts.len());
            checker.prime_boxed_types();
            checker.check_source_file_interfaces_only_filtered_post_merge(
                lib_file.root_index,
                &self.affected_lib_interfaces,
                &self.affected_lib_extension_interfaces,
            );

            let mut diagnostics = std::mem::take(&mut checker.ctx.diagnostics);
            Self::sort_and_dedup(&mut diagnostics);

            self.lib_file_result(lib_idx, lib_file, diagnostics)
        })
    }

    /// Run the per-user-file checks, sequentially for a single file and in
    /// parallel (preserving `file_idx` order) otherwise.
    fn run_file_checks(&self) -> Vec<FileCheckResult> {
        // Single-file optimization: skip Rayon overhead when there's only one file.
        // For multi-file projects, use parallel iteration via Rayon's work-stealing
        // scheduler. `par_iter().enumerate()` preserves input ordering (file_idx) so
        // results are deterministic regardless of which thread completes first.
        if self.program.files.len() <= 1 {
            self.program
                .files
                .iter()
                .enumerate()
                .map(|(file_idx, file)| self.check_one_file(file_idx, file))
                .collect()
        } else {
            maybe_parallel_iter!(self.program.files)
                .enumerate()
                .map(|(file_idx, file)| self.check_one_file(file_idx, file))
                .collect()
        }
    }

    /// Append the per-lib-file check results to `file_results`.
    ///
    /// When no user code touches a lib interface, the lib results are empty
    /// placeholders. Otherwise each affected lib is checked twice — once for the
    /// baseline and once with augmentations — and only the augmentation-induced
    /// diagnostics are retained.
    fn run_lib_checks(&self, file_results: &mut Vec<FileCheckResult>) {
        if self.affected_lib_interfaces.is_empty() {
            file_results.extend(
                self.checker_lib_files
                    .iter()
                    .enumerate()
                    .map(|(lib_idx, lib_file)| self.lib_file_result(lib_idx, lib_file, Vec::new())),
            );
        } else {
            let baseline_lib_diagnostics: FxHashSet<(String, u32, u32, String)> = self
                .checker_lib_files
                .iter()
                .enumerate()
                .flat_map(|(lib_idx, lib_file)| {
                    let file_result = self.check_one_lib_baseline(lib_idx, lib_file);
                    let file_name = file_result.file_name.clone();
                    file_result
                        .diagnostics
                        .into_iter()
                        .map(move |diag| lib_diagnostic_fingerprint(&file_name, &diag))
                })
                .collect();

            file_results.extend(self.checker_lib_files.iter().enumerate().map(
                |(lib_idx, lib_file)| {
                    let mut file_result = self.check_one_lib(lib_idx, lib_file);
                    let file_name = file_result.file_name.clone();
                    file_result.diagnostics.retain(|diag| {
                        !baseline_lib_diagnostics
                            .contains(&lib_diagnostic_fingerprint(&file_name, diag))
                    });
                    file_result
                },
            ));
        }
    }

    /// Apply cross-file augmentation diagnostics and fold per-file results into
    /// the final [`CheckResult`].
    fn aggregate(&self, file_results: Vec<FileCheckResult>) -> CheckResult {
        self.aggregate_with_poll(file_results, |_| Ok::<(), std::convert::Infallible>(()))
            .unwrap_or_else(|never| match never {})
    }

    // Whole augmentation/suppression passes are cooperative boundaries. Their
    // existing internal walks are not force-interruptible; no partial result is
    // returned when a stop is observed after a pass.
    fn aggregate_with_poll<E>(
        &self,
        mut file_results: Vec<FileCheckResult>,
        mut poll: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<CheckResult, E> {
        poll(0)?;
        add_reexported_module_augmentation_enum_conflict_diagnostics(
            self.program,
            self.resolved_module_paths.as_ref(),
            &mut file_results,
        );
        poll(0)?;
        suppress_parallel_import_shadowing_namespace_type_diagnostics(
            self.program,
            self.resolved_module_paths.as_ref(),
            &mut file_results,
        );
        poll(0)?;
        add_parallel_global_augmentation_member_conflict_diagnostics(
            self.program,
            &mut file_results,
        );

        poll(0)?;
        // Cross-file augmentation passes can append declaration diagnostics.
        // Keep their application effects, but enforce the same source policy on
        // final declaration rows without removing any member or parse result.
        let mut diagnostic_count = 0;
        for result in &mut file_results {
            poll(1)?;
            let check = if result.file_idx < self.program.files.len() {
                self.checks_file_semantics(&self.program.files[result.file_idx])
            } else {
                self.checker_lib_files
                    .get(result.file_idx - self.program.files.len())
                    .is_none_or(|lib| self.checks_library_semantics(lib))
            };
            if !check {
                result.diagnostics.clear();
            }
            diagnostic_count += result.diagnostics.len();
        }
        poll(0)?;

        Ok(CheckResult {
            file_results,
            function_count: 0,
            diagnostic_count,
        })
    }
}

#[cfg(test)]
mod project_module_resolution_tests {
    use super::*;
    use crate::checker::context::{
        ResolutionModeOverride, ResolutionRequestKind, ResolvedModuleRequestOutcome,
        ResolvedModuleRequestOutcomeMap,
    };
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    #[test]
    fn reference_query_admission_rejects_a_forged_incomplete_program_map() {
        let mut program = compile_files_with_libs(
            vec![
                (
                    "src/main.ts".into(),
                    "/// <reference path='./present.d.ts' />\nexport {};".into(),
                ),
                ("src/present.d.ts".into(), "interface Present {}".into()),
            ],
            &[],
        );
        program.project_module_resolution_outcomes =
            Some(Arc::new(ResolvedModuleRequestOutcomeMap::default()));
        let cancelled = AtomicBool::new(false);
        let budget = tsz_common::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancelled,
            10_000,
        );
        let options = CheckerOptions::default();
        assert!(matches!(
            check_files_parallel(&program, &options, &[]),
            Err(ProjectModuleResolutionError::InvalidReference { .. })
        ));
        assert!(matches!(
            check_files_parallel_with_project_semantic_options(
                &program,
                &options,
                &[],
                ProjectSemanticOptions::structural()
            ),
            Err(ProjectModuleResolutionError::InvalidReference { .. })
        ));
        assert!(matches!(
            check_functions_parallel(&program),
            Err(ProjectModuleResolutionError::InvalidReference { .. })
        ));
        assert!(matches!(
            check_functions_with_stats(&program),
            Err(ProjectModuleResolutionError::InvalidReference { .. })
        ));
        assert!(matches!(
            ProjectCheckerSession::new(
                &program,
                &options,
                &[],
                ProjectSemanticOptions::structural(),
                &budget
            ),
            Err(ProjectCheckerSessionError::ModuleResolution(
                ProjectModuleResolutionError::InvalidReference { .. }
            ))
        ));
        assert!(matches!(
            PreparedProjectCheckerImage::new(
                program,
                options,
                &[],
                ProjectSemanticOptions::structural(),
                Some(&budget)
            ),
            Err(ProjectCheckerSessionError::ModuleResolution(
                ProjectModuleResolutionError::InvalidReference { .. }
            ))
        ));
    }

    #[test]
    fn legacy_outcome_attachment_defers_reference_coverage_until_options_are_known() {
        let mut program = compile_files_with_libs(
            vec![(
                "src/main.ts".into(),
                "/// <reference types='unused-types' />\nexport {};".into(),
            )],
            &[],
        );
        program.set_project_module_resolutions(&[]).unwrap();
        let mut options = CheckerOptions::default();
        options.no_resolve = true;
        assert!(check_files_parallel(&program, &options, &[]).is_ok());
        options.no_resolve = false;
        assert!(matches!(
            check_files_parallel(&program, &options, &[]),
            Err(ProjectModuleResolutionError::InvalidReference { .. })
        ));
        let cancelled = AtomicBool::new(false);
        let budget = tsz_common::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancelled,
            10_000,
        );
        program
            .set_project_module_resolutions_with_execution_checkpoint(&[], &budget)
            .unwrap();
        options.no_resolve = true;
        assert!(
            ProjectCheckerSession::new(
                &program,
                &options,
                &[],
                ProjectSemanticOptions::structural(),
                &budget
            )
            .is_ok()
        );
        program.project_module_resolution_outcomes = None;
        assert!(
            check_files_parallel(&program, &CheckerOptions::default(), &[]).is_ok(),
            "standalone checking remains the legacy path"
        );
    }
    struct ArmedExecutionStop {
        armed: AtomicBool,
        remaining: AtomicU64,
        stopped: AtomicBool,
    }

    impl ArmedExecutionStop {
        fn new() -> Self {
            Self {
                armed: AtomicBool::new(false),
                remaining: AtomicU64::new(u64::MAX),
                stopped: AtomicBool::new(false),
            }
        }

        fn arm_after(&self, successful_units: u64) {
            self.remaining.store(successful_units, Ordering::Release);
            self.armed.store(true, Ordering::Release);
        }
    }

    impl tsz_common::ExecutionCheckpoint for ArmedExecutionStop {
        fn checkpoint(
            &self,
            work_units: u64,
        ) -> Result<(), tsz_common::ProjectExecutionStop> {
            use tsz_common::ProjectExecutionStop;
            if self.stopped.load(Ordering::Acquire) {
                return Err(ProjectExecutionStop::WorkBudgetExhausted);
            }
            if !self.armed.load(Ordering::Acquire) {
                return Ok(());
            }
            if self
                .remaining
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(work_units)
                })
                .is_err()
            {
                self.stopped.store(true, Ordering::Release);
                return Err(ProjectExecutionStop::WorkBudgetExhausted);
            }
            Ok(())
        }
    }

    #[test]
    fn project_checker_session_resolves_imported_members_in_full_program_context() {
        let mut program = compile_files_with_libs(
            vec![
                (
                    "src/app.service.ts".to_owned(),
                    "export class AppService { value = 42; }".to_owned(),
                ),
                (
                    "src/app.controller.ts".to_owned(),
                    "import { AppService } from './app.service'; const appService = new AppService(); export const result = appService.value;".to_owned(),
                ),
            ],
            &[],
        );
        let importer_index = program
            .files
            .iter()
            .position(|file| file.file_name == "src/app.controller.ts")
            .unwrap();
        program
            .set_project_module_resolutions(&[ProjectModuleResolution {
                importer_path: "src/app.controller.ts".to_owned(),
                specifier: "./app.service".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::File {
                    path: "src/app.service.ts".to_owned(),
                },
            }])
            .unwrap();

        let file = &program.files[importer_index];
        let access_index = (0..file.arena.len())
            .map(|index| NodeIndex(index as u32))
            .find(|&index| {
                file.arena
                    .get(index)
                    .is_some_and(|node| node.kind == syntax_kind_ext::PROPERTY_ACCESS_EXPRESSION)
            })
            .expect("fixture should contain the imported member access");
        let checker_options = CheckerOptions::default();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let execution_budget = tsz_common::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancelled,
            10_000,
        );
        let session = ProjectCheckerSession::new(
            &program,
            &checker_options,
            &[],
            ProjectSemanticOptions::structural(),
            &execution_budget,
        )
        .unwrap();
        let actual = session
            .with_file_checker(importer_index, |checker, _binder, _file| {
                checker.get_type_of_node(access_index)
            })
            .unwrap();

        assert_eq!(actual, TypeId::NUMBER);
    }

    #[test]
    fn exact_project_requests_dominate_legacy_success_for_every_kind_mode_and_outcome() {
        let program = compile_files_with_libs(
            vec![
                ("src/main.ts".to_owned(), "export {};".to_owned()),
                (
                    "src/dep.ts".to_owned(),
                    "export const value = 1;".to_owned(),
                ),
                (
                    "src/wrong.ts".to_owned(),
                    "export const value = 'wrong';".to_owned(),
                ),
            ],
            &[],
        );
        let source = program
            .files
            .iter()
            .position(|file| file.file_name == "src/main.ts")
            .unwrap();
        let target = program
            .files
            .iter()
            .position(|file| file.file_name == "src/dep.ts")
            .unwrap();
        let wrong = program
            .files
            .iter()
            .position(|file| file.file_name == "src/wrong.ts")
            .unwrap();
        let kinds = [
            ProjectModuleRequestKind::EsmImport,
            ProjectModuleRequestKind::EsmReExport,
            ProjectModuleRequestKind::DynamicImport,
            ProjectModuleRequestKind::CjsRequire,
        ];
        let checker_kinds = [
            ResolutionRequestKind::EsmImport,
            ResolutionRequestKind::EsmReExport,
            ResolutionRequestKind::DynamicImport,
            ResolutionRequestKind::CjsRequire,
        ];
        let modes = [
            None,
            Some(ResolutionModeOverride::Import),
            Some(ResolutionModeOverride::Require),
        ];
        let options = CheckerOptions::default();
        for (kind, checker_kind) in kinds.iter().zip(checker_kinds) {
            for mode in modes {
                for outcome in [
                    Some(ProjectModuleResolutionTarget::File {
                        path: "src/dep.ts".to_owned(),
                    }),
                    Some(ProjectModuleResolutionTarget::External {
                        identity: "admitted-external:dep".to_owned(),
                    }),
                    Some(ProjectModuleResolutionTarget::Unresolved),
                    None,
                ] {
                    let is_file =
                        matches!(outcome, Some(ProjectModuleResolutionTarget::File { .. }));
                    let is_external = matches!(
                        outcome,
                        Some(ProjectModuleResolutionTarget::External { .. })
                    );
                    let resolutions = outcome
                        .as_ref()
                        .map(|outcome| ProjectModuleResolution {
                            importer_path: "src/main.ts".to_owned(),
                            specifier: "./dep".to_owned(),
                            request_kind: kind.clone(),
                            resolution_mode: mode,
                            target: outcome.clone(),
                        })
                        .into_iter()
                        .collect::<Vec<_>>();
                    let plan = ParallelCheckPlan::build(
                        &program,
                        &options,
                        &[],
                        ProjectSemanticOptions::structural(),
                        Some(Arc::new(
                            build_project_module_resolution_outcomes(&program, &resolutions)
                                .unwrap(),
                        )),
                        None,
                    )
                    .unwrap();
                    for legacy_success in [false, true] {
                        plan.with_bound_file_checker_and_types(source, &program.files[source], |checker, _, _, _| {
                            checker.ctx.set_resolved_modules(if legacy_success {
                                FxHashSet::from_iter(["./dep".to_owned()])
                            } else { FxHashSet::default() });
                            checker.ctx.set_resolved_module_paths(Arc::new(FxHashMap::from_iter([
                                ((source, "./dep".to_owned()), wrong),
                            ])));
                            assert_eq!(checker.ctx.module_resolved_for_request("./dep", mode, checker_kind), is_file || is_external);
                            assert_eq!(checker.ctx.resolve_import_target_from_file_for_request(source, "./dep", mode, checker_kind), is_file.then_some(target));
                            assert_eq!(checker.ctx.module_resolved_without_program_file_for_request("./dep", mode, checker_kind), is_external);
                            if outcome.is_some() {
                                assert_eq!(checker.ctx.resolution_mode_for_project_request("./dep", checker_kind, mode), mode);
                            }
                            for explicit in [ResolutionModeOverride::Import, ResolutionModeOverride::Require] {
                                assert_eq!(checker.ctx.resolution_mode_for_project_request("./dep", checker_kind, Some(explicit)), Some(explicit), "explicit override cannot borrow an authoritative None key");
                            }
                            for absent_kind in checker_kinds {
                                if absent_kind != checker_kind || mode.is_some() || outcome.is_none() {
                                    assert_eq!(checker.ctx.resolution_mode_for_project_request("./dep", absent_kind, None), checker.ctx.resolution_mode_for_request(absent_kind, None), "missing None key cannot borrow another kind or mode");
                                }
                            }
                            for other_kind in checker_kinds {
                                for other_mode in modes {
                                    if other_kind != checker_kind || other_mode != mode {
                                        assert!(!checker.ctx.module_resolved_for_request("./dep", other_mode, other_kind));
                                        assert_eq!(checker.ctx.resolve_import_target_from_file_for_request(source, "./dep", other_mode, other_kind), None);
                                        assert!(!checker.ctx.module_resolved_without_program_file_for_request("./dep", other_mode, other_kind));
                                    }
                                }
                            }
                        });
                    }
                }
            }
        }
    }

    #[test]
    fn stopped_lazy_alias_walk_does_not_poison_a_fresh_project_session() {
        let mut source = String::from("type Alias0 = string;\n");
        for index in 1..=48 {
            source.push_str(&format!("type Alias{index} = Alias{};\n", index - 1));
        }
        source.push_str("export const value: Alias48 = 'ok';\n");
        let mut program = compile_files_with_libs(vec![("src/main.ts".to_owned(), source)], &[]);
        program.set_project_module_resolutions(&[]).unwrap();
        let file_index = program
            .files
            .iter()
            .position(|file| file.file_name == "src/main.ts")
            .unwrap();
        let checker_options = CheckerOptions::default();
        let stop = ArmedExecutionStop::new();
        let stopped_session = ProjectCheckerSession::new(
            &program,
            &checker_options,
            &[],
            ProjectSemanticOptions::structural(),
            &stop,
        )
        .unwrap();

        let stopped = stopped_session.with_file_checker(file_index, |checker, binder, _file| {
            let alias = binder.file_locals.get("Alias48").unwrap();
            stop.arm_after(5);
            let alias_type = checker.get_type_of_symbol(alias);
            checker.resolve_lazy_type(alias_type)
        });
        assert!(matches!(
            stopped,
            Err(ProjectCheckerSessionError::ExecutionStopped(
                tsz_common::ProjectExecutionStop::WorkBudgetExhausted
            ))
        ));
        assert!(
            stop.stopped.load(Ordering::Acquire),
            "the real alias walk must exhaust the armed work budget"
        );

        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let fresh_budget = tsz_common::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancelled,
            10_000,
        );
        let fresh_session = ProjectCheckerSession::new(
            &program,
            &checker_options,
            &[],
            ProjectSemanticOptions::structural(),
            &fresh_budget,
        )
        .unwrap();
        let actual = fresh_session
            .with_file_checker(file_index, |checker, binder, _file| {
                let alias = binder.file_locals.get("Alias48").unwrap();
                let alias_type = checker.get_type_of_symbol(alias);
                checker.resolve_lazy_type(alias_type)
            })
            .unwrap();
        assert_eq!(actual, TypeId::STRING);
    }

    #[test]
    fn explicit_project_resolution_preserves_outcomes_and_disables_filename_guessing() {
        let mut program = compile_files_with_libs(
            vec![
                (
                    "src/main.ts".to_owned(),
                    "import { value } from './dep'; export const result = value;".to_owned(),
                ),
                (
                    "src/dep.ts".to_owned(),
                    "export const value = 1;".to_owned(),
                ),
            ],
            &[],
        );
        let importer_idx = program
            .files
            .iter()
            .position(|file| file.file_name == "src/main.ts")
            .unwrap();
        let target_idx = program
            .files
            .iter()
            .position(|file| file.file_name == "src/dep.ts")
            .unwrap();
        let binders: Arc<Vec<Arc<BinderState>>> = Arc::new(
            program
                .files
                .iter()
                .enumerate()
                .map(|(file_idx, file)| {
                    Arc::new(create_binder_from_bound_file(file, &program, file_idx))
                })
                .collect(),
        );
        let arenas = Arc::new(
            program
                .files
                .iter()
                .map(|file| Arc::clone(&file.arena))
                .collect::<Vec<_>>(),
        );
        let mut checker = CheckerState::new(
            &program.files[importer_idx].arena,
            &binders[importer_idx],
            &program.type_interner,
            "src/main.ts".to_owned(),
            CheckerOptions::default(),
        );
        checker.ctx.set_all_arenas(Arc::clone(&arenas));
        checker.ctx.set_all_binders(Arc::clone(&binders));
        checker.ctx.set_current_file_idx(importer_idx);

        assert_eq!(
            checker
                .ctx
                .resolve_import_target_from_file(importer_idx, "./dep"),
            Some(target_idx),
            "the standalone checker fixture should demonstrate its filename fallback"
        );
        drop(checker);

        program
            .set_project_module_resolutions(&[ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "./dep".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::Unresolved,
            }])
            .unwrap();
        let mut checker = CheckerState::new(
            &program.files[importer_idx].arena,
            &binders[importer_idx],
            &program.type_interner,
            "src/main.ts".to_owned(),
            CheckerOptions::default(),
        );
        checker.ctx.set_all_arenas(Arc::clone(&arenas));
        checker.ctx.set_all_binders(Arc::clone(&binders));
        checker.ctx.set_current_file_idx(importer_idx);
        let outcomes = Arc::clone(program.project_module_resolution_outcomes.as_ref().unwrap());
        checker.ctx.set_project_module_resolution_outcomes(outcomes);
        assert_eq!(
            checker
                .ctx
                .resolve_import_target_from_file(importer_idx, "./dep"),
            None,
            "an explicit unresolved result must not guess the same-directory file"
        );
        assert!(
            !checker
                .ctx
                .module_resolved_without_program_file_for_request(
                    "./dep",
                    None,
                    ResolutionRequestKind::EsmImport,
                )
        );

        let mut file_outcomes = ResolvedModuleRequestOutcomeMap::default();
        file_outcomes.insert(
            (
                importer_idx,
                "./dep".to_owned(),
                None,
                ResolutionRequestKind::EsmImport,
            ),
            ResolvedModuleRequestOutcome::File(target_idx),
        );
        checker
            .ctx
            .set_project_module_resolution_outcomes(Arc::new(file_outcomes));
        assert_eq!(
            checker.ctx.resolve_import_target_from_file_for_request(
                importer_idx,
                "./dep",
                None,
                ResolutionRequestKind::EsmImport,
            ),
            Some(target_idx),
            "the exact compiler-owned file target must be used"
        );

        let mut exact_outcomes = ResolvedModuleRequestOutcomeMap::default();
        exact_outcomes.insert(
            (
                importer_idx,
                "./dep".to_owned(),
                Some(ResolutionModeOverride::Import),
                ResolutionRequestKind::DynamicImport,
            ),
            ResolvedModuleRequestOutcome::External {
                identity: "workspace-ambient:./dep".to_owned(),
            },
        );
        checker
            .ctx
            .set_project_module_resolution_outcomes(Arc::new(exact_outcomes));
        assert!(
            checker
                .ctx
                .module_resolved_without_program_file_for_request(
                    "./dep",
                    Some(ResolutionModeOverride::Import),
                    ResolutionRequestKind::DynamicImport,
                )
        );
        assert!(
            !checker
                .ctx
                .module_resolved_without_program_file_for_request(
                    "./dep",
                    None,
                    ResolutionRequestKind::EsmImport,
                )
        );
        assert_eq!(
            checker.ctx.resolve_import_target_from_file_for_request(
                importer_idx,
                "./dep",
                Some(ResolutionModeOverride::Import),
                ResolutionRequestKind::DynamicImport,
            ),
            None,
            "external is not an in-program file target"
        );
    }

    #[test]
    fn explicit_project_resolution_rejects_unsupported_and_conflicting_requests() {
        let program = compile_files_with_libs(
            vec![("src/main.ts".to_owned(), "export {};".to_owned())],
            &[],
        );
        let unsupported = ProjectModuleResolution {
            importer_path: "src/main.ts".to_owned(),
            specifier: "pkg".to_owned(),
            request_kind: ProjectModuleRequestKind::Unsupported {
                syntax_kind: "ImportType".to_owned(),
            },
            resolution_mode: None,
            target: ProjectModuleResolutionTarget::Unresolved,
        };
        assert!(matches!(
            build_project_module_resolution_outcomes(&program, &[unsupported.clone()]),
            Err(ProjectModuleResolutionError::UnsupportedRequestKind { .. })
        ));
        assert!(matches!(
            build_project_module_resolution_outcomes(&program, &[unsupported.clone(), unsupported]),
            Err(ProjectModuleResolutionError::UnsupportedRequestKind { .. })
        ));

        let exact = ProjectModuleResolution {
            importer_path: "src/main.ts".to_owned(),
            specifier: "pkg".to_owned(),
            request_kind: ProjectModuleRequestKind::EsmImport,
            resolution_mode: None,
            target: ProjectModuleResolutionTarget::Unresolved,
        };
        assert!(matches!(
            build_project_module_resolution_outcomes(&program, &[exact.clone(), exact]),
            Err(ProjectModuleResolutionError::DuplicateRequest { .. })
        ));
    }
}

/// Pre-computed data shared across all file binders in a parallel check.
///
/// These are computed ONCE from the program's files and shared via Arc,
/// eliminating `O(N_files^2)` redundant iteration in `create_binder_from_bound_file()`.
pub struct SharedBinderData {
    /// Merged module augmentations from all files.
    pub merged_module_augmentations:
        rustc_hash::FxHashMap<String, Vec<crate::binder::ModuleAugmentation>>,
    /// Merged augmentation target modules from all files.
    pub merged_augmentation_target_modules: rustc_hash::FxHashMap<crate::binder::SymbolId, String>,
    /// Merged global augmentations from all files.
    pub merged_global_augmentations:
        rustc_hash::FxHashMap<String, Vec<crate::binder::GlobalAugmentation>>,
}

impl SharedBinderData {
    /// Build shared binder data from all files in one pass.
    pub fn from_program(files: &[BoundFile]) -> Self {
        let module_augmentation_keys = files
            .iter()
            .map(|file| file.module_augmentations.len())
            .sum();
        let augmentation_target_count = files
            .iter()
            .map(|file| file.augmentation_target_modules.len())
            .sum();
        let global_augmentation_keys = files
            .iter()
            .map(|file| file.global_augmentations.len())
            .sum();

        let mut merged_module_augmentations = rustc_hash::FxHashMap::with_capacity_and_hasher(
            module_augmentation_keys,
            Default::default(),
        );
        let mut merged_augmentation_target_modules =
            rustc_hash::FxHashMap::with_capacity_and_hasher(
                augmentation_target_count,
                Default::default(),
            );
        let mut merged_global_augmentations = rustc_hash::FxHashMap::with_capacity_and_hasher(
            global_augmentation_keys,
            Default::default(),
        );

        for file in files {
            for (spec, augs) in file.module_augmentations.iter() {
                merged_module_augmentations
                    .entry(spec.clone())
                    .or_insert_with(|| Vec::with_capacity(augs.len()))
                    .extend(augs.iter().map(|aug| {
                        crate::binder::ModuleAugmentation::with_arena(
                            aug.name.clone(),
                            aug.node,
                            Arc::clone(&file.arena),
                        )
                    }));
            }
            for (&sym_id, module_spec) in file.augmentation_target_modules.iter() {
                merged_augmentation_target_modules.insert(sym_id, module_spec.clone());
            }
            for (name, decls) in file.global_augmentations.iter() {
                merged_global_augmentations
                    .entry(name.clone())
                    .or_insert_with(|| Vec::with_capacity(decls.len()))
                    .extend(decls.iter().map(|aug| {
                        crate::binder::GlobalAugmentation::with_arena(
                            aug.node,
                            Arc::clone(&file.arena),
                            aug.flags,
                        )
                    }));
            }
        }

        Self {
            merged_module_augmentations,
            merged_augmentation_target_modules,
            merged_global_augmentations,
        }
    }
}

/// Create a `BinderState` from a `BoundFile` for type checking.
///
/// This path is retained for tsz-core callers that want the legacy per-file
/// subset of `declaration_arenas` (only non-local, non-lib-originated entries,
/// as captured in `BoundFile.declaration_arenas`). The CLI driver uses its own
/// path (`create_binder_from_bound_file_with_augmentations`) which shares the
/// program-wide map via `Arc::clone` — see the perf follow-up doc §3.2.
pub fn create_binder_from_bound_file(
    file: &BoundFile,
    program: &MergedProgram,
    file_idx: usize,
) -> BinderState {
    create_binder_from_bound_file_with_owners(file, program, file_idx, false)
}

/// Reconstruct a project checker binder with the complete merged declaration ownership maps.
///
/// `BoundFile` maps intentionally omit library-originated declarations, so using them together
/// with merged program globals can make a library declaration fall back to a same-index node in
/// the current source file. Project-wide checking must retain the merged maps, including every
/// declaration arena for merged standard-library interfaces such as augmented `Error`.
pub fn create_binder_from_bound_file_with_program_owners(
    file: &BoundFile,
    program: &MergedProgram,
    file_idx: usize,
) -> BinderState {
    create_binder_from_bound_file_with_owners(file, program, file_idx, true)
}

fn create_binder_from_bound_file_with_owners(
    file: &BoundFile,
    program: &MergedProgram,
    file_idx: usize,
    use_program_owners: bool,
) -> BinderState {
    let (declaration_arenas, sym_to_decl_indices, symbol_arenas) = if use_program_owners {
        (
            Arc::clone(&program.declaration_arenas),
            Arc::clone(&program.sym_to_decl_indices),
            Arc::clone(&program.symbol_arenas),
        )
    } else {
        (
            Arc::clone(&file.declaration_arenas),
            Arc::clone(&file.sym_to_decl_indices),
            Arc::clone(&file.symbol_arenas),
        )
    };

    // Merge per-file locals with program globals via the shared helper,
    // which short-circuits to an O(1) `Arc::clone` when one side is empty.
    let file_locals = program.build_merged_file_locals(file_idx);

    let mut binder = BinderState::from_bound_state_with_scopes_and_augmentations(
        BinderOptions::default(),
        program.symbols.clone(),
        file_locals,
        // Arc::clone is O(1); cross-file lookup binders share the per-file
        // map by reference instead of deep-cloning it.
        Arc::clone(&file.node_symbols),
        BinderStateScopeInputs {
            scopes: file.scopes.clone(),
            node_scope_ids: file.node_scope_ids.clone(),
            global_augmentations: Arc::clone(&file.global_augmentations),
            module_augmentations: Arc::clone(&file.module_augmentations),
            augmentation_target_modules: Arc::clone(&file.augmentation_target_modules),
            module_exports: program.module_exports.clone(),
            module_declaration_exports_publicly: file.module_declaration_exports_publicly.clone(),
            reexports: program.reexports.clone(),
            wildcard_reexports: program.wildcard_reexports.clone(),
            symbol_arenas,
            declaration_arenas,
            sym_to_decl_indices,
            cross_file_node_symbols: Arc::clone(&program.cross_file_node_symbols),
            shorthand_ambient_modules: program.shorthand_ambient_modules.clone(),
            flow_nodes: file.flow_nodes.clone(),
            // Arc::clone is O(1); cross-file lookup binders share the per-file
            // node_flow map by reference instead of deep-cloning it.
            node_flow: Arc::clone(&file.node_flow),
            switch_clause_to_switch: file.switch_clause_to_switch.clone(),
            expando_properties: file.expando_properties.clone(),
            alias_partners: program.alias_partners.clone(),
        },
    );

    binder.is_external_module = file.is_external_module;
    binder.file_features = file.file_features;
    // file_locals includes program globals, so its exact library origins must
    // come from the same complete program, not the compacted per-file subset.
    binder.lib_symbol_reverse_remap = Arc::clone(&program.lib_symbol_reverse_remap);
    binder.lib_binders = program.lib_binders.clone();
    binder.lib_symbol_ids = program.lib_symbol_ids.clone();
    binder.lib_type_namespace = Arc::new(program.build_lib_type_namespace(file_idx));

    // Compose semantic_defs: start with the global map (cross-file + lib entries)
    // then overlay the file's own entries. Per-file entries take precedence for
    // symbols declared in this file, ensuring file-scoped identity is authoritative.
    //
    // PERF: When the shared DefinitionStore is fully populated (parallel path),
    // semantic_defs are never read by the checker (warm_local_caches_from_shared_store
    // and resolve_cross_batch_heritage both skip when fully_populated=true).
    // Skip the expensive clone+overlay to avoid O(files * total_defs) work.
    if !program.definition_store.is_fully_populated() {
        if file.semantic_defs.is_empty() {
            binder.semantic_defs = Arc::clone(&program.semantic_defs);
        } else {
            let mut composed_semantic_defs = (*program.semantic_defs).clone();
            for (sym_id, entry) in file.semantic_defs.iter() {
                composed_semantic_defs.insert(*sym_id, entry.clone());
            }
            binder.semantic_defs = Arc::new(composed_semantic_defs);
        }
    }
    if !binder.scopes.is_empty() {
        binder.current_scope_id = crate::binder::ScopeId(0);
    }

    binder.declared_modules = program.declared_modules.clone();

    // Mark lib symbols as merged since the MergedProgram's symbol arena
    // contains all remapped lib symbols with unique global IDs.
    // This enables the fast path in get_symbol() that avoids cross-binder lookups.
    binder.set_lib_symbols_merged(true);

    binder
}

/// Create a `BinderState` from a `BoundFile` using pre-computed shared augmentation data.
///
/// This avoids the `O(N_files)` augmentation merge per file by reusing data computed once
/// via `SharedBinderData::from_program`. For ts-toolbelt (242 files), this eliminates
/// ~242 * 242 = 58,564 redundant augmentation iterations.
pub fn create_binder_from_bound_file_with_shared(
    file: &BoundFile,
    program: &MergedProgram,
    file_idx: usize,
    _shared: &SharedBinderData,
) -> BinderState {
    // Keep the legacy per-file subset behavior here (see `create_binder_from_bound_file`):
    // these paths are used by `check_files_parallel` and tests that expect the
    // binder's `declaration_arenas` to exclude lib-originated symbols.
    let declaration_arenas = Arc::clone(&file.declaration_arenas);
    let sym_to_decl_indices = Arc::clone(&file.sym_to_decl_indices);
    let symbol_arenas = Arc::clone(&file.symbol_arenas);

    // Merge per-file locals with program globals via the shared helper,
    // which short-circuits to an O(1) `Arc::clone` when one side is empty.
    let file_locals = program.build_merged_file_locals(file_idx);

    let mut binder = BinderState::from_bound_state_with_scopes_and_augmentations(
        BinderOptions::default(),
        program.symbols.clone(),
        file_locals,
        // Arc::clone is O(1); cross-file lookup binders share the per-file
        // map by reference instead of deep-cloning it.
        Arc::clone(&file.node_symbols),
        BinderStateScopeInputs {
            scopes: file.scopes.clone(),
            node_scope_ids: file.node_scope_ids.clone(),
            global_augmentations: Arc::clone(&file.global_augmentations),
            module_augmentations: Arc::clone(&file.module_augmentations),
            augmentation_target_modules: Arc::clone(&file.augmentation_target_modules),
            module_exports: program.module_exports.clone(),
            module_declaration_exports_publicly: file.module_declaration_exports_publicly.clone(),
            reexports: program.reexports.clone(),
            wildcard_reexports: program.wildcard_reexports.clone(),
            symbol_arenas,
            declaration_arenas,
            sym_to_decl_indices,
            cross_file_node_symbols: Arc::clone(&program.cross_file_node_symbols),
            shorthand_ambient_modules: program.shorthand_ambient_modules.clone(),
            flow_nodes: file.flow_nodes.clone(),
            // Arc::clone is O(1); cross-file lookup binders share the per-file
            // node_flow map by reference instead of deep-cloning it.
            node_flow: Arc::clone(&file.node_flow),
            switch_clause_to_switch: file.switch_clause_to_switch.clone(),
            expando_properties: file.expando_properties.clone(),
            alias_partners: program.alias_partners.clone(),
        },
    );

    binder.is_external_module = file.is_external_module;
    binder.file_features = file.file_features;
    // file_locals includes program globals, so its exact library origins must
    // come from the same complete program, not the compacted per-file subset.
    binder.lib_symbol_reverse_remap = Arc::clone(&program.lib_symbol_reverse_remap);
    binder.lib_binders = program.lib_binders.clone();
    binder.lib_symbol_ids = program.lib_symbol_ids.clone();
    binder.lib_type_namespace = Arc::new(program.build_lib_type_namespace(file_idx));

    if !program.definition_store.is_fully_populated() {
        if file.semantic_defs.is_empty() {
            binder.semantic_defs = Arc::clone(&program.semantic_defs);
        } else {
            let mut composed_semantic_defs = (*program.semantic_defs).clone();
            for (sym_id, entry) in file.semantic_defs.iter() {
                composed_semantic_defs.insert(*sym_id, entry.clone());
            }
            binder.semantic_defs = Arc::new(composed_semantic_defs);
        }
    }
    if !binder.scopes.is_empty() {
        binder.current_scope_id = crate::binder::ScopeId(0);
    }

    binder.declared_modules = program.declared_modules.clone();
    binder.set_lib_symbols_merged(true);

    binder
}

/// Check function bodies with statistics
pub fn check_functions_with_stats(
    program: &MergedProgram,
) -> Result<(CheckResult, CheckStats), ProjectModuleResolutionError> {
    let result = check_functions_parallel(program)?;

    let stats = CheckStats {
        file_count: result.file_results.len(),
        function_count: result.function_count,
        diagnostic_count: result.diagnostic_count,
    };

    Ok((result, stats))
}

/// Statistics about parallel type checking
#[derive(Debug, Clone)]
pub struct CheckStats {
    /// Number of files checked
    pub file_count: usize,
    /// Number of functions checked
    pub function_count: usize,
    /// Number of diagnostics produced
    pub diagnostic_count: usize,
}

/// Parse files and collect statistics
pub fn parse_files_with_stats(files: Vec<(String, String)>) -> (Vec<ParseResult>, ParallelStats) {
    let total_bytes: usize = files.iter().map(|(_, src)| src.len()).sum();
    let file_count = files.len();

    let results = parse_files_parallel(files);

    let total_nodes: usize = results.iter().map(|r| r.arena.len()).sum();
    let error_count: usize = results.iter().map(|r| r.parse_diagnostics.len()).sum();

    let stats = ParallelStats {
        file_count,
        total_bytes,
        total_nodes,
        error_count,
    };

    (results, stats)
}

#[cfg(test)]
#[path = "../../../tests/parallel_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../../../tests/cross_file_interface_merge_ts2717_driver_tests.rs"]
mod cross_file_interface_merge_ts2717_driver_tests;

#[cfg(test)]
#[path = "../../../tests/cross_file_class_decl_owner_driver_tests.rs"]
mod cross_file_class_decl_owner_driver_tests;
