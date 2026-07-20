//! Pipeline part: **server configuration** (`server::ServerConfiguration`).
//!
//! TDD specs for the static, private-facing default configuration.

use server::{Deployment, Role, ServerConfiguration};

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

/// The default config exposes the instance token.
///
/// Assert: `instance_token()` returns the `"{org}/{db}"` salt used for
///   deterministic symbol ids — the stable identity of the definitive source.
#[test]
fn default_exposes_instance_token() {
    let configuration = ServerConfiguration::default();
    let token = configuration.definitive.endpoints.instance_token();
    assert_eq!(
        token, "nudox/registry",
        "the default instance token is the documented development value"
    );
}

/// The default role is `All` (single-node runs both compute and fan-out).
#[test]
fn default_role_is_all() {
    let configuration = ServerConfiguration::default();
    assert_eq!(configuration.role, Role::All);
    assert!(configuration.role.runs_forge(), "All must run the compile worker");
    assert!(configuration.role.runs_gateway(), "All must run the fan-out loops");
}

/// Each role gates exactly the loops it owns.
#[test]
fn role_gating_partitions_the_loops() {
    // Forge runs the compile worker but not the fan-out/index loops.
    assert!(Role::Forge.runs_forge());
    assert!(!Role::Forge.runs_gateway());
    // Gateway runs the fan-out/index loops but not the compile worker.
    assert!(!Role::Gateway.runs_forge());
    assert!(Role::Gateway.runs_gateway());
}

/// Role deserializes from the lowercase wire token (`NUDOX_ROLE=forge`).
#[test]
fn role_parses_lowercase_token() {
    let forge: Role = serde_json::from_str("\"forge\"").expect("forge parses");
    assert_eq!(forge, Role::Forge);
    let gateway: Role = serde_json::from_str("\"gateway\"").expect("gateway parses");
    assert_eq!(gateway, Role::Gateway);
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

// ──────────────────────────────────────────────────────────────────────────────
// Boot guard: production deployment validation
// ──────────────────────────────────────────────────────────────────────────────

/// Production deployment with the default rerank model → validation passes.
///
/// Assert: a `production` deployment with the stock configuration passes
///   `validate()` — the catalog is local DoltLite (no credentials), and the
///   default rerank model (`mxbai-rerank-base-v2`) is Apache-2.0 licensed.
#[test]
fn production_with_default_config_passes_validation() {
    let mut configuration = ServerConfiguration::default();
    configuration.deployment = Deployment::Production;

    configuration.validate().expect(
        "production with default configuration must pass structural validation"
    );
}

/// Development (default) with default credentials → validation passes.
///
/// Assert: the default `development` tier accepts the stock localhost config so
///   a `cargo run` / `buck2 run` works out of the box without configuring secrets.
#[test]
fn development_with_default_credentials_passes_validation() {
    let configuration = ServerConfiguration::default();
    assert_eq!(
        configuration.deployment,
        Deployment::Development,
        "the default deployment must be Development"
    );
    configuration.validate().expect(
        "development + default credentials must pass validation (no boot guard in dev)"
    );
}
