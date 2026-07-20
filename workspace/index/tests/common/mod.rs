//! Shared test harness: build a migrated in-memory catalog writer, plus a
//! programmable fake git repository for ingest tests.
//!
//! Every integration test opens a [`MemoryEngine`], migrates it to schema v4,
//! and wraps it in a [`CatalogWriter`]. The memory engine is the test-only
//! facade fake (never a product mode); versioning calls run against its honest
//! recorded commit log.
//!
//! Not every test binary uses every helper, so allow dead code here.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use index::engine::memory::MemoryEngine;
use index::ingest::git::{GitRepository, GitRepositoryError, LsRemoteRef};
use index::migrations::runner::migrate_to_v4;
use index::store::writer::CatalogWriter;

/// Open a fresh, migrated in-memory catalog writer.
pub fn migrated_writer() -> CatalogWriter<MemoryEngine> {
    let engine = MemoryEngine::open_in_memory().expect("open in-memory engine");
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

/// A fake git remote whose responses are scripted per URL. Used by ingest tests.
pub struct FakeGitRepository {
    ls_remote: BTreeMap<String, Result<Vec<u8>, String>>,
    head: BTreeMap<String, Result<Option<String>, String>>,
    pub calls: AtomicUsize,
}

impl FakeGitRepository {
    pub fn new() -> Self {
        Self { ls_remote: BTreeMap::new(), head: BTreeMap::new(), calls: AtomicUsize::new(0) }
    }

    pub fn with_ls_remote(mut self, url: &str, bytes: &[u8]) -> Self {
        self.ls_remote.insert(url.to_owned(), Ok(bytes.to_vec()));
        self
    }

    pub fn with_ls_remote_error(mut self, url: &str, message: &str) -> Self {
        self.ls_remote.insert(url.to_owned(), Err(message.to_owned()));
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
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            GitRepositoryError::MalformedOutput { url: url.to_owned(), detail: "utf8".into() }
        })?;
        let mut refs = Vec::new();
        for line in text.split('\n') {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (oid, reference) = line.split_once('\t').ok_or_else(|| {
                GitRepositoryError::MalformedOutput { url: url.to_owned(), detail: "no tab".into() }
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
