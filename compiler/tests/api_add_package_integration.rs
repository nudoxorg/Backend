use std::{fs, path::{Path, PathBuf}, process::Command, sync::Arc, time::Duration};

use color_eyre::eyre::WrapErr;
use lang_types::Language;
use nudox::{api::{self, AppState}, config::PipelineConfig, local_registry::LocalRegistry, search::SessionStore, storage::StorageLayout};
use reqwest::{Client, StatusCode};
use semver::Version;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{task::JoinHandle, time::Instant};
use url::Url;

#[tokio::test]
async fn add_typescript_packages_via_api_supports_registry_and_explicit_git_sources()
-> color_eyre::Result<()> {
	let server = TestServer::spawn().await?;

	let npm_package = server
		.add_package(json!({
			"language": Language::TypeScript,
			"name": "@types/node",
			"version": Version::parse("24.0.0")?,
		}))
		.await?;
	assert_eq!(npm_package["name"], "@types/node");
	assert_eq!(npm_package["version"], "24.0.0");
	assert_eq!(npm_package["state"]["health"], "healthy");
	assert!(npm_package["source"].as_str().unwrap_or_default().starts_with("https://"));
	assert_eq!(npm_package["state"]["tracked_version_commit"], "npm:@types/node@24.0.0");
	assert!(npm_package["state"]["entry_count"].as_u64().unwrap_or_default() > 0);
	assert!(npm_package["state"]["document_count"].as_u64().unwrap_or_default() > 0);
	assert!(npm_package["state"]["last_error"].is_null());

	let path_repo = git_typescript_fixture("git-toolkit", "1.2.3")?;
	let path_package = server
		.add_package(json!({
			"language": Language::TypeScript,
			"name": "git-toolkit",
			"version": Version::parse("1.2.3")?,
			"source": path_repo.path().display().to_string(),
		}))
		.await?;
	assert_eq!(path_package["name"], "git-toolkit");
	assert_eq!(path_package["version"], "1.2.3");
	assert_eq!(path_package["source"], canonical_directory_url(path_repo.path())?.as_str());
	assert_eq!(path_package["state"]["health"], "healthy");
	assert!(path_package["state"]["tracked_version_commit"].as_str().unwrap_or_default().len() >= 7);
	assert_eq!(
		path_package["state"]["tracked_version_commit"],
		path_package["state"]["latest_remote_commit"]
	);
	assert!(path_package["state"]["entry_count"].as_u64().unwrap_or_default() > 0);
	assert!(path_package["state"]["document_count"].as_u64().unwrap_or_default() > 0);
	assert!(path_package["state"]["last_error"].is_null());

	let file_url_repo = git_typescript_fixture("git-toolkit-url", "1.2.4")?;
	let file_url_package = server
		.add_package(json!({
			"language": Language::TypeScript,
			"name": "git-toolkit-url",
			"version": Version::parse("1.2.4")?,
			"source": directory_url(file_url_repo.path())?.to_string(),
		}))
		.await?;
	assert_eq!(file_url_package["name"], "git-toolkit-url");
	assert_eq!(file_url_package["version"], "1.2.4");
	assert_eq!(file_url_package["source"], directory_url(file_url_repo.path())?.as_str());
	assert_eq!(file_url_package["state"]["health"], "healthy");
	assert_eq!(
		file_url_package["state"]["tracked_version_commit"],
		file_url_package["state"]["latest_remote_commit"]
	);
	assert!(file_url_package["state"]["entry_count"].as_u64().unwrap_or_default() > 0);
	assert!(file_url_package["state"]["document_count"].as_u64().unwrap_or_default() > 0);
	assert!(file_url_package["state"]["last_error"].is_null());

	Ok(())
}

#[tokio::test]
async fn add_package_returns_immediately_while_background_sync_runs() -> color_eyre::Result<()> {
	let server = TestServer::spawn().await?;
	let repo = git_typescript_fixture("async-toolkit", "4.0.0")?;

	let package = server
		.enqueue_package(json!({
			"language": Language::TypeScript,
			"name": "async-toolkit",
			"version": Version::parse("4.0.0")?,
			"source": repo.path().display().to_string(),
		}))
		.await?;

	assert_eq!(package["name"], "async-toolkit");
	assert_eq!(package["state"]["sync_status"], "queued");
	assert_eq!(package["state"]["health"], "pending");

	let package_id = package["id"].as_u64().expect("package id should be present");
	let eventual = server.wait_for_terminal_package(package_id).await?;
	assert_eq!(eventual["state"]["health"], "healthy");
	assert_eq!(eventual["state"]["sync_status"], "idle");
	assert!(eventual["state"]["last_error"].is_null());

	Ok(())
}

#[tokio::test]
async fn package_registry_persists_across_server_restart() -> color_eyre::Result<()> {
	let base_storage = tempfile::tempdir()?;
	let first_server = TestServer::spawn_with_storage(base_storage.path()).await?;
	let repo = git_typescript_fixture("persistent-toolkit", "2.0.0")?;

	let package = first_server
		.add_package(json!({
			"language": Language::TypeScript,
			"name": "persistent-toolkit",
			"version": Version::parse("2.0.0")?,
			"source": repo.path().display().to_string(),
		}))
		.await?;
	assert_eq!(package["state"]["health"], "healthy");
	drop(first_server);

	let second_server = TestServer::spawn_with_storage(base_storage.path()).await?;
	let packages = second_server.list_packages().await?;

	assert_eq!(packages.as_array().map(|value| value.len()), Some(1));
	assert_eq!(packages[0]["name"], "persistent-toolkit");
	assert_eq!(packages[0]["version"], "2.0.0");
	assert_eq!(packages[0]["state"]["health"], "healthy");
	assert!(base_storage.path().join("packages.json").is_file());

	let duplicate = second_server
		.add_package(json!({
			"language": Language::TypeScript,
			"name": "persistent-toolkit",
			"version": Version::parse("2.0.0")?,
			"source": repo.path().display().to_string(),
		}))
		.await?;
	assert_eq!(duplicate["id"], package["id"]);

	Ok(())
}

#[tokio::test]
async fn duplicate_add_returns_existing_package_without_requeue() -> color_eyre::Result<()> {
	let server = TestServer::spawn().await?;
	let repo = git_typescript_fixture("idempotent-toolkit", "2.1.0")?;

	let initial = server
		.add_package(json!({
			"language": Language::TypeScript,
			"name": "idempotent-toolkit",
			"version": Version::parse("2.1.0")?,
			"source": repo.path().display().to_string(),
		}))
		.await?;
	assert_eq!(initial["state"]["health"], "healthy");

	let duplicate = server
		.enqueue_package(json!({
			"language": Language::TypeScript,
			"name": "idempotent-toolkit",
			"version": Version::parse("2.1.0")?,
			"source": repo.path().display().to_string(),
		}))
		.await?;
	assert_eq!(duplicate["id"], initial["id"]);
	assert_eq!(duplicate["state"]["sync_status"], "idle");
	assert_eq!(duplicate["state"]["health"], "healthy");

	Ok(())
}

#[tokio::test]
async fn repository_backed_packages_ignore_legacy_numeric_cache_directories()
-> color_eyre::Result<()> {
	let base_storage = tempfile::tempdir()?;
	let legacy_repo_dir = base_storage.path().join("repositories/1/.git");
	fs::create_dir_all(&legacy_repo_dir)?;
	fs::write(legacy_repo_dir.join("HEAD"), b"not a git repository")?;

	let server = TestServer::spawn_with_storage(base_storage.path()).await?;
	let repo = git_typescript_fixture("cache-safe-toolkit", "3.1.4")?;
	let package = server
		.add_package(json!({
			"language": Language::TypeScript,
			"name": "cache-safe-toolkit",
			"version": Version::parse("3.1.4")?,
			"source": repo.path().display().to_string(),
		}))
		.await?;

	assert_eq!(package["state"]["health"], "healthy");
	assert!(base_storage.path().join("repositories/1/.git/HEAD").is_file());
	assert!(
		base_storage.path().join("repositories/typescript").read_dir()?.any(|entry| entry.is_ok())
	);

	Ok(())
}

#[tokio::test]
#[ignore = "live npm smoke test"]
async fn add_live_typescript_registry_packages_smoke() -> color_eyre::Result<()> {
	let server = TestServer::spawn().await?;

	for (name, version) in [("@types/node", "24.0.0"), ("zod", "3.25.76"), ("nanoid", "5.1.6")] {
		let started = Instant::now();
		let package = server
			.add_package(json!({
				"language": Language::TypeScript,
				"name": name,
				"version": Version::parse(version)?,
			}))
			.await?;
		eprintln!(
			"indexed {name}@{version} in {:?} entries={} docs={}",
			started.elapsed(),
			package["state"]["entry_count"].as_u64().unwrap_or_default(),
			package["state"]["document_count"].as_u64().unwrap_or_default(),
		);
		assert_eq!(package["state"]["health"], "healthy");
		assert!(package["state"]["entry_count"].as_u64().unwrap_or_default() > 0);
		assert!(package["state"]["document_count"].as_u64().unwrap_or_default() > 0);
	}

	Ok(())
}

struct TestServer {
	_base_storage: Option<TempDir>,
	base_url:      String,
	task:          JoinHandle<()>,
}

impl TestServer {
	async fn spawn() -> color_eyre::Result<Self> {
		let base_storage = tempfile::tempdir()?;
		Self::spawn_with_optional_storage(Some(base_storage), None).await
	}

	async fn spawn_with_storage(path: &Path) -> color_eyre::Result<Self> {
		Self::spawn_with_optional_storage(None, Some(path)).await
	}

	async fn spawn_with_optional_storage(
		base_storage: Option<TempDir>,
		existing_root: Option<&Path>,
	) -> color_eyre::Result<Self> {
		let storage_root = base_storage
			.as_ref()
			.map(|tempdir| tempdir.path().to_path_buf())
			.or_else(|| existing_root.map(Path::to_path_buf))
			.expect("storage root should be available");
		let storage = StorageLayout::new(&storage_root);
		storage.ensure()?;

		let registry =
			Arc::new(LocalRegistry::new(storage, Duration::from_secs(3600), PipelineConfig {
				terminus:        None,
				qdrant:          None,
				embedding_model: "text-embedding-3-small".to_owned(),
				upload_schema:   false,
			}));
		let app = api::router(AppState {
			registry,
			pipeline: PipelineConfig {
				terminus:        None,
				qdrant:          None,
				embedding_model: "text-embedding-3-small".to_owned(),
				upload_schema:   false,
			},
			sessions: SessionStore::default(),
		});
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
		let address = listener.local_addr()?;
		let task = tokio::spawn(async move {
			let _ = axum::serve(listener, app).await;
		});

		Ok(Self { _base_storage: base_storage, base_url: format!("http://{address}"), task })
	}

	async fn add_package(&self, payload: Value) -> color_eyre::Result<Value> {
		let package = self.enqueue_package(payload).await?;
		let package_id = package["id"].as_u64().expect("package id should be present");
		self.wait_for_terminal_package(package_id).await
	}

	async fn enqueue_package(&self, payload: Value) -> color_eyre::Result<Value> {
		let response =
			Client::new().post(format!("{}/api/packages", self.base_url)).json(&payload).send().await?;

		let status = response.status();
		let body = response.text().await?;
		assert!(
			matches!(status, StatusCode::OK | StatusCode::CREATED | StatusCode::ACCEPTED),
			"unexpected response body: {body}"
		);
		Ok(serde_json::from_str(&body)?)
	}

	async fn list_packages(&self) -> color_eyre::Result<Value> {
		let response = Client::new().get(format!("{}/api/packages", self.base_url)).send().await?;
		let status = response.status();
		let body = response.text().await?;
		assert_eq!(status, StatusCode::OK, "unexpected response body: {body}");
		Ok(serde_json::from_str(&body)?)
	}

	async fn get_package(&self, id: u64) -> color_eyre::Result<Value> {
		let response = Client::new().get(format!("{}/api/packages/{id}", self.base_url)).send().await?;
		let status = response.status();
		let body = response.text().await?;
		assert_eq!(status, StatusCode::OK, "unexpected response body: {body}");
		Ok(serde_json::from_str(&body)?)
	}

	async fn wait_for_terminal_package(&self, id: u64) -> color_eyre::Result<Value> {
		let deadline = Instant::now() + Duration::from_secs(30);
		loop {
			let package = self.get_package(id).await?;
			let sync_status = package["state"]["sync_status"].as_str().unwrap_or_default();
			let health = package["state"]["health"].as_str().unwrap_or_default();
			if sync_status == "idle" || health == "degraded" {
				return Ok(package);
			}
			if Instant::now() >= deadline {
				panic!("timed out waiting for package {id} to finish syncing: {package}");
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	}
}

impl Drop for TestServer {
	fn drop(&mut self) { self.task.abort(); }
}

fn git_typescript_fixture(package_name: &str, version: &str) -> color_eyre::Result<TempDir> {
	let fixture = fixture_root().join("ts/repository");
	let temp_dir = tempfile::tempdir()?;
	copy_tree(&fixture, temp_dir.path())?;
	write_package_manifest(temp_dir.path(), package_name, version)?;

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

fn write_package_manifest(
	repository_root: &Path,
	package_name: &str,
	version: &str,
) -> color_eyre::Result<()> {
	let manifest_path = repository_root.join("package.json");
	let manifest = json!({
		"name": package_name,
		"version": version,
		"types": "mod.ts",
	});
	fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
		.wrap_err_with(|| format!("failed to write `{}`", manifest_path.display()))?;
	Ok(())
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

fn directory_url(path: &Path) -> color_eyre::Result<Url> {
	Url::from_directory_path(path)
		.map_err(|()| color_eyre::eyre::eyre!("failed to convert `{}` into a file URL", path.display()))
}

fn canonical_directory_url(path: &Path) -> color_eyre::Result<Url> {
	directory_url(&path.canonicalize()?)
}
