use std::fs;

use color_eyre::eyre::{self, WrapErr};
use lang_types::Language;
use nudox::traits::{builder::get_registry, package::Package, registry::Registry};
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

	// // Add some identification to the instance that the compiler is running on
	// // USE ENV Variables
	// let config = TerminusConfig {
	// 	endpoint: Url::parse("http://54.159.188.191:6363").unwrap(),
	// 	user: "admin".into(),
	// 	password: "root".into(),
	// 	org: "nudox".into(),
	// 	db: "main".into(),
	// };

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

	dbg!(&index);

	let context_object = json!({
		"@type": "@context",
		"@schema": "terminusdb:///schema#",
		"@base": "terminusdb:///data/",
		"xsd": "http://www.w3.org/2001/XMLSchema#",
		"sys": "http://terminusdb.com/schema/sys#"
	});

	// let mut runner = Runner::new(DocCtx::init(
	// 	CrateInfo::new("rust", TEST_PACKAGE, VERSION.to_string()),
	// 	context_object,
	// ));

	// // Feed entries from the index into the runner
	// runner.run(index.entries_by_path.into_values());

	// let store = runner.into_docs();
	// info!(documents = store.docs.len(), "emission complete");

	// // Upload schema first, then instance documents
	// // TODO: Switch to the LinkML outputted schema
	// let schema_json: Vec<serde_json::Value> =
	// 	serde_json::from_str(include_str!("../schema.json")).unwrap();

	// fs::write("out.json", serde_json::to_string(&store).unwrap());

	// // The schema should NOT FAIL .. if it does somehting is inherently wrong,
	// and // we should just stop the server/throw a Major Error Alert Also only
	// needs to // be uploaded at schema changes/new DB creations
	// upload_schema(&config, schema_json)
	// 	.await
	// 	.map_err(|e| eyre::eyre!(e))
	// 	.wrap_err("schema upload failed")?;

	// upload_documents(&config, store)
	// 	.await
	// 	.map_err(|e| eyre::eyre!(e))
	// 	.wrap_err("document upload failed")?;

	Ok(())
}
