use super::*;
use crate::checker::context::ResolutionModeOverride;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tsz_common::{ExecutionCheckpoint, ProjectExecutionStop};

struct LookupMeter {
    charged: AtomicU64,
    stop_once: AtomicBool,
}

impl LookupMeter {
    fn new() -> Self {
        Self {
            charged: AtomicU64::new(0),
            stop_once: AtomicBool::new(false),
        }
    }
}

impl ExecutionCheckpoint for LookupMeter {
    fn checkpoint(&self, work: u64) -> Result<(), ProjectExecutionStop> {
        self.charged.fetch_add(work, Ordering::Relaxed);
        if self.stop_once.swap(false, Ordering::AcqRel) {
            Err(ProjectExecutionStop::Cancelled)
        } else {
            Ok(())
        }
    }
}

fn requests() -> Vec<ProjectModuleResolution> {
    let mut rows = Vec::new();
    for kind in [
        ProjectModuleRequestKind::EsmImport,
        ProjectModuleRequestKind::DynamicImport,
        ProjectModuleRequestKind::CjsRequire,
        ProjectModuleRequestKind::EsmReExport,
    ] {
        for mode in [
            None,
            Some(ResolutionModeOverride::Import),
            Some(ResolutionModeOverride::Require),
        ] {
            rows.push(ProjectModuleResolution {
                importer_path: "src/main.ts".to_owned(),
                specifier: "pkg".to_owned(),
                request_kind: kind.clone(),
                resolution_mode: mode,
                target: ProjectModuleResolutionTarget::External {
                    identity: "admitted-ambient:pkg".to_owned(),
                },
            });
        }
    }
    rows
}

fn program(rows: &[ProjectModuleResolution]) -> MergedProgram {
    let mut program = compile_files_with_libs(
        vec![
            ("src/main.ts".to_owned(), "export {};".to_owned()),
            (
                "src/owner.ts".to_owned(),
                "export const value: number = 42;".to_owned(),
            ),
            (
                "ambient.d.ts".to_owned(),
                "declare module 'pkg' { export const value: number; }".to_owned(),
            ),
        ],
        &[],
    );
    program.set_project_module_resolutions(rows).unwrap();
    program
}

fn lookup(rows: &[ProjectModuleResolution]) -> (bool, Option<usize>, u64) {
    let program = program(rows);
    let options = CheckerOptions {
        module: tsz_common::common::ModuleKind::ESNext,
        ..CheckerOptions::default()
    };
    let meter = LookupMeter::new();
    let session = ProjectCheckerSession::new(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::declaration_scoped(),
        &meter,
    )
    .unwrap();
    session
        .with_file_checker(0, |checker, _, _| {
            let before = meter.charged.load(Ordering::Relaxed);
            let visible = checker
                .ctx
                .module_exports_for_module(checker.ctx.binder, "pkg")
                .is_some_and(|exports| exports.has("value"));
            let target = checker.ctx.resolve_import_target_from_file(0, "pkg");
            let work = meter.charged.load(Ordering::Relaxed) - before;
            (visible, target, work)
        })
        .unwrap()
}

#[test]
fn generic_module_consensus_preserves_every_kind_and_mode() {
    let baseline = requests();
    assert_eq!(baseline.len(), 12);
    let (visible, target, _) = lookup(&baseline);
    assert!(visible);
    assert_eq!(
        target, None,
        "an external consensus never invents a file target"
    );
    for index in 0..baseline.len() {
        for contrary in [
            ProjectModuleResolutionTarget::Unresolved,
            ProjectModuleResolutionTarget::File {
                path: "src/owner.ts".to_owned(),
            },
            ProjectModuleResolutionTarget::External {
                identity: "admitted-ambient:other".to_owned(),
            },
        ] {
            let mut rows = baseline.clone();
            rows[index].target = contrary;
            let (visible, target, _) = lookup(&rows);
            assert!(!visible, "kind/mode row {index} cannot be ignored");
            assert_eq!(target, None, "mixed requests do not select a file");
        }
    }
    let mut files = baseline;
    for row in &mut files {
        row.target = ProjectModuleResolutionTarget::File {
            path: "src/owner.ts".to_owned(),
        };
    }
    let (visible, target, _) = lookup(&files);
    assert!(!visible, "a file consensus cannot lend an ambient table");
    assert_eq!(target, Some(1));
}

#[test]
fn generic_module_consensus_charges_bounded_work_with_unrelated_requests() {
    let baseline = requests();
    let mut unrelated = baseline.clone();
    for index in 0..512 {
        let mut row = baseline[0].clone();
        row.specifier = format!("unrelated-{index}");
        unrelated.push(row);
    }
    let (visible, target, work) = lookup(&baseline);
    let (extra_visible, extra_target, extra_work) = lookup(&unrelated);
    assert!(visible && extra_visible);
    assert_eq!(target, extra_target);
    assert!(work > 0, "real lookup/hash work is charged to the caller");
    assert_eq!(
        work, extra_work,
        "unrelated rows cannot add whole-map lookup work"
    );
}

#[test]
fn canonical_module_lookup_stop_never_lends_exports_or_reopens_session() {
    let program = program(&requests());
    let options = CheckerOptions::default();
    let meter = LookupMeter::new();
    let session = ProjectCheckerSession::new(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::declaration_scoped(),
        &meter,
    )
    .unwrap();
    let stopped = session.with_file_checker(0, |checker, _, _| {
        meter.stop_once.store(true, Ordering::Release);
        assert!(checker
            .ctx
            .module_exports_for_module(checker.ctx.binder, "src/owner.ts")
            .is_none());
    });
    assert_eq!(
        meter.checkpoint(0),
        Ok(()),
        "underlying stop is really one-shot"
    );
    assert_eq!(
        stopped,
        Err(ProjectCheckerSessionError::ExecutionStopped(
            ProjectExecutionStop::Cancelled
        ))
    );
    let later: Result<(), ProjectCheckerSessionError> = session
        .with_file_checker(0, |_, _, _| panic!("stopped lookup scope cannot reopen"));
    assert_eq!(
        later,
        Err(ProjectCheckerSessionError::ExecutionStopped(
            ProjectExecutionStop::Cancelled
        ))
    );
}
