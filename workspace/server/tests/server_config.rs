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
// Boot guard: production + default credentials → hard error
// ──────────────────────────────────────────────────────────────────────────────

/// Production + default postgres URL → validation error naming the field.
///
/// Assert: a `production` deployment with the stock `postgres://nudox:nudox@…`
///   URL must be rejected at `validate()` before any network connection opens.
#[test]
fn production_with_default_postgres_is_rejected() {
    let mut configuration = ServerConfiguration::default();
    configuration.deployment = Deployment::Production;
    // postgres URL is still the default ("postgres://nudox:nudox@127.0.0.1:5432/nudox")
    // terminus_password is still "root" too, but postgres is checked first.

    let error = configuration.validate().expect_err(
        "production + default postgres must be a hard validation error",
    );
    let message = error.to_string();
    assert!(
        message.contains("endpoints.postgres"),
        "error must name the unsafe field; got: {message}"
    );
}

/// Production + default terminus password → validation error naming the field.
///
/// Assert: if postgres is customised but terminus_password is still `root`,
///   `validate()` must reject with the terminus field named.
#[test]
fn production_with_default_terminus_password_is_rejected() {
    use secrecy::SecretString;

    let mut configuration = ServerConfiguration::default();
    configuration.deployment = Deployment::Production;
    // Override postgres so that check passes and we reach the password check.
    configuration.definitive.endpoints.postgres =
        SecretString::from("postgres://prod_user:s3cr3t@db.example.com:5432/prod");

    let error = configuration.validate().expect_err(
        "production + default terminus password must be a hard validation error",
    );
    let message = error.to_string();
    assert!(
        message.contains("endpoints.terminus_password"),
        "error must name the unsafe field; got: {message}"
    );
}

/// Production + both credentials non-default → validation passes.
///
/// Assert: once both secrets are overridden, `validate()` returns `Ok`.
#[test]
fn production_with_real_credentials_passes_validation() {
    use secrecy::SecretString;

    let mut configuration = ServerConfiguration::default();
    configuration.deployment = Deployment::Production;
    configuration.definitive.endpoints.postgres =
        SecretString::from("postgres://prod_user:s3cr3t@db.example.com:5432/prod");
    configuration.definitive.endpoints.terminus_password =
        SecretString::from("sup3r_s3cr3t_terminus_pw");

    configuration.validate().expect(
        "production with non-default credentials must pass structural validation",
    );
}

/// Development (default) with default credentials → validation passes.
///
/// Assert: the boot guard is not applied in the `development` tier so local
///   `buck2 run` / CI setups work without configuring secrets.
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
