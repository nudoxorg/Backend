use std::{collections::BTreeSet, fs, path::{Path, PathBuf}, process::Command};

use color_eyre::eyre::WrapErr;
use nudox::{core::{rust::RustPackage, ts::TsPackage}, terminusdb::{Runner, termdb::{CrateInfo, DocCtx, DocStore}}, traits::package::Package};
use semver::Version;
use tempfile::TempDir;
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
	assert!(
		implemented_protocols
			.iter()
			.any(|value| { value.as_str().is_some_and(|uri| uri.ends_with("calculator::BlanketView")) })
	);
	assert!(methods.len() > 3);
	assert!(!has_entry_path_suffix(&store, "calculator::Counter::View"));
	assert!(!has_entry_path_suffix(&store, "calculator::Counter::view"));
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
	assert!(names.contains("Greeter"));
	assert!(names.contains("DEFAULT_GREETING"));
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
	version: Version,
	ir: nudox::pipeline::Ir<nudox::pipeline::Collected>,
) -> color_eyre::Result<DocStore> {
	let context = serde_json::json!({
		"@type": "@context",
		"@schema": "terminusdb:///schema#",
		"@base": "terminusdb:///data/",
		"xsd": "http://www.w3.org/2001/XMLSchema#",
		"sys": "http://terminusdb.com/schema/sys#"
	});
	let mut runner = Runner::new(DocCtx::init(
		CrateInfo::new(language.to_owned(), package_name.to_owned(), version.to_string()),
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
