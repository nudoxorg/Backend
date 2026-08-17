//! The connective mesh: coordinating an ingest across subsystems.
//!
//! The durability model is **catalog-as-WAL + derived stores as pollers**: a
//! publish is one catalog write (the local SQLite/DoltLite engine via
//! `CatalogWriter::apply_ops` — the old postgres connection typestate is gone),
//! recording the package row + parse-status + outbox entry; every derived store
//! (qdrant / package-index / graph) is a poller with its own durable cursor in
//! `sink_watermarks`, redelivered at-least-once. So the coordination layer here
//! only ever *enqueues* and *records intents* — it never fans out synchronously,
//! and a partial failure converges on retry rather than corrupting a subset of
//! stores.
//!
//! Flows:
//! - [`initialization`]: ensure a package is present + fresh (enqueue if not);
//! - [`indexing`]: drive one job through acquire → extract → compile → emit;
//! - [`search`]: dispatch a read to the right surface;
//! - [`health`]: parse-status + readiness.

// The in-process (non-Linux) compile strategy `indexing::execute_compile_phase`
// dispatches to on `#[cfg(not(target_os = "linux"))]` — runs the language
// producer directly on the host CPU (no microVM cage) and writes catalog symbols
// + qdrant vectors. The `#[cfg(target_os = "linux")]` branch keeps the real
// SmolvmCage path inline in `indexing.rs`. See that dispatch (`// reconcile:
// in-process (macOS) vs cage (linux)`).
pub(crate) mod compile_inprocess;
pub mod health;
pub mod indexing;
pub mod initialization;
pub mod packages;
pub mod search;
