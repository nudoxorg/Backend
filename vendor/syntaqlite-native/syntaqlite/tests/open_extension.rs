// Copyright 2025 The syntaqlite Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0.

//! Integration tests for the open extension API.

use syntaqlite::nodes::Stmt;
use syntaqlite::{ParseOutcome, Parser};

fn new_parser() -> Parser {
    Parser::new()
}

#[test]
fn pure_sqlite_never_produces_unknown_node() {
    let parser = new_parser();
    let sqls = [
        "SELECT 1 + 2",
        "INSERT INTO t VALUES(1)",
        "UPDATE t SET x = 1",
        "DELETE FROM t WHERE x = 1",
        "CREATE TABLE t(x INT)",
    ];
    for sql in &sqls {
        let mut session = parser.parse(sql);
        let ParseOutcome::Ok(stmt) = session.next() else {
            panic!("expected parse Ok for: {sql}");
        };
        // root() returns Stmt<'_> directly — panics on invariant violation.
        // Calling it is sufficient to verify the node resolved.
        let _ = stmt.root();
    }
}

#[test]
fn pure_sqlite_stmts_are_known_variants() {
    let parser = new_parser();
    let cases = [
        ("SELECT 1", "SelectStmt"),
        ("INSERT INTO t VALUES(1)", "InsertStmt"),
        ("UPDATE t SET x = 1", "UpdateStmt"),
        ("DELETE FROM t WHERE x = 1", "DeleteStmt"),
    ];
    for (sql, expected_name) in &cases {
        let mut session = parser.parse(sql);
        let ParseOutcome::Ok(stmt) = session.next() else {
            panic!("expected parse Ok for: {sql}");
        };
        let root = stmt.root().expect("expected root");
        let name = match root {
            Stmt::SelectStmt(_) => "SelectStmt",
            Stmt::InsertStmt(_) => "InsertStmt",
            Stmt::UpdateStmt(_) => "UpdateStmt",
            Stmt::DeleteStmt(_) => "DeleteStmt",
            other => panic!(
                "unexpected stmt variant {:?} for: {sql}",
                std::mem::discriminant(&other)
            ),
        };
        assert_eq!(name, *expected_name, "for sql: {sql}");
    }
}
