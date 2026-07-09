//! Content-addressed producer cache at the generate boundary (design §10 / P4).
//!
//! `JobKey = H(producer_version ‖ toolchain ‖ source_tree ‖ dep_lock)`.
//! Same key ⇒ serve cached IR (postcard), never re-run producers.
//!
//! Backed by the unified [`cas::Tiered`] store (L1 StampedeCache + L2 DiskCas).

use std::path::Path;
use std::sync::OnceLock;

use bytes::Bytes;
use cas::{Cas, Tiered};
use heart::{ContentHash, JobKey, Toolchain};

/// Bump when IR shape / lowering semantics change (automatic invalidation).
/// `/2` marks the postcard value encoding (was JSON under `/1`).
pub const PRODUCER_VERSION: &str = "nudox-producer/2";

/// Process-wide CAS used by generate.
///
/// Open failures fall back to an L1-only store so a one-shot disk/permission
/// problem does not permanently disable caching for the process lifetime.
fn cas() -> Option<&'static Tiered> {
	static CAS: OnceLock<Option<Tiered>> = OnceLock::new();
	CAS.get_or_init(|| {
		if matches!(
			std::env::var("NUDOX_PARSE_CACHE_DISABLE").as_deref(),
			Ok("1") | Ok("true")
		) {
			return None;
		}
		match Tiered::default_open() {
			Ok(t) => Some(t),
			Err(e) => {
				tracing::error!(
					error = %e,
					"cas default_open failed; falling back to memory-only L1"
				);
				Some(Tiered::memory_only(256))
			},
		}
	})
	.as_ref()
}

/// Drive a CAS future from the sync generate path.
///
/// Generate runs on `spawn_blocking` (or bare unit tests). When a multi-thread
/// runtime handle is present we use `block_in_place` so an accidental call from
/// an async task parks the worker instead of panicking. Without a runtime we
/// spin a tiny current-thread one for tests.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
	match tokio::runtime::Handle::try_current() {
		Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
		Err(_) => {
			static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
			let rt = RT.get_or_init(|| {
				tokio::runtime::Builder::new_current_thread()
					.enable_all()
					.build()
					.expect("cas fallback runtime")
			});
			rt.block_on(fut)
		},
	}
}

/// Build the design §10 job key from known parts.
pub fn key(
	toolchain: &Toolchain,
	source_hash: ContentHash,
	dep_lock_hash: ContentHash,
) -> JobKey {
	// Stable, order-preserving encoding already used elsewhere for toolchain.
	let tc = serde_json::to_vec(toolchain).unwrap_or_default();
	JobKey::derive(
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

/// Lookup opaque cached bytes under `key`.
pub fn get(key: ContentHash) -> Option<Bytes> {
	let c = cas()?;
	match block_on(c.get(key)) {
		Ok(Some(bytes)) => {
			sandbox::observer::global().cache_hit();
			Some(bytes)
		},
		Ok(None) => {
			sandbox::observer::global().cache_miss();
			None
		},
		Err(e) => {
			tracing::warn!(error = %e, "cas get failed");
			sandbox::observer::global().cache_miss();
			None
		},
	}
}

/// Drop a poison / wrong-version entry so a subsequent put can land.
pub fn invalidate(key: ContentHash) {
	let Some(c) = cas() else { return };
	if let Err(e) = block_on(c.invalidate(key)) {
		tracing::warn!(error = %e, "cas invalidate failed");
	}
}

/// Store opaque bytes under `key` (best-effort, first-write-wins).
pub fn put(key: ContentHash, bytes: Bytes) {
	let Some(c) = cas() else { return };
	if let Err(e) = block_on(c.put_keyed(key, bytes)) {
		tracing::warn!(error = %e, "cas put failed");
	}
}
