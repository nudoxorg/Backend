use thiserror::Error;

#[derive(Debug, Error)]
pub enum IngestError {
	#[error("occurrence store register_library failed for `{package}`")]
	StoreRegister {
		package: String,
		#[source]
		source:  nudox_core::Error,
	},

	#[error("Package resolution failed")]
	PackageResolution {
		#[source]
		source: reqwest::Error,
	},

	#[error("pipeline error: {details}")]
	Pipeline { details: String },
}
