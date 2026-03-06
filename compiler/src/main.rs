use lang_types::Language;
use semver::Version;
use serde_json::json;
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
	let config = TerminusConfig {
		endpoint: Url::parse("http://54.159.188.191:6363").unwrap(),
		user:     "onyx".into(),
		password: "B0tbN1ght^".into(),
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
	let index = ir.index().into_index();

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

	// Upload schema first, then instance documents
	let schema_json: Vec<serde_json::Value> =
		serde_json::from_str(include_str!("../schema.json")).unwrap();
	if let Err(e) = upload_schema(&config, schema_json).await {
		eprintln!("Schema upload failed: {e}");
	}

	if let Err(e) = upload_documents(&config, store).await {
		eprintln!("Document upload failed: {e}");
	}
}
