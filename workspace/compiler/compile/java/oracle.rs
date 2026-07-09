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

use crate::compile::isolate::{self, IsolatedCommand, IsolatedFailure, IsolatedFailureKind};
use crate::compile::producer;
use sandbox::ProducerProfile;

use super::schema;
use super::error::{DocletError, ExtractionError, JavadocError, OracleError};

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
pub fn extract(source_roots: &[PathBuf]) -> Result<schema::Extraction, OracleError> {
	let files = collect_sources(source_roots);
	if files.is_empty() {
		return Err(OracleError::NoJavaSources {
			roots: source_roots.to_vec(),
		});
	}
	let classes = compile_oracle()?;
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
pub fn compile_oracle() -> Result<PathBuf, OracleError> {
	let dir = materialize_oracle()?;
	let classes = dir.join("classes");
	let stamp = dir.join(".compiled");
	if stamp.is_file() && classes.is_dir() {
		return Ok(classes);
	}

	fs::create_dir_all(&classes).map_err(|source| DocletError::CreateClassesDirFailed {
		path: classes.clone(),
		source,
	})?;

	// Network is already Off via IsolatedCommand (design §8). Annotation
	// processors run inside the cage with no ambient net.
	let mut cmd = IsolatedCommand::new("javac", ProducerProfile::Java)
		.arg("-encoding")
		.arg("UTF-8")
		.arg("-d")
		.arg(&classes)
		.ro(&dir)
		.rw(&classes)
		.rw(std::env::temp_dir());
	for (name, _) in ORACLE_SOURCES {
		cmd = cmd.arg(dir.join(name));
	}
	let output = isolate::run_isolated(cmd).map_err(|e| map_java_spawn(e, "javac"))?;
	if !output.status.success() {
		return Err(DocletError::DocletCompileFailed {
			status: output.status.to_string(),
			stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
			stdout: Some(String::from_utf8_lossy(&output.stdout).trim().to_string()),
		}
		.into());
	}

	fs::write(&stamp, b"ok").map_err(|source| DocletError::WriteStampFailed {
		path: stamp.clone(),
		source,
	})?;
	Ok(classes)
}

/// Write the embedded sources into a stable, content-addressed temp
/// directory and return it. Idempotent; the directory name keys the exact
/// vendored revision, so an existing copy is always current.
pub fn materialize_oracle() -> Result<PathBuf, DocletError> {
	producer::materialize_oracle(ORACLE_SOURCES, "java")
		.map(|p| p.dir)
		.map_err(|e| DocletError::MaterializeSourceFailed {
			path: std::env::temp_dir().join("nudox-java-oracle"),
			source: std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
		})
}

/// Stable FNV-1a hash over the embedded sources (shared substrate).
fn oracle_hash() -> u64 {
	producer::oracle_hash(ORACLE_SOURCES)
}

/// Run `javadoc -doclet` over the collected files and parse the document.
fn run_doclet(classes: &Path, files: &[PathBuf]) -> Result<schema::Extraction, OracleError> {
	// Fresh per-run scratch: the argfile and the JSON out-path.
	let scratch = tempdir_for_run()?;
	let argfile = scratch.join("sources.args");
	let outfile = scratch.join("extraction.json");

	let mut listing = String::new();
	for file in files {
		listing.push_str(&argfile_quote(&file.display().to_string()));
		listing.push('\n');
	}
	fs::write(&argfile, listing).map_err(|source| JavadocError::WriteArgfileFailed {
		path: argfile.clone(),
		source,
	})?;

	// Source roots must be RO-visible; scratch + classes RW.
	let mut cmd = IsolatedCommand::new("javadoc", ProducerProfile::Java)
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
		.ro(classes)
		.rw(&scratch)
		.rw(std::env::temp_dir());
	for file in files {
		if let Some(parent) = file.parent() {
			cmd = cmd.ro(parent);
		}
	}
	let output = isolate::run_isolated(cmd).map_err(|e| map_javadoc_spawn(e))?;

	if !output.status.success() {
		return Err(JavadocError::JavadocOracleFailed {
			status: output.status.to_string(),
			stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
			stdout: Some(String::from_utf8_lossy(&output.stdout).trim().to_string()),
		}
		.into());
	}

	let json = fs::read(&outfile).map_err(|source| JavadocError::ReadOracleOutputFailed {
		path: outfile.clone(),
		source,
	})?;
	let extraction: schema::Extraction = serde_json::from_slice(&json)?;

	// Validate against patterns in the Java extractor (format, presence of
	// types, TypeMirror::Error nodes that indicate resolution failure in
	// javax.lang.model).
	if extraction.format != 1 {
		return Err(ExtractionError::UnsupportedFormat { format: extraction.format }.into());
	}
	if extraction.types.is_empty() {
		return Err(ExtractionError::NoTypesExtracted.into());
	}
	for t in &extraction.types {
		// Surface TypeMirror::Error from the Java oracle (Extractor walks
		// javax.lang.model and emits Error{name: sourceText} for unresolvable
		// in API surface positions such as throws).
		for m in &t.methods {
			for th in &m.thrown {
				if let schema::TypeMirror::Error { name } = th {
					return Err(ExtractionError::TypeMirrorErrorInApi { name: name.clone() }.into());
				}
			}
		}
	}

	// Best-effort cleanup; the scratch dir is per-run and disposable.
	let _ = fs::remove_dir_all(&scratch);

	Ok(extraction)
}

/// A unique per-run scratch directory under the system temp dir.
fn tempdir_for_run() -> Result<PathBuf, JavadocError> {
	let dir = std::env::temp_dir().join(format!(
		"nudox-java-run-{}-{:x}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos())
			.unwrap_or(0)
	));
	fs::create_dir_all(&dir).map_err(|source| JavadocError::TempDirFailed {
		path: dir.clone(),
		source,
	})?;
	Ok(dir)
}

/// Quote one path for a javadoc @argfile: tokens are whitespace-separated,
/// double quotes group, backslash escapes.
fn argfile_quote(path: &str) -> String {
	let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
	format!("\"{escaped}\"")
}

fn map_java_spawn(err: IsolatedFailure, _tool: &str) -> OracleError {
	match err.kind {
		IsolatedFailureKind::ToolchainMissing(_) | IsolatedFailureKind::Sandbox(_) => {
			DocletError::SpawnJavacFailed {
				source: std::io::Error::new(std::io::ErrorKind::Other, err.to_string()),
			}
			.into()
		}
		_ => DocletError::DocletCompileFailed {
			status: err.kind.to_string(),
			stderr: err.stderr.unwrap_or_default(),
			stdout: err.stdout,
		}
		.into(),
	}
}

fn map_javadoc_spawn(err: IsolatedFailure) -> OracleError {
	match err.kind {
		IsolatedFailureKind::ToolchainMissing(_) | IsolatedFailureKind::Sandbox(_) => {
			JavadocError::SpawnJavadocFailed {
				source: std::io::Error::new(std::io::ErrorKind::Other, err.to_string()),
			}
			.into()
		}
		_ => JavadocError::JavadocOracleFailed {
			status: err.kind.to_string(),
			stderr: err.stderr.unwrap_or_default(),
			stdout: err.stdout,
		}
		.into(),
	}
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
