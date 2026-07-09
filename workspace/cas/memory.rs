//! In-process CAS for tests and pure-L1 setups.

use std::collections::HashMap;
use std::sync::Mutex;

use bytes::Bytes;
use heart::ContentHash;

use crate::{Cas, CasError};

/// Mutex-backed map. No durability — unit tests and ephemeral L1 fill.
#[derive(Default)]
pub struct MemoryCas {
	inner: Mutex<HashMap<ContentHash, Bytes>>,
}

impl MemoryCas {
	/// Empty store.
	pub fn new() -> Self { Self::default() }
}

impl Cas for MemoryCas {
	async fn get(&self, key: ContentHash) -> Result<Option<Bytes>, CasError> {
		let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
		Ok(guard.get(&key).cloned())
	}

	async fn put(&self, bytes: Bytes) -> Result<ContentHash, CasError> {
		let key = ContentHash::of_bytes(&bytes);
		self.put_keyed(key, bytes).await?;
		Ok(key)
	}

	async fn put_keyed(&self, key: ContentHash, bytes: Bytes) -> Result<bool, CasError> {
		let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
		use std::collections::hash_map::Entry;
		match guard.entry(key) {
			Entry::Occupied(_) => Ok(false),
			Entry::Vacant(slot) => {
				slot.insert(bytes);
				Ok(true)
			},
		}
	}
}
