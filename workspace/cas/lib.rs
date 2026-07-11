//! Unified content-addressed storage.
//!
//! One [`Cas`] trait, three tiers:
//! - **L1** — in-process [`StampedeCache`] (coalesced, stampede-resistant)
//! - **L2** — node-local plain-directory [`DiskCas`] (blake3-named files)
//! - **L3** — any [`Cas`] impl; typically `registry::StoreCas` over the live
//!   object store. When absent, the zero-sized [`NoL3`] sentinel fills the slot.
//!
//! Keys are [`heart::ContentHash`]. Job-scoped composite keys are
//! [`heart::JobKey`] (a newtype over the same digest).
//!
//! ## Object-safe erasure
//!
//! [`Cas`] uses `async fn` in trait (RPITIT) so it is not object-safe. Use the
//! companion [`DynCas`] trait when a `&dyn` pointer is needed: every `T: Cas`
//! gets a blanket [`DynCas`] impl that boxes the futures.

#![allow(
	async_fn_in_trait,
	reason = "native RPITIT is the crate-wide convention; not object-safe by design"
)]

mod disk;
mod error;
mod memory;
mod tiered;

pub use caching::StampedeCache;
pub use disk::DiskCas;
pub use error::CasError;
pub use heart::{ContentHash, JobKey};
pub use memory::MemoryCas;
pub use tiered::{NoL3, Tiered};

use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;

/// Content-addressed byte store.
///
/// - [`Cas::put`] hashes `bytes` and stores under that digest.
/// - [`Cas::put_keyed`] stores under a caller-supplied key (job keys or content
///   keys). **First-write-wins**: if the key already exists the call is a no-op
///   (`Ok(false)`) and the stored value is **not** compared or replaced.
///   Callers that must repair a poison entry should [`Cas::invalidate`] first.
/// - For job keys the envelope binds value→self-hash only; the key is not
///   required to equal `ContentHash::of_bytes(value)`.
pub trait Cas: Send + Sync {
	/// Lookup `key`. `Ok(None)` is a clean miss.
	fn get(&self, key: ContentHash) -> impl Future<Output = Result<Option<Bytes>, CasError>> + Send;

	/// Store `bytes` under `ContentHash::of_bytes(bytes)`.
	fn put(&self, bytes: Bytes) -> impl Future<Output = Result<ContentHash, CasError>> + Send;

	/// Store `bytes` under a pre-computed `key`.
	///
	/// Returns `true` when the key was newly written, `false` when it already
	/// existed (first-write-wins; existing payload is left alone).
	fn put_keyed(
		&self,
		key: ContentHash,
		bytes: Bytes,
	) -> impl Future<Output = Result<bool, CasError>> + Send;

	/// Drop `key` from every tier that holds it so a subsequent put can land.
	fn invalidate(
		&self,
		key: ContentHash,
	) -> impl Future<Output = Result<(), CasError>> + Send;
}

// ─── Object-safe erasure ──────────────────────────────────────────────────────

/// A `dyn`-safe mirror of [`Cas`] whose methods return boxed futures.
///
/// `Cas` uses `async fn` in trait (RPITIT) and is therefore not object-safe.
/// `DynCas` provides an identical API that can be used behind `&dyn DynCas`.
/// Every `T: Cas` receives a blanket implementation, so concrete types (e.g.
/// [`Tiered`], [`MemoryCas`]) can be erased without any additional boilerplate.
///
/// Only the methods that the compile path actually calls (`get`, `put_keyed`,
/// `invalidate`) are included here to keep the surface minimal.
pub trait DynCas: Send + Sync {
	/// Lookup `key`. `Ok(None)` is a clean miss.
	fn get<'a>(
		&'a self,
		key: ContentHash,
	) -> Pin<Box<dyn Future<Output = Result<Option<Bytes>, CasError>> + Send + 'a>>;

	/// Store `bytes` under a pre-computed `key` (first-write-wins).
	fn put_keyed<'a>(
		&'a self,
		key: ContentHash,
		bytes: Bytes,
	) -> Pin<Box<dyn Future<Output = Result<bool, CasError>> + Send + 'a>>;

	/// Drop `key` from every tier that holds it so a subsequent put can land.
	fn invalidate<'a>(
		&'a self,
		key: ContentHash,
	) -> Pin<Box<dyn Future<Output = Result<(), CasError>> + Send + 'a>>;
}

impl<T: Cas> DynCas for T {
	fn get<'a>(
		&'a self,
		key: ContentHash,
	) -> Pin<Box<dyn Future<Output = Result<Option<Bytes>, CasError>> + Send + 'a>> {
		Box::pin(Cas::get(self, key))
	}

	fn put_keyed<'a>(
		&'a self,
		key: ContentHash,
		bytes: Bytes,
	) -> Pin<Box<dyn Future<Output = Result<bool, CasError>> + Send + 'a>> {
		Box::pin(Cas::put_keyed(self, key, bytes))
	}

	fn invalidate<'a>(
		&'a self,
		key: ContentHash,
	) -> Pin<Box<dyn Future<Output = Result<(), CasError>> + Send + 'a>> {
		Box::pin(Cas::invalidate(self, key))
	}
}
