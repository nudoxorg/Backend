//! Store-neutral embedding input types.
//!
//! These live in the `ingest` module because they belong to the parse-once
//! projection layer. The `terminusdb` module consumes them rather than defining
//! them — keeping the dependency arrow pointing inward.

use crate::http::error::EmbeddingError;

/// The high-level semantic type of a vectorized record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
	/// Main semantic view of a node for feature-oriented retrieval.
	SemanticNode,
	/// Future: shape/field/layout-oriented similarity.
	StructuralNode,
}

impl RecordKind {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::SemanticNode => "semantic_node",
			Self::StructuralNode => "structural_node",
		}
	}
}

/// The content view represented by a vector.
///
/// One graph node may emit multiple records (docs, signature, context,
/// structural summary). This enum is intentionally small for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepresentationKind {
	Docs,
	Signature,
	Context,
	Structural,
}

impl RepresentationKind {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Docs => "docs",
			Self::Signature => "signature",
			Self::Context => "context",
			Self::Structural => "structural",
		}
	}
}

/// The store-agnostic input unit for embedding.
///
/// Produced by
/// [`crate::ingest::parsed_symbol::ParsedSymbol::into_embedding_document`]
/// during the parse-once projection; consumed by `EmbeddingService` in the
/// terminus module. Keeping this type here ensures the neutral projection layer
/// has no dependency on terminus-specific modules.
#[derive(Debug, Clone)]
pub struct EmbeddingDocument {
	/// Stable logical key for this embedding record (usually the entry URI).
	pub record_key:          String,
	/// Canonical URI shared with TerminusDB.
	pub uri:                 String,
	/// Text that will be sent to the embedding provider.
	pub text:                String,
	pub fq_name:             Option<String>,
	pub language:            String,
	pub package:             String,
	pub version:             Option<String>,
	pub symbol_kind:         Option<String>,
	pub record_kind:         RecordKind,
	pub representation_kind: RepresentationKind,
	pub chunk_index:         Option<u32>,
	pub chunk_count:         Option<u32>,
}

impl EmbeddingDocument {
	pub fn validate(&self) -> Result<(), EmbeddingError> {
		if self.text.trim().is_empty() {
			return Err(EmbeddingError::MissingText);
		}
		Ok(())
	}
}

/// Build the embedding text body for a symbol.
///
/// Single owner of the identity→text mapping: always includes `fq_name` and
/// `name`; appends kind, aliases, and documentation when present. Fed directly
/// from the parse-once projection so neither the full-text nor vector sink
/// needs to reconstruct it.
pub fn build_entry_embedding_text(
	name: &str,
	fq_name: &str,
	symbol_kind: Option<&str>,
	aliases: &[String],
	documentation: Option<&str>,
) -> String {
	let mut parts: Vec<String> = Vec::new();

	parts.push(format!("fq_name: {fq_name}"));
	parts.push(format!("name: {name}"));

	if let Some(kind) = symbol_kind {
		parts.push(format!("kind: {kind}"));
	}

	if !aliases.is_empty() {
		parts.push(format!("aliases: {}", aliases.join(", ")));
	}

	if let Some(doc) = documentation {
		let trimmed = doc.trim();
		if !trimmed.is_empty() {
			parts.push(format!("documentation: {trimmed}"));
		}
	}

	parts.join("\n")
}
