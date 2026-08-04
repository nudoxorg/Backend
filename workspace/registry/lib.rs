//! Registry — the **graph + vector** serving layer (§8 re-layering).
//!
//! Post-relayering, `registry` is deliberately *only* two things:
//!
//! - [`graph`] — a Trustfall adapter over in-memory IR ([`ir::IrView`])
//!   plus a disposable reverse-position index. Queries are *exactly* what
//!   Trustfall can express — no further wrapping.
//! - [`vector`] — the vector-search plane (folded in from the former standalone
//!   `vector` crate): the pure `core` vocabulary plus the feature-gated
//!   `local` / `remote` / `embed` impl planes and the serving-side gate + cache.
//!
//! The registry does **not** own storage/coordination. The catalog, doltlite,
//! outbox, job queue, blob/CAS, object-pack, and upstream pollers all live in
//! the `index` crate; the client/GUI composes `index` and `registry`. registry
//! therefore has NO dependency on `index` (or doltlite).
#![feature(return_type_notation)]

pub mod graph;
/// The vector-search plane (`core` + feature-gated `local`/`remote`/`embed`,
/// plus the serving-side `gate` + `cache`).
pub mod vector;
