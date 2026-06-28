use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
	#[error("IR generation failed for `{package}`")]
	IrGeneration {
		package: String,
		#[source]
		source:  crate::parse::rust::error::Package,
	},

	#[error("TypeScript IR generation failed for `{package}`")]
	TsIrGeneration {
		package: String,
		#[source]
		source:  crate::parse::typescript::error::Package,
	},

	#[error("could not determine a TypeScript entry point in `{path}`")]
	TypescriptEntryPointDiscovery { path: String },

	#[error("TypeScript entry point `{path}` does not exist")]
	TypescriptEntryPointMissing { path: String },

	#[error("storage error at `{path}`: {source}")]
	Storage {
		path:   std::path::PathBuf,
		#[source]
		source: std::io::Error,
	},

	#[error("JSON error: {0}")]
	Json(#[from] serde_json::Error),
}
