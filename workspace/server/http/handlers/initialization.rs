//! Initialization handler: "ensure the library is ready; if not, request it".
//! A thin wrapper over the indexing add-package handler that blocks-or-returns
//! based on readiness rather than always returning immediately.

use std::sync::Arc;

use axum::{Json, extract::State};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;
use crate::http::dto::{AddPackageDto, AddPackageResponseDto};

/// `POST /packages/ensure` — ensure a package is initialized and return its
/// current state (enqueuing indexing work if absent/stale).
pub async fn ensure<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Json(req): Json<AddPackageDto>,
) -> ServerResult<Json<AddPackageResponseDto>> {
	let _ = (server, req);
	todo!("Server::ensure_initialized, project to AddPackageResponseDto")
}
