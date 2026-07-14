//! Resolving a C# project: layout discovery, csproj coordinates, source
//! roots, and the extract-and-lower entry point (CSHARP-PLAN §3.2).
//!
//! A C# "package" in registry terms is a *project* — the unit rooted at a
//! `*.csproj` (SDK-style) or a plain source tree. Discovery answers two
//! questions:
//!
//! 1. **Where is the project root?** The nearest ancestor carrying a
//!    `*.csproj` / `*.sln` / `Directory.Build.props`; a bare source tree is
//!    its own root.
//! 2. **Which directories are source roots?** The project directory (SDK-style
//!    projects glob every `.cs` recursively, skipping `bin/`, `obj/`,
//!    `artifacts/`).
//!
//! The csproj is read with a hand-rolled minimal XML scan (the workspace has
//! no XML dependency), capturing `PackageId`, `Version`/`VersionPrefix`,
//! `TargetFramework(s)`, and `RootNamespace`. Property interpolation
//! (`$(...)`) is NOT performed.

use std::fs;
use std::path::{Path, PathBuf};

use ir::entry::Index;

use super::context;
use super::error::{CSharpError, CSharpPackageError};
use super::oracle;

/// How the project is built (drives source-root discovery only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
	/// SDK-style project (`*.csproj` with implicit `.cs` globbing).
	Sdk,
	/// A bare source tree with no manifest.
	Plain,
}

/// The handful of csproj leaves this producer reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CsprojInfo {
	pub package_id: Option<String>,
	pub assembly_name: Option<String>,
	pub version: Option<String>,
	pub version_prefix: Option<String>,
	pub target_frameworks: Vec<String>,
	pub root_namespace: Option<String>,
}

impl CsprojInfo {
	/// The effective package name: `PackageId`, else `AssemblyName`.
	pub fn name(&self) -> Option<&str> {
		self.package_id.as_deref().or(self.assembly_name.as_deref())
	}

	/// The effective version: `Version`, else `VersionPrefix`.
	pub fn effective_version(&self) -> Option<&str> {
		self.version.as_deref().or(self.version_prefix.as_deref())
	}
}

/// A discovered C# project.
#[derive(Debug, Clone)]
pub struct CSharpProject {
	pub root: PathBuf,
	pub layout: Layout,
	pub info: CsprojInfo,
	pub source_roots: Vec<PathBuf>,
}

/// Discover, extract, and lower the C# project at (or above) `root`.
pub fn lower_package<C: crate::compile::producer::ForgeContext>(
	ctx: &C,
	root: &Path,
) -> Result<Index, CSharpError> {
	let project = discover_project(root)?;
	let extraction = oracle::extract(ctx, &project.source_roots).map_err(|source| {
		CSharpPackageError::OracleExtractFailed { root: project.root.clone(), source }
	})?;
	Ok(context::lower_extraction(&extraction))
}

/// Locate the project root at or above `start` and assemble its source roots.
pub fn discover_project(start: &Path) -> Result<CSharpProject, CSharpError> {
	let start_dir = if start.is_file() {
		start.parent().unwrap_or(Path::new("."))
	} else {
		start
	};
	if !start_dir.is_dir() {
		return Err(CSharpPackageError::NotDirectory { path: start_dir.to_path_buf() }.into());
	}

	// Walk upward for a manifest.
	let mut dir = start_dir;
	let (root, layout, csproj) = loop {
		if let Some(csproj) = find_csproj(dir) {
			break (dir.to_path_buf(), Layout::Sdk, Some(csproj));
		}
		if dir.join("Directory.Build.props").is_file() || has_solution(dir) {
			break (dir.to_path_buf(), Layout::Sdk, None);
		}
		match dir.parent() {
			Some(parent) => dir = parent,
			None => break (start_dir.to_path_buf(), Layout::Plain, None),
		}
	};

	let info = match &csproj {
		Some(path) => {
			let text = fs::read_to_string(path).map_err(|source| {
				CSharpPackageError::ProjectReadFailed { path: path.clone(), source }
			})?;
			parse_csproj(&text)
		}
		None => CsprojInfo::default(),
	};

	let source_roots = source_roots(&root);
	if source_roots.is_empty() {
		let layout_name = match layout {
			Layout::Sdk => "SDK",
			Layout::Plain => "Plain",
		};
		return Err(CSharpPackageError::NoSourceRoots {
			root: root.clone(),
			layout: layout_name.to_string(),
		}
		.into());
	}

	Ok(CSharpProject { root, layout, info, source_roots })
}

/// The first `*.csproj` directly under `dir`, if any.
fn find_csproj(dir: &Path) -> Option<PathBuf> {
	let mut found: Option<PathBuf> = None;
	for entry in fs::read_dir(dir).ok()?.flatten() {
		let path = entry.path();
		if path.extension().and_then(|e| e.to_str()) == Some("csproj") {
			// Prefer a lexicographically-first csproj for determinism.
			match &found {
				Some(existing) if existing <= &path => {}
				_ => found = Some(path),
			}
		}
	}
	found
}

fn has_solution(dir: &Path) -> bool {
	fs::read_dir(dir)
		.map(|entries| {
			entries.flatten().any(|e| {
				e.path().extension().and_then(|x| x.to_str()) == Some("sln")
			})
		})
		.unwrap_or(false)
}

/// The source roots: the project root (SDK-style globs recursively). A bare
/// `src/` is preferred when present.
fn source_roots(root: &Path) -> Vec<PathBuf> {
	let src = root.join("src");
	if src.is_dir() && contains_cs(&src, 0) {
		return vec![src];
	}
	if contains_cs(root, 0) {
		return vec![root.to_path_buf()];
	}
	Vec::new()
}

/// Whether any `.cs` file lives under `dir` (bounded depth; skips build dirs).
fn contains_cs(dir: &Path, depth: usize) -> bool {
	if depth > 12 {
		return false;
	}
	let Ok(entries) = fs::read_dir(dir) else {
		return false;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("cs") {
			return true;
		}
		if path.is_dir() {
			if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
				if name.starts_with('.') || is_build_dir(name) {
					continue;
				}
			}
			if contains_cs(&path, depth + 1) {
				return true;
			}
		}
	}
	false
}

/// Whether a directory name is a build-output directory to skip.
pub fn is_build_dir(name: &str) -> bool {
	matches!(name, "bin" | "obj" | "artifacts" | "node_modules")
}

// ---------------------------------------------------------------------------
// Minimal csproj extraction
// ---------------------------------------------------------------------------

/// Extract [`CsprojInfo`] from csproj text with a minimal element-path scanner
/// (no XML dependency). Comments are skipped; only first occurrences win.
pub fn parse_csproj(text: &str) -> CsprojInfo {
	let mut info = CsprojInfo::default();
	let mut stack: Vec<String> = Vec::new();
	let bytes = text.as_bytes();
	let mut i = 0usize;

	while i < bytes.len() {
		let Some(open) = text[i..].find('<').map(|o| i + o) else {
			break;
		};

		// Capture text content for tracked leaves.
		if let Some(slot) = tracked_slot(&stack) {
			let content = text[i..open].trim();
			if !content.is_empty() {
				apply_slot(&mut info, slot, content);
			}
		}

		let rest = &text[open..];
		if rest.starts_with("<!--") {
			match text[open + 4..].find("-->") {
				Some(end) => {
					i = open + 4 + end + 3;
					continue;
				}
				None => break,
			}
		}
		if rest.starts_with("<?") || rest.starts_with("<!") {
			match text[open..].find('>') {
				Some(end) => {
					i = open + end + 1;
					continue;
				}
				None => break,
			}
		}

		let Some(close) = text[open..].find('>').map(|o| open + o) else {
			break;
		};
		let tag_body = &text[open + 1..close];

		if let Some(name) = tag_body.strip_prefix('/') {
			let name = name.trim();
			if stack.last().is_some_and(|top| top == name) {
				stack.pop();
			}
		} else {
			let self_closing = tag_body.ends_with('/');
			let name = tag_body
				.trim_end_matches('/')
				.split(|c: char| c.is_whitespace())
				.next()
				.unwrap_or("")
				.to_string();
			if !name.is_empty() && !self_closing {
				stack.push(name);
			}
		}

		i = close + 1;
	}

	info
}

/// Which slot the current element path (a `PropertyGroup` leaf) feeds.
fn tracked_slot(stack: &[String]) -> Option<&'static str> {
	// Match `Project/PropertyGroup/<Leaf>` regardless of intervening depth.
	let leaf = stack.last()?;
	let parent = stack.iter().rev().nth(1)?;
	if parent != "PropertyGroup" {
		return None;
	}
	match leaf.as_str() {
		"PackageId" => Some("PackageId"),
		"AssemblyName" => Some("AssemblyName"),
		"Version" => Some("Version"),
		"VersionPrefix" => Some("VersionPrefix"),
		"TargetFramework" => Some("TargetFramework"),
		"TargetFrameworks" => Some("TargetFrameworks"),
		"RootNamespace" => Some("RootNamespace"),
		_ => None,
	}
}

fn apply_slot(info: &mut CsprojInfo, slot: &str, content: &str) {
	let value = decode_xml_entities(content);
	match slot {
		"PackageId" if info.package_id.is_none() => info.package_id = Some(value),
		"AssemblyName" if info.assembly_name.is_none() => info.assembly_name = Some(value),
		"Version" if info.version.is_none() => info.version = Some(value),
		"VersionPrefix" if info.version_prefix.is_none() => info.version_prefix = Some(value),
		"RootNamespace" if info.root_namespace.is_none() => info.root_namespace = Some(value),
		"TargetFramework" | "TargetFrameworks" if info.target_frameworks.is_empty() => {
			info.target_frameworks =
				value.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
		}
		_ => {}
	}
}

fn decode_xml_entities(text: &str) -> String {
	text.replace("&amp;", "&")
		.replace("&lt;", "<")
		.replace("&gt;", ">")
		.replace("&quot;", "\"")
		.replace("&apos;", "'")
}

// ---------------------------------------------------------------------------
// Tests (pure — no toolchain required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn csproj_coordinates() {
		let csproj = r#"<Project Sdk="Microsoft.NET.Sdk">
	<PropertyGroup>
		<TargetFramework>net10.0</TargetFramework>
		<PackageId>Acme.Widgets</PackageId>
		<Version>1.2.3</Version>
		<RootNamespace>Acme.Widgets</RootNamespace>
	</PropertyGroup>
</Project>"#;
		let info = parse_csproj(csproj);
		assert_eq!(info.name(), Some("Acme.Widgets"));
		assert_eq!(info.effective_version(), Some("1.2.3"));
		assert_eq!(info.target_frameworks, vec!["net10.0".to_string()]);
		assert_eq!(info.root_namespace.as_deref(), Some("Acme.Widgets"));
	}

	#[test]
	fn version_prefix_and_multi_tfm() {
		let csproj = r#"<Project>
	<PropertyGroup>
		<AssemblyName>Lib</AssemblyName>
		<VersionPrefix>2.0.0</VersionPrefix>
		<TargetFrameworks>net10.0;net8.0</TargetFrameworks>
	</PropertyGroup>
</Project>"#;
		let info = parse_csproj(csproj);
		assert_eq!(info.name(), Some("Lib"));
		assert_eq!(info.effective_version(), Some("2.0.0"));
		assert_eq!(info.target_frameworks, vec!["net10.0".to_string(), "net8.0".to_string()]);
	}
}
