use std::{path::PathBuf, sync::Arc};

use blobstore::ObjectStoreBlobStore;
use nudox_core::{BLOB_SCHEMA_VERSION, BlobInfo, BlobStore, ByteSpan, ChunkMetadata, GlobalSymbolId, Language, LibRef, RepoId, ResolutionOutcome, Result, SearchIndex, SearchQuery, SymbolOrigin, VectorIndex};
use embed::PlaceholderEmbedder;
use orchestrator::{Orchestrator, memory::{InMemoryFutureParseQueue, InMemoryGlobalSymbolStore}};
use pipeline::{Pipeline, PipelineConfig, PipelineInput};
use search::{InMemoryVectorIndex, QdrantVectorIndex, TantivySearchIndex};

struct EmbeddingModel {
	name: String,
	dim:  usize,
}

impl EmbeddingModel {
	fn from_env() -> Self {
		EmbeddingModel {
			name: std::env::var("NUDOX_EMBED_MODEL").unwrap_or_else(|_| "placeholder-v1".to_string()),
			dim:  std::env::var("NUDOX_EMBED_DIM").ok().and_then(|s| s.parse().ok()).unwrap_or(256),
		}
	}
}

struct Config {
	blob_store_root:   PathBuf,
	tantivy_dir:       PathBuf,
	qdrant_url:        Option<String>,
	qdrant_collection: String,
	model:             EmbeddingModel,
}

impl Config {
	fn from_env() -> Self {
		let blob_store_root = std::env::var("NUDOX_BLOB_STORE_ROOT")
			.map(PathBuf::from)
			.unwrap_or_else(|_| PathBuf::from("target/nudox-blobs"));
		let tantivy_dir = std::env::var("NUDOX_TANTIVY_DIR")
			.map(PathBuf::from)
			.unwrap_or_else(|_| PathBuf::from("target/nudox-tantivy"));
		let qdrant_url = std::env::var("NUDOX_QDRANT_URL").ok();
		let qdrant_collection =
			std::env::var("NUDOX_QDRANT_COLLECTION").unwrap_or_else(|_| "nudox-embeddings".to_string());
		Config { blob_store_root, tantivy_dir, qdrant_url, qdrant_collection, model: EmbeddingModel::from_env() }
	}
}

fn span_for(raw_code: &str, needle: &str) -> ByteSpan {
	let start = raw_code.find(needle).unwrap_or(0);
	ByteSpan { start, end: start + needle.len() }
}

fn metadata(repo_id: &RepoId, file_path: impl Into<PathBuf>, raw_code: &str) -> ChunkMetadata {
	ChunkMetadata {
		repo_id:             repo_id.clone(),
		file_path:           file_path.into(),
		file_span:           ByteSpan { start: 0, end: raw_code.len() },
		parsed_at:           chrono::Utc::now(),
		lang:                Language::Rust,
		lang_version:        None,
		blob_schema_version: BLOB_SCHEMA_VERSION,
	}
}

fn pipeline_input(
	repo_id: &RepoId,
	file_path: impl Into<PathBuf>,
	raw_code: &str,
	symbol_name: &str,
	symbol_origin: SymbolOrigin,
	docstring: Option<&str>,
) -> PipelineInput {
	PipelineInput {
		raw_code: raw_code.to_string(),
		symbol_name: symbol_name.to_string(),
		symbol_span: span_for(raw_code, symbol_name),
		symbol_origin,
		metadata: metadata(repo_id, file_path, raw_code),
		docstring: docstring.map(str::to_string),
	}
}

fn print_section(title: &str) {
	println!("\n=== {title} ===");
}

fn print_outcome(label: &str, outcome: &ResolutionOutcome) {
	match outcome {
		ResolutionOutcome::Resolved { global_id, blob_ref } => {
			println!("{label}: RESOLVED  blob_ref={blob_ref} global_id={global_id}");
		}
		ResolutionOutcome::Deferred { lib, blob_ref } => {
			println!("{label}: DEFERRED  blob_ref={blob_ref} waiting_on={} {}", lib.name, lib.version);
		}
	}
}

fn print_blob_summary(blob_ref: &nudox_core::BlobRef, info: &BlobInfo) {
	println!(
		"blob_ref={} symbol={} resolved_global_id={} embeddings={}",
		blob_ref,
		info.symbol_name,
		info.resolution.resolved_id().map(|id| id.to_string()).unwrap_or_else(|| "<none>".into()),
		info.embeddings.len()
	);
}

#[tokio::main]
async fn main() -> Result<()> {
	dotenvy::dotenv().ok();
	tracing_subscriber::fmt::init();

	let config = Config::from_env();

	print_section("Config");
	println!("blob_store_root: {}", config.blob_store_root.display());
	println!("tantivy_dir: {}", config.tantivy_dir.display());
	println!("qdrant_url: {}", config.qdrant_url.as_deref().unwrap_or("in-memory"));

	let repo_id = RepoId("demo-repo".into());
	let serde_lib = LibRef { name: "serde".into(), version: "1.0".into() };
	let tokio_lib = LibRef { name: "tokio".into(), version: "1.0".into() };

	let pipeline = Pipeline::new(
		vec![Box::new(PlaceholderEmbedder::new(&config.model.name, config.model.dim))],
		PipelineConfig::default(),
	);

	let blob_store = ObjectStoreBlobStore::local(config.blob_store_root.clone())?;
	let blob_store_handle = blob_store.clone();

	let tantivy = Arc::new(TantivySearchIndex::open_or_create(&config.tantivy_dir)?);
	let tantivy_query = Arc::clone(&tantivy);
	let search: Arc<dyn SearchIndex> = tantivy;

	let vector: Arc<dyn VectorIndex> = if let Some(ref url) = config.qdrant_url {
		let idx = QdrantVectorIndex::connect(url, config.qdrant_collection.clone()).await?;
		idx.ensure_collection(config.model.dim as u64).await?;
		Arc::new(idx)
	} else {
		Arc::new(InMemoryVectorIndex::new())
	};

	let global_store = InMemoryGlobalSymbolStore::new();
	let queue = InMemoryFutureParseQueue::new();

	let global_store_handle = global_store.clone();
	let queue_handle = queue.clone();

	let serde_global_id = GlobalSymbolId(uuid::Uuid::new_v4());
	global_store_handle.insert(&serde_lib, "Serialize", serde_global_id);

	let orchestrator = Orchestrator::new(
		Arc::new(global_store),
		Arc::new(blob_store),
		Arc::new(queue),
		Arc::clone(&search),
		Arc::clone(&vector),
	);

	let repo_local = pipeline
		.process(pipeline_input(
			&repo_id,
			"src/local.rs",
			"fn local_helper() -> usize { 42 }",
			"local_helper",
			SymbolOrigin::Repo { repo_id: repo_id.clone() },
			Some("Returns the answer for the local demo path."),
		))
		.await?;

	let serde_usage = pipeline
		.process(pipeline_input(
			&repo_id,
			"src/serde_example.rs",
			"use serde::Serialize;\n#[derive(Serialize)]\nstruct User { id: u64 }",
			"Serialize",
			SymbolOrigin::ExternalLib { lib: serde_lib.clone() },
			Some("Serialize the demo user model."),
		))
		.await?;

	let tokio_usage = pipeline
		.process(pipeline_input(
			&repo_id,
			"src/tokio_example.rs",
			"tokio::spawn(async move { println!(\"hello from demo\"); });",
			"spawn",
			SymbolOrigin::ExternalLib { lib: tokio_lib.clone() },
			Some("Spawn an async task in the demo."),
		))
		.await?;

	print_section("Pipeline outputs");
	println!(
		"repo-local embeddings={} symbol={}",
		repo_local.embeddings.len(),
		repo_local.symbol_name
	);
	println!(
		"serde usage embeddings={} symbol={}",
		serde_usage.embeddings.len(),
		serde_usage.symbol_name
	);
	println!(
		"tokio usage embeddings={} symbol={}",
		tokio_usage.embeddings.len(),
		tokio_usage.symbol_name
	);

	print_section("Initial ingest");
	let repo_outcome = orchestrator.ingest(repo_local).await?;
	let serde_outcome = orchestrator.ingest(serde_usage).await?;
	let tokio_outcome = orchestrator.ingest(tokio_usage).await?;

	print_outcome("repo-local", &repo_outcome);
	print_outcome("serde", &serde_outcome);
	print_outcome("tokio", &tokio_outcome);

	print_section("Deferred queue");
	let queued_before = queue_handle.peek_for_lib(&tokio_lib);
	println!("queued for tokio {}: {} blob(s)", tokio_lib.version, queued_before.len());
	for blob_ref in &queued_before {
		println!("queued blob_ref={}", blob_ref.0);
	}

	print_section("Back-fill resolve_lib(tokio)");
	let tokio_global_id = GlobalSymbolId(uuid::Uuid::new_v4());
	global_store_handle.insert(&tokio_lib, "spawn", tokio_global_id);
	let report = orchestrator.resolve_lib(&tokio_lib).await?;
	println!(
		"seen={} resolved={} skipped={}",
		report.blobs_seen, report.blobs_resolved, report.blobs_skipped
	);

	print_section("Blob files on disk");
	let mut blob_refs = blob_store_handle.list().await?;
	blob_refs.sort_by_key(|r| r.0.clone());
	for blob_ref in &blob_refs {
		let info = blob_store_handle.get(blob_ref).await?;
		print_blob_summary(blob_ref, &info);
	}

	print_section("Search query demo");
	let hits = tantivy_query.search("Serialize", 10).await?;
	if hits.is_empty() {
		println!("  (no results — index may not have committed yet)");
	}
	for hit in &hits {
		println!("  hit: symbol={} blob_ref={} score={:.3}", hit.symbol_name, hit.blob_ref, hit.score);
	}

	print_section("Where to look next");
	println!("blob JSON files: {}", config.blob_store_root.join("blobs").display());
	println!("configure via env vars or .env file (see .env.example)");

	Ok(())
}
