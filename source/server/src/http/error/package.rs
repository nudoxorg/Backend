use std::{io, process::ExitStatus};

use semver::Version;
use thiserror::Error;

use super::git::GitError;
use super::registry::RegistryError;
use crate::core::rust::ParseError;

#[derive(Debug, Error)]
pub enum PackageError {
	#[error("IO error: {0}")]
	Io(#[from] io::Error),

	#[error("process `{command}` failed with {status}{details}")]
	Process { command: String, status: ExitStatus, details: String },

	#[error("parse error: {0}")]
	Parse(#[from] ParseError),

	#[error("serialization error: {0}")]
	Serialization(#[from] serde_json::Error),

	#[error("feature not implemented")]
	NotImplemented,

	#[error("registry error: {0}")]
	Registry(#[from] RegistryError),

	#[error("metadata error: {0}")]
	Metadata(String),

	#[error("version {0} not found")]
	VersionNotFound(Version),

	#[error("git error: {0}")]
	Git(#[from] GitError),
}
