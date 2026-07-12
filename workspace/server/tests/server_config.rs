//! Pipeline part: **server configuration** (`server::ServerConfiguration`).
//!
//! TDD specs for the static, private-facing default configuration.

use server::ServerConfiguration;

/// The default config binds the documented local address.
///
/// Assert: `ServerConfiguration::default().serving_address` is the documented
///   local development address, `127.0.0.1:8080`.
#[test]
fn default_binds_local_address() {
    let configuration = ServerConfiguration::default();
    assert_eq!(
        configuration.serving_address,
        std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
        "the default must bind loopback on the documented development port"
    );
}

/// The default config knows the terminus endpoint.
///
/// Assert: `terminus_endpoint()` returns the configured terminus URL.
#[test]
fn default_exposes_terminus_endpoint() {
    let configuration = ServerConfiguration::default();
    let endpoint = configuration.terminus_endpoint();
    assert_eq!(
        endpoint.as_str(),
        "http://127.0.0.1:6363/",
        "the definitive source's terminus endpoint must be the localhost default"
    );
    // The accessor is the definitive base's endpoint — the single-source path.
    assert_eq!(endpoint, &configuration.definitive.endpoints.terminus);
}

/// The upload timeout is the documented constant.
///
/// Assert: `limits.upload_timeout` is 2000ms.
#[test]
fn upload_timeout_is_two_seconds() {
    let configuration = ServerConfiguration::default();
    assert_eq!(
        configuration.limits.upload_timeout,
        std::time::Duration::from_millis(2000),
        "admin mutations must give up after the documented two seconds"
    );
}
