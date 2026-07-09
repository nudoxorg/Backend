//! Node-local plain-directory CAS.
//!
//! Layout: `{root}/cas/{blake3-hex}` — same shape as the historical parse-cache
//! L2 and the registry object-store prefix. Each blob is self-authenticating:
//! `blake3(value) ‖ value`.
//!
//! # Blocking I/O
//!
//! All methods hit the filesystem **synchronously**. Call them from a blocking
//! context (`spawn_blocking`, or after `block_in_place`) — never from a Tokio
//! worker task that needs to make progress while this runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use heart::ContentHash;

use crate::{Cas, CasError};

/// Directory of blake3-named files.
#[derive(Debug, Clone)]
pub struct DiskCas {
	root: PathBuf,
}

impl DiskCas {
	/// Open (or create) a CAS rooted at `root`. Blobs live in `root/cas/`.
	pub fn open(root: impl Into<PathBuf>) -> Result<Self, CasError> {
		let root = root.into();
		let cas_dir = root.join("cas");
		fs::create_dir_all(&cas_dir).map_err(|source| CasError::io(Some(cas_dir), source))?;
		Ok(Self { root })
	}

	/// Process default: `$NUDOX_PARSE_CACHE` or temp `nudox-parse-cache`
	/// (same env as the historical parse cache so existing ops configs work).
	pub fn default_open() -> Result<Self, CasError> {
		let root = std::env::var_os("NUDOX_PARSE_CACHE")
			.map(PathBuf::from)
			.unwrap_or_else(|| std::env::temp_dir().join("nudox-parse-cache"));
		Self::open(root)
	}

	/// Root directory (parent of `cas/`).
	pub fn path(&self) -> &Path { &self.root }

	fn blob_path(&self, key: ContentHash) -> PathBuf {
		self.root.join("cas").join(key.hex())
	}

	/// Unique sibling of `path` for a race-free publish step.
	fn unique_tmp(path: &Path) -> PathBuf {
		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let n = COUNTER.fetch_add(1, Ordering::Relaxed);
		let pid = std::process::id();
		path.with_extension(format!("{pid}.{n}.tmp"))
	}

	/// Synchronous get — for callers already on a blocking thread.
	pub fn get_sync(&self, key: ContentHash) -> Result<Option<Bytes>, CasError> {
		let path = self.blob_path(key);
		match fs::read(&path) {
			Ok(raw) => match strip_envelope(&raw) {
				Some(value) => Ok(Some(Bytes::from(value))),
				None => {
					tracing::warn!(path = %path.display(), "disk cas envelope corrupt");
					let _ = fs::remove_file(&path);
					Err(CasError::Integrity { path })
				},
			},
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
			Err(source) => Err(CasError::io(Some(path), source)),
		}
	}

	/// Synchronous put_keyed (first-write-wins, race-safe).
	///
	/// Writes a uniquely-named temp blob, then publishes with `hard_link` so a
	/// concurrent creator loses cleanly (`AlreadyExists` → `Ok(false)`). Falls
	/// back to rename when hard links are unavailable.
	pub fn put_keyed_sync(&self, key: ContentHash, bytes: Bytes) -> Result<bool, CasError> {
		let path = self.blob_path(key);
		if path.exists() {
			return Ok(false);
		}

		let tmp = Self::unique_tmp(&path);
		fs::write(&tmp, envelope(&bytes))
			.map_err(|source| CasError::io(Some(tmp.clone()), source))?;

		match fs::hard_link(&tmp, &path) {
			Ok(()) => {
				let _ = fs::remove_file(&tmp);
				Ok(true)
			},
			Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
				let _ = fs::remove_file(&tmp);
				Ok(false)
			},
			Err(_) => {
				// FS without hard links (or cross-device): exclusive-ish rename.
				if path.exists() {
					let _ = fs::remove_file(&tmp);
					return Ok(false);
				}
				match fs::rename(&tmp, &path) {
					Ok(()) => Ok(true),
					Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
						let _ = fs::remove_file(&tmp);
						Ok(false)
					},
					Err(source) => {
						let _ = fs::remove_file(&tmp);
						Err(CasError::io(Some(path), source))
					},
				}
			},
		}
	}

	/// Synchronous invalidate.
	pub fn invalidate_sync(&self, key: ContentHash) -> Result<(), CasError> {
		let path = self.blob_path(key);
		match fs::remove_file(&path) {
			Ok(()) => Ok(()),
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
			Err(source) => Err(CasError::io(Some(path), source)),
		}
	}
}

impl Cas for DiskCas {
	async fn get(&self, key: ContentHash) -> Result<Option<Bytes>, CasError> {
		self.get_sync(key)
	}

	async fn put(&self, bytes: Bytes) -> Result<ContentHash, CasError> {
		let key = ContentHash::of_bytes(&bytes);
		self.put_keyed(key, bytes).await?;
		Ok(key)
	}

	async fn put_keyed(&self, key: ContentHash, bytes: Bytes) -> Result<bool, CasError> {
		self.put_keyed_sync(key, bytes)
	}

	async fn invalidate(&self, key: ContentHash) -> Result<(), CasError> {
		self.invalidate_sync(key)
	}
}

/// `blake3(value) ‖ value` so disk bits are self-authenticating.
fn envelope(value: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(32 + value.len());
	out.extend_from_slice(blake3::hash(value).as_bytes());
	out.extend_from_slice(value);
	out
}

fn strip_envelope(bytes: &[u8]) -> Option<Vec<u8>> {
	if bytes.len() < 32 {
		return None;
	}
	let (digest, value) = bytes.split_at(32);
	if digest != blake3::hash(value).as_bytes() {
		return None;
	}
	Some(value.to_vec())
}

#[cfg(test)]
pub(crate) fn envelope_for_test(value: &[u8]) -> Vec<u8> { envelope(value) }
