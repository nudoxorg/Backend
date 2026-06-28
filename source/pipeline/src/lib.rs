pub mod treesitter;

use nudox_core::{BlobInfo, ByteSpan, ChunkMetadata, Embedder, EmbeddingPurpose, EmbeddingRecord, OccurrenceId, Result, SourceChunk, SymbolOrigin};
use uuid::Uuid;

/// Raw input accepted by the pipeline before BlobInfo is assembled.
pub struct PipelineInput {
	/// The raw source code surrounding the symbol occurrence.
	pub raw_code:      String,
	/// Name of the symbol as it appeared at the use site.
	pub symbol_name:   String,
	/// Byte span of the symbol within `raw_code`.
	pub symbol_span:   ByteSpan,
	/// Classification of where the symbol came from.
	pub symbol_origin: SymbolOrigin,
	/// File-level metadata for the occurrence.
	pub metadata:      ChunkMetadata,
	/// Optional pre-extracted docstring or comment text for a separate embedding.
	pub docstring:     Option<String>,
}

/// Configuration for the pipeline.
pub struct PipelineConfig {
	/// Maximum lines of surrounding context when the enclosing function is too
	/// large.
	pub max_context_lines: usize,
	/// Whether to generate a separate embedding for docstrings/comments.
	pub embed_docstrings:  bool,
}

impl Default for PipelineConfig {
	fn default() -> Self { PipelineConfig { max_context_lines: 80, embed_docstrings: true } }
}

/// Converts a `PipelineInput` into a complete `BlobInfo`.
///
/// Runs every configured embedder against every chunk, producing one
/// [`EmbeddingRecord`] per (embedder, purpose) pair. Each record carries the
/// embedder's `model_type` and `model_id` so consumers can disambiguate
/// vectors from different providers.
///
/// `resolved_global_id` is always `None` here; resolution is the orchestrator's
/// job.
pub struct Pipeline {
	embedders: Vec<Box<dyn Embedder>>,
	config:    PipelineConfig,
}

impl Pipeline {
	/// Create a pipeline driven by the given embedders.
	///
	/// Each input chunk will be embedded by every embedder in order, producing
	/// one [`EmbeddingRecord`] per embedder per purpose.
	pub fn new(embedders: Vec<Box<dyn Embedder>>, config: PipelineConfig) -> Self {
		Pipeline { embedders, config }
	}

	/// Number of embedders driving this pipeline.
	pub fn embedder_count(&self) -> usize { self.embedders.len() }

	/// Process a single input, generating embeddings and assembling a `BlobInfo`.
	///
	/// Uses the Backend IR's tree-sitter parser to extract the smallest enclosing
	/// function/closure node around `symbol_span` as the embedding chunk
	/// (snippet-boundary policy). Falls back to a centered line window when the
	/// enclosing function exceeds `max_context_lines`, or to the full `raw_code`
	/// when no enclosing function is found. The parsed tree s-expression and
	/// extracted references are stored in `SourceChunk::treesitter_repr` as a
	/// JSON payload.
	#[tracing::instrument(skip(self, input), fields(symbol_name = %input.symbol_name, embedders = self.embedders.len()))]
	pub async fn process(&self, input: PipelineInput) -> Result<BlobInfo> {
		let occurrence_id = OccurrenceId(Uuid::new_v4());

		// Parse raw_code and extract the smallest enclosing function as the
		// embedding chunk, walking symbol references as a side-effect.
		let (snippet, snippet_span, treesitter_repr) = treesitter::parse_and_extract(
			&input.raw_code,
			input.metadata.lang,
			input.symbol_span,
			self.config.max_context_lines,
		);

		// Adjust symbol span to be relative to the extracted snippet.
		let symbol_span_in_snippet = ByteSpan {
			start: input.symbol_span.start.saturating_sub(snippet_span.start),
			end:   input.symbol_span.end.saturating_sub(snippet_span.start),
		};

		let source = SourceChunk {
			raw_code: snippet.clone(),
			treesitter_repr,
			symbol_span: symbol_span_in_snippet,
		};

		let mut embeddings = Vec::with_capacity(self.embedders.len() * 2);

		for embedder in &self.embedders {
			let code_vec = embedder.embed(&source, EmbeddingPurpose::Code).await?;
			embeddings.push(EmbeddingRecord {
				model_type: embedder.model_type(),
				model:      embedder.model_id().to_string(),
				purpose:    EmbeddingPurpose::Code,
				vector:     code_vec,
			});

			if self.config.embed_docstrings
				&& let Some(doc) = &input.docstring {
					let doc_chunk = SourceChunk {
						raw_code:        doc.clone(),
						treesitter_repr: None,
						symbol_span:     ByteSpan { start: 0, end: doc.len() },
					};
					let doc_vec = embedder.embed(&doc_chunk, EmbeddingPurpose::Docstring).await?;
					embeddings.push(EmbeddingRecord {
						model_type: embedder.model_type(),
						model:      embedder.model_id().to_string(),
						purpose:    EmbeddingPurpose::Docstring,
						vector:     doc_vec,
					});
				}
		}

		Ok(BlobInfo {
			occurrence_id,
			symbol_name: input.symbol_name,
			symbol_origin: input.symbol_origin,
			resolved_global_id: None,
			kind: None,
			source,
			embeddings,
			metadata: input.metadata,
		})
	}
}

#[cfg(test)]
mod tests {
	use nudox_core::{BLOB_SCHEMA_VERSION, ByteSpan, ChunkMetadata, Language, ModelType, RepoId, SymbolOrigin};
	use embed::{MockEmbedder, PlaceholderEmbedder};

	use super::*;

	#[test]
	fn pipeline_config_default_values() {
		let config = PipelineConfig::default();
		assert_eq!(config.max_context_lines, 80);
		assert!(config.embed_docstrings);
	}

	#[test]
	fn pipeline_new_constructs_without_panic() {
		let _pipeline = Pipeline::new(vec![Box::new(MockEmbedder::new(16))], PipelineConfig::default());
	}

	fn make_input(docstring: Option<&str>) -> PipelineInput {
		PipelineInput {
			raw_code:      "fn foo() {}".into(),
			symbol_name:   "foo".into(),
			symbol_span:   ByteSpan { start: 3, end: 6 },
			symbol_origin: SymbolOrigin::Repo { repo_id: RepoId("r1".into()) },
			metadata:      ChunkMetadata {
				repo_id:             RepoId("r1".into()),
				file_path:           "src/lib.rs".into(),
				file_span:           ByteSpan { start: 0, end: 11 },
				parsed_at:           chrono::Utc::now(),
				lang:                Language::Rust,
				lang_version:        None,
				blob_schema_version: BLOB_SCHEMA_VERSION,
			},
			docstring:     docstring.map(String::from),
		}
	}

	#[tokio::test]
	async fn process_with_single_embedder_produces_code_record() {
		let pipeline = Pipeline::new(vec![Box::new(MockEmbedder::new(8))], PipelineConfig::default());
		let info = pipeline.process(make_input(None)).await.unwrap();
		assert_eq!(info.symbol_name, "foo");
		assert!(info.resolved_global_id.is_none());
		assert_eq!(info.embeddings.len(), 1);
		let rec = &info.embeddings[0];
		assert_eq!(rec.vector.len(), 8);
		assert!(matches!(rec.purpose, EmbeddingPurpose::Code));
		assert_eq!(rec.model, "mock");
		assert_eq!(rec.model_type, ModelType::Mock);
	}

	#[tokio::test]
	async fn process_with_docstring_produces_two_records_per_embedder() {
		let pipeline = Pipeline::new(vec![Box::new(MockEmbedder::new(8))], PipelineConfig::default());
		let info = pipeline.process(make_input(Some("/// docs here"))).await.unwrap();
		assert_eq!(info.embeddings.len(), 2);
		assert!(matches!(info.embeddings[1].purpose, EmbeddingPurpose::Docstring));
	}

	#[tokio::test]
	async fn process_with_docstring_disabled_skips_docstring() {
		let cfg = PipelineConfig { max_context_lines: 80, embed_docstrings: false };
		let pipeline = Pipeline::new(vec![Box::new(MockEmbedder::new(8))], cfg);
		let info = pipeline.process(make_input(Some("/// docs"))).await.unwrap();
		assert_eq!(info.embeddings.len(), 1);
	}

	#[tokio::test]
	async fn process_with_two_embedders_produces_record_per_embedder() {
		let pipeline = Pipeline::new(
			vec![
				Box::new(MockEmbedder::new(8)),
				Box::new(PlaceholderEmbedder::new("placeholder-v1", 16)),
			],
			PipelineConfig::default(),
		);
		let info = pipeline.process(make_input(None)).await.unwrap();
		assert_eq!(info.embeddings.len(), 2);

		let mock_rec =
			info.embeddings.iter().find(|r| r.model_type == ModelType::Mock).expect("mock record");
		let placeholder_rec = info
			.embeddings
			.iter()
			.find(|r| r.model_type == ModelType::Placeholder)
			.expect("placeholder record");
		assert_eq!(mock_rec.vector.len(), 8);
		assert_eq!(placeholder_rec.vector.len(), 16);
		assert_eq!(placeholder_rec.model, "placeholder-v1");
	}

	#[tokio::test]
	async fn process_with_two_embedders_and_docstring_produces_four_records() {
		let pipeline = Pipeline::new(
			vec![
				Box::new(MockEmbedder::new(8)),
				Box::new(PlaceholderEmbedder::new("placeholder-v1", 16)),
			],
			PipelineConfig::default(),
		);
		let info = pipeline.process(make_input(Some("/// docs"))).await.unwrap();
		assert_eq!(info.embeddings.len(), 4);

		let docstring_count =
			info.embeddings.iter().filter(|r| matches!(r.purpose, EmbeddingPurpose::Docstring)).count();
		assert_eq!(docstring_count, 2);
	}

	#[tokio::test]
	async fn process_extracts_enclosing_function_as_snippet() {
		let input = PipelineInput {
			raw_code:      "fn bar() { let x = 1; }".into(),
			symbol_name:   "bar".into(),
			symbol_span:   ByteSpan { start: 3, end: 6 },
			symbol_origin: SymbolOrigin::Repo { repo_id: RepoId("r1".into()) },
			metadata:      ChunkMetadata {
				repo_id:             RepoId("r1".into()),
				file_path:           "src/lib.rs".into(),
				file_span:           ByteSpan { start: 0, end: 23 },
				parsed_at:           chrono::Utc::now(),
				lang:                Language::Rust,
				lang_version:        None,
				blob_schema_version: BLOB_SCHEMA_VERSION,
			},
			docstring:     None,
		};
		let pipeline = Pipeline::new(vec![Box::new(MockEmbedder::new(4))], PipelineConfig::default());
		let info = pipeline.process(input).await.unwrap();
		assert!(info.source.treesitter_repr.is_some());
		assert!(info.source.raw_code.contains("fn bar"));
	}

	#[tokio::test]
	async fn process_treesitter_repr_contains_snippet_sexp() {
		let input = PipelineInput {
			raw_code:      "fn greet() { println!(\"hello\"); }".into(),
			symbol_name:   "greet".into(),
			symbol_span:   ByteSpan { start: 3, end: 8 },
			symbol_origin: SymbolOrigin::Repo { repo_id: RepoId("r1".into()) },
			metadata:      ChunkMetadata {
				repo_id:             RepoId("r1".into()),
				file_path:           "src/main.rs".into(),
				file_span:           ByteSpan { start: 0, end: 33 },
				parsed_at:           chrono::Utc::now(),
				lang:                Language::Rust,
				lang_version:        None,
				blob_schema_version: BLOB_SCHEMA_VERSION,
			},
			docstring:     None,
		};
		let pipeline = Pipeline::new(vec![Box::new(MockEmbedder::new(4))], PipelineConfig::default());
		let info = pipeline.process(input).await.unwrap();
		let payload: serde_json::Value =
			serde_json::from_slice(&info.source.treesitter_repr.as_ref().unwrap().0).unwrap();
		// The sexp is for the extracted snippet (the function node), which is
		// parsed as its own source_file fragment.
		assert!(payload["sexp"].as_str().unwrap().contains("source_file"));
		// The sexp must also contain function_item since the snippet IS a function.
		assert!(payload["sexp"].as_str().unwrap().contains("function_item"));
	}
}
