use color_eyre::eyre::{self, WrapErr};
use lang_types::Language;
use nudox::{terminusdb::{Runner, embedding_service::{EmbeddingService, OpenAIEmbeddingProvider, PointIdFactory, QdrantPointFactory, embedding_documents_from_docstore}, qdrant_upload::{QdrantConfig, upload_points}, termdb::{CrateInfo, DocCtx}, upload::{TerminusConfig, upload_documents, upload_schema}}, traits::{builder::get_registry, package::Package, registry::Registry}};
use qdrant_client::qdrant::Distance;
use semver::Version;
use serde_json::json;
use tracing::{info, info_span};
use tracing_subscriber::EnvFilter;
use url::Url;

const TEST_PACKAGE: &str = "axum";
const VERSION: Version = Version::new(0, 8, 8);

#[tokio::main]
async fn main() -> eyre::Result<()> {
	color_eyre::install()?;

	tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::new("debug"))
    .with_target(true)  // temporarily enable to see the actual targets
    .compact()
    .init();

	// Add some identification to the instance that the compiler is running on
	// USE ENV Variables
	let config = TerminusConfig {
		endpoint: Url::parse("http://54.159.188.191:6363").unwrap(),
		user:     "admin".into(),
		password: "root".into(),
		org:      "admin".into(),
		db:       "main".into(),
	};

	let qdrant_config = QdrantConfig {
		endpoint:        Url::parse("http://localhost:6334").unwrap(),
		collection_name: format!("rust_{}_{}", TEST_PACKAGE, VERSION),
		vector_size:     1536,
		distance:        Distance::Cosine,
	};

	let registry = get_registry(Language::Rust);
	let mut packages = registry
		.get_packages_by_name(TEST_PACKAGE)
		.await
		.wrap_err_with(|| format!("registry lookup failed for `{TEST_PACKAGE}`"))?;

	let pkg = packages.remove(0);
	let ir = pkg
		.retrieve(VERSION, None)
		.wrap_err_with(|| format!("IR retrieval failed for `{TEST_PACKAGE}`"))?;

	// Collected → Indexed
	let index = info_span!("indexing", package = TEST_PACKAGE).in_scope(|| ir.index().into_index());

	let context_object = json!({
		"@type": "@context",
		"@schema": "terminusdb:///schema#",
		"@base": "terminusdb:///data/",
		"xsd": "http://www.w3.org/2001/XMLSchema#",
		"sys": "http://terminusdb.com/schema/sys#"
	});

	let mut runner = Runner::new(DocCtx::init(
		CrateInfo::new("rust", TEST_PACKAGE, VERSION.to_string()),
		context_object,
	));

	// Feed entries from the index into the runner
	runner.run(index.entries_by_id.into_values());

	let store = runner.into_docs();
	info!(documents = store.docs.len(), "emission complete");

	let embedding_provider = OpenAIEmbeddingProvider::new("text-embedding-3-small");
	let embedding_service = EmbeddingService::new(embedding_provider);
	// Upload schema first, then instance documents
	// TODO: Switch to the LinkML outputted schema
	let schema_json: Vec<serde_json::Value> =
		serde_json::from_str(include_str!("../schema.json")).unwrap();

	// The schema should NOT FAIL .. if it does somehting is inherently wrong, and
	// we should just stop the server/throw a Major Error Alert Also only needs to
	// be uploaded at schema changes/new DB creations
	upload_schema(&config, schema_json)
		.await
		.map_err(|e| eyre::eyre!(e))
		.wrap_err("schema upload failed")?;

	upload_documents(&config, &store)
		.await
		.map_err(|e| eyre::eyre!(e))
		.wrap_err("document upload failed")?;

	let version_string = VERSION.to_string();

	let embedding_docs =
		embedding_documents_from_docstore(&store, "rust", TEST_PACKAGE, Some(version_string.as_str()))
			.map_err(|e| eyre::eyre!(e))
			.wrap_err("embedding document projection failed")?;
	info!(count = embedding_docs.len(), "embedding documents prepared");
	let embedded_records = embedding_service
		.embed_documents(embedding_docs)
		.await
		.map_err(|e| eyre::eyre!(e))
		.wrap_err("embedding generation failed")?;
	info!(count = embedded_records.len(), "embedding generation complete");
	let mut points = Vec::with_capacity(embedded_records.len());
	for (idx, record) in embedded_records.into_iter().enumerate() {
		let point_id = PointIdFactory::from_u64(idx as u64 + 1);
		let point = QdrantPointFactory::build_point(point_id, record)
			.map_err(|e| eyre::eyre!(e))
			.wrap_err("qdrant point construction failed")?;
		points.push(point);
	}
	upload_points(&qdrant_config, points)
		.await
		.map_err(|e| eyre::eyre!(e))
		.wrap_err("qdrant upload failed")?;
	Ok(())
}
