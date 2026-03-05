use lang_types::Language;
use semver::Version;
use serde_json::json;
use terminusdb::{Runner, termdb::{CrateInfo, DocCtx}};
use tokio::fs::write;

use crate::traits::{builder::get_registry, package::Package, registry::Registry};

mod core;
mod error;
mod traits;

const TEST_PACKAGE: &str = "axum";
const VERSION: Version = Version::new(0, 8, 8);

#[tokio::main]
async fn main() {
	let registry = get_registry(Language::Rust);
	let packages = registry.get_packages_by_name(TEST_PACKAGE).await;

	let entries = match packages {
		Ok(mut packages) => {
			let pkg = packages.remove(0);
			pkg.retrieve(VERSION, None)
		}
		Err(_other) => todo!(),
	}
	.unwrap();

	let json_out = serde_json::to_string(&entries).unwrap();

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

	runner.run(entries);

	let store = runner.into_docs();
	if let Ok(jsonld_out) = store.into_json_ld_insert() {
		write("out.jsonld", jsonld_out).await;
	}

	write("out.json", json_out).await;
}
