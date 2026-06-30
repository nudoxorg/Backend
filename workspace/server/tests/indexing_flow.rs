//! Diagram flow: **indexing** (`server::coordination::indexing`).
//!
//! "runs computer · update postgres (tantivy polls) · generate blob information".

/// Indexing runs the compiler/generation over the materialized package.
///
/// Assert: `compiler::generate` is driven to produce the CST/IR/tar resolutions.
#[tokio::test]
async fn indexing_runs_the_compiler() {
    todo!("assert indexing drives compiler::generate");
}

/// Indexing generates blob information.
///
/// Assert: per-symbol blob info (incl. the code/treesitter hash) is produced and
///   the blob is persisted.
#[tokio::test]
async fn indexing_generates_blob_information() {
    todo!("assert blob info is generated + stored");
}

/// Indexing updates postgres status; tantivy picks it up by polling.
///
/// Assert: indexing writes status to postgres (push), and the tantivy index
///   reflects it after a poll (pull) — not via a direct push to tantivy.
#[tokio::test]
async fn indexing_updates_postgres_and_tantivy_polls() {
    todo!("assert pg push + tantivy pull-by-poll");
}

/// One parse fans out to every store (text, vector, graph, blob).
///
/// Assert: a single projection updates the tantivy, qdrant, terminus, and blob
///   stores — parsed once, fanned out.
#[tokio::test]
async fn one_parse_fans_out_to_all_stores() {
    todo!("assert parse-once / fan-out across all stores");
}
