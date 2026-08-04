//! Caching + content-addressed storage — the workspace's shared cache tier.
//!
//! This module unifies two things that used to be separate crates (`caching` and
//! `cas`): the stampede-resistant in-process cache primitives, and the tiered
//! content-addressed store built on top of them. They lived apart only for
//! historical reasons — `cas` depended on `caching` for its L1 tier, and both
//! depended on `heart` for [`ContentHash`](crate::ContentHash). Folding them in
//! here collapses that layering into one place.
//!
//! ## Stampede-resistant caching
//!
//! When a hot key expires (or a deploy cold-starts every replica at once), a
//! naive cache lets the whole herd recompute in lockstep and slam the backend.
//! That is a *cache stampede*. The primitives here package the three standard
//! defences:
//!
//! - [`single_flight`] — coalesce concurrent misses to one compute (the herd
//!   waits and shares the leader's result).
//! - [`stampede`] — [`StampedeCache`], a moka-backed cache that layers
//!   probabilistic early recomputation (XFetch) and stale-while-revalidate on
//!   top of coalescing, so a hot key is refreshed *before* it expires.
//! - [`jitter`] — de-synchronise the expiries of entries written together.
//!
//! ## Content-addressed storage
//!
//! One [`Cas`] trait, three tiers:
//! - **L1** — in-process [`StampedeCache`] (coalesced, stampede-resistant)
//! - **L2** — node-local plain-directory [`DiskCas`] (blake3-named files)
//! - **L3** — any [`Cas`] impl; typically a store over the live object store.
//!   When absent, the zero-sized [`NoL3`] sentinel fills the slot.
//!
//! Keys are [`ContentHash`](crate::ContentHash). Job-scoped composite keys are
//! [`JobKey`](crate::JobKey) (a newtype over the same digest).
//!
//! ### Erasure
//!
//! [`Cas`] uses `async fn` in trait (RPITIT) so it is not object-safe. The
//! design deliberately keeps a **single** erasure strategy — static generics.
//! Callers that need to abstract over the backend do so with a type parameter
//! (`Tiered<L3>`), never a boxed trait object. Absence of an L3 tier is
//! expressed *exactly one way*: the [`NoL3`] type.
//!
//! ### Eviction
//!
//! Dropping a key is a distinct capability from storing one: content-addressed
//! object stores have no per-key delete, so [`Cas`] does not promise it. The
//! [`EvictableCas`] sub-trait carries `invalidate` and is implemented only by
//! the tiers that can honestly evict ([`DiskCas`], [`MemoryCas`], [`Tiered`]).

#![allow(
	async_fn_in_trait,
	reason = "native RPITIT is the CAS convention; the trait is not object-safe by design"
)]

// ── Stampede-resistant caching primitives (formerly the `caching` crate) ──────
mod jitter;
mod single_flight;
mod stampede;

pub use jitter::jittered;
pub use single_flight::{SingleFlight, Ticket};
pub use stampede::StampedeCache;

// ── Content-addressed storage (formerly the `cas` crate) ──────────────────────
mod disk;
mod error;
mod memory;
mod tiered;

pub use disk::DiskCas;
pub use error::CasError;
pub use memory::MemoryCas;
pub use tiered::{NoL3, Tiered};

// Keys live in `heart::content`; re-export here so CAS callers get them from the
// same path as the store types (as the former `cas` crate did).
pub use crate::{ContentHash, JobKey};

use std::future::Future;

use bytes::Bytes;

/// Content-addressed byte store.
///
/// - [`Cas::put`] hashes `bytes` and stores under that digest.
/// - [`Cas::put_keyed`] stores under a caller-supplied key (job keys or content
///   keys). **First-write-wins**: if the key already exists the call is a no-op
///   (`Ok(false)`) and the stored value is **not** compared or replaced.
///   Callers that must repair a poison entry need an [`EvictableCas`] tier and
///   should [`EvictableCas::invalidate`] first.
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
}

/// A [`Cas`] tier that can drop a key so a subsequent put can land.
///
/// Eviction is *not* part of the base [`Cas`] contract: content-addressed object
/// stores have no per-key delete and hold immutable data, so they are honestly
/// incapable of it at the type level. Only the tiers that own mutable local
/// state ([`DiskCas`], [`MemoryCas`]) — and the composite [`Tiered`] over them —
/// implement this.
pub trait EvictableCas: Cas {
	/// Drop `key` from this tier so a subsequent put can land.
	fn invalidate(&self, key: ContentHash) -> impl Future<Output = Result<(), CasError>> + Send;
}
