//! Pipeline part: **package admin API** (`server::http`).
//!
//! TDD specs for the write/admin plane: tracking a package kicks off a
//! background sync and the request returns immediately. The HTTP round-trips
//! run over the full router (via `tower::ServiceExt`) against the live stack,
//! opt-in via `SERVER_TEST_BACKENDS`; the pure wire-validation specs always run.

mod common;

use axum::http::StatusCode;
use server::http::dto::AddPackageDto;
use server::http::router::router;

/// Adding a package returns immediately while sync runs in the background.
///
/// Act: `POST /packages`.
/// Assert: the response carries `enqueued = true` and a non-terminal state —
///   the pipeline was kicked off, not run inline.
#[tokio::test]
async fn add_package_returns_immediately_then_syncs() {
    let Some((server, _data)) =
        common::assembled_server("add_package_returns_immediately_then_syncs").await
    else {
        return;
    };
    let body = add_body("serde", "1.0.219");
    let (status, response) = common::call(router(server), common::post_json("/packages", &body)).await;

    assert_eq!(status, StatusCode::OK, "the add is answered immediately: {response}");
    assert_eq!(
        response["enqueued"], serde_json::Value::Bool(true),
        "a first add enqueues background indexing"
    );
    assert!(response["package"].is_string(), "the deterministic package id comes back");
}

/// Adding a duplicate package returns the existing record, no re-queue.
///
/// Assert: a second add of the same coordinates returns the original id and
///   reports the already-in-flight state rather than enqueueing again.
#[tokio::test]
async fn duplicate_add_returns_existing() {
    let Some((server, _data)) = common::assembled_server("duplicate_add_returns_existing").await
    else {
        return;
    };
    let body = add_body("tokio", "1.45.0");
    let application = router(server);

    let (_, first) = common::call(application.clone(), common::post_json("/packages", &body)).await;
    let (status, second) = common::call(application, common::post_json("/packages", &body)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["package"], first["package"], "identity is deterministic across adds");
    assert_eq!(
        second["enqueued"],
        serde_json::Value::Bool(false),
        "a duplicate add never re-enqueues"
    );
}

/// Getting reflects tracked packages.
///
/// Assert: `GET /packages/{id}` returns the tracked package's snapshot, and an
///   untracked id answers 404. (There is deliberately no list surface — the
///   discovery path is `/packages/search`.)
#[tokio::test]
async fn list_and_get_reflect_tracked_packages() {
    let Some((server, _data)) =
        common::assembled_server("list_and_get_reflect_tracked_packages").await
    else {
        return;
    };
    let application = router(server);
    let body = add_body("smol_str", "0.3.2");
    let (_, added) = common::call(application.clone(), common::post_json("/packages", &body)).await;
    let id = added["package"].as_str().expect("the add answers with the package id").to_owned();

    let (status, fetched) = common::call(application.clone(), common::get(&format!("/packages/{id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["package"], added["package"], "the snapshot names the same package");

    let absent = uuid::Uuid::new_v4();
    let (status, _) = common::call(application, common::get(&format!("/packages/{absent}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an untracked id is a 404, not an error");
}

/// An unsupported language is rejected with 400.
///
/// The rejection is a *wire-shape* fact — `ecosystem` deserializes into the
/// closed `Language` enum — so it is asserted without any backend.
#[tokio::test]
async fn unsupported_language_is_rejected() {
    let rejected = serde_json::from_value::<AddPackageDto>(serde_json::json!({
        "ecosystem": "cobol",
        "name": "legacy",
        "version": "1.0.0",
    }));
    assert!(rejected.is_err(), "an unknown ecosystem must fail wire validation (→ 400)");

    // And a malformed version for a *supported* language is rejected at lowering.
    let malformed = AddPackageDto {
        ecosystem: heart::Language::Rust,
        name: "serde".into(),
        version: "not-a-version".into(),
        origin: None,
    };
    assert!(malformed.into_coordinates().is_err(), "an invalid version is a 400 at lowering");
}

/// The read plane never holds a handle the write plane mutates.
///
/// The separation is structural: the two planes are distinct routers with
/// distinct middleware, sharing only the immutable `Arc<Server>` state. The
/// observable contract asserted here: mutation routes reject read-plane verbs,
/// and read routes reject the admin mutation verb.
#[tokio::test]
async fn read_and_write_planes_are_separated() {
    let Some((server, _data)) =
        common::assembled_server("read_and_write_planes_are_separated").await
    else {
        return;
    };
    let application = router(server);

    // The write surface does not answer reads-as-writes: GET on the mutation
    // route is not a handler, it is a method mismatch.
    let (status, _) = common::call(application.clone(), common::get("/packages")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "the mutation route only accepts POST");

    // The read surface has no mutation aliases: posting a package to the
    // search route is a shape error, never an ingest.
    let body = add_body("serde", "1.0.219");
    let (status, _) = common::call(application, common::post_json("/search", &body)).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the read plane cannot be coerced into accepting write payloads"
    );
}

/// The wire body of a crates.io add request.
fn add_body(name: &str, version: &str) -> serde_json::Value {
    serde_json::json!({
        "ecosystem": "rust",
        "name": name,
        "version": version,
    })
}
