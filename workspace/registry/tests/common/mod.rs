//! Shared fixtures for the registry integration tests: coordinate/package
//! constructors and a fully-populated blob builder, so every spec exercises the
//! same validated shapes the pipeline produces.
#![allow(dead_code, reason = "each integration test binary uses its own subset")]

use std::path::{Path, PathBuf};

use heart::{Edition, Language, PackageVersion, RegistryOrigin, ResolutionState, Toolchain};
use registry::{
    BlobManifest, GlobalPackage, Package,
    blob::{BlobBuilder, ReferenceSet, creation::PendingSection},
    index::{GlobalStore, TerminusInstance},
    package::{Coordinates, PackageName},
};

/// A validated crates.io coordinate tuple.
pub fn rust_coordinates(name: &str, version: &str) -> Coordinates {
    Coordinates {
        origin: RegistryOrigin::CratesIo,
        name: PackageName::new(Language::Rust, name).expect("fixture names are valid"),
        version: PackageVersion::try_from((Language::Rust, version))
            .expect("fixture versions are valid"),
    }
}

/// A per-source package over [`rust_coordinates`].
pub fn rust_package(name: &str, version: &str) -> Package {
    Package { coordinates: rust_coordinates(name, version), toolchain: rust_toolchain() }
}

/// A fixed rustc toolchain provenance.
pub fn rust_toolchain() -> Toolchain {
    Toolchain::Rust { compiler: semver::Version::new(1, 85, 0), edition: Edition::E2024 }
}

/// A finalized manifest + its pending `cas/` writes for `package`, carrying two
/// source files, an IR section, and an (empty) reference section.
pub fn built_manifest(package: &Package) -> (BlobManifest, Vec<PendingSection>) {
    built_manifest_with(package, b"pub fn answer() -> u32 { 42 }")
}

/// Like [`built_manifest`], with caller-controlled bytes for the first source
/// file (so tests can vary the content generation).
pub fn built_manifest_with(
    package: &Package,
    lib_bytes: &'static [u8],
) -> (BlobManifest, Vec<PendingSection>) {
    let mut builder = BlobBuilder::new(package.id(), package.toolchain.clone());
    builder
        .push_file("src/lib.rs".into(), bytes::Bytes::from_static(lib_bytes))
        .expect("fresh path");
    builder
        .push_file("README.md".into(), bytes::Bytes::from_static(b"# fixture"))
        .expect("fresh path");
    builder.set_ir(bytes::Bytes::from_static(b"ir-section-bytes")).expect("first ir");
    builder
        .set_references(&ReferenceSet { by_file: Vec::new() })
        .expect("first references");
    builder.finalize().expect("a complete builder finalizes")
}

/// A validated PyPI coordinate tuple + package (for cross-ecosystem specs).
pub fn python_package(name: &str, version: &str) -> Package {
    let coordinates = Coordinates {
        origin: RegistryOrigin::PyPi,
        name: PackageName::new(Language::Python, name).expect("fixture names are valid"),
        version: PackageVersion::try_from((Language::Python, version))
            .expect("fixture versions are valid"),
    };
    let toolchain = Toolchain::Python { interpreter: semver::Version::new(3, 12, 0) };
    Package { coordinates, toolchain }
}

/// A collision-free crates.io package name: `prefix` plus a fresh uuid, so
/// postgres-backed tests never trip over rows a previous run left behind.
pub fn unique_rust_name(prefix: &str) -> String {
    format!("{prefix}{}", uuid::Uuid::new_v4().simple())
}

/// A [`GlobalPackage`] minted from a per-source package the deterministic way
/// (`id` is always the coordinates fingerprint, never invented).
pub fn global_package(package: Package, state: ResolutionState) -> GlobalPackage {
    GlobalPackage { id: package.id(), package, state, facets: None }
}

/// A unique temporary directory removed on drop (for tantivy replicas).
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("registry-test-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temp dir under std::env::temp_dir is creatable");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The postgres pool the infrastructure-gated specs run against, or `None`
/// (with a skip note) when no database is configured/reachable — so the suite
/// stays green offline while remaining a real test where postgres exists.
pub async fn postgres_pool(test: &str) -> Option<sqlx::PgPool> {
    let url = match std::env::var("REGISTRY_TEST_POSTGRES")
        .or_else(|_| std::env::var("DATABASE_URL"))
    {
        Ok(url) => url,
        Err(_) => {
            eprintln!(
                "skipping {test}: set REGISTRY_TEST_POSTGRES (or DATABASE_URL) to run \
                 postgres-backed registry tests"
            );
            return None;
        }
    };
    match sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(&url).await {
        Ok(pool) => Some(pool),
        Err(error) => {
            eprintln!("skipping {test}: postgres is configured but unreachable: {error}");
            None
        }
    }
}

/// The `{org}/{db}` instance token every gated spec salts symbol ids with.
pub fn test_instance() -> TerminusInstance {
    TerminusInstance::new("test-org/test-db").expect("the fixture token is `org/db`-shaped")
}

/// A connected [`GlobalStore`] over `pool` (applies the idempotent schema).
pub async fn global_store(pool: sqlx::PgPool) -> GlobalStore {
    use heart::Connect;
    GlobalStore::new(pool, test_instance())
        .connect()
        .await
        .expect("the configured postgres accepts the registry schema")
}
