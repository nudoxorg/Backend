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
	async fn get(&self, key: ContentHash) -> Result<Option<Bytes>, CasError>;

	/// Store `bytes` under `ContentHash::of_bytes(bytes)`.
	async fn put(&self, bytes: Bytes) -> Result<ContentHash, CasError>;

	/// Store `bytes` under a pre-computed `key`.
	///
	/// Returns `true` when the key was newly written, `false` when it already
	/// existed (first-write-wins; existing payload is left alone).
	async fn put_keyed(&self, key: ContentHash, bytes: Bytes) -> Result<bool, CasError>;

	/// Drop `key` from every tier that holds it so a subsequent put can land.
	async fn invalidate(&self, key: ContentHash) -> Result<(), CasError>;
}
