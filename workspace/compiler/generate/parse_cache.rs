//! Content-addressed source / lock hashing for the producer job key.
//!
//! The CAS itself is owned by the injected [`ForgeContext`](crate::compile::producer::ForgeContext);
//! this module only computes the hashes that feed `JobKey` derivation (in
//! [`seal_package`](crate::compile::producer::seal_package)). No process-global
//! cache lives here anymore.

use std::path::Path;

use heart::ContentHash;

/// Hash a source tree (sorted path ‖ file blake3).
pub fn hash_source_tree(root: &Path) -> std::io::Result<ContentHash> {
	let mut files: Vec<(String, ContentHash)> = Vec::new();
	walk(root, root, &mut files)?;
	files.sort_by(|a, b| a.0.cmp(&b.0));
	let mut h = ContentHash::builder();
	for (path, digest) in files {
		h.update(path.as_bytes());
		h.update(&[0]);
		h.update(digest.as_bytes());
	}
	Ok(h.finalize())
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, ContentHash)>) -> std::io::Result<()> {
	for entry in std::fs::read_dir(dir)? {
		let entry = entry?;
		let ft = entry.file_type()?;
		let path = entry.path();
		if ft.is_dir() {
			if entry.file_name() == ".git" || entry.file_name() == "target" {
				continue;
			}
			walk(root, &path, out)?;
		} else if ft.is_file() {
			let rel = path
				.strip_prefix(root)
				.unwrap_or(&path)
				.to_string_lossy()
				.into_owned();
			let bytes = std::fs::read(&path)?;
			out.push((rel, ContentHash::of_bytes(&bytes)));
		}
	}
	Ok(())
}

/// Hash common lockfiles if present; empty digest when none.
pub fn hash_dep_lock(root: &Path) -> ContentHash {
	const LOCKS: &[&str] = &[
		"Cargo.lock",
		"package-lock.json",
		"yarn.lock",
		"pnpm-lock.yaml",
		"go.sum",
		"poetry.lock",
		"Pipfile.lock",
		"flake.lock",
	];
	let mut h = ContentHash::builder();
	let mut any = false;
	for name in LOCKS {
		let p = root.join(name);
		if let Ok(bytes) = std::fs::read(&p) {
			any = true;
			h.update(name.as_bytes());
			h.update(&[0]);
			h.update(ContentHash::of_bytes(&bytes).as_bytes());
		}
	}
	if any {
		h.finalize()
	} else {
		ContentHash::of_bytes(&[])
	}
}
