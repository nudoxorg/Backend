use std::{ops::Range, sync::Arc};

use arborium_tree_sitter as tree_sitter;
use yoke::{Yoke, Yokeable};

use crate::syntax::{ParseError, ResolvedReference, walker::walk_references};

#[derive(Yokeable, Debug, Clone)]
pub struct FunctionBody {
	pub tree:       tree_sitter::Tree,
	pub references: Vec<ResolvedReference>,
}

impl PartialEq for FunctionBody {
	fn eq(&self, other: &Self) -> bool { self.references == other.references }
}

impl Eq for FunctionBody {}

pub type ParsedBody = Yoke<FunctionBody, Arc<str>>;

impl FunctionBody {
	pub fn parse(
		source: Arc<str>,
		language: tree_sitter::Language,
	) -> Result<ParsedBody, ParseError> {
		Yoke::try_attach_to_cart(source, |source_ref| {
			let mut parser = tree_sitter::Parser::new();
			parser.set_language(&language)?;
			let tree = parser.parse(source_ref.as_bytes(), None).ok_or(ParseError::Parse)?;
			Ok(FunctionBody { tree, references: Vec::new() })
		})
	}

	pub fn parse_and_resolve(
		source: Arc<str>,
		language: tree_sitter::Language,
		classify: impl FnMut(&str, &str, Option<&str>, Range<usize>) -> Option<ResolvedReference>,
	) -> Result<ParsedBody, ParseError> {
		Yoke::try_attach_to_cart(source, |source_ref| {
			let mut parser = tree_sitter::Parser::new();
			parser.set_language(&language)?;
			let tree = parser.parse(source_ref.as_bytes(), None).ok_or(ParseError::Parse)?;
			let references = walk_references(&tree, source_ref, classify);
			Ok(FunctionBody { tree, references })
		})
	}
}
