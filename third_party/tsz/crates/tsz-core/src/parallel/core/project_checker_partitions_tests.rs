use super::*;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tsz_common::{ProjectExecutionBudget, ProjectExecutionStop};

fn budget(cancelled: &AtomicBool) -> ProjectExecutionBudget<'_> {
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
    let name = "lib/custom.d.ts".to_owned();
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

#[test]
fn partitions_preserve_cross_file_identity_options_libraries_and_diagnostics() {
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
    let cancelled = AtomicBool::new(false);
    let expected = check_files_parallel_with_project_inputs_and_execution_checkpoint(
        &program,
        &options,
        &libs,
        semantic,
        &resolutions,
        &budget(&cancelled),
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
        let actual = check_files_with_project_inputs_and_checker_partitions(
            &program,
            &options,
            &libs,
            semantic,
            &resolutions,
            ProjectCheckerExecution {
                checkpoint: &budget(&cancelled),
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
        &budget(&cancelled),
    )
    .unwrap();
    assert!(
        expected.file_results[0]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == 2307)
    );
    let actual = check_files_with_project_inputs_and_checker_partitions(
        &program,
        &options,
        &[],
        semantic,
        &[unresolved.clone()],
        ProjectCheckerExecution {
            checkpoint: &budget(&cancelled),
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
                checkpoint: &budget(&cancelled),
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
        (budget(&cancelled), ProjectExecutionStop::Cancelled),
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
    let budget = budget(&cancelled);
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::structural(),
        Some(Arc::new(
            build_project_module_resolution_outcomes(&program, &[]).unwrap(),
        )),
        Some(&budget),
    );
    let mut completed = Vec::new();
    let result = plan.check_file_partition_observing_completion(0, &program.files, |index| {
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
