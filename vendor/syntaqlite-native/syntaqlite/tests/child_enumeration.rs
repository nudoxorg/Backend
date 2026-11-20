// Copyright 2025 The syntaqlite Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0.

//! Tests for child node enumeration via `AnyParsedStatement::child_node_ids`.
//!
//! `child_node_ids` auto-expands list nodes: for a `SelectStmt` with a
//! `ResultColumnList`, the list's elements are returned directly rather than
//! the list node itself.

use syntaqlite::any::{AnyNodeId, AnyNodeTag, AnyParsedStatement};
use syntaqlite::nodes::NodeTag;
use syntaqlite::{ParseOutcome, Parser};

/// Resolve the node at `id` and return its grammar-level tag.
fn node_tag(stmt: &AnyParsedStatement<'_>, id: AnyNodeId) -> AnyNodeTag {
    stmt.extract_fields(id).expect("node id should resolve").0
}

#[test]
fn select_columns_and_table_ref_in_child_list() {
    // `child_node_ids` expands lists: ResultColumnList becomes its elements.
    // For "SELECT a, b FROM t", the SelectStmt children are:
    //   [ResultColumn(a), ResultColumn(b), TableRef(t)]
    let parser = Parser::new();
    let mut session = parser.parse("SELECT a, b FROM t");
    let ParseOutcome::Ok(stmt) = session.next() else {
        panic!("expected Ok");
    };
    let any_stmt = stmt.erase();
    let stmt_id = any_stmt.root_id();

    let children: Vec<_> = any_stmt.child_node_ids(stmt_id).collect();
    assert!(
        !children.is_empty(),
        "SelectStmt should have at least one child"
    );

    let tags: Vec<_> = children.iter().map(|id| node_tag(&any_stmt, *id)).collect();
    let result_col_tag = AnyNodeTag::from(NodeTag::ResultColumn);
    let table_ref_tag = AnyNodeTag::from(NodeTag::TableRef);

    assert!(
        tags.contains(&result_col_tag),
        "children should include ResultColumn(s), got: {tags:?}"
    );
    assert!(
        tags.contains(&table_ref_tag),
        "children should include TableRef, got: {tags:?}"
    );

    // Exactly 2 result columns.
    let col_count = tags.iter().filter(|&&t| t == result_col_tag).count();
    assert_eq!(col_count, 2, "expected 2 result columns, got {col_count}");
}

#[test]
fn result_columns_enumerated_for_select_a_b_c() {
    // For "SELECT a, b, c FROM t", child_node_ids on SelectStmt expands the
    // ResultColumnList and returns each ResultColumn separately.
    let parser = Parser::new();
    let mut session = parser.parse("SELECT a, b, c FROM t");
    let ParseOutcome::Ok(stmt) = session.next() else {
        panic!("expected Ok");
    };
    let any_stmt = stmt.erase();
    let stmt_id = any_stmt.root_id();

    let children: Vec<_> = any_stmt.child_node_ids(stmt_id).collect();
    let tags: Vec<_> = children.iter().map(|id| node_tag(&any_stmt, *id)).collect();
    let result_col_tag = AnyNodeTag::from(NodeTag::ResultColumn);

    let col_count = tags.iter().filter(|&&t| t == result_col_tag).count();
    assert_eq!(
        col_count, 3,
        "ResultColumnList for 'a, b, c' should enumerate 3 ResultColumn children"
    );
}
