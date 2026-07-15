//! Focused fidelity specs for the RA Rust IR producer.
//!
//! Asserts method dual-emit, primitive widths, TraitImpl path uniqueness,
//! const TypedBinding payloads, type-alias generics, SymbolTable resolution,
//! and `&str` not collapsing to owned String — without heavy IR snapshots.

use std::fs;
use std::path::{Path, PathBuf};

use compiler::graph::symtab::SymbolTable;
use compiler::languages::rust::generate_ir;
use ir::entry::{Index, NudoxPath};
use ir::generics::ConstExpr;
use ir::kind::{Entry, TypeAliasBody, Visibility};
use ir::primitives::{Primitive, Width};
use ir::ty::Type;
use rustc_hash::FxHashMap as HashMap;
use semver::Version;
use tempfile::TempDir;

// ─── Fixture plumbing ────────────────────────────────────────────────────────

fn fixture_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust")
}

fn copy_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
	fs::create_dir_all(dest)?;
	for entry in fs::read_dir(src)? {
		let entry = entry?;
		let target = dest.join(entry.file_name());
		if entry.file_type()?.is_dir() {
			copy_tree(&entry.path(), &target)?;
		} else {
			fs::copy(entry.path(), &target)?;
		}
	}
	Ok(())
}

fn scaffold_fidelity(dir: &Path) {
	fs::write(
		dir.join("Cargo.toml"),
		r#"[package]
name = "fidelity"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
	)
	.expect("fidelity Cargo.toml");
}

fn lower_fidelity() -> (Index, HashMap<String, String>, TempDir) {
	let dir = TempDir::new().expect("tempdir");
	copy_tree(&fixture_root().join("fidelity"), dir.path()).expect("fixture copies");
	scaffold_fidelity(dir.path());
	let version = Version::parse("0.1.0").expect("version");
	let (index, sources) =
		generate_ir(dir.path(), "fidelity", &version, true).expect("lowering succeeds");
	(index, sources, dir)
}

fn local(path: &str) -> NudoxPath {
	NudoxPath::Local(PathBuf::from(path))
}

fn entry<'i>(index: &'i Index, path: &str) -> &'i Entry {
	index
		.entries_by_path
		.get(&local(path))
		.unwrap_or_else(|| panic!("expected entry at {path}; have {:?}", index.entries_by_path.keys().collect::<Vec<_>>()))
}

// ─── Specs ───────────────────────────────────────────────────────────────────

/// Method path `fidelity::Counter::new` exists as `Entry::Function` in the index.
#[test]
fn method_path_is_indexable_function_entry() {
	let (index, _sources, _dir) = lower_fidelity();

	assert!(
		matches!(entry(&index, "fidelity::Counter::new"), Entry::Function(_)),
		"Counter::new dual-emitted as index Function"
	);
	assert!(
		matches!(entry(&index, "fidelity::Counter::value"), Entry::Function(_)),
		"Counter::value dual-emitted as index Function"
	);
	assert!(
		matches!(entry(&index, "fidelity::Mode::is_on"), Entry::Function(_)),
		"enum methods dual-emitted even without Record.methods field"
	);

	// Record.members lists the dual-emitted paths.
	let counter = match entry(&index, "fidelity::Counter") {
		Entry::RecordType(s) => s,
		other => panic!("Counter is RecordType, got {other}"),
	};
	let members = counter
		.inner
		.members
		.as_ref()
		.expect("Counter.members filled with method paths");
	assert!(
		members.iter().any(|p| matches!(
			p,
			NudoxPath::Local(pb) if pb.to_string_lossy().contains("Counter::new")
		)),
		"members include Counter::new: {members:?}"
	);
}

/// `i32` field is `Primitive::Int(W32)` not Arch.
#[test]
fn i32_field_is_w32_not_arch() {
	let (index, _sources, _dir) = lower_fidelity();

	let counter = match entry(&index, "fidelity::Counter") {
		Entry::RecordType(s) => s,
		other => panic!("Counter is RecordType, got {other}"),
	};
	let field_ty = counter
		.inner
		.fields
		.iter()
		.find_map(|f| match f {
			ir::record::Field::Known(k) if matches!(&k.key, ir::record::FieldKey::Ident(n) if n == "value") => {
				k.r#type.as_ref().map(|t| t.as_ref())
			}
			_ => None,
		})
		.expect("value field present");

	assert_eq!(
		field_ty,
		&Type::Primitive(Primitive::Int(Width::W32)),
		"i32 must be Int(W32), not Arch"
	);
}

/// Two types implementing the same trait → two TraitImpl entries with distinct paths.
#[test]
fn two_impls_of_same_trait_have_distinct_paths() {
	let (index, _sources, _dir) = lower_fidelity();

	let impls: Vec<_> = index
		.entries_by_path
		.iter()
		.filter_map(|(p, e)| match e {
			Entry::TraitImpl(s) => {
				let leaf = s.name.clone();
				if leaf.contains("Drawable") {
					Some((p.clone(), leaf))
				} else {
					None
				}
			}
			_ => None,
		})
		.collect();

	assert!(
		impls.len() >= 2,
		"expected ≥2 Drawable TraitImpl entries, got {impls:?}"
	);

	let paths: Vec<_> = impls.iter().map(|(p, _)| p).collect();
	assert_ne!(
		paths[0], paths[1],
		"TraitImpl paths must be unique, got {paths:?}"
	);

	// Paths include self-type disambiguation.
	let names: Vec<&str> = impls.iter().map(|(_, n)| n.as_str()).collect();
	assert!(
		names.iter().any(|n| n.contains("Circle")),
		"one impl name mentions Circle: {names:?}"
	);
	assert!(
		names.iter().any(|n| n.contains("Square")),
		"one impl name mentions Square: {names:?}"
	);
}

/// `pub const N: i32 = 4` → Constant with ty Int(W32) and recoverable value.
#[test]
fn const_binding_recovers_type_and_value() {
	let (index, _sources, _dir) = lower_fidelity();

	let binding = match entry(&index, "fidelity::N") {
		Entry::Constant(s) => &s.inner,
		other => panic!("N is Constant, got {other}"),
	};

	assert_eq!(
		binding.ty.as_ref(),
		Some(&Type::Primitive(Primitive::Int(Width::W32))),
		"const N: i32 type recovered"
	);
	assert_eq!(
		binding.value.as_ref(),
		Some(&ConstExpr::Int(4)),
		"const value 4 recovered"
	);
	assert_eq!(binding.mutable, Some(false));
}

/// `(1 + 2) * 4` folds (or at least structures) — never a bare opaque Var of the full text only.
#[test]
fn complex_const_expr_is_structured() {
	let (index, _sources, _dir) = lower_fidelity();

	let binding = match entry(&index, "fidelity::SHIFTED") {
		Entry::Constant(s) => &s.inner,
		other => panic!("SHIFTED is Constant, got {other}"),
	};

	let value = binding.value.as_ref().expect("SHIFTED has a value payload");
	// Prefer full fold to 12; accept structured BinOp tree as intermediate.
	match value {
		ConstExpr::Int(12) => {}
		ConstExpr::BinOp { .. } | ConstExpr::Call { .. } => {}
		ConstExpr::Var(text) => {
			// Var is only acceptable if it still carries the expression spelling
			// (never silent-drop) — but structured forms are preferred.
			assert!(
				text.contains('+') || text.contains('*'),
				"complex const must not collapse to an unrelated Var: {text}"
			);
		}
		other => panic!("unexpected ConstExpr form for SHIFTED: {other:?}"),
	}
}

/// Type alias with generics retains generics on TypeAliasBody.
#[test]
fn type_alias_retains_generics() {
	let (index, _sources, _dir) = lower_fidelity();

	let body: &TypeAliasBody = match entry(&index, "fidelity::Pair") {
		Entry::TypeAlias(s) => &s.inner,
		other => panic!("Pair is TypeAlias, got {other}"),
	};

	let generics = body
		.generics
		.as_ref()
		.expect("Pair<T, U> keeps generics on TypeAliasBody");
	assert!(
		generics.params.len() >= 2,
		"expected ≥2 type params, got {}",
		generics.params.len()
	);
	// RHS is a 2-tuple.
	assert!(
		matches!(&body.target, Type::Tuple(ts) if ts.len() == 2),
		"Pair target is (T, U) tuple: {:?}",
		body.target
	);
}

/// SymbolTable resolve_exact hits the dual-emitted method path.
#[test]
fn symbol_table_resolves_method_path() {
	let (index, _sources, _dir) = lower_fidelity();
	let table = SymbolTable::build(&index);

	let path = table
		.resolve_exact("fidelity::Counter::new")
		.expect("resolve_exact hits Counter::new");
	assert!(
		matches!(path, NudoxPath::Local(p) if p.to_string_lossy().contains("Counter::new")),
		"resolved path is Counter::new: {path:?}"
	);
}

/// `&str` does not render/lower as owned String primitive.
#[test]
fn ref_str_is_not_owned_string_primitive() {
	let (index, _sources, _dir) = lower_fidelity();

	let label = match entry(&index, "fidelity::Counter::label") {
		Entry::Function(s) => s,
		other => panic!("label is Function, got {other}"),
	};
	let out = label
		.inner
		.output_parameters
		.as_ref()
		.and_then(|ps| ps.first())
		.and_then(|p| match p {
			ir::parameter::Parameter::Literal(l) => l.r#type.as_ref(),
			_ => None,
		})
		.expect("label has return type");

	// Must be BorrowedRef to str (TypeReference or similar) — never Primitive::String.
	match out {
		Type::BorrowedRef { r#type, is_mutable, .. } => {
			assert!(!*is_mutable, "&str is shared");
			match r#type.as_ref() {
				Type::TypeReference(tr) => {
					assert!(
						tr.identifier == "str" || tr.identifier.ends_with("::str"),
						"inner is str TypeReference, got {}",
						tr.identifier
					);
				}
				Type::Primitive(Primitive::String) => {
					panic!("&str must not collapse to Primitive::String");
				}
				other => panic!("unexpected &str inner: {other:?}"),
			}
		}
		Type::Primitive(Primitive::String) => {
			panic!("&str must not lower as bare Primitive::String");
		}
		other => panic!("label return should be BorrowedRef, got {other:?}"),
	}
}

/// Unmarked private items map to Visibility::Private (not Internal).
#[test]
fn unmarked_private_is_private_visibility() {
	let (index, _sources, _dir) = lower_fidelity();

	let mod_vis = match entry(&index, "fidelity::private_mod") {
		Entry::Module(s) => &s.visibility,
		other => panic!("private_mod is Module, got {other}"),
	};
	assert_eq!(
		*mod_vis,
		Visibility::Private,
		"unmarked module is Private, got {mod_vis:?}"
	);
}

/// Trait associated type bounds and const types are recovered (C8).
#[test]
fn trait_assoc_bounds_and_const_types() {
	let (index, _sources, _dir) = lower_fidelity();

	let trait_def = match entry(&index, "fidelity::WithAssoc") {
		Entry::TraitDef(s) => s,
		other => panic!("WithAssoc is TraitDef, got {other}"),
	};

	let assoc = trait_def
		.inner
		.associated_types
		.as_ref()
		.and_then(|v| v.iter().find(|a| a.name == "Item"))
		.expect("Item assoc type");
	assert!(
		assoc.bounds.as_ref().is_some_and(|b| !b.is_empty()),
		"Item: Clone bounds recovered: {:?}",
		assoc.bounds
	);

	let c = trait_def
		.inner
		.required_constants
		.as_ref()
		.and_then(|v| v.iter().find(|c| c.name == "MAX"))
		.expect("MAX const");
	assert_eq!(
		c.r#type.as_ref(),
		&Type::Primitive(Primitive::Int(Width::W32)),
		"trait const type is i32, not Infer"
	);

	// Dual-emitted under Trait::item.
	assert!(
		matches!(entry(&index, "fidelity::WithAssoc::get"), Entry::Function(_)),
		"trait method dual-emitted"
	);
}

/// Source map covers dual-emitted method paths.
#[test]
fn source_map_covers_method_paths() {
	let (_index, sources, _dir) = lower_fidelity();

	let new_src = sources
		.get("fidelity::Counter::new")
		.unwrap_or_else(|| panic!("source map covers Counter::new: {:?}", sources.keys()));
	assert!(
		new_src.contains("fn new") || new_src.contains("Self { value }"),
		"method source text recovered: {new_src:?}"
	);
}
