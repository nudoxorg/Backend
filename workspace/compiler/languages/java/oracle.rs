//! Materializing, compiling, and running the vendored javadoc doclet.
//!
//! The doclet sources (`oracle/nudox/oracle/*.java`) are embedded into this
//! binary at compile time and written into a content-addressed temp
//! directory on first use, so the producer behaves identically under cargo,
//! buck, or a bare binary — no source checkout is assumed at runtime. The
//! same directory caches the compiled classes: a `.compiled` stamp makes
//! repeat runs skip `javac` entirely (the directory name encodes the exact
//! source revision, so present == current).
//!
//! Invocation is two steps:
//!   1. `javac -d <dir>/classes <sources>` — the doclet has zero
//!      dependencies beyond the JDK itself (JDK 17+; the doclet uses
//!      `getPermittedSubclasses`, records, and pattern matching).
//!   2. `javadoc -doclet nudox.oracle.Extractor -docletpath <dir>/classes
//!      -private -quiet -encoding UTF-8 -outfile <json> @<argfile>` over
//!      every `.java` file found under the requested source roots.
//!
//! `-private` includes every declaration regardless of access — lowering
//! (not extraction) is where policy lives. The file list rides an @argfile
//! to dodge OS argv limits; entries are quoted per javadoc's argfile rules.
//!
//! Both tools come from `PATH`. JEP 467 Markdown doc comments (`///`) are
//! only *parsed as documentation* by JDK ≥ 23 toolchains — an older JDK
//! silently reports `doc: null` for them, so prefer a current JDK.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::schema;

/// The embedded doclet, written out verbatim before compiling.
const ORACLE_SOURCES: &[(&str, &str)] = &[
	("nudox/oracle/Extractor.java", include_str!("oracle/nudox/oracle/Extractor.java")),
	("nudox/oracle/Json.java", include_str!("oracle/nudox/oracle/Json.java")),
];

/// Guidance appended to toolchain-spawn failures.
const TOOLCHAIN_HINT: &str =
	"is a JDK (17+, ideally 23+ for Markdown doc comments) on PATH? \
	 e.g. `nix shell nixpkgs#jdk`";

/// Run the oracle over `source_roots` and deserialize its JSON document.
///
/// This is the one-call entry point: it materializes + compiles the doclet
/// (cached), collects the source set, runs `javadoc`, and parses the
/// emitted document into the [`schema`] mirror.
pub fn extract(source_roots: &[PathBuf]) -> Result<schema::Extraction> {
	let files = collect_sources(source_roots);
	if files.is_empty() {
		bail!(
			"no .java sources found under {}",
			source_roots
				.iter()
				.map(|p| p.display().to_string())
				.collect::<Vec<_>>()
				.join(", ")
		);
	}
	let classes = compile_oracle().context("preparing the vendored javadoc doclet")?;
	run_doclet(&classes, &files)
}

/// Every `.java` file under the roots, sorted for determinism. Hidden
/// directories and build outputs (`target/`, `build/`, `out/`) are skipped;
/// `package-info.java` is kept (package docs), `module-info.java` is kept
/// (JPMS metadata).
pub fn collect_sources(source_roots: &[PathBuf]) -> Vec<PathBuf> {
	let mut files = Vec::new();
	for root in source_roots {
		collect_java_files(root, &mut files);
	}
	files.sort();
	files.dedup();
	files
}

fn collect_java_files(dir: &Path, out: &mut Vec<PathBuf>) {
	let Ok(entries) = fs::read_dir(dir) else {
		return;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		let Ok(file_type) = entry.file_type() else {
			continue;
		};
		if file_type.is_dir() {
			if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
				if name.starts_with('.') || matches!(name, "target" | "build" | "out") {
					continue;
				}
			}
			collect_java_files(&path, out);
		} else if file_type.is_file()
			&& path.extension().and_then(|e| e.to_str()) == Some("java")
		{
			out.push(path);
		}
	}
}

/// Materialize the embedded doclet sources into their content-addressed
/// directory and compile them (cached via a `.compiled` stamp). Returns the
/// classes directory for `-docletpath`.
pub fn compile_oracle() -> Result<PathBuf> {
	let dir = materialize_oracle()?;
	let classes = dir.join("classes");
	let stamp = dir.join(".compiled");
	if stamp.is_file() && classes.is_dir() {
		return Ok(classes);
	}

	fs::create_dir_all(&classes)
		.with_context(|| format!("creating {}", classes.display()))?;

	let mut cmd = Command::new("javac");
	cmd.arg("-encoding").arg("UTF-8").arg("-d").arg(&classes);
	for (name, _) in ORACLE_SOURCES {
		cmd.arg(dir.join(name));
	}
	let output = cmd
		.output()
		.with_context(|| format!("spawning `javac` — {TOOLCHAIN_HINT}"))?;
	if !output.status.success() {
		bail!(
			"compiling the javadoc doclet failed ({}): {}",
			output.status,
			String::from_utf8_lossy(&output.stderr).trim()
		);
	}

	fs::write(&stamp, b"ok").with_context(|| format!("writing {}", stamp.display()))?;
	Ok(classes)
}

/// Write the embedded sources into a stable, content-addressed temp
/// directory and return it. Idempotent; the directory name keys the exact
/// vendored revision, so an existing copy is always current.
pub fn materialize_oracle() -> Result<PathBuf> {
	let dir =
		std::env::temp_dir().join(format!("nudox-java-oracle-{:016x}", oracle_hash()));
	for (name, contents) in ORACLE_SOURCES {
		let path = dir.join(name);
		if path.is_file() {
			continue;
		}
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)
				.with_context(|| format!("creating {}", parent.display()))?;
		}
		fs::write(&path, contents)
			.with_context(|| format!("writing oracle source {}", path.display()))?;
	}
	Ok(dir)
}

/// Stable FNV-1a hash over the embedded sources, keying the materialization
/// directory to the vendored revision (mirrors the Go producer).
fn oracle_hash() -> u64 {
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
	for (name, contents) in ORACLE_SOURCES {
		for byte in name.bytes().chain(contents.bytes()) {
			hash ^= byte as u64;
			hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
		}
	}
	hash
}

/// Run `javadoc -doclet` over the collected files and parse the document.
fn run_doclet(classes: &Path, files: &[PathBuf]) -> Result<schema::Extraction> {
	// Fresh per-run scratch: the argfile and the JSON out-path.
	let scratch = tempdir_for_run()?;
	let argfile = scratch.join("sources.args");
	let outfile = scratch.join("extraction.json");

	let mut listing = String::new();
	for file in files {
		listing.push_str(&argfile_quote(&file.display().to_string()));
		listing.push('\n');
	}
	fs::write(&argfile, listing)
		.with_context(|| format!("writing argfile {}", argfile.display()))?;

	let output = Command::new("javadoc")
		.arg("-doclet")
		.arg("nudox.oracle.Extractor")
		.arg("-docletpath")
		.arg(classes)
		.arg("-private")
		.arg("-quiet")
		.arg("-encoding")
		.arg("UTF-8")
		.arg("-outfile")
		.arg(&outfile)
		.arg(format!("@{}", argfile.display()))
		.output()
		.with_context(|| format!("spawning `javadoc` — {TOOLCHAIN_HINT}"))?;

	if !output.status.success() {
		bail!(
			"javadoc oracle failed ({}): {}",
			output.status,
			String::from_utf8_lossy(&output.stderr).trim()
		);
	}

	let json = fs::read(&outfile).with_context(|| {
		format!("reading oracle output {} (doclet ran but wrote nothing?)", outfile.display())
	})?;
	let extraction: schema::Extraction =
		serde_json::from_slice(&json).context("parsing oracle JSON output")?;

	// Best-effort cleanup; the scratch dir is per-run and disposable.
	let _ = fs::remove_dir_all(&scratch);

	Ok(extraction)
}

/// A unique per-run scratch directory under the system temp dir.
fn tempdir_for_run() -> Result<PathBuf> {
	let dir = std::env::temp_dir().join(format!(
		"nudox-java-run-{}-{:x}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos())
			.unwrap_or(0)
	));
	fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
	Ok(dir)
}

/// Quote one path for a javadoc @argfile: tokens are whitespace-separated,
/// double quotes group, backslash escapes.
fn argfile_quote(path: &str) -> String {
	let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
	format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn argfile_quoting() {
		assert_eq!(argfile_quote("/plain/File.java"), "\"/plain/File.java\"");
		assert_eq!(
			argfile_quote("/with space/A \"b\".java"),
			"\"/with space/A \\\"b\\\".java\""
		);
	}

	#[test]
	fn oracle_hash_is_stable_within_a_build() {
		assert_eq!(oracle_hash(), oracle_hash());
	}
}
