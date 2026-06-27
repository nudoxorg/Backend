//! The parse-once projection.
//!
//! [`ParsedSymbol`] is the single neutral record derived from the IR
//! [`Index`](ir::entry::Index). Every non-graph sink (blobs, full-text, vectors,
//! the SQLite occurrence store) projects from a `Vec<ParsedSymbol>` rather than
//! re-parsing the Terminus JSON-LD `DocStore`. The canonical [`EntryUri`] and
//! deterministic [`GlobalSymbolId`] are computed **once**, here, and threaded
//! into every store so they all agree on a symbol's identity.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use ir::entry::{Entry, Index};
use nudox_core::{BLOB_SCHEMA_VERSION, BlobInfo, ByteSpan, ChunkMetadata, GlobalSymbolId, Language, LibRef, OccurrenceId, RepoId, SourceChunk, SymbolKind, SymbolOrigin, TreesitterRepr};
use nudox_pipeline::treesitter::parse_and_extract;

use crate::{identity::{EntryUri, compute_symbol_id, path::{fq_name, nudox_path_to_str}}, terminusdb::embedding_service::{EmbeddingDocument, RecordKind, RepresentationKind, build_entry_embedding_text}};

/// Per-package coordinates shared by every symbol in one ingestion.
#[derive(Debug, Clone)]
pub struct PackageCoord {
	pub language: Language,
	pub package:  Arc<str>,
	pub version:  Arc<str>,
}

impl PackageCoord {
	pub fn new(language: Language, package: &str, version: &str) -> Self {
		Self { language, package: Arc::from(package), version: Arc::from(version) }
	}

	fn lib_ref(&self) -> LibRef { LibRef { name: self.package.to_string(), version: self.version.to_string() } }

	fn repo_id(&self) -> RepoId { RepoId(format!("lib:{}:{}", self.package, self.version)) }
}

/// Batch-level identity witness: known once before projection, shared by all symbols.
///
/// `Deterministic` means a Terminus instance is configured and every symbol
/// will receive a stable, content-addressed [`GlobalSymbolId`]. `Local` means
/// no global spine is present — symbols fall back to `Repo` origin so the
/// no-Terminus `/symbol-search` path still works.
///
/// Carrying this as an enum (rather than `Option<&str>`) makes the invariant
/// that *either all symbols or no symbols have a global id* unrepresentable to
/// violate: there is no per-symbol `Option` that could disagree with the batch.
pub enum Identity<'a> {
	/// Terminus is configured; `instance` is `"{org}/{db}"`.
	Deterministic { instance: &'a str },
	/// No global spine; symbols fall back to `Repo` origin.
	Local,
}

/// One symbol, projected once from the IR and ready for every sink.
#[derive(Debug, Clone)]
pub struct ParsedSymbol {
	/// Canonical cross-store identity.
	pub entry_uri:       EntryUri,
	pub name:            String,
	pub fq_name:         String,
	pub kind:            SymbolKind,
	pub aliases:         Vec<String>,
	pub documentation:   Option<String>,
	/// Text fed to the embedding provider and the full-text index.
	pub embedding_text:  String,
	/// Source for blob storage / tree-sitter (best-effort; empty when no source).
	pub raw_code:        String,
	pub treesitter_repr: Option<TreesitterRepr>,
	pub symbol_span:     ByteSpan,
}

/// Project every entry in `index` into a [`ParsedSymbol`], computing the
/// canonical URI exactly once per symbol.
///
/// `source_map` maps fully-qualified name → raw source (Rust only; empty
/// otherwise). `identity` encodes whether a Terminus instance is configured —
/// it is a batch-level fact decided before projection starts.
pub fn project<'id>(
	coord: &PackageCoord,
	index: &Index,
	source_map: &HashMap<String, String>,
	identity: &'id Identity<'id>,
) -> Vec<ParsedSymbol> {
	index
		.entries_by_path
		.values()
		.map(|entry| project_entry(coord, entry, source_map, identity))
		.collect()
}

fn project_entry<'id>(
	coord: &PackageCoord,
	entry: &Entry,
	source_map: &HashMap<String, String>,
	_identity: &'id Identity<'id>,
) -> ParsedSymbol {
	let path = entry.path();
	let entry_uri = EntryUri::new(coord.language.as_str(), &coord.package, &nudox_path_to_str(path));

	let fq = fq_name(path);
	let name = entry.name().to_owned();
	let kind = symbol_kind_of(entry);
	let aliases: Vec<String> = entry
		.aliases()
		.map(|set| set.iter().map(|segments| segments.join("::")).collect())
		.unwrap_or_default();
	let documentation = entry.documentation().map(str::to_owned);

	let embedding_text = build_entry_embedding_text(
		&name,
		&fq,
		Some(kind.label()),
		&aliases,
		documentation.as_deref(),
	);

	let SourceChunk { raw_code, treesitter_repr, symbol_span } =
		extract_source(&fq, &embedding_text, source_map);

	ParsedSymbol {
		entry_uri,
		name,
		fq_name: fq,
		kind,
		aliases,
		documentation,
		embedding_text,
		raw_code,
		treesitter_repr,
		symbol_span,
	}
}

/// Map an IR entry to the flat [`SymbolKind`] taxonomy — directly, with no
/// string round-trip through the JSON-LD `kind` field.
fn symbol_kind_of(entry: &Entry) -> SymbolKind {
	match entry {
		Entry::Function(_) => SymbolKind::Function,
		Entry::RecordType(_) => SymbolKind::Struct,
		Entry::SumType(_) => SymbolKind::Enum,
		Entry::TraitDef(_) | Entry::TraitImpl(_) => SymbolKind::Trait,
		Entry::TypeAlias(_) => SymbolKind::TypeAlias,
		Entry::Constant(_) | Entry::Variable(_) => SymbolKind::Const,
		_ => SymbolKind::Other,
	}
}

/// Best-effort source extraction. When the rustdoc source map has the symbol's
/// raw code, run tree-sitter; otherwise fall back to the embedding text so the
/// blob still carries something searchable.
fn extract_source(
	fq: &str,
	embedding_text: &str,
	source_map: &HashMap<String, String>,
) -> SourceChunk {
	match source_map.get(fq) {
		Some(raw) => {
			let span = ByteSpan { start: 0, end: raw.len() };
			let (snippet, snippet_span, treesitter_repr) =
				parse_and_extract(raw, Language::Rust, span, 80);
			let symbol_span =
				ByteSpan { start: 0, end: snippet.len().saturating_sub(snippet_span.start) };
			SourceChunk { raw_code: snippet, treesitter_repr, symbol_span }
		}
		None => {
			let len = embedding_text.len();
			SourceChunk {
				raw_code:        embedding_text.to_owned(),
				treesitter_repr: None,
				symbol_span:     ByteSpan { start: 0, end: len },
			}
		}
	}
}

impl ParsedSymbol {
	/// Project to a [`BlobInfo`] for `Orchestrator::ingest`.
	///
	/// When `identity` is `Deterministic` the symbol is an `ExternalLib` carrying
	/// a content-addressed [`GlobalSymbolId`] (the orchestrator honors it);
	/// `Local` falls back to a `Repo` origin so the no-Terminus `/symbol-search`
	/// path still resolves and indexes immediately.
	pub fn to_blob_info(&self, coord: &PackageCoord, identity: &Identity<'_>) -> BlobInfo {
		let (symbol_origin, resolved_global_id): (SymbolOrigin, Option<GlobalSymbolId>) =
			match identity {
				Identity::Deterministic { instance } => (
					SymbolOrigin::ExternalLib { lib: coord.lib_ref() },
					Some(compute_symbol_id(instance, &self.entry_uri)),
				),
				Identity::Local => (SymbolOrigin::Repo { repo_id: coord.repo_id() }, None),
			};

		BlobInfo {
			occurrence_id: OccurrenceId(uuid::Uuid::new_v4()),
			symbol_name: self.fq_name.clone(),
			symbol_origin,
			resolved_global_id,
			kind: Some(self.kind),
			source: SourceChunk {
				raw_code:        self.raw_code.clone(),
				treesitter_repr: self.treesitter_repr.clone(),
				symbol_span:     self.symbol_span,
			},
			embeddings: vec![],
			metadata: ChunkMetadata {
				repo_id:             coord.repo_id(),
				file_path:           PathBuf::from(self.entry_uri.to_string()),
				file_span:           ByteSpan { start: 0, end: 0 },
				parsed_at:           chrono::Utc::now(),
				lang:                coord.language,
				lang_version:        None,
				blob_schema_version: BLOB_SCHEMA_VERSION,
			},
		}
	}

	/// Project to an [`EmbeddingDocument`] for the vector pipeline, consuming
	/// the symbol. The record key is the canonical entry URI, tying the eventual
	/// Qdrant point to the same identity spine as every other store.
	///
	/// Takes `self` so `embedding_text` and `fq_name` are moved rather than
	/// cloned — callers in the vector pipeline process each symbol exactly once.
	pub fn into_embedding_document(self, coord: &PackageCoord) -> EmbeddingDocument {
		let uri = self.entry_uri.to_string();
		let record_key = uri.clone();
		EmbeddingDocument {
			record_key,
			uri,
			text: self.embedding_text,
			fq_name: Some(self.fq_name),
			language: coord.language.as_str().to_owned(),
			package: coord.package.to_string(),
			version: Some(coord.version.to_string()),
			symbol_kind: Some(self.kind.label().to_owned()),
			record_kind: RecordKind::SemanticNode,
			representation_kind: RepresentationKind::Docs,
			chunk_index: None,
			chunk_count: None,
		}
	}
}
