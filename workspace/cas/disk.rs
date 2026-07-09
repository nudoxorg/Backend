//! Node-local plain-directory CAS.
//!
//! Layout: `{root}/cas/{blake3-hex}` — same shape as the historical parse-cache
//! L2 and the registry object-store prefix. Each blob is self-authenticating:
//! `blake3(value) ‖ value`.

use std::fs;
use std::path::{Path, PathBuf};

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

	/// Synchronous put_keyed.
	pub fn put_keyed_sync(&self, key: ContentHash, bytes: Bytes) -> Result<bool, CasError> {
		let path = self.blob_path(key);
		if path.exists() {
			return Ok(false);
		}
		let tmp = path.with_extension("tmp");
		fs::write(&tmp, envelope(&bytes)).map_err(|source| CasError::io(Some(tmp.clone()), source))?;
		fs::rename(&tmp, &path).map_err(|source| CasError::io(Some(path), source))?;
		Ok(true)
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
