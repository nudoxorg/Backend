//! The dream, against a live emit (REFERENCES-PLAN §6.4): generate the
//! `references` fixture through the real pipeline, project it to the graph, and
//! assert the call graph is reified — `hello` calls `yo`, `report` calls
//! `hello` — as `Reference` edges between fully-qualified symbols.
//!
//! Default producer is rust-analyzer (same as `generate_blob`); the fixture is
//! copied into a tempdir so cargo's `target/` never pollutes the repo.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use compiler::error::GenerateError;
use compiler::generate::linked_data::{emit, DocumentSink};
use compiler::generate::{self, PackageInput};
use compiler::graph::link::PackageCtx;
use heart::{Edition, Language, PackageVersion, RegistryOrigin, Toolchain};
use ir::entry::NudoxPath;
use ir::syntax::{Confidence, Role};
use registry::identity::PackageCoordinates;
use registry::package::PackageName;
use serde_json::Value;
use tempfile::TempDir;

fn fixture_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust/references")
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

fn package_input(dir: &TempDir) -> PackageInput {
	copy_tree(&fixture_root(), dir.path()).expect("fixture copies");
	fs::write(
		dir.path().join("Cargo.toml"),
		r#"[package]
name = "refsfixture"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
	)
	.expect("root Cargo.toml");
	PackageInput {
		coordinates: PackageCoordinates {
			origin: RegistryOrigin::CratesIo,
			name: PackageName::new(Language::Rust, "refsfixture").expect("valid package name"),
			version: PackageVersion::try_from((Language::Rust, "0.1.0")).expect("valid version"),
		},
		toolchain: Toolchain::Rust {
			compiler: semver::Version::new(1, 85, 0),
			edition: Edition::E2021,
		},
		root: dir.path().to_path_buf(),
	}
}

/// Copy the multi-module `wsrefs` fixture (with its `helper` path-dependency)
/// into a tempdir and build a `PackageInput`.
fn ws_package_input(dir: &TempDir) -> PackageInput {
	copy_tree(
		&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust/wsrefs"),
		dir.path(),
	)
	.expect("fixture copies");
	fs::write(
		dir.path().join("Cargo.toml"),
		r#"[package]
name = "wsrefs"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[dependencies]
helper = { path = "helper" }
"#,
	)
	.expect("root Cargo.toml");
	fs::write(
		dir.path().join("helper/Cargo.toml"),
		r#"[package]
name = "helper"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
	)
	.expect("helper Cargo.toml");
	PackageInput {
		coordinates: PackageCoordinates {
			origin: RegistryOrigin::CratesIo,
			name: PackageName::new(Language::Rust, "wsrefs").expect("valid package name"),
			version: PackageVersion::try_from((Language::Rust, "0.1.0")).expect("valid version"),
		},
		toolchain: Toolchain::Rust {
			compiler: semver::Version::new(1, 85, 0),
			edition: Edition::E2021,
		},
		root: dir.path().to_path_buf(),
	}
}

#[derive(Default)]
struct Collector {
	docs: Vec<Value>,
}

impl DocumentSink for Collector {
	fn write(&mut self, document: Value) -> Result<(), GenerateError> {
		self.docs.push(document);
		Ok(())
	}

	fn flush(&mut self) -> Result<(), GenerateError> {
		Ok(())
	}
}

/// The `@id` a link field points at, whether serialized as a bare string or a
/// `{"@id": …}` object.
fn link_id(v: &Value) -> Option<String> {
	match v {
		Value::String(s) => Some(s.clone()),
		Value::Object(o) => o.get("@id").and_then(Value::as_str).map(str::to_string),
		_ => None,
	}
}

/// Last `::`-segment of an fq name.
fn leaf(fq: &str) -> &str {
	fq.rsplit("::").next().unwrap_or(fq)
}

#[test]
fn call_graph_is_reified_as_reference_edges() {
	let dir = TempDir::new().expect("tempdir");
	let input = package_input(&dir);
	let generated = generate::generate(&input).expect("generation succeeds");

	// The occurrence corpus itself must carry the definitions and the resolved
	// calls (independent of the graph projection).
	assert!(
		!generated.occurrences.files.is_empty(),
		"the references fixture must yield occurrences"
	);

	let ctx = PackageCtx {
		language: "rust".into(),
		package: "refsfixture".into(),
		version: Some("0.1.0".into()),
	};
	let mut sink = Collector::default();
	emit(&generated.surface, &generated.occurrences, ctx, &mut sink).expect("emission succeeds");

	// Map every emitted Symbol's @id → fq_name.
	let id_to_fq: HashMap<String, String> = sink
		.docs
		.iter()
		.filter(|d| d["@type"] == "Symbol")
		.filter_map(|d| {
			let id = d.get("@id").and_then(Value::as_str)?.to_string();
			let fq = d.get("fq_name").and_then(Value::as_str)?.to_string();
			Some((id, fq))
		})
		.collect();

	// Gather Reference edges as (source-leaf, target-leaf, kind-json).
	let mut edges: Vec<(String, String)> = Vec::new();
	for d in sink.docs.iter().filter(|d| d["@type"] == "Reference") {
		let (Some(s), Some(t)) = (d.get("source").and_then(link_id), d.get("target").and_then(link_id))
		else {
			continue;
		};
		// FunctionCall edges only (TerminusDBModel lowercases enum variants).
		if d.get("kind").and_then(Value::as_str) != Some("functioncall") {
			continue;
		}
		let sfq = id_to_fq.get(&s).map(|f| leaf(f).to_string()).unwrap_or_default();
		let tfq = id_to_fq.get(&t).map(|f| leaf(f).to_string()).unwrap_or_default();
		edges.push((sfq, tfq));
	}

	assert!(
		edges.iter().any(|(s, t)| s == "hello" && t == "yo"),
		"expected a FunctionCall edge hello → yo, got {edges:?}"
	);
	assert!(
		edges.iter().any(|(s, t)| s == "report" && t == "hello"),
		"expected a FunctionCall edge report → hello, got {edges:?}"
	);
}

/// The last `::`-segment of a [`NudoxPath`]'s spelling.
fn nudox_leaf(p: &NudoxPath) -> String {
	let s = match p {
		NudoxPath::Local(pb) => pb.to_string_lossy().into_owned(),
		NudoxPath::External { dependency, path } => {
			let ps = path.to_string_lossy();
			if ps.is_empty() { dependency.clone() } else { format!("{dependency}::{ps}") }
		}
	};
	s.rsplit("::").next().unwrap_or(&s).to_string()
}

/// Cross-module resolution in a workspace crate: `app::run` and `util::u` both
/// call `math::add` across module boundaries — one via a crate-absolute path
/// (`Index` tier), one via a `use` import (`Import` tier) — and the whole thing
/// lives in a workspace with a `helper` path-dependency.
#[test]
fn cross_module_references_resolve_across_modules() {
	let dir = TempDir::new().expect("tempdir");
	let input = ws_package_input(&dir);
	let generated = generate::generate(&input).expect("generation succeeds");

	// Collect resolved reference occurrences as (enclosing-leaf, target-leaf, tier).
	let mut refs: Vec<(String, String, Confidence)> = Vec::new();
	for file in &generated.occurrences.files {
		for o in &file.occurrences {
			if o.role != Role::Reference {
				continue;
			}
			if let Some(encl) = &o.enclosing {
				refs.push((nudox_leaf(encl), nudox_leaf(&o.target), o.confidence));
			}
		}
	}

	// `app::run` reaches `math::add` by its crate-absolute path → Index tier.
	assert!(
		refs.iter().any(|(s, t, c)| s == "run" && t == "add" && *c == Confidence::Index),
		"expected run → add at Index (crate::math::add), got {refs:?}"
	);
	// `util::u` reaches `math::add` through a `use` binding → Import tier.
	assert!(
		refs.iter().any(|(s, t, c)| s == "u" && t == "add" && *c == Confidence::Import),
		"expected u → add at Import (use crate::math::add), got {refs:?}"
	);

	// And the graph reifies both cross-module edges.
	let ctx = PackageCtx {
		language: "rust".into(),
		package: "wsrefs".into(),
		version: Some("0.1.0".into()),
	};
	let mut sink = Collector::default();
	emit(&generated.surface, &generated.occurrences, ctx, &mut sink).expect("emission succeeds");

	let id_to_fq: HashMap<String, String> = sink
		.docs
		.iter()
		.filter(|d| d["@type"] == "Symbol")
		.filter_map(|d| {
			Some((
				d.get("@id").and_then(Value::as_str)?.to_string(),
				d.get("fq_name").and_then(Value::as_str)?.to_string(),
			))
		})
		.collect();

	let mut edges: Vec<(String, String)> = Vec::new();
	for d in sink.docs.iter().filter(|d| d["@type"] == "Reference") {
		if d.get("kind").and_then(Value::as_str) != Some("functioncall") {
			continue;
		}
		let (Some(s), Some(t)) = (d.get("source").and_then(link_id), d.get("target").and_then(link_id))
		else {
			continue;
		};
		let sfq = id_to_fq.get(&s).map(|f| leaf(f).to_string()).unwrap_or_default();
		let tfq = id_to_fq.get(&t).map(|f| leaf(f).to_string()).unwrap_or_default();
		edges.push((sfq, tfq));
	}

	assert!(
		edges.iter().any(|(s, t)| s == "run" && t == "add"),
		"expected a graph edge run → add, got {edges:?}"
	);
	assert!(
		edges.iter().any(|(s, t)| s == "u" && t == "add"),
		"expected a graph edge u → add, got {edges:?}"
	);
}
