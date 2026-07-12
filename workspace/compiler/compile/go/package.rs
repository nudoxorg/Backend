//! Resolving a Go module: go.mod discovery and oracle invocation.
//!
//! A Go "package" in registry terms is a *module* — the unit rooted at a
//! `go.mod`. This module locates that root, parses the module path and
//! Go version out of `go.mod` (the two directives we need; the full
//! grammar is delegated to the toolchain), and runs the vendored oracle
//! over the tree.
//!
//! ## Oracle invocation
//! The oracle (`//workspace/compiler/compile/go/oracle:oracle`) is built
//! ahead of time by Buck2 and shipped as a `resources` artifact alongside
//! this binary — see `producer::buck_resource`. Invocation runs the
//! prebuilt binary directly against the target module root; there is no
//! runtime compile step.

use std::fs;
use std::path::{Path, PathBuf};

use crate::compile::isolate::{self, IsolatedCommand, IsolatedFailureKind};
use crate::compile::producer;
use sandbox::ProducerProfile;

use super::error::{GoError, Result};

use super::oracle;

/// Resolve the Buck2-built oracle binary shipped alongside this executable.
pub fn oracle_binary() -> Result<PathBuf> {
	producer::buck_resource("go-oracle").map_err(|source| GoError::ResourceNotFound { source })
}

/// go.mod-derived metadata for a module on disk.
#[derive(Debug, Clone)]
pub struct GoModule {
	/// The directory containing `go.mod`.
	pub root: PathBuf,

	/// The module path (the `module` directive), e.g.
	/// `github.com/user/repo/v2`.
	pub module_path: String,

	/// The `go` directive, when present (e.g. `"1.23"`).
	pub go_version: Option<String>,
}

/// Locate the nearest `go.mod` at or above `start` and parse it.
pub fn discover_module(start: &Path) -> Result<GoModule> {
	let go_mod = find_go_mod(start).ok_or_else(|| GoError::NoGoMod { start: start.to_path_buf() })?;
	parse_go_mod(&go_mod)
}

/// Walk from `start` upward to the filesystem root looking for `go.mod`.
fn find_go_mod(start: &Path) -> Option<PathBuf> {
	let mut dir = if start.is_file() { start.parent()? } else { start };
	loop {
		let candidate = dir.join("go.mod");
		if candidate.is_file() {
			return Some(candidate);
		}
		dir = dir.parent()?;
	}
}

/// Parse the `module` and `go` directives out of a `go.mod` file.
///
/// This is intentionally a minimal line-oriented parse — the oracle (via
/// the Go toolchain) is the authority on full go.mod semantics; we only
/// need the module path for identifier qualification and the language
/// version for reporting.
pub fn parse_go_mod(path: &Path) -> Result<GoModule> {
	let text = fs::read_to_string(path)
		.map_err(|source| GoError::ReadGoMod { path: path.to_path_buf(), source })?;

	let mut module_path = None;
	let mut go_version = None;

	for line in text.lines() {
		let line = strip_line_comment(line).trim();
		if let Some(rest) = line.strip_prefix("module ") {
			module_path = Some(rest.trim().trim_matches('"').to_string());
		} else if let Some(rest) = line.strip_prefix("go ") {
			go_version = Some(rest.trim().to_string());
		}
	}

	let module_path = module_path
		.ok_or_else(|| GoError::NoModuleDirective { path: path.to_path_buf() })?;
	let root = path
		.parent()
		.map(Path::to_path_buf)
		.unwrap_or_else(|| PathBuf::from("."));

	Ok(GoModule { root, module_path, go_version })
}

/// Drop a trailing `// ...` comment from a go.mod line.
fn strip_line_comment(line: &str) -> &str {
	match line.find("//") {
		Some(idx) => &line[..idx],
		None => line,
	}
}

/// Run the vendored oracle over the module rooted at `module_root` and
/// deserialize its JSON document.
///
/// The oracle is the Buck2-built binary shipped as a resource alongside this
/// executable (see [`oracle_binary`]). Diagnostics stream to the oracle's
/// stderr and are surfaced in the error on failure; load errors that still
/// produced a document are carried in [`oracle::Output::errors`].
pub fn run_oracle<C: crate::compile::producer::ForgeContext>(
	ctx: &C,
	module_root: &Path,
) -> Result<oracle::Output> {
	let bin = oracle_binary()?;
	let target = module_root
		.canonicalize()
		.map_err(|source| GoError::ResolveModuleRoot { root: module_root.to_path_buf(), source })?;

	// Network off + GOPROXY=off: the oracle's own deps were compiled in by
	// Buck2; at runtime it only needs to read the target module's own cache.
	//
	// The sealed env has no ambient `$HOME`, so `go` cannot derive its default
	// `GOCACHE`/`GOPATH` and aborts ("build cache is required"). Point both (and
	// `HOME`) at scratch under the temp dir, which is already an RW mount.
	let scratch = std::env::temp_dir();
	let go_cache = scratch.join("nudox-go-build-cache");
	let go_path = scratch.join("nudox-go-path");
	let output = isolate::run_isolated(
		ctx,
		IsolatedCommand::new(&bin, ProducerProfile::Go)
			.arg(&target)
			.env("GOWORK", "off")
			.env("GOFLAGS", "-mod=mod")
			.env("GOPROXY", "off")
			.env("GOCACHE", go_cache)
			.env("GOPATH", go_path)
			.env("HOME", scratch.clone())
			.ro(&bin)
			.ro(&target)
			.rw(scratch),
	)
	.map_err(|e| match e.kind {
		IsolatedFailureKind::ToolchainMissing(_) | IsolatedFailureKind::Sandbox(_) => {
			GoError::SpawnOracle {
				source: std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
			}
		}
		_ => GoError::OracleExecution {
			target: target.clone(),
			status: e.kind.to_string(),
			stderr: e.stderr.unwrap_or_default(),
		},
	})?;

	if !output.status.success() {
		return Err(GoError::OracleExecution {
			target,
			status: output.status.to_string(),
			stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
		});
	}

	serde_json::from_slice(&output.stdout).map_err(|source| GoError::OracleOutputParse {
		source,
		stdout: Some(String::from_utf8_lossy(&output.stdout).to_string()),
	})
}
