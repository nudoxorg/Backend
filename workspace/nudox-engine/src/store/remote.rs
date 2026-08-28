//! Durable shared package and IR objects.
//!
//! The local archive cache is intentionally an acquisition cache.  This
//! module is the next boundary: immutable package bytes and serialized IR are
//! addressed by BLAKE3 and can be published to, then replayed from, a shared
//! HTTP CAS.  The local [`heart::cache::DiskCas`] remains the durable read
//! through, so a later process can replay without the checkout or the remote.

use std::path::PathBuf;

use bytes::Bytes;
use heart::{
    ContentHash,
    cache::{Cas, CasError, DiskCas},
};
use nudox_ir::{
    apply::PristineIntroTable,
    change::{IntroId, PackageLineageId},
    entry::Entry,
    view::IrView,
};
use serde::{Deserialize, Serialize};

/// Which immutable object is being exchanged.  The type is metadata for
/// callers and a stable URL namespace for mirrors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Package,
    Ir,
}

impl ObjectKind {
    fn path(self) -> &'static str {
        match self {
            Self::Package => "packages",
            Self::Ir => "ir",
        }
    }
}

/// A replayable declaration-plane snapshot.
///
/// Derived indexes are deliberately absent: they are rebuilt by
/// `PackageView::build` after verification, so a remote object cannot smuggle
/// stale projections past the content address.
///
/// The wire encoding is postcard, not JSON: `nudox_ir::entry::Entry` has zero
/// `#[serde(skip_serializing_if)]` fields, so nothing is silently omitted from
/// the byte-for-byte layout postcard requires, and on this payload postcard
/// runs roughly 60% smaller and 6x faster than `serde_json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IrSnapshot {
    pub package: PackageLineageId,
    pub declarations: Vec<(IntroId, Entry, Option<IntroId>)>,
}

impl IrSnapshot {
    pub fn from_view(view: &IrView) -> Self {
        Self {
            package: view.package().clone(),
            declarations: view
                .entries_sorted()
                .map(|(intro, entry)| (intro, entry.clone(), view.parent_of(intro)))
                .collect(),
        }
    }

    pub fn replay(self) -> IrView {
        let mut table = PristineIntroTable::new();
        for (intro, entry, parent) in self.declarations {
            table.insert_live(intro, entry, parent);
        }
        IrView::with_package(self.package, table)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteStoreError {
    #[error("local CAS error: {0}")]
    Cas(#[from] CasError),
    #[error("remote CAS request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("remote CAS returned HTTP status {0}")]
    Status(reqwest::StatusCode),
    #[error("remote object {kind:?} has digest {actual}, expected {expected}")]
    Integrity {
        kind: ObjectKind,
        actual: ContentHash,
        expected: ContentHash,
    },
    #[error("remote IR snapshot could not be decoded: {0}")]
    Decode(#[from] postcard::Error),
}

/// A shared HTTP-backed CAS with a durable local read-through.
#[derive(Clone, Debug)]
pub struct RemoteStore {
    local: DiskCas,
    base: String,
    client: reqwest::Client,
}

impl RemoteStore {
    pub fn open(root: impl Into<PathBuf>, base: &str) -> Result<Self, RemoteStoreError> {
        Ok(Self {
            local: DiskCas::open(root)?,
            base: base.trim_end_matches('/').to_owned(),
            client: reqwest::Client::new(),
        })
    }

    pub fn local(&self) -> &DiskCas {
        &self.local
    }

    pub async fn publish(
        &self,
        kind: ObjectKind,
        bytes: impl Into<Bytes>,
    ) -> Result<ContentHash, RemoteStoreError> {
        let bytes = bytes.into();
        let hash = ContentHash::of_bytes(&bytes);
        self.local.put_keyed(hash, bytes.clone()).await?;
        let response = self
            .client
            .put(self.url(kind, hash))
            .body(bytes)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(RemoteStoreError::Status(response.status()));
        }
        Ok(hash)
    }

    pub async fn fetch(
        &self,
        kind: ObjectKind,
        hash: ContentHash,
    ) -> Result<Bytes, RemoteStoreError> {
        if let Some(bytes) = self.local.get(hash).await? {
            return Ok(bytes);
        }
        let response = self.client.get(self.url(kind, hash)).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RemoteStoreError::Status(response.status()));
        }
        let response = response.error_for_status()?;
        let bytes = response.bytes().await?;
        let actual = ContentHash::of_bytes(&bytes);
        if actual != hash {
            return Err(RemoteStoreError::Integrity {
                kind,
                actual,
                expected: hash,
            });
        }
        self.local.put_keyed(hash, bytes.clone()).await?;
        Ok(bytes)
    }

    pub async fn fetch_package(&self, hash: ContentHash) -> Result<Bytes, RemoteStoreError> {
        self.fetch(ObjectKind::Package, hash).await
    }

    pub async fn publish_package(
        &self,
        bytes: impl Into<Bytes>,
    ) -> Result<ContentHash, RemoteStoreError> {
        self.publish(ObjectKind::Package, bytes).await
    }

    pub async fn fetch_ir(&self, hash: ContentHash) -> Result<IrSnapshot, RemoteStoreError> {
        let bytes = self.fetch(ObjectKind::Ir, hash).await?;
        Ok(postcard::from_bytes(&bytes)?)
    }

    pub async fn replay_ir(&self, hash: ContentHash) -> Result<IrView, RemoteStoreError> {
        Ok(self.fetch_ir(hash).await?.replay())
    }

    pub async fn publish_ir(&self, snapshot: &IrSnapshot) -> Result<ContentHash, RemoteStoreError> {
        self
            .publish(ObjectKind::Ir, postcard::to_allocvec(snapshot)?)
            .await
    }

    fn url(&self, kind: ObjectKind, hash: ContentHash) -> String {
        format!("{}/{}/{}", self.base, kind.path(), hash)
    }
}
