//! The connective mesh: coordinating an ingest across subsystems.
//!
//! The durability model is **postgres-as-WAL + derived stores as pollers**: a
//! publish is one postgres transaction (package row + parse-status + outbox
//! entry); every derived store (qdrant / package-index / graph) is a poller with its
//! own durable cursor. So the coordination layer here only ever *enqueues* and
//! *records intents* — it never fans out synchronously, and a partial failure
//! converges on retry rather than corrupting a subset of stores.
//!
//! Flows:
//! - [`initialization`]: ensure a package is present + fresh (enqueue if not);
//! - [`indexing`]: drive one job through acquire → extract → compile → emit;
//! - [`search`]: dispatch a read to the right surface;
//! - [`health`]: parse-status + readiness.

// The macOS/non-Linux compile strategy `indexing::execute_compile_phase`
// dispatches to (`// reconcile: in-process (macOS) vs cage (linux)`). Not
// declared under `#[cfg(not(target_os = "linux"))]`: it is a small,
// self-contained module and gating it would only relocate the same dead-code
// question onto its Linux-side sibling (`compile_cage.rs`) without avoiding
// it, since neither module is meaningfully useful cross-compiled anyway.
pub(crate) mod compile_inprocess;
pub mod health;
pub mod indexing;
pub mod initialization;
pub mod packages;
pub mod search;
