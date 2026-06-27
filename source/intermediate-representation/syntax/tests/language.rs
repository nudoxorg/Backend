use super::common::*;
use crate::syntax::{types::ReferenceKind, walker::walk_references};

#[test]
fn c_demo_recognizes_function_call() {
	let (tree, source) = parse_body("foo()");
	let refs = walk_references(&tree, &source, compose(categorize_c_demo, resolve_test_path));
	assert_eq!(refs.len(), 1);
	assert_eq!(refs[0].kind, ReferenceKind::FunctionCall);
}

#[test]
fn c_demo_recognizes_field_access() {
	let (tree, source) = parse_body("foo.bar()");
	let refs = walk_references(&tree, &source, compose(categorize_c_demo, resolve_test_path));
	assert!(refs.iter().any(|r| r.kind == ReferenceKind::FieldAccess));
}

#[test]
fn cpp_demo_recognizes_scoped_type() {
	let (tree, source) = parse_body("HashMap::new()");
	let refs = walk_references(&tree, &source, compose(categorize_cpp_demo, resolve_test_path));
	assert!(!refs.is_empty(), "expected at least one reference");
}

#[test]
fn ts_demo_recognizes_type_reference() {
	let (tree, source) = parse_body("let x: HashMap<Vec<u8>, Option<u8>> = HashMap::new()");
	let refs = walk_references(&tree, &source, compose(categorize_ts_demo, resolve_test_path));
	assert!(
		refs.iter().any(|r| r.kind == ReferenceKind::TypeReference),
		"expected TypeReference, got {refs:?}"
	);
}

#[test]
fn compose_defaults_to_none() {
	let (tree, source) = parse_body("42");
	let refs = walk_references(&tree, &source, compose(categorize_c_demo, resolve_test_path));
	assert!(refs.is_empty());
}
