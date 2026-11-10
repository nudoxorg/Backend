#![cfg(all(feature = "fmt", feature = "sqlite"))]
//! Integration tests for public formatter keyword-case configuration.

use syntaqlite::fmt::KeywordCase;
use syntaqlite::{FormatConfig, Formatter};

#[test]
fn keyword_case_upper() {
    let cfg = FormatConfig::default().with_keyword_case(KeywordCase::Upper);
    let mut fmt = Formatter::with_config(&cfg);
    let out = fmt
        .format("select 1")
        .expect("formatting should succeed for valid SQL");
    assert_eq!(out, "SELECT 1;\n");
}

#[test]
fn keyword_case_lower() {
    let cfg = FormatConfig::default().with_keyword_case(KeywordCase::Lower);
    let mut fmt = Formatter::with_config(&cfg);
    let out = fmt
        .format("SELECT 1")
        .expect("formatting should succeed for valid SQL");
    assert_eq!(out, "select 1;\n");
}
