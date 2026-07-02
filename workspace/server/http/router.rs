//! The route table.
//!
//! Splits a **read plane** (search, expand, sessions, health) from a
//! **write/admin plane** (add/get/sync packages) so the two can carry different
//! middleware (body limits, auth, rate limits) and a read never shares a code
//! path with ingest. Both planes share the `Arc<Server<M>>` application state.

use std::sync::Arc;

use axum::Router;

use runtime::vector::EmbeddingModel;
use crate::Server;

/// Build the full application router over a shared server handle.
///
/// Layers cross-cutting middleware (tracing, request-id, timeout, body limit,
/// compression, panic capture) via `tower-http`.
pub fn router<M: EmbeddingModel>(server: Arc<Server<M>>) -> Router {
	let _ = server;
	todo!("merge read_plane() + write_plane(), attach tower-http layers, set state")
}

/// The read plane: `/search`, `/search/semantic`, `/packages/search`,
/// `/symbols/:id`, `/expand`, `/sessions/:id`.
fn read_plane<M: EmbeddingModel>() -> Router<Arc<Server<M>>> {
	todo!("route the read handlers; responses stream as NDJSON where they page")
}

/// The write/admin plane: `POST /packages` (add/index), `GET /packages/:id`,
/// `POST /packages/:id/sync`, `/healthz`, `/readyz`, `/metrics`.
fn write_plane<M: EmbeddingModel>() -> Router<Arc<Server<M>>> {
	todo!("route the indexing/admin/health handlers with stricter body limits")
}
