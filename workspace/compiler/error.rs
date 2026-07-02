//! The compiler's error taxonomy for the generation pipeline.

use thiserror::Error;

/// A failure while generating the resolutions (surface IR, CST, source archive,
/// blob info, linked data) for a package.
#[derive(Debug, Error)]
pub enum GenerateError {
	/// Lowering the package source to the IR surface failed.
	#[error("failed to lower source to IR")]
	Lower(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// Extracting the CST for a file failed.
	#[error("failed to extract CST for {path}")]
	Cst {
		/// The offending source file.
		path: String,
		/// The underlying parse error.
		#[source]
		source: ir::syntax::ParseError,
	},

	/// Reading the package source / building the archive failed.
	#[error("failed to read or archive package source")]
	Archive(#[source] std::io::Error),

	/// Emitting linked-data documents to the sink failed.
	#[error("failed to emit linked data")]
	Emit(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// The package's ecosystem has no generation backend wired up.
	#[error("no generation backend for this ecosystem")]
	UnsupportedEcosystem,
}

impl heart::Retryable for GenerateError {
	fn is_retryable(&self) -> bool {
		// Generation is CPU-bound and deterministic: I/O aside, a failure will
		// recur. Only transient source-read errors are worth a retry.
		matches!(self, GenerateError::Archive(e) if e.kind() == std::io::ErrorKind::Interrupted)
	}
}
