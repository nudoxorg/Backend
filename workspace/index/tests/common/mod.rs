//! Shared test harness: build a migrated catalog writer, plus a programmable
//! fake git repository for ingest tests.
//!
//! Every integration test opens [`index::engine::Configured`] — the engine this
//! build selects, which under the default feature set is the **real** DoltLite
//! prolly-tree engine — migrates it to schema v4, and wraps it in a
//! [`CatalogWriter`]. Nothing here names a concrete engine, so no test can drift
//! onto a different one than the rest of the suite.
//!
//! Not every test binary uses every helper, so allow dead code here.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

// `<Configured as OpenCatalog>::…` rather than `Configured::…` throughout: the
// call must go through the trait, not through whichever inherent constructor the
// selected engine happens to also expose. Otherwise the harness silently depends
// on an engine's private API surface and stops compiling the day a build selects
// one whose constructors are named differently — which is how it came to name a
// concrete engine in the first place.
use index::engine::{Configured, OpenCatalog};
use index::ingest::git::{GitRepository, GitRepositoryError, LsRemoteRef};
use index::migrations::runner::migrate_to_v4;
use index::store::writer::CatalogWriter;

/// Open a fresh, migrated catalog writer with no file behind it.
pub fn migrated_writer() -> CatalogWriter<Configured> {
    let engine = <Configured as OpenCatalog>::open_in_memory().expect("open in-memory engine");
    migrate_to_v4(&engine).expect("migrate to schema v4");
    CatalogWriter::new(engine)
}

/// A deterministic package stem id from a small seed.
pub fn stem_id(seed: u8) -> index::ids::PackageStemId {
    let mut bytes = [0u8; 16];
    bytes[0] = seed;
    index::ids::PackageStemId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

/// A deterministic version (package) id from a small seed.
pub fn version_id(seed: u8) -> index::ids::PackageId {
    let mut bytes = [0u8; 16];
    bytes[15] = seed;
    index::ids::PackageId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

/// A deterministic generation stamp from a small seed.
pub fn gen_stamp(seed: u8) -> index::ids::GenerationStamp {
    let mut bytes = [0u8; 32];
    bytes[0] = seed;
    index::ids::GenerationStamp::from_bytes(bytes)
}

// ─────────────────────────────────────────────────────────────────────────────
// Storage-characteristics harness: the real `.real-crates/` corpus + an
// on-disk (not `:memory:`) catalog engine, so integration tests can take real
// `disk_delta_bytes` measurements (doctrine §4).
// ─────────────────────────────────────────────────────────────────────────────

/// The repository root, derived from `CARGO_MANIFEST_DIR` (this package is
/// `workspace/index`, two levels below the root).
pub fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
}

/// The real, checked-in corpus manifest — the exact source
/// `corpus/fetch.nu` reads. Tests key off this file (not off re-derived
/// directory-name guesses) so every corpus fact this suite relies on is
/// exactly the fact the fetch step itself relied on.
pub fn corpus_manifest_path() -> std::path::PathBuf {
    repo_root().join("corpus/manifest.toml")
}

/// Where `corpus/fetch.nu` materializes fixtures.
pub fn real_crates_root() -> std::path::PathBuf {
    repo_root().join(".real-crates")
}

/// One `[[packages]]` entry of `corpus/manifest.toml`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestPackage {
    pub ecosystem: String,
    pub name: String,
    pub versions: Vec<ManifestVersion>,
}

/// One entry of a manifest package's `versions` array.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestVersion {
    pub version: String,
    #[allow(dead_code)]
    pub hash: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ManifestFile {
    packages: Vec<ManifestPackage>,
}

/// Parse the real corpus manifest (doctrine §4: real data, not a fixture this
/// suite authored). Panics on a missing/malformed manifest — that is a repo
/// setup defect, not a soft-skippable test outcome.
pub fn load_corpus_manifest() -> Vec<ManifestPackage> {
    let path = corpus_manifest_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let parsed: ManifestFile = toml::from_str(&text)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    parsed.packages
}

/// Filesystem-safe directory name, mirroring `corpus/fetch.nu`'s
/// `safe-dir-name` byte-for-byte (`/` and `:` both become `__`), so this
/// resolves exactly the directories the fetch script wrote.
pub fn safe_dir_name(name: &str, version: &str) -> String {
    format!(
        "{}-{version}",
        name.replace('/', "__").replace(':', "__")
    )
}

/// One real, on-disk fixture: which package/version the manifest says it is,
/// and where its checkout lives.
#[derive(Debug, Clone)]
pub struct CorpusFixture {
    pub ecosystem: String,
    pub name: String,
    pub version: String,
    pub path: std::path::PathBuf,
}

/// Every manifest-listed `(ecosystem, name, version)` whose fixture directory
/// actually exists on disk, in stable `(ecosystem, name, version)` order.
///
/// `corpus/manifest.toml` may list more than any one checkout has fetched
/// (L9's Nix-reproducible fetch is opt-in); silently treating "listed" as
/// "present" would misreport corpus size, so this filters to what is actually
/// there and callers can report `.len()` against the manifest's own total.
pub fn available_corpus_fixtures() -> Vec<CorpusFixture> {
    let root = real_crates_root();
    let mut out: Vec<CorpusFixture> = load_corpus_manifest()
        .into_iter()
        .flat_map(|package| {
            let ecosystem = package.ecosystem.clone();
            let name = package.name.clone();
            let root = root.clone();
            package.versions.into_iter().filter_map(move |version| {
                let dir_name = safe_dir_name(&name, &version.version);
                let path = root.join(&dir_name);
                path.is_dir().then_some(CorpusFixture {
                    ecosystem: ecosystem.clone(),
                    name: name.clone(),
                    version: version.version,
                    path,
                })
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (&a.ecosystem, &a.name, &a.version).cmp(&(&b.ecosystem, &b.name, &b.version))
    });
    out
}

/// Map a manifest ecosystem token to the catalog's `heart::Language`. `None`
/// for a token this harness does not (yet) map — callers skip rather than
/// guess.
pub fn language_for_ecosystem(ecosystem: &str) -> Option<heart::Language> {
    match ecosystem {
        "crates.io" => Some(heart::Language::Rust),
        "npm" => Some(heart::Language::Typescript),
        "pypi" => Some(heart::Language::Python),
        "go" => Some(heart::Language::Go),
        "maven" => Some(heart::Language::Java),
        "nuget" => Some(heart::Language::CSharp),
        "cpp" => Some(heart::Language::Cpp),
        _ => None,
    }
}

/// Open a fresh, migrated catalog writer whose bytes live at `path`, so storage
/// tests have something real to measure.
///
/// Under the default feature set this is the product's engine — a
/// content-addressed prolly-tree store that also writes commit history — so its
/// `disk_delta_bytes` is the catalog's actual storage cost, not an analogue of
/// it. That is a change: this helper used to be pinned to
/// `MemoryEngine::open_at_path`, whose bytes were real *SQLite* bytes for a
/// storage engine the product does not use. Every figure taken through this
/// helper before 2026-08-08 describes stock SQLite, and they are not comparable
/// to the ones taken after.
pub fn migrated_disk_writer(path: &std::path::Path) -> CatalogWriter<Configured> {
    let engine = <Configured as OpenCatalog>::open_at_path(path).expect("open on-disk engine");
    migrate_to_v4(&engine).expect("migrate to schema v4");
    CatalogWriter::new(engine)
}

/// A fake git remote whose responses are scripted per URL. Used by ingest tests.
pub struct FakeGitRepository {
    ls_remote: BTreeMap<String, Result<Vec<u8>, String>>,
    head: BTreeMap<String, Result<Option<String>, String>>,
    pub calls: AtomicUsize,
}

impl FakeGitRepository {
    pub fn new() -> Self {
        Self {
            ls_remote: BTreeMap::new(),
            head: BTreeMap::new(),
            calls: AtomicUsize::new(0),
        }
    }

    pub fn with_ls_remote(mut self, url: &str, bytes: &[u8]) -> Self {
        self.ls_remote.insert(url.to_owned(), Ok(bytes.to_vec()));
        self
    }

    pub fn with_ls_remote_error(mut self, url: &str, message: &str) -> Self {
        self.ls_remote
            .insert(url.to_owned(), Err(message.to_owned()));
        self
    }

    pub fn with_head(mut self, url: &str, oid: Option<&str>) -> Self {
        self.head.insert(url.to_owned(), Ok(oid.map(str::to_owned)));
        self
    }
}

fn fake_err(url: &str, message: &str) -> GitRepositoryError {
    GitRepositoryError::CommandFailed {
        url: url.to_owned(),
        status: 128,
        stderr: message.to_owned(),
    }
}

impl GitRepository for FakeGitRepository {
    fn ls_remote_bytes(&self, url: &str) -> Result<Vec<u8>, GitRepositoryError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        match self.ls_remote.get(url) {
            Some(Ok(bytes)) => Ok(bytes.clone()),
            Some(Err(message)) => Err(fake_err(url, message)),
            None => Err(fake_err(url, "no scripted ls-remote")),
        }
    }

    fn list_remote_refs(&self, url: &str) -> Result<Vec<LsRemoteRef>, GitRepositoryError> {
        let bytes = self.ls_remote_bytes(url)?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| GitRepositoryError::MalformedOutput {
                url: url.to_owned(),
                detail: "utf8".into(),
            })?;
        let mut refs = Vec::new();
        for line in text.split('\n') {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (oid, reference) =
                line.split_once('\t')
                    .ok_or_else(|| GitRepositoryError::MalformedOutput {
                        url: url.to_owned(),
                        detail: "no tab".into(),
                    })?;
            refs.push(LsRemoteRef {
                object_id: oid.trim().to_owned(),
                reference: reference.trim().to_owned(),
            });
        }
        Ok(refs)
    }

    fn head_object_id(&self, url: &str) -> Result<Option<String>, GitRepositoryError> {
        match self.head.get(url) {
            Some(Ok(oid)) => Ok(oid.clone()),
            Some(Err(message)) => Err(fake_err(url, message)),
            None => Ok(None),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Real, on-disk git repositories (doctrine §4: real inputs, not fixtures)
// ─────────────────────────────────────────────────────────────────────────────
//
// `ingest_enumerate.rs` proves enumeration/monitoring against a real
// `git init` fixture in isolation. The end-to-end pipeline test additionally
// needs repositories that *grow* — a fresh tag/release lands on an existing
// repo mid-test — and that scale to N releases for throughput measurement, so
// the builder lives here rather than being copied a third time.

/// Run `git` in `dir` with a deterministic, hermetic identity: arg-vector
/// only (no shell), global/system config disabled. Shared by every ingest
/// test that drives a *real* on-disk git repository.
pub fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// Append one real, non-trivial line to `root/CHANGELOG` and commit it. Real
/// content (not an empty commit) so the repository's on-disk size is a
/// genuine function of how many releases it has had, not a fixed-size
/// fixture reused N times.
fn commit_one_release(root: &Path, index: usize, salt: &str) {
    let line = format!("release {index} ({salt}): {}\n", "x".repeat(64));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("CHANGELOG"))
        .expect("open CHANGELOG");
    file.write_all(line.as_bytes()).expect("append CHANGELOG");
    drop(file);
    git(root, &["add", "CHANGELOG"]);
    git(root, &["commit", "-q", "-m", &format!("release {index}")]);
}

/// Initialize a real git repository at `root` (must already exist and be
/// empty) with `n` sequential commits, each lightweight-tagged `v1.0.<i>`.
/// Returns the tags in commit order. Lightweight tags already proven (in
/// `ingest_enumerate.rs`) to yield a full 40-hex peeled oid through both real
/// adapters, so this stays representative without needing `-a`.
pub fn init_repo_with_tags(root: &Path, n: usize) -> Vec<String> {
    git(root, &["init", "-q", "-b", "main"]);
    let mut tags = Vec::with_capacity(n);
    for i in 0..n {
        commit_one_release(root, i, "init");
        let tag = format!("v1.0.{i}");
        git(root, &["tag", &tag]);
        tags.push(tag);
    }
    tags
}

/// Push one more real, tagged commit onto a repository already built by
/// [`init_repo_with_tags`] — simulating a genuine new upstream release
/// landing between two polls. Returns the new tag's canonical string.
pub fn push_one_more_tagged_commit(root: &Path, next_index: usize) -> String {
    commit_one_release(root, next_index, "followup");
    let tag = format!("v1.0.{next_index}");
    git(root, &["tag", &tag]);
    tag
}
