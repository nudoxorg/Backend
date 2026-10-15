use tsz_common::{ProjectExecutionFile, ProjectExecutionPhase, ProjectExecutionSpan};

/// Failure to start or complete a bounded project checker run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectPartitionCheckError {
    /// Exact module-resolution or execution failure from the admitted program.
    Program(ProjectProgramCheckError),
    /// The owned image lacks its exact project-resolution witness.
    CheckerSession(ProjectCheckerSessionError),
    /// The caller's requested private worker pool could not be started.
    WorkerPoolUnavailable(String),
    /// One or more files spent their whole per-file work allowance.
    ///
    /// No result of this run is publishable: a file that stopped part-way may
    /// have written partial state into program-wide tables that every other
    /// file in the same image reads. The caller must rebuild the image from
    /// its bound sources and skip these files; the list is sorted by file
    /// index and complete for the files this run reached.
    FileWorkExhausted(Vec<ProjectFileWorkExhaustion>),
}

impl std::fmt::Display for ProjectPartitionCheckError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Program(error) => write!(formatter, "{error}"),
            Self::CheckerSession(error) => write!(formatter, "{error}"),
            Self::WorkerPoolUnavailable(error) => {
                write!(
                    formatter,
                    "project checker worker pool unavailable: {error}"
                )
            }
            Self::FileWorkExhausted(files) => write!(
                formatter,
                "{} file(s) exhausted their per-file work allowance",
                files.len()
            ),
        }
    }
}

impl std::error::Error for ProjectPartitionCheckError {}

/// One file whose semantic check spent its entire per-file work allowance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFileWorkExhaustion {
    /// Index of the file in the checked program.
    pub file_index: usize,
    /// Exact program path of the file.
    pub path: String,
    /// The per-file allowance that was spent.
    pub allowance: u64,
}

/// Per-file work isolation for one partitioned project check.
///
/// Each semantic file check runs under its own slice of the program's shared
/// checkpoint, so a single expensive file ends as a named
/// [`ProjectFileWorkExhaustion`] instead of draining the work that every other
/// file needs. Skipped files keep their parse, bind, and merge results; only
/// their own semantic check is not run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectFileWork {
    /// Maximum work one file's semantic check may spend.
    pub allowance: u64,
    /// Exact program paths whose semantic check is not run.
    pub skipped: std::collections::BTreeSet<String>,
}

/// Results of one partition plus the files that spent their work allowance.
#[derive(Default)]
struct PartitionCheck {
    results: Vec<FileCheckResult>,
    exhausted: Vec<ProjectFileWorkExhaustion>,
}

/// Caller-admitted concurrency and one borrowed cooperative project checkpoint.
#[derive(Clone, Copy)]
pub struct ProjectCheckerExecution<'checkpoint> {
    /// Shared deadline, cancellation, and work control for every partition.
    pub checkpoint: &'checkpoint dyn tsz_common::ExecutionCheckpoint,
    /// Maximum simultaneous private checker workers; no ambient default.
    pub workers: std::num::NonZeroUsize,
}

struct ProjectCheckerInputs<'program> {
    program: &'program MergedProgram,
    options: &'program CheckerOptions,
    libraries: &'program [Arc<LibFile>],
    semantics: ProjectSemanticOptions,
    resolutions: &'program [ProjectModuleResolution],
}

/// Check every admitted file using at most `workers` private checker workers.
///
/// Indexed partitions borrow one immutable project plan. Each partition owns
/// one checker and one local query cache; file-local state is reset between
/// files, while shared arenas, binders, definitions, and L2 remain shared.
/// Results retain source order and the same library and diagnostic projections
/// as fresh-file checking. A stopped partition produces no publishable partial
/// result. This is cooperative cancellation: already-running solver work must
/// reach a checkpoint before this function can return. Plan-building helpers
/// (including single-file binder construction and library arena cloning) and complete
/// diagnostic augmentation/suppression passes remain indivisible polling
/// boundaries. This API promises neither a hard deadline nor a total heap bound.
///
/// The caller selects concurrency explicitly. This API does not use ambient
/// Rayon concurrency or change the legacy fresh-file checking policy.
pub fn check_files_with_project_inputs_and_checker_partitions(
    program: &MergedProgram,
    checker_options: &CheckerOptions,
    lib_files: &[Arc<LibFile>],
    project_semantic_options: ProjectSemanticOptions,
    resolutions: &[ProjectModuleResolution],
    execution: ProjectCheckerExecution<'_>,
) -> std::result::Result<CheckResult, ProjectPartitionCheckError> {
    check_project_partitions_observing_completion(
        ProjectCheckerInputs {
            program,
            options: checker_options,
            libraries: lib_files,
            semantics: project_semantic_options,
            resolutions,
        },
        execution,
        &|_| {},
    )
}

fn check_project_partitions_observing_completion(
    inputs: ProjectCheckerInputs<'_>,
    execution: ProjectCheckerExecution<'_>,
    completed: &(impl Fn(usize) + Sync),
) -> std::result::Result<CheckResult, ProjectPartitionCheckError> {
    check_project_partitions_observing_file_boundaries(inputs, execution, &|_| {}, completed)
}

fn check_project_partitions_observing_file_boundaries(
    inputs: ProjectCheckerInputs<'_>,
    execution: ProjectCheckerExecution<'_>,
    starting: &(impl Fn(usize) + Sync),
    completed: &(impl Fn(usize) + Sync),
) -> std::result::Result<CheckResult, ProjectPartitionCheckError> {
    let ProjectCheckerInputs {
        program,
        options: checker_options,
        libraries: lib_files,
        semantics: project_semantic_options,
        resolutions,
    } = inputs;
    use ProjectPartitionCheckError::Program;
    let execution_scope = tsz_common::LatchedExecutionCheckpoint::new(execution.checkpoint);
    let execution_checkpoint = &execution_scope as &dyn tsz_common::ExecutionCheckpoint;
    let workers = execution.workers;
    let poll = || {
        execution_checkpoint
            .checkpoint(0)
            .map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)))
    };
    poll()?;
    let resolution_span = ProjectExecutionSpan::new(
        Some(execution_checkpoint),
        ProjectExecutionPhase::ModuleOutcomes,
        None,
    );
    let outcomes = build_project_module_resolution_outcomes_with_poll(
        program,
        resolutions,
        |work| {
            execution_checkpoint
                .checkpoint(work)
                .map_err(ProjectProgramCheckError::ExecutionStopped)
        },
        |path, poll| validate_project_virtual_path_with_poll(path, poll),
    )
    .map_err(Program)?;
    poll()?;
    resolution_span.returned();
    let worker_count = workers.get().min(program.files.len().max(1));
    run_on_private_checker_workers(worker_count, || {
    let plan_span = ProjectExecutionSpan::new(
            Some(execution_checkpoint),
            ProjectExecutionPhase::CheckerPlan,
            None,
        );
        let plan = ParallelCheckPlan::build(
            program,
            checker_options,
            lib_files,
            project_semantic_options,
            Some(Arc::new(outcomes)),
            Some(execution_checkpoint),
        )
        .map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)))?;
        poll()?;
        plan_span.returned();
    check_prepared_project_partitions_in_current_pool(&plan, workers, None, starting, completed)
    })
}

fn check_prepared_project_partitions(
    plan: &ParallelCheckPlan<'_>,
    workers: std::num::NonZeroUsize,
    file_work: Option<&ProjectFileWork>,
    starting: &(impl Fn(usize) + Sync),
    completed: &(impl Fn(usize) + Sync),
) -> Result<CheckResult, ProjectPartitionCheckError> {
    let worker_count = workers.get().min(plan.program.files.len().max(1));
    run_on_private_checker_workers(worker_count, || {
        check_prepared_project_partitions_in_current_pool(
            plan, workers, file_work, starting, completed,
        )
    })
}

fn check_prepared_project_partitions_in_current_pool(
    plan: &ParallelCheckPlan<'_>,
    workers: std::num::NonZeroUsize,
    file_work: Option<&ProjectFileWork>,
    starting: &(impl Fn(usize) + Sync),
    completed: &(impl Fn(usize) + Sync),
) -> Result<CheckResult, ProjectPartitionCheckError> {
    use ProjectPartitionCheckError::Program;
    let execution_checkpoint = plan.execution_control().expect("partition plan has caller checkpoint");
    let poll = || plan.checkpoint(0).map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)));
    poll()?;
    let worker_count = workers.get().min(plan.program.files.len().max(1));
    let run = || -> Result<CheckResult, ProjectPartitionCheckError> {
        let prime_span = ProjectExecutionSpan::new(
            Some(execution_checkpoint),
            ProjectExecutionPhase::AugmentationPrime,
            None,
        );
        plan.prime_module_augmentation_bodies();
        poll()?;
        prime_span.returned();
        // Only files that are semantically checked are divided among the workers.
        // Declaration files that are skipped, and files the caller already named
        // as too expensive, cost nothing; counting them would hand one worker all
        // of the real work whenever they sort ahead of the application sources
        // (`node_modules` sorts before `src`).
        let (checked_partitions, unchecked_indices) = plan.semantic_check_partitions(worker_count, file_work);
        let check_partition = |indices: &Vec<usize>| {
            plan.check_file_partition_isolated(indices, file_work, starting, completed)
        };
        #[cfg(not(target_arch = "wasm32"))]
        let partitions = checked_partitions
            .par_iter()
            .map(|indices| check_partition(indices))
            .collect::<Vec<_>>();
        #[cfg(target_arch = "wasm32")]
        let partitions = checked_partitions
            .iter()
            .map(|indices| check_partition(indices))
            .collect::<Vec<_>>();
        let mut file_results = Vec::with_capacity(plan.program.files.len() + plan.checker_lib_files.len());
        for index in unchecked_indices {
            // Not semantically checked: an empty result keeps every file present.
            starting(index);
            file_results.push(ParallelCheckPlan::user_file_result(index, &plan.program.files[index], Vec::new()));
            completed(index);
        }
        let mut exhausted = Vec::new();
        for partition in partitions {
            let partition = partition
                .map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)))?;
            exhausted.extend(partition.exhausted);
            for result in partition.results {
                execution_checkpoint.checkpoint(1).map_err(|reason| {
                    Program(ProjectProgramCheckError::ExecutionStopped(reason))
                })?;
                file_results.push(result);
            }
        }
        poll()?;
        // Partitions are visited in order and each is ascending, so the checked files
        // are ascending already; unchecked files were added first. Restore file order.
        file_results.sort_by_key(|result| result.file_idx);
        if !exhausted.is_empty() {
            // Program-wide stops (polled above) take precedence. No result of this
            // run is publishable.
            return Err(ProjectPartitionCheckError::FileWorkExhausted(exhausted));
        }
        plan.run_lib_checks_with_execution_checkpoint(&mut file_results)
            .map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)))?;
        poll()?;
        let aggregate_span = ProjectExecutionSpan::new(
            Some(execution_checkpoint),
            ProjectExecutionPhase::Aggregate,
            None,
        );
        let result = plan
            .aggregate_with_poll(file_results, |work| execution_checkpoint.checkpoint(work))
            .map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)))?;
        poll()?;
        aggregate_span.returned();
        Ok(result)
    };
    run()
}

fn run_on_private_checker_workers<Output: Send>(
    worker_count: usize,
    run: impl FnOnce() -> Result<Output, ProjectPartitionCheckError> + Send,
) -> Result<Output, ProjectPartitionCheckError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count)
            .stack_size(tsz_common::limits::THREAD_STACK_SIZE_BYTES)
            .thread_name(|index| format!("tsz-checker-{index}"))
            .build()
            .map_err(|error| ProjectPartitionCheckError::WorkerPoolUnavailable(error.to_string()))?;
        pool.install(run)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = worker_count;
        run()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryCheckCompletion {
    Baseline(usize),
    Augmented(usize),
}

impl ParallelCheckPlan<'_> {
    /// Divides the files that are semantically checked into contiguous partitions of
    /// equal size, one per worker, and returns the files that are not checked.
    ///
    /// Declaration files whose policy skips semantics, and files the caller already
    /// named, cost nothing and are excluded from the division. Counting them would
    /// hand one worker all of the real work whenever they sort ahead of the
    /// application sources, which `node_modules` does before `src`.
    fn semantic_check_partitions(
        &self,
        worker_count: usize,
        file_work: Option<&ProjectFileWork>,
    ) -> (Vec<Vec<usize>>, Vec<usize>) {
        let (checked, unchecked): (Vec<usize>, Vec<usize>) = (0..self.program.files.len())
            .partition(|&index| {
                let file = &self.program.files[index];
                self.checks_file_semantics(file)
                    && !file_work.is_some_and(|work| work.skipped.contains(&file.file_name))
            });
        let partition_size = checked.len().div_ceil(worker_count.max(1)).max(1);
        let partitions = checked
            .chunks(partition_size)
            .map(<[usize]>::to_vec)
            .collect();
        (partitions, unchecked)
    }

    fn run_lib_checks_with_execution_checkpoint(
        &self,
        file_results: &mut Vec<FileCheckResult>,
    ) -> std::result::Result<(), tsz_common::ProjectExecutionStop> {
        self.run_lib_checks_observing_completion(file_results, |_| {})
    }

    fn run_lib_checks_observing_completion(
        &self,
        file_results: &mut Vec<FileCheckResult>,
        mut completed: impl FnMut(LibraryCheckCompletion),
    ) -> std::result::Result<(), tsz_common::ProjectExecutionStop> {
        let poll = |work| self.checkpoint(work);
        poll(0)?;
        let mut library_results = Vec::with_capacity(self.checker_lib_files.len());
        if self.affected_lib_interfaces.is_empty() {
            for (index, lib) in self.checker_lib_files.iter().enumerate() {
                poll(1)?;
                library_results.push(self.lib_file_result(index, lib, Vec::new()));
            }
        } else {
            let mut baseline_diagnostics = FxHashSet::default();
            for (index, lib) in self.checker_lib_files.iter().enumerate() {
                poll(1)?;
                let span = ProjectExecutionSpan::new(
                    self.execution_control(),
                    ProjectExecutionPhase::LibraryBaseline,
                    Some(ProjectExecutionFile {
                        index: self.program.files.len() + index,
                        path: &lib.file_name,
                    }),
                );
                let result = self.check_one_lib_baseline(index, lib);
                span.returned();
                poll(0)?;
                completed(LibraryCheckCompletion::Baseline(index));
                poll(0)?;
                for diagnostic in result.diagnostics {
                    poll(1)?;
                    baseline_diagnostics
                        .insert(lib_diagnostic_fingerprint(&result.file_name, &diagnostic));
                }
            }
            for (index, lib) in self.checker_lib_files.iter().enumerate() {
                poll(1)?;
                let span = ProjectExecutionSpan::new(
                    self.execution_control(),
                    ProjectExecutionPhase::LibraryCheck,
                    Some(ProjectExecutionFile {
                        index: self.program.files.len() + index,
                        path: &lib.file_name,
                    }),
                );
                let mut result = self.check_one_lib(index, lib);
                span.returned();
                poll(0)?;
                let mut kept = Vec::with_capacity(result.diagnostics.len());
                for diagnostic in result.diagnostics {
                    poll(1)?;
                    if !baseline_diagnostics
                        .contains(&lib_diagnostic_fingerprint(&result.file_name, &diagnostic))
                    {
                        kept.push(diagnostic);
                    }
                }
                result.diagnostics = kept;
                completed(LibraryCheckCompletion::Augmented(index));
                poll(0)?;
                library_results.push(result);
            }
        }
        poll(0)?;
        let previous_len = file_results.len();
        let appended = (|| {
            for result in library_results {
                poll(1)?;
                file_results.push(result);
            }
            poll(0)
        })();
        if appended.is_err() {
            file_results.truncate(previous_len);
        }
        appended
    }

    #[cfg(test)]
    fn check_file_partition(
        &self,
        first_file_idx: usize,
        files: &[BoundFile],
    ) -> std::result::Result<Vec<FileCheckResult>, tsz_common::ProjectExecutionStop> {
        self.check_file_partition_observing_completion(first_file_idx, files, &|_| {}, |_| {})
    }

    fn check_file_partition_observing_completion(
        &self,
        first_file_idx: usize,
        files: &[BoundFile],
        starting: &(impl Fn(usize) + Sync),
        completed: impl FnMut(usize),
    ) -> std::result::Result<Vec<FileCheckResult>, tsz_common::ProjectExecutionStop> {
        let indices = (first_file_idx..first_file_idx + files.len()).collect::<Vec<_>>();
        self.check_file_partition_isolated(&indices, None, starting, completed)
            .map(|partition| partition.results)
    }

    /// Check one contiguous partition of files with one reusable checker.
    ///
    /// Without `file_work` this is the amortized legacy path: one query cache
    /// and one checker serve every file. With `file_work`, each semantic file
    /// check runs under its own [`tsz_common::FileWorkSlice`]. A file that
    /// spends its slice is recorded, its partial diagnostics are dropped, and
    /// the query cache and checker that observed the stop are retired before
    /// the next file, because the cache latches the stop it saw. The run keeps
    /// checking so the caller learns every such file in one pass; its results
    /// are not publishable (see [`ProjectPartitionCheckError::FileWorkExhausted`]).
    /// Program-wide stops always take precedence and abort the partition.
    fn check_file_partition_isolated(
        &self,
        file_indices: &[usize],
        file_work: Option<&ProjectFileWork>,
        starting: &(impl Fn(usize) + Sync),
        mut completed: impl FnMut(usize),
    ) -> std::result::Result<PartitionCheck, tsz_common::ProjectExecutionStop> {
        let poll = || self.checkpoint(0);
        poll()?;
        let mut check = PartitionCheck::default();
        let files = &self.program.files;
        if file_indices.is_empty() {
            return Ok(check);
        }
        check.results.reserve(file_indices.len());
        // The slice is armed per file; it only exists when a policy and a
        // caller checkpoint both exist.
        let slice = file_work
            .and(self.execution_control())
            .map(|parent| tsz_common::FileWorkSlice::new(parent, u64::MAX));
        let mut next = 0;
        while next < file_indices.len() {
            let query_cache = self.make_query_cache_with(
                slice
                    .as_ref()
                    .map(|slice| slice as &dyn tsz_common::ExecutionCheckpoint)
                    .or_else(|| self.execution_control()),
            );
            let build_start = next;
            let mut checker = self.configured_file_checker(
                file_indices[build_start],
                &files[file_indices[build_start]],
                &query_cache,
            );
            let mut retire = false;
            while next < file_indices.len() && !retire {
                let offset = next;
                let file_idx = file_indices[offset];
                let file = &files[file_idx];
                // Once another worker latches a stop, no later file is started.
                poll()?;
                let observed_file = Some(ProjectExecutionFile {
                    index: file_idx,
                    path: &file.file_name,
                });
                let reset_span = ProjectExecutionSpan::new(
                    self.execution_control(),
                    ProjectExecutionPhase::FileReset,
                    observed_file,
                );
                if offset != build_start {
                    query_cache.reset_for_next_file();
                    checker.ctx.switch_to_file(
                        &file.arena,
                        self.all_binders[file_idx].as_ref(),
                        file.file_name.clone(),
                        file_idx,
                    );
                }
                reset_span.returned();
                starting(file_idx);
                // Reset/switch may clear large local maps. Poll again so a stop
                // observed during that boundary cannot start the next file.
                poll()?;
                let skipped = file_work.is_some_and(|work| work.skipped.contains(&file.file_name));
                if let (Some(slice), Some(work)) = (slice.as_ref(), file_work) {
                    slice.arm(work.allowance);
                }
                let span = ProjectExecutionSpan::new(
                    self.execution_control(),
                    ProjectExecutionPhase::FileCheck,
                    observed_file,
                );
                if self.checks_file_semantics(file) && !skipped {
                    // The file's slice (or the program checkpoint) is observed by every
                    // solver walk of this check, not only by code holding the query cache.
                    let _ambient = slice
                        .as_ref()
                        .map(|slice| slice as &dyn tsz_common::ExecutionCheckpoint)
                        .or_else(|| self.execution_control())
                        .map(tsz_common::enter_ambient_checkpoint);
                    checker.check_source_file(file.source_file);
                }
                span.returned();
                // A program-wide stop wins over a file's own exhaustion.
                poll()?;
                if let Some(slice) = slice.as_ref().filter(|slice| slice.is_exhausted()) {
                    check.exhausted.push(ProjectFileWorkExhaustion {
                        file_index: file_idx,
                        path: file.file_name.clone(),
                        allowance: slice.allowance(),
                    });
                    check
                        .results
                        .push(Self::user_file_result(file_idx, file, Vec::new()));
                    completed(file_idx);
                    // The next cache and checker start from a clean, unarmed slice.
                    slice.arm(u64::MAX);
                    retire = true;
                    next += 1;
                    continue;
                }
                let post_span = ProjectExecutionSpan::new(
                    self.execution_control(),
                    ProjectExecutionPhase::FilePostprocess,
                    observed_file,
                );
                let diagnostics = std::mem::take(&mut checker.ctx.diagnostics);
                check
                    .results
                    .push(Self::user_file_result(file_idx, file, diagnostics));
                post_span.returned();
                completed(file_idx);
                next += 1;
            }
        }
        Ok(check)
    }
}

#[cfg(test)]
#[path = "project_checker_partitions_tests.rs"]
mod project_checker_partitions_tests;
