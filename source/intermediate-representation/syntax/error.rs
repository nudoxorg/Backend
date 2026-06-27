use arborium_tree_sitter as tree_sitter;

#[derive(Debug)]
pub enum ParseError {
	Language(tree_sitter::LanguageError),
	Parse,
}

impl std::fmt::Display for ParseError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			ParseError::Language(e) => write!(f, "language error: {e}"),
			ParseError::Parse => write!(f, "parse failed"),
		}
	}
}

impl std::error::Error for ParseError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			ParseError::Language(e) => Some(e),
			ParseError::Parse => None,
		}
	}
}

impl From<tree_sitter::LanguageError> for ParseError {
	fn from(e: tree_sitter::LanguageError) -> Self { ParseError::Language(e) }
}
