//! `HfTokenCounter` — the recipe's token arithmetic over the *model's own*
//! tokenizer (feature `onnx`).
//!
//! The recipe budgets (09b §3.1b: sig ≤ 512, doc ≤ 256, body = remainder of
//! 1024) are defined in **model tokens**, so they must be measured with the
//! pinned `tokenizer.json` — the same file whose sha participates in
//! `tool_digest`. A different tokenizer silently shifts every truncation
//! boundary and thus every `embed_key` input.

use std::path::Path;

use tokenizers::Tokenizer;
use crate::vector::core::recipe::TokenCounter;

/// [`TokenCounter`] over a HF `tokenizer.json`.
pub struct HfTokenCounter {
	tokenizer: Tokenizer,
}

impl HfTokenCounter {
	/// Load from a `tokenizer.json` path (normally
	/// [`super::weights::WeightsSpec::sidecar`]`("tokenizer.json")`).
	pub fn from_file(path: impl AsRef<Path>) -> Result<Self, String> {
		Tokenizer::from_file(path.as_ref())
			.map(|tokenizer| Self { tokenizer })
			.map_err(|error| format!("loading tokenizer {}: {error}", path.as_ref().display()))
	}

	/// Wrap an already-loaded tokenizer (e.g. shared with a fastembed
	/// session).
	pub fn new(tokenizer: Tokenizer) -> Self { Self { tokenizer } }

	/// Encode without special tokens: the recipe counts *content* tokens;
	/// CLS/SEP overhead is the runtime's concern, not the recipe's.
	fn encode(&self, text: &str) -> tokenizers::Encoding {
		// Hard fail on tokenizer errors (09c §4.6): a text we cannot
		// tokenize deterministically must not silently produce a vector.
		self.tokenizer.encode(text, false).expect("tokenizer encode failed")
	}
}

impl TokenCounter for HfTokenCounter {
	fn count(&self, text: &str) -> usize { self.encode(text).get_ids().len() }

	/// Cut on a token boundary and re-decode, so the text the model sees is
	/// exactly the text the budget accounted for. Deterministic: no sampling,
	/// no smart selection (09b §3.1b truncation algorithm).
	fn truncate_to(&self, text: &str, max_tokens: usize) -> String {
		let encoding = self.encode(text);
		let ids = encoding.get_ids();
		if ids.len() <= max_tokens {
			// Under budget: return the original verbatim (avoids any
			// decode round-trip drift on the common path).
			return text.to_owned();
		}
		self.tokenizer
			.decode(&ids[..max_tokens], true)
			.expect("tokenizer decode failed")
	}
}
