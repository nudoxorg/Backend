//! Health/status handlers: liveness, readiness, and the prometheus exposition.

#[allow(unused_imports)]
use crate::server::registry;
use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use heart::surface::{Capabilities, PROTOCOL_VERSION, SurfaceId};

use crate::server::Server;
use crate::server::coordination::health::Health;
use crate::server::http::dto::HealthDto;
use registry::vector::EmbeddingModel;

/// `GET /healthz` — liveness: the process is up. Always `200` if reachable.
pub async fn livez() -> StatusCode {
    StatusCode::OK
}

/// `GET /readyz` — readiness: every backing store is reachable/migrated.
/// `200` while the federation serves (degraded overlays are reported, not
/// fatal); `503` once the definitive base is down.
#[tracing::instrument(skip_all)]
pub async fn readyz<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
) -> (StatusCode, Json<HealthDto>) {
    let (status, health) = match server.health().await {
        Health::Ready => (
            StatusCode::OK,
            HealthDto {
                ready: true,
                degraded: Vec::new(),
            },
        ),
        Health::Degraded(degraded) => {
            tracing::warn!(?degraded, "serving degraded");
            (
                StatusCode::OK,
                HealthDto {
                    ready: true,
                    degraded,
                },
            )
        }
        // `degraded` carries the impaired backends here too. A 503 whose body
        // said `"degraded":[]` read as "not ready, and nothing is wrong",
        // which is the one thing that cannot be true.
        Health::Down(down) => {
            tracing::error!(?down, "not serving: a required backend is down");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                HealthDto {
                    ready: false,
                    degraded: down,
                },
            )
        }
    };
    (status, Json(health))
}

/// `GET /capabilities` — the capability handshake (LOCAL-REMOTE-CONTRACT.md
/// §6.2): which surfaces this node authoritatively serves, at which protocol
/// version, so a client's router can decide what to delegate to the remote and
/// what to serve locally. Unauthenticated, like `/readyz` — a client must be
/// able to learn what a node can do *before* it holds any capability token, and
/// the answer reveals no data, only shape.
#[tracing::instrument(skip_all)]
pub async fn capabilities<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
) -> Json<Capabilities> {
    // `semantic` is honest about the vector plane: a node that is serving but
    // whose Qdrant backend is impaired still answers name/type search, yet must
    // not advertise a semantic capability it cannot currently honour — else a
    // router delegates a semantic query that silently returns nothing.
    let (serving, degraded) = match server.health().await {
        Health::Ready => (true, Vec::new()),
        Health::Degraded(degraded) => (true, degraded),
        Health::Down(down) => (false, down),
    };
    let semantic = serving && !degraded.contains(&heart::BackendKind::Qdrant);

    Json(Capabilities {
        protocol: PROTOCOL_VERSION,
        // `Usages` is typed and routed but still answers 501 until the reverse-
        // position projection lands end-to-end, so it is deliberately NOT
        // advertised: a router must not delegate a query the node will reject.
        surfaces: vec![SurfaceId::Symbols, SurfaceId::Packages],
        // No single global corpus generation is exposed yet — generation stamps
        // are per-package in the catalog, not one served-corpus number — so a
        // router treats delegated data as current-but-ungeneration-stamped
        // rather than assuming staleness. Wired when a corpus generation exists.
        generation: None,
        semantic,
    })
}

/// `GET /metrics` — the prometheus exposition text. The recorder (and its
/// render handle) is installed once by `heart::telemetry::init` in `main`, fanned out
/// to the OTLP pipeline; here we just render it. `503` until it is installed
/// or if some other recorder won the global-recorder race.
pub async fn metrics() -> Response {
    match heart::telemetry::render_prometheus() {
        Some(body) => body.into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
