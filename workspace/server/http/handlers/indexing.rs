//! Write/admin handlers: add a package (idempotent), fetch its state, trigger a
//! re-sync. These enqueue work; they never run the pipeline inline. All three
//! answer with the domain [`Initialized`] (id + lifecycle state), serialized
//! directly — there is no parallel response DTO.

use std::sync::Arc;

use axum::{
	Json,
	extract::{Path, State},
};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::coordination::initialization::Initialized;
use crate::error::ServerResult;
use crate::http::dto::AddPackageDto;

/// `POST /packages` — resolve coordinates and ensure the package is indexed.
/// Idempotent: a duplicate returns the existing id + state (never re-enqueues).
/// (Also serves the former `/packages/ensure`; the two were the same operation.)
pub async fn add_package<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Json(req): Json<AddPackageDto>,
) -> ServerResult<Json<Initialized>> {
	// Parse + validate the wire request into typed coordinates (this is where the
	// version is consumed, so the derived id is correct).
	let coordinates = req.into_coordinates()?;
	let _ = (server, coordinates);
	todo!("ensure_initialized(coordinates, ctx), return the Initialized outcome")
}

/// `GET /packages/:id` — the package's current lifecycle state.
pub async fn get_package<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<Initialized>> {
	let _ = (server, id);
	todo!("look up parse status by PackageId")
}

/// `POST /packages/:id/sync` — force a freshness re-check and re-enqueue if stale.
pub async fn sync_package<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<Initialized>> {
	let _ = (server, id);
	todo!("recompute content hash, compare freshness, re-enqueue if stale")
}
