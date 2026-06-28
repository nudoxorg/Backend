use thiserror::Error;

use crate::core::ts::TsPackageError;

#[derive(Debug, Error)]
pub enum IngestError {
	#[error("IR generation failed for `{package}`")]
	IrGeneration {
		package: String,
		#[source]
		source:  nudox_rust_parser::PackageError,
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

	#[error("TsPackage resolution failed")]
	TsPackageResolution {
		#[source]
		source: reqwest::Error,
	},

	#[error("pipeline error: {details}")]
	Pipeline { details: String },
}
