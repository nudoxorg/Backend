//! Admin handlers: typed-authz consumers.
//!
//! Every handler in this module takes an [`AdminPrincipal`] extractor, which
//! forces the extractor to run for every admin route. The extractor mints an
//! [`AdminCap`] via [`Server::authorize_admin`]; the cap is then threaded into
//! the save machinery. A handler that does not hold an `AdminCap` cannot call
//! the guarded methods — the type system enforces this at compile time.
//!
//! # Routes
//! - `POST /admin/packages/:id/verify`  — deep blob integrity audit
//! - `POST /admin/packages/:id/rebuild` — re-emit fan-out intent for all derived stores

use std::sync::Arc;

use axum::{
	Json,
	extract::{Path, State},
};
use heart::PackageId;
use serde::Serialize;

use registry::runtime::vector::EmbeddingModel;
use crate::Server;
use crate::authz::AdminPrincipal;
use crate::error::ServerResult;
use crate::save::blobs::BlobAudit;

/// Response for `POST /admin/packages/:id/verify`.
#[derive(Debug, Serialize)]
pub struct VerifyResponse {
	pub package: uuid::Uuid,
	pub sound: bool,
	pub files_verified: usize,
	pub bytes_verified: u64,
	pub missing: usize,
	pub corrupt: usize,
	pub size_mismatches: usize,
	pub matches_recorded: bool,
	pub references_decoded: bool,
}

impl From<BlobAudit> for VerifyResponse {
	fn from(audit: BlobAudit) -> Self {
		Self {
			package: *audit.package.as_uuid(),
			sound: audit.is_sound(),
			files_verified: audit.files_verified,
			bytes_verified: audit.bytes_verified,
			missing: audit.missing.len(),
			corrupt: audit.corrupt.len(),
			size_mismatches: audit.size_mismatches.len(),
			matches_recorded: audit.matches_recorded,
			references_decoded: audit.references_decoded,
		}
	}
}

/// Response for `POST /admin/packages/:id/rebuild`.
#[derive(Debug, Serialize)]
pub struct RebuildResponse {
	pub package: uuid::Uuid,
	/// The content hash the fan-out intents were re-emitted at.
	pub snapshot: String,
}

/// `POST /admin/packages/:id/verify`
///
/// Runs a deep blob integrity audit for the package, reading every CAS section
/// back through the hash-verifying store path. Returns a summary of what was
/// found — missing, corrupt, hash-mismatched, or decode-failed sections.
///
/// Requires an [`AdminPrincipal`]: the extractor must run before this handler
/// body executes. The resulting [`AdminCap`] is threaded into [`Server::verify_blobs`].
///
/// # Phase 4f TODO
/// A store-wide orphan sweep (CAS objects not referenced by any manifest) needs
/// `Store::list()` on [`registry::Store`]. Until that API exists, auditing is
/// per-package from the manifest down: it can prove presence and integrity, but
/// not the absence of garbage. See `save/blobs.rs` for the full gap description.
#[tracing::instrument(skip_all, fields(package = %id))]
pub async fn verify_package<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	admin: AdminPrincipal,
	Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<VerifyResponse>> {
	let cap = server.authorize_admin(&admin, "admin.verify")?;
	let package = PackageId::from_uuid(id);
	let audit = server.verify_blobs(&cap, package).await?;
	tracing::info!(
		%package,
		sound = audit.is_sound(),
		files = audit.files_verified,
		"admin verify completed"
	);
	Ok(Json(VerifyResponse::from(audit)))
}

/// `POST /admin/packages/:id/rebuild`
///
/// Re-emits the fan-out intent for every derived store at the package's
/// recorded generation, letting pollers re-materialize. The outbox dedupe on
/// `(package, generation, sink)` makes this idempotent.
///
/// Requires an [`AdminPrincipal`]: the extractor must run before this handler
/// body executes. The resulting [`AdminCap`] is threaded into [`Server::rebuild_from_blobs`].
#[tracing::instrument(skip_all, fields(package = %id))]
pub async fn rebuild_package<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	admin: AdminPrincipal,
	Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<RebuildResponse>> {
	let cap = server.authorize_admin(&admin, "admin.rebuild")?;
	let package = PackageId::from_uuid(id);
	let snapshot = server.rebuild_from_blobs(&cap, package).await?;
	tracing::info!(%package, "admin rebuild completed");
	Ok(Json(RebuildResponse {
		package: id,
		snapshot: snapshot.to_string(),
	}))
}
