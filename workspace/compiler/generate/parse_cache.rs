//! Content-addressed IR parse cache at the generate boundary (design §10 / P4).
//!
//! `key = H(producer_version ‖ toolchain ‖ source_tree ‖ dep_lock)`.
//! Same key ⇒ serve cached IR JSON, never re-run producers.
//!
//! Backed by [`sandbox::ParseCache`] (L1 map + L2 disk CAS).

use std::path::Path;
use std::sync::OnceLock;

use heart::{ContentHash, Toolchain};
use sandbox::{CacheKey, ParseCache};

/// Bump when IR shape / lowering semantics change (automatic invalidation).
pub const PRODUCER_VERSION: &str = "nudox-producer/1";

/// Process-wide parse cache.
fn cache() -> Option<&'static ParseCache> {
	static CACHE: OnceLock<Option<ParseCache>> = OnceLock::new();
	CACHE
		.get_or_init(|| {
			if matches!(
				std::env::var("NUDOX_PARSE_CACHE_DISABLE").as_deref(),
				Ok("1") | Ok("true")
			) {
				return None;
			}
			ParseCache::default_open().ok()
		})
		.as_ref()
}

/// Build the design §10 key from known parts.
pub fn key(
	toolchain: &Toolchain,
	source_hash: ContentHash,
	dep_lock_hash: ContentHash,
) -> CacheKey {
	// Stable, order-preserving encoding already used elsewhere for toolchain.
	let tc = serde_json::to_vec(toolchain).unwrap_or_default();
	CacheKey::derive(
		PRODUCER_VERSION.as_bytes(),
		&tc,
		source_hash.as_bytes(),
		dep_lock_hash.as_bytes(),
	)
}

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

/// Lookup cached IR JSON.
pub fn get(k: &CacheKey) -> Option<Vec<u8>> {
	cache()?.get(k)
}

/// Store IR JSON under `k`.
pub fn put(k: CacheKey, ir_json: Vec<u8>) {
	if let Some(c) = cache() {
		let _ = c.put_composite(k, ir_json);
	}
}
