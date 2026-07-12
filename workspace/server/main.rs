//! Binary entrypoint: resolve configuration, initialise tracing + metrics,
//! assemble the `Server` over verified backing infrastructure, and serve until
//! shutdown.

use std::sync::Arc;

use server::{Server, ServerConfiguration, UnrestrictedAccess};

/// The compiled-in embedding model. The whole server is monomorphized over this
/// brand, so a store built for a *different model* (not merely a different
/// dimension) cannot be wired in. Switching models is a deliberate recompile +
/// migration.
type EmbedModel = runtime::vector::models::OpenAi3Small;

/// Fast general-purpose allocator for the serving path.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// The vendored tracing-subscriber build carries no env-filter; the level
	// comes from `NUDOX_LOG`/`RUST_LOG` as a plain level name, defaulting to info.
	// (The prometheus recorder installs inside `serve()`, on the runtime.)
	tracing_subscriber::fmt().with_max_level(log_level()).init();

	let config = ServerConfiguration::resolve()?;

	// Access control for hosted deployments happens at the fronting proxy (see
	// `heart::access`); in-process, everyone authenticated to reach us may act.
	let policy = Arc::new(UnrestrictedAccess);

	let server = Arc::new(Server::<EmbedModel>::assemble(config, policy).await?);
	server.serve().await?;
	Ok(())
}

/// The maximum tracing level: `NUDOX_LOG` (then `RUST_LOG`) as a level name,
/// defaulting to `info`.
fn log_level() -> tracing::Level {
	["NUDOX_LOG", "RUST_LOG"]
		.iter()
		.find_map(|name| std::env::var(name).ok())
		.and_then(|raw| raw.parse().ok())
		.unwrap_or(tracing::Level::INFO)
}
