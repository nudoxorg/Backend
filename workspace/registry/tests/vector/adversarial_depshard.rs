#![cfg(feature = "local")]
//! Adversarial depshard install tests: stale leftover directories, extreme
//! manifest values, eviction correctness (09-vector §20.3/§20.4, §13.5).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use common::*;
use heart::{ContentHash, PackageId};
use vector::NAMESPACE_NUDOX;
use vector::local::{
    ArtifactFetcher, DepManifestEntry, FetchError, InstallOutcome, WorkingSet, evict, install,
    pack_shard,
};

fn pkg(name: &str) -> PackageId {
    PackageId::from_name(&NAMESPACE_NUDOX, name.as_bytes())
}

struct StubFetcher {
    response: Vec<u8>,
    hash: ContentHash,
    calls: AtomicUsize,
}

impl StubFetcher {
    fn serving(response: Vec<u8>, hash: ContentHash) -> Self {
        Self { response, hash, calls: AtomicUsize::new(0) }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ArtifactFetcher for StubFetcher {
    async fn fetch(&self, _artifact: &ContentHash) -> Result<Vec<u8>, FetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.response.clone())
    }
}

/// Bake a real shard artifact.
async fn baked_artifact() -> (Vec<u8>, ContentHash) {
    let src = tempfile::tempdir().unwrap();
    let store = open_mutable_f32(src.path()).await;
    store.upsert(corpus(10, "dep")).await.unwrap();
    store.close().await.unwrap();
    settle().await;
    pack_shard(src.path()).unwrap()
}

fn entry_with_ram(
    package: PackageId,
    artifact_id: ContentHash,
    version: &str,
    ram_estimate: u64,
) -> DepManifestEntry {
    DepManifestEntry {
        package,
        version: version.into(),
        artifact_id,
        ram_estimate,
    }
}

async fn empty_working_set(project_dir: &Path) -> vector::local::SharedWorkingSet {
    let project = open_mutable_f32(project_dir).await;
    WorkingSet::new(Arc::new(project)).into_shared()
}

// ─── Area 8a: Stale leftover directories ─────────────────────────────────────

/// The fetcher returns correct bytes but:
///   - The destination directory (`<dep_root>/<pkg>/<version>/`) ALREADY EXISTS
///     from a crashed prior install (stale final dir).
///   - The staging directory (`<dest>.tmp`) ALSO EXISTS from a crashed unpack.
///
/// Install must converge to a working shard despite both leftovers.
/// Contract: `unpack_and_load` removes any existing dest before unpacking, and
/// `unpack_shard` removes any stale `.tmp` sibling on entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_with_stale_dest_and_stale_tmp_converges() {
    let (artifact, hash) = baked_artifact().await;
    let package = pkg("stale-dep");
    let project_dir = tempfile::tempdir().unwrap();
    let dep_root = tempfile::tempdir().unwrap();
    let working_set = empty_working_set(project_dir.path()).await;

    let shard_dir = dep_root.path()
        .join(package.to_string())
        .join("1.0.0");

    // Simulate a crashed prior install: create both the stale final dir and stale .tmp.
    std::fs::create_dir_all(&shard_dir).unwrap();
    std::fs::write(shard_dir.join("stale_marker.txt"), b"crashed").unwrap();

    let tmp_dir = dep_root.path()
        .join(package.to_string())
        .join("1.0.0.tmp");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    std::fs::write(tmp_dir.join("stale_tmp_marker.txt"), b"crashed_tmp").unwrap();

    let fetcher = StubFetcher::serving(artifact, hash);
    let entry = entry_with_ram(package, fetcher.hash, "1.0.0", 10 * 922);

    let outcome = install(&fetcher, &entry, dep_root.path(), &f32_schema(), &working_set)
        .await
        .unwrap();

    // Install must succeed despite both leftovers.
    assert_eq!(
        outcome,
        InstallOutcome::Installed,
        "install must converge despite stale dest and stale .tmp dirs"
    );

    // Stale marker must be gone (replaced by fresh unpack).
    assert!(
        !shard_dir.join("stale_marker.txt").exists(),
        "stale final dir must be replaced by fresh unpack"
    );
    // Fresh schema.json must be present.
    assert!(
        shard_dir.join("schema.json").is_file(),
        "fresh schema.json must be present after install with stale leftovers"
    );
    // Shard must be searchable.
    {
        let ws = working_set.read().await;
        assert!(ws.resident_packages().contains(&package));
        let hits = ws.search_all(request(basis(0), 15)).await.unwrap();
        assert!(hits.iter().any(|h| h.id == pid(0)), "dep points must be searchable after recovery");
    }
    assert_eq!(fetcher.calls(), 1, "exactly one fetch for a clean install despite stale dirs");
}

// ─── Area 8b: ram_estimate extremes ──────────────────────────────────────────

/// ram_estimate of 0: install must not panic or error — treat as zero cost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_with_ram_estimate_zero_does_not_panic() {
    let (artifact, hash) = baked_artifact().await;
    let package = pkg("zero-ram");
    let project_dir = tempfile::tempdir().unwrap();
    let dep_root = tempfile::tempdir().unwrap();
    let working_set = empty_working_set(project_dir.path()).await;

    let fetcher = StubFetcher::serving(artifact, hash);
    let entry = entry_with_ram(package, fetcher.hash, "1.0.0", 0);

    let outcome = install(&fetcher, &entry, dep_root.path(), &f32_schema(), &working_set)
        .await
        .unwrap();
    assert_eq!(outcome, InstallOutcome::Installed, "ram_estimate=0 must install without panic");

    let ws = working_set.read().await;
    assert!(ws.resident_packages().contains(&package));
}

/// ram_estimate of u64::MAX: install must not panic or error — the cost is
/// stored and forwarded to the admission budget, which may reject it, but the
/// install machinery itself must be resilient.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_with_ram_estimate_max_does_not_panic() {
    let (artifact, hash) = baked_artifact().await;
    let package = pkg("max-ram");
    let project_dir = tempfile::tempdir().unwrap();
    let dep_root = tempfile::tempdir().unwrap();
    let working_set = empty_working_set(project_dir.path()).await;

    let fetcher = StubFetcher::serving(artifact, hash);
    let entry = entry_with_ram(package, fetcher.hash, "1.0.0", u64::MAX);

    let outcome = install(&fetcher, &entry, dep_root.path(), &f32_schema(), &working_set)
        .await
        .unwrap();
    // Install itself doesn't check the budget — that's the admission layer's job.
    assert_eq!(
        outcome,
        InstallOutcome::Installed,
        "ram_estimate=u64::MAX must not panic in the install machinery"
    );
}

// ─── Area 8c: Evict during no-search ─────────────────────────────────────────

/// Evict a registered dep when no searches are in flight.
/// After eviction: dir is gone, registry is updated, re-install works.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn evict_no_search_dir_gone_registry_updated() {
    let (artifact, hash) = baked_artifact().await;
    let package = pkg("evict-me");
    let project_dir = tempfile::tempdir().unwrap();
    let dep_root = tempfile::tempdir().unwrap();
    let working_set = empty_working_set(project_dir.path()).await;

    let fetcher = StubFetcher::serving(artifact, hash);
    let entry = entry_with_ram(package, fetcher.hash, "1.0.0", 10 * 922);

    // Install first.
    let outcome = install(&fetcher, &entry, dep_root.path(), &f32_schema(), &working_set)
        .await
        .unwrap();
    assert_eq!(outcome, InstallOutcome::Installed);

    let shard_dir = dep_root.path().join(package.to_string()).join("1.0.0");
    assert!(shard_dir.exists(), "shard dir must exist after install");

    // Evict.
    evict(package, dep_root.path(), &working_set).await.unwrap();

    // Directory must be gone.
    assert!(
        !dep_root.path().join(package.to_string()).exists(),
        "package directory must be gone after eviction"
    );
    // Registry must no longer contain the package.
    assert!(
        !working_set.read().await.resident_packages().contains(&package),
        "evicted package must not be in resident_packages"
    );

    // Re-install must work (the state is clean after eviction).
    let outcome2 = install(&fetcher, &entry, dep_root.path(), &f32_schema(), &working_set)
        .await
        .unwrap();
    assert_eq!(outcome2, InstallOutcome::Installed, "re-install after eviction must succeed");
    assert!(working_set.read().await.resident_packages().contains(&package));
}

/// Evict a package that was never installed (not in working set, no dir).
/// Must succeed cleanly (idempotent, no panic).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evict_nonexistent_package_is_idempotent() {
    let project_dir = tempfile::tempdir().unwrap();
    let dep_root = tempfile::tempdir().unwrap();
    let working_set = empty_working_set(project_dir.path()).await;

    let package = pkg("never-installed");
    // Must not panic or error.
    evict(package, dep_root.path(), &working_set).await.unwrap();
    assert!(!working_set.read().await.resident_packages().contains(&package));
}
