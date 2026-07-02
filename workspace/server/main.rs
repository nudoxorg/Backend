//! Binary entrypoint: resolve configuration, initialise tracing + metrics,
//! assemble the `Server` over verified backing infrastructure, and serve until
//! shutdown.

use std::sync::Arc;

use server::{Server, ServerConfiguration};

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
	// TODO: init tracing-subscriber (env-filter + json) and the prometheus recorder.
	let config = ServerConfiguration::resolve()?;

	// TODO: build the concrete AccessPolicy (default or enterprise-supplied).
	let policy = todo!("construct the access policy");

	let server = Arc::new(Server::<EmbedModel>::assemble(config, policy).await?);
	server.serve().await?;
	Ok(())
}
