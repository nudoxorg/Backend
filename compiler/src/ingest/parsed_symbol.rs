//! The parse-once projection.
//!
//! [`ParsedSymbol`] is the single neutral record derived from the IR
//! [`Index`](ir::entry::Index). Every non-graph sink (blobs, full-text, vectors,
//! the SQLite occurrence store) projects from a `Vec<ParsedSymbol>` rather than
//! re-parsing the Terminus JSON-LD `DocStore`. The canonical [`EntryUri`] and
//! deterministic [`GlobalSymbolId`] are computed **once**, here, and threaded
//! into every store so they all agree on a symbol's identity.

use std::{collections::HashMap, path::PathBuf};

use ir::entry::{Entry, Index};
use nudox_core::{BLOB_SCHEMA_VERSION, BlobInfo, ByteSpan, ChunkMetadata, GlobalSymbolId, Language, LibRef, OccurrenceId, RepoId, SourceChunk, SymbolKind, SymbolOrigin, TreesitterRepr};
use nudox_pipeline::treesitter::parse_and_extract;

use crate::{identity::{EntryUri, compute_symbol_id, path::{fq_name, nudox_path_to_str}}, terminusdb::embedding_service::{EmbeddingDocument, RecordKind, RepresentationKind, build_entry_embedding_text}};

/// Per-package coordinates shared by every symbol in one ingestion.
#[derive(Debug, Clone)]
pub struct PackageCoord {
	pub language: String,
	pub package:  String,
	pub version:  String,
}

impl PackageCoord {
	pub fn new(language: &str, package: &str, version: &str) -> Self {
		Self { language: language.to_owned(), package: package.to_owned(), version: version.to_owned() }
	}

	fn lib_ref(&self) -> LibRef { LibRef { name: self.package.clone(), version: self.version.clone() } }

	fn repo_id(&self) -> RepoId { RepoId(format!("lib:{}:{}", self.package, self.version)) }
}

/// One symbol, projected once from the IR and ready for every sink.
#[derive(Debug, Clone)]
pub struct ParsedSymbol {
	/// Canonical cross-store identity.
	pub entry_uri:       EntryUri,
	/// Deterministic global id, present when a Terminus instance is configured.
	pub global_id:       Option<GlobalSymbolId>,
	pub name:            String,
	pub fq_name:         String,
	pub kind:            SymbolKind,
	/// Lower-case kind label (e.g. `"function"`) for payloads / embedding text.
	pub kind_label:      String,
	pub aliases:         Vec<String>,
	pub documentation:   Option<String>,
	/// Text fed to the embedding provider and the full-text index.
	pub embedding_text:  String,
	/// Source for blob storage / tree-sitter (best-effort; empty when no source).
	pub raw_code:        String,
	pub treesitter_repr: TreesitterRepr,
	pub symbol_span:     ByteSpan,
}

/// Project every entry in `index` into a [`ParsedSymbol`], computing the
/// canonical URI and deterministic id exactly once per symbol.
///
/// `source_map` maps fully-qualified name → raw source (Rust only; empty
/// otherwise). `terminus_instance` is `"{org}/{db}"` when Terminus is
/// configured — its presence is what makes ids deterministic.
pub fn project(
	coord: &PackageCoord,
	index: &Index,
	source_map: &HashMap<String, String>,
	terminus_instance: Option<&str>,
) -> Vec<ParsedSymbol> {
	index
		.entries_by_path
		.values()
		.map(|entry| project_entry(coord, entry, source_map, terminus_instance))
		.collect()
}

fn project_entry(
	coord: &PackageCoord,
	entry: &Entry,
	source_map: &HashMap<String, String>,
	terminus_instance: Option<&str>,
) -> ParsedSymbol {
	let path = entry.path();
	let entry_uri = EntryUri::new(&coord.language, &coord.package, &nudox_path_to_str(path));
	let global_id = terminus_instance.map(|instance| compute_symbol_id(instance, &entry_uri));

	let fq = fq_name(path);
	let name = entry.name().to_owned();
	let kind = symbol_kind_of(entry);
	let kind_label = entry.kind_tag().to_owned();
	let aliases: Vec<String> = entry
		.aliases()
		.map(|set| set.iter().map(|segments| segments.join("::")).collect())
		.unwrap_or_default();
	let documentation = entry.documentation().map(str::to_owned);

	let embedding_text = build_entry_embedding_text(
		&name,
		&fq,
		Some(&kind_label),
		&aliases,
		documentation.as_deref(),
	);

	let (raw_code, treesitter_repr, symbol_span) = extract_source(&fq, &embedding_text, source_map);

	ParsedSymbol {
		entry_uri,
		global_id,
		name,
		fq_name: fq,
		kind,
		kind_label,
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
) -> (String, TreesitterRepr, ByteSpan) {
	match source_map.get(fq) {
		Some(raw) => {
			let span = ByteSpan { start: 0, end: raw.len() };
			let (snippet, snippet_span, ts_repr) = parse_and_extract(raw, Language::Rust, span, 80);
			let adjusted =
				ByteSpan { start: 0, end: snippet.len().saturating_sub(snippet_span.start) };
			(snippet, ts_repr, adjusted)
		}
		None => {
			let len = embedding_text.len();
			(embedding_text.to_owned(), TreesitterRepr(vec![]), ByteSpan { start: 0, end: len })
		}
	}
}

impl ParsedSymbol {
	/// Project to a [`BlobInfo`] for `Orchestrator::ingest`.
	///
	/// When a deterministic [`GlobalSymbolId`] is available the symbol is an
	/// `ExternalLib` carrying that id (the orchestrator honors it); otherwise it
	/// falls back to a `Repo` origin so the no-Terminus `/symbol-search` path
	/// still resolves and indexes immediately.
	pub fn to_blob_info(&self, coord: &PackageCoord) -> BlobInfo {
		let (symbol_origin, resolved_global_id) = match self.global_id {
			Some(global_id) => (SymbolOrigin::ExternalLib { lib: coord.lib_ref() }, Some(global_id)),
			None => (SymbolOrigin::Repo { repo_id: coord.repo_id() }, None),
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
				lang:                Language::Rust,
				lang_version:        None,
				blob_schema_version: BLOB_SCHEMA_VERSION,
			},
		}
	}

	/// Project to an [`EmbeddingDocument`] for the vector pipeline. The record
	/// key is the canonical entry URI, tying the eventual Qdrant point to the
	/// same identity spine as every other store.
	pub fn to_embedding_document(&self, coord: &PackageCoord) -> EmbeddingDocument {
		let uri = self.entry_uri.to_string();
		EmbeddingDocument {
			record_key: uri.clone(),
			uri,
			text: self.embedding_text.clone(),
			fq_name: Some(self.fq_name.clone()),
			language: coord.language.clone(),
			package: coord.package.clone(),
			version: Some(coord.version.clone()),
			symbol_kind: Some(self.kind_label.clone()),
			record_kind: RecordKind::SemanticNode,
			representation_kind: RepresentationKind::Docs,
			chunk_index: None,
			chunk_count: None,
		}
	}
}
