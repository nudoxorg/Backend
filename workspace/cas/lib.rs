//! Unified content-addressed storage.
//!
//! One [`Cas`] trait, three tiers:
//! - **L1** — in-process [`StampedeCache`] (coalesced, stampede-resistant)
//! - **L2** — node-local plain-directory [`DiskCas`] (blake3-named files)
//! - **L3** — shared object store via [`RegistryCas`] (stub this phase)
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
mod registry;
mod tiered;

pub use caching::StampedeCache;
pub use disk::DiskCas;
pub use error::CasError;
pub use heart::{ContentHash, JobKey};
pub use memory::MemoryCas;
pub use registry::RegistryCas;
pub use tiered::Tiered;

use bytes::Bytes;

/// Content-addressed byte store.
///
/// - [`Cas::put`] hashes `bytes` and stores under that digest.
/// - [`Cas::put_keyed`] stores under a caller-supplied key (job keys, content
///   keys). Returns `true` when the key was newly written, `false` if it
///   already existed (idempotent).
pub trait Cas: Send + Sync {
	/// Lookup `key`. `Ok(None)` is a clean miss.
	async fn get(&self, key: ContentHash) -> Result<Option<Bytes>, CasError>;

	/// Store `bytes` under `ContentHash::of_bytes(bytes)`.
	async fn put(&self, bytes: Bytes) -> Result<ContentHash, CasError>;

	/// Store `bytes` under a pre-computed `key`. Idempotent.
	async fn put_keyed(&self, key: ContentHash, bytes: Bytes) -> Result<bool, CasError>;
}
