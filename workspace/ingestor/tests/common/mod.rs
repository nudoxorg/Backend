//! Shared test harness for the ingestor: a migrated in-memory catalog writer, a
//! programmable fake [`GitRepository`], and small assertion helpers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use index::engine::memory::MemoryEngine;
use index::migrations::runner::migrate_to_v4;
use index::store::writer::CatalogWriter;

use ingestor::git::{GitRepository, GitRepositoryError, LsRemoteRef};

/// A fresh, migrated in-memory catalog writer (index's test-engine facade).
pub fn migrated_writer() -> CatalogWriter<MemoryEngine> {
    let engine = MemoryEngine::open_in_memory().expect("open in-memory engine");
    migrate_to_v4(&engine).expect("migrate to schema v4");
    CatalogWriter::new(engine)
}

/// A fake git remote whose responses are scripted per URL. Not thread-safe by
/// design (single-threaded tests); wraps its tables in `RefCell` so the trait's
/// `&self` methods can record call counts.
pub struct FakeGitRepository {
    ls_remote: BTreeMap<String, Result<Vec<u8>, String>>,
    head: BTreeMap<String, Result<Option<String>, String>>,
    pub calls: AtomicUsize,
}

impl FakeGitRepository {
    pub fn new() -> Self {
        Self { ls_remote: BTreeMap::new(), head: BTreeMap::new(), calls: AtomicUsize::new(0) }
    }

    /// Script `ls-remote` bytes for a URL.
    pub fn with_ls_remote(mut self, url: &str, bytes: &[u8]) -> Self {
        self.ls_remote.insert(url.to_owned(), Ok(bytes.to_vec()));
        self
    }

    /// Script an `ls-remote` failure for a URL (nonexistent repo / timeout).
    pub fn with_ls_remote_error(mut self, url: &str, message: &str) -> Self {
        self.ls_remote.insert(url.to_owned(), Err(message.to_owned()));
        self
    }

    /// Script the HEAD oid for a URL.
    pub fn with_head(mut self, url: &str, oid: Option<&str>) -> Self {
        self.head.insert(url.to_owned(), Ok(oid.map(str::to_owned)));
        self
    }
}

fn err(url: &str, message: &str) -> GitRepositoryError {
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
            Some(Err(message)) => Err(err(url, message)),
            None => Err(err(url, "no scripted ls-remote")),
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
            refs.push(LsRemoteRef { object_id: oid.trim().to_owned(), reference: reference.trim().to_owned() });
        }
        Ok(refs)
    }

    fn head_object_id(&self, url: &str) -> Result<Option<String>, GitRepositoryError> {
        match self.head.get(url) {
            Some(Ok(oid)) => Ok(oid.clone()),
            Some(Err(message)) => Err(err(url, message)),
            None => Ok(None),
        }
    }
}
