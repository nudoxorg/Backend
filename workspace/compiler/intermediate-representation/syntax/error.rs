use arborium_tree_sitter as tree_sitter;
use thiserror::Error;

/// Syntax / tree-sitter parse failures. Carries optional source-file context
/// so that top-level errors (e.g. CST generation) can preserve the full chain
/// including the originating LanguageError.
#[derive(Debug, Error)]
pub enum ParseError {
	#[error("language error: {0}")]
	Language(#[from] tree_sitter::LanguageError),

	#[error("parse failed")]
	Parse,

	/// Language grammar initialization error with the file that triggered it.
	#[error("language error for {path}: {source}")]
	LanguageWithSource {
		path:   String,
		#[source]
		source: tree_sitter::LanguageError,
	},

	/// General parse failure for a specific file.
	#[error("parse failed for source file {path}")]
	ParseWithSource { path: String },
}

impl ParseError {
	/// Attach file context when available, promoting to the contextual variant.
	/// Idempotent for already-contextual variants.
	pub fn with_source_file(self, path: impl Into<String>) -> Self {
		let p = path.into();
		match self {
			ParseError::Language(source) => ParseError::LanguageWithSource { path: p, source },
			ParseError::Parse => ParseError::ParseWithSource { path: p },
			other => other,
		}
	}
}
