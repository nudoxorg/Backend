/// Failure to start or complete a bounded project checker run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectPartitionCheckError {
    /// Exact module-resolution or execution failure from the admitted program.
    Program(ProjectProgramCheckError),
    /// The caller's requested private worker pool could not be started.
    WorkerPoolUnavailable(String),
}

impl std::fmt::Display for ProjectPartitionCheckError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Program(error) => write!(formatter, "{error}"),
            Self::WorkerPoolUnavailable(error) => {
                write!(
                    formatter,
                    "project checker worker pool unavailable: {error}"
                )
            }
        }
    }
}

impl std::error::Error for ProjectPartitionCheckError {}

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
    let worker_count = workers.get().min(program.files.len().max(1));
    let run = || -> Result<CheckResult, ProjectPartitionCheckError> {
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
        plan.prime_module_augmentation_bodies();
        poll()?;
        let partition_size = program.files.len().div_ceil(worker_count).max(1);
        let check_partition = |partition_idx: usize, files: &[BoundFile]| {
            plan.check_file_partition_observing_completion(
                partition_idx * partition_size,
                files,
                starting,
                completed,
            )
        };
        #[cfg(not(target_arch = "wasm32"))]
        let partitions = program
            .files
            .par_chunks(partition_size)
            .enumerate()
            .map(|(index, files)| check_partition(index, files))
            .collect::<Vec<_>>();
        #[cfg(target_arch = "wasm32")]
        let partitions = program
            .files
            .chunks(partition_size)
            .enumerate()
            .map(|(index, files)| check_partition(index, files))
            .collect::<Vec<_>>();
        let mut file_results = Vec::with_capacity(program.files.len() + lib_files.len());
        for partition in partitions {
            for result in partition
                .map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)))?
            {
                execution_checkpoint.checkpoint(1).map_err(|reason| {
                    Program(ProjectProgramCheckError::ExecutionStopped(reason))
                })?;
                file_results.push(result);
            }
        }
        poll()?;
        plan.run_lib_checks_with_execution_checkpoint(&mut file_results)
            .map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)))?;
        poll()?;
        let result = plan
            .aggregate_with_poll(file_results, |work| execution_checkpoint.checkpoint(work))
            .map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)))?;
        poll()?;
        Ok(result)
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count)
            .stack_size(tsz_common::limits::THREAD_STACK_SIZE_BYTES)
            .thread_name(|index| format!("tsz-checker-{index}"))
            .build()
            .map_err(|error| {
                ProjectPartitionCheckError::WorkerPoolUnavailable(error.to_string())
            })?;
        poll()?;
        pool.install(run)
    }
    #[cfg(target_arch = "wasm32")]
    {
        run()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryCheckCompletion {
    Baseline(usize),
    Augmented(usize),
}

impl ParallelCheckPlan<'_> {
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
                let result = self.check_one_lib_baseline(index, lib);
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
                let mut result = self.check_one_lib(index, lib);
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
        mut completed: impl FnMut(usize),
    ) -> std::result::Result<Vec<FileCheckResult>, tsz_common::ProjectExecutionStop> {
        let poll = || self.checkpoint(0);
        poll()?;
        let Some(first_file) = files.first() else {
            return Ok(Vec::new());
        };
        let query_cache = self.make_query_cache();
        let mut checker = self.configured_file_checker(first_file_idx, first_file, &query_cache);
        let mut results = Vec::with_capacity(files.len());
        for (offset, file) in files.iter().enumerate() {
            // Once another worker latches a stop, no later file is started.
            poll()?;
            let file_idx = first_file_idx + offset;
            if offset != 0 {
                query_cache.reset_for_next_file();
                checker.ctx.switch_to_file(
                    &file.arena,
                    self.all_binders[file_idx].as_ref(),
                    file.file_name.clone(),
                    file_idx,
                );
            }
            starting(file_idx);
            // Reset/switch may clear large local maps. Poll again so a stop
            // observed during that boundary cannot start the next file.
            poll()?;
            checker.check_source_file(file.source_file);
            poll()?;
            let diagnostics = std::mem::take(&mut checker.ctx.diagnostics);
            results.push(Self::user_file_result(file_idx, file, diagnostics));
            completed(file_idx);
        }
        Ok(results)
    }
}

#[cfg(test)]
#[path = "project_checker_partitions_tests.rs"]
mod project_checker_partitions_tests;
