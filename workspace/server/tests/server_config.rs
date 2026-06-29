//! Pipeline part: **server configuration** (`server::ServerConfiguration`).
//!
//! TDD specs for the static, private-facing default configuration.

/// The default config binds the documented local address.
///
/// Assert: `ServerConfiguration::default().serving_address` is `127.0.0.1:1000`.
#[test]
fn default_binds_local_address() {
    todo!("assert the default serving address");
}

/// The default config knows the terminus endpoint.
///
/// Assert: `terminus_endpoint()` returns the configured terminus URL.
#[test]
fn default_exposes_terminus_endpoint() {
    todo!("assert the terminus endpoint is present");
}

/// The upload timeout is the documented constant.
///
/// Assert: `UPLOAD_TIMEOUT` is 2000ms.
#[test]
fn upload_timeout_is_two_seconds() {
    todo!("assert the upload timeout constant");
}
