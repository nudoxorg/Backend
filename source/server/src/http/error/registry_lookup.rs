use std::io;

use lang_types::Language;
use producers::parse::typescript::error::Package as TsPackageError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RegistryLookupError {
	#[error("crates.io API error for `{package}`: {source}")]
	CratesIo {
		language: Language,
		package:  String,
		#[source]
		source:   producers::parse::rust::Registry,
	},

	#[error("npm registry error for `{package}`: {source}")]
	Npm {
		language: Language,
		package:  String,
		#[source]
		source:   TsPackageError,
	},

	#[error("package `{package}` not found")]
	NotFound { language: Language, package: String },

	#[error("failed to resolve path `{path}` for `{package}`: {source}")]
	PathResolution {
		language: Language,
		package:  String,
		path:     String,
		#[source]
		source:   io::Error,
	},

	#[error("`{path}` is not a valid repository path or URL for `{package}`")]
	InvalidRepositoryPath { language: Language, package: String, path: String },
}
