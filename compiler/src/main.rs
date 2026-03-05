use ir::entry::Entry;
use lang_types::Language;
use semver::Version;
use serde_json::json;
use terminusdb::{Runner, termdb::{CrateInfo, DocCtx}};
use tokio::fs::write;

use crate::traits::{builder::get_registry, registry::Registry};

mod core;
mod error;
mod traits;

const TEST_PACKAGE: &str = "axum";
const VERSION: Version = Version::new(0, 8, 8);

#[tokio::main]
async fn main() {
	let registry = get_registry(Language::Rust);
	let packages = registry.get_packages_by_name(TEST_PACKAGE).await;

	let out = match packages {
		Ok(mut packages) => {
			let pkg = packages.remove(0); // Take ownership by removing from vec
			tokio::task::spawn_blocking(move || pkg.retrieve(VERSION, None)).await.unwrap()
		}
		Err(_other) => todo!(),
	}
	.unwrap();

	let entry_struct: Vec<Entry> = serde_json::from_str(&out).unwrap();

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

	runner.run(entry_struct);

	let store = runner.into_docs();
	// Vec of Values
	if let Ok(jsonld_out) = store.into_json_ld_insert() {
		// write the map { "<uri>": <Value>, ... }
		write("out.jsonld", jsonld_out).await;
	}

	write("out.json", out).await;
}
