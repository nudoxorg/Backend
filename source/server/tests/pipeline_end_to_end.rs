use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}, process::Command};

use color_eyre::eyre::WrapErr;
use nudox::{nudox_core::{rust::RustPackage, ts::TsPackage}, git::{clone_repository, find_commit_for_version, materialize_commit}, emit::Runner, terminus::{schema::{CrateInfo, DocCtx, DocStore}, upload::{TerminusConfig, upload_schema}}};
use rustdoc_types::{Crate as RustdocCrate, ItemEnum};
use semver::Version;
use tempfile::TempDir;
use terminusdb_client::{BranchSpec, DocumentInsertArgs, TerminusDBHttpClient};
use url::Url;

#[test]
fn rust_regular_pipeline_end_to_end() -> color_eyre::Result<()> {
	let repository = git_fixture("rust/regular")?;
	let package = RustPackage {
		slug:        "calculator".into(),
		name:        "calculator".into(),
		language:    lang_types::Language::Rust,
		uuid:        1,
		source:      file_url(repository.path())?,
		direct_repo: true,
		description: Some("regular rust fixture".into()),
	};

	let ir = package.retrieve(Version::parse("0.1.0")?, None)?;
	let store = emit_store("rust", "calculator", Version::parse("0.1.0")?, ir)?;
	let names = doc_names(&store);
	let counter_entry = entry_with_path_suffix(&store, "calculator::Counter")
		.expect("expected Counter entry in emitted Rust docs");
	let counter_kind =
		kind_for_entry(&store, counter_entry).expect("expected Counter kind in emitted docs");
	let implemented_protocols = counter_entry
		.get("implemented_protocols")
		.and_then(|value| value.as_array())
		.expect("expected Counter entry to expose implemented_protocols");
	let methods = counter_kind
		.get("methods")
		.and_then(|value| value.as_array())
		.expect("expected Counter kind to inline impl methods");

	assert!(names.contains("add"));
	assert!(names.contains("Counter"));
	assert!(names.contains("HiddenCounter"));
	assert!(
		implemented_protocols
			.iter()
			.any(|value| { value.as_str().is_some_and(|uri| uri.ends_with("calculator::BlanketView")) })
	);
	assert!(methods.len() > 3);
	assert!(!has_entry_path_suffix(&store, "calculator::Counter::View"));
	assert!(!has_entry_path_suffix(&store, "calculator::Counter::view"));
	assert!(has_entry_path_suffix(&store, "calculator::internals"));
	assert!(has_entry_path_suffix(&store, "calculator::internals::HiddenCounter"));
	assert!(store.docs.contains_key("RecordType/rust/calculator/calculator::Counter"));
	assert!(store.docs.len() >= 4);

	Ok(())
}

#[test]
fn rust_workspace_pipeline_end_to_end() -> color_eyre::Result<()> {
	let repository = git_fixture("rust/workspace")?;
	let package = RustPackage {
		slug:        "odd-duck".into(),
		name:        "odd-duck".into(),
		language:    lang_types::Language::Rust,
		uuid:        2,
		source:      file_url(repository.path())?,
		direct_repo: true,
		description: Some("workspace rust fixture".into()),
	};

	let ir = package.retrieve(Version::parse("0.3.1")?, None)?;
	let store = emit_store("rust", "odd-duck", Version::parse("0.3.1")?, ir)?;
	let names = doc_names(&store);

	assert!(names.contains("Widget"));
	assert!(names.contains("Mode"));
	assert!(names.contains("Behavior"));
	assert!(store.docs.len() >= 6);

	Ok(())
}

#[test]
fn rust_binary_workspace_pipeline_includes_local_library_deps() -> color_eyre::Result<()> {
	let repository = git_fixture("rust/binary_workspace")?;
	let package = RustPackage {
		slug:        "app".into(),
		name:        "app".into(),
		language:    lang_types::Language::Rust,
		uuid:        8,
		source:      file_url(repository.path())?,
		direct_repo: true,
		description: Some("binary workspace rust fixture".into()),
	};

	let ir = package.retrieve(Version::parse("0.1.0")?, None)?;
	let store = emit_store("rust", "app", Version::parse("0.1.0")?, ir)?;
	let names = doc_names(&store);

	assert!(names.contains("app"));
	assert!(names.contains("CoreCounter"));
	assert!(has_entry_path_suffix(&store, "corelib::CoreCounter"));
	assert!(store.docs.len() >= 3);

	Ok(())
}

#[test]
#[ignore = "local repro for axum parser failures"]
fn rust_axum_pipeline_repro() -> color_eyre::Result<()> {
	let package = RustPackage {
		slug:        "axum".into(),
		name:        "axum".into(),
		language:    lang_types::Language::Rust,
		uuid:        5,
		source:      Url::parse("https://github.com/tokio-rs/axum")?,
		direct_repo: true,
		description: Some("live axum repro".into()),
	};

	match package.retrieve(Version::parse("0.8.8")?, None) {
		Ok(ir) => {
			let store = emit_store("rust", "axum", Version::parse("0.8.8")?, ir)?;
			eprintln!("axum emitted document count: {}", store.docs.len());
			assert!(!store.docs.is_empty(), "expected axum docs to be emitted");
			assert!(store.docs.keys().all(|uri| !uri.starts_with("trait_def/")));
			assert!(store.docs.keys().all(|uri| !uri.starts_with("record/")));
			assert!(store.docs.keys().any(|uri| uri.starts_with("TraitDef/")));
			Ok(())
		}
		Err(error) => panic!("axum retrieve failed: {error:?}"),
	}
}

#[test]
#[ignore = "manual diagnostic for axum entry cardinality"]
fn rust_axum_089_entry_diagnostic() -> color_eyre::Result<()> {
	let package = RustPackage {
		slug:        "axum".into(),
		name:        "axum".into(),
		language:    lang_types::Language::Rust,
		uuid:        7,
		source:      Url::parse("https://github.com/tokio-rs/axum")?,
		direct_repo: true,
		description: Some("live axum 0.8.9 diagnostic".into()),
	};
	let version = Version::parse("0.8.9")?;
	let scratch = tempfile::tempdir()?;
	let repository_dir = scratch.path().join("repository");
	let workspace_dir = scratch.path().join("workspace");
	let repository = clone_repository(&repository_dir, &package.source)?;
	let target_oid = find_commit_for_version(&repository, &version, &package.name, None)
		.ok_or_else(|| color_eyre::eyre::eyre!("failed to find commit for axum 0.8.9"))?;
	materialize_commit(&repository, target_oid, &workspace_dir)?;

	let status = Command::new("cargo")
		.arg("rustdoc")
		.arg("--package")
		.arg("axum")
		.arg("--")
		.arg("-Z")
		.arg("unstable-options")
		.arg("--output-format")
		.arg("json")
		.current_dir(&workspace_dir)
		.status()
		.wrap_err("failed to spawn cargo rustdoc for axum 0.8.9")?;

	if !status.success() {
		color_eyre::eyre::bail!("cargo rustdoc failed for axum 0.8.9 with status {status}");
	}

	let json_path = workspace_dir.join("target/doc/axum.json");
	let json_content = fs::read_to_string(&json_path)
		.wrap_err_with(|| format!("failed to read rustdoc JSON at {}", json_path.display()))?;
	let rustdoc_crate: RustdocCrate = serde_json::from_str(&json_content)?;

	let mut rustdoc_kind_counts = BTreeMap::new();
	for item in rustdoc_crate.index.values() {
		*rustdoc_kind_counts.entry(rustdoc_kind_name(&item.inner)).or_insert(0usize) += 1;
	}

	let ir = package.retrieve(version.clone(), None)?;
	let index = ir.index();
	let mut entry_kind_counts = BTreeMap::new();
	for entry in index.iter() {
		*entry_kind_counts.entry(entry.kind_tag()).or_insert(0usize) += 1;
	}

	eprintln!("axum 0.8.9 rustdoc item count: {}", rustdoc_crate.index.len());
	eprintln!(
		"axum 0.8.9 rustdoc item kinds:\n{}",
		serde_json::to_string_pretty(&rustdoc_kind_counts)?
	);
	eprintln!("axum 0.8.9 nudox entry count: {}", index.len());
	eprintln!("axum 0.8.9 nudox entry kinds:\n{}", serde_json::to_string_pretty(&entry_kind_counts)?);

	assert!(index.len() >= 150, "expected axum 0.8.9 to produce at least 150 entries");

	Ok(())
}

#[tokio::test]
#[ignore = "manual repro for axum terminus uploads"]
async fn rust_axum_terminus_upload_repro() -> color_eyre::Result<()> {
	let package = RustPackage {
		slug:        "axum".into(),
		name:        "axum".into(),
		language:    lang_types::Language::Rust,
		uuid:        6,
		source:      Url::parse("https://github.com/tokio-rs/axum")?,
		direct_repo: true,
		description: Some("live axum terminus repro".into()),
	};

	let ir = package.retrieve(Version::parse("0.8.8")?, None)?;
	let store = emit_store("rust", "axum", Version::parse("0.8.8")?, ir)?;
	if let Some(module_doc) = store.docs.get("Module/rust/axum/axum") {
		eprintln!(
			"module kind payload:\n{}",
			serde_json::to_string_pretty(module_doc).expect("module doc should serialize")
		);
	}
	let config = TerminusConfig {
		endpoint: Url::parse(
			&std::env::var("NUDOX_TERMINUS_ENDPOINT")
				.unwrap_or_else(|_| "http://127.0.0.1:63630".to_owned()),
		)?,
		user:     std::env::var("NUDOX_TERMINUS_USER").unwrap_or_else(|_| "admin".to_owned()),
		password: std::env::var("NUDOX_TERMINUS_PASSWORD").unwrap_or_else(|_| "root".to_owned()),
		org:      std::env::var("NUDOX_TERMINUS_ORG").unwrap_or_else(|_| "admin".to_owned()),
		db:       std::env::var("NUDOX_TERMINUS_DB").unwrap_or_else(|_| "main".to_owned()),
	};

	let schema_json: Vec<serde_json::Value> = serde_json::from_str(include_str!("../schema.json"))?;
	upload_schema(&config, schema_json)
		.await
		.map_err(|error| color_eyre::eyre::eyre!("failed to upload schema for axum repro: {error}"))?;

	let client = TerminusDBHttpClient::new_with_database(
		config.endpoint.clone(),
		&config.user,
		&config.password,
		&config.db,
		&config.org,
	)
	.await
	.map_err(|error| color_eyre::eyre::eyre!("failed to create terminus client: {error}"))?;
	let args = DocumentInsertArgs {
		spec: BranchSpec::new(&config.db),
		author: "nudox-compiler".to_string(),
		message: "manual axum repro".to_string(),
		skip_existence_check: true,
		..Default::default()
	};

	let mut docs: Vec<(&String, &serde_json::Value)> = store.docs.iter().collect();
	docs.sort_by(|(left_uri, left_doc), (right_uri, right_doc)| {
		let left_is_entry = left_doc.get("@type").and_then(|value| value.as_str()) == Some("Entry");
		let right_is_entry = right_doc.get("@type").and_then(|value| value.as_str()) == Some("Entry");
		let left_depth =
			left_doc.get("path").and_then(|value| value.as_array()).map_or(0, |segments| segments.len());
		let right_depth =
			right_doc.get("path").and_then(|value| value.as_array()).map_or(0, |segments| segments.len());
		left_is_entry
			.cmp(&right_is_entry)
			.then_with(|| right_depth.cmp(&left_depth))
			.then_with(|| left_uri.cmp(right_uri))
	});

	for (uri, doc) in docs {
		eprintln!("inserting uri: {uri}");
		if let Err(error) = client.insert_documents(vec![doc], args.clone()).await {
			eprintln!("failed uri: {uri}");
			eprintln!(
				"failed payload:\n{}",
				serde_json::to_string_pretty(doc).expect("payload should serialize")
			);
			panic!("terminus insert failed for {uri}: {error:?}");
		}
	}

	Ok(())
}

#[test]
fn typescript_regular_pipeline_end_to_end() -> color_eyre::Result<()> {
	let entry_point = fixture_root().join("ts/regular/greeter.ts");
	let package = TsPackage {
		slug:        "greeter".into(),
		name:        "greeter".into(),
		uuid:        3,
		source:      file_url(&entry_point)?,
		description: Some("regular ts fixture".into()),
		entry_point: entry_point.display().to_string(),
	};

	let ir = package.retrieve(Version::parse("1.0.0")?, None)?;
	let store = emit_store("typescript", "greeter", Version::parse("1.0.0")?, ir)?;
	let names = doc_names(&store);

	assert!(names.contains("greet"));
	assert!(names.contains("tag"));
	assert!(names.contains("Greeter"));
	assert!(names.contains("DEFAULT_GREETING"));
	assert!(names.contains("SECRET_GREETING"));
	assert!(names.contains("WhisperGreeter"));
	assert!(store.docs.len() >= 4);

	Ok(())
}

#[test]
fn typescript_unique_pipeline_end_to_end() -> color_eyre::Result<()> {
	let entry_point = fixture_root().join("ts/unique/toolkit.ts");
	let package = TsPackage {
		slug:        "toolkit".into(),
		name:        "toolkit".into(),
		uuid:        4,
		source:      file_url(&entry_point)?,
		description: Some("unique ts fixture".into()),
		entry_point: entry_point.display().to_string(),
	};

	let ir = package.retrieve(Version::parse("2.1.0")?, None)?;
	let store = emit_store("typescript", "toolkit", Version::parse("2.1.0")?, ir)?;
	let names = doc_names(&store);

	assert!(names.contains("toolkit"));
	assert!(names.contains("Builder"));
	assert!(names.contains("compose"));
	assert!(store.docs.len() >= 5);

	Ok(())
}

fn emit_store(
	language: &str,
	package_name: &str,
	_version: Version,
	ir: nudox::core::pipeline::Ir<nudox::core::pipeline::Collected>,
) -> color_eyre::Result<DocStore> {
	let context = serde_json::json!({
		"@type": "@context",
		"@schema": "terminusdb:///schema#",
		"@base": "terminusdb:///data/",
		"xsd": "http://www.w3.org/2001/XMLSchema#",
		"sys": "http://terminusdb.com/schema/sys#"
	});
	let mut runner = Runner::new(DocCtx::init(
		CrateInfo::new(language.to_owned(), package_name.to_owned()),
		context,
	));
	let index = ir.index().into_index();
	runner.run(index.entries_by_path.into_values());
	Ok(runner.into_docs())
}

fn doc_names(store: &DocStore) -> BTreeSet<String> {
	store
		.docs
		.values()
		.filter_map(|value| value.get("name").and_then(|name| name.as_str()))
		.map(str::to_owned)
		.collect()
}

fn has_entry_path_suffix(store: &DocStore, suffix: &str) -> bool {
	store.docs.keys().any(|uri| uri.starts_with("Entry/") && uri.ends_with(suffix))
}

fn entry_with_path_suffix<'a>(store: &'a DocStore, suffix: &str) -> Option<&'a serde_json::Value> {
	store
		.docs
		.iter()
		.find(|(uri, _)| uri.starts_with("Entry/") && uri.ends_with(suffix))
		.map(|(_, value)| value)
}

fn kind_for_entry<'a>(
	store: &'a DocStore,
	entry: &serde_json::Value,
) -> Option<&'a serde_json::Value> {
	entry.get("kind").and_then(|value| value.as_str()).and_then(|kind_uri| store.docs.get(kind_uri))
}

fn git_fixture(relative_fixture: &str) -> color_eyre::Result<TempDir> {
	let fixture = fixture_root().join(relative_fixture);
	let temp_dir = tempfile::tempdir()?;
	copy_tree(&fixture, temp_dir.path())?;

	run_git(temp_dir.path(), ["init", "-b", "main"])?;
	run_git(temp_dir.path(), ["add", "."])?;
	run_git(temp_dir.path(), [
		"-c",
		"user.name=Codex",
		"-c",
		"user.email=codex@example.com",
		"-c",
		"commit.gpgsign=false",
		"-c",
		"tag.gpgsign=false",
		"commit",
		"-m",
		"fixture",
	])?;

	Ok(temp_dir)
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> color_eyre::Result<()> {
	let status =
		Command::new("git").args(args).current_dir(cwd).status().wrap_err("failed to spawn git")?;

	if !status.success() {
		color_eyre::eyre::bail!("git {:?} failed with status {}", args, status);
	}

	Ok(())
}

fn copy_tree(src: &Path, dest: &Path) -> color_eyre::Result<()> {
	for entry in fs::read_dir(src)? {
		let entry = entry?;
		let source_path = entry.path();
		let destination_path = dest.join(entry.file_name());

		if entry.file_type()?.is_dir() {
			fs::create_dir_all(&destination_path)?;
			copy_tree(&source_path, &destination_path)?;
		} else {
			fs::copy(&source_path, &destination_path).wrap_err_with(|| {
				format!("failed to copy `{}` to `{}`", source_path.display(), destination_path.display())
			})?;
		}
	}

	Ok(())
}

fn fixture_root() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures") }

fn file_url(path: &Path) -> color_eyre::Result<Url> {
	Url::from_file_path(path)
		.map_err(|_| color_eyre::eyre::eyre!("failed to convert `{}` into a file URL", path.display()))
}

fn rustdoc_kind_name(inner: &ItemEnum) -> &'static str {
	match inner {
		ItemEnum::Module(_) => "module",
		ItemEnum::ExternCrate { .. } => "extern_crate",
		ItemEnum::Use(_) => "use",
		ItemEnum::Union(_) => "union",
		ItemEnum::Struct(_) => "struct",
		ItemEnum::StructField(_) => "struct_field",
		ItemEnum::Enum(_) => "enum",
		ItemEnum::Variant(_) => "variant",
		ItemEnum::Function(_) => "function",
		ItemEnum::Trait(_) => "trait",
		ItemEnum::TraitAlias(_) => "trait_alias",
		ItemEnum::Impl(_) => "impl",
		ItemEnum::TypeAlias(_) => "type_alias",
		ItemEnum::Constant { .. } => "constant",
		ItemEnum::Static(_) => "static",
		ItemEnum::Macro(_) => "macro",
		ItemEnum::ProcMacro(_) => "proc_macro",
		ItemEnum::Primitive(_) => "primitive",
		ItemEnum::AssocConst { .. } => "assoc_const",
		ItemEnum::AssocType { .. } => "assoc_type",
		ItemEnum::ExternType => "extern_type",
	}
}
