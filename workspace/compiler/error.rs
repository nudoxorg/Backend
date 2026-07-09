//! The compiler's error taxonomy for the generation pipeline.

use thiserror::Error;

/// Concrete failures from the linked-data emit phase (no more dyn boxes).
#[derive(Debug, Error)]
pub enum EmitLinkedDataError {
	#[error("conflicting document bodies for @id {id}")]
	ConflictingDocumentBody { id: String },
}

/// A failure while generating the resolutions (surface IR, CST, source archive,
/// blob info, linked data) for a package.
///
/// Lower/Emit no longer use `Box<dyn Error>`; every language backend and
/// emit failure is carried by a named concrete variant so sources are
/// explicitly chained and downcasting / matching is possible without
/// type erasure.
#[derive(Debug, Error)]
pub enum GenerateError {
	/// Rust source lowering to IR failed (cargo/rustdoc + rustdoc JSON parse).
	#[error("failed to lower Rust source to IR")]
	LowerRust(#[from] crate::languages::rust::Package),

	/// TypeScript source lowering to IR failed (deno-graph + deno-doc).
	#[error("failed to lower TypeScript source to IR")]
	LowerTypescript(#[from] crate::languages::typescript::Package),

	/// Go module lowering to IR failed.
	#[error("failed to lower Go source to IR")]
	LowerGo(#[from] crate::languages::go::GoError),

	/// Java project lowering to IR failed.
	#[error("failed to lower Java source to IR")]
	LowerJava(#[from] crate::languages::java::JavaError),

	/// Nix flake lowering to IR failed (FlakeHub acquisition, static analysis,
	/// or hermetic evaluation).
	#[error("failed to lower Nix source to IR")]
	LowerNix(#[from] crate::languages::nix::NixError),

	/// Extracting the CST for a file failed.
	#[error("failed to extract CST for {path}")]
	Cst {
		/// The offending source file.
		path: String,
		/// The underlying parse error (now may carry its own source file +
		/// the original LanguageError via source chain).
		#[source]
		source: ir::syntax::ParseError,
	},

	/// Reading the package source / building the archive failed.
	#[error("failed to read or archive package source")]
	Archive(#[from] std::io::Error),

	/// VCS/git clone, fetch, materialize, reference walk or manifest parse failed.
	/// These surface during git-based package acquisition (before lowering).
	#[error("VCS/git operation failed")]
	Vcs(#[from] crate::languages::vcs::GitError),

	/// Emitting linked-data documents to the sink failed.
	#[error("failed to emit linked data")]
	EmitLinkedData(#[from] EmitLinkedDataError),

	/// The package's ecosystem has no generation backend wired up.
	#[error("no generation backend for this ecosystem")]
	UnsupportedEcosystem,

	/// Rust package coordinates did not carry a Cargo version.
	#[error("rust packages require a Cargo version")]
	UnsupportedRust,

	/// TypeScript package coordinates did not carry an npm version.
	#[error("typescript packages require an npm version")]
	UnsupportedTypescript,
}

impl heart::Retryable for GenerateError {
	fn is_retryable(&self) -> bool {
		// Generation is CPU-bound and deterministic: I/O aside, a failure will
		// recur. Only transient source-read errors are worth a retry.
		// VCS git ops (clone/fetch) can be transient (network, lock, etc).
		match self {
			GenerateError::Archive(e) if e.kind() == std::io::ErrorKind::Interrupted => true,
			GenerateError::Vcs(g) => is_git_retryable(g),
			_ => false,
		}
	}
}

/// Classify GitError for retry: IO errors that look transient, plus fetch/connect errors.
fn is_git_retryable(e: &crate::languages::vcs::GitError) -> bool {
	match e {
		crate::languages::vcs::GitError::Io { source, .. } => matches!(
			source.kind(),
			std::io::ErrorKind::Interrupted
				| std::io::ErrorKind::TimedOut
				| std::io::ErrorKind::ConnectionReset
				| std::io::ErrorKind::ConnectionAborted
		),
		crate::languages::vcs::GitError::Fetch(_) => true, // connect, receive, fallback etc are often transient
		_ => false,
	}
}
