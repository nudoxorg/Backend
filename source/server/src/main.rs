use nudox::{config::AppConfig, server};
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	let config = AppConfig::from_env()?;

	tracing_subscriber::fmt()
		.with_env_filter(EnvFilter::new(config.tracing_filter.clone()))
		.with_target(true)
		.compact()
		.init();

	server::run(config).await?;

	Ok(())
}
