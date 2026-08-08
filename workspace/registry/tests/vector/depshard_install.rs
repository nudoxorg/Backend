#![cfg(feature = "local")]
//! Dep-shard install flow against a stub fetcher: the happy path registers
//! a searchable read-only shard; every §20.3 fallback (missing artifact,
//! persistent hash mismatch, unloadable artifact) remote-routes with the
//! exact retry budget — never more, never fewer.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use common::*;
use heart::{ContentHash, PackageId};
use registry::vector::{NAMESPACE_NUDOX, VectorStore};
use registry::vector::local::{
	ArtifactFetcher, DepManifestEntry, FetchError, InstallOutcome, RemoteRouteReason, WorkingSet,
	evict, install, pack_shard,
};

fn pkg(name: &str) -> PackageId {
	PackageId::from_name(&NAMESPACE_NUDOX, name.as_bytes())
}

/// Serves a fixed response and counts calls.
struct StubFetcher {
	response: Result<Vec<u8>, ()>,
	calls: AtomicUsize,
}

impl StubFetcher {
	fn serving(bytes: Vec<u8>) -> Self {
		Self { response: Ok(bytes), calls: AtomicUsize::new(0) }
	}

	fn missing() -> Self {
		Self { response: Err(()), calls: AtomicUsize::new(0) }
	}

	fn calls(&self) -> usize {
		self.calls.load(Ordering::SeqCst)
	}
}

#[async_trait]
impl ArtifactFetcher for StubFetcher {
	async fn fetch(&self, artifact: &ContentHash) -> Result<Vec<u8>, FetchError> {
		self.calls.fetch_add(1, Ordering::SeqCst);
		match &self.response {
			Ok(bytes) => Ok(bytes.clone()),
			Err(()) => Err(FetchError::NotFound(*artifact)),
		}
	}
}

/// Bake a real 10-point shard and return its artifact.
async fn baked_artifact() -> (Vec<u8>, ContentHash) {
	let src = tempfile::tempdir().unwrap();
	let store = open_mutable_f32(src.path()).await;
	store.upsert(corpus(10, "dep")).await.unwrap();
	store.close().await.unwrap();
	settle().await;
	pack_shard(src.path()).unwrap()
}

async fn empty_working_set(project_dir: &Path) -> registry::vector::local::SharedWorkingSet {
	let project = open_mutable_f32(project_dir).await;
	WorkingSet::new(Arc::new(project)).into_shared()
}

fn entry(package: PackageId, artifact_id: ContentHash) -> DepManifestEntry {
	DepManifestEntry { package, version: "1.2.3".into(), artifact_id, ram_estimate: 10 * 922 }
}

/// fetch → verify → unpack → load → register: one fetch, a resident
/// searchable dep shard under `dep_root/<pkg>/<version>/`, then evict
/// removes both the registration and the directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_registers_searchable_shard_and_evict_removes_it() {
	let (artifact, hash) = baked_artifact().await;
	let fetcher = StubFetcher::serving(artifact);
	let package = pkg("dep");

	let project_dir = tempfile::tempdir().unwrap();
	let dep_root = tempfile::tempdir().unwrap();
	let working_set = empty_working_set(project_dir.path()).await;

	let outcome = install(
		&fetcher,
		&entry(package, hash),
		dep_root.path(),
		&f32_schema(),
		&working_set,
	)
	.await
	.unwrap();
	assert_eq!(outcome, InstallOutcome::Installed);
	assert_eq!(fetcher.calls(), 1, "clean install is exactly one fetch");

	let shard_dir = dep_root.path().join(package.to_string()).join("1.2.3");
	assert!(shard_dir.join("schema.json").is_file(), "shard unpacked in place");

	{
		let ws = working_set.read().await;
		assert!(ws.resident_packages().contains(&package));
		// The baked points answer through the fan-out.
		let hits = ws.search_all(request(basis(0), 20)).await.unwrap();
		assert!(hits.iter().any(|hit| hit.id == pid(0)), "dep points must be searchable");
	}

	evict(package, dep_root.path(), &working_set).await.unwrap();
	let ws = working_set.read().await;
	assert!(!ws.resident_packages().contains(&package), "evict deregisters");
	assert!(
		!dep_root.path().join(package.to_string()).exists(),
		"evict deletes the package directory"
	);
}

/// Missing upstream artifact: exactly one fetch, remote-route, no residue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_artifact_remote_routes_without_retry() {
	let fetcher = StubFetcher::missing();
	let package = pkg("dep");
	let project_dir = tempfile::tempdir().unwrap();
	let dep_root = tempfile::tempdir().unwrap();
	let working_set = empty_working_set(project_dir.path()).await;

	let outcome = install(
		&fetcher,
		&entry(package, ContentHash::of_bytes(b"whatever")),
		dep_root.path(),
		&f32_schema(),
		&working_set,
	)
	.await
	.unwrap();

	assert_eq!(outcome, InstallOutcome::RemoteRoute(RemoteRouteReason::ArtifactMissing));
	assert_eq!(fetcher.calls(), 1, "missing is definitive; no blind retry");
	assert!(!working_set.read().await.resident_packages().contains(&package));
	assert!(!dep_root.path().join(package.to_string()).exists(), "no residue on disk");
}

/// Persistently wrong bytes: exactly two fetches (the one §20.3 refetch),
/// then remote-route — and nothing was ever unpacked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_hash_mismatch_refetches_once_then_remote_routes() {
	let fetcher = StubFetcher::serving(b"attacker controlled bytes".to_vec());
	let package = pkg("dep");
	let expected = ContentHash::of_bytes(b"the real artifact");
	let project_dir = tempfile::tempdir().unwrap();
	let dep_root = tempfile::tempdir().unwrap();
	let working_set = empty_working_set(project_dir.path()).await;

	let outcome = install(
		&fetcher,
		&entry(package, expected),
		dep_root.path(),
		&f32_schema(),
		&working_set,
	)
	.await
	.unwrap();

	assert_eq!(outcome, InstallOutcome::RemoteRoute(RemoteRouteReason::HashMismatch));
	assert_eq!(fetcher.calls(), 2, "exactly one refetch on hash mismatch");
	assert!(!working_set.read().await.resident_packages().contains(&package));
	assert!(
		!dep_root.path().join(package.to_string()).exists(),
		"mismatched bytes must never touch the dep root"
	);
}

/// Correctly hashed garbage (verification passes, unpack/load cannot):
/// one refetch, then remote-route with the directory swept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloadable_artifact_remote_routes_after_one_refetch() {
	let garbage = b"correctly hashed but not a tar.zst".to_vec();
	let hash = ContentHash::of_bytes(&garbage);
	let fetcher = StubFetcher::serving(garbage);
	let package = pkg("dep");
	let project_dir = tempfile::tempdir().unwrap();
	let dep_root = tempfile::tempdir().unwrap();
	let working_set = empty_working_set(project_dir.path()).await;

	let outcome = install(
		&fetcher,
		&entry(package, hash),
		dep_root.path(),
		&f32_schema(),
		&working_set,
	)
	.await
	.unwrap();

	assert_eq!(outcome, InstallOutcome::RemoteRoute(RemoteRouteReason::LoadFailure));
	assert_eq!(fetcher.calls(), 2, "load failure earns exactly one clean refetch");
	assert!(!working_set.read().await.resident_packages().contains(&package));
	let package_dir = dep_root.path().join(package.to_string());
	let version_dir = package_dir.join("1.2.3");
	assert!(!version_dir.exists(), "failed install must sweep its directory");
}

/// Upgrade is a whole-directory swap: installing v2 over a registered v1
/// leaves exactly the v2 directory and the v2 registration.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_swaps_whole_directory() {
	let (artifact, hash) = baked_artifact().await;
	let package = pkg("dep");
	let project_dir = tempfile::tempdir().unwrap();
	let dep_root = tempfile::tempdir().unwrap();
	let working_set = empty_working_set(project_dir.path()).await;

	let fetcher = StubFetcher::serving(artifact.clone());
	let v1 = DepManifestEntry {
		package,
		version: "1.0.0".into(),
		artifact_id: hash,
		ram_estimate: 10 * 922,
	};
	assert_eq!(
		install(&fetcher, &v1, dep_root.path(), &f32_schema(), &working_set).await.unwrap(),
		InstallOutcome::Installed
	);

	let v2 = DepManifestEntry { version: "2.0.0".into(), ..v1.clone() };
	assert_eq!(
		install(&fetcher, &v2, dep_root.path(), &f32_schema(), &working_set).await.unwrap(),
		InstallOutcome::Installed
	);

	let package_dir = dep_root.path().join(package.to_string());
	assert!(!package_dir.join("1.0.0").exists(), "old version dir must be swapped out");
	assert!(package_dir.join("2.0.0").join("schema.json").is_file());
	assert!(working_set.read().await.resident_packages().contains(&package));

	// The upgraded shard still answers.
	let hits =
		working_set.read().await.search_all(request(basis(0), 20)).await.unwrap();
	assert!(hits.iter().any(|hit| hit.id == pid(0)));
}
