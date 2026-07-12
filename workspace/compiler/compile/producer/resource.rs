//! Resolving Buck2 `resources` artifacts materialized alongside this binary.
//!
//! When a `rust_library`/`rust_binary` target declares a `resources = {...}`
//! dict, Buck2 writes a `<binary-name>.resources.json` manifest next to the
//! final executable, mapping `"<declaring-package>/<name>"` to a path
//! relative to the executable's directory (see
//! `build/prelude-local/rust/rust_binary.bzl`). This is the runtime-side
//! half of that contract: no cargo fallback exists because this workspace
//! has no cargo build — Buck2 is the only way these binaries get built.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// The BUCK package that declares the `go-oracle` / `java-oracle.jar`
/// resources (`workspace/compiler/BUCK`'s `compiler` rust_crate).
const RESOURCE_PACKAGE: &str = "workspace/compiler";

/// Resolve the on-disk path of a Buck2 `resources` entry named `name`,
/// declared in `workspace/compiler/BUCK`'s `resources = {...}` dict.
pub fn buck_resource(name: &str) -> io::Result<PathBuf> {
	let exe = std::env::current_exe()?;
	let file_name = exe
		.file_name()
		.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "current_exe has no file name"))?
		.to_string_lossy()
		.into_owned();
	let manifest_path = exe.with_file_name(format!("{file_name}.resources.json"));

	let contents = std::fs::read_to_string(&manifest_path).map_err(|source| {
		io::Error::new(
			source.kind(),
			format!("reading buck2 resources manifest {}: {source}", manifest_path.display()),
		)
	})?;
	let manifest: HashMap<String, String> = serde_json::from_str(&contents).map_err(|e| {
		io::Error::new(
			io::ErrorKind::InvalidData,
			format!("parsing buck2 resources manifest {}: {e}", manifest_path.display()),
		)
	})?;

	let key = format!("{RESOURCE_PACKAGE}/{name}");
	let relative = manifest.get(&key).ok_or_else(|| {
		io::Error::new(
			io::ErrorKind::NotFound,
			format!("resource `{key}` not found in {}", manifest_path.display()),
		)
	})?;

	let base = exe.parent().unwrap_or_else(|| Path::new("."));
	Ok(base.join(relative))
}
