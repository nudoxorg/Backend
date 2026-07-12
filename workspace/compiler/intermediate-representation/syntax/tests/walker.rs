use std::path::PathBuf;

use super::common::*;
use crate::{entry::NudoxPath, syntax::{types::ReferenceKind, walker::walk_references}};

#[test]
fn empty_body_produces_no_references() {
	let (tree, source) = parse_body("{}");
	let refs = walk_references(&tree, &source, |_, _, _, _| None);
	assert!(refs.is_empty());
}

#[test]
fn unknown_identifiers_are_filtered_out() {
	let (tree, source) = parse_body("unknown_function()");
	let refs = walk_references(&tree, &source, |_name, kind, parent_kind, _span| {
		categorize_rust(kind, parent_kind)?;
		None
	});
	assert!(refs.is_empty());
}

#[test]
fn single_function_call_is_resolved() {
	let (tree, source) = parse_body("foo()");
	let refs = walk_references(&tree, &source, classify_test);
	assert_eq!(refs.len(), 1);
	assert_eq!(refs[0].kind, ReferenceKind::FunctionCall);
	assert_eq!(refs[0].target, NudoxPath::Local(PathBuf::from("foo")));
}

#[test]
fn nested_calls_produce_multiple_references() {
	let (tree, source) = parse_body("foo(bar())");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(refs.len() >= 2);
	assert_targets(&refs, &["foo", "bar"]);
}

#[test]
fn field_access_is_classified_correctly() {
	let (tree, source) = parse_body("self.foo()");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(
		refs.iter().any(|r| r.kind == ReferenceKind::FieldAccess),
		"expected at least one FieldAccess"
	);
}

#[test]
fn macro_invocation_is_classified_correctly() {
	let (tree, source) = parse_body("println!(\"hello\")");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(
		refs.iter().any(|r| r.kind == ReferenceKind::MacroInvocation),
		"expected at least one MacroInvocation"
	);
	assert_kind(&refs, "println", ReferenceKind::MacroInvocation);
}

#[test]
fn type_reference_is_classified_correctly() {
	let (tree, source) = parse_body("let x: HashMap<String, Vec<u8>> = HashMap::new()");
	let refs = walk_references(&tree, &source, classify_test);
	assert_kind(&refs, "HashMap", ReferenceKind::TypeReference);
	assert!(
		refs.iter().any(|r| r.kind == ReferenceKind::TypeReference),
		"expected at least one TypeReference"
	);
}

#[test]
fn scoped_identifier_is_classified_as_type_reference() {
	let (tree, source) = parse_body("std::collections::HashMap::new()");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(
		refs.iter().any(|r| r.kind == ReferenceKind::TypeReference),
		"expected at least one TypeReference from scoped identifiers"
	);
}

#[test]
fn categorize_can_exclude_certain_node_kinds() {
	let (tree, source) = parse_body("42");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(refs.is_empty());
}

// -----------------------------------------------------------------------
// Import statements
// -----------------------------------------------------------------------

#[test]
fn simple_import_resolves_scoped_path() {
	let (tree, source) = parse_body("use std::collections::HashMap;");
	let refs = walk_references(&tree, &source, classify_test);
	assert_targets(&refs, &["std::collections::HashMap"]);
	assert!(
		refs[0].kind == ReferenceKind::Import || refs[0].kind == ReferenceKind::TypeReference,
		"scoped import identifier should be Import or TypeReference, got {:?}",
		refs[0].kind
	);
}

#[test]
fn glob_import_skips_wildcard_and_resolves_path() {
	let (tree, source) = parse_body("use crate::module::*;");
	let refs = walk_references(&tree, &source, classify_test);
	let wildcard_refs: Vec<_> = refs.iter().filter(|r| ref_target_name(r) == "*").collect();
	assert!(
		wildcard_refs.is_empty(),
		"wildcard should not appear as a reference, got {wildcard_refs:?}"
	);
	assert_targets(&refs, &["crate::module"]);
}

#[test]
fn aliased_import_finds_both_names() {
	let (tree, source) = parse_body("use HashMap as Map;");
	let refs = walk_references(&tree, &source, classify_test);
	assert_targets(&refs, &["HashMap", "Map"]);
}

#[test]
fn aliased_import_resolves_with_import_kind() {
	let (tree, source) = parse_body("use std::collections::HashMap as Map;");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(
		refs.iter().any(|r| r.kind == ReferenceKind::Import),
		"expected at least one Import kind reference, got {refs:?}"
	);
}

#[test]
fn nested_import_resolves_std_crate_and_sub_paths() {
	let (tree, source) = parse_body("use std::{collections::HashMap, io::Result};");
	let refs = walk_references(&tree, &source, classify_test);
	assert_targets(&refs, &["std", "collections::HashMap", "io::Result"]);
	assert!(
		refs
			.iter()
			.any(|r| matches!(&r.target, NudoxPath::External { dependency, .. } if dependency == "std")),
		"expected reference from crate 'std' in nested import, got {refs:?}"
	);
}

#[test]
fn self_import_resolves_full_local_path() {
	let (tree, source) = parse_body("use self::foo::bar;");
	let refs = walk_references(&tree, &source, classify_test);
	assert_targets(&refs, &["self::foo::bar"]);
}

// -----------------------------------------------------------------------
// Chained method calls
// -----------------------------------------------------------------------

#[test]
fn chained_method_calls_all_have_field_access() {
	let (tree, source) = parse_body("self.foo().bar().baz()");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(refs.len() >= 3, "expected at least 3 references from chained calls, got {refs:?}");
	assert!(
		refs.iter().all(|r| r.kind == ReferenceKind::FieldAccess),
		"expected all chained call identifiers to be FieldAccess, got {refs:?}"
	);
}

#[test]
fn chained_method_invocations_are_field_access_not_function_call() {
	let (tree, source) = parse_body("self.foo().bar()");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(
		refs.iter().any(|r| r.kind == ReferenceKind::FieldAccess),
		"expected FieldAccess, got {refs:?}"
	);
	assert!(
		refs.iter().all(|r| r.kind != ReferenceKind::FunctionCall),
		"chained methods should not produce FunctionCall, got {refs:?}"
	);
}

// -----------------------------------------------------------------------
// Complex generics
// -----------------------------------------------------------------------

#[test]
fn deeply_nested_generics_produce_type_references() {
	let code = "let x: HashMap<String, Vec<Option<u8>>> = HashMap::new();";
	let (tree, source) = parse_body(code);
	let refs = walk_references(&tree, &source, classify_test);
	assert_kind(&refs, "HashMap", ReferenceKind::TypeReference);
}

// -----------------------------------------------------------------------
// Import + body usage — same name resolved differently by context
// -----------------------------------------------------------------------

#[test]
fn import_and_body_usage_resolve_same_name_differently() {
	let code = "\
use std::collections::HashMap;

fn example() {
    let mut map: HashMap<Vec<u8>, Option<u8>> = HashMap::new();
    map.insert(vec![], None);
}";
	let (tree, source) = parse_body(code);
	let refs = walk_references(&tree, &source, classify_test);

	let external_from_std: Vec<_> = refs
		.iter()
		.filter(|r| matches!(&r.target, NudoxPath::External { dependency, .. } if dependency == "std"))
		.collect();
	assert!(
		!external_from_std.is_empty(),
		"expected External reference from crate 'std', got {refs:?}"
	);

	assert!(
		refs
			.iter()
			.any(|r| { r.kind == ReferenceKind::TypeReference && ref_target_name(r) == "HashMap" }),
		"expected Local TypeReference for 'HashMap' from type annotation, got {refs:?}"
	);

	let type_names: Vec<_> = refs
		.iter()
		.filter(|r| r.kind == ReferenceKind::TypeReference)
		.map(|r| ref_target_name(r))
		.collect();
	for name in &["HashMap", "Vec", "Option"] {
		assert!(
			type_names.contains(&name.to_string()),
			"expected TypeReference for {name:?}, got {type_names:?}"
		);
	}
}

// -----------------------------------------------------------------------
// Mixed reference kinds in statements
// -----------------------------------------------------------------------

#[test]
fn multiple_statements_produce_all_reference_kinds() {
	let code = "\
use std::collections::HashMap;
fn test() {
    let mut map = HashMap::new();
    map.insert(\"k\", \"v\");
    println!(\"{}\", map.len());
}";
	let (tree, source) = parse_body(code);
	let refs = walk_references(&tree, &source, classify_test);
	assert!(!refs.is_empty(), "expected references from multi-statement body");
	let kinds: Vec<_> = refs.iter().map(|r| r.kind.clone()).collect();
	assert!(kinds.contains(&ReferenceKind::Import), "expected Import (from use path), got {kinds:?}");
	assert!(
		kinds.contains(&ReferenceKind::TypeReference),
		"expected TypeReference (from nested scoped_identifier), got {kinds:?}"
	);
	assert!(
		kinds.contains(&ReferenceKind::MacroInvocation),
		"expected MacroInvocation (println!), got {kinds:?}"
	);
}

// -----------------------------------------------------------------------
// Edge cases — whitespace, comments, empty
// -----------------------------------------------------------------------

#[test]
fn empty_source_produces_no_references() {
	let (tree, source) = parse_body("");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(refs.is_empty());
}

#[test]
fn whitespace_only_source_produces_no_references() {
	let (tree, source) = parse_body("   \n  \t  ");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(refs.is_empty());
}

#[test]
fn comment_only_source_produces_no_references() {
	let (tree, source) = parse_body("// just a comment\n/* block */");
	let refs = walk_references(&tree, &source, classify_test);
	assert!(refs.is_empty());
}
