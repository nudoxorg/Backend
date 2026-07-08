//! Resolving a Rust package + its documented local/workspace members, running
//! `cargo rustdoc --output-format json`, and reading the result.
//!
//! The pipeline hands this module an already-materialized source root (see
//! `crate::generate::PackageInput`); acquisition (registry download, VCS
//! checkout) happens upstream. `cargo metadata` is driven through the CLI and
//! deserialized into the minimal mirror structs below rather than pulling in
//! the full `cargo_metadata` crate.

use std::{collections::{BTreeSet, VecDeque}, fs, path::{Path, PathBuf}, process::Command, sync::Arc};

use ir::pipeline::{Collected, Ir};
use rustc_hash::FxHashMap as HashMap;
use semver::Version;
use serde::Deserialize;
use tracing::{debug, info, instrument};

use super::{context::RustdocParser, error::Package};

/// A Rust package to document from a local source tree.
#[derive(Clone, Debug)]
pub struct RustPackage {
	/// The (root) package name as `cargo metadata` reports it.
	pub name: String,

	/// Direct-repo mode: run the `--document-private-items` pass and pull the
	/// workspace's local library dependencies into the documented set.
	pub direct_repo: bool,
}

// ─── Minimal `cargo metadata` mirror ─────────────────────────────────────────
//
// Only the fields this module reads; everything else in the (large) metadata
// JSON is ignored.

type PackageId = String;

#[derive(Deserialize)]
struct Metadata {
	packages:       Vec<CargoPackage>,
	resolve:        Option<Resolve>,
	workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct CargoPackage {
	id:            PackageId,
	name:          String,
	manifest_path: PathBuf,
	targets:       Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
	name: String,
	kind: Vec<String>,
}

#[derive(Deserialize)]
struct Resolve {
	nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
	id:   PackageId,
	deps: Vec<Dep>,
}

#[derive(Deserialize)]
struct Dep {
	pkg: PackageId,
}

impl RustPackage {
	fn cargo_package_spec_for(package_name: &str, version: &Version) -> String {
		format!("{package_name}@{version}")
	}

	fn run_cargo_rustdoc(
		&self,
		code: &Path,
		package_name: &str,
		version: &Version,
		lib_only: bool,
	) -> std::result::Result<std::process::Output, Package> {
		let mut command = Command::new("cargo");
		command
			.arg("rustdoc")
			.arg("--package")
			.arg(Self::cargo_package_spec_for(package_name, version));
		if lib_only {
			command.arg("--lib");
		}
		command
			.arg("--")
			.args(self.direct_repo.then_some("--document-private-items"))
			.arg("-Z")
			.arg("unstable-options")
			.arg("--output-format")
			.arg("json")
			.current_dir(code);

		let output = command.output()?;
		if output.status.success() {
			return Ok(output);
		}

		let stderr = summarize_command_output(&output.stderr);
		let stdout = summarize_command_output(&output.stdout);
		let details = if !stderr.is_empty() {
			format!(": {stderr}")
		} else if !stdout.is_empty() {
			format!(": {stdout}")
		} else {
			String::new()
		};

		Err(Package::Process {
			command: if lib_only { "cargo rustdoc --lib".into() } else { "cargo rustdoc".into() },
			status: output.status,
			details,
		})
	}

	#[instrument(skip_all, fields(package = %self.name))]
	fn generate_ir_for_package(
		&self,
		code: &Path,
		package_name: &str,
		doc_target_name: &str,
		version: &Version,
	) -> std::result::Result<(Ir<Collected>, HashMap<String, String>), Package> {
		match self.run_cargo_rustdoc(code, package_name, version, false) {
			Ok(_) => {}
			Err(Package::Process { details, .. })
				if details.contains("extra arguments to `rustdoc` can only be passed to one target") =>
			{
				self.run_cargo_rustdoc(code, package_name, version, true)?;
			}
			Err(error) => return Err(error),
		}

		let json_path =
			code.join("target").join("doc").join(format!("{}.json", doc_target_name.replace('-', "_")));

		let json_bytes = fs::read(&json_path)?;
		let rustdoc_crate: rustdoc_types::Crate = serde_json::from_slice(&json_bytes)?;
		drop(json_bytes);
		debug!("rustdoc JSON parsed");

		let source_map = source_map_from_crate(&rustdoc_crate, code);

		let mut parser = RustdocParser::from_doc(rustdoc_crate)?;

		let parse_result = parser.parse()?;
		info!(entries = parse_result.len(), "IR generation complete");

		Ok((Ir::from_entries(parse_result), source_map))
	}

	pub fn generate_ir_with_sources(
		&self,
		code: &Path,
		version: &Version,
	) -> std::result::Result<(Ir<Collected>, HashMap<String, String>), Package> {
		let metadata = cargo_metadata(code)?;
		let packages = documented_local_packages(&metadata, &self.name, self.direct_repo);

		// Each documented package drives an independent `cargo rustdoc`
		// subprocess + JSON parse + IR walk, sequentially — cargo itself
		// already parallelizes the expensive part (the build).
		let mut entries = Vec::new();
		let mut source_map = HashMap::default();
		for package_id in &packages {
			let package = metadata
				.packages
				.iter()
				.find(|candidate| candidate.id == *package_id)
				.expect("documented package id should exist in metadata");
			let doc_target_name =
				package_doc_target_name(package).unwrap_or_else(|| package.name.clone());
			let (package_ir, package_sources) =
				self.generate_ir_for_package(code, &package.name, &doc_target_name, version)?;
			entries.extend(package_ir.into_entries());
			source_map.extend(package_sources);
		}

		Ok((Ir::from_entries(entries), source_map))
	}

	pub fn generate_ir(
		&self,
		code: &Path,
		version: &Version,
	) -> std::result::Result<Ir<Collected>, Package> {
		self.generate_ir_with_sources(code, version).map(|(ir, _)| ir)
	}
}

fn cargo_metadata(code: &Path) -> std::result::Result<Metadata, Package> {
	let output = Command::new("cargo")
		.arg("metadata")
		.arg("--format-version")
		.arg("1")
		.current_dir(code)
		.output()?;

	if !output.status.success() {
		return Err(Package::Metadata(summarize_command_output(&output.stderr)));
	}

	serde_json::from_slice(&output.stdout).map_err(|source| Package::Metadata(source.to_string()))
}

fn source_map_from_crate(
	krate: &rustdoc_types::Crate,
	workspace: &Path,
) -> HashMap<String, String> {
	let mut map = HashMap::default();
	// Many functions share the same source file. Read each file at most once and
	// keep its contents alongside the byte offset of every line start, so each
	// function's span can be sliced out without re-reading or re-splitting the
	// file. `None` marks files that failed to read so we don't retry them.
	let mut file_cache: HashMap<PathBuf, Option<(Arc<str>, Vec<usize>)>> = HashMap::default();

	for (id, item) in &krate.index {
		if !matches!(&item.inner, rustdoc_types::ItemEnum::Function(_)) {
			continue;
		}
		let Some(span) = &item.span else { continue };
		let Some(summary) = krate.paths.get(id) else { continue };
		let fq_name = summary.path.join("::");

		let source_file = workspace.join(&span.filename);
		let cached = file_cache.entry(source_file.clone()).or_insert_with(|| {
			let source = fs::read_to_string(&source_file).ok()?;
			let offsets = line_start_offsets(&source);
			Some((Arc::<str>::from(source), offsets))
		});
		let Some((source, line_starts)) = cached.as_ref() else { continue };

		// `span.begin.0` / `span.end.0` are 1-based line numbers; slice lines
		// `begin..=end` by byte range, then normalize with `.lines()` (which
		// strips `\r`) over just the small slice.
		let Some(&start) = line_starts.get(span.begin.0.saturating_sub(1)) else { continue };
		let end = line_starts.get(span.end.0).copied().unwrap_or(source.len());
		let raw: String = source[start..end].lines().collect::<Vec<_>>().join("\n");

		if !raw.is_empty() {
			map.insert(fq_name, raw);
		}
	}
	map
}

/// Byte offset of the start of each line (line `n`, 1-based, begins at
/// `offsets[n - 1]`). Splitting on `\n` matches `str::lines` line boundaries.
fn line_start_offsets(source: &str) -> Vec<usize> {
	let mut offsets = vec![0usize];
	for (idx, byte) in source.bytes().enumerate() {
		if byte == b'\n' {
			offsets.push(idx + 1);
		}
	}
	offsets
}

fn documented_local_packages(
	metadata: &Metadata,
	root_package_name: &str,
	direct_repo: bool,
) -> Vec<PackageId> {
	let Some(root_package) =
		metadata.packages.iter().find(|package| package.name == root_package_name)
	else {
		return Vec::new();
	};
	let mut documented = vec![root_package.id.clone()];
	if !direct_repo || package_has_library(root_package) {
		return documented;
	}

	let Some(resolve) = &metadata.resolve else {
		return documented;
	};
	let package_map: HashMap<_, _> =
		metadata.packages.iter().map(|package| (&package.id, package)).collect();
	let node_map: HashMap<_, _> = resolve.nodes.iter().map(|node| (&node.id, node)).collect();
	let workspace_root = metadata.workspace_root.as_path();
	let mut seen = BTreeSet::from([root_package.id.clone()]);
	let mut queue = VecDeque::from([root_package.id.clone()]);

	while let Some(package_id) = queue.pop_front() {
		let Some(node) = node_map.get(&package_id) else {
			continue;
		};

		for dependency in &node.deps {
			let dependency_id = &dependency.pkg;
			if !seen.insert(dependency_id.clone()) {
				continue;
			}
			queue.push_back(dependency_id.clone());

			let Some(package) = package_map.get(dependency_id) else {
				continue;
			};
			if !package.manifest_path.starts_with(workspace_root) {
				continue;
			}
			if package_has_library(package) {
				documented.push(package.id.clone());
			}
		}
	}

	documented
}

fn package_has_library(package: &CargoPackage) -> bool {
	package.targets.iter().any(|target| target.kind.iter().any(|k| is_library_target_kind(k)))
}

fn package_doc_target_name(package: &CargoPackage) -> Option<String> {
	package
		.targets
		.iter()
		.find(|target| target.kind.iter().any(|k| is_library_target_kind(k)))
		.or_else(|| {
			package
				.targets
				.iter()
				.find(|target| !target.kind.iter().any(|kind| kind == "custom-build"))
		})
		.map(|target| target.name.clone())
}

fn is_library_target_kind(kind: &str) -> bool {
	matches!(kind, "lib" | "rlib" | "staticlib" | "cdylib" | "dylib")
}

fn summarize_command_output(bytes: &[u8]) -> String {
	let text = String::from_utf8_lossy(bytes);
	let trimmed = text.trim();
	if trimmed.is_empty() {
		return String::new();
	}

	const LIMIT: usize = 2_000;
	if trimmed.len() <= LIMIT {
		return trimmed.to_owned();
	}

	let mut end = LIMIT;
	while !trimmed.is_char_boundary(end) {
		end -= 1;
	}

	format!("{}...", &trimmed[..end])
}

#[cfg(test)]
mod tests {
	use semver::Version;

	use super::RustPackage;

	#[test]
	fn cargo_package_spec_is_version_qualified() {
		assert_eq!(
			RustPackage::cargo_package_spec_for("serde_json", &Version::parse("1.0.82").unwrap()),
			"serde_json@1.0.82"
		);
	}
}
