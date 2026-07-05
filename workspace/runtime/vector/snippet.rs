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
	pub fn extract(source: &str, span: Range<usize>, context_lines: u32) -> Self {
		let _ = (source, span, context_lines);
		todo!("clip to context_lines around span, recompute the relative highlight range")
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
	let _ = (store, gate, embedder, snippet, purpose, limit);
	todo!("embed the snippet, k-NN search the store")
}
