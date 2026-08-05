//! Write/admin handlers: add a package (idempotent), fetch its state, trigger a
//! re-sync. These enqueue work; they never run the pipeline inline. All three
//! answer with the domain [`Initialized`] (id + lifecycle state), serialized
//! directly — there is no parallel response DTO.

#[allow(unused_imports)]
use crate::registry;
use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use heart::{Freshness, PackageId, ResolutionState};

use crate::Server;
use crate::authz::Principal;
use crate::coordination::indexing::Indexer;
use crate::coordination::initialization::{
    InitializationDecision, Initialized, initialization_decision,
};
use crate::error::{ServerError, ServerResult};
use crate::http::dto::AddPackageDto;
use registry::vector::EmbeddingModel;

/// `POST /packages` — resolve coordinates and ensure the package is indexed.
/// Idempotent: a duplicate returns the existing id + state (never re-enqueues).
/// (Also serves the former `/packages/ensure`; the two were the same operation.)
#[tracing::instrument(skip_all, fields(ecosystem = %req.ecosystem, name = %req.name, version = %req.version))]
pub async fn add_package<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    principal: Principal,
    Json(req): Json<AddPackageDto>,
) -> ServerResult<Json<Initialized>> {
    let cap = server.authorize_write(&principal, "packages.ensure_initialized")?;
    // Parse + validate the wire request into typed coordinates (this is where the
    // version is consumed, so the derived id is correct).
    let coordinates = req.into_coordinates()?;
    let initialized = server.ensure_initialized(&cap, &coordinates).await?;
    tracing::info!(
        package = %initialized.package,
        enqueued = initialized.enqueued,
        "package ensured"
    );
    Ok(Json(initialized))
}

/// `GET /packages/:id` — the package's current lifecycle state.
#[tracing::instrument(skip_all, fields(package = %id))]
pub async fn get_package<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<Initialized>> {
    let package = PackageId::from_uuid(id);
    let state = server
        .parse_status(package)
        .await?
        .ok_or(ServerError::NotFound)?;
    Ok(Json(Initialized {
        package,
        state,
        enqueued: false,
    }))
}

/// `GET /packages/:id/files/*path` — stream one source file from the stored
/// package snapshot. The manifest is the path allow-list and the CAS read
/// verifies content integrity before returning bytes.
#[tracing::instrument(skip_all, fields(package = %id, path = %path))]
pub async fn get_source_file<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    Path((id, path)): Path<(uuid::Uuid, String)>,
) -> ServerResult<Response> {
    let path = path.trim_start_matches('/');
    if path.is_empty()
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "..")
    {
        return Err(crate::error::BadRequestReason::InvalidSourcePath {
            path: path.to_owned(),
        }
        .into());
    }

    let package = PackageId::from_uuid(id);
    let (hash, bytes) = server.source_file(package, path).await?;
    let mut response = Body::from(bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(value) = HeaderValue::try_from(format!("\"{hash}\"")) {
        response.headers_mut().insert(header::ETAG, value);
    }
    Ok(response)
}

/// `POST /packages/:id/sync` — force a freshness re-check and re-enqueue if stale.
#[tracing::instrument(skip_all, fields(package = %id))]
pub async fn sync_package<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    principal: Principal,
    Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<Initialized>> {
    let _cap = server.authorize_write(&principal, "packages.sync")?;
    let package = PackageId::from_uuid(id);
    let state = server
        .parse_status(package)
        .await?
        .ok_or(ServerError::NotFound)?;

    // Freshness is only decidable for a stored package: recompute the canonical
    // content hash from the current blob manifest and compare it to the snapshot
    // recorded at the `Stored` transition.
    let freshness = match &state {
        ResolutionState::Stored { hash } => {
            let recomputed = Indexer::new(Arc::clone(&server))
                .content_hash(package)
                .await?;
            Some(Freshness::compare(*hash, recomputed))
        }
        _ => None,
    };

    let decision = initialization_decision(Some(&state), freshness);
    let enqueued = matches!(decision, InitializationDecision::Enqueue);
    if enqueued {
        // Idempotent: the queue enforces one live job per package.
        server
            .queue()
            .enqueue(package)
            .await
            .map_err(crate::registry::RegistryError::from)?;
    }
    tracing::info!(?freshness, ?decision, "sync decision");
    Ok(Json(Initialized {
        package,
        state,
        enqueued,
    }))
}
