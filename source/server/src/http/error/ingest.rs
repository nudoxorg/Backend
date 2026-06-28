use thiserror::Error;

use parsers::rust::error;
use parsers::typescript::error::Package as TsPackageError;

#[derive(Debug, Error)]
pub enum IngestError {
	#[error("IR generation failed for `{package}`")]
	IrGeneration {
		package: String,
		#[source]
		source:  error::Package,
	},

	#[error("IR generation failed for `{package}`")]
	TsIrGeneration {
		package: String,
		#[source]
		source:  TsPackageError,
	},

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
