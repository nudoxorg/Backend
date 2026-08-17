//! Binary entrypoint: resolve configuration, initialise tracing + metrics,
//! assemble the `Server` over verified backing infrastructure, and serve until
//! shutdown.

use std::sync::Arc;

use index::server::{Driver, ServerConfiguration};

/// The compiled-in embedding model. The whole server is monomorphized over this
/// brand, so a store built for a *different model* (not merely a different
/// dimension) cannot be wired in. Switching models is a deliberate recompile +
/// migration.
///
/// `NomicEmbedText` rather than the parity `JinaCodeV2` brand: this build is
/// wired against a local Ollama instance (`127.0.0.1:11434`) that has
/// `nomic-embed-text` pulled, not the `JinaCodeV2` ONNX weights artifact.
/// Both brands are 768-dimensional (`registry::vector::core::model`), so the
/// qdrant collection schema is unaffected — only the model id embedded
/// points are tagged with changes. Revert to `JinaCodeV2` for a deployment
/// that serves the real parity weights.
type EmbedModel = index::server::vector::NomicEmbedText;

/// Fast general-purpose allocator for the serving path.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = ServerConfiguration::resolve()?;

    // Telemetry init replaces the old bare `tracing_subscriber::fmt()...init()`
    // call: it builds one `Registry` with a journal-bound `fmt` layer plus (SDK
    // permitting) OTLP trace/metric/log pipelines and installs the W3C
    // TraceContext propagator. Held for the lifetime of `main` so its `Drop`
    // flushes/shuts every provider down on exit. Must run after `config`
    // resolves (env is authoritative for both), and before anything that might
    // emit a `tracing` event, so nothing during assembly is lost to the old
    // subscriber. See `workspace/telemetry` and `OBSERVABILITY-PLAN.md`.
    //
    // `service_version`/`build_system` have no Buck2-injected value today (no
    // `CARGO_PKG_VERSION` equivalent wired through the Buck2 build yet — see
    // CARGO-BUCK-PLAN.md for the tooling that would thread one through). Under
    // `cargo build` this crate has no `Cargo.toml` of its own, so
    // `env!("CARGO_PKG_VERSION")` falls back to whatever cargo sets for the
    // invoking package; under Buck2 it resolves to the literal string. Both
    // are honest placeholders pending a real build-version injection (TODO).
    let service_version = option_env!("CARGO_PKG_VERSION")
        .unwrap_or("unknown")
        .to_owned();
    let telemetry_config =
        heart::telemetry::TelemetryConfig::resolve(service_version, "buck2-or-cargo");
    let _guard = heart::telemetry::init(&telemetry_config)?;

    // The compile plane now runs as a separate daemon; `Server::assemble` only
    // constructs a `CompilerClient` pointing at `config.compiler_endpoint`. No
    // in-process cage, CAS, or worker pools.

    // Access control for hosted deployments happens at the fronting proxy (see
    // `heart::access`); in-process, everyone authenticated to reach us may act.
    let server = Arc::new(Driver::<EmbedModel>::assemble(config).await?);
    server.serve().await?;
    Ok(())
}
