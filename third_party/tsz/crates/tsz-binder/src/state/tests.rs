use super::{BinderOptions, BinderState, LibContext, SemanticDefEntry, SemanticDefKind, core};
use crate::flow::{FlowNodeId, flow_flags};
use crate::scopes::ContainerKind;
use crate::{SymbolId, SymbolTable, symbol_flags};
use std::sync::Arc;
use tsz_common::common::ScriptTarget;
use tsz_common::{ProjectExecutionBudget, ProjectExecutionStop};
use tsz_parser::parser::{ParserState, node_flags, syntax_kind_ext};

mod enclosing_scope_memo;
mod exports_jsdoc;
mod hoisting_scopes_flow;
mod loop_flow;
mod module_augments;
mod module_detection;
mod semantic_defs_core;
mod semantic_defs_cross_file;
mod semantic_defs_extended;
mod state_storage;
mod symbols_and_flags;

#[test]
fn source_binding_returns_the_shared_typed_stop_reason() {
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    let mut parser = ParserState::new(
        "test.ts".to_string(),
        "const value = 1;".to_string(),
    );
    let root = parser.parse_source_file();
    let (arena, _) = parser.into_parts();
    let cancelled = AtomicBool::new(true);
    let budget = ProjectExecutionBudget::new(
        Instant::now() + Duration::from_secs(1),
        &cancelled,
        100,
    );
    let mut binder = BinderState::new();

    assert_eq!(
        binder.bind_source_file_with_execution_checkpoint(&arena, root, &budget),
        Err(ProjectExecutionStop::Cancelled),
    );
}

#[test]
fn source_binding_stops_while_traversing_hoisted_statements() {
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    let mut parser = ParserState::new(
        "test.ts".to_string(),
        "var first = 1; var second = 2; var third = 3; var fourth = 4;"
            .to_string(),
    );
    let root = parser.parse_source_file();
    let (arena, _) = parser.into_parts();
    let cancelled = AtomicBool::new(false);
    let budget = ProjectExecutionBudget::new(
        Instant::now() + Duration::from_secs(1),
        &cancelled,
        2,
    );
    let mut binder = BinderState::new();

    assert_eq!(
        binder.bind_source_file_with_execution_checkpoint(&arena, root, &budget),
        Err(ProjectExecutionStop::WorkBudgetExhausted),
    );
}

fn parse_test_source(source: &str) -> (tsz_parser::ParserState, tsz_parser::parser::NodeIndex) {
    let mut parser = tsz_parser::ParserState::new("test.ts".to_string(), source.to_string());
    let root = parser.parse_source_file();
    (parser, root)
}

fn parse_and_bind(source: &str) -> (BinderState, ParserState) {
    let (parser, root) = parse_test_source(source);
    let mut binder = BinderState::new();
    binder.bind_source_file(parser.get_arena(), root);
    (binder, parser)
}

fn parse_and_bind_with_options(source: &str, options: BinderOptions) -> (BinderState, ParserState) {
    let (parser, root) = parse_test_source(source);
    let mut binder = BinderState::with_options(options);
    binder.bind_source_file(parser.get_arena(), root);
    (binder, parser)
}

/// Count flow nodes with specific flags.
fn count_flow_nodes_with_flags(binder: &BinderState, flags: u32) -> usize {
    (0..binder.flow_nodes.len())
        .filter(|&i| {
            binder
                .flow_nodes
                .get(FlowNodeId(i as u32))
                .is_some_and(|n| n.has_any_flags(flags))
        })
        .count()
}

// =============================================================================
// 1. HOISTING RULES
// =============================================================================

fn bind_source(source: &str) -> BinderState {
    let (parser, root) = parse_test_source(source);
    let mut binder = BinderState::new();
    binder.bind_source_file(parser.get_arena(), root);
    binder
}
