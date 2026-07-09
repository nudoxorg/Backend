//! Shared content-addressed materialization for vendored oracles (Go + Java).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{ProducerError, oracle_dir};

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
///
/// Writes are atomic (`*.part` → rename) so a killed mid-write cannot poison
/// the content-addressed cache. Existing files with the wrong length are rewritten.
pub fn materialize(files: &[(&str, &str)], label: &str) -> Result<OraclePath, ProducerError> {
	let hash = oracle_hash(files);
	let dir = oracle_dir(label, hash);
	fs::create_dir_all(&dir)?;

	for (name, contents) in files {
		let path = dir.join(name);
		if path.is_file() {
			let ok = fs::metadata(&path)
				.map(|m| m.len() == contents.len() as u64)
				.unwrap_or(false);
			if ok {
				continue;
			}
			// Corrupt / partial — rewrite atomically.
		}
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}
		write_atomic(&path, contents.as_bytes())?;
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

/// Write `bytes` to `path` via a sibling `*.part` file + rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ProducerError> {
	let parent = path.parent().unwrap_or_else(|| Path::new("."));
	let name = path
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_else(|| "oracle.bin".into());
	let tmp = parent.join(format!(".{name}.part"));
	{
		let mut f = fs::File::create(&tmp)?;
		f.write_all(bytes)?;
		f.sync_all()?;
	}
	fs::rename(&tmp, path)?;
	Ok(())
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

	#[test]
	fn materialize_rewrites_truncated_file() {
		let files = [("poison.txt", "full-contents-here")];
		let hash = oracle_hash(&files);
		let dir = oracle_dir("test-poison", hash);
		let _ = fs::remove_dir_all(&dir);
		fs::create_dir_all(&dir).unwrap();
		// Simulate a killed mid-write.
		fs::write(dir.join("poison.txt"), b"trunc").unwrap();

		let got = materialize(&files, "test-poison").unwrap();
		assert_eq!(
			fs::read_to_string(got.dir.join("poison.txt")).unwrap(),
			"full-contents-here"
		);
	}
}
