//! End-to-end references pipeline: parse real source → `LanguageSpec` extract →
//! resolve → assert occurrences (REFERENCES-PLAN §6.4, in miniature), plus a
//! per-language extraction smoke test that exercises each `LanguageSpec` against
//! the pinned arborium grammar (the runnable half of §6.1).

use std::path::{Path, PathBuf};

use arborium_tree_sitter as tree_sitter;
use compiler::{
	generate::resolve::resolve,
	treesitter::{
		spec::{DefKind, PackageLayout},
		spec_for,
	},
};
use heart::Language;
use ir::entry::{Index, NudoxPath};
use ir::syntax::{ReferenceKind, Role};

// ─── helpers ─────────────────────────────────────────────────────────────────

fn parse(grammar: &str, src: &str) -> tree_sitter::Tree {
	let mut parser = tree_sitter::Parser::new();
	parser.set_language(&arborium::get_language(grammar).expect("grammar registered")).unwrap();
	parser.parse(src.as_bytes(), None).expect("parses")
}

fn local(seg: &str) -> NudoxPath {
	NudoxPath::Local(PathBuf::from(seg))
}

fn func_entry(name: &str, path: &str) -> (NudoxPath, ir::kind::Entry) {
	let mut s = ir::kind::Symbol::placeholder(ir::function::Function {
		input_parameters: None,
		output_parameters: None,
		type_links: None,
		attributes: None,
		generics: None,
		receiver: None,
		overloads: None,
		implemented: true,
		members: None,
		implemented_protocols: None,
	});
	s.name = name.into();
	s.path = local(path);
	(local(path), ir::kind::Entry::Function(s))
}

// ─── the dream, in miniature ─────────────────────────────────────────────────

/// `fn hello() { yo() }` — the call resolves to `yo`, attributed to `hello`,
/// with both sides fully-qualified `NudoxPath`s.
#[test]
fn rust_call_resolves_and_attributes_to_enclosing() {
	let src = "pub fn yo() {}\n\npub fn hello() {\n    yo();\n}\n";
	let tree = parse("rust", src);

	let layout = PackageLayout { package: "calc".into() };
	let extraction = spec_for(Language::Rust).extract(&tree, src, Path::new("lib.rs"), &layout);

	// The extractor found both definitions and the call.
	assert!(
		extraction.definitions.iter().any(|d| d.name == "yo"),
		"expected a `yo` definition, got {:?}",
		extraction.definitions
	);
	assert!(
		extraction.references.iter().any(|r| r.segments == ["yo"] && r.kind == ReferenceKind::FunctionCall),
		"expected a `yo()` FunctionCall, got {:?}",
		extraction.references
	);

	// The rust-analyzer producer fq-scheme is crate-rooted (`calc::hello`), and
	// the Rust extractor's module_path now matches (root segment = crate name).
	let index = Index {
		root_ids: vec![],
		entries_by_path: vec![func_entry("yo", "calc::yo"), func_entry("hello", "calc::hello")]
			.into_iter()
			.collect(),
	};

	let set = resolve(Language::Rust, &[(PathBuf::from("lib.rs"), extraction)], &index);

	// Exactly one reference occurrence: yo(), used inside hello.
	let refs: Vec<_> = set.files[0].occurrences.iter().filter(|o| o.role == Role::Reference).collect();
	assert_eq!(refs.len(), 1, "one reference occurrence, got {refs:?}");
	assert_eq!(refs[0].target, local("calc::yo"));
	assert_eq!(refs[0].enclosing, Some(local("calc::hello")), "attributed to hello");
	assert!(refs[0].anchored, "enclosing hello is an index entry");

	// A Definition occurrence for each function, anchored to the index.
	let defs: Vec<_> = set.files[0].occurrences.iter().filter(|o| o.role == Role::Definition).collect();
	assert!(defs.iter().any(|o| o.target == local("calc::yo")));
	assert!(defs.iter().any(|o| o.target == local("calc::hello")));
}

// ─── per-language extraction smoke ───────────────────────────────────────────
//
// Each parses a small snippet and asserts the extractor produced the expected
// shape against the pinned arborium grammar. This is the tripwire for grammar
// node-kind drift in any single `LanguageSpec` (REFERENCES-PLAN §6.6).

fn extract(lang: Language, grammar: &str, src: &str) -> compiler::treesitter::spec::Extraction {
	let tree = parse(grammar, src);
	let layout = PackageLayout { package: "pkg".into() };
	spec_for(lang).extract(&tree, src, Path::new("mod"), &layout)
}

#[test]
fn rust_extractor_smoke() {
	let e = extract(Language::Rust, "rust", "pub fn f() { g(); other::h(); }\npub fn g() {}\n");
	assert!(e.definitions.iter().any(|d| d.name == "f" && matches!(d.kind, DefKind::Function)));
	assert!(e.references.iter().any(|r| r.segments == ["g"] && r.kind == ReferenceKind::FunctionCall));
	assert!(
		e.references.iter().any(|r| r.segments == ["other", "h"]),
		"scoped chain captured as one reference, got {:?}",
		e.references
	);
}

#[test]
fn go_extractor_smoke() {
	let src = "package main\nimport \"fmt\"\nfunc F() { fmt.Println() }\n";
	let e = extract(Language::Go, "go", src);
	assert!(e.definitions.iter().any(|d| d.name == "F"));
	assert!(
		e.references.iter().any(|r| r.segments == ["fmt", "Println"]),
		"fmt.Println captured, got {:?}",
		e.references
	);
	assert!(e.imports.iter().any(|i| i.local == "fmt"), "fmt import bound, got {:?}", e.imports);
}

#[test]
fn python_extractor_smoke() {
	let src = "class C:\n    def m(self):\n        self.n()\n    def n(self):\n        pass\n";
	let e = extract(Language::Python, "python", src);
	assert!(e.definitions.iter().any(|d| d.name == "C" && matches!(d.kind, DefKind::Class)));
	assert!(e.definitions.iter().any(|d| d.name == "m"));
	assert!(
		e.references.iter().any(|r| r.kind == ReferenceKind::MethodCall),
		"self.n() is a MethodCall, got {:?}",
		e.references
	);
}

#[test]
fn typescript_extractor_smoke() {
	let src = "import { a } from './util';\nexport function f() { a(); obj.m(); }\n";
	let e = extract(Language::Typescript, "typescript", src);
	assert!(e.definitions.iter().any(|d| d.name == "f"));
	assert!(e.imports.iter().any(|i| i.local == "a"), "named import bound, got {:?}", e.imports);
	assert!(e.references.iter().any(|r| r.kind == ReferenceKind::MethodCall), "obj.m() MethodCall");
}

#[test]
fn java_extractor_smoke() {
	let src = "package com.x;\nclass A {\n  void m() { obj.n(); }\n}\n";
	let e = extract(Language::Java, "java", src);
	assert!(e.definitions.iter().any(|d| d.name == "A" && matches!(d.kind, DefKind::Class)));
	assert!(e.definitions.iter().any(|d| d.name == "m"));
	assert!(e.references.iter().any(|r| r.kind == ReferenceKind::MethodCall));
}

#[test]
fn nix_extractor_smoke() {
	let src = "{ f = x: x; g = f 1; a.b = 2; }";
	let e = extract(Language::Nix, "nix", src);
	assert!(e.definitions.iter().any(|d| d.name == "f"), "attrpath def, got {:?}", e.definitions);
	assert!(
		e.references.iter().any(|r| r.kind == ReferenceKind::FunctionCall),
		"application f 1 is a FunctionCall, got {:?}",
		e.references
	);
}

// ─── Example rendering (the "Show" step, §4.5) ───────────────────────────────

/// A resolved use site renders as its enclosing-function snippet with the call
/// site located for highlighting — the payoff of the references feature.
#[test]
fn render_example_highlights_call_site() {
	use compiler::treesitter::render_example;

	let src = "pub fn yo() {}\n\npub fn hello() {\n    yo();\n}\n";
	let at = src.find("yo();").expect("call present");
	let ex = render_example(src, Language::Rust, at..at + 2, 40);

	assert!(ex.snippet.contains("fn hello"), "snippet is the enclosing fn: {:?}", ex.snippet);
	assert!(!ex.snippet.contains("fn yo() {}"), "not the whole file: {:?}", ex.snippet);
	assert_eq!(&ex.snippet[ex.highlight.clone()], "yo", "highlight lands on the use site");
}

/// An out-of-range span degrades gracefully rather than panicking.
#[test]
fn render_example_out_of_range_is_safe() {
	use compiler::treesitter::render_example;

	let src = "fn a() {}\n";
	let ex = render_example(src, Language::Rust, 999..1000, 40);
	assert!(ex.highlight.start <= ex.snippet.len());
	assert!(ex.highlight.end <= ex.snippet.len());
}
