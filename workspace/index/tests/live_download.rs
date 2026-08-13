#![cfg(feature = "server")]
//! Required live-stack source-file download contract.
//!
//! These tests assert the `GET /packages/:id/files/*path` surface against a real
//! assembled server with reachable backends. Opt-in is mandatory: missing
//! `SERVER_TEST_BACKENDS=1` or a failed backend connection is a hard failure,
//! not a silent skip.

mod server_common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use heart::telemetry::{TelemetryConfig, init};
use serde_json::Value;

/// The download surface: a tracked + stored package serves a known source file
/// with the correct content-type, a valid quoted ETag, and non-empty bytes.
/// Traversal paths (`../`) are rejected.
#[tokio::test]
#[ignore = "requires SERVER_TEST_BACKENDS=1 and live catalog/qdrant/terminus/object-store backends"]
async fn source_file_download_and_traversal_rejection() {
    let _telemetry = init(&TelemetryConfig::resolve(
        env!("CARGO_PKG_VERSION"),
        "cargo",
    ))
    .expect("telemetry initialization must be fail-open");
    let (server, _data) = server_common::required_assembled_server("live_download_contract").await;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("test listener binds");
    let address = listener.local_addr().expect("listener has an address");
    let serving = tokio::spawn(Arc::clone(&server).serve_on(listener));
    let client = reqwest::Client::new();
    let base = format!("http://{address}");

    // Wait for the server to become reachable.
    let alive = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match client.get(format!("{base}/healthz")).send().await {
                Ok(response) if response.status().is_success() => break,
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await;
    assert!(
        alive.is_ok(),
        "server must become reachable within 10 seconds"
    );

    // Add a well-known crate (small, stable, compiles everywhere).
    let add = client
        .post(format!("{base}/packages"))
        .json(&serde_json::json!({
            "ecosystem": "rust",
            "name": "serde",
            "version": "1.0.219"
        }))
        .send()
        .await
        .expect("add package request succeeds at the HTTP layer");
    assert!(
        add.status().is_success(),
        "add package must return success, got {}",
        add.status()
    );

    let payload: Value = add
        .json()
        .await
        .expect("add package response body is valid JSON");
    let package_id = payload["package"]
        .as_str()
        .expect("add response carries a string package id")
        .to_owned();
    assert!(
        payload["enqueued"] == Value::Bool(true),
        "a first add enqueues background indexing"
    );

    // Poll until the package reaches Stored, bounded at 5 minutes.
    let stored = poll_stored(&client, &base, &package_id, Duration::from_secs(300)).await;

    assert!(!stored.is_null(), "Stored state must carry a content hash");

    // Fetch a known file from the package snapshot.
    let file = client
        .get(format!("{base}/packages/{package_id}/files/src/lib.rs"))
        .send()
        .await
        .expect("source file request succeeds at the HTTP layer");
    assert_eq!(
        file.status(),
        reqwest::StatusCode::OK,
        "src/lib.rs must return 200"
    );

    let content_type = file
        .headers()
        .get("content-type")
        .expect("source file response carries a content-type header")
        .to_str()
        .expect("content-type header is valid ASCII");
    assert_eq!(
        content_type, "application/octet-stream",
        "source file content-type must be application/octet-stream"
    );

    let etag = file
        .headers()
        .get("etag")
        .expect("source file response carries an ETag header")
        .to_str()
        .expect("ETag header is valid ASCII");
    assert!(
        etag.starts_with('"') && etag.ends_with('"'),
        "ETag must be a valid quoted string, got: {etag}"
    );
    assert!(etag.len() > 2, "ETag must not be empty quotes, got: {etag}");

    let bytes = file.bytes().await.expect("source file body is readable");
    assert!(!bytes.is_empty(), "src/lib.rs must return non-empty bytes");

    // Traversal is rejected at the handler level — the `..` segment is caught
    // before any filesystem or CAS read.
    let traversal = client
        .get(format!("{base}/packages/{package_id}/files/../secret"))
        .send()
        .await
        .expect("traversal request succeeds at the HTTP layer");
    assert!(
        !traversal.status().is_success(),
        "traversal path ../secret must not return 2xx, got {}",
        traversal.status()
    );
    let traversal_body = traversal
        .text()
        .await
        .expect("traversal response body is readable");
    assert!(
        !traversal_body.contains("secret"),
        "traversal response must not serve secret content, got: {traversal_body}"
    );

    serving.abort();
    let _ = serving.await;
}

/// Poll `GET /packages/:id` until the state variant is `Stored` or the deadline
/// expires. Returns the `Stored` payload (the inner JSON object).
async fn poll_stored(
    client: &reqwest::Client,
    base: &str,
    package_id: &str,
    deadline: Duration,
) -> Value {
    let started = Instant::now();
    loop {
        assert!(
            started.elapsed() < deadline,
            "package {package_id} did not reach Stored within {deadline:?}"
        );

        let response = client
            .get(format!("{base}/packages/{package_id}"))
            .send()
            .await
            .expect("poll package request succeeds at the HTTP layer");
        assert!(
            response.status().is_success(),
            "poll package must not fail, got {}",
            response.status()
        );

        let body: Value = response
            .json()
            .await
            .expect("poll package response body is valid JSON");

        let state_obj = &body["state"];

        if state_obj.get("Stored").is_some() {
            return state_obj["Stored"].clone();
        }

        if state_obj.get("DeadLettered").is_some() {
            panic!(
                "package {} reached DeadLettered instead of Stored: {:?}",
                package_id, state_obj
            );
        }

        if let Some(variant) = state_obj.as_object().and_then(|o| o.keys().next()) {
            eprintln!("package {} state: {variant}, waiting...", package_id);
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
