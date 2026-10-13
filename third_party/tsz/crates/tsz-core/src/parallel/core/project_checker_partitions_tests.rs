use super::*;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tsz_common::{ProjectExecutionBudget, ProjectExecutionStop};
use tsz_parser::parser::node::NodeAccess;

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
    let options = CheckerOptions {
        module: tsz_common::common::ModuleKind::ESNext,
        ..CheckerOptions::default()
    };
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
    let classic = check_files_parallel_with_project_inputs_and_execution_checkpoint(
        &program,
        &CheckerOptions::default(),
        &[],
        semantic,
        &[unresolved.clone()],
        &execution_budget(&cancelled),
    )
    .unwrap();
    assert!(
        classic.file_results[0]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == 2792),
        "default Classic module diagnostics still retain authoritative unresolved membership"
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
    let file = &program.files[0];
    let inherited_member = (0..file.arena.len())
        .map(|index| NodeIndex(index as u32))
        .find(|&index| {
            file.arena
                .get(index)
                .is_some_and(|node| node.kind == syntax_kind_ext::PROPERTY_ACCESS_EXPRESSION)
        })
        .expect("inherited member expression");
    let observed = plan.with_bound_file_checker_and_types(0, file, |checker, _, _, _| {
        checker.get_type_of_node(inherited_member)
    });
    assert_eq!(
        observed,
        TypeId::NUMBER,
        "consumer must retain exact inherited number before owner check"
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
        let stopped_union = types.union(vec![TypeId::NUMBER, TypeId::STRING]);
        assert_eq!(
            stopped_union,
            TypeId::UNKNOWN,
            "the actual database union must observe the one-shot stop"
        );
        // The solver-local sentinel must not become a successful semantic
        // callback result at the typed session boundary.
        stopped_union
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
    use crate::checker::context::ResolutionModeOverride;
    use std::sync::{Mutex, mpsc};
    use tsz_common::common::ModuleKind;
    // Each case owns a pristine program/store. The incorrect string return
    // must observe the inherited number; UNKNOWN/any cannot pass this oracle.
    for (module, resolution_mode, return_type, expected_diagnostics) in [
        (ModuleKind::None, None, "number", 0),
        (ModuleKind::None, None, "string", 1),
        (ModuleKind::ESNext, None, "number", 0),
        (ModuleKind::ESNext, None, "string", 1),
        (
            ModuleKind::ESNext,
            Some(ResolutionModeOverride::Import),
            "number",
            0,
        ),
        (
            ModuleKind::ESNext,
            Some(ResolutionModeOverride::Import),
            "string",
            1,
        ),
        (
            ModuleKind::NodeNext,
            Some(ResolutionModeOverride::Require),
            "number",
            0,
        ),
        (
            ModuleKind::NodeNext,
            Some(ResolutionModeOverride::Require),
            "string",
            1,
        ),
    ] {
        let semantic = ProjectSemanticOptions::declaration_scoped();
        // An explicit outcome mode must come from this retained type-only
        // request's syntax. Plain .ts has no package-format evidence here;
        // an implicit NodeNext edge must not infer the supplied Require mode.
        let attributes = match (module, resolution_mode) {
            (ModuleKind::NodeNext, Some(ResolutionModeOverride::Require)) => {
                " with { 'resolution-mode': 'require' }"
            }
            _ => "",
        };
        let binds = parse_and_bind_parallel_with_libs(vec![
        ("src/consumer.ts".to_owned(), format!("import type {{ Derived }} from './owner'{attributes}; export function inherited(value: Derived<number>): {return_type} {{ return value.base; }}")),
        ("src/owner.ts".to_owned(), "export interface Base<T> { base: T } export interface Derived<T> extends Base<T> { own: T }".to_owned()),
    ], &[]);
        let refs = binds.iter().collect::<Vec<_>>();
        let program = merge_bind_results_ref_with_project_semantic_options(&refs, semantic);
        assert_eq!(program.files[0].file_name, "src/consumer.ts");
        assert_eq!(program.files[1].file_name, "src/owner.ts");
        let options = CheckerOptions {
            module,
            ..CheckerOptions::default()
        };
        let resolutions = [ProjectModuleResolution {
            importer_path: "src/consumer.ts".to_owned(),
            specifier: "./owner".to_owned(),
            request_kind: ProjectModuleRequestKind::EsmImport,
            resolution_mode,
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
            result.diagnostic_count, expected_diagnostics,
            "cold inherited number must accept number and reject string before provider checking: {:?}",
            result.file_results
        );
        assert!(
            result.file_results[1].diagnostics.is_empty(),
            "provider declaration remains valid"
        );
        if expected_diagnostics == 0 {
            assert!(result.file_results[0].diagnostics.is_empty());
        } else {
            assert_eq!(result.file_results[0].diagnostics.len(), 1);
            assert_eq!(
                result.file_results[0].diagnostics[0].code, 2322,
                "inherited number must produce a real incompatible string-return diagnostic"
            );
        }
    }
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

#[test]
fn syntax_requests_cannot_borrow_other_kind_mode_or_ambient_success() {
    use crate::checker::context::ResolutionModeOverride;
    use tsz_common::common::ModuleKind;
    let forms = [
        (
            ProjectModuleRequestKind::EsmImport,
            None,
            "import { value } from 'dep'; export const observed = value;",
        ),
        (
            ProjectModuleRequestKind::EsmReExport,
            None,
            "export { value } from 'dep';",
        ),
        (
            ProjectModuleRequestKind::DynamicImport,
            Some(ResolutionModeOverride::Import),
            "export const observed = import('dep');",
        ),
        (
            ProjectModuleRequestKind::CjsRequire,
            Some(ResolutionModeOverride::Require),
            "import dependency = require('dep'); export const observed = dependency.value;",
        ),
    ];
    for (kind, mode, source) in forms {
        for case in 0..6 {
            let program = compile_files_with_libs(
                vec![
                    ("src/main.ts".to_owned(), source.to_owned()),
                    (
                        "src/dep.ts".to_owned(),
                        "export const value: number = 42;".to_owned(),
                    ),
                    (
                        "ambient.d.ts".to_owned(),
                        "declare module 'dep' { export const value: string; }".to_owned(),
                    ),
                ],
                &[],
            );
            let mut resolution = ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "dep".to_owned(),
                request_kind: kind.clone(),
                resolution_mode: mode,
                target: ProjectModuleResolutionTarget::File {
                    path: "src/dep.ts".to_owned(),
                },
            };
            match case {
                0 => {} // Exact File positive.
                1 => {
                    resolution.target = ProjectModuleResolutionTarget::External {
                        identity: "compiler-admitted-ambient:dep".to_owned(),
                    }
                }
                2 => resolution.target = ProjectModuleResolutionTarget::Unresolved,
                3 => {} // No request entry.
                4 => {
                    resolution.request_kind = if kind == ProjectModuleRequestKind::EsmImport {
                        ProjectModuleRequestKind::EsmReExport
                    } else {
                        ProjectModuleRequestKind::EsmImport
                    }
                }
                5 => {
                    resolution.resolution_mode =
                        Some(if mode == Some(ResolutionModeOverride::Require) {
                            ResolutionModeOverride::Import
                        } else {
                            ResolutionModeOverride::Require
                        })
                }
                _ => unreachable!(),
            }
            let resolutions = if case == 3 {
                Vec::new()
            } else {
                vec![resolution]
            };
            let options = CheckerOptions {
                module: ModuleKind::ESNext,
                ..CheckerOptions::default()
            };
            let plan = ParallelCheckPlan::build(
                &program,
                &options,
                &[],
                ProjectSemanticOptions::structural(),
                Some(Arc::new(
                    build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
                )),
                None,
            )
            .unwrap();
            let results = plan.check_file_partition(0, &program.files[..1]).unwrap();
            let missing = results[0]
                .diagnostics
                .iter()
                .filter(|diagnostic| matches!(diagnostic.code, 2307 | 2792))
                .count();
            assert_eq!(
                missing,
                usize::from(case >= 2),
                "request {kind:?}/{mode:?} case {case}: {:?}",
                results[0].diagnostics
            );
        }
    }
}

#[test]
fn cold_barrel_consumer_preserves_number_and_string_discrimination() {
    for barrel in [
        "export { value } from './owner';",
        "export * from './owner';",
    ] {
        for (result_type, expected_diagnostics) in [("number", 0), ("string", 1)] {
            let program = compile_files_with_libs(
                vec![
                    (
                        "src/consumer.ts".to_owned(),
                        format!(
                            "import {{ value }} from './barrel'; export function observed(): {result_type} {{ return value; }}"
                        ),
                    ),
                    ("src/barrel.ts".to_owned(), barrel.to_owned()),
                    (
                        "src/owner.ts".to_owned(),
                        "export const value: number = 42;".to_owned(),
                    ),
                ],
                &[],
            );
            let resolutions = [
                ProjectModuleResolution {
                    importer_path: "src/consumer.ts".to_owned(),
                    specifier: "./barrel".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmImport,
                    resolution_mode: None,
                    target: ProjectModuleResolutionTarget::File {
                        path: "src/barrel.ts".to_owned(),
                    },
                },
                ProjectModuleResolution {
                    importer_path: "src/barrel.ts".to_owned(),
                    specifier: "./owner".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmReExport,
                    resolution_mode: None,
                    target: ProjectModuleResolutionTarget::File {
                        path: "src/owner.ts".to_owned(),
                    },
                },
            ];
            let options = CheckerOptions::default();
            let plan = ParallelCheckPlan::build(
                &program,
                &options,
                &[],
                ProjectSemanticOptions::declaration_scoped(),
                Some(Arc::new(
                    build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
                )),
                None,
            )
            .unwrap();
            let consumer = plan.check_file_partition(0, &program.files[..1]).unwrap();
            assert_eq!(
                consumer[0].diagnostics.len(),
                expected_diagnostics,
                "{barrel}: {:?}",
                consumer[0].diagnostics
            );
            assert!(
                consumer[0]
                    .diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic.code == 2322)
            );
            let file = &program.files[0];
            let usage = file
                .arena
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(raw, node)| {
                    file.arena
                        .get_identifier(node)
                        .filter(|identifier| identifier.escaped_text == "value")
                        .map(|_| NodeIndex(raw as u32))
                })
                .max_by_key(|&index| file.arena.get(index).unwrap().pos)
                .unwrap();
            let value_type = plan.with_bound_file_checker_and_types(0, file, |checker, _, _, _| {
                checker.get_type_of_node(usage)
            });
            assert_eq!(
                value_type,
                TypeId::NUMBER,
                "cold barrel value has an exact numeric type"
            );
            let owners = plan.check_file_partition(1, &program.files[1..]).unwrap();
            assert!(
                owners.iter().all(|owner| owner.diagnostics.is_empty()),
                "{:?}",
                owners
            );
        }
    }
}

#[test]
fn ordinary_import_none_mode_keeps_its_exact_file_and_members() {
    use crate::checker::context::ResolutionModeOverride;
    use tsz_common::common::ModuleKind;
    for (name, result_type, expected_code) in [
        ("value", "number", None),
        ("value", "string", Some(2322)),
        ("foreignOnly", "number", Some(2305)),
    ] {
        let program = compile_files_with_libs(
            vec![
                (
                    "src/main.ts".to_owned(),
                    format!(
                        "import {{ {name} }} from './dep'; export function observed(): {result_type} {{ return {name}; }}"
                    ),
                ),
                (
                    "src/dep.ts".to_owned(),
                    "export const value: number = 42;".to_owned(),
                ),
                (
                    "src/wrong.ts".to_owned(),
                    "export const value: string = 'wrong'; export const foreignOnly: number = 1;"
                        .to_owned(),
                ),
            ],
            &[],
        );
        let resolution = |kind, mode, path: &str| ProjectModuleResolution {
            importer_path: "src/main.ts".to_owned(),
            specifier: "./dep".to_owned(),
            request_kind: kind,
            resolution_mode: mode,
            target: ProjectModuleResolutionTarget::File {
                path: path.to_owned(),
            },
        };
        let resolutions = [
            resolution(ProjectModuleRequestKind::EsmImport, None, "src/dep.ts"),
            resolution(ProjectModuleRequestKind::EsmReExport, None, "src/wrong.ts"),
            resolution(
                ProjectModuleRequestKind::EsmImport,
                Some(ResolutionModeOverride::Require),
                "src/wrong.ts",
            ),
        ];
        let options = CheckerOptions {
            module: ModuleKind::ESNext,
            ..CheckerOptions::default()
        };
        let plan = ParallelCheckPlan::build(
            &program,
            &options,
            &[],
            ProjectSemanticOptions::declaration_scoped(),
            Some(Arc::new(
                build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
            )),
            None,
        )
        .unwrap();
        let result = plan.check_file_partition(0, &program.files[..1]).unwrap();
        let codes = result[0]
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            expected_code.into_iter().collect::<Vec<_>>(),
            "{name}/{result_type}: {:?}",
            result[0].diagnostics
        );
        if name == "value" {
            let file = &program.files[0];
            let usage = file
                .arena
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(raw, node)| {
                    file.arena
                        .get_identifier(node)
                        .filter(|identifier| identifier.escaped_text == "value")
                        .map(|_| NodeIndex(raw as u32))
                })
                .max_by_key(|&index| file.arena.get(index).unwrap().pos)
                .unwrap();
            assert_eq!(
                plan.with_bound_file_checker_and_types(0, file, |checker, _, _, _| checker
                    .get_type_of_node(usage)),
                TypeId::NUMBER
            );
        }
    }
}

#[test]
fn export_star_cannot_inspect_other_kind_export_equals_target() {
    let program = compile_files_with_libs(
        vec![
            (
                "src/barrel.ts".to_owned(),
                "export * from './dep';".to_owned(),
            ),
            (
                "src/dep.ts".to_owned(),
                "const value = 42; export = value;".to_owned(),
            ),
        ],
        &[],
    );
    let resolutions = [ProjectModuleResolution {
        importer_path: "src/barrel.ts".to_owned(),
        specifier: "./dep".to_owned(),
        request_kind: ProjectModuleRequestKind::EsmImport,
        resolution_mode: None,
        target: ProjectModuleResolutionTarget::File {
            path: "src/dep.ts".to_owned(),
        },
    }];
    let options = CheckerOptions {
        module: tsz_common::common::ModuleKind::ESNext,
        ..CheckerOptions::default()
    };
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::structural(),
        Some(Arc::new(
            build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
        )),
        None,
    )
    .unwrap();
    let results = plan.check_file_partition(0, &program.files[..1]).unwrap();
    let codes = results[0]
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect::<Vec<_>>();
    assert_eq!(
        codes,
        vec![2307],
        "wrong-kind export-equals target cannot lend TS2498: {:?}",
        results[0].diagnostics
    );
}

#[test]
fn exact_external_aliases_keep_ambient_string_against_competing_file_number() {
    use crate::checker::context::ResolutionModeOverride;
    use tsz_common::common::ModuleKind;
    for namespace in [false, true] {
        for (name, return_type, expected) in [
            ("value", "string", None),
            ("value", "number", Some(2322)),
            (
                "foreignOnly",
                "number",
                Some(if namespace { 2339 } else { 2305 }),
            ),
        ] {
            let expression = if namespace {
                format!("dependency.{name}")
            } else {
                name.to_owned()
            };
            let import = if namespace {
                "import * as dependency from 'dep';".to_owned()
            } else {
                format!("import {{ {name} }} from 'dep';")
            };
            let program = compile_files_with_libs(
                vec![
                    (
                        "src/main.ts".to_owned(),
                        format!(
                            "{import} export function observed(): {return_type} {{ return {expression}; }}"
                        ),
                    ),
                    (
                        "src/wrong.ts".to_owned(),
                        "export const value: number = 42; export const foreignOnly: number = 1;"
                            .to_owned(),
                    ),
                    (
                        "ambient.d.ts".to_owned(),
                        "declare module 'dep' { export const value: string; }".to_owned(),
                    ),
                ],
                &[],
            );
            let resolutions = [
                ProjectModuleResolution {
                    importer_path: "src/main.ts".to_owned(),
                    specifier: "dep".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmImport,
                    resolution_mode: None,
                    target: ProjectModuleResolutionTarget::External {
                        identity: "admitted-ambient:dep".to_owned(),
                    },
                },
                ProjectModuleResolution {
                    importer_path: "src/main.ts".to_owned(),
                    specifier: "dep".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmReExport,
                    resolution_mode: None,
                    target: ProjectModuleResolutionTarget::File {
                        path: "src/wrong.ts".to_owned(),
                    },
                },
                ProjectModuleResolution {
                    importer_path: "src/main.ts".to_owned(),
                    specifier: "dep".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmImport,
                    resolution_mode: Some(ResolutionModeOverride::Require),
                    target: ProjectModuleResolutionTarget::File {
                        path: "src/wrong.ts".to_owned(),
                    },
                },
            ];
            let options = CheckerOptions {
                module: ModuleKind::ESNext,
                ..CheckerOptions::default()
            };
            let plan = ParallelCheckPlan::build(
                &program,
                &options,
                &[],
                ProjectSemanticOptions::declaration_scoped(),
                Some(Arc::new(
                    build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
                )),
                None,
            )
            .unwrap();
            let checked = plan.check_file_partition(0, &program.files[..1]).unwrap();
            let codes = checked[0]
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>();
            assert_eq!(
                codes,
                expected.into_iter().collect::<Vec<_>>(),
                "namespace={namespace}/{name}/{return_type}: {:?}",
                checked[0].diagnostics
            );
            if name == "value" && !namespace {
                let file = &program.files[0];
                let usage = file
                    .arena
                    .nodes
                    .iter()
                    .enumerate()
                    .filter_map(|(raw, node)| {
                        file.arena
                            .get_identifier(node)
                            .filter(|identifier| identifier.escaped_text == "value")
                            .map(|_| NodeIndex(raw as u32))
                    })
                    .max_by_key(|&index| file.arena.get(index).unwrap().pos)
                    .unwrap();
                assert_eq!(
                    plan.with_bound_file_checker_and_types(0, file, |checker, _, _, _| checker
                        .get_type_of_node(usage)),
                    TypeId::STRING
                );
            }
        }
    }
}

#[test]
fn node_next_foreign_plain_ts_request_requires_admitted_file_format() {
    use crate::checker::context::ResolutionModeOverride;
    use tsz_common::common::ModuleKind;
    for (format, ambiguous) in [
        (None, false),
        (None, true),
        (Some(true), false),
        (Some(true), true),
    ] {
        let program = compile_files_with_libs(vec![
            ("src/main.ts".to_owned(), "import { value } from './barrel'; export function observed(): number { return value; }".to_owned()),
            ("src/barrel.ts".to_owned(), "export { value } from 'dep';".to_owned()),
            ("src/dep.ts".to_owned(), "export const value: number = 42;".to_owned()),
            ("src/wrong.ts".to_owned(), "export const value: string = 'wrong';".to_owned()),
        ], &[]);
        let mut resolutions = vec![
            ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "./barrel".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: Some(ResolutionModeOverride::Import),
                target: ProjectModuleResolutionTarget::File {
                    path: "src/barrel.ts".to_owned(),
                },
            },
            ProjectModuleResolution {
                importer_path: "src/barrel.ts".to_owned(),
                specifier: "dep".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmReExport,
                resolution_mode: Some(ResolutionModeOverride::Import),
                target: ProjectModuleResolutionTarget::File {
                    path: "src/dep.ts".to_owned(),
                },
            },
        ];
        if ambiguous {
            resolutions.push(ProjectModuleResolution {
                importer_path: "src/barrel.ts".to_owned(),
                specifier: "dep".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmReExport,
                resolution_mode: Some(ResolutionModeOverride::Require),
                target: ProjectModuleResolutionTarget::File {
                    path: "src/wrong.ts".to_owned(),
                },
            });
        }
        let options = CheckerOptions {
            module: ModuleKind::NodeNext,
            ..CheckerOptions::default()
        };
        let plan = ParallelCheckPlan::build(
            &program,
            &options,
            &[],
            ProjectSemanticOptions::declaration_scoped(),
            Some(Arc::new(
                build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
            )),
            None,
        )
        .unwrap();
        let file = &program.files[0];
        let usage = file
            .arena
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(raw, node)| {
                file.arena
                    .get_identifier(node)
                    .filter(|identifier| identifier.escaped_text == "value")
                    .map(|_| NodeIndex(raw as u32))
            })
            .max_by_key(|&index| file.arena.get(index).unwrap().pos)
            .unwrap();
        let (observed, diagnostics) =
            plan.with_bound_file_checker_and_types(0, file, |checker, _, _, _| {
                if let Some(format) = format {
                    checker.ctx.file_is_esm_map = Some(Arc::new(
                        [
                            ("src/main.ts".to_owned(), format),
                            ("src/barrel.ts".to_owned(), format),
                        ]
                        .into_iter()
                        .collect(),
                    ));
                }
                checker.check_source_file(file.source_file);
                let observed = checker.get_type_of_node(usage);
                (observed, std::mem::take(&mut checker.ctx.diagnostics))
            });
        if format.is_none() {
            assert_ne!(
                observed,
                TypeId::NUMBER,
                "unavailable format cannot select the number file"
            );
            assert_ne!(
                observed,
                TypeId::STRING,
                "unavailable format cannot select the string file"
            );
            assert!(
                diagnostics.iter().any(|diagnostic| diagnostic.code == 2307),
                "unavailable format must refuse the implicit request: {:?}",
                diagnostics
            );
        } else {
            assert!(diagnostics.is_empty(), "{:?}", diagnostics);
            assert_eq!(observed, TypeId::NUMBER);
        }
    }
}

fn assert_cold_request_semantics(
    files: Vec<(String, String)>,
    resolutions: &[ProjectModuleResolution],
    codes: &[u32],
    expected: Option<TypeId>,
) {
    assert_cold_request_semantics_and_owner(files, resolutions, codes, expected, None);
}

fn assert_cold_request_semantics_and_owner(
    files: Vec<(String, String)>,
    resolutions: &[ProjectModuleResolution],
    codes: &[u32],
    expected: Option<TypeId>,
    owner_codes: Option<(usize, &[u32])>,
) {
    use tsz_common::common::ModuleKind;
    let program = compile_files_with_libs(files, &[]);
    let options = CheckerOptions {
        module: ModuleKind::ESNext,
        ..CheckerOptions::default()
    };
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::declaration_scoped(),
        Some(Arc::new(
            build_project_module_resolution_outcomes(&program, resolutions).unwrap(),
        )),
        None,
    )
    .unwrap();
    let result = plan.check_file_partition(0, &program.files[..1]).unwrap();
    assert_eq!(
        result[0]
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>(),
        codes,
        "{:?}",
        result[0].diagnostics
    );
    let file = &program.files[0];
    let usage = file
        .arena
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(raw, node)| {
            file.arena
                .get_identifier(node)
                .filter(|identifier| identifier.escaped_text == "value")
                .map(|_| NodeIndex(raw as u32))
        })
        .max_by_key(|&index| file.arena.get(index).unwrap().pos)
        .unwrap();
    let usage = file
        .arena
        .parent_of(usage)
        .filter(|&parent| {
            file.arena
                .get(parent)
                .and_then(|node| file.arena.get_access_expr(node))
                .is_some_and(|access| access.name_or_argument == usage)
        })
        .unwrap_or(usage);
    let observed = plan.with_bound_file_checker_and_types(0, file, |checker, _, _, _| {
        checker.get_type_of_node(usage)
    });
    if let Some(expected) = expected {
        assert_eq!(observed, expected);
    } else {
        assert_ne!(
            observed,
            TypeId::NUMBER,
            "refused request cannot borrow competitor NUMBER"
        );
        assert_ne!(
            observed,
            TypeId::STRING,
            "refused request cannot borrow competitor STRING"
        );
    }
    if let Some((owner, expected)) = owner_codes {
        let checked = plan
            .check_file_partition(owner, &program.files[owner..owner + 1])
            .unwrap();
        assert_eq!(
            checked[0]
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            expected,
            "{:?}",
            checked[0].diagnostics
        );
    }
}

#[test]
fn named_and_wildcard_edges_keep_disjoint_same_specifier_modes() {
    use crate::checker::context::ResolutionModeOverride;
    for wildcard in [false, true] {
        for shape in ["ImportShape", "RequireShape"] {
            for (result_type, codes) in [("number", vec![]), ("string", vec![2322])] {
                let barrel = if wildcard {
                    "export type * from 'dep' with { 'resolution-mode': 'import' }; export type * from 'dep' with { 'resolution-mode': 'require' };".to_owned()
                } else {
                    "export type { ImportShape } from 'dep' with { 'resolution-mode': 'import' }; export type { RequireShape } from 'dep' with { 'resolution-mode': 'require' };".to_owned()
                };
                let files = vec![
                    ("src/main.ts".to_owned(), format!("import type {{ {shape} }} from './barrel'; declare const observed: {shape}; export function read(): {result_type} {{ return observed.value; }}")),
                    ("src/barrel.ts".to_owned(), barrel),
                    ("src/import.ts".to_owned(), "export interface ImportShape { value: number }".to_owned()),
                    ("src/require.ts".to_owned(), "export interface RequireShape { value: number }".to_owned()),
                    ("src/wrong.ts".to_owned(), "export interface ImportShape { value: string } export interface RequireShape { value: string }".to_owned()),
                ];
                let request = |importer: &str, specifier: &str, kind, mode, target: &str| {
                    ProjectModuleResolution {
                        importer_path: importer.to_owned(),
                        specifier: specifier.to_owned(),
                        request_kind: kind,
                        resolution_mode: mode,
                        target: ProjectModuleResolutionTarget::File {
                            path: target.to_owned(),
                        },
                    }
                };
                let resolutions = [
                    request(
                        "src/main.ts",
                        "./barrel",
                        ProjectModuleRequestKind::EsmImport,
                        None,
                        "src/barrel.ts",
                    ),
                    request(
                        "src/barrel.ts",
                        "dep",
                        ProjectModuleRequestKind::EsmReExport,
                        Some(ResolutionModeOverride::Import),
                        "src/import.ts",
                    ),
                    request(
                        "src/barrel.ts",
                        "dep",
                        ProjectModuleRequestKind::EsmReExport,
                        Some(ResolutionModeOverride::Require),
                        "src/require.ts",
                    ),
                    request(
                        "src/barrel.ts",
                        "dep",
                        ProjectModuleRequestKind::EsmImport,
                        None,
                        "src/wrong.ts",
                    ),
                ];
                assert_cold_request_semantics(files, &resolutions, &codes, Some(TypeId::NUMBER));
            }
        }
    }
}

#[test]
fn selected_namespace_keeps_its_surface_after_value_capture() {
    use crate::checker::context::ResolutionModeOverride;
    for external in [false, true] {
        for receiver in ["dependency", "captured"] {
            for indexed in [false, true] {
                for (return_type, expected) in [("string", vec![]), ("number", vec![2322])] {
                    let expression = if indexed {
                        format!(r#"{receiver}["value"]"#)
                    } else {
                        format!("{receiver}.value")
                    };
                    let files = vec![
                    (
                        "src/main.ts".to_owned(),
                        format!(
                            "import * as dependency from 'dep'; const captured = dependency; export function read(): {return_type} {{ return {expression}; }}"
                        ),
                    ),
                    (
                        "src/wrong.ts".to_owned(),
                        "export const value: number = 42; export const foreignOnly: number = 1;"
                            .to_owned(),
                    ),
                    (
                        "src/owner.ts".to_owned(),
                        "export const value: string = 'selected';".to_owned(),
                    ),
                    (
                        "ambient.d.ts".to_owned(),
                        "declare module 'dep' { export const value: string; }".to_owned(),
                    ),
                ];
                    let resolutions = [
                        ProjectModuleResolution {
                            importer_path: "src/main.ts".to_owned(),
                            specifier: "dep".to_owned(),
                            request_kind: ProjectModuleRequestKind::EsmImport,
                            resolution_mode: None,
                            target: if external {
                                ProjectModuleResolutionTarget::External {
                                    identity: "admitted-ambient:dep".to_owned(),
                                }
                            } else {
                                ProjectModuleResolutionTarget::File {
                                    path: "src/owner.ts".to_owned(),
                                }
                            },
                        },
                        ProjectModuleResolution {
                            importer_path: "src/main.ts".to_owned(),
                            specifier: "dep".to_owned(),
                            request_kind: ProjectModuleRequestKind::EsmReExport,
                            resolution_mode: None,
                            target: ProjectModuleResolutionTarget::File {
                                path: "src/wrong.ts".to_owned(),
                            },
                        },
                        ProjectModuleResolution {
                            importer_path: "src/main.ts".to_owned(),
                            specifier: "dep".to_owned(),
                            request_kind: ProjectModuleRequestKind::EsmImport,
                            resolution_mode: Some(ResolutionModeOverride::Require),
                            target: ProjectModuleResolutionTarget::File {
                                path: "src/wrong.ts".to_owned(),
                            },
                        },
                    ];
                    let program = compile_files_with_libs(files, &[]);
                    let options = CheckerOptions {
                        module: tsz_common::common::ModuleKind::ESNext,
                        ..CheckerOptions::default()
                    };
                    let plan = ParallelCheckPlan::build(
                        &program,
                        &options,
                        &[],
                        ProjectSemanticOptions::declaration_scoped(),
                        Some(Arc::new(
                            build_project_module_resolution_outcomes(&program, &resolutions)
                                .unwrap(),
                        )),
                        None,
                    )
                    .unwrap();
                    let result = plan.check_file_partition(0, &program.files[..1]).unwrap();
                    assert_eq!(
                        result[0]
                            .diagnostics
                            .iter()
                            .map(|diagnostic| diagnostic.code)
                            .collect::<Vec<_>>(),
                        expected,
                        "external={external}, receiver={receiver}, indexed={indexed}, result={return_type}: {:?}",
                        result[0].diagnostics,
                    );
                }
            }
        }
    }
}

#[test]
fn selected_namespace_runtime_surface_excludes_named_and_wildcard_type_only_edges() {
    for wildcard in [false, true] {
        let clause = if wildcard {
            "export type *"
        } else {
            "export type { value }"
        };
        let files = vec![
            ("src/main.ts".to_owned(), "import * as observed from 'pkg'; export function read(): number { return observed.value; }".to_owned()),
            ("ambient.d.ts".to_owned(), format!("declare module 'pkg' {{ {clause} from './owner'; }}")),
            ("src/owner.ts".to_owned(), "export const value: number = 42;".to_owned()),
        ];
        let resolutions = [
            ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "pkg".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::External {
                    identity: "admitted-ambient:pkg".to_owned(),
                },
            },
            ProjectModuleResolution {
                importer_path: "ambient.d.ts".to_owned(),
                specifier: "./owner".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmReExport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::File {
                    path: "src/owner.ts".to_owned(),
                },
            },
        ];
        assert_cold_request_semantics(files, &resolutions, &[2339], None);
    }
}


#[test]
fn selected_external_namespace_keeps_only_its_own_augmentation_members() {
    for (property, result_type, codes) in [
        ("value", "string", vec![]),
        ("added", "number", vec![]),
        ("foreignOnly", "number", vec![2339]),
    ] {
        let files = vec![
            ("src/main.ts".to_owned(), format!("import * as observed from 'pkg'; export function read(): {result_type} {{ return observed.{property}; }}")),
            ("ambient.d.ts".to_owned(), "declare module 'pkg' { export const value: string; } declare module 'foreign' {}".to_owned()),
            ("augment.ts".to_owned(), "export {}; declare module 'pkg' { export const added: number; } declare module 'foreign' { export const foreignOnly: number; }".to_owned()),
            ("src/wrong.ts".to_owned(), "export const value: number = 42; export const foreignOnly: number = 1;".to_owned()),
        ];
        let resolutions = [
            ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "pkg".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::External {
                    identity: "admitted-ambient:pkg".to_owned(),
                },
            },
            ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "pkg".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmReExport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::File {
                    path: "src/wrong.ts".to_owned(),
                },
            },
        ];
        // The established augmentation contract admits the exact added member;
        // it does not turn another module's declarations into namespace values.
        let program = compile_files_with_libs(files, &[]);
        let options = CheckerOptions {
            module: tsz_common::common::ModuleKind::ESNext,
            ..CheckerOptions::default()
        };
        let plan = ParallelCheckPlan::build(
            &program,
            &options,
            &[],
            ProjectSemanticOptions::declaration_scoped(),
            Some(Arc::new(
                build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
            )),
            None,
        )
        .unwrap();
        let result = plan.check_file_partition(0, &program.files[..1]).unwrap();
        assert_eq!(
            result[0]
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            codes,
            "property={property}: {:?}",
            result[0].diagnostics,
        );
    }
}

#[test]
fn selected_file_namespace_default_keeps_node_cjs_interop_against_competing_kind() {
    use crate::checker::context::ResolutionModeOverride;
    for namespace in [false, true] {
        for (result_type, codes) in [("number", vec![]), ("string", vec![2322])] {
            // Genuine TS5.9.3 refuses a namespace import of an export= class
            // (TS2497), even with interop. Keep its legal default import and a
            // legal object namespace import as distinct exact-request consumers.
            let import = if namespace {
                "* as observed"
            } else {
                "observed"
            };
            let cjs = if namespace {
                "declare const api: { value: number; default: string; }; export = api;"
            } else {
                "declare class api { static value: number; static default: string; } export = api;"
            };
            let mut program = compile_files_with_libs(
                vec![
                    (
                        "src/main.mts".to_owned(),
                        format!(
                            "import {import} from './cjs'; export function read(): {result_type} {{ return observed.value; }}"
                        ),
                    ),
                    ("src/cjs.cts".to_owned(), cjs.to_owned()),
                    (
                        "src/wrong.mts".to_owned(),
                        "export default 'wrong';".to_owned(),
                    ),
                ],
                &[],
            );
            // Explicit extensions supply these producer format facts; there is no
            // package-format inference for a plain .ts file in this control.
            program.file_is_esm_map = Arc::new(
                [
                    ("src/main.mts".to_owned(), true),
                    ("src/cjs.cts".to_owned(), false),
                    ("src/wrong.mts".to_owned(), true),
                ]
                .into_iter()
                .collect(),
            );
            let resolutions = [
                ProjectModuleResolution {
                    importer_path: "src/main.mts".to_owned(),
                    specifier: "./cjs".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmImport,
                    resolution_mode: Some(ResolutionModeOverride::Import),
                    target: ProjectModuleResolutionTarget::File {
                        path: "src/cjs.cts".to_owned(),
                    },
                },
                ProjectModuleResolution {
                    importer_path: "src/main.mts".to_owned(),
                    specifier: "./cjs".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmReExport,
                    resolution_mode: Some(ResolutionModeOverride::Import),
                    target: ProjectModuleResolutionTarget::File {
                        path: "src/wrong.mts".to_owned(),
                    },
                },
            ];
            let options = CheckerOptions {
                module: tsz_common::common::ModuleKind::NodeNext,
                es_module_interop: true,
                allow_synthetic_default_imports: true,
                ..CheckerOptions::default()
            };
            let plan = ParallelCheckPlan::build(
                &program,
                &options,
                &[],
                ProjectSemanticOptions::declaration_scoped(),
                Some(Arc::new(
                    build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
                )),
                None,
            )
            .unwrap();
            let result = plan.check_file_partition(0, &program.files[..1]).unwrap();
            assert_eq!(
                result[0]
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.code)
                    .collect::<Vec<_>>(),
                codes,
                "namespace={namespace} result={result_type}: {:?}",
                result[0].diagnostics,
            );
        }
    }
}

#[test]
fn selected_file_class_namespace_import_is_refused_with_and_without_interop() {
    use crate::checker::context::ResolutionModeOverride;
    for interop in [false, true] {
        // SDK TS5.9.3 reports TS2497 for this exact illegal class namespace
        // import under both options. Its additional property diagnostic is
        // not an assertion of complete TSZ diagnostic parity in this control.
        let mut program = compile_files_with_libs(
            vec![
                ("src/main.mts".to_owned(),
                    "import * as observed from './cjs'; export function read(): number { return observed.default.value; }".to_owned()),
                ("src/cjs.cts".to_owned(),
                    "declare class api { static value: number; static default: string; } export = api;".to_owned()),
                ("src/wrong.mts".to_owned(), "export default 'wrong';".to_owned()),
            ], &[],
        );
        program.file_is_esm_map = Arc::new(
            [
                ("src/main.mts".to_owned(), true),
                ("src/cjs.cts".to_owned(), false),
                ("src/wrong.mts".to_owned(), true),
            ]
            .into_iter()
            .collect(),
        );
        let resolutions = [
            ProjectModuleResolution {
                importer_path: "src/main.mts".to_owned(),
                specifier: "./cjs".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: Some(ResolutionModeOverride::Import),
                target: ProjectModuleResolutionTarget::File {
                    path: "src/cjs.cts".to_owned(),
                },
            },
            ProjectModuleResolution {
                importer_path: "src/main.mts".to_owned(),
                specifier: "./cjs".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmReExport,
                resolution_mode: Some(ResolutionModeOverride::Import),
                target: ProjectModuleResolutionTarget::File {
                    path: "src/wrong.mts".to_owned(),
                },
            },
        ];
        let options = CheckerOptions {
            module: tsz_common::common::ModuleKind::NodeNext,
            es_module_interop: interop,
            allow_synthetic_default_imports: interop,
            ..CheckerOptions::default()
        };
        let plan = ParallelCheckPlan::build(
            &program,
            &options,
            &[],
            ProjectSemanticOptions::declaration_scoped(),
            Some(Arc::new(
                build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
            )),
            None,
        )
        .unwrap();
        let checked = plan.check_file_partition(0, &program.files[..1]).unwrap();
        let codes = checked[0]
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert!(
            codes.contains(&2497),
            "exact selected CJS class namespace is refused, interop={interop}: {:?}",
            checked[0].diagnostics
        );
        assert!(
            !codes.contains(&2307),
            "refusal is the admitted CJS shape, not missing module resolution: {:?}",
            checked[0].diagnostics
        );
    }
}

#[test]
fn nested_ambient_reexport_owner_cannot_borrow_foreign_owner_edge() {
    use crate::checker::context::ResolutionModeOverride;
    for wildcard in [false, true] {
        for admitted in [false, true] {
            let clause = if wildcard {
                "export type *"
            } else {
                "export type { Shape }"
            };
            let files = vec![
                ("src/main.ts".to_owned(), "import type { Shape } from 'pkg'; declare const observed: Shape; export function read(): number { return observed.value; }".to_owned()),
                ("ambient.d.ts".to_owned(), format!("declare module 'foreign' {{ {clause} from 'dep' with {{ 'resolution-mode': 'require' }}; }} declare module 'pkg' {{ {clause} from 'dep' with {{ 'resolution-mode': 'import' }}; }}")),
                ("src/owner.ts".to_owned(), "export interface Shape { value: number }".to_owned()),
                ("src/wrong.ts".to_owned(), "export interface Shape { value: string }".to_owned()),
            ];
            let resolutions = [
                ProjectModuleResolution {
                    importer_path: "src/main.ts".to_owned(),
                    specifier: "pkg".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmImport,
                    resolution_mode: None,
                    target: ProjectModuleResolutionTarget::External {
                        identity: "admitted-ambient:pkg".to_owned(),
                    },
                },
                ProjectModuleResolution {
                    importer_path: "ambient.d.ts".to_owned(),
                    specifier: "dep".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmReExport,
                    resolution_mode: Some(ResolutionModeOverride::Import),
                    target: if admitted {
                        ProjectModuleResolutionTarget::File {
                            path: "src/owner.ts".to_owned(),
                        }
                    } else {
                        ProjectModuleResolutionTarget::Unresolved
                    },
                },
                ProjectModuleResolution {
                    importer_path: "ambient.d.ts".to_owned(),
                    specifier: "dep".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmReExport,
                    resolution_mode: Some(ResolutionModeOverride::Require),
                    target: ProjectModuleResolutionTarget::File {
                        path: "src/wrong.ts".to_owned(),
                    },
                },
            ];
            assert_cold_request_semantics_and_owner(
                files,
                &resolutions,
                if !admitted && wildcard { &[2305] } else { &[] },
                admitted.then_some(TypeId::NUMBER),
                (!admitted).then_some((1, &[2307][..])),
            );
        }
    }
}

#[test]
fn selected_file_and_refused_request_dominate_shorthand_ambient_module() {
    for admitted in [false, true] {
        for (return_type, codes) in [("number", vec![]), ("string", vec![2322])] {
            let files = vec![
                (
                    "src/main.ts".to_owned(),
                    format!(
                        "import * as observed from 'dep'; export function read(): {return_type} {{ return observed.value; }}"
                    ),
                ),
                (
                    "src/owner.ts".to_owned(),
                    "export const value: number = 42;".to_owned(),
                ),
                (
                    "ambient.d.ts".to_owned(),
                    "declare module 'dep';".to_owned(),
                ),
            ];
            let resolutions = [ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "dep".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: if admitted {
                    ProjectModuleResolutionTarget::File {
                        path: "src/owner.ts".to_owned(),
                    }
                } else {
                    ProjectModuleResolutionTarget::Unresolved
                },
            }];
            assert_cold_request_semantics(
                files,
                &resolutions,
                if admitted { &codes } else { &[2307] },
                admitted.then_some(TypeId::NUMBER),
            );
        }
    }
}

#[test]
fn external_export_equals_hydrates_only_the_exact_nested_import_type() {
    for (return_type, codes) in [("number", vec![]), ("string", vec![2322])] {
        let files = vec![
            (
                "src/main.ts".to_owned(),
                format!(
                    "import * as observed from 'pkg'; export function read(): {return_type} {{ return observed.value; }}"
                ),
            ),
            (
                "ambient.d.ts".to_owned(),
                "declare module 'pkg' { const api: typeof import('./owner'); export = api; }"
                    .to_owned(),
            ),
            (
                "src/owner.ts".to_owned(),
                "export const value: number = 42;".to_owned(),
            ),
            (
                "src/wrong.ts".to_owned(),
                "export const value: string = 'wrong';".to_owned(),
            ),
        ];
        let resolutions = [
            ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "pkg".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::External {
                    identity: "admitted-ambient:pkg".to_owned(),
                },
            },
            ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "pkg".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmReExport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::File {
                    path: "src/wrong.ts".to_owned(),
                },
            },
            ProjectModuleResolution {
                importer_path: "ambient.d.ts".to_owned(),
                specifier: "./owner".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::File {
                    path: "src/owner.ts".to_owned(),
                },
            },
            ProjectModuleResolution {
                importer_path: "ambient.d.ts".to_owned(),
                specifier: "./owner".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmReExport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::File {
                    path: "src/wrong.ts".to_owned(),
                },
            },
        ];
        assert_cold_request_semantics(files, &resolutions, &codes, Some(TypeId::NUMBER));
    }
}

#[test]
fn malformed_named_edge_cannot_borrow_a_valid_same_specifier_sibling() {
    use crate::checker::context::ResolutionModeOverride;
    let files = vec![
        ("src/main.ts".to_owned(), "import type { Shape } from './barrel'; declare const observed: Shape; export function read(): number { return observed.value; }".to_owned()),
        ("src/barrel.ts".to_owned(), "export type { Shape } from 'dep' with { 'resolution-mode': 'invalid' }; export type { Other } from 'dep' with { 'resolution-mode': 'require' };".to_owned()),
        ("src/wrong.ts".to_owned(), "export interface Shape { value: number } export interface Other { value: number }".to_owned()),
    ];
    let resolutions = [
        ProjectModuleResolution {
            importer_path: "src/main.ts".to_owned(),
            specifier: "./barrel".to_owned(),
            request_kind: ProjectModuleRequestKind::EsmImport,
            resolution_mode: None,
            target: ProjectModuleResolutionTarget::File {
                path: "src/barrel.ts".to_owned(),
            },
        },
        ProjectModuleResolution {
            importer_path: "src/barrel.ts".to_owned(),
            specifier: "dep".to_owned(),
            request_kind: ProjectModuleRequestKind::EsmReExport,
            resolution_mode: Some(ResolutionModeOverride::Require),
            target: ProjectModuleResolutionTarget::File {
                path: "src/wrong.ts".to_owned(),
            },
        },
    ];
    // The public alias remains captured, but its malformed declaration cannot
    // lend the valid sibling's native member semantics to the consumer.
    assert_cold_request_semantics(files, &resolutions, &[], None);
}

#[test]
fn implicit_node_next_edge_cannot_borrow_explicit_sibling_mode() {
    use crate::checker::context::ResolutionModeOverride;
    use tsz_common::common::ModuleKind;
    for reexport in [false, true] {
        let edge = if reexport {
            "export type"
        } else {
            "import type"
        };
        let owner = format!(
            "{edge} {{ Shape }} from 'dep'; {edge} {{ Other }} from 'dep' with {{ 'resolution-mode': 'require' }};"
        );
        let main = if reexport {
            "import type { Shape } from './barrel' with { 'resolution-mode': 'import' };"
        } else {
            ""
        };
        let usage = "declare const observed: Shape; export function read(): number { return observed.value; }";
        let files = if reexport {
            vec![
                ("src/main.ts".to_owned(), format!("{main} {usage}")),
                ("src/barrel.ts".to_owned(), owner),
            ]
        } else {
            vec![("src/main.ts".to_owned(), format!("{owner} {usage}"))]
        };
        let mut files = files;
        files.push((
            "src/dep.ts".to_owned(),
            "export interface Shape { value: number } export interface Other { value: string }"
                .to_owned(),
        ));
        let program = compile_files_with_libs(files, &[]);
        let owner_idx = usize::from(reexport);
        let mut resolutions = vec![ProjectModuleResolution {
            importer_path: program.files[owner_idx].file_name.clone(),
            specifier: "dep".to_owned(),
            request_kind: if reexport {
                ProjectModuleRequestKind::EsmReExport
            } else {
                ProjectModuleRequestKind::EsmImport
            },
            resolution_mode: Some(ResolutionModeOverride::Require),
            target: ProjectModuleResolutionTarget::File {
                path: "src/dep.ts".to_owned(),
            },
        }];
        if reexport {
            resolutions.push(ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "./barrel".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: Some(ResolutionModeOverride::Import),
                target: ProjectModuleResolutionTarget::File {
                    path: "src/barrel.ts".to_owned(),
                },
            });
        }
        let options = CheckerOptions {
            module: ModuleKind::NodeNext,
            ..CheckerOptions::default()
        };
        let plan = ParallelCheckPlan::build(
            &program,
            &options,
            &[],
            ProjectSemanticOptions::declaration_scoped(),
            Some(Arc::new(
                build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
            )),
            None,
        )
        .unwrap();
        // Query the consumer before checking the owner: a sole explicit sibling
        // is not per-file format evidence for the missing implicit request.
        let file = &program.files[0];
        let usage = file
            .arena
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(raw, node)| {
                file.arena
                    .get_identifier(node)
                    .filter(|name| name.escaped_text == "value")
                    .map(|_| NodeIndex(raw as u32))
            })
            .max_by_key(|&idx| file.arena.get(idx).unwrap().pos)
            .unwrap();
        let usage = file.arena.parent_of(usage).unwrap();
        let observed = plan.with_bound_file_checker_and_types(0, file, |checker, _, _, _| {
            checker.get_type_of_node(usage)
        });
        assert_ne!(
            observed,
            TypeId::NUMBER,
            "implicit edge borrowed explicit sibling"
        );
        let owner = plan
            .check_file_partition(owner_idx, &program.files[owner_idx..owner_idx + 1])
            .unwrap();
        assert_eq!(
            owner[0]
                .diagnostics
                .iter()
                .map(|d| d.code)
                .collect::<Vec<_>>(),
            vec![2307]
        );
    }
}

#[test]
fn import_type_and_typeof_member_chains_keep_exact_request_authority() {
    use crate::checker::context::ResolutionModeOverride;
    for (syntax, mode) in [
        (
            "type Shape = import('dep').Shape; declare const observed: Shape;",
            None,
        ),
        (
            "declare const observed: { value: typeof import('dep').value };",
            None,
        ),
        ("declare const observed: typeof import('dep').Foo;", None),
        (
            "type Shape = import('dep', { with: { 'resolution-mode': 'require' } }).Shape; declare const observed: Shape;",
            Some(ResolutionModeOverride::Require),
        ),
        (
            "declare const observed: { value: typeof import('dep', { with: { 'resolution-mode': 'require' } }).value };",
            Some(ResolutionModeOverride::Require),
        ),
        (
            "declare const observed: typeof import('dep', { with: { 'resolution-mode': 'require' } }).Foo;",
            Some(ResolutionModeOverride::Require),
        ),
    ] {
        for state in 0..5 {
            for (result, positive_codes) in [("number", vec![]), ("string", vec![2322])] {
                let files = vec![
                    ("src/main.ts".to_owned(), format!("{syntax} export function read(): {result} {{ return observed.value; }}")),
                    ("src/owner.ts".to_owned(), "export interface Shape { value: number } export const value: number = 42; export class Foo { static value: number = 42; }".to_owned()),
                    ("src/wrong.ts".to_owned(), "export interface Shape { value: string } export const value: string = 'wrong'; export class Foo { static value: string = 'wrong'; }".to_owned()),
                ];
                let mut resolutions = vec![ProjectModuleResolution {
                    importer_path: "src/main.ts".to_owned(),
                    specifier: "dep".to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmReExport,
                    resolution_mode: None,
                    target: ProjectModuleResolutionTarget::File {
                        path: "src/wrong.ts".to_owned(),
                    },
                }];
                if state != 2 {
                    resolutions.push(ProjectModuleResolution {
                        importer_path: "src/main.ts".to_owned(),
                        specifier: "dep".to_owned(),
                        request_kind: ProjectModuleRequestKind::EsmImport,
                        resolution_mode: if mode.is_some() {
                            None
                        } else {
                            Some(ResolutionModeOverride::Require)
                        },
                        target: ProjectModuleResolutionTarget::File {
                            path: "src/wrong.ts".to_owned(),
                        },
                    });
                }
                if state != 2 && state != 4 {
                    resolutions.push(ProjectModuleResolution {
                        importer_path: "src/main.ts".to_owned(),
                        specifier: "dep".to_owned(),
                        request_kind: if state == 3 {
                            ProjectModuleRequestKind::CjsRequire
                        } else {
                            ProjectModuleRequestKind::EsmImport
                        },
                        resolution_mode: mode,
                        target: if state == 1 {
                            ProjectModuleResolutionTarget::Unresolved
                        } else {
                            ProjectModuleResolutionTarget::File {
                                path: "src/owner.ts".to_owned(),
                            }
                        },
                    });
                }
                assert_cold_request_semantics(
                    files,
                    &resolutions,
                    if state == 0 { &positive_codes } else { &[2307] },
                    (state == 0).then_some(TypeId::NUMBER),
                );
            }
        }
    }
}

#[test]
fn external_wildcard_owner_cannot_lend_same_spelling_program_file_members() {
    for namespace in [false, true] {
        let import = if namespace {
            "import * as observed from 'src/pkg.ts';"
        } else {
            "import { value } from 'src/pkg.ts'; const observed = { value };"
        };
        let files = vec![
            (
                "src/main.ts".to_owned(),
                format!("{import} export function read(): number {{ return observed.value; }}"),
            ),
            (
                "src/pkg.ts".to_owned(),
                "export const value: number = 42;".to_owned(),
            ),
            (
                "ambient.d.ts".to_owned(),
                "declare module 'src/*' { export const present: string; }".to_owned(),
            ),
        ];
        let resolutions = [
            ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "src/pkg.ts".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmImport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::External {
                    identity: "admitted-ambient:src/*".to_owned(),
                },
            },
            ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "src/pkg.ts".to_owned(),
                request_kind: ProjectModuleRequestKind::EsmReExport,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::File {
                    path: "src/pkg.ts".to_owned(),
                },
            },
        ];
        assert_cold_request_semantics(
            files,
            &resolutions,
            if namespace { &[2339] } else { &[2305] },
            None,
        );
    }
}

#[test]
fn captured_file_and_ambient_owner_key_collision_refuses_reexport_rows() {
    for wildcard in [false, true] {
        let clause = if wildcard {
            "export type *"
        } else {
            "export type { Shape }"
        };
        let files = vec![
            ("src/main.ts".to_owned(), "import type { Shape } from './barrel'; declare const observed: Shape; export function read(): number { return observed.value; }".to_owned()),
            ("src/barrel.ts".to_owned(), format!("{clause} from 'dep'; declare module 'src/barrel.ts' {{ {clause} from 'other'; }}")),
            ("src/owner.ts".to_owned(), "export interface Shape { value: number }".to_owned()),
            ("src/wrong.ts".to_owned(), "export interface Shape { value: string }".to_owned()),
        ];
        let request =
            |importer: &str, specifier: &str, target: &str, kind| ProjectModuleResolution {
                importer_path: importer.to_owned(),
                specifier: specifier.to_owned(),
                request_kind: kind,
                resolution_mode: None,
                target: ProjectModuleResolutionTarget::File {
                    path: target.to_owned(),
                },
            };
        let resolutions = [
            request(
                "src/main.ts",
                "./barrel",
                "src/barrel.ts",
                ProjectModuleRequestKind::EsmImport,
            ),
            request(
                "src/barrel.ts",
                "dep",
                "src/owner.ts",
                ProjectModuleRequestKind::EsmReExport,
            ),
            request(
                "src/barrel.ts",
                "other",
                "src/wrong.ts",
                ProjectModuleRequestKind::EsmReExport,
            ),
        ];
        assert_cold_request_semantics(
            files,
            &resolutions,
            if wildcard { &[2305] } else { &[] },
            None,
        );
    }
}

#[test]
fn merged_external_ambient_owners_keep_all_exact_wildcard_edges() {
    for first_has_edge in [false, true] {
        for admitted in [false, true] {
            for (return_type, codes) in [("number", vec![]), ("string", vec![2322])] {
                let first_edge = if first_has_edge {
                    "export * from 'first';"
                } else {
                    ""
                };
                let files = vec![
                    (
                        "src/main.ts".to_owned(),
                        format!(
                            "import * as observed from 'pkg'; export function read(): {return_type} {{ return observed.value; }}"
                        ),
                    ),
                    (
                        "ambient-a.d.ts".to_owned(),
                        format!(
                            "declare module 'pkg' {{ export const marker: string; {first_edge} }}"
                        ),
                    ),
                    (
                        "ambient-b.d.ts".to_owned(),
                        "declare module 'pkg' { export * from 'second'; }".to_owned(),
                    ),
                    (
                        "src/first.ts".to_owned(),
                        "export const foreign: string = 'first';".to_owned(),
                    ),
                    (
                        "src/second.ts".to_owned(),
                        "export const value: number = 42;".to_owned(),
                    ),
                    (
                        "src/wrong.ts".to_owned(),
                        "export const value: string = 'wrong';".to_owned(),
                    ),
                ];
                let mut resolutions = vec![
                    ProjectModuleResolution {
                        importer_path: "src/main.ts".to_owned(),
                        specifier: "pkg".to_owned(),
                        request_kind: ProjectModuleRequestKind::EsmImport,
                        resolution_mode: None,
                        target: ProjectModuleResolutionTarget::External {
                            identity: "admitted-ambient:pkg".to_owned(),
                        },
                    },
                    ProjectModuleResolution {
                        importer_path: "ambient-b.d.ts".to_owned(),
                        specifier: "second".to_owned(),
                        request_kind: ProjectModuleRequestKind::EsmReExport,
                        resolution_mode: None,
                        target: if admitted {
                            ProjectModuleResolutionTarget::File {
                                path: "src/second.ts".to_owned(),
                            }
                        } else {
                            ProjectModuleResolutionTarget::Unresolved
                        },
                    },
                    ProjectModuleResolution {
                        importer_path: "ambient-b.d.ts".to_owned(),
                        specifier: "second".to_owned(),
                        request_kind: ProjectModuleRequestKind::EsmImport,
                        resolution_mode: None,
                        target: ProjectModuleResolutionTarget::File {
                            path: "src/wrong.ts".to_owned(),
                        },
                    },
                ];
                if first_has_edge {
                    resolutions.push(ProjectModuleResolution {
                        importer_path: "ambient-a.d.ts".to_owned(),
                        specifier: "first".to_owned(),
                        request_kind: ProjectModuleRequestKind::EsmReExport,
                        resolution_mode: None,
                        target: ProjectModuleResolutionTarget::File {
                            path: "src/first.ts".to_owned(),
                        },
                    });
                }
                assert_cold_request_semantics_and_owner(
                    files,
                    &resolutions,
                    if admitted { &codes } else { &[2339] },
                    admitted.then_some(TypeId::NUMBER),
                    (!admitted).then_some((2, &[2307][..])),
                );
            }
        }
    }
}

#[test]
fn merged_wildcard_rows_preserve_exact_lexical_edges_before_resolution() {
    let program = compile_files_with_libs(vec![
        ("a.d.ts".to_owned(), "declare module 'pkg' { export type * from 'dep' with { 'resolution-mode': 'import' }; export * from 'dep'; }".to_owned()),
        ("b.d.ts".to_owned(), "declare module 'pkg' { export type * from 'dep' with { 'resolution-mode': 'import' }; }".to_owned()),
    ], &[]);
    assert_eq!(
        program
            .wildcard_reexports
            .get("pkg")
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            ("dep".to_owned(), true),
            ("dep".to_owned(), false),
            ("dep".to_owned(), true)
        ]
    );
    let rows = program.wildcard_reexports.get("pkg").unwrap();
    let file_indices = rows
        .edges()
        .map(|edge| edge.declaration.unwrap().location.file_idx)
        .collect::<Vec<_>>();
    assert_eq!(file_indices, vec![0, 0, 1]);
    for edge in rows.edges() {
        let retained = edge.declaration.unwrap();
        let file = &program.files[retained.location.file_idx as usize];
        let node = file.arena.get(retained.declaration).unwrap();
        assert_eq!(
            (node.pos, node.end),
            (retained.location.pos, retained.location.end)
        );
        let export = file.arena.get_export_decl(node).unwrap();
        assert_eq!(export.is_type_only, edge.target.1);
        assert_eq!(
            file.arena.get_literal_text(export.module_specifier),
            Some(edge.target.0.as_str())
        );
        let owner = file.arena.get(retained.owner).unwrap();
        assert_eq!(
            (owner.pos, owner.end),
            (retained.owner_location.pos, retained.owner_location.end)
        );
        assert_eq!(retained.owner_location.file_idx, retained.location.file_idx);
    }
}

#[test]
fn nested_identifier_namespace_cannot_shift_ambient_wildcard_edge_mode() {
    use crate::checker::context::ResolutionModeOverride;
    for depth in [1, 160] {
        for admitted in [false, true] {
            for (return_type, codes) in [("number", vec![]), ("string", vec![2322])] {
                let nested = format!(
                    "{} export type * from 'bad' with {{ 'resolution-mode': 'require' }}; {}",
                    "namespace N {".repeat(depth),
                    "}".repeat(depth)
                );
                let files = vec![
                    (
                        "src/main.ts".to_owned(),
                        format!(
                            "import type {{ Shape }} from 'pkg'; declare const observed: Shape; export function read(): {return_type} {{ return observed.value; }}"
                        ),
                    ),
                    (
                        "ambient.d.ts".to_owned(),
                        format!(
                            "declare module 'pkg' {{ {nested} export type * from 'good' with {{ 'resolution-mode': 'import' }}; }}"
                        ),
                    ),
                    (
                        "src/owner.ts".to_owned(),
                        "export interface Shape { value: number }".to_owned(),
                    ),
                    (
                        "src/wrong.ts".to_owned(),
                        "export interface Shape { value: string }".to_owned(),
                    ),
                ];
                let request = |specifier: &str, mode, target| ProjectModuleResolution {
                    importer_path: "ambient.d.ts".to_owned(),
                    specifier: specifier.to_owned(),
                    request_kind: ProjectModuleRequestKind::EsmReExport,
                    resolution_mode: mode,
                    target,
                };
                let resolutions = [
                    ProjectModuleResolution {
                        importer_path: "src/main.ts".to_owned(),
                        specifier: "pkg".to_owned(),
                        request_kind: ProjectModuleRequestKind::EsmImport,
                        resolution_mode: None,
                        target: ProjectModuleResolutionTarget::External {
                            identity: "admitted-ambient:pkg".to_owned(),
                        },
                    },
                    request(
                        "good",
                        Some(ResolutionModeOverride::Import),
                        if admitted {
                            ProjectModuleResolutionTarget::File {
                                path: "src/owner.ts".to_owned(),
                            }
                        } else {
                            ProjectModuleResolutionTarget::Unresolved
                        },
                    ),
                    request(
                        "good",
                        Some(ResolutionModeOverride::Require),
                        ProjectModuleResolutionTarget::File {
                            path: "src/wrong.ts".to_owned(),
                        },
                    ),
                    request(
                        "bad",
                        Some(ResolutionModeOverride::Require),
                        ProjectModuleResolutionTarget::File {
                            path: "src/wrong.ts".to_owned(),
                        },
                    ),
                ];
                // The nested namespace's invalid edge is retained as a file
                // edge by the real binder. It cannot occupy the ambient row,
                // even before a later valid sibling or beyond 128 namespaces.
                assert_cold_request_semantics(
                    files,
                    &resolutions,
                    if admitted { &codes } else { &[2305] },
                    admitted.then_some(TypeId::NUMBER),
                );
            }
        }
    }
}

// These source forms are intentionally invalid export locations. The binder
// nevertheless visits their declarations: blocks, both IF branches, callable
// bodies, loops, switch clauses, try blocks, and labeled statements. Retained
// identity must select the actual following sibling independently of that walk.
const NESTED_WILDCARD_SOURCE_FORMS: &[(&str, &str)] = &[
    ("block", "{ EDGE }"),
    ("if_then", "if (true) { EDGE }"),
    ("if_else", "if (false) {} else { EDGE }"),
    ("function_body", "function nested() { EDGE }"),
    ("while_body", "while (false) { EDGE }"),
    ("switch_clause", "switch (0) { case 0: EDGE break; }"),
    ("try_block", "try { EDGE } catch {}"),
    ("labeled_block", "nested: { EDGE }"),
];

fn nested_wildcard_owner_source(form: &str, ambient: bool) -> String {
    let invalid = form.replace(
        "EDGE",
        "export type * from 'bad' with { 'resolution-mode': 'require' };",
    );
    let body =
        format!("{invalid} export type * from 'good' with {{ 'resolution-mode': 'import' }};");
    if ambient {
        format!("declare module 'pkg' {{ {body} }}")
    } else {
        body
    }
}

#[test]
fn nested_statement_wildcards_retain_actual_declaration_and_owner() {
    for &(name, form) in NESTED_WILDCARD_SOURCE_FORMS {
        for ambient in [false, true] {
            let program = compile_files_with_libs(
                vec![(
                    "src/barrel.ts".to_owned(),
                    nested_wildcard_owner_source(form, ambient),
                )],
                &[],
            );
            let owner_key = if ambient { "pkg" } else { "src/barrel.ts" };
            let rows = program.wildcard_reexports.get(owner_key).unwrap();
            assert_eq!(
                rows.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
                ["bad", "good"],
                "actual binder must retain both {name} declarations (ambient={ambient})"
            );
            let file = &program.files[0];
            let mut declarations = Vec::new();
            for edge in rows.edges() {
                let retained = edge.declaration.unwrap();
                assert_eq!(retained.location.file_idx, 0);
                assert_eq!(retained.owner_location.file_idx, 0);
                let node = file.arena.get(retained.declaration).unwrap();
                assert_eq!(
                    (node.pos, node.end),
                    (retained.location.pos, retained.location.end)
                );
                let export = file.arena.get_export_decl(node).unwrap();
                assert_eq!(
                    file.arena.get_literal_text(export.module_specifier),
                    Some(edge.target.0.as_str())
                );
                assert!(export.is_type_only);
                let owner = file.arena.get(retained.owner).unwrap();
                assert_eq!(
                    (owner.pos, owner.end),
                    (retained.owner_location.pos, retained.owner_location.end)
                );
                if ambient {
                    let module = file.arena.get_module(owner).unwrap();
                    assert_eq!(file.arena.get_literal_text(module.name), Some("pkg"));
                } else {
                    assert_eq!(owner.kind, syntax_kind_ext::SOURCE_FILE);
                }
                declarations.push(retained.declaration);
            }
            assert_ne!(declarations[0], declarations[1]);
        }
    }
}

#[test]
fn nested_statement_wildcards_cannot_retarget_following_sibling_request() {
    use crate::checker::context::ResolutionModeOverride;
    for &(_, form) in NESTED_WILDCARD_SOURCE_FORMS {
        for ambient in [false, true] {
            for admitted in [false, true] {
                for (return_type, codes) in [("number", vec![]), ("string", vec![2322])] {
                    let source = if ambient { "pkg" } else { "./barrel" };
                    let files = vec![
                        (
                            "src/main.ts".to_owned(),
                            format!(
                                "import type {{ Shape }} from '{source}'; declare const observed: Shape; export function read(): {return_type} {{ return observed.value; }}"
                            ),
                        ),
                        (
                            "src/barrel.ts".to_owned(),
                            nested_wildcard_owner_source(form, ambient),
                        ),
                        (
                            "src/owner.ts".to_owned(),
                            "export interface Shape { value: number }".to_owned(),
                        ),
                        (
                            "src/wrong.ts".to_owned(),
                            "export interface Shape { value: string }".to_owned(),
                        ),
                        (
                            "src/bad.ts".to_owned(),
                            "export interface ForeignShape { value: string }".to_owned(),
                        ),
                    ];
                    let request = |specifier: &str, mode, target| ProjectModuleResolution {
                        importer_path: "src/barrel.ts".to_owned(),
                        specifier: specifier.to_owned(),
                        request_kind: ProjectModuleRequestKind::EsmReExport,
                        resolution_mode: mode,
                        target,
                    };
                    let resolutions = [
                        ProjectModuleResolution {
                            importer_path: "src/main.ts".to_owned(),
                            specifier: source.to_owned(),
                            request_kind: ProjectModuleRequestKind::EsmImport,
                            resolution_mode: None,
                            target: if ambient {
                                ProjectModuleResolutionTarget::External {
                                    identity: "admitted-ambient:pkg".to_owned(),
                                }
                            } else {
                                ProjectModuleResolutionTarget::File {
                                    path: "src/barrel.ts".to_owned(),
                                }
                            },
                        },
                        request(
                            "good",
                            Some(ResolutionModeOverride::Import),
                            if admitted {
                                ProjectModuleResolutionTarget::File {
                                    path: "src/owner.ts".to_owned(),
                                }
                            } else {
                                ProjectModuleResolutionTarget::Unresolved
                            },
                        ),
                        request(
                            "good",
                            Some(ResolutionModeOverride::Require),
                            ProjectModuleResolutionTarget::File {
                                path: "src/wrong.ts".to_owned(),
                            },
                        ),
                        request(
                            "bad",
                            Some(ResolutionModeOverride::Require),
                            ProjectModuleResolutionTarget::File {
                                path: "src/bad.ts".to_owned(),
                            },
                        ),
                    ];
                    // Only the consumer is checked first: malformed owner syntax
                    // cannot lend a competitor or erase the admitted sibling.
                    assert_cold_request_semantics(
                        files,
                        &resolutions,
                        if admitted { &codes } else { &[2305] },
                        admitted.then_some(TypeId::NUMBER),
                    );
                }
            }
        }
    }
}

#[test]
fn selected_named_type_only_reexports_keep_runtime_and_type_meanings() {
    use crate::checker::context::ResolutionModeOverride;
    // A normal outer wrapper must not erase a type-only inner edge. The same
    // source chain is valid for type and typeof positions, including a class
    // that also has a runtime value. Check the consumer before any owner.
    for transitive in [false, true] {
        for external_dep in [false, true] {
            for runtime in [false, true] {
                let pkg = if transitive {
                    "export { value, Shape } from 'bridge';"
                } else {
                    "export type { value, Shape } from 'dep';"
                };
                let main = if runtime {
                    "import { value } from 'pkg'; export function read(): number { return value; }"
                } else {
                    "import { value, Shape } from 'pkg'; export type Scalar = typeof value; export type Constructor = typeof Shape; declare const observed: Shape; export function read(): number { return observed.value; }"
                };
                let files = vec![
                    ("src/main.ts".to_owned(), main.to_owned()),
                    ("ambient.d.ts".to_owned(), format!("declare module 'pkg' {{ {pkg} }} declare module 'bridge' {{ export type {{ value, Shape }} from 'dep'; }} declare module 'dep' {{ export const value: number; export class Shape {{ value: number; }} }}")),
                    ("src/owner.ts".to_owned(), "export const value: number = 1; export class Shape { value: number = 1; }".to_owned()),
                    ("src/wrong.ts".to_owned(), "export const value: string = 'foreign'; export class Shape { value: string = 'foreign'; }".to_owned()),
                ];
                let mut resolutions = vec![
                    selected_edge(
                        "src/main.ts",
                        "pkg",
                        ProjectModuleRequestKind::EsmImport,
                        selected_external("pkg"),
                    ),
                    selected_edge(
                        "src/main.ts",
                        "pkg",
                        ProjectModuleRequestKind::EsmReExport,
                        selected_file("src/wrong.ts"),
                    ),
                    selected_edge(
                        "ambient.d.ts",
                        "bridge",
                        ProjectModuleRequestKind::EsmReExport,
                        selected_external("bridge"),
                    ),
                    selected_edge(
                        "ambient.d.ts",
                        "dep",
                        ProjectModuleRequestKind::EsmReExport,
                        if external_dep {
                            selected_external("dep")
                        } else {
                            selected_file("src/owner.ts")
                        },
                    ),
                    selected_edge(
                        "ambient.d.ts",
                        "dep",
                        ProjectModuleRequestKind::CjsRequire,
                        selected_file("src/wrong.ts"),
                    ),
                ];
                let mut sibling = selected_edge(
                    "ambient.d.ts",
                    "dep",
                    ProjectModuleRequestKind::EsmReExport,
                    selected_file("src/wrong.ts"),
                );
                sibling.resolution_mode = Some(ResolutionModeOverride::Require);
                resolutions.push(sibling);
                if runtime {
                    assert_cold_selected_diagnostics(files, &resolutions, &[1362]);
                } else {
                    assert_cold_request_semantics(files, &resolutions, &[], Some(TypeId::NUMBER));
                }
            }
        }
    }
}

#[test]
fn selected_namespace_checks_transitive_default_type_only_wrapper() {
    for type_only in [false, true] {
        for captured in [false, true] {
            for (return_type, mismatch) in [("number", false), ("string", true)] {
                let receiver = if captured { "captured" } else { "observed" };
                let files = vec![
                    (
                        "src/main.ts".to_owned(),
                        format!(
                            "import * as observed from './b'; const captured = observed; export function read(): {return_type} {{ return {receiver}.A.value; }}"
                        ),
                    ),
                    (
                        "src/a.ts".to_owned(),
                        format!(
                            "class A {{ static value: number = 1; }} export {}{{ A as default }};",
                            if type_only { "type " } else { "" }
                        ),
                    ),
                    (
                        "src/b.ts".to_owned(),
                        "import A from './a'; export { A };".to_owned(),
                    ),
                    (
                        "src/wrong.ts".to_owned(),
                        "export class A { static value: string = 'foreign'; }".to_owned(),
                    ),
                ];
                let resolutions = [
                    selected_edge(
                        "src/main.ts",
                        "./b",
                        ProjectModuleRequestKind::EsmImport,
                        selected_file("src/b.ts"),
                    ),
                    selected_edge(
                        "src/main.ts",
                        "./b",
                        ProjectModuleRequestKind::EsmReExport,
                        selected_file("src/wrong.ts"),
                    ),
                    selected_edge(
                        "src/b.ts",
                        "./a",
                        ProjectModuleRequestKind::EsmImport,
                        selected_file("src/a.ts"),
                    ),
                    selected_edge(
                        "src/b.ts",
                        "./a",
                        ProjectModuleRequestKind::CjsRequire,
                        selected_file("src/wrong.ts"),
                    ),
                ];
                if type_only {
                    assert_cold_selected_diagnostics(files, &resolutions, &[2339]);
                } else {
                    assert_cold_request_semantics(
                        files,
                        &resolutions,
                        if mismatch { &[2322] } else { &[] },
                        Some(TypeId::NUMBER),
                    );
                }
            }
        }
    }
}

#[test]
fn selected_external_named_edges_keep_exact_member_and_cycle_identity() {
    // Actual literal owners share one declaration file. Dep's own direct
    // declaration wins before its named cycle; a same-key Require/File target
    // has conflicting types and an extra name that cannot fill a missing member.
    for style in ["type", "named", "namespace", "self_rename"] {
        for (return_type, mismatch) in [("number", false), ("string", true)] {
            let main = match style {
                "type" => format!(
                    "import type {{ Shape }} from 'pkg'; declare const observed: Shape; export function read(): {return_type} {{ return observed.value; }}"
                ),
                "named" => format!(
                    "import {{ value }} from 'pkg'; export function read(): {return_type} {{ return value; }}"
                ),
                "namespace" => format!(
                    "import * as observed from 'pkg'; export function read(): {return_type} {{ return observed.value; }}"
                ),
                _ => format!(
                    "import {{ renamed }} from 'pkg'; export function read(): {return_type} {{ return renamed; }}"
                ),
            };
            let files = vec![
                ("src/main.ts".to_owned(), main),
                ("ambient.d.ts".to_owned(), "declare module 'pkg' { export { Shape, value } from 'dep'; export { local as renamed } from 'pkg'; export const local: number; } declare module 'dep' { export interface Shape { value: number; } export const value: number; export { local } from 'pkg'; }".to_owned()),
                ("src/wrong.ts".to_owned(), "export interface Shape { value: string; } export const value: string = 'foreign'; export const renamed: string = 'foreign'; export const foreignOnly: number = 1;".to_owned()),
            ];
            let resolutions = [
                selected_edge(
                    "src/main.ts",
                    "pkg",
                    ProjectModuleRequestKind::EsmImport,
                    selected_external("pkg"),
                ),
                selected_edge(
                    "src/main.ts",
                    "pkg",
                    ProjectModuleRequestKind::CjsRequire,
                    selected_file("src/wrong.ts"),
                ),
                selected_edge(
                    "ambient.d.ts",
                    "dep",
                    ProjectModuleRequestKind::EsmReExport,
                    selected_external("dep"),
                ),
                selected_edge(
                    "ambient.d.ts",
                    "dep",
                    ProjectModuleRequestKind::CjsRequire,
                    selected_file("src/wrong.ts"),
                ),
                selected_edge(
                    "ambient.d.ts",
                    "pkg",
                    ProjectModuleRequestKind::EsmReExport,
                    selected_external("pkg"),
                ),
            ];
            assert_cold_selected_diagnostics(
                files,
                &resolutions,
                if mismatch { &[2322] } else { &[] },
            );
        }
    }
    // A cycle with no declaration is not a value or authority to borrow a
    // competing File row. Its type lookup remains neither primitive.
    let files = vec![
        ("src/main.ts".to_owned(), "import type { Shape } from 'pkg'; declare const observed: Shape; export function read(): number { return observed.value; }".to_owned()),
        ("ambient.d.ts".to_owned(), "declare module 'pkg' { export { Shape } from 'dep'; } declare module 'dep' { export { Shape } from 'pkg'; }".to_owned()),
        ("src/wrong.ts".to_owned(), "export interface Shape { value: number; }".to_owned()),
    ];
    let resolutions = [
        selected_edge(
            "src/main.ts",
            "pkg",
            ProjectModuleRequestKind::EsmImport,
            selected_external("pkg"),
        ),
        selected_edge(
            "ambient.d.ts",
            "dep",
            ProjectModuleRequestKind::EsmReExport,
            selected_external("dep"),
        ),
        selected_edge(
            "ambient.d.ts",
            "pkg",
            ProjectModuleRequestKind::EsmReExport,
            selected_external("pkg"),
        ),
        selected_edge(
            "ambient.d.ts",
            "dep",
            ProjectModuleRequestKind::CjsRequire,
            selected_file("src/wrong.ts"),
        ),
    ];
    assert_cold_request_semantics(files, &resolutions, &[], None);
}

#[test]
fn selected_external_named_edge_cannot_fill_holes_from_competing_file() {
    for namespace in [false, true] {
        let source = if namespace {
            "import * as observed from 'pkg'; export function read(): number { return observed.foreignOnly; }"
        } else {
            "import { foreignOnly } from 'pkg'; export function read(): number { return foreignOnly; }"
        };
        let files = vec![
            ("src/main.ts".to_owned(), source.to_owned()),
            ("ambient.d.ts".to_owned(), "declare module 'pkg' { export { value } from 'dep'; } declare module 'dep' { export const value: number; }".to_owned()),
            ("src/wrong.ts".to_owned(), "export const value: string = 'foreign'; export const foreignOnly: number = 1;".to_owned()),
        ];
        let resolutions = [
            selected_edge(
                "src/main.ts",
                "pkg",
                ProjectModuleRequestKind::EsmImport,
                selected_external("pkg"),
            ),
            selected_edge(
                "src/main.ts",
                "pkg",
                ProjectModuleRequestKind::CjsRequire,
                selected_file("src/wrong.ts"),
            ),
            selected_edge(
                "ambient.d.ts",
                "dep",
                ProjectModuleRequestKind::EsmReExport,
                selected_external("dep"),
            ),
            selected_edge(
                "ambient.d.ts",
                "dep",
                ProjectModuleRequestKind::CjsRequire,
                selected_file("src/wrong.ts"),
            ),
        ];
        assert_cold_selected_diagnostics(
            files,
            &resolutions,
            if namespace { &[2339] } else { &[2305] },
        );
    }
}

#[test]
fn selected_external_alias_query_stop_discards_output_and_latches_session() {
    struct ArmedStop(AtomicBool);
    impl tsz_common::ExecutionCheckpoint for ArmedStop {
        fn checkpoint(&self, _work: u64) -> Result<(), ProjectExecutionStop> {
            if self.0.swap(false, Ordering::AcqRel) {
                Err(ProjectExecutionStop::Cancelled)
            } else {
                Ok(())
            }
        }
    }
    let mut program = compile_files_with_libs(vec![
        ("src/main.ts".to_owned(), "import { value } from 'pkg'; export function read(): number { return value; }".to_owned()),
        ("ambient.d.ts".to_owned(), "declare module 'pkg' { export { value } from 'dep'; } declare module 'dep' { export const value: number; }".to_owned()),
    ], &[]);
    program
        .set_project_module_resolutions(&[
            selected_edge(
                "src/main.ts",
                "pkg",
                ProjectModuleRequestKind::EsmImport,
                selected_external("pkg"),
            ),
            selected_edge(
                "ambient.d.ts",
                "dep",
                ProjectModuleRequestKind::EsmReExport,
                selected_external("dep"),
            ),
        ])
        .unwrap();
    let options = CheckerOptions {
        module: tsz_common::common::ModuleKind::ESNext,
        ..CheckerOptions::default()
    };
    let stop = ArmedStop(AtomicBool::new(false));
    let session = ProjectCheckerSession::new(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::declaration_scoped(),
        &stop,
    )
    .unwrap();
    let usage = program.files[0]
        .arena
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(raw, node)| {
            program.files[0]
                .arena
                .get_identifier(node)
                .filter(|identifier| identifier.escaped_text == "value")
                .map(|_| NodeIndex(raw as u32))
        })
        .max_by_key(|&idx| program.files[0].arena.get(idx).unwrap().pos)
        .unwrap();
    let ran = AtomicBool::new(false);
    let result = session.with_file_checker_and_types(0, |checker, _, _, _| {
        ran.store(true, Ordering::Release);
        stop.0.store(true, Ordering::Release);
        checker.get_type_of_node(usage)
    });
    assert!(ran.load(Ordering::Acquire));
    assert!(matches!(
        result,
        Err(ProjectCheckerSessionError::ExecutionStopped(
            ProjectExecutionStop::Cancelled
        ))
    ));
    assert_eq!(
        tsz_common::ExecutionCheckpoint::checkpoint(&stop, 0),
        Ok(()),
        "underlying checkpoint is one-shot"
    );
    let later = session.with_file_checker(0, |_, _, _| {
        panic!("latched session must not invoke a later query")
    });
    assert!(matches!(
        later,
        Err(ProjectCheckerSessionError::ExecutionStopped(
            ProjectExecutionStop::Cancelled
        ))
    ));
}

#[test]
fn selected_import_equals_keeps_exact_type_only_export_assignment() {
    use crate::checker::context::ResolutionModeOverride;
    for external in [false, true] {
        for runtime in [false, true] {
            let main = if runtime {
                "import observed = require('pkg'); export const used = observed;"
            } else {
                "import observed = require('pkg'); export type Query = typeof observed;"
            };
            let files = vec![
                ("src/main.ts".to_owned(), main.to_owned()),
                ("src/owner.ts".to_owned(), "import type { value } from 'dep'; export = value;".to_owned()),
                ("ambient.d.ts".to_owned(), "declare module 'pkg' { import type { value } from 'dep'; export = value; } declare module 'dep' { export const value: number; }".to_owned()),
                ("src/wrong.ts".to_owned(), "const value: string = 'foreign'; export = value;".to_owned()),
            ];
            let mut exact = selected_edge(
                "src/main.ts",
                "pkg",
                ProjectModuleRequestKind::CjsRequire,
                if external {
                    selected_external("pkg")
                } else {
                    selected_file("src/owner.ts")
                },
            );
            exact.resolution_mode = Some(ResolutionModeOverride::Require);
            let resolutions = [
                exact,
                selected_edge(
                    "src/main.ts",
                    "pkg",
                    ProjectModuleRequestKind::EsmImport,
                    selected_file("src/wrong.ts"),
                ),
                selected_edge(
                    "src/owner.ts",
                    "dep",
                    ProjectModuleRequestKind::EsmImport,
                    selected_external("dep"),
                ),
                selected_edge(
                    "ambient.d.ts",
                    "dep",
                    ProjectModuleRequestKind::EsmImport,
                    selected_external("dep"),
                ),
            ];
            if !runtime {
                // Keep the original illegal module-format case as a negative
                // control instead of suppressing its genuine TS1202.
                assert_cold_selected_diagnostics(files.clone(), &resolutions, &[1202]);
            }
            assert_cold_selected_diagnostics_with_options(
                files,
                &resolutions,
                if runtime { &[1361] } else { &[] },
                CheckerOptions {
                    // Genuine TS5.9.3 emits TS1202 for this import-equals
                    // syntax under ESNext, but accepts its type query in CJS.
                    module: tsz_common::common::ModuleKind::CommonJS,
                    ..CheckerOptions::default()
                },
            );
        }
    }
}

#[test]
fn selected_negative_duplicate_import_cannot_supply_runtime_value() {
    for cyclic in [false, true] {
        let files = vec![
            ("src/main.ts".to_owned(), "import type { value } from 'pkg'; import { value } from 'other'; export const used = value;".to_owned()),
            ("ambient.d.ts".to_owned(), "declare module 'pkg' { export const value: number; } declare module 'other' { export { value } from 'other'; }".to_owned()),
            ("src/wrong.ts".to_owned(), "export const value: string = 'foreign';".to_owned()),
        ];
        let resolutions = [
            selected_edge(
                "src/main.ts",
                "pkg",
                ProjectModuleRequestKind::EsmImport,
                selected_external("pkg"),
            ),
            selected_edge(
                "src/main.ts",
                "other",
                ProjectModuleRequestKind::EsmImport,
                if cyclic {
                    selected_external("other")
                } else {
                    ProjectModuleResolutionTarget::Unresolved
                },
            ),
            selected_edge(
                "ambient.d.ts",
                "other",
                ProjectModuleRequestKind::EsmReExport,
                selected_external("other"),
            ),
            selected_edge(
                "src/main.ts",
                "other",
                ProjectModuleRequestKind::CjsRequire,
                selected_file("src/wrong.ts"),
            ),
        ];
        let program = compile_files_with_libs(files, &[]);
        let options = CheckerOptions {
            module: tsz_common::common::ModuleKind::ESNext,
            ..CheckerOptions::default()
        };
        let plan = ParallelCheckPlan::build(
            &program,
            &options,
            &[],
            ProjectSemanticOptions::declaration_scoped(),
            Some(Arc::new(
                build_project_module_resolution_outcomes(&program, &resolutions).unwrap(),
            )),
            None,
        )
        .unwrap();
        let checked = plan.check_file_partition(0, &program.files[..1]).unwrap();
        assert!(
            checked[0]
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == 1361),
            "negative ordinary import cannot erase actual import-type runtime refusal (cyclic={cyclic}): {:?}",
            checked[0].diagnostics
        );
        if !cyclic {
            assert!(
                checked[0]
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == 2307),
                "the exact unresolved request remains unresolved"
            );
        }
    }
}

fn selected_edge(
    importer: &str,
    source: &str,
    kind: ProjectModuleRequestKind,
    target: ProjectModuleResolutionTarget,
) -> ProjectModuleResolution {
    ProjectModuleResolution {
        importer_path: importer.to_owned(),
        specifier: source.to_owned(),
        request_kind: kind,
        resolution_mode: None,
        target,
    }
}

fn selected_external(identity: &str) -> ProjectModuleResolutionTarget {
    ProjectModuleResolutionTarget::External {
        identity: format!("admitted-ambient:{identity}"),
    }
}

fn selected_file(path: &str) -> ProjectModuleResolutionTarget {
    ProjectModuleResolutionTarget::File {
        path: path.to_owned(),
    }
}

fn assert_cold_selected_diagnostics(
    files: Vec<(String, String)>,
    resolutions: &[ProjectModuleResolution],
    codes: &[u32],
) {
    assert_cold_selected_diagnostics_with_options(
        files,
        resolutions,
        codes,
        CheckerOptions {
            module: tsz_common::common::ModuleKind::ESNext,
            ..CheckerOptions::default()
        },
    );
}

fn assert_cold_selected_diagnostics_with_options(
    files: Vec<(String, String)>,
    resolutions: &[ProjectModuleResolution],
    codes: &[u32],
    options: CheckerOptions,
) {
    let program = compile_files_with_libs(files, &[]);
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::declaration_scoped(),
        Some(Arc::new(
            build_project_module_resolution_outcomes(&program, resolutions).unwrap(),
        )),
        None,
    )
    .unwrap();
    let checked = plan.check_file_partition(0, &program.files[..1]).unwrap();
    assert_eq!(
        checked[0]
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>(),
        codes,
        "{:?}",
        checked[0].diagnostics
    );
}

#[test]
fn observed_partitions_preserve_diagnostics_and_exact_file_boundaries() {
    use std::sync::Mutex;
    use tsz_common::{
        ExecutionCheckpoint, ProjectExecutionEvent, ProjectExecutionPhase as P,
        ProjectExecutionTransition as T,
    };
    struct Observed<'a> {
        budget: ProjectExecutionBudget<'a>,
        rows: Mutex<Vec<(P, T, Option<(usize, String)>)>>,
    }
    impl ExecutionCheckpoint for Observed<'_> {
        fn checkpoint(&self, work: u64) -> Result<(), ProjectExecutionStop> {
            self.budget.checkpoint(work)
        }
        fn observe(&self, event: ProjectExecutionEvent<'_>) {
            self.rows.lock().unwrap().push((
                event.phase,
                event.transition,
                event.file.map(|file| (file.index, file.path.to_owned())),
            ));
        }
    }
    let cancelled = AtomicBool::new(false);
    let (baseline, libs, options, resolutions, semantic) = pristine_parity_fixture();
    let expected = check_files_with_project_inputs_and_checker_partitions(
        &baseline,
        &options,
        &libs,
        semantic,
        &resolutions,
        ProjectCheckerExecution {
            checkpoint: &execution_budget(&cancelled),
            workers: NonZeroUsize::new(2).unwrap(),
        },
    )
    .unwrap();
    let (program, libs, options, resolutions, semantic) = pristine_parity_fixture();
    let observer = Observed {
        budget: execution_budget(&cancelled),
        rows: Mutex::new(Vec::new()),
    };
    let actual = check_files_with_project_inputs_and_checker_partitions(
        &program,
        &options,
        &libs,
        semantic,
        &resolutions,
        ProjectCheckerExecution {
            checkpoint: &observer,
            workers: NonZeroUsize::new(2).unwrap(),
        },
    )
    .unwrap();
    assert_same_results(&actual, &expected);
    let rows = observer.rows.lock().unwrap();
    for (index, file) in program.files.iter().enumerate() {
        for phase in [
            P::FileReset,
            P::FileCheck,
            P::FilePostprocess,
            P::SourcePreparation,
            P::SourceStatements,
            P::SourcePostchecks,
        ] {
            for transition in [T::Entered, T::Returned] {
                assert_eq!(
                    rows.iter()
                        .filter(|(p, t, path)| *p == phase
                            && *t == transition
                            && path.as_ref() == Some(&(index, file.file_name.clone())))
                        .count(),
                    1,
                    "exact admitted file must expose each boundary once: {phase:?} {transition:?} {}",
                    file.file_name
                );
            }
        }
    }
    assert!(
        !rows
            .iter()
            .any(|(_, transition, _)| *transition == T::Unwound)
    );
}

#[test]
fn sealed_program_indexes_match_legacy_rows_and_share_across_workers_and_resets() {
    fn fixture(extra: &str) -> MergedProgram {
        let binds = parse_and_bind_parallel_with_libs(vec![
            ("src/owner.ts".to_owned(), "export interface Shape { left: number }".to_owned()),
            ("src/augment.ts".to_owned(), "import './owner'; declare module './owner' { interface Shape { right: string } }".to_owned()),
            ("src/ambient.d.ts".to_owned(), format!("declare module 'owned' {{ export interface Direct {{ value: string }} {extra} }}")),
        ], &[]);
        let refs = binds.iter().collect::<Vec<_>>();
        merge_bind_results_ref_with_project_semantic_options(
            &refs,
            ProjectSemanticOptions::declaration_scoped(),
        )
    }
    let program = fixture("");
    let options = CheckerOptions::default();
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::declaration_scoped(),
        None,
        None,
    )
    .unwrap();
    let cache = plan.make_query_cache();
    let mut actual = plan.configured_file_checker(0, &program.files[0], &cache);
    let mut legacy = CheckerState::with_options_and_shared_def_store(
        &program.files[0].arena,
        plan.all_binders[0].as_ref(),
        &cache,
        program.files[0].file_name.clone(),
        &options,
        Arc::clone(&program.definition_store),
    );
    legacy.ctx.set_all_arenas(Arc::clone(&plan.all_arenas));
    if let Some(modules) = plan.shared_declared_modules.as_ref() {
        legacy
            .ctx
            .set_declared_modules_from_skeleton(Arc::clone(modules));
    }
    legacy.ctx.set_all_binders(Arc::clone(&plan.all_binders));
    assert_eq!(
        actual.ctx.global_file_locals_index,
        legacy.ctx.global_file_locals_index
    );
    assert_eq!(
        actual.ctx.global_module_exports_index,
        legacy.ctx.global_module_exports_index
    );
    assert_eq!(
        actual.ctx.global_augmentation_targets_index,
        legacy.ctx.global_augmentation_targets_index
    );
    assert_eq!(
        actual.ctx.global_module_binder_index,
        legacy.ctx.global_module_binder_index
    );
    assert_eq!(actual.ctx.global_arena_index, legacy.ctx.global_arena_index);
    assert_eq!(
        actual.ctx.global_expando_index,
        legacy.ctx.global_expando_index
    );
    assert_eq!(actual.ctx.module_specifiers, legacy.ctx.module_specifiers);
    assert_eq!(
        actual.ctx.module_path_specifiers,
        legacy.ctx.module_path_specifiers
    );
    let augmentation_rows = |ctx: &crate::checker::context::CheckerContext<'_>| {
        let mut rows = ctx
            .global_module_augmentations_index
            .as_ref()
            .unwrap()
            .iter()
            .flat_map(|(specifier, rows)| {
                rows.iter().map(move |(file, aug)| {
                    (
                        specifier.clone(),
                        *file,
                        aug.name.clone(),
                        aug.node,
                        aug.arena.as_ref().map(|arena| Arc::as_ptr(arena) as usize),
                    )
                })
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| (row.0.clone(), row.1, row.2.clone(), row.3.0));
        rows
    };
    assert!(
        !augmentation_rows(&actual.ctx).is_empty(),
        "fixture must exercise actual augmentation owner rows"
    );
    assert_eq!(
        augmentation_rows(&actual.ctx),
        augmentation_rows(&legacy.ctx)
    );
    let other_cache = plan.make_query_cache();
    let other = plan.configured_file_checker(1, &program.files[1], &other_cache);
    assert!(Arc::ptr_eq(
        actual.ctx.global_module_exports_index.as_ref().unwrap(),
        other.ctx.global_module_exports_index.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        actual.ctx.global_file_locals_index.as_ref().unwrap(),
        other.ctx.global_file_locals_index.as_ref().unwrap()
    ));
    actual.ctx.switch_to_file(
        &program.files[1].arena,
        plan.all_binders[1].as_ref(),
        program.files[1].file_name.clone(),
        1,
    );
    assert!(Arc::ptr_eq(
        actual.ctx.global_module_exports_index.as_ref().unwrap(),
        other.ctx.global_module_exports_index.as_ref().unwrap()
    ));
    let changed = fixture("export const Later: number;");
    let changed_plan = ParallelCheckPlan::build(
        &changed,
        &options,
        &[],
        ProjectSemanticOptions::declaration_scoped(),
        None,
        None,
    )
    .unwrap();
    let changed_cache = changed_plan.make_query_cache();
    let changed_checker =
        changed_plan.configured_file_checker(0, &changed.files[0], &changed_cache);
    assert!(!Arc::ptr_eq(
        actual.ctx.global_module_exports_index.as_ref().unwrap(),
        changed_checker
            .ctx
            .global_module_exports_index
            .as_ref()
            .unwrap()
    ));
    assert!(
        changed_checker
            .ctx
            .global_module_exports_index
            .as_ref()
            .unwrap()
            .values()
            .any(|exports| exports.contains_key("Later"))
    );
    assert!(
        !actual
            .ctx
            .global_module_exports_index
            .as_ref()
            .unwrap()
            .values()
            .any(|exports| exports.contains_key("Later"))
    );
}

#[test]
fn configured_declaration_diagnostics_preserve_members_types_and_application_errors() {
    use tsz_common::options::checker::DeclarationDiagnosticPolicy;
    let declaration = "export interface Broken { missing: DoesNotExist; } export declare const value: { value: number };";
    for skip in [false, true] {
        let files = vec![
            (
                "src/main.ts".to_owned(),
                "import { value, Broken } from '../types/dep'; export type UsedBroken = Broken; export const wrong: string = value.value;"
                    .to_owned(),
            ),
            ("types/dep.d.ts".to_owned(), declaration.to_owned()),
        ];
        let resolutions = [ProjectModuleResolution {
            importer_path: "src/main.ts".to_owned(),
            specifier: "../types/dep".to_owned(),
            request_kind: ProjectModuleRequestKind::EsmImport,
            resolution_mode: None,
            target: ProjectModuleResolutionTarget::File {
                path: "types/dep.d.ts".to_owned(),
            },
        }];
        let options = CheckerOptions {
            declaration_diagnostics: if skip {
                DeclarationDiagnosticPolicy::SkipSemantic
            } else {
                DeclarationDiagnosticPolicy::Check
            },
            ..CheckerOptions::default()
        };
        let cancelled = AtomicBool::new(false);
        let fresh_program = compile_files_with_libs(files.clone(), &[]);
        let fresh = check_files_parallel_with_project_inputs_and_execution_checkpoint(
            &fresh_program,
            &options,
            &[],
            ProjectSemanticOptions::declaration_scoped(),
            &resolutions,
            &execution_budget(&cancelled),
        )
        .unwrap();
        let program = compile_files_with_libs(files, &[]);
        let partitioned = check_files_with_project_inputs_and_checker_partitions(
            &program,
            &options,
            &[],
            ProjectSemanticOptions::declaration_scoped(),
            &resolutions,
            ProjectCheckerExecution {
                checkpoint: &execution_budget(&cancelled),
                workers: NonZeroUsize::new(2).unwrap(),
            },
        )
        .unwrap();
        assert_same_results(&fresh, &partitioned);
        assert_eq!(
            partitioned.file_results.len(),
            2,
            "skipping diagnostics never removes a member"
        );
        let app = partitioned
            .file_results
            .iter()
            .find(|row| row.file_name == "src/main.ts")
            .unwrap();
        assert_eq!(
            app.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
            [2322],
            "application still consumes the declared NUMBER type: {:?}",
            app.diagnostics
        );
        let decl = partitioned
            .file_results
            .iter()
            .find(|row| row.file_name == "types/dep.d.ts")
            .unwrap();
        assert_eq!(
            decl.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
            if skip { vec![] } else { vec![2304] },
            "skipLibCheck={skip}: {:?}",
            decl.diagnostics
        );
        let source = program
            .files
            .iter()
            .find(|file| file.file_name == "types/dep.d.ts")
            .unwrap();
        assert!(
            source
                .arena
                .get_source_file_at(source.source_file)
                .unwrap()
                .is_declaration_file
        );
        assert_eq!(
            source
                .arena
                .get_source_file_at(source.source_file)
                .unwrap()
                .text
                .as_ref(),
            declaration
        );
    }
}

#[test]
fn owned_image_keeps_pristine_diagnostics_and_reuses_exact_views_for_queries() {
    use std::sync::Mutex;
    use tsz_common::{ExecutionCheckpoint, ProjectExecutionEvent, ProjectExecutionPhase, ProjectExecutionTransition};
    struct Observer<'a> {
        budget: &'a ProjectExecutionBudget<'a>,
        entries: Mutex<Vec<ProjectExecutionPhase>>,
    }
    impl ExecutionCheckpoint for Observer<'_> {
        fn checkpoint(&self, work: u64) -> Result<(), ProjectExecutionStop> {
            self.budget.checkpoint(work)
        }
        fn observe(&self, event: ProjectExecutionEvent<'_>) {
            if event.transition == ProjectExecutionTransition::Entered {
                self.entries.lock().unwrap().push(event.phase);
            }
        }
    }
    let (mut expected_program, libs, options, resolutions, semantic) = pristine_parity_fixture();
    expected_program.set_project_module_resolutions(&resolutions).unwrap();
    let cancelled = AtomicBool::new(false);
    let budget = execution_budget(&cancelled);
    let expected = check_files_parallel_with_project_inputs_and_execution_checkpoint(
        &expected_program, &options, &libs, semantic, &resolutions, &budget,
    ).unwrap();
    let expected_session = ProjectCheckerSession::new(&expected_program, &options, &libs, semantic, &budget).unwrap();
    let expected_a = expected_program.files.iter().position(|file| file.file_name == "src/a.ts").unwrap();
    let expected_type = expected_session.with_file_checker(expected_a, |checker, binder, _| {
        let symbol = binder.file_locals.get("a").unwrap();
        let ty = checker.get_type_of_symbol(symbol);
        checker.format_type(ty)
    }).unwrap();
    let (mut program, libs, options, resolutions, semantic) = pristine_parity_fixture();
    program.set_project_module_resolutions(&resolutions).unwrap();
    let observer = Observer { budget: &budget, entries: Mutex::new(Vec::new()) };
    let image = ProjectCheckerImage::new_with_checker_execution(
        program, options, &libs, semantic,
        ProjectCheckerExecution { checkpoint: &observer, workers: NonZeroUsize::new(2).unwrap() },
    ).unwrap();
    let original_views = Arc::clone(&image.views);
    let checked = image.check_with_partitions(ProjectCheckerExecution {
        checkpoint: &observer, workers: NonZeroUsize::new(2).unwrap(),
    }).unwrap();
    assert_same_results(&checked, &expected);
    let construction_phases = observer.entries.lock().unwrap().clone();
    for phase in [ProjectExecutionPhase::LibraryViews, ProjectExecutionPhase::BinderViews, ProjectExecutionPhase::SharedCheckerIndexes] {
        assert_eq!(construction_phases.iter().filter(|&&entry| entry == phase).count(), 1);
    }
    let a = image.files.iter().position(|file| file.file_name == "src/a.ts").unwrap();
    let mut observed_types = Vec::new();
    for _ in 0..3 {
        let session = image.query_session(&observer).unwrap();
        assert!(Arc::ptr_eq(&session.plan.views, &original_views));
        observed_types.push(session.with_file_checker(a, |checker, binder, _| {
            let symbol = binder.file_locals.get("a").expect("actual imported generic value");
            let ty = checker.get_type_of_symbol(symbol);
            checker.format_type(ty)
        }).unwrap());
    }
    assert!(observed_types.iter().all(|ty| ty == &expected_type));
    assert!(!observed_types[0].contains("unknown"), "actual imported generic type: {:?}", observed_types);
    let after_queries = observer.entries.lock().unwrap().clone();
    for phase in [ProjectExecutionPhase::LibraryViews, ProjectExecutionPhase::BinderViews, ProjectExecutionPhase::SharedCheckerIndexes, ProjectExecutionPhase::AugmentationPrime] {
        assert_eq!(after_queries.iter().filter(|&&entry| entry == phase).count(),
            construction_phases.iter().filter(|&&entry| entry == phase).count(),
            "query-session construction must not rebuild shared views or reprime augmentations");
    }
    let repeated = image.check_with_partitions(ProjectCheckerExecution {
        checkpoint: &observer, workers: NonZeroUsize::new(1).unwrap(),
    }).unwrap();
    assert_same_results(&repeated, &expected);
}

#[test]
fn owned_image_rejects_fresh_budget_after_actual_solver_stop() {
    struct OneShotStop(AtomicBool);
    impl tsz_common::ExecutionCheckpoint for OneShotStop {
        fn checkpoint(&self, _work: u64) -> Result<(), ProjectExecutionStop> {
            if self.0.swap(false, Ordering::AcqRel) { Err(ProjectExecutionStop::Cancelled) } else { Ok(()) }
        }
    }
    let mut program = compile_files_with_libs(
        vec![("src/main.ts".into(), "export const value = 1;".into())], &[],
    );
    program.set_project_module_resolutions(&[]).unwrap();
    let image = ProjectCheckerImage::new(program, CheckerOptions::default(), &[], ProjectSemanticOptions::structural(), None).unwrap();
    image.check_fresh(None).unwrap();
    let one_shot = OneShotStop(AtomicBool::new(false));
    let session = image.query_session(&one_shot).unwrap();
    let discarded = session.with_file_checker_and_types(0, |_, _, _, database| {
        one_shot.0.store(true, Ordering::Release);
        assert_eq!(database.union(vec![TypeId::NUMBER, TypeId::STRING]), TypeId::UNKNOWN);
        "this callback output must be discarded"
    });
    assert!(matches!(discarded, Err(ProjectCheckerSessionError::ExecutionStopped(ProjectExecutionStop::Cancelled))));
    assert!(!image.is_usable());
    let cancelled = AtomicBool::new(false);
    let fresh_budget = execution_budget(&cancelled);
    assert!(matches!(image.query_session(&fresh_budget), Err(ProjectCheckerSessionError::ExecutionStopped(ProjectExecutionStop::Cancelled))));
    assert!(matches!(image.check_with_partitions(ProjectCheckerExecution {
        checkpoint: &fresh_budget, workers: NonZeroUsize::new(1).unwrap(),
    }), Err(ProjectPartitionCheckError::Program(ProjectProgramCheckError::ExecutionStopped(ProjectExecutionStop::Cancelled)))));
    assert_eq!(fresh_budget.stop_reason(), None, "the old image stop must not poll a new lease");
}
