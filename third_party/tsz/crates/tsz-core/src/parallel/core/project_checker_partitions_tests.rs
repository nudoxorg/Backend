use super::*;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tsz_common::{ProjectExecutionBudget, ProjectExecutionStop};

fn execution_budget(cancelled: &AtomicBool) -> ProjectExecutionBudget<'_> {
    ProjectExecutionBudget::new(
        Instant::now() + Duration::from_secs(30),
        cancelled,
        10_000_000,
    )
}

fn assert_same_results(actual: &CheckResult, expected: &CheckResult) {
    assert_eq!(actual.diagnostic_count, expected.diagnostic_count);
    assert_eq!(actual.function_count, expected.function_count);
    assert_eq!(actual.file_results.len(), expected.file_results.len());
    for (actual, expected) in actual.file_results.iter().zip(&expected.file_results) {
        assert_eq!(actual.file_idx, expected.file_idx);
        assert_eq!(actual.file_name, expected.file_name);
        assert_eq!(actual.diagnostics, expected.diagnostics);
        assert!(actual.function_results.is_empty());
        assert!(expected.function_results.is_empty());
    }
}

fn in_memory_lib() -> Arc<LibFile> {
    in_memory_lib_named("lib/custom.d.ts")
}

fn in_memory_lib_named(name: &str) -> Arc<LibFile> {
    let name = name.to_owned();
    let mut parser = ParserState::new(name.clone(),
        "interface Object {} interface Function {} interface CallableFunction {} interface NewableFunction {} interface String {} interface Number {} interface Boolean {} interface RegExp {} interface IArguments {} interface Array<T> { length: number; [n: number]: T; } interface Shared { baseline: number; }".to_owned());
    let source_file = parser.parse_source_file();
    assert!(parser.get_diagnostics().is_empty());
    let mut binder = BinderState::new();
    binder.bind_source_file(parser.get_arena(), source_file);
    Arc::new(LibFile::new(
        name,
        Arc::new(parser.into_arena()),
        Arc::new(binder),
        source_file,
    ))
}

fn pristine_parity_fixture() -> (
    MergedProgram,
    Vec<Arc<LibFile>>,
    CheckerOptions,
    Vec<ProjectModuleResolution>,
    ProjectSemanticOptions,
) {
    let libs = vec![in_memory_lib()];
    let sources = vec![
        ("src/base.ts".to_owned(), "export class Base<T> { constructor(public value: T) {} } export type Box<T> = { value: T };".to_owned()),
        ("src/a.ts".to_owned(), "import { Base, Box } from './base'; class Leaf extends Base<number> {} const good: Box<number> = { value: 1 }; const bad: number = 'wrong'; export const a = new Leaf(good.value);".to_owned()),
        ("src/b.ts".to_owned(), "import { Base, Box } from './base'; class Leaf extends Base<string> {} const good: Box<string> = { value: 'ok' }; const bad: string = 2; export const b = new Leaf(good.value);".to_owned()),
        ("src/c.ts".to_owned(), "export {}; type Box<T> = { value: T }; const good: Box<boolean> = { value: true }; const bad: boolean = 1;".to_owned()),
        ("src/d.ts".to_owned(), "export {}; interface Optional { value?: number } const bad: Optional = { value: undefined };".to_owned()),
        ("src/e.ts".to_owned(), "export {}; const values: Array<number> = [1]; const bad: number = values[20];".to_owned()),
    ];
    let semantic = ProjectSemanticOptions::declaration_scoped();
    let binds = parse_and_bind_parallel_with_libs(sources, &libs);
    let refs = binds.iter().collect::<Vec<_>>();
    let program = merge_bind_results_ref_with_project_semantic_options(&refs, semantic);
    let options = CheckerOptions {
        strict: true,
        strict_null_checks: true,
        no_unchecked_indexed_access: true,
        exact_optional_property_types: true,
        ..CheckerOptions::default()
    };
    let resolutions = ["src/a.ts", "src/b.ts"].map(|importer| ProjectModuleResolution {
        importer_path: importer.to_owned(),
        specifier: "./base".to_owned(),
        request_kind: ProjectModuleRequestKind::EsmImport,
        resolution_mode: None,
        target: ProjectModuleResolutionTarget::File {
            path: "src/base.ts".to_owned(),
        },
    });
    (program, libs, options, resolutions.to_vec(), semantic)
}

#[test]
fn partitions_preserve_cross_file_identity_options_libraries_and_diagnostics() {
    let (program, libs, options, resolutions, semantic) = pristine_parity_fixture();
    let cancelled = AtomicBool::new(false);
    let expected = check_files_parallel_with_project_inputs_and_execution_checkpoint(
        &program,
        &options,
        &libs,
        semantic,
        &resolutions,
        &execution_budget(&cancelled),
    )
    .unwrap();
    for path in ["src/a.ts", "src/b.ts", "src/c.ts"] {
        assert!(
            expected
                .file_results
                .iter()
                .find(|file| file.file_name == path)
                .unwrap()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == 2322),
            "fixture must diagnose the intentional assignment in {path}"
        );
    }
    assert_eq!(
        expected.file_results.len(),
        program.files.len() + libs.len()
    );
    for (path, code) in [("src/d.ts", 2375), ("src/e.ts", 2322)] {
        assert!(
            expected
                .file_results
                .iter()
                .find(|file| file.file_name == path)
                .unwrap()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == code),
            "fixture must exercise the admitted strict option in {path}"
        );
    }
    for workers in [1, 2, 4, 8] {
        let (actual_program, actual_libs, actual_options, actual_resolutions, actual_semantic) =
            pristine_parity_fixture();
        assert!(!Arc::ptr_eq(
            &actual_program.definition_store,
            &program.definition_store
        ));
        assert!(!Arc::ptr_eq(&actual_libs[0].arena, &libs[0].arena));
        assert!(!Arc::ptr_eq(&actual_libs[0].binder, &libs[0].binder));
        let actual = check_files_with_project_inputs_and_checker_partitions(
            &actual_program,
            &actual_options,
            &actual_libs,
            actual_semantic,
            &actual_resolutions,
            ProjectCheckerExecution {
                checkpoint: &execution_budget(&cancelled),
                workers: NonZeroUsize::new(workers).unwrap(),
            },
        )
        .unwrap();
        assert_same_results(&actual, &expected);
    }
}

#[test]
fn partitions_keep_exact_unresolved_requests_and_typed_bad_targets() {
    let program = compile_files_with_libs(
        vec![(
            "src/main.ts".to_owned(),
            "import { missing } from './missing'; export const value = missing;".to_owned(),
        )],
        &[],
    );
    let options = CheckerOptions::default();
    let semantic = ProjectSemanticOptions::structural();
    let unresolved = ProjectModuleResolution {
        importer_path: "src/main.ts".to_owned(),
        specifier: "./missing".to_owned(),
        request_kind: ProjectModuleRequestKind::EsmImport,
        resolution_mode: None,
        target: ProjectModuleResolutionTarget::Unresolved,
    };
    let cancelled = AtomicBool::new(false);
    let expected = check_files_parallel_with_project_inputs_and_execution_checkpoint(
        &program,
        &options,
        &[],
        semantic,
        &[unresolved.clone()],
        &execution_budget(&cancelled),
    )
    .unwrap();
    assert!(
        expected.file_results[0]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == 2307)
    );
    let actual_program = compile_files_with_libs(
        vec![(
            "src/main.ts".to_owned(),
            "import { missing } from './missing'; export const value = missing;".to_owned(),
        )],
        &[],
    );
    assert!(!Arc::ptr_eq(
        &actual_program.definition_store,
        &program.definition_store
    ));
    let actual = check_files_with_project_inputs_and_checker_partitions(
        &actual_program,
        &options,
        &[],
        semantic,
        &[unresolved.clone()],
        ProjectCheckerExecution {
            checkpoint: &execution_budget(&cancelled),
            workers: NonZeroUsize::new(2).unwrap(),
        },
    )
    .unwrap();
    assert_same_results(&actual, &expected);
    let bad_target = ProjectModuleResolution {
        target: ProjectModuleResolutionTarget::File {
            path: "src/absent.ts".to_owned(),
        },
        ..unresolved
    };
    assert!(matches!(
        check_files_with_project_inputs_and_checker_partitions(
            &program,
            &options,
            &[],
            semantic,
            &[bad_target],
            ProjectCheckerExecution {
                checkpoint: &execution_budget(&cancelled),
                workers: NonZeroUsize::new(2).unwrap()
            }
        ),
        Err(ProjectPartitionCheckError::Program(
            ProjectProgramCheckError::ModuleResolution(
                ProjectModuleResolutionError::TargetNotInProgram { .. }
            )
        ))
    ));
}

#[test]
fn cancelled_and_expired_projects_return_typed_stops_without_results() {
    let program = compile_files_with_libs(
        vec![("src/a.ts".to_owned(), "export const a = 1;".to_owned())],
        &[],
    );
    let cancelled = AtomicBool::new(true);
    let expired = AtomicBool::new(false);
    let cases = [
        (
            execution_budget(&cancelled),
            ProjectExecutionStop::Cancelled,
        ),
        (
            ProjectExecutionBudget::new(
                Instant::now() - Duration::from_secs(1),
                &expired,
                1_000_000,
            ),
            ProjectExecutionStop::Deadline,
        ),
        (
            ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(30), &expired, 0),
            ProjectExecutionStop::WorkBudgetExhausted,
        ),
    ];
    for (budget, reason) in cases {
        assert!(
            matches!(check_files_with_project_inputs_and_checker_partitions(
            &program, &CheckerOptions::default(), &[], ProjectSemanticOptions::structural(), &[],
            ProjectCheckerExecution { checkpoint: &budget, workers: NonZeroUsize::new(2).unwrap() }), Err(ProjectPartitionCheckError::Program(
                ProjectProgramCheckError::ExecutionStopped(actual))) if actual == reason)
        );
    }
}

#[test]
fn stop_after_completed_file_prevents_starting_later_partition_files() {
    let sources = (0..8)
        .map(|index| {
            (
                format!("src/{index}.ts"),
                format!("export const value: number = {index};"),
            )
        })
        .collect();
    let program = compile_files_with_libs(sources, &[]);
    let options = CheckerOptions::default();
    let cancelled = AtomicBool::new(false);
    let budget = execution_budget(&cancelled);
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::structural(),
        Some(Arc::new(
            build_project_module_resolution_outcomes(&program, &[]).unwrap(),
        )),
        Some(&budget),
    )
    .unwrap();
    let mut completed = Vec::new();
    let result =
        plan.check_file_partition_observing_completion(0, &program.files, &|_| {}, |index| {
            completed.push(index);
            cancelled.store(true, Ordering::Release);
        });
    assert!(matches!(result, Err(ProjectExecutionStop::Cancelled)));
    assert_eq!(
        completed,
        vec![0],
        "the real first-file check completed; later files did not"
    );
    assert_eq!(budget.stop_reason(), Some(ProjectExecutionStop::Cancelled));
}

#[test]
fn stopped_admission_precedes_invalid_resolution_and_resolution_bytes_are_metered() {
    let mut invalid =
        compile_files_with_libs(vec![("src/a.ts".to_owned(), "export {};".to_owned())], &[]);
    invalid.files[0].file_name = "../invalid.ts".to_owned();
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        check_files_with_project_inputs_and_checker_partitions(
            &invalid,
            &CheckerOptions::default(),
            &[],
            ProjectSemanticOptions::structural(),
            &[],
            ProjectCheckerExecution {
                checkpoint: &execution_budget(&cancelled),
                workers: NonZeroUsize::new(2).unwrap()
            }
        ),
        Err(ProjectPartitionCheckError::Program(
            ProjectProgramCheckError::ExecutionStopped(ProjectExecutionStop::Cancelled)
        ))
    ));
    let not_cancelled = AtomicBool::new(false);
    let expired = ProjectExecutionBudget::new(
        Instant::now() - Duration::from_secs(1),
        &not_cancelled,
        1_000_000,
    );
    assert!(matches!(
        check_files_with_project_inputs_and_checker_partitions(
            &invalid,
            &CheckerOptions::default(),
            &[],
            ProjectSemanticOptions::structural(),
            &[],
            ProjectCheckerExecution {
                checkpoint: &expired,
                workers: NonZeroUsize::new(2).unwrap()
            }
        ),
        Err(ProjectPartitionCheckError::Program(
            ProjectProgramCheckError::ExecutionStopped(ProjectExecutionStop::Deadline)
        ))
    ));

    let program =
        compile_files_with_libs(vec![("src/a.ts".to_owned(), "export {};".to_owned())], &[]);
    let resolution = ProjectModuleResolution {
        importer_path: "src/a.ts".to_owned(),
        specifier: "x".repeat(1024),
        request_kind: ProjectModuleRequestKind::EsmImport,
        resolution_mode: None,
        target: ProjectModuleResolutionTarget::Unresolved,
    };
    let budget =
        ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(30), &not_cancelled, 30);
    let result = build_project_module_resolution_outcomes_with_poll(
        &program,
        &[resolution],
        |units| {
            budget
                .checkpoint(units)
                .map_err(ProjectProgramCheckError::ExecutionStopped)
        },
        |path, poll| validate_project_virtual_path_with_poll(path, poll),
    );
    assert!(matches!(
        result,
        Err(ProjectProgramCheckError::ExecutionStopped(
            ProjectExecutionStop::WorkBudgetExhausted
        ))
    ));
    assert_eq!(
        budget.stop_reason(),
        Some(ProjectExecutionStop::WorkBudgetExhausted)
    );
}

#[test]
fn metered_path_validation_preserves_legacy_path_grammar() {
    for path in [
        "",
        "/a.ts",
        "src/a.ts",
        "src/café.ts",
        "src/a\\b.ts",
        "a\0.ts",
        "a//b.ts",
        "./a.ts",
        "a/../b.ts",
        "a/.",
        "a/",
        "a..ts",
        "文件/入口.ts",
    ] {
        let expected = validate_project_virtual_path(path);
        let actual = validate_project_virtual_path_with_poll(path, &mut |_units| {
            Ok::<_, ProjectModuleResolutionError>(())
        });
        assert_eq!(actual, expected, "grammar changed for {path:?}");
    }
    let path = format!("{}/café.ts", "segment/".repeat(100));
    assert_eq!(
        validate_project_virtual_path_with_poll(&path, &mut |_units| Ok::<
            _,
            ProjectModuleResolutionError,
        >(())),
        validate_project_virtual_path(&path)
    );
}

fn pristine_library_fixture() -> (MergedProgram, Vec<Arc<LibFile>>, CheckerOptions) {
    let libs = vec![
        in_memory_lib_named("lib/a.d.ts"),
        in_memory_lib_named("lib/b.d.ts"),
    ];
    let binds = parse_and_bind_parallel_with_libs(
        vec![
            (
                "src/a.ts".to_owned(),
                "interface Shared { added: string; }".to_owned(),
            ),
            (
                "src/b.ts".to_owned(),
                "export const independent = 1;".to_owned(),
            ),
        ],
        &libs,
    );
    let refs = binds.iter().collect::<Vec<_>>();
    let semantic = ProjectSemanticOptions::structural();
    let program = merge_bind_results_ref_with_project_semantic_options(&refs, semantic);
    let options = CheckerOptions::default();
    (program, libs, options)
}

#[test]
fn isolated_library_baseline_observes_budget_and_mid_library_stop_discards_output() {
    let (program, libs, options) = pristine_library_fixture();
    let semantic = ProjectSemanticOptions::structural();
    let cancelled = AtomicBool::new(false);
    let budget = execution_budget(&cancelled);
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &libs,
        semantic,
        Some(Arc::new(
            build_project_module_resolution_outcomes(&program, &[]).unwrap(),
        )),
        Some(&budget),
    )
    .unwrap();
    assert!(
        plan.affected_lib_interfaces.contains("Shared"),
        "real user declaration must engage the library baseline"
    );
    {
        use tsz_solver::construction::QueryDatabase;
        let shared = plan
            .shared_query_cache
            .as_ref()
            .expect("two-file plan must have shared L2");
        let before = shared.total_entries();
        let baseline = plan.make_lib_baseline_query_cache();
        let literal = program.type_interner.literal_string("baseline-isolation");
        assert!(baseline.is_subtype_of(literal, TypeId::STRING));
        assert!(baseline.relation_cache_stats().subtype_entries > 0);
        assert_eq!(
            shared.total_entries(),
            before,
            "baseline must not publish its memo into augmented L2"
        );
    }
    cancelled.store(true, Ordering::Release);
    assert_eq!(
        budget.stop_reason(),
        None,
        "only the baseline's borrowed budget should latch this stop"
    );
    let _discarded_baseline = plan.check_one_lib_baseline(0, &plan.checker_lib_files[0]);
    assert_eq!(budget.stop_reason(), Some(ProjectExecutionStop::Cancelled));

    let (program, libs, options) = pristine_library_fixture();
    let cancelled = AtomicBool::new(false);
    let budget = execution_budget(&cancelled);
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &libs,
        semantic,
        Some(Arc::new(
            build_project_module_resolution_outcomes(&program, &[]).unwrap(),
        )),
        Some(&budget),
    )
    .unwrap();
    assert!(
        plan.affected_lib_interfaces.contains("Shared"),
        "real user declaration must engage the library baseline"
    );
    let mut results = vec![plan.lib_file_result(
        99,
        &libs[0],
        vec![Diagnostic::error(
            "sentinel.ts",
            7,
            1,
            "previous complete file",
            2322,
        )],
    )];
    let before = results[0].diagnostics.clone();
    let mut completions = Vec::new();
    let result = plan.run_lib_checks_observing_completion(&mut results, |completion| {
        completions.push(completion);
        cancelled.store(true, Ordering::Release);
    });
    assert!(matches!(result, Err(ProjectExecutionStop::Cancelled)));
    assert_eq!(completions, vec![LibraryCheckCompletion::Baseline(0)]);
    assert_eq!(
        results.len(),
        1,
        "stopped library stage must not append partial results"
    );
    assert_eq!(results[0].diagnostics, before);
}

#[test]
fn cold_consumer_before_interface_owner_keeps_inherited_member_semantics() {
    let binds = parse_and_bind_parallel_with_libs(vec![
        ("src/consumer.ts".to_owned(), "import type { Derived } from './owner'; export function inherited(value: Derived<number>): number { return value.base; }".to_owned()),
        ("src/owner.ts".to_owned(), "export interface Base<T> { base: T } export interface Derived<T> extends Base<T> { own: T }".to_owned()),
    ], &[]);
    let refs = binds.iter().collect::<Vec<_>>();
    let semantic = ProjectSemanticOptions::declaration_scoped();
    let program = merge_bind_results_ref_with_project_semantic_options(&refs, semantic);
    assert_eq!(program.files[0].file_name, "src/consumer.ts");
    let options = CheckerOptions::default();
    let resolution = ProjectModuleResolution {
        importer_path: "src/consumer.ts".to_owned(),
        specifier: "./owner".to_owned(),
        request_kind: ProjectModuleRequestKind::EsmImport,
        resolution_mode: None,
        target: ProjectModuleResolutionTarget::File {
            path: "src/owner.ts".to_owned(),
        },
    };
    let cancelled = AtomicBool::new(false);
    let budget = execution_budget(&cancelled);
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &[],
        semantic,
        Some(Arc::new(
            build_project_module_resolution_outcomes(&program, &[resolution]).unwrap(),
        )),
        Some(&budget),
    )
    .unwrap();
    let consumer = plan.check_file_partition(0, &program.files[..1]).unwrap();
    assert!(
        consumer[0].diagnostics.is_empty(),
        "cold consumer must resolve the inherited member before owner check: {:?}",
        consumer[0].diagnostics
    );
    let owner = plan.check_file_partition(1, &program.files[1..]).unwrap();
    assert!(
        owner[0].diagnostics.is_empty(),
        "owner declaration is valid: {:?}",
        owner[0].diagnostics
    );
}

#[test]
fn outer_api_discards_completed_partition_results_when_another_partition_stops() {
    use std::sync::{Mutex, mpsc};
    let program = compile_files_with_libs(
        (0..4)
            .map(|index| {
                (
                    format!("src/{index}.ts"),
                    format!("export const value: number = {index};"),
                )
            })
            .collect(),
        &[],
    );
    let options = CheckerOptions::default();
    let cancelled = AtomicBool::new(false);
    let budget = execution_budget(&cancelled);
    let (completed_sender, completed_receiver) = mpsc::channel();
    let completed_receiver = Mutex::new(completed_receiver);
    let first_partition_completed = AtomicBool::new(false);
    let observe = |index| {
        if index == 1 {
            first_partition_completed.store(true, Ordering::Release);
            cancelled.store(true, Ordering::Release);
            completed_sender.send(()).unwrap();
        } else if index == 2 {
            // Keep this worker from finishing its partition before the first
            // partition has completed real checking. This synchronization is
            // bounded and only controls test scheduling, not solver results.
            completed_receiver
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    };
    let result = check_project_partitions_observing_completion(
        ProjectCheckerInputs {
            program: &program,
            options: &options,
            libraries: &[],
            semantics: ProjectSemanticOptions::structural(),
            resolutions: &[],
        },
        ProjectCheckerExecution {
            checkpoint: &budget,
            workers: NonZeroUsize::new(2).unwrap(),
        },
        &observe,
    );
    assert!(
        first_partition_completed.load(Ordering::Acquire),
        "one complete real partition is required before the stop"
    );
    assert!(matches!(
        result,
        Err(ProjectPartitionCheckError::Program(
            ProjectProgramCheckError::ExecutionStopped(ProjectExecutionStop::Cancelled)
        ))
    ));
    assert_eq!(budget.stop_reason(), Some(ProjectExecutionStop::Cancelled));
}

#[test]
fn project_session_latches_one_shot_database_stop_and_discards_callback_output() {
    struct OneShotStop(AtomicBool);
    impl tsz_common::ExecutionCheckpoint for OneShotStop {
        fn checkpoint(&self, _work: u64) -> Result<(), ProjectExecutionStop> {
            if self.0.swap(false, Ordering::AcqRel) {
                Err(ProjectExecutionStop::Cancelled)
            } else {
                Ok(())
            }
        }
    }
    let mut program = compile_files_with_libs(
        vec![(
            "src/main.ts".to_owned(),
            "export const value = 1;".to_owned(),
        )],
        &[],
    );
    program.set_project_module_resolutions(&[]).unwrap();
    let options = CheckerOptions::default();
    let source = OneShotStop(AtomicBool::new(false));
    let session = ProjectCheckerSession::new(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::structural(),
        &source,
    )
    .unwrap();
    let callback_ran = AtomicBool::new(false);
    let result = session.with_file_checker_and_types(0, |_, _, _, types| {
        callback_ran.store(true, Ordering::Release);
        source.0.store(true, Ordering::Release);
        assert_eq!(
            types.execution_checkpoint(1),
            Err(ProjectExecutionStop::Cancelled)
        );
        // This models a solver-local sentinel after a typed database stop. It
        // must not become a successful semantic callback result.
        TypeId::UNKNOWN
    });
    assert!(callback_ran.load(Ordering::Acquire));
    assert_eq!(
        tsz_common::ExecutionCheckpoint::checkpoint(&source, 0),
        Ok(()),
        "underlying checkpoint must actually be one-shot"
    );
    assert!(matches!(
        result,
        Err(ProjectCheckerSessionError::ExecutionStopped(
            ProjectExecutionStop::Cancelled
        ))
    ));
    let later_ran = AtomicBool::new(false);
    let later = session.with_file_checker(0, |_, _, _| {
        later_ran.store(true, Ordering::Release);
        TypeId::NUMBER
    });
    assert!(matches!(
        later,
        Err(ProjectCheckerSessionError::ExecutionStopped(
            ProjectExecutionStop::Cancelled
        ))
    ));
    assert!(
        !later_ran.load(Ordering::Acquire),
        "latched scope must reject later callbacks"
    );
}

#[test]
fn library_name_index_stop_drops_partial_names_and_legacy_index_is_unchanged() {
    let lib = in_memory_lib();
    let contexts = [LibContext {
        arena: Arc::clone(&lib.arena),
        binder: Arc::clone(&lib.binder),
    }];
    let expected = tsz_checker::context::build_lib_file_local_names(&contexts).unwrap();
    assert!(expected.len() > 3);
    let mut remaining = 2_u64;
    let stopped = tsz_checker::context::build_lib_file_local_names_with_poll(&contexts, |work| {
        remaining = remaining
            .checked_sub(work)
            .ok_or(ProjectExecutionStop::WorkBudgetExhausted)?;
        Ok(())
    });
    assert!(matches!(
        stopped,
        Err(ProjectExecutionStop::WorkBudgetExhausted)
    ));
    let complete = tsz_checker::context::build_lib_file_local_names_with_poll(&contexts, |_| {
        Ok::<(), ProjectExecutionStop>(())
    })
    .unwrap()
    .unwrap();
    assert_eq!(complete, expected);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn two_worker_cold_consumer_finishes_before_interface_owner_starts() {
    use std::sync::{Mutex, mpsc};
    let semantic = ProjectSemanticOptions::declaration_scoped();
    let binds = parse_and_bind_parallel_with_libs(vec![
        ("src/consumer.ts".to_owned(), "import type { Derived } from './owner'; export function inherited(value: Derived<number>): number { return value.base; }".to_owned()),
        ("src/owner.ts".to_owned(), "export interface Base<T> { base: T } export interface Derived<T> extends Base<T> { own: T }".to_owned()),
    ], &[]);
    let refs = binds.iter().collect::<Vec<_>>();
    let program = merge_bind_results_ref_with_project_semantic_options(&refs, semantic);
    assert_eq!(program.files[0].file_name, "src/consumer.ts");
    assert_eq!(program.files[1].file_name, "src/owner.ts");
    let options = CheckerOptions::default();
    let resolutions = [ProjectModuleResolution {
        importer_path: "src/consumer.ts".to_owned(),
        specifier: "./owner".to_owned(),
        request_kind: ProjectModuleRequestKind::EsmImport,
        resolution_mode: None,
        target: ProjectModuleResolutionTarget::File {
            path: "src/owner.ts".to_owned(),
        },
    }];
    let cancelled = AtomicBool::new(false);
    let budget = execution_budget(&cancelled);
    let (sender, receiver) = mpsc::channel();
    let receiver = Mutex::new(receiver);
    let (owner_entered_sender, owner_entered_receiver) = mpsc::channel();
    let owner_entered_receiver = Mutex::new(owner_entered_receiver);
    let consumer_complete = AtomicBool::new(false);
    let owner_started_after_consumer = AtomicBool::new(false);
    let threads = Mutex::new(Vec::new());
    let starting = |index| {
        threads
            .lock()
            .unwrap()
            .push(std::thread::current().name().unwrap().to_owned());
        if index == 0 {
            owner_entered_receiver
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .expect("provider gate must occupy the other real private worker");
        } else if index == 1 {
            owner_entered_sender.send(()).unwrap();
            receiver
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .expect("consumer must finish on the other admitted worker");
            assert!(consumer_complete.load(Ordering::Acquire));
            owner_started_after_consumer.store(true, Ordering::Release);
        }
    };
    let completed = |index| {
        if index == 0 {
            consumer_complete.store(true, Ordering::Release);
            sender.send(()).unwrap();
        }
    };
    let result = check_project_partitions_observing_file_boundaries(
        ProjectCheckerInputs {
            program: &program,
            options: &options,
            libraries: &[],
            semantics: semantic,
            resolutions: &resolutions,
        },
        ProjectCheckerExecution {
            checkpoint: &budget,
            workers: NonZeroUsize::new(2).unwrap(),
        },
        &starting,
        &completed,
    )
    .expect("complete real two-worker check");
    assert!(consumer_complete.load(Ordering::Acquire));
    assert!(owner_started_after_consumer.load(Ordering::Acquire));
    let threads = threads.lock().unwrap();
    assert_eq!(threads.len(), 2);
    assert!(
        threads
            .iter()
            .all(|name| name == "tsz-checker-0" || name == "tsz-checker-1")
    );
    assert_ne!(
        threads[0], threads[1],
        "the bounded gate requires two real private workers"
    );
    assert_eq!(result.file_results.len(), 2);
    assert_eq!(
        result.diagnostic_count, 0,
        "cold function-annotation inherited member must resolve before provider checking: {:?}",
        result.file_results
    );
    assert!(
        result
            .file_results
            .iter()
            .all(|file| file.diagnostics.is_empty())
    );
}

#[test]
fn exhausted_library_materialization_rolls_back_completed_library_pushes() {
    let (program, libs, options) = pristine_library_fixture();
    let cancelled = AtomicBool::new(false);
    let budget = execution_budget(&cancelled);
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &libs,
        ProjectSemanticOptions::declaration_scoped(),
        Some(Arc::new(
            build_project_module_resolution_outcomes(&program, &[]).unwrap(),
        )),
        Some(&budget),
    )
    .unwrap();
    assert!(plan.affected_lib_interfaces.contains("Shared"));
    let mut results = vec![plan.lib_file_result(
        99,
        &libs[0],
        vec![Diagnostic::error(
            "sentinel.ts",
            7,
            1,
            "previous complete file",
            2322,
        )],
    )];
    let original = results[0].diagnostics.clone();
    let mut completed = Vec::new();
    let stop = plan.run_lib_checks_observing_completion(&mut results, |completion| {
        completed.push(completion);
        if completion == LibraryCheckCompletion::Augmented(1) {
            let remaining = budget.remaining_work_units();
            assert!(remaining > 1);
            budget.checkpoint(remaining - 1).unwrap();
        }
    });
    assert_eq!(
        completed,
        [
            LibraryCheckCompletion::Baseline(0),
            LibraryCheckCompletion::Baseline(1),
            LibraryCheckCompletion::Augmented(0),
            LibraryCheckCompletion::Augmented(1)
        ]
    );
    assert!(matches!(
        stop,
        Err(ProjectExecutionStop::WorkBudgetExhausted)
    ));
    assert_eq!(
        budget.remaining_work_units(),
        0,
        "first completed library consumed the remaining move allowance before second push stopped"
    );
    assert_eq!(results.len(), 1, "append failure rolls back library pushes");
    assert_eq!(results[0].diagnostics, original);
}
