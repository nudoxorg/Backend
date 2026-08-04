//! Running the vendored javadoc doclet.
//!
//! The doclet (`//workspace/compiler/compile/java/oracle:extractor`) is
//! built ahead of time by Buck2 as a plain `java_library` (it has no
//! dependencies beyond the JDK itself — JDK 17+; the doclet uses
//! `getPermittedSubclasses`, records, and pattern matching) and shipped as a
//! `resources` artifact jar alongside this binary — see
//! `producer::buck_resource`. There is no runtime `javac` step.
//!
//! Invocation: `javadoc -doclet nudox.oracle.Extractor -docletpath <jar>
//! -private -quiet -encoding UTF-8 -outfile <json> @<argfile>` over every
//! `.java` file found under the requested source roots.
//!
//! `-private` includes every declaration regardless of access — lowering
//! (not extraction) is where policy lives. The file list rides an @argfile
//! to dodge OS argv limits; entries are quoted per javadoc's argfile rules.
//!
//! `javadoc` comes from `PATH` (via the sandboxed toolchain). JEP 467
//! Markdown doc comments (`///`) are only *parsed as documentation* by
//! JDK ≥ 23 toolchains — an older JDK silently reports `doc: null` for
//! them, so prefer a current JDK.

use std::fs;
use std::path::{Path, PathBuf};

use crate::compile::isolate::{self, IsolatedCommand, IsolatedFailure, IsolatedFailureKind};
use crate::compile::producer;
use sandbox::ProducerProfile;

use super::schema;
use super::error::{DocletError, ExtractionError, JavadocError, OracleError};

/// Resolve the Buck2-built doclet jar shipped alongside this executable.
fn doclet_jar() -> Result<PathBuf, DocletError> {
	producer::buck_resource("java-oracle.jar").map_err(|source| DocletError::ResourceNotFound { source })
}

/// Run the oracle over `source_roots` and deserialize its JSON document.
///
/// This is the one-call entry point: it collects the source set, runs
/// `javadoc` against the prebuilt doclet jar, and parses the emitted
/// document into the [`schema`] mirror.
pub fn extract<C: crate::compile::producer::ForgeContext>(
	ctx: &C,
	source_roots: &[PathBuf],
) -> Result<schema::Extraction, OracleError> {
	let files = collect_sources(source_roots);
	if files.is_empty() {
		return Err(OracleError::NoJavaSources {
			roots: source_roots.to_vec(),
		});
	}
	let jar = doclet_jar()?;
	run_doclet(ctx, &jar, &files)
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

/// Run `javadoc -doclet` over the collected files and parse the document.
fn run_doclet<C: crate::compile::producer::ForgeContext>(
	ctx: &C,
	jar: &Path,
	files: &[PathBuf],
) -> Result<schema::Extraction, OracleError> {
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

	// Source roots must be RO-visible; scratch RW; the prebuilt doclet jar RO.
	let mut cmd = IsolatedCommand::new("javadoc", ProducerProfile::Java)
		.arg("-doclet")
		.arg("nudox.oracle.Extractor")
		.arg("-docletpath")
		.arg(jar)
		.arg("-private")
		.arg("-quiet")
		.arg("-encoding")
		.arg("UTF-8")
		.arg("-outfile")
		.arg(&outfile)
		.arg(format!("@{}", argfile.display()))
		.ro(jar)
		.rw(&scratch)
		.rw(std::env::temp_dir());
	for file in files {
		if let Some(parent) = file.parent() {
			cmd = cmd.ro(parent);
		}
	}
	let output = isolate::run_isolated(ctx, cmd).map_err(|e| map_javadoc_spawn(e))?;

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
///
/// The name mixes pid, a nanosecond clock, and a process-global atomic counter
/// so concurrent runs in one process (e.g. parallel producer jobs) can never
/// collide on the same dir even if the clock reads the same nanosecond twice.
fn tempdir_for_run() -> Result<PathBuf, JavadocError> {
	use std::sync::atomic::{AtomicU64, Ordering};
	static SEQ: AtomicU64 = AtomicU64::new(0);
	let dir = std::env::temp_dir().join(format!(
		"nudox-java-run-{}-{:x}-{}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos())
			.unwrap_or(0),
		SEQ.fetch_add(1, Ordering::Relaxed),
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
}
