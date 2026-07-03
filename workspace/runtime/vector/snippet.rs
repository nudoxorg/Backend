//! Similarity from an ad-hoc snippet, plus the source-snippet types used to
//! render a result for display.
//!
//! Two related jobs live here:
//! - **query-by-snippet**: embed an arbitrary piece of code the caller pasted
//!   (no stored symbol needed) and search for near symbols;
//! - **result snippets**: the small excerpt of source shown alongside a hit, so
//!   the display layer has a language-agnostic shape to render.

use std::{num::NonZeroUsize, ops::Range};

use serde::{Deserialize, Serialize};

use heart::{AccessContext, SymbolId, Scored};
use smol_str::SmolStr;

use crate::{
	error::VectorError,
	vector::{
		SemanticLive,
		embedding::{Embedder, EmbeddingPurpose},
		gate::SemanticGate,
		model::EmbeddingModel,
	},
};

/// A rendered source excerpt for a search result: the lines to show plus the
/// byte range within them to emphasize (the matched span).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
	/// The source excerpt to display.
	pub text: SmolStr,
	/// 1-based line number the excerpt begins at, for gutter display.
	pub start_line: u32,
	/// The byte range *within `text`* to highlight (the matched span).
	pub highlight: Range<usize>,
}

impl Snippet {
	/// Extract a display snippet from `source`, centered on `span`, with up to
	/// `context_lines` of surrounding context on each side.
	pub fn extract(source: &str, span: Range<usize>, context_lines: u32) -> Self {
		let _ = (source, span, context_lines);
		todo!("clip to context_lines around span, recompute the relative highlight range")
	}
}

/// Embed an arbitrary code `snippet` and search for semantically near symbols,
/// without requiring the snippet to correspond to any stored symbol.
///
/// Gated and access-scoped like every semantic query. Embedding happens on the
/// gated path (heavy work is never implicit).
pub async fn similar_to_snippet<M: EmbeddingModel, E: Embedder<Model = M>>(
	store: &SemanticLive<M>,
	gate: SemanticGate,
	embedder: &E,
	snippet: &str,
	purpose: EmbeddingPurpose,
	limit: NonZeroUsize,
	scope: &AccessContext,
) -> Result<Vec<Scored<SymbolId>>, VectorError> {
	let _ = (store, gate, embedder, snippet, purpose, limit, scope);
	todo!("embed the snippet, k-NN search the store")
}
