//! Similarity from an ad-hoc snippet, plus the source-snippet types.

use std::{num::NonZeroUsize, ops::Range};

use serde::{Deserialize, Serialize};

use heart::{SymbolId, Scored};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
	pub text: SmolStr,
	pub start_line: u32,
	pub highlight: Range<usize>,
}

impl Snippet {
	/// Clip `source` to the lines containing `span` (a byte range) plus
	/// `context_lines` of context on each side, recomputing the highlight
	/// relative to the clip. `start_line` is the 1-based line the clip begins
	/// on. Out-of-bounds or non-boundary span indices are clamped inward, so
	/// extraction is total over any input.
	pub fn extract(source: &str, span: Range<usize>, context_lines: u32) -> Self {
		// Clamp an index into the source, snapping back to a char boundary.
		let clamp = |index: usize| {
			let mut index = index.min(source.len());
			while !source.is_char_boundary(index) {
				index -= 1;
			}
			index
		};
		let start = clamp(span.start);
		let end = clamp(span.end).max(start);

		// Walk the clip start back to the beginning of the span's line, then
		// `context_lines` more line starts.
		let line_start =
			|from: usize| source[..from].rfind('\n').map_or(0, |newline| newline + 1);
		let mut clip_start = line_start(start);
		for _ in 0..context_lines {
			if clip_start == 0 {
				break;
			}
			clip_start = line_start(clip_start - 1);
		}

		// And the clip end forward to the end of the span's line, then
		// `context_lines` more line ends.
		let line_end =
			|from: usize| source[from..].find('\n').map_or(source.len(), |newline| from + newline);
		let mut clip_end = line_end(end);
		for _ in 0..context_lines {
			if clip_end >= source.len() {
				break;
			}
			clip_end = line_end(clip_end + 1);
		}

		let start_line = source[..clip_start].bytes().filter(|byte| *byte == b'\n').count() as u32 + 1;
		Snippet {
			text: SmolStr::new(&source[clip_start..clip_end]),
			start_line,
			highlight: (start - clip_start)..(end - clip_start),
		}
	}
}

pub async fn similar_to_snippet<M: EmbeddingModel, E: Embedder<Model = M>>(
	store: &SemanticLive<M>,
	gate: SemanticGate,
	embedder: &E,
	snippet: &str,
	purpose: EmbeddingPurpose,
	limit: NonZeroUsize,
) -> Result<Vec<Scored<SymbolId>>, VectorError> {
	use futures::TryStreamExt;

	// Embedding happens here, inside the gated path — never on the default one.
	let query = embedder.embed(snippet, purpose).await.map_err(VectorError::Embed)?;
	store.search(gate, &query, limit, None).await?.try_collect().await
}
