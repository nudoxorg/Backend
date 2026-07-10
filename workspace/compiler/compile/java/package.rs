//! Resolving a Java project: layout discovery, Maven coordinates, source
//! roots, and the extract-and-lower entry point.
//!
//! A Java "package" in registry terms is a *project* — the unit rooted at a
//! `pom.xml` (Maven), a `build.gradle`/`settings.gradle` (Gradle), or a
//! plain source tree. Discovery here answers two questions:
//!
//! 1. **Where is the project root?** The nearest ancestor of the given path
//!    carrying a build manifest; a bare source tree is its own root.
//! 2. **Which directories are source roots?** Maven's
//!    `<build><sourceDirectory>` when declared; otherwise every
//!    `src/main/java` under the root (which handles both multi-module Maven
//!    and multi-project Gradle uniformly); otherwise `src/`; otherwise the
//!    root itself.
//!
//! Maven coordinates (`groupId:artifactId:version`) are read from
//! `pom.xml` with a **hand-rolled minimal XML scan** — the workspace has no
//! XML dependency, and the oracle/toolchain remains the authority on full
//! POM semantics. The scanner tracks the element path, skips comments and
//! CDATA, and captures only the handful of `project/...` leaf values this
//! producer needs (including the `<parent>` fallback for group/version).
//! Property interpolation (`${...}`) is NOT performed; interpolated values
//! are surfaced verbatim.
//!
//! The public entry point, [`lower_package`], mirrors
//! `python::PythonContext::lower_package`: discover → run the oracle
//! (`oracle::extract`) → lower into an [`Index`] (`context`).

use std::fs;
use std::path::{Path, PathBuf};

use ir::entry::Index;

use super::context;
use super::oracle;
use super::error::{JavaError, JavaPackageError, OracleError};

/// How the project is built (drives source-root discovery only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
	Maven,
	Gradle,
	Plain,
}

/// Maven coordinates, as declared (uninterpolated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MavenCoordinates {
	pub group_id:    String,
	pub artifact_id: String,
	pub version:     String,
}

/// A discovered Java project.
#[derive(Debug, Clone)]
pub struct JavaProject {
	/// The project root (where the manifest lives, or the given tree).
	pub root: PathBuf,

	/// The detected build layout.
	pub layout: Layout,

	/// Maven coordinates, when a `pom.xml` declares them.
	pub coordinates: Option<MavenCoordinates>,

	/// The directories whose `.java` trees feed the oracle.
	pub source_roots: Vec<PathBuf>,
}

/// Discover, extract, and lower the Java project at (or above) `root` into
/// a single [`Index`] — the producer's public entry point.
pub fn lower_package(
	ctx: &dyn crate::compile::producer::ForgeContext,
	root: &Path,
) -> Result<Index, JavaError> {
	let project = discover_project(root)?;
	let extraction = oracle::extract(ctx, &project.source_roots).map_err(|source| {
		JavaPackageError::OracleExtractFailed { root: project.root.clone(), source }
	})?;
	Ok(context::lower_extraction(&extraction))
}

/// Locate the project root at or above `start` and assemble its source
/// roots. `start` may be the root itself, a subdirectory, or a file.
pub fn discover_project(start: &Path) -> Result<JavaProject, JavaError> {
	let start_dir = if start.is_file() {
		start.parent().unwrap_or(Path::new("."))
	} else {
		start
	};
	if !start_dir.is_dir() {
		return Err(JavaPackageError::NotDirectory { path: start_dir.to_path_buf() }.into());
	}

	// Walk upward looking for a build manifest.
	let mut dir = start_dir;
	let (root, layout) = loop {
		if dir.join("pom.xml").is_file() {
			break (dir.to_path_buf(), Layout::Maven);
		}
		if has_gradle_manifest(dir) {
			break (dir.to_path_buf(), Layout::Gradle);
		}
		match dir.parent() {
			Some(parent) => dir = parent,
			// No manifest anywhere above: a plain source tree.
			None => break (start_dir.to_path_buf(), Layout::Plain),
		}
	};

	let coordinates = match layout {
		Layout::Maven => {
			let pom_path = root.join("pom.xml");
			// Explicit guard + error (NoPom variant) even though is_file was
			// true at discovery time (race or removal).
			if !pom_path.is_file() {
				return Err(JavaPackageError::NoPom { path: pom_path.clone() }.into());
			}
			let text = fs::read_to_string(&pom_path).map_err(|source| {
				JavaPackageError::PomReadFailed { path: pom_path.clone(), source }
			})?;
			parse_pom(&text).coordinates()
		}
		_ => None,
	};

	let source_roots = source_roots(&root, layout);
	if source_roots.is_empty() {
		let layout_name = match layout {
			Layout::Maven => "Maven",
			Layout::Gradle => "Gradle",
			Layout::Plain => "Plain",
		};
		return Err(JavaPackageError::NoSourceRootsDetailed {
			root: root.clone(),
			layout: layout_name.to_string(),
		}
		.into());
	}

	Ok(JavaProject { root, layout, coordinates, source_roots })
}

fn has_gradle_manifest(dir: &Path) -> bool {
	["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"]
		.iter()
		.any(|name| dir.join(name).is_file())
}

/// Assemble the source roots for a project root (see the module doc).
fn source_roots(root: &Path, layout: Layout) -> Vec<PathBuf> {
	// An explicit Maven <sourceDirectory> wins outright.
	if layout == Layout::Maven {
		if let Ok(text) = fs::read_to_string(root.join("pom.xml")) {
			if let Some(dir) = parse_pom(&text).source_directory {
				let resolved = root.join(dir);
				if resolved.is_dir() {
					return vec![resolved];
				}
			}
		}
	}

	// The conventional layout, found recursively so multi-module Maven and
	// multi-project Gradle both work without parsing module lists.
	let mut conventional = Vec::new();
	find_conventional_roots(root, 0, &mut conventional);
	if !conventional.is_empty() {
		conventional.sort();
		return conventional;
	}

	// A bare `src/` tree, then the root itself.
	let src = root.join("src");
	if src.is_dir() && contains_java(&src, 0) {
		return vec![src];
	}
	if contains_java(root, 0) {
		return vec![root.to_path_buf()];
	}
	Vec::new()
}

/// Collect every `src/main/java` directory under `dir` (bounded depth).
fn find_conventional_roots(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
	if depth > 8 {
		return;
	}
	let candidate = dir.join("src").join("main").join("java");
	if candidate.is_dir() {
		out.push(candidate);
	}
	let Ok(entries) = fs::read_dir(dir) else {
		return;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		if !path.is_dir() {
			continue;
		}
		if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
			if name.starts_with('.')
				|| matches!(name, "src" | "target" | "build" | "out" | "node_modules")
			{
				continue;
			}
		}
		find_conventional_roots(&path, depth + 1, out);
	}
}

/// Whether any `.java` file lives under `dir` (bounded depth).
fn contains_java(dir: &Path, depth: usize) -> bool {
	if depth > 12 {
		return false;
	}
	let Ok(entries) = fs::read_dir(dir) else {
		return false;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("java") {
			return true;
		}
		if path.is_dir() {
			if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
				if name.starts_with('.') || matches!(name, "target" | "build" | "out") {
					continue;
				}
			}
			if contains_java(&path, depth + 1) {
				return true;
			}
		}
	}
	false
}

// ---------------------------------------------------------------------------
// Minimal pom.xml extraction
// ---------------------------------------------------------------------------

/// The handful of POM leaves this producer reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PomInfo {
	pub group_id:         Option<String>,
	pub artifact_id:      Option<String>,
	pub version:          Option<String>,
	pub parent_group_id:  Option<String>,
	pub parent_version:   Option<String>,
	pub source_directory: Option<String>,
}

impl PomInfo {
	/// Effective coordinates, applying the Maven parent-inheritance rule
	/// for `groupId`/`version`.
	pub fn coordinates(&self) -> Option<MavenCoordinates> {
		let group_id = self.group_id.clone().or_else(|| self.parent_group_id.clone())?;
		let artifact_id = self.artifact_id.clone()?;
		let version = self.version.clone().or_else(|| self.parent_version.clone())?;
		Some(MavenCoordinates { group_id, artifact_id, version })
	}
}

/// Extract [`PomInfo`] from `pom.xml` text with a minimal element-path
/// scanner (no XML dependency in the workspace — see the module doc).
/// Comments and CDATA are skipped; only first occurrences are captured.
pub fn parse_pom(text: &str) -> PomInfo {
	let mut info = PomInfo::default();
	let mut stack: Vec<String> = Vec::new();
	let bytes = text.as_bytes();
	let mut i = 0usize;

	while i < bytes.len() {
		let Some(open) = text[i..].find('<').map(|o| i + o) else {
			break;
		};

		// Capture text content between this element's tags when the current
		// path is one we track.
		if let Some(target) = tracked_slot(&mut info, &stack) {
			if target.is_none() {
				let content = text[i..open].trim();
				if !content.is_empty() {
					*target = Some(decode_xml_entities(content));
				}
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
		if rest.starts_with("<![CDATA[") {
			match text[open + 9..].find("]]>") {
				Some(end) => {
					i = open + 9 + end + 3;
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
			// Closing tag.
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

/// The `PomInfo` slot the current element path feeds, if any.
fn tracked_slot<'a>(
	info: &'a mut PomInfo,
	stack: &[String],
) -> Option<&'a mut Option<String>> {
	let path: Vec<&str> = stack.iter().map(String::as_str).collect();
	match path.as_slice() {
		["project", "groupId"] => Some(&mut info.group_id),
		["project", "artifactId"] => Some(&mut info.artifact_id),
		["project", "version"] => Some(&mut info.version),
		["project", "parent", "groupId"] => Some(&mut info.parent_group_id),
		["project", "parent", "version"] => Some(&mut info.parent_version),
		["project", "build", "sourceDirectory"] => Some(&mut info.source_directory),
		_ => None,
	}
}

/// Decode the XML entities that plausibly appear in coordinate text.
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
	fn pom_coordinates() {
		let pom = r#"<?xml version="1.0"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
	<!-- a <groupId>fake</groupId> in a comment -->
	<modelVersion>4.0.0</modelVersion>
	<groupId>com.example</groupId>
	<artifactId>widget</artifactId>
	<version>1.2.3</version>
	<dependencies>
		<dependency>
			<groupId>junit</groupId>
			<artifactId>junit</artifactId>
			<version>4.13.2</version>
		</dependency>
	</dependencies>
</project>"#;
		let info = parse_pom(pom);
		assert_eq!(
			info.coordinates(),
			Some(MavenCoordinates {
				group_id:    "com.example".to_string(),
				artifact_id: "widget".to_string(),
				version:     "1.2.3".to_string(),
			})
		);
	}

	#[test]
	fn pom_parent_inheritance_and_source_dir() {
		let pom = r#"<project>
	<parent>
		<groupId>com.example.parent</groupId>
		<artifactId>parent</artifactId>
		<version>9.9.9</version>
	</parent>
	<artifactId>child</artifactId>
	<build>
		<sourceDirectory>src/java</sourceDirectory>
	</build>
</project>"#;
		let info = parse_pom(pom);
		let coords = info.coordinates().expect("parent-inherited coordinates");
		assert_eq!(coords.group_id, "com.example.parent");
		assert_eq!(coords.artifact_id, "child");
		assert_eq!(coords.version, "9.9.9");
		assert_eq!(info.source_directory.as_deref(), Some("src/java"));
	}

	#[test]
	fn pom_without_coordinates_yields_none() {
		assert_eq!(parse_pom("<project></project>").coordinates(), None);
	}

	#[test]
	fn entities_decode() {
		let pom = "<project><groupId>a&amp;b</groupId><artifactId>x</artifactId><version>1</version></project>";
		assert_eq!(parse_pom(pom).group_id.as_deref(), Some("a&b"));
	}
}
