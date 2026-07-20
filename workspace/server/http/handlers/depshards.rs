//! Dep-shard distribution endpoints (09-vector §20.3).
//!
//! # `GET /v1/depshards/{package}/{version}/manifest`
//!
//! Returns the [`DepshardManifestDto`] for the requested package: the full
//! edgepack key (so the client can verify it derived the same identity), the
//! artifact CAS key + RAM estimate once baked, and the lifecycle status.
//!
//! # Semantics
//!
//! - `404` when the package uuid is malformed or the package is not known.
//! - `503` when `config.depshards.enabled` is false (the surface is gated).
//! - `200 { status: "pending" }` when no `edgepack_artifacts` row exists yet
//!   (or the row is still in `claimed` state) — the client should poll back.
//! - `200 { status: "ready", artifact_id: "…", ram_estimate: … }` once baked.
//! - `200 { status: "failed" }` when the bake failed terminally (the client
//!   should not install; a recipe rotation is required to retry).
//!
//! # Artifact retrieval
//!
//! Artifact bytes are served by the existing [`registry::Store::get_section`]
//! path — same CAS streaming as every other blob section. There is no presign
//! path (the object store build enables only `file://` and `memory://`; S3
//! presign is a documented future addition). Clients retrieve the artifact via
//! the same blob-streaming convention used for source sections today.

use std::sync::Arc;

use axum::{
	Json,
	extract::{Path, State},
	http::StatusCode,
	response::{IntoResponse, Response},
};
use registry::vector::EmbeddingModel;

use crate::Server;
use crate::authz::Principal;
use crate::bakery;
use crate::error::ServerResult;
use crate::http::dto::DepshardManifestDto;

/// `GET /v1/depshards/{package}/{version}/manifest`
///
/// Returns the edgepack manifest for `package@version` under the current
/// recipe/model fingerprint. Status `pending` when no bake has completed yet.
#[tracing::instrument(skip_all, fields(%package, %version))]
pub async fn get_manifest<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	_principal: Principal,
	Path((package, version)): Path<(uuid::Uuid, String)>,
) -> ServerResult<Response> {
	if !server.config().depshards.enabled {
		return Ok((
			StatusCode::SERVICE_UNAVAILABLE,
			axum::Json(serde_json::json!({ "error": "dep-shard serving is disabled on this node" })),
		)
			.into_response());
	}

	let package_id = heart::PackageId::from_uuid(package);

	// Derive the edgepack key for this package@version under the *deployed*
	// model brand — a fleet on E5/OpenAI must never advertise Jina artifacts.
	let key = bakery::edgepack_key::<M>(package_id, &version);

	// Look up the artifact row (if any) in the claim ledger.
	let row = if let Some(store) = server.edgepacks() {
		let fingerprint = bakery::recipe_fingerprint::<M>();
		match store.get(package_id, &version, &fingerprint).await {
			Ok(row) => row,
			Err(error) => {
				// Treat a ledger read failure as a transient "pending" rather
				// than a 5xx — the manifest surface must never block installs.
				tracing::warn!(%package, %version, error = %error, "edgepack ledger lookup failed; serving pending");
				None
			}
		}
	} else {
		// depshards.enabled but no edgepacks store means the bakery table was
		// not created (the node has depshards.enabled=true but bakery.enabled
		// and depshards.enabled were both false at boot — shouldn't happen).
		None
	};

	let dto = DepshardManifestDto::from_parts(&key, row.as_ref());

	// Telemetry: label the response with its status for visibility.
	metrics::counter!(
		"depshard_manifest_requests",
		"status" => dto.status.clone(),
	)
	.increment(1);

	tracing::debug!(
		%package,
		%version,
		status = %dto.status,
		artifact_id = dto.artifact_id.as_deref().unwrap_or("none"),
		"depshard manifest served"
	);

	Ok(Json(dto).into_response())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::bakery::{EdgepackRow, EdgepackStatus};

	/// Pending manifest: all key fields present, no artifact/ram, status pending.
	#[test]
	fn pending_manifest_shape() {
		let key = vector_core::shard::EdgepackKey {
			package: heart::PackageId::from_uuid(uuid::Uuid::from_u128(42)),
			version: "1.0.0".into(),
			model_id: vector_core::model::JinaCodeV2::id(),
			recipe_id: bakery::RECIPE_ID.into(),
			quant_profile: vector_core::quant::QP1,
			edge_format_version: vector_core::shard::EDGE_FORMAT_VERSION,
		};
		let dto = DepshardManifestDto::from_parts(&key, None);
		assert_eq!(dto.status, "pending");
		assert!(dto.artifact_id.is_none());
		assert!(dto.ram_estimate.is_none());
		assert!(!dto.edgepack_key_digest.is_empty());
		assert_eq!(dto.edge_format_version, vector_core::shard::EDGE_FORMAT_VERSION);
	}

	/// Ready manifest: artifact_id + ram_estimate present, status ready.
	#[test]
	fn ready_manifest_shape() {
		let key = vector_core::shard::EdgepackKey {
			package: heart::PackageId::from_uuid(uuid::Uuid::from_u128(42)),
			version: "1.0.0".into(),
			model_id: vector_core::model::JinaCodeV2::id(),
			recipe_id: bakery::RECIPE_ID.into(),
			quant_profile: vector_core::quant::QP1,
			edge_format_version: vector_core::shard::EDGE_FORMAT_VERSION,
		};
		let artifact_hash = heart::ContentHash::of_bytes(b"shard artifact");
		let row = EdgepackRow {
			digest: key.digest(),
			status: EdgepackStatus::Ready,
			artifact: Some(artifact_hash),
			ram_estimate: Some(4096),
		};
		let dto = DepshardManifestDto::from_parts(&key, Some(&row));
		assert_eq!(dto.status, "ready");
		assert!(dto.is_ready());
		let artifact_id = dto.artifact_id.expect("artifact_id present when ready");
		assert_eq!(artifact_id.len(), 64, "hex-encoded blake3 is 64 chars");
		assert!(
			artifact_id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
			"lowercase hex"
		);
		assert_eq!(dto.ram_estimate, Some(4096));
	}

	/// Failed manifest: status failed, no artifact/ram.
	#[test]
	fn failed_manifest_shape() {
		let key = vector_core::shard::EdgepackKey {
			package: heart::PackageId::from_uuid(uuid::Uuid::from_u128(42)),
			version: "1.0.0".into(),
			model_id: vector_core::model::JinaCodeV2::id(),
			recipe_id: bakery::RECIPE_ID.into(),
			quant_profile: vector_core::quant::QP1,
			edge_format_version: vector_core::shard::EDGE_FORMAT_VERSION,
		};
		let row = EdgepackRow {
			digest: key.digest(),
			status: EdgepackStatus::Failed,
			artifact: None,
			ram_estimate: None,
		};
		let dto = DepshardManifestDto::from_parts(&key, Some(&row));
		assert_eq!(dto.status, "failed");
		assert!(dto.artifact_id.is_none());
		assert!(dto.ram_estimate.is_none());
	}
}
