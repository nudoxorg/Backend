use std::path::PathBuf;

use clang::Clang;
use expect_test::{ExpectFile, expect_file};
use ir::entry::Entry;

use crate::core::clang::parser::ClangParser;

fn parse_files(files: Vec<PathBuf>) -> Vec<Entry> {
	let parser = ClangParser::from_files(Clang::new().unwrap(), files);
	parser.parse().expect("failed to parse files")
}

fn test_dir(name: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/core/clang/tests").join(name)
}

fn test_file(name: &str) -> PathBuf { test_dir(name).join(name).with_extension("c") }

fn check(file: &str, expect: ExpectFile) {
	let files = vec![test_file(file)];
	let entries = parse_files(files);

	expect.assert_debug_eq(&entries);
}

#[test]
fn test_simple01_functions() { check("simple01", expect_file!["./tests/simple01/expected.ir"]); }

#[test]
fn test_simple02_struct() { check("simple02", expect_file!["./tests/simple02/expected.ir"]); }

#[test]
fn test_simple03_mixed() { check("simple03", expect_file!["./tests/simple03/expected.ir"]); }
