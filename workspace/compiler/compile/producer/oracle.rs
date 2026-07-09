//! Shared content-addressed materialization for vendored oracles (Go + Java).

use std::fs;
use std::path::{Path, PathBuf};

use super::ProducerError;

/// Path to a materialised oracle tree (content-addressed under temp).
#[derive(Debug, Clone)]
pub struct OraclePath {
	/// Directory holding the embedded sources (and optional build products).
	pub dir: PathBuf,
}

impl OraclePath {
	/// Borrow the directory.
	pub fn as_path(&self) -> &Path {
		&self.dir
	}
}

impl AsRef<Path> for OraclePath {
	fn as_ref(&self) -> &Path {
		&self.dir
	}
}

/// Write embedded oracle sources into a stable, content-addressed temp dir.
///
/// `label` is a short tag (`"go"`, `"java"`) used in the directory name.
/// The directory name keys the exact FNV-1a hash of `(name, contents)` pairs,
/// so an existing copy is always current for this binary revision.
pub fn materialize(files: &[(&str, &str)], label: &str) -> Result<OraclePath, ProducerError> {
	let hash = oracle_hash(files);
	let dir = std::env::temp_dir().join(format!("nudox-{label}-oracle-{hash:016x}"));
	fs::create_dir_all(&dir)?;

	for (name, contents) in files {
		let path = dir.join(name);
		// Content-addressed dir ⇒ present == current.
		if path.is_file() {
			continue;
		}
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::write(&path, contents)?;
	}
	Ok(OraclePath { dir })
}

/// Stable FNV-1a over embedded sources (keys materialization directories).
pub fn oracle_hash(files: &[(&str, &str)]) -> u64 {
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
	for (name, contents) in files {
		for byte in name.bytes().chain(contents.bytes()) {
			hash ^= byte as u64;
			hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
		}
	}
	hash
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hash_stable_within_process() {
		let files = [("a.go", "package main\n"), ("b.go", "func f() {}\n")];
		assert_eq!(oracle_hash(&files), oracle_hash(&files));
	}

	#[test]
	fn materialize_idempotent() {
		let files = [("marker.txt", "hello-oracle")];
		let a = materialize(&files, "test-oracle").unwrap();
		let b = materialize(&files, "test-oracle").unwrap();
		assert_eq!(a.dir, b.dir);
		assert_eq!(
			fs::read_to_string(a.dir.join("marker.txt")).unwrap(),
			"hello-oracle"
		);
	}
}
