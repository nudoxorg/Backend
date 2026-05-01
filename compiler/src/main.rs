use color_eyre::eyre;
use nudox::{config::AppConfig, server};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> eyre::Result<()> {
	color_eyre::install()?;
	let config = AppConfig::from_env()?;

	tracing_subscriber::fmt()
		.with_env_filter(EnvFilter::new(config.tracing_filter.clone()))
		.with_target(true)
		.compact()
		.init();

	server::run(config).await?;

	Ok(())
}
