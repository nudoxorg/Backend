//! [`StoreCas`] — a [`cas::Cas`] adapter over a live [`Store`].
//!
//! This is the L3 implementation that wires the registry's content-addressed
//! object store into the `cas::Tiered` stack, giving the compile plane durable,
//! distributed persistence without any object-store types leaking into `cas`.
//!
//! ## Error mapping
//!
//! | `StoreError` variant          | `CasError` mapping                           |
//! |-------------------------------|----------------------------------------------|
//! | `NotFound`                    | `Ok(None)` on get (clean miss)               |
//! | `Integrity { expected, found }`| `CasError::Integrity { path, expected, found }`|
//! | everything else               | `CasError::Io` (path from store error where available) |
//!
//! `put_keyed` and `put` call `Store::put_section` which is already idempotent
//! (first-write-wins).
//!
//! `invalidate` is a no-op: the object store has no per-key delete in the
//! current interface, and CAS keys are content-addressed so a "wrong" entry
//! is structurally impossible (any bytes round-trip to the same hash). If a
//! hard delete is ever needed, call `Store::delete` directly.

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use cas::{Cas, CasError, ContentHash};
use heart::Live;

use crate::{blob::creation::PendingSection, error::StoreError, store::Store};

/// A [`cas::Cas`] implementation backed by a live registry [`Store`].
///
/// Wrap a `Store<Live>` in an `Arc` and pass it to [`Tiered::with_l3`] to
/// enable durable, object-store-backed L3 persistence.
///
/// ```text
/// let l3 = StoreCas::new(Arc::clone(&store));
/// let cas  = Tiered::with_l3(256, Some(DiskCas::open(&root)?), l3);
/// ```
#[derive(Clone)]
pub struct StoreCas {
	store: Arc<Store<Live>>,
}

impl StoreCas {
	/// Wrap a live store handle.
	pub fn new(store: Arc<Store<Live>>) -> Self {
		Self { store }
	}
}

/// Map a [`StoreError`] to a [`CasError`].
///
/// - `NotFound` → clean miss (`Ok(None)` is returned by the caller, not here;
///   this function is only called for actual errors).
/// - `Integrity` → `CasError::Integrity` (same semantic; path carried as string
///   because `CasError::Integrity` takes a `PathBuf`).
/// - Anything else → `CasError::Io` with `None` path and the error stringified
///   into an ad-hoc `io::Error`.
fn store_err_to_cas(err: StoreError) -> CasError {
	match err {
		StoreError::Integrity { path, .. } => {
			CasError::Integrity { path: PathBuf::from(path.to_string()) }
		},
		other => {
			let io_err = std::io::Error::other(other.to_string());
			CasError::Io { path: None, source: io_err }
		},
	}
}

impl Cas for StoreCas {
	async fn get(&self, key: ContentHash) -> Result<Option<Bytes>, CasError> {
		match self.store.get_section(key).await {
			Ok(bytes) => Ok(Some(bytes)),
			Err(StoreError::NotFound { .. }) => Ok(None),
			Err(e) => Err(store_err_to_cas(e)),
		}
	}

	async fn put(&self, bytes: Bytes) -> Result<ContentHash, CasError> {
		let key = ContentHash::of_bytes(&bytes);
		self.put_keyed(key, bytes).await?;
		Ok(key)
	}

	async fn put_keyed(&self, key: ContentHash, bytes: Bytes) -> Result<bool, CasError> {
		let section = PendingSection { hash: key, bytes };
		self.store.put_section(&section).await.map_err(store_err_to_cas)
	}

	async fn invalidate(&self, _key: ContentHash) -> Result<(), CasError> {
		// The object store has no per-key delete; content-addressed keys are
		// self-validating so a stale entry is not possible. No-op is correct.
		Ok(())
	}
}
