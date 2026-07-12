use std::{path::PathBuf, sync::Arc};

use super::common::*;
use crate::{entry::NudoxPath, syntax::{body::{FunctionBody, ParsedBody}, types::{ReferenceKind, ResolvedReference}}};

#[test]
fn parse_returns_empty_references() {
	let body: ParsedBody = FunctionBody::parse(Arc::from("{}"), rust_lang()).unwrap();
	let body_ref = body.get();
	assert!(body_ref.references.is_empty());
	assert!(body_ref.tree.root_node().child_count() > 0);
}

#[test]
fn parse_and_resolve_full_workflow() {
	let source: Arc<str> = Arc::from("foo(bar())");
	let body: ParsedBody =
		FunctionBody::parse_and_resolve(source, rust_lang(), classify_test).unwrap();
	let body_ref = body.get();
	assert!(body_ref.references.len() >= 2);
	assert!(body_ref.references.iter().any(|r| r.target == NudoxPath::Local(PathBuf::from("foo"))));
	assert!(body_ref.references.iter().any(|r| r.target == NudoxPath::Local(PathBuf::from("bar"))));
}

#[test]
fn parse_and_resolve_with_no_matches() {
	let source: Arc<str> = Arc::from("no_match()");
	let body: ParsedBody =
		FunctionBody::parse_and_resolve(source, rust_lang(), |_name, kind, parent_kind, _span| {
			categorize_rust(kind, parent_kind)?;
			None
		})
		.unwrap();
	assert!(body.get().references.is_empty());
}

#[test]
fn parse_and_resolve_categorize_filters_nodes() {
	let source: Arc<str> = Arc::from("foo()");
	let body: ParsedBody =
		FunctionBody::parse_and_resolve(source, rust_lang(), |_, _, _, _| None).unwrap();
	assert!(body.get().references.is_empty());
}

#[test]
fn yoke_preserves_source_text() {
	let source_text = "fn test() { foo() }";
	let source: Arc<str> = Arc::from(source_text);
	let body: ParsedBody =
		FunctionBody::parse_and_resolve(source.clone(), rust_lang(), classify_test).unwrap();
	let _ = body.get();
}

#[test]
fn body_partial_eq_ignores_tree() {
	let body_a = FunctionBody {
		tree:       parse_body("foo()").0,
		references: vec![ResolvedReference {
			target: NudoxPath::Local(PathBuf::from("foo")),
			span:   0..3,
			kind:   ReferenceKind::FunctionCall,
		}],
	};
	let body_b = FunctionBody {
		tree:       parse_body("bar()").0,
		references: vec![ResolvedReference {
			target: NudoxPath::Local(PathBuf::from("foo")),
			span:   0..3,
			kind:   ReferenceKind::FunctionCall,
		}],
	};
	assert_eq!(body_a, body_b);
}

#[test]
fn body_partial_eq_detects_different_references() {
	let tree = parse_body("foo()").0;
	let body_a = FunctionBody {
		tree:       tree.clone(),
		references: vec![ResolvedReference {
			target: NudoxPath::Local(PathBuf::from("foo")),
			span:   0..3,
			kind:   ReferenceKind::FunctionCall,
		}],
	};
	let body_b = FunctionBody { tree, references: Vec::new() };
	assert_ne!(body_a, body_b);
}
