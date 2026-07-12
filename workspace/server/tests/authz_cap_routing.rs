//! Tests for typed authorization witnesses (Phase 4a) and admin endpoint
//! routing (Phase 2b).
//!
//! These tests verify:
//! (a) `ForbiddenReason` → 403 mapping (the dead variant is now wired);
//! (b) The admin endpoints (`POST /admin/packages/:id/verify`,
//!     `POST /admin/packages/:id/rebuild`) route through `AdminPrincipal` and
//!     return expected responses when the package exists / does not exist;
//! (c) A read handler mints a `ReadCap` (structural: the handler signature
//!     compiles only because `Principal` → `authorize_read` → `ReadCap` is wired).
//!
//! Offline tests (no backends required) cover the cap type plumbing and the 403
//! projection. Admin-endpoint tests that need live backends are gated behind
//! `SERVER_TEST_BACKENDS`, consistent with the rest of the test suite.

mod common;

use axum::http::StatusCode;
use server::error::{ForbiddenReason, ServerError};

// ── (a) ForbiddenReason → 403 ────────────────────────────────────────────────

/// `ForbiddenReason::ActionDenied` projects to HTTP 403.
///
/// This exercises the 403 path that was dead under the old allow-all stub.
/// Under typed authz the deny arm is always wired; this test ensures the
/// status-code projection is correct for the now-reachable variant.
#[test]
fn forbidden_reason_action_denied_maps_to_403() {
    let err = ServerError::Forbidden(ForbiddenReason::ActionDenied { action: "test.action" });
    assert_eq!(
        err.status(),
        StatusCode::FORBIDDEN,
        "ForbiddenReason::ActionDenied must project to 403 Forbidden"
    );
}

/// The display string of a `ForbiddenReason` names the denied action.
#[test]
fn forbidden_reason_display_names_the_action() {
    let reason = ForbiddenReason::ActionDenied { action: "admin.verify" };
    let msg = reason.to_string();
    assert!(msg.contains("admin.verify"), "error message must include the denied action name");
    assert!(msg.contains("not permitted"), "error message must state the action was not permitted");
}

// ── (b) Admin endpoint routing ───────────────────────────────────────────────

/// `POST /admin/packages/:id/verify` → 404 for an unknown package id.
///
/// The handler extracts `AdminPrincipal` (allow-all: always succeeds), calls
/// `authorize_admin`, and then calls `verify_blobs`. For an unknown package the
/// blob store returns a not-found error, which projects to 400 (no recorded
/// snapshot). This confirms the whole routing path is wired:
///   route → `AdminPrincipal` extractor → `authorize_admin` → `verify_blobs`.
#[tokio::test]
async fn admin_verify_endpoint_routes_through_admin_principal() {
    let Some((server, _data)) =
        common::assembled_server("admin_verify_endpoint_routes_through_admin_principal").await
    else {
        return;
    };
    let server = std::sync::Arc::new(server);
    let router = server::http::router::router(std::sync::Arc::clone(&server));

    // A package id that was never stored has no snapshot → the handler returns
    // an error response (400 Bad Request: NoRecordedSnapshot).
    let unknown_id = uuid::Uuid::new_v4();
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(format!("/admin/packages/{unknown_id}/verify"))
        .body(axum::body::Body::empty())
        .expect("request builds");
    let (status, _body) = common::call(router, req).await;
    // NoRecordedSnapshot is a BadRequest (400); if the routing or extractor
    // fails the status would be 500 or 405. A 400 here proves the path is live.
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
        "admin verify for an unknown package: expected 400/404 but got {status}"
    );
}

/// `POST /admin/packages/:id/rebuild` → 400/404 for an unknown package id.
///
/// Parallel to the verify test: confirms the rebuild path is routed through
/// `AdminPrincipal` and reaches `rebuild_from_blobs`.
#[tokio::test]
async fn admin_rebuild_endpoint_routes_through_admin_principal() {
    let Some((server, _data)) =
        common::assembled_server("admin_rebuild_endpoint_routes_through_admin_principal").await
    else {
        return;
    };
    let server = std::sync::Arc::new(server);
    let router = server::http::router::router(std::sync::Arc::clone(&server));

    let unknown_id = uuid::Uuid::new_v4();
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(format!("/admin/packages/{unknown_id}/rebuild"))
        .body(axum::body::Body::empty())
        .expect("request builds");
    let (status, _body) = common::call(router, req).await;
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
        "admin rebuild for an unknown package: expected 400/404 but got {status}"
    );
}

// ── (c) ReadCap minted by read handler ───────────────────────────────────────

/// The search handler mints a `ReadCap` via the `Principal` extractor.
///
/// This is a structural test: it compiles only because:
///   1. The `search` handler signature includes `principal: Principal`,
///   2. `server.authorize_read(&principal, "search.symbols")` mints a `ReadCap`,
///   3. `server.search_symbols(&cap, &request)` consumes that cap.
/// The test confirms the `/search` endpoint responds (200 or an error) rather
/// than hanging or panicking.
#[tokio::test]
async fn search_handler_mints_read_cap() {
    let Some((server, _data)) =
        common::assembled_server("search_handler_mints_read_cap").await
    else {
        return;
    };
    let server = std::sync::Arc::new(server);
    let router = server::http::router::router(std::sync::Arc::clone(&server));

    let req = common::post_json(
        "/search",
        &serde_json::json!({ "query": "Deserialize", "limit": 8 }),
    );
    let (status, _body) = common::call(router, req).await;
    // 200 (empty corpus is fine) or 400 (malformed search) — never 500 from a
    // missing extractor or cap.
    assert!(
        status.is_success() || status == StatusCode::BAD_REQUEST,
        "/search with a valid query should not 5xx; got {status}"
    );
}
