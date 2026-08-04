//! The connective mesh: the transactional outbox plus the server-folded
//! coordination flows (health / indexing / initialization / packages / search),
//! salvaged from the deleted `registry::coordination` and the dissolved
//! `server::coordination` (§8/§9a).
//!
//! The durability model is **catalog-as-WAL + derived stores as pollers**: a
//! publish is one catalog transaction (business row + parse-status + outbox
//! entry); every derived store (qdrant/tantivy/usage-index) is a poller with its
//! own durable cursor. So the coordination layer only ever *enqueues* and
//! *records intents* — it never fans out synchronously, and a partial failure
//! converges on retry rather than corrupting a subset of stores.

pub mod outbox;

pub use outbox::{
    catalog_sink, Outbox, OutboxEntry, OutboxOp, OutboxSeq, SinkKind, SinkLockGuard,
};
// NOTE: `outbox` needs `crate::catalog::GlobalStore`; catalog lands in Tier 3
// just above this in build order, so this compiles once catalog is present.

// The server-folded coordination flows (health / indexing / initialization /
// packages / search) are wired in once their downstream dependencies land; see
// the tiered salvage below.
// pub mod health;
// pub mod indexing;
// pub mod initialization;
// pub mod packages;
// pub mod search;
