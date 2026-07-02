//! Write/admin handlers: add a package (idempotent), fetch its state, trigger a
//! re-sync. These enqueue work; they never run the pipeline inline.

use std::sync::Arc;

use axum::{
	Json,
	extract::{Path, State},
};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;
use crate::http::dto::{AddPackageDto, AddPackageResponseDto};

/// `POST /packages` — resolve coordinates and ensure the package is indexed.
/// Idempotent: a duplicate returns the existing id + state (never re-enqueues).
pub async fn add_package<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Json(req): Json<AddPackageDto>,
) -> ServerResult<Json<AddPackageResponseDto>> {
	let _ = (server, req);
	todo!("parse coordinates, ensure_initialized, project to AddPackageResponseDto")
}

/// `GET /packages/:id` — the package's current lifecycle state.
pub async fn get_package<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<AddPackageResponseDto>> {
	let _ = (server, id);
	todo!("look up parse status by PackageId")
}

/// `POST /packages/:id/sync` — force a freshness re-check and re-enqueue if stale.
pub async fn sync_package<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<AddPackageResponseDto>> {
	let _ = (server, id);
	todo!("recompute generation, compare freshness, re-enqueue if stale")
}
