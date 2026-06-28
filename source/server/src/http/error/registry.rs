use thiserror::Error;

#[derive(Debug, Error)]
pub enum RegistryError {
	#[error("crates.io registry error: {0}")]
	CratesIo(#[from] producers::parse::rust::Registry),
}
