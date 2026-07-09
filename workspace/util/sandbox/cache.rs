//! Content-addressed parse-result cache (design §10).
//!
//! Key = H(producer_version ‖ toolchain_hash ‖ source_hash ‖ dep_lock_hash).
//! Value = opaque IR bytes (JSON Index, etc.).
//!
//! L1: in-process [`std::sync::Mutex`] map with capacity bound (no foyer dep).
//! L2: directory of blake3-named blobs (mirrors registry CAS layout).
//!
//! The big performance lever is skipping re-parse entirely — not the sandbox.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::SandboxError;
use crate::observer;

/// Opaque 32-byte content digest (blake3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey([u8; 32]);

impl CacheKey {
	/// Raw bytes.
	pub const fn as_bytes(&self) -> &[u8; 32] {
		&self.0
	}

	/// Lower-hex encoding for filesystem names.
	pub fn hex(&self) -> String {
		self.0.iter().map(|b| format!("{b:02x}")).collect()
	}

	/// Build from the four design components (length-prefixed streaming).
	pub fn derive(
		producer_version: &[u8],
		toolchain_hash: &[u8],
		source_hash: &[u8],
		dep_lock_hash: &[u8],
	) -> Self {
		let mut h = blake3::Hasher::new();
		h.update(&(producer_version.len() as u64).to_le_bytes());
		h.update(producer_version);
		h.update(&(toolchain_hash.len() as u64).to_le_bytes());
		h.update(toolchain_hash);
		h.update(&(source_hash.len() as u64).to_le_bytes());
		h.update(source_hash);
		h.update(&(dep_lock_hash.len() as u64).to_le_bytes());
		h.update(dep_lock_hash);
		Self(*h.finalize().as_bytes())
	}

	/// Convenience: hash arbitrary bytes as one component digest.
	pub fn of_bytes(bytes: &[u8]) -> Self {
		Self(*blake3::hash(bytes).as_bytes())
	}
}

/// Disk + memory content-addressed store for parse outputs.
pub struct ParseCache {
	dir: PathBuf,
	l1: Mutex<HashMap<CacheKey, Vec<u8>>>,
	l1_cap: usize,
}

impl ParseCache {
	/// Open (or create) a cache rooted at `dir`.
	pub fn open(dir: impl Into<PathBuf>, l1_cap: usize) -> Result<Self, SandboxError> {
		let dir = dir.into();
		fs::create_dir_all(dir.join("cas"))?;
		Ok(Self {
			dir,
			l1: Mutex::new(HashMap::new()),
			l1_cap: l1_cap.max(1),
		})
	}

	/// Process-default: `$NUDOX_PARSE_CACHE` or temp `nudox-parse-cache`.
	pub fn default_open() -> Result<Self, SandboxError> {
		let dir = std::env::var_os("NUDOX_PARSE_CACHE")
			.map(PathBuf::from)
			.unwrap_or_else(|| std::env::temp_dir().join("nudox-parse-cache"));
		Self::open(dir, 256)
	}

	fn cas_path(&self, key: &CacheKey) -> PathBuf {
		self.dir.join("cas").join(key.hex())
	}

	/// Lookup; records hit/miss on the global observer.
	pub fn get(&self, key: &CacheKey) -> Option<Vec<u8>> {
		{
			let guard = self.l1.lock().unwrap_or_else(|e| e.into_inner());
			if let Some(v) = guard.get(key) {
				observer::global().cache_hit();
				return Some(v.clone());
			}
		}
		let path = self.cas_path(key);
		match fs::read(&path) {
			Ok(bytes) => {
				// Envelope: blake3(value) ‖ value — integrity independent of key.
				let Some(value) = strip_envelope(&bytes) else {
					tracing::warn!(path = %path.display(), "parse cache envelope corrupt");
					let _ = fs::remove_file(&path);
					observer::global().cache_miss();
					return None;
				};
				self.l1_insert(*key, value.clone());
				observer::global().cache_hit();
				Some(value)
			}
			Err(_) => {
				observer::global().cache_miss();
				None
			}
		}
	}

	/// Store `value` under a composite input key (design §10).
	pub fn put_composite(&self, key: CacheKey, value: Vec<u8>) -> Result<(), SandboxError> {
		let path = self.cas_path(&key);
		if !path.exists() {
			let tmp = path.with_extension("tmp");
			fs::write(&tmp, envelope(&value))?;
			fs::rename(&tmp, &path)?;
		}
		self.l1_insert(key, value);
		Ok(())
	}

	fn l1_insert(&self, key: CacheKey, value: Vec<u8>) {
		let mut guard = self.l1.lock().unwrap_or_else(|e| e.into_inner());
		if guard.len() >= self.l1_cap {
			// Cheap eviction: clear half (not LRU; fine for L1 of last-resort).
			let n = guard.len() / 2;
			let drop_keys: Vec<CacheKey> = guard.keys().take(n).copied().collect();
			for k in drop_keys {
				guard.remove(&k);
			}
		}
		guard.insert(key, value);
	}

	/// Root directory.
	pub fn path(&self) -> &Path {
		&self.dir
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
mod tests {
	use super::*;

	#[test]
	fn derive_is_stable() {
		let a = CacheKey::derive(b"1.0.0", b"tc", b"src", b"lock");
		let b = CacheKey::derive(b"1.0.0", b"tc", b"src", b"lock");
		assert_eq!(a, b);
		let c = CacheKey::derive(b"1.0.1", b"tc", b"src", b"lock");
		assert_ne!(a, c);
	}

	#[test]
	fn roundtrip() {
		let dir = std::env::temp_dir().join(format!("nudox-pc-{}", std::process::id()));
		let _ = fs::remove_dir_all(&dir);
		let cache = ParseCache::open(&dir, 8).unwrap();
		let key = CacheKey::derive(b"p", b"t", b"s", b"d");
		cache.put_composite(key, b"ir-json".to_vec()).unwrap();
		assert_eq!(cache.get(&key).as_deref(), Some(b"ir-json".as_slice()));
		let _ = fs::remove_dir_all(&dir);
	}
}
