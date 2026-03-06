use serde_json::Value;
use terminusdb_client::{BranchSpec, DocumentInsertArgs, TerminusDBHttpClient};
use url::Url;

use super::termdb::DocStore;

/// Configuration for connecting to a TerminusDB instance.
pub struct TerminusConfig {
	pub endpoint: Url,
	pub user:     String,
	pub password: String,
	pub org:      String,
	pub db:       String,
}

/// Upload all documents from a `DocStore` to a TerminusDB instance.
///
/// This inserts the raw JSON-LD values directly using PUT with `create=true`
/// (upsert semantics), matching the existing JSON-LD format produced by the
/// `Runner`.
pub async fn upload_documents(config: &TerminusConfig, store: DocStore) -> anyhow::Result<()> {
	let client = TerminusDBHttpClient::new_with_database(
		config.endpoint.clone(),
		&config.user,
		&config.password,
		&config.db,
		&config.org,
	)
	.await?;

	let documents = store.into_documents();
	if documents.is_empty() {
		eprintln!("No documents to upload");
		return Ok(());
	}

	eprintln!("Uploading {} documents to {}/{}", documents.len(), config.org, config.db);

	let spec = BranchSpec::new(&config.db);
	let args = DocumentInsertArgs {
		spec,
		author: "nudox-compiler".to_string(),
		message: "automated upload from compiler pipeline".to_string(),
		skip_existence_check: true,
		..Default::default()
	};

	let doc_refs: Vec<&Value> = documents.iter().collect();
	let result = client.insert_documents(doc_refs, args).await?;

	if let Some(commit_id) = result.extract_commit_id() {
		eprintln!("Upload committed: {commit_id}");
	}

	let mut inserted = 0usize;
	let mut updated = 0usize;
	for (_id, res) in result.iter() {
		match res {
			terminusdb_client::TDBInsertInstanceResult::Inserted(_) => inserted += 1,
			terminusdb_client::TDBInsertInstanceResult::AlreadyExists(_) => updated += 1,
		}
	}
	eprintln!("Result: {inserted} inserted, {updated} updated");

	Ok(())
}

/// Upload schema documents to the TerminusDB instance schema graph.
///
/// The schema JSON is expected to be the array of class/context definitions
/// matching the TerminusDB schema format (e.g. from `schema.json`).
pub async fn upload_schema(config: &TerminusConfig, schema_docs: Vec<Value>) -> anyhow::Result<()> {
	let client = TerminusDBHttpClient::new_with_database(
		config.endpoint.clone(),
		&config.user,
		&config.password,
		&config.db,
		&config.org,
	)
	.await?;

	if schema_docs.is_empty() {
		eprintln!("No schema documents to upload");
		return Ok(());
	}

	eprintln!("Uploading {} schema documents to {}/{}", schema_docs.len(), config.org, config.db);

	let spec = BranchSpec::new(&config.db);
	let args = DocumentInsertArgs {
		spec,
		author: "nudox-compiler".to_string(),
		message: "automated schema upload from compiler pipeline".to_string(),
		skip_existence_check: true,
		..Default::default()
	}
	.as_schema();

	let doc_refs: Vec<&Value> = schema_docs.iter().collect();
	let result = client.insert_documents(doc_refs, args).await?;

	if let Some(commit_id) = result.extract_commit_id() {
		eprintln!("Schema upload committed: {commit_id}");
	}

	Ok(())
}
