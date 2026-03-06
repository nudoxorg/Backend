use lang_types::Language;
use semver::Version;
use serde_json::json;
use tracing::{error, info, info_span};
use tracing_subscriber::EnvFilter;
use url::Url;

use crate::{terminusdb::{Runner, termdb::{CrateInfo, DocCtx}, upload::{TerminusConfig, upload_documents, upload_schema}}, traits::{builder::get_registry, package::Package, registry::Registry}};

mod core;
mod error;
pub(crate) mod git;
mod pipeline;
mod terminusdb;
mod traits;

const TEST_PACKAGE: &str = "axum";
const VERSION: Version = Version::new(0, 8, 8);

#[tokio::main]
async fn main() {
	tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::new("debug"))
    .with_target(true)  // temporarily enable to see the actual targets
    .compact()
    .init();

	let config = TerminusConfig {
		endpoint: Url::parse("http://54.159.188.191:6363").unwrap(),
		user:     "admin".into(),
		password: "root".into(),
		org:      "nudox".into(),
		db:       "main".into(),
	};

	let registry = get_registry(Language::Rust);
	let packages = registry.get_packages_by_name(TEST_PACKAGE).await;

	let ir = match packages {
		Ok(mut packages) => {
			let pkg = packages.remove(0);
			pkg.retrieve(VERSION, None)
		}
		Err(_other) => todo!(),
	}
	.unwrap();

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

	// Upload schema first, then instance documents
	let schema_json: serde_json::Value =
		serde_json::from_str(include_str!("../../schema.jsonld")).unwrap();

	if let Err(e) = upload_schema(&config, vec![schema_json]).await {
		error!(error = %e, "schema upload failed");
	}

	if let Err(e) = upload_documents(&config, store).await {
		error!(error = %e, "document upload failed");
	}
}
