//! Running the C# Roslyn oracle.
//!
//! The oracle (`//workspace/compiler/compile/csharp/oracle:oracle`) is a
//! framework-dependent `dotnet publish` directory built ahead of time by
//! Buck2 and shipped as a `resources` artifact alongside this binary — see
//! `producer::buck_resource`. There is no runtime `dotnet build` step.
//!
//! Invocation (mode S — sealed source tree):
//! `dotnet <publish>/oracle.dll --mode source --root <dir>... --out <json>`.
//! `dotnet` comes from `PATH` (the sandboxed toolchain); `DOTNET_CLI_HOME` is
//! pointed into per-run scratch (dotnet needs a writable HOME — the same class
//! of fix as the GOCACHE sandbox-PATH issue).

use std::fs;
use std::path::{Path, PathBuf};

use sandbox::ProducerProfile;

use crate::compile::isolate::{self, IsolatedCommand, IsolatedFailure, IsolatedFailureKind};
use crate::compile::producer;

use super::error::{DotnetError, ExtractionError, OracleError, PublishError};
use super::schema;

/// Resolve the Buck2-built oracle publish directory and its `oracle.dll`.
fn oracle_dll() -> Result<PathBuf, PublishError> {
	let publish_dir = producer::buck_resource("csharp-oracle")
		.map_err(|source| PublishError::ResourceNotFound { source })?;
	let dll = publish_dir.join("oracle.dll");
	if !dll.is_file() {
		return Err(PublishError::MissingEntrypoint { path: publish_dir });
	}
	Ok(dll)
}

/// Run the oracle (mode S) over `source_roots` and deserialize its JSON.
pub fn extract<C: crate::compile::producer::ForgeContext>(
	ctx: &C,
	source_roots: &[PathBuf],
) -> Result<schema::Extraction, OracleError> {
	if source_roots.is_empty() || !any_sources(source_roots) {
		return Err(OracleError::NoSources { roots: source_roots.to_vec() });
	}
	let dll = oracle_dll()?;
	run_oracle(ctx, &dll, source_roots)
}

/// Whether any `.cs` file exists under the roots.
fn any_sources(roots: &[PathBuf]) -> bool {
	roots.iter().any(|r| contains_cs(r, 0))
}

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
				if name.starts_with('.') || super::package::is_build_dir(name) {
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

/// Run `dotnet oracle.dll --mode source ...` and parse the document.
fn run_oracle<C: crate::compile::producer::ForgeContext>(
	ctx: &C,
	dll: &Path,
	source_roots: &[PathBuf],
) -> Result<schema::Extraction, OracleError> {
	let scratch = tempdir_for_run()?;
	let outfile = scratch.join("extraction.json");
	let dotnet_home = scratch.join("dotnet-home");
	fs::create_dir_all(&dotnet_home).map_err(|source| DotnetError::ScratchDirFailed {
		path: dotnet_home.clone(),
		source,
	})?;

	let publish_dir = dll.parent().unwrap_or(Path::new("."));

	let mut cmd = IsolatedCommand::new("dotnet", ProducerProfile::CSharp)
		.arg(dll)
		.arg("--mode")
		.arg("source");
	for root in source_roots {
		cmd = cmd.arg("--root").arg(root);
	}
	cmd = cmd
		.arg("--out")
		.arg(&outfile)
		.env("DOTNET_CLI_HOME", &dotnet_home)
		.env("DOTNET_NOLOGO", "1")
		.env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
		.env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
		.ro(publish_dir)
		.rw(&scratch)
		.rw(std::env::temp_dir());
	for root in source_roots {
		cmd = cmd.ro(root);
	}

	let output = isolate::run_isolated(ctx, cmd).map_err(map_dotnet_spawn)?;

	if !output.status.success() {
		return Err(DotnetError::OracleFailed {
			status: output.status.to_string(),
			stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
			stdout: Some(String::from_utf8_lossy(&output.stdout).trim().to_string()),
		}
		.into());
	}

	let json = fs::read(&outfile).map_err(|source| DotnetError::ReadOracleOutputFailed {
		path: outfile.clone(),
		source,
	})?;
	let extraction: schema::Extraction = serde_json::from_slice(&json)?;

	if extraction.format != 1 {
		return Err(ExtractionError::UnsupportedFormat { format: extraction.format }.into());
	}
	if extraction.types.is_empty() {
		return Err(ExtractionError::NoTypesExtracted.into());
	}

	let _ = fs::remove_dir_all(&scratch);

	Ok(extraction)
}

/// A unique per-run scratch directory under the system temp dir.
fn tempdir_for_run() -> Result<PathBuf, DotnetError> {
	use std::sync::atomic::{AtomicU64, Ordering};
	static SEQ: AtomicU64 = AtomicU64::new(0);
	let dir = std::env::temp_dir().join(format!(
		"nudox-csharp-run-{}-{:x}-{}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos())
			.unwrap_or(0),
		SEQ.fetch_add(1, Ordering::Relaxed),
	));
	fs::create_dir_all(&dir).map_err(|source| DotnetError::ScratchDirFailed {
		path: dir.clone(),
		source,
	})?;
	Ok(dir)
}

fn map_dotnet_spawn(err: IsolatedFailure) -> OracleError {
	match err.kind {
		IsolatedFailureKind::ToolchainMissing(_) | IsolatedFailureKind::Sandbox(_) => {
			DotnetError::SpawnDotnetFailed {
				source: std::io::Error::other(err.to_string()),
			}
			.into()
		}
		_ => DotnetError::OracleFailed {
			status: err.kind.to_string(),
			stderr: err.stderr.unwrap_or_default(),
			stdout: err.stdout,
		}
		.into(),
	}
}
