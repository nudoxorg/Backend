use thiserror::Error;

#[derive(Debug, Error)]
pub enum RegistryError {
	#[error("crates.io API error: {0}")]
	CratesIo(#[from] crates_io_api::Error),
}
