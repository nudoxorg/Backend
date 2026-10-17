use super::*;
use crate::context::{CheckerOptions, ProgramContext};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tsz_binder::BinderState;
use tsz_common::{ExecutionCheckpoint, ProjectExecutionStop};
use tsz_parser::parser::ParserState;
use tsz_solver::construction::{QueryCache, TypeInterner};

// The source stops once; the real QueryCache must retain that stop even when
// later caller polls would otherwise succeed.
struct StopAfter {
    armed: AtomicBool,
    calls: AtomicUsize,
    stop_at: usize,
}

impl StopAfter {
    fn new(stop_at: usize) -> Self {
        Self {
            armed: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            stop_at,
        }
    }

    fn arm(&self) {
        self.calls.store(0, Ordering::Relaxed);
        self.armed.store(true, Ordering::Release);
    }
}

impl ExecutionCheckpoint for StopAfter {
    fn checkpoint(&self, _work: u64) -> Result<(), ProjectExecutionStop> {
        if self.armed.load(Ordering::Acquire)
            && self.calls.fetch_add(1, Ordering::AcqRel) + 1 == self.stop_at
        {
            return Err(ProjectExecutionStop::Cancelled);
        }
        Ok(())
    }
}

fn with_program_checker(
    source: &str,
    checkpoint: &StopAfter,
    run: impl FnOnce(&mut CheckerState<'_>, NodeIndex),
) -> Vec<crate::diagnostics::Diagnostic> {
    let mut parser = ParserState::new("pass.ts".into(), source.into());
    let root = parser.parse_source_file();
    assert!(parser.get_diagnostics().is_empty());
    let mut binder = BinderState::new();
    binder.bind_source_file(parser.get_arena(), root);
    let arena = Arc::new(parser.into_arena());
    let binder = Arc::new(binder);
    let interner = TypeInterner::new();
    let queries = QueryCache::new(&interner).with_execution_checkpoint(checkpoint);
    let mut checker = CheckerState::new(
        &arena,
        &binder,
        &queries,
        "pass.ts".into(),
        CheckerOptions::default(),
    );
    let mut program = ProgramContext {
        all_arenas: Arc::new(vec![Arc::clone(&arena)]),
        all_binders: Arc::new(vec![Arc::clone(&binder)]),
        symbol_file_targets: Arc::new(binder.symbols.iter().map(|symbol| (symbol.id, 0)).collect()),
        project_module_resolution_outcomes: Some(Arc::new(Default::default())),
        ..ProgramContext::default()
    };
    program.build_global_indices();
    program.build_global_symbol_file_index();
    program.apply_to(&mut checker.ctx);
    checker.ctx.set_current_file_idx(0);
    for symbol in binder.symbols.iter() {
        assert_eq!(
            checker.ctx.resolve_symbol_declaring_file_index(symbol.id),
            Some(0)
        );
    }
    run(&mut checker, root);
    checker.ctx.diagnostics.clone()
}

#[test]
fn heritage_prepass_stops_inside_nested_namespace_walk() {
    let source = format!(
        "namespace Outer {{ {} }}",
        (0..64)
            .map(|n| format!("namespace N{n} {{ export interface Child {{ extra: string; }} }}"))
            .collect::<String>()
    );
    // Plain interfaces perform no type-query work: stopping this real recursive
    // pass therefore requires its traversal polls, not an incidental solver call.
    let stopped = StopAfter::new(5);
    with_program_checker(&source, &stopped, |checker, root| {
        let statements = checker
            .ctx
            .arena
            .get_source_file_at(root)
            .unwrap()
            .statements
            .nodes
            .clone();
        stopped.arm();
        checker.publish_heritage_interface_bodies(&statements);
        assert_eq!(
            checker.ctx.types.execution_checkpoint(0),
            Err(ProjectExecutionStop::Cancelled)
        );
        assert_eq!(
            stopped.calls.load(Ordering::Acquire),
            5,
            "latched stop prevents later traversal"
        );
    });
    assert_eq!(
        stopped.checkpoint(0),
        Ok(()),
        "the underlying source only stopped once"
    );

    let complete = StopAfter::new(usize::MAX);
    with_program_checker(&source, &complete, |checker, root| {
        let statements = checker
            .ctx
            .arena
            .get_source_file_at(root)
            .unwrap()
            .statements
            .nodes
            .clone();
        complete.arm();
        checker.publish_heritage_interface_bodies(&statements);
        assert_eq!(checker.ctx.types.execution_checkpoint(0), Ok(()));
    });
    assert!(
        complete.calls.load(Ordering::Acquire) > 64,
        "the uncancelled recursive pass visits the full fixture"
    );
}

#[test]
fn grammar_arena_sweeps_stop_before_late_diagnostics() {
    let padding = (0..32)
        .map(|n| format!("const v{n} = {n};"))
        .collect::<String>();
    for namespace in [false, true] {
        let suffix = if namespace {
            "namespace N { export as namespace Bad; }"
        } else {
            "let bad: unique symbol;"
        };
        let source = format!("{padding}{suffix}");
        let run = |checker: &mut CheckerState<'_>, root, checkpoint: &StopAfter| {
            let statements = checker
                .ctx
                .arena
                .get_source_file_at(root)
                .unwrap()
                .statements
                .nodes
                .clone();
            checkpoint.arm();
            if namespace {
                // Exercise the same production top-level walk directly: failure
                // must happen before any arena sweep can mask a missing poll.
                let top_level = checker.top_level_namespace_exports(&statements);
                if checkpoint.stop_at < statements.len() {
                    assert!(top_level.is_none(), "stop during top-level collection");
                    assert!(checker.ctx.diagnostics.is_empty());
                } else {
                    assert!(top_level.is_some(), "uncancelled collection completes");
                }
                checker.check_namespace_export_declaration_grammar(&statements, false);
            } else {
                checker.check_unique_symbol_grammar();
            }
        };
        let stopped = StopAfter::new(3);
        let partial = with_program_checker(&source, &stopped, |checker, root| {
            run(checker, root, &stopped);
            assert_eq!(
                checker.ctx.types.execution_checkpoint(0),
                Err(ProjectExecutionStop::Cancelled)
            );
            assert_eq!(stopped.calls.load(Ordering::Acquire), 3);
        });
        assert!(
            partial.is_empty(),
            "a stopped sweep cannot reach the late grammar violation"
        );
        let complete = StopAfter::new(usize::MAX);
        let diagnostics = with_program_checker(&source, &complete, |checker, root| {
            run(checker, root, &complete)
        });
        let code = if namespace { 1316 } else { 1332 };
        assert!(
            diagnostics.iter().any(|diagnostic| diagnostic.code == code),
            "uncancelled grammar pass must still emit {code}: {diagnostics:?}"
        );
        assert!(complete.calls.load(Ordering::Acquire) > 32);
    }
}
