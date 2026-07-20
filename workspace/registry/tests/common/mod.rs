//! Shared fixtures for the registry integration tests: coordinate/package
//! constructors and a fully-populated blob builder, so every spec exercises the
//! same validated shapes the pipeline produces.
#![allow(dead_code, reason = "each integration test binary uses its own subset")]

use std::path::{Path, PathBuf};

use heart::{Edition, Language, PackageVersion, RegistryOrigin, ResolutionState, Toolchain};
use registry::{
    BlobManifest, GlobalPackage, Package,
    blob::{BlobBuilder, ReferenceSet, creation::PendingSection},
    index::{GlobalStore, InstanceToken},
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

/// A validated Go module coordinate tuple + package.
pub fn go_package(module_path: &str, version: &str) -> Package {
    let coordinates = Coordinates {
        origin: RegistryOrigin::Custom {
            name: "proxy.golang.org".into(),
            url: url::Url::parse("https://proxy.golang.org").expect("fixture url"),
        },
        name: PackageName::new(Language::Go, module_path).expect("fixture Go module is valid"),
        version: PackageVersion::try_from((Language::Go, version))
            .expect("fixture version is valid"),
    };
    let toolchain = Toolchain::Go { compiler: semver::Version::new(1, 22, 0) };
    Package { coordinates, toolchain }
}

/// A validated Maven coordinate tuple + package (groupId:artifactId form accepted).
pub fn java_package(artifact: &str, version: &str) -> Package {
    let coordinates = Coordinates {
        origin: RegistryOrigin::Custom {
            name: "repo1.maven.org".into(),
            url: url::Url::parse("https://repo1.maven.org").expect("fixture url"),
        },
        name: PackageName::new(Language::Java, artifact).expect("fixture Maven artifact is valid"),
        version: PackageVersion::try_from((Language::Java, version))
            .expect("fixture version is valid"),
    };
    let toolchain = Toolchain::Java { compiler: semver::Version::new(21, 0, 0) };
    Package { coordinates, toolchain }
}

/// A validated NuGet coordinate tuple + package (for C# / NuGet ecosystem specs).
pub fn csharp_package(name: &str, version: &str) -> Package {
    let coordinates = Coordinates {
        origin: RegistryOrigin::NuGet,
        name: PackageName::new(Language::CSharp, name).expect("fixture names are valid"),
        version: PackageVersion::try_from((Language::CSharp, version))
            .expect("fixture versions are valid"),
    };
    let toolchain = Toolchain::CSharp { sdk: semver::Version::new(10, 0, 0) };
    Package { coordinates, toolchain }
}

/// A collision-free crates.io package name: `prefix` plus a fresh uuid, so
/// catalog-backed tests never trip over rows a previous run left behind.
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

/// Open a fresh in-memory catalog store for tests. Returns `(GlobalStore, CatalogWriter)`.
pub fn catalog_store(test: &str) -> (GlobalStore<index::engine::memory::MemoryEngine>, std::sync::Arc<index::store::writer::CatalogWriter<index::engine::memory::MemoryEngine>>) {
    let _ = test;
    let engine = index::engine::memory::MemoryEngine::open_in_memory()
        .expect("in-memory catalog engine opens");
    index::migrations::runner::migrate_to_v4(&engine).expect("catalog migrates");
    let writer = std::sync::Arc::new(index::store::writer::CatalogWriter::new(engine));
    let instance = InstanceToken::new("test/catalog").expect("fixture instance");
    (GlobalStore::new(std::sync::Arc::clone(&writer), instance), writer)
}

/// The `{org}/{db}` instance token every gated spec salts symbol ids with.
pub fn test_instance() -> InstanceToken {
    InstanceToken::new("test-org/test-db").expect("the fixture token is `org/db`-shaped")
}
