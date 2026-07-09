//! CAS errors.

use std::path::PathBuf;

use thiserror::Error;

/// Failure from a content-addressed store operation.
#[derive(Debug, Error)]
pub enum CasError {
	/// Local filesystem failure.
	#[error("cas io error at {path:?}: {source}")]
	Io {
		/// Path involved, when known.
		path: Option<PathBuf>,
		/// Underlying IO error.
		#[source]
		source: std::io::Error,
	},

	/// Disk blob failed its self-integrity check (blake3 envelope).
	#[error("cas integrity check failed at {path}")]
	Integrity {
		/// Path of the corrupt blob.
		path: PathBuf,
	},

	/// Operation not available on this backend (e.g. unwired L3).
	#[error("cas: {0}")]
	Unsupported(&'static str),
}

impl CasError {
	pub(crate) fn io(path: impl Into<Option<PathBuf>>, source: std::io::Error) -> Self {
		Self::Io { path: path.into(), source }
	}
}
