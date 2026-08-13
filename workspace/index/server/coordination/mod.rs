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

/// The Linux compile strategy (ephemeral SmolvmCage → NdIrF1 IR). Gated to
/// `target_os = "linux"` because the microVM cage runtime needs KVM; the
/// `#[cfg(not(target_os = "linux"))]` compile path is the sibling in-process
/// strategy (`compile_inprocess`). See `indexing::Indexer::execute_compile_phase`.
#[cfg(target_os = "linux")]
pub(crate) mod compile_cage;
pub mod health;
pub mod indexing;
pub mod initialization;
pub mod packages;
pub mod search;
