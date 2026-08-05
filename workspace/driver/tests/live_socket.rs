//! Required live-stack socket checks.
//!
//! These tests are intentionally ignored in the default Cargo run because they
//! need the configured catalog, object store, and vector backends. Running them
//! explicitly is a hard gate: missing opt-in or a failed backend connection is
//! a test failure, never a green skip.

mod common;

use std::sync::Arc;
use std::time::Duration;

use heart::telemetry::{TelemetryConfig, init};

#[tokio::test]
#[ignore = "requires SERVER_TEST_BACKENDS=1 and live backend services"]
async fn live_server_exposes_health_readiness_and_metrics_over_tcp() {
    let _telemetry = init(&TelemetryConfig::resolve(
        env!("CARGO_PKG_VERSION"),
        "cargo",
    ))
    .expect("telemetry initialization must be fail-open");
    let (server, _data) = common::required_assembled_server("live_socket_contract").await;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("test listener binds");
    let address = listener.local_addr().expect("listener has an address");
    let serving = tokio::spawn(Arc::clone(&server).serve_on(listener));
    let client = reqwest::Client::new();
    let base = format!("http://{address}");

    let response = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match client.get(format!("{base}/healthz")).send().await {
                Ok(response) if response.status().is_success() => break response,
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("server must become reachable within 10 seconds");
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let readiness = client
        .get(format!("{base}/readyz"))
        .send()
        .await
        .expect("readiness request succeeds at the HTTP layer");
    assert!(
        readiness.status().is_success() || readiness.status().is_server_error(),
        "readiness must be an explicit health result, got {}",
        readiness.status()
    );

    let metrics = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .expect("metrics request succeeds at the HTTP layer");
    assert_eq!(metrics.status(), reqwest::StatusCode::OK);
    let body = metrics.text().await.expect("metrics body is readable");
    assert!(
        body.contains("http_requests"),
        "RED metrics are missing: {body}"
    );

    serving.abort();
    let _ = serving.await;
}
