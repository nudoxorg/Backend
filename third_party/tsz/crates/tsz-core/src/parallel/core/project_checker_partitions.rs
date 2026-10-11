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

/// Check every admitted file using at most `workers` private checker workers.
///
/// Indexed partitions borrow one immutable project plan. Each partition owns
/// one checker and one local query cache; file-local state is reset between
/// files, while shared arenas, binders, definitions, and L2 remain shared.
/// Results retain source order and the same library and diagnostic projections
/// as fresh-file checking. A stopped partition produces no publishable partial
/// result. This is cooperative cancellation: already-running solver work must
/// reach a checkpoint before this function can return.
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
    use ProjectPartitionCheckError::Program;
    let execution_checkpoint = execution.checkpoint;
    let workers = execution.workers;
    let outcomes = build_project_module_resolution_outcomes(program, resolutions)
        .map_err(|error| Program(ProjectProgramCheckError::ModuleResolution(error)))?;
    let poll = || {
        execution_checkpoint
            .checkpoint(0)
            .map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)))
    };
    poll()?;
    let plan = ParallelCheckPlan::build(
        program,
        checker_options,
        lib_files,
        project_semantic_options,
        Some(Arc::new(outcomes)),
        Some(execution_checkpoint),
    );
    poll()?;
    plan.prime_module_augmentation_bodies();
    poll()?;
    let worker_count = workers.get().min(program.files.len().max(1));
    let partition_size = program.files.len().div_ceil(worker_count).max(1);
    let check_partition = |partition_idx: usize, files: &[BoundFile]| {
        plan.check_file_partition(partition_idx * partition_size, files)
    };
    #[cfg(not(target_arch = "wasm32"))]
    let partitions = {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count)
            .stack_size(tsz_common::limits::THREAD_STACK_SIZE_BYTES)
            .thread_name(|index| format!("tsz-checker-{index}"))
            .build()
            .map_err(|error| {
                ProjectPartitionCheckError::WorkerPoolUnavailable(error.to_string())
            })?;
        pool.install(|| {
            program
                .files
                .par_chunks(partition_size)
                .enumerate()
                .map(|(index, files)| check_partition(index, files))
                .collect::<Vec<_>>()
        })
    };
    #[cfg(target_arch = "wasm32")]
    let partitions = program
        .files
        .chunks(partition_size)
        .enumerate()
        .map(|(index, files)| check_partition(index, files))
        .collect::<Vec<_>>();
    let mut file_results = Vec::with_capacity(program.files.len() + lib_files.len());
    for partition in partitions {
        file_results.extend(
            partition
                .map_err(|reason| Program(ProjectProgramCheckError::ExecutionStopped(reason)))?,
        );
    }
    poll()?;
    plan.run_lib_checks(&mut file_results);
    poll()?;
    let result = plan.aggregate(file_results);
    poll()?;
    Ok(result)
}

impl ParallelCheckPlan<'_> {
    fn check_file_partition(
        &self,
        first_file_idx: usize,
        files: &[BoundFile],
    ) -> std::result::Result<Vec<FileCheckResult>, tsz_common::ProjectExecutionStop> {
        self.check_file_partition_observing_completion(first_file_idx, files, |_| {})
    }

    fn check_file_partition_observing_completion(
        &self,
        first_file_idx: usize,
        files: &[BoundFile],
        mut completed: impl FnMut(usize),
    ) -> std::result::Result<Vec<FileCheckResult>, tsz_common::ProjectExecutionStop> {
        let poll = || match self.execution_checkpoint {
            Some(checkpoint) => checkpoint.checkpoint(0),
            None => Ok(()),
        };
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
