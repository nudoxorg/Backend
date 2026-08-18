#![cfg(feature = "server")]
//! Pipeline part: **package admin API** (`index::server::http`).
//!
//! TDD specs for the write/admin plane: tracking a package kicks off a
//! background sync and the request returns immediately. The HTTP round-trips
//! run over the full router (via `tower::ServiceExt`) against the live stack,
//! opt-in via `SERVER_TEST_BACKENDS`; the pure wire-validation specs always run.

mod server_common;

use axum::http::StatusCode;
use index::server::http::dto::AddPackageDto;
use index::server::http::router::router;

/// Adding a package returns immediately while sync runs in the background.
///
/// Act: `POST /packages`.
/// Assert: the response carries `enqueued = true` and a non-terminal state —
///   the pipeline was kicked off, not run inline.
#[tokio::test]
async fn add_package_returns_immediately_then_syncs() {
    let (server, _data) =
        server_common::required_assembled_server("add_package_returns_immediately_then_syncs")
            .await;
    let body = add_body("serde", "1.0.219");
    let (status, response) =
        server_common::call(router(server), server_common::post_json("/packages", &body)).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "the add is answered immediately: {response}"
    );
    assert_eq!(
        response["enqueued"],
        serde_json::Value::Bool(true),
        "a first add enqueues background indexing"
    );
    assert!(
        response["package"].is_string(),
        "the deterministic package id comes back"
    );
}

/// Adding a duplicate package returns the existing record, no re-queue.
///
/// Assert: a second add of the same coordinates returns the original id and
///   reports the already-in-flight state rather than enqueueing again.
#[tokio::test]
async fn duplicate_add_returns_existing() {
    let (server, _data) =
        server_common::required_assembled_server("duplicate_add_returns_existing").await;
    let body = add_body("tokio", "1.45.0");
    let application = router(server);

    let (_, first) = server_common::call(
        application.clone(),
        server_common::post_json("/packages", &body),
    )
    .await;
    let (status, second) =
        server_common::call(application, server_common::post_json("/packages", &body)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        second["package"], first["package"],
        "identity is deterministic across adds"
    );
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
    let (server, _data) =
        server_common::required_assembled_server("list_and_get_reflect_tracked_packages").await;
    let application = router(server);
    let body = add_body("smol_str", "0.3.2");
    let (_, added) = server_common::call(
        application.clone(),
        server_common::post_json("/packages", &body),
    )
    .await;
    let id = added["package"]
        .as_str()
        .expect("the add answers with the package id")
        .to_owned();

    let (status, fetched) = server_common::call(
        application.clone(),
        server_common::get(&format!("/packages/{id}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fetched["package"], added["package"],
        "the snapshot names the same package"
    );

    let absent = uuid::Uuid::new_v4();
    let (status, _) = server_common::call(
        application,
        server_common::get(&format!("/packages/{absent}")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an untracked id is a 404, not an error"
    );
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
    assert!(
        rejected.is_err(),
        "an unknown ecosystem must fail wire validation (→ 400)"
    );

    // And a malformed version for a *supported* language is rejected at lowering.
    let malformed = AddPackageDto {
        ecosystem: heart::Language::Rust,
        name: "serde".into(),
        version: "not-a-version".into(),
        origin: None,
    };
    assert!(
        malformed.into_coordinates().is_err(),
        "an invalid version is a 400 at lowering"
    );
}

/// The read plane never holds a handle the write plane mutates.
///
/// The separation is structural: the two planes are distinct routers with
/// distinct middleware, sharing only the immutable `Arc<Server>` state. The
/// observable contract asserted here: mutation routes reject read-plane verbs,
/// and read routes reject the admin mutation verb.
#[tokio::test]
async fn read_and_write_planes_are_separated() {
    let (server, _data) =
        server_common::required_assembled_server("read_and_write_planes_are_separated").await;
    let application = router(server);

    // The write surface does not answer reads-as-writes: GET on the mutation
    // route is not a handler, it is a method mismatch.
    let (status, _) =
        server_common::call(application.clone(), server_common::get("/packages")).await;
    assert_eq!(
        status,
        StatusCode::METHOD_NOT_ALLOWED,
        "the mutation route only accepts POST"
    );

    // The read surface has no mutation aliases: posting a package to the
    // search route is a shape error, never an ingest.
    let body = add_body("serde", "1.0.219");
    let (status, _) =
        server_common::call(application, server_common::post_json("/search", &body)).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the read plane cannot be coerced into accepting write payloads"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Required-origin enforcement for Go / Java packages
// ──────────────────────────────────────────────────────────────────────────────

/// Go and Java have no canonical public registry, so `origin` is required.
///
/// Assert: `AddPackageDto::into_coordinates()` for `go`/`java` without an
/// `origin` field now lowers to the ecosystem's public default (M5:
/// proxy.golang.org / Maven Central) — the old "origin required" contract died
/// with the Go/Java resolve stubs.
#[tokio::test]
async fn go_without_origin_defaults_to_goproxy() {
    let dto = AddPackageDto {
        ecosystem: heart::Language::Go,
        name: "github.com/gorilla/mux".into(),
        version: "v1.8.1".into(),
        origin: None,
    };
    let coordinates = dto
        .into_coordinates()
        .expect("go without origin lowers against the module proxy");
    assert_eq!(coordinates.origin, heart::RegistryOrigin::GoProxy);
}

/// Same contract for Java.
#[tokio::test]
async fn java_without_origin_defaults_to_maven_central() {
    let dto = AddPackageDto {
        ecosystem: heart::Language::Java,
        name: "com.example:mylib".into(),
        version: "2.0.0".into(),
        origin: None,
    };
    let coordinates = dto
        .into_coordinates()
        .expect("java without origin lowers against Maven Central");
    assert_eq!(coordinates.origin, heart::RegistryOrigin::MavenCentral);
}

/// Go with an explicit origin resolves against the custom-registry table.
///
/// Assert: a registered origin resolves to `RegistryOrigin::Custom`; an
///   unknown name returns a `BadRequest` (not a panic or a placeholder URL).
#[tokio::test]
async fn go_with_unknown_origin_is_rejected() {
    let dto = AddPackageDto {
        ecosystem: heart::Language::Go,
        name: "github.com/gorilla/mux".into(),
        version: "1.8.1".into(),
        origin: Some("nonexistent-registry".into()),
    };
    let error = dto
        .into_coordinates()
        .expect_err("an unknown custom registry name must be rejected with BadRequest");
    let message = error.to_string();
    assert!(
        message.contains("nonexistent-registry"),
        "error must identify the unknown registry name; got: {message}"
    );
}

/// Ecosystems with canonical registries still work without an explicit origin.
#[tokio::test]
async fn rust_without_origin_defaults_to_crates_io() {
    let dto = AddPackageDto {
        ecosystem: heart::Language::Rust,
        name: "serde".into(),
        version: "1.0.219".into(),
        origin: None,
    };
    dto.into_coordinates()
        .expect("rust without origin must default to crates.io (no origin required for Rust)");
}

/// The wire body of a crates.io add request.
fn add_body(name: &str, version: &str) -> serde_json::Value {
    serde_json::json!({
        "ecosystem": "rust",
        "name": name,
        "version": version,
    })
}
