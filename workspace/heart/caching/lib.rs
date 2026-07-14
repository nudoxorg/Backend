//! Stampede-resistant caching primitives shared across the read and write planes.
//!
//! The workspace already caches the one irreducibly expensive thing — embeddings
//! — with a bare [`moka`] get-then-insert (`runtime/vector/cache.rs`). That is the
//! right call *there*: embedding is deterministic, so a racing double-compute is
//! harmless. But other hot paths aren't so forgiving. A popular package's derived
//! metadata, a search-result page, a GitHub enrichment fetch: when one of those
//! expires (or a deploy cold-starts every replica at once), a naive cache lets
//! the whole herd recompute in lockstep and slam the backend. That is a *cache
//! stampede*, and preventing it is as much an uptime feature as a latency one.
//!
//! This crate packages the three standard defences so callers get them for free:
//!
//! - [`single_flight`] — coalesce concurrent misses to one compute (the herd
//!   waits and shares the leader's result), while keeping the codebase's
//!   deliberate choice to never share non-`Clone` errors across callers.
//! - [`stampede`] — [`StampedeCache`], a moka-backed cache that layers
//!   probabilistic early recomputation (XFetch) and stale-while-revalidate on top
//!   of coalescing, so a hot key is refreshed *before* it expires, in the
//!   background, spread across time instead of all at once.
//! - [`jitter`] — de-synchronise the expiries of entries written together.
//!
//! It is intentionally backend-agnostic (generic over `K`/`V`) and deps-light
//! (moka + tokio + dashmap + rand): a disk-backed L2 tier is a documented
//! follow-up, not a hidden dependency. See [`stampede`] for the design notes.

pub mod jitter;
pub mod single_flight;
pub mod stampede;

pub use jitter::jittered;
pub use single_flight::{SingleFlight, Ticket};
pub use stampede::StampedeCache;
