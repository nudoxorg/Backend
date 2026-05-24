use std::{ops::Range, path::PathBuf};

use arborium_tree_sitter as tree_sitter;

use crate::{entry::NudoxPath, syntax::types::{ReferenceKind, ResolvedReference}};

// -------------------------------------------------------------------
// Parse helpers
// -------------------------------------------------------------------

pub fn parse_body(source: &str) -> (tree_sitter::Tree, String) {
	let mut parser = tree_sitter::Parser::new();
	parser.set_language(&arborium::get_language("rust").unwrap()).unwrap();
	let tree = parser.parse(source.as_bytes(), None).unwrap();
	(tree, source.to_owned())
}

pub fn rust_lang() -> tree_sitter::Language { arborium::get_language("rust").unwrap() }

// -------------------------------------------------------------------
// Rust node-kind categorizer
// -------------------------------------------------------------------

pub fn categorize_rust(kind: &str, parent: Option<&str>) -> Option<ReferenceKind> {
	match (kind, parent) {
		("identifier", Some("scoped_identifier")) => None,
		("identifier", Some("call_expression")) => Some(ReferenceKind::FunctionCall),
		("identifier", Some("macro_invocation")) => Some(ReferenceKind::MacroInvocation),
		("identifier", Some("scoped_use_tree")) => Some(ReferenceKind::Import),
		("identifier", Some("use_as_clause")) => Some(ReferenceKind::Import),
		("field_identifier", Some("field_expression")) | ("identifier", Some("field_expression")) => {
			Some(ReferenceKind::FieldAccess)
		}
		("identifier", _) => Some(ReferenceKind::VariableUse),
		("scoped_identifier", Some("scoped_use_tree" | "use_declaration")) => {
			Some(ReferenceKind::Import)
		}
		("scoped_identifier", _) => Some(ReferenceKind::TypeReference),
		("type_identifier", _) => Some(ReferenceKind::TypeReference),
		_ => None,
	}
}

// -------------------------------------------------------------------
// Path resolver (test double)
// -------------------------------------------------------------------

pub const LOCAL_NAMES: &[&str] = &[
	"HashMap",
	"Vec",
	"Option",
	"Result",
	"foo",
	"bar",
	"baz",
	"println",
	"Some",
	"None",
	"Map",
	"insert",
	"len",
	"module",
	"collections",
	"io",
	"crate",
	"self",
	"collections::HashMap",
	"io::Result",
	"crate::module",
	"self::foo::bar",
];

pub fn resolve_test_path(
	name: &str,
	_span: Range<usize>,
	_kind: ReferenceKind,
) -> Option<NudoxPath> {
	let externals = ["std"];
	if externals.contains(&name) {
		return Some(NudoxPath::External { path: PathBuf::new(), dependency: name.to_string() });
	}
	if let Some((dep, rest)) = name.split_once("::") {
		if externals.contains(&dep) {
			return Some(NudoxPath::External {
				path:       PathBuf::from(rest),
				dependency: dep.to_string(),
			});
		}
		if LOCAL_NAMES.contains(&name) {
			return Some(NudoxPath::Local(PathBuf::from(name)));
		}
	}
	if LOCAL_NAMES.contains(&name) { Some(NudoxPath::Local(PathBuf::from(name))) } else { None }
}

// -------------------------------------------------------------------
// Assertion helpers
// -------------------------------------------------------------------

pub fn ref_target_name(r: &ResolvedReference) -> String {
	match &r.target {
		NudoxPath::Local(p) => p.display().to_string(),
		NudoxPath::External { path, dependency } => {
			let p = path.display().to_string();
			if p.is_empty() { dependency.clone() } else { format!("{dependency}::{p}") }
		}
	}
}

pub fn assert_targets(refs: &[ResolvedReference], expected: &[&str]) {
	let found: Vec<_> = refs.iter().map(ref_target_name).collect();
	for name in expected {
		assert!(found.contains(&name.to_string()), "expected target {name:?} not found; got {found:?}");
	}
}

pub fn assert_kind(refs: &[ResolvedReference], name: &str, kind: ReferenceKind) {
	for r in refs {
		if ref_target_name(r) == name {
			assert_eq!(r.kind, kind, "unexpected kind for {name:?}");
			return;
		}
	}
	panic!("target {name:?} not found in references");
}

// -------------------------------------------------------------------
// Classify combos
// -------------------------------------------------------------------

pub fn classify_test(
	name: &str,
	kind: &str,
	parent_kind: Option<&str>,
	span: Range<usize>,
) -> Option<ResolvedReference> {
	let knd = categorize_rust(kind, parent_kind)?;
	let target = resolve_test_path(name, span.clone(), knd.clone())?;
	Some(ResolvedReference { target, span, kind: knd })
}

// -------------------------------------------------------------------
// Language demo classifiers — C, C++, TypeScript
// -------------------------------------------------------------------

pub fn categorize_c_demo(kind: &str, parent: Option<&str>) -> Option<ReferenceKind> {
	match (kind, parent) {
		("identifier", Some("call_expression")) => Some(ReferenceKind::FunctionCall),
		("identifier", Some("field_expression")) => Some(ReferenceKind::FieldAccess),
		("identifier", Some("preproc_include")) => Some(ReferenceKind::Import),
		("type_identifier", _) => Some(ReferenceKind::TypeReference),
		("identifier", _) => Some(ReferenceKind::VariableUse),
		_ => None,
	}
}

pub fn categorize_cpp_demo(kind: &str, parent: Option<&str>) -> Option<ReferenceKind> {
	match (kind, parent) {
		("identifier", Some("call_expression")) => Some(ReferenceKind::FunctionCall),
		("identifier", Some("field_expression")) => Some(ReferenceKind::FieldAccess),
		("qualified_identifier", _) => Some(ReferenceKind::TypeReference),
		("template_function", _) => Some(ReferenceKind::FunctionCall),
		("template_type", _) => Some(ReferenceKind::TypeReference),
		("type_identifier", _) => Some(ReferenceKind::TypeReference),
		("identifier", _) => Some(ReferenceKind::VariableUse),
		_ => None,
	}
}

pub fn categorize_ts_demo(kind: &str, parent: Option<&str>) -> Option<ReferenceKind> {
	match (kind, parent) {
		("identifier", Some("call_expression")) => Some(ReferenceKind::FunctionCall),
		("property_identifier", Some("property_access_expression")) => Some(ReferenceKind::FieldAccess),
		("type_identifier", _) => Some(ReferenceKind::TypeReference),
		("identifier", Some("import_specifier")) => Some(ReferenceKind::Import),
		("identifier", _) => Some(ReferenceKind::VariableUse),
		_ => None,
	}
}

/// Compose a classify closure from separate categorize + resolve functions.
pub fn compose<C, R>(
	mut categorize: C,
	mut resolve: R,
) -> impl FnMut(&str, &str, Option<&str>, Range<usize>) -> Option<ResolvedReference>
where
	C: FnMut(&str, Option<&str>) -> Option<ReferenceKind>,
	R: FnMut(&str, Range<usize>, ReferenceKind) -> Option<NudoxPath>,
{
	move |name, kind, parent_kind, span| {
		let knd = categorize(kind, parent_kind)?;
		let target = resolve(name, span.clone(), knd.clone())?;
		Some(ResolvedReference { target, span, kind: knd })
	}
}
