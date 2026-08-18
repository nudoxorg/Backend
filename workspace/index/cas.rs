//! The content-addressed object store — the registry's durable, immutable blob
//! layer over `object_store` (S3 / GCS / Azure / local filesystem).
//!
//! ## Layout
//! Two key spaces, both derived — never opaque refs:
//! - **`cas/{blake3-hex}`** — immutable, content-addressed file and section
//!   bytes. Keyed purely by hash, so identical content across packages and
//!   versions is stored exactly once (dedupe) and every read is integrity-
//!   checkable.
//! - **`ptr/{package-id}`** — a small mutable pointer from a package's
//!   deterministic [`PackageId`] (itself a UUIDv5 fingerprint of the validated
//!   coordinate tuple) to its current manifest's [`ContentHash`]. Keying on the
//!   id rather than raw name segments means a scoped name like `@types/node` or
//!   a version with slashes can never break the key layout or escape its
//!   prefix — the leaf is always a fixed-shape UUID.
//!
//! ## Idempotency
//! A `cas/` put of content whose hash already exists is a no-op — puts are
//! idempotent by construction, which is what makes blob emission safely
//! retryable.

use std::sync::Arc;

use crate::package::Coordinates as PackageCoordinates;
use futures::TryStreamExt as _;
use heart::{
    BackendKind, Cold, Connect, ConnectError, ConnectFailure, Live, PackageId, Probeable,
    content::ContentHash, timed_probe,
};
use object_store::{ObjectStore, path::Path};

use crate::{
    blob::{BlobManifest, creation::PendingSection},
    error::{BlobError, StoreError, hash_hex, verify_integrity},
};

/// The registry's content-addressed store over object storage.
///
/// `S` is the connection typestate: [`Cold`] until [`Connect::connect`] proves
/// the bucket is reachable, then [`Live`]. Read/write methods live only on the
/// `Live` form.
pub struct Store<S = Live> {
    backend: Arc<dyn ObjectStore>,
    _state: std::marker::PhantomData<S>,
}

impl<S> Clone for Store<S> {
    /// Clone shares the underlying `Arc<dyn ObjectStore>` backend; no
    /// deep copy is performed.
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            _state: std::marker::PhantomData,
        }
    }
}

impl Store<Cold> {
    /// Wrap an object-store backend without verifying reachability. The choice of
    /// backend (`AmazonS3`, `GoogleCloudStorage`, `LocalFileSystem`, ...) is where
    /// self-hosting / per-source federation plugs in.
    pub fn new(backend: Arc<dyn ObjectStore>) -> Self {
        Self {
            backend,
            _state: std::marker::PhantomData,
        }
    }
}

impl Connect for Store<Cold> {
    type Live = Store<Live>;

    /// A "connection" is a bucket-reachability check: HEAD a sentinel key and
    /// confirm credentials + network before going [`Live`]. A `NotFound` on the
    /// sentinel is *success* — the bucket answered.
    async fn connect(self) -> Result<Self::Live, ConnectError> {
        let sentinel = Path::from("cas/.reachability-probe");
        match self.backend.head(&sentinel).await {
            Ok(_) | Err(object_store::Error::NotFound { .. }) => {
                tracing::debug!("object store answered the sentinel probe; going live");
                Ok(Store {
                    backend: self.backend,
                    _state: std::marker::PhantomData,
                })
            }
            Err(
                object_store::Error::Unauthenticated { .. }
                | object_store::Error::PermissionDenied { .. },
            ) => Err(ConnectError::new(
                BackendKind::ObjectStore,
                ConnectFailure::Auth,
            )),
            Err(other) => Err(ConnectError::new(
                BackendKind::ObjectStore,
                ConnectFailure::Io(std::io::Error::other(other)),
            )),
        }
    }
}

/// Map a raw backend error on `path` onto the store's error vocabulary,
/// folding the backend's `NotFound` into the rich typed [`StoreError::NotFound`]
/// variant (path preserved; source discarded — absence is the signal).
fn keyed(path: &Path, error: object_store::Error) -> StoreError {
    match &error {
        object_store::Error::NotFound { .. } => StoreError::NotFound { path: path.clone() },
        object_store::Error::PermissionDenied { .. } => StoreError::ObjectStorePermissionDenied {
            path: Some(path.clone()),
            source: error,
        },
        object_store::Error::Unauthenticated { .. } => {
            StoreError::ObjectStoreUnauthenticated { source: error }
        }
        object_store::Error::Precondition { .. } => StoreError::ObjectStorePreconditionFailed {
            path: path.clone(),
            source: error,
        },
        object_store::Error::AlreadyExists { .. } => StoreError::ObjectStoreAlreadyExists {
            path: path.clone(),
            source: error,
        },
        object_store::Error::Generic { store, .. } => StoreError::ObjectStoreGeneric {
            store,
            source: error,
        },
        object_store::Error::JoinError { .. } => StoreError::ObjectStoreJoin { source: error },
        _ => StoreError::Backend(error),
    }
}

impl Store<Live> {
    /// The `cas/{hash}` object path for a content hash.
    pub fn cas_path(hash: ContentHash) -> Path {
        Path::from(format!("cas/{}", hash_hex(&hash)))
    }

    /// The shared object-store backend handle, so sibling stores over the same
    /// namespace (e.g. [`crate::compiled::ObjectCompiledStore`]) can be built
    /// without threading a second `Arc` through configuration.
    pub fn backend(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.backend)
    }

    /// The `ptr/{package-id}` pointer path for a package's coordinates. The leaf
    /// is the deterministic [`PackageId`] — a fixed-shape UUID derived from the
    /// validated coordinate tuple — so no name or version, however exotic, can
    /// break the layout.
    pub fn pointer_path(coordinates: &PackageCoordinates) -> Path {
        Self::pointer_path_for(coordinates.id())
    }

    /// The pointer path from a bare [`PackageId`] — what [`Store::put_manifest`]
    /// (which only carries the id) writes and what
    /// [`Store::pointer_path`] delegates to, so the two can never diverge.
    fn pointer_path_for(package: PackageId) -> Path {
        Path::from(format!("ptr/{}", package.as_uuid()))
    }

    /// Verify a section's bytes against its declared key, in `cas/{hash}` —
    /// there is exactly one namespace and every object in it is checked,
    /// never skipped.
    ///
    /// Two addressing schemes currently produce objects in that namespace,
    /// so this accepts either:
    ///
    /// - the ordinary physical scheme, `hash == ContentHash::of_bytes(bytes)`
    ///   — every section except generation-root entry payloads (files,
    ///   `ir_ref`, `references_ref`, the `GenerationRoot`'s own encoded
    ///   bytes); or
    /// - the entry-payload scheme
    ///   ([`crate::blob::BlobBuilder::set_generation_root`]): `hash` is
    ///   `ir::content::entry_storage_hash` of the semantic `Entry` the bytes
    ///   decode to — a domain-separated hash over a narrower, position-free
    ///   preimage than the bytes themselves (it excludes `sym.source`/
    ///   `sym.span`; see `ir::generation`'s module doc, "Moved is not
    ///   changed"), so it cannot be recomputed by rehashing the bytes
    ///   directly. It is instead recomputed the *same way the addressing
    ///   itself works*: decode the payload back into an `Entry` (the wire
    ///   format `set_generation_root`'s payloads commit to — JSON) and ask
    ///   `ir` for its storage hash.
    ///
    /// A section satisfying neither is genuinely corrupt or mis-addressed:
    /// the physical-scheme mismatch is what's returned in that case, since
    /// that is the scheme every pre-P3 section still uses.
    fn verify_section_integrity(
        path: &Path,
        bytes: &bytes::Bytes,
        hash: ContentHash,
    ) -> Result<(), StoreError> {
        if ContentHash::of_bytes(bytes) == hash {
            return Ok(());
        }
        if let Ok(entry) = serde_json::from_slice::<ir::entry::Entry>(bytes) {
            let recomputed =
                ContentHash::from_bytes(*ir::content::entry_storage_hash(&entry).as_bytes());
            if recomputed == hash {
                return Ok(());
            }
        }
        verify_integrity(path.clone(), bytes, hash)
    }

    /// Idempotently store one content-addressed section. If an object already
    /// exists at `cas/{section.hash}`, this is a no-op (returns `false`);
    /// otherwise it writes and returns `true`. `// object-store put may run on
    /// spawn_blocking depending on backend`.
    pub async fn put_section(&self, section: &PendingSection) -> Result<bool, StoreError> {
        let path = Self::cas_path(section.hash);
        // Refuse to persist bytes that do not hash to their declared key — a
        // mis-addressed put would poison the CAS for every future reader.
        Self::verify_section_integrity(&path, &section.bytes, section.hash)?;
        match self.backend.head(&path).await {
            Ok(_) => Ok(false),
            Err(object_store::Error::NotFound { .. }) => {
                self.backend
                    .put(&path, section.bytes.clone().into())
                    .await?;
                tracing::debug!(key = %path, size = section.bytes.len(), "cas section written");
                Ok(true)
            }
            Err(other) => Err(keyed(&path, other)),
        }
    }

    /// Fetch a content-addressed section's bytes, verifying they hash back to
    /// the key. `// blake3 verification runs on spawn_blocking`.
    pub async fn get_section(&self, hash: ContentHash) -> Result<bytes::Bytes, StoreError> {
        let path = Self::cas_path(hash);
        let result = self.backend.get(&path).await.map_err(|e| keyed(&path, e))?;
        let bytes = result.bytes().await.map_err(|e| keyed(&path, e))?;
        Self::verify_section_integrity(&path, &bytes, hash)?;
        Ok(bytes)
    }

    /// Fetch a ranged slice of a content-addressed section (for large files a
    /// caller only partially needs). A partial read cannot be integrity-verified
    /// against the whole-object hash; callers trade verification for bandwidth.
    pub async fn get_section_range(
        &self,
        hash: ContentHash,
        range: std::ops::Range<u64>,
    ) -> Result<bytes::Bytes, StoreError> {
        let path = Self::cas_path(hash);
        self.backend
            .get_range(&path, range.start as usize..range.end as usize)
            .await
            .map_err(|e| keyed(&path, e))
    }

    /// Record a manifest: write it as its own `cas/` object and repoint the
    /// package's `ptr/` to it. Threaded through the access layer.
    #[tracing::instrument(skip(self, manifest), fields(package = %manifest.package))]
    pub async fn put_manifest(&self, manifest: &BlobManifest) -> Result<ContentHash, StoreError> {
        manifest
            .validate()
            .map_err(|e| StoreError::Blob(Box::new(e)))?;
        // Use the single canonical CAS-key derivation (Hash ②) so this site and
        // any future reader share exactly one encoding path.  See the two-hash
        // doc comment in `blob/mod.rs` for why this is distinct from the
        // generation-stamp (Hash ①) produced by `identity_bytes`.
        let hash = manifest
            .manifest_cas_key()
            .map_err(|e| StoreError::Blob(Box::new(e)))?;
        let bytes = postcard::to_allocvec(manifest)
            .map_err(|e| StoreError::Blob(Box::new(BlobError::Codec(e))))?;

        // The manifest itself is content-addressed (idempotent put)...
        self.put_section(&PendingSection {
            hash,
            bytes: bytes.into(),
        })
        .await?;

        // ...and the pointer repoint is a single-object replace, which every
        // object-store backend performs atomically.
        let pointer = Self::pointer_path_for(manifest.package);
        self.backend
            .put(
                &pointer,
                bytes::Bytes::copy_from_slice(hash.as_bytes()).into(),
            )
            .await?;
        tracing::info!(manifest = %hash_hex(&hash), "manifest recorded and pointer repointed");
        Ok(hash)
    }

    /// Load the current manifest for a package by resolving its `ptr/` to a
    /// manifest hash and fetching it.
    pub async fn get_manifest(
        &self,
        package: &PackageCoordinates,
    ) -> Result<BlobManifest, StoreError> {
        let pointer = Self::pointer_path(package);
        let result = self
            .backend
            .get(&pointer)
            .await
            .map_err(|e| keyed(&pointer, e))?;
        let raw = result.bytes().await.map_err(|e| keyed(&pointer, e))?;
        let digest: [u8; 32] =
            raw.as_ref()
                .try_into()
                .map_err(|_| StoreError::InvalidPointerLength {
                    path: pointer.clone(),
                    len: raw.len(),
                })?;

        let bytes = self.get_section(ContentHash::from_bytes(digest)).await?;
        let manifest: BlobManifest = postcard::from_bytes(&bytes)
            .map_err(|e| StoreError::Blob(Box::new(BlobError::Codec(e))))?;
        manifest
            .validate()
            .map_err(|e| StoreError::Blob(Box::new(e)))?;
        Ok(manifest)
    }

    /// Whether a package currently has a stored manifest.
    pub async fn exists(&self, package: &PackageCoordinates) -> Result<bool, StoreError> {
        let pointer = Self::pointer_path(package);
        match self.backend.head(&pointer).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(other) => Err(keyed(&pointer, other)),
        }
    }

    /// Enumerate every content-addressed section currently held in the `cas/`
    /// key space.
    ///
    /// Returns the set of [`ContentHash`]es whose `cas/{hex}` objects exist in
    /// the backend. Keys that do not parse as 32-byte BLAKE3 hex (e.g. the
    /// reachability-probe sentinel) are silently skipped — they are not CAS
    /// sections.
    ///
    /// This is a read-only enumeration. It is used by audit / orphan-detection
    /// paths to compute which stored blobs no live manifest references; it does
    /// **not** delete anything.
    ///
    /// # Pagination
    /// `object_store::ObjectStore::list` returns a stream that the backend paginates
    /// internally (S3 list pages, GCS list pages, local readdir batches). We drive
    /// the stream to completion with `try_collect`, so all pages are consumed and
    /// the result is the complete key set.
    pub async fn list_cas(&self) -> Result<Vec<ContentHash>, StoreError> {
        let cas_prefix = Path::from("cas");
        let metas: Vec<object_store::ObjectMeta> =
            self.backend.list(Some(&cas_prefix)).try_collect().await?;

        let mut hashes = Vec::with_capacity(metas.len());
        for meta in metas {
            // Strip the "cas/" prefix to get the hex leaf.
            let path_str = meta.location.as_ref();
            let Some(hex) = path_str.strip_prefix("cas/") else {
                continue; // Shouldn't happen, but skip malformed keys.
            };
            // Skip the sentinel and any non-CAS entries (not 64 hex chars = 32 bytes).
            if hex.len() != 64 {
                continue;
            }
            let mut raw = [0u8; 32];
            if data_encoding::HEXLOWER
                .decode_mut(hex.as_bytes(), &mut raw)
                .is_err()
            {
                tracing::debug!(key = path_str, "skipping non-hex cas key during list");
                continue;
            }
            hashes.push(ContentHash::from_bytes(raw));
        }
        Ok(hashes)
    }

    /// Resolve a package's coordinates to its deterministic id (pure delegation
    /// to heart; kept here so callers don't re-derive the layout key twice).
    pub fn id_of(package: &PackageCoordinates) -> PackageId {
        package.id()
    }
}

impl Probeable for Store<Live> {
    fn backend(&self) -> BackendKind {
        BackendKind::ObjectStore
    }

    async fn probe(&self) -> heart::Probe {
        timed_probe(BackendKind::ObjectStore, async {
            let sentinel = ContentHash::of_bytes(b"nudox readiness sentinel");
            match self.get_section(sentinel).await {
                Ok(_) | Err(StoreError::NotFound { .. }) => None,
                Err(error) => Some(error.to_string()),
            }
        })
        .await
    }
}

const _: fn() = || {
    heart::assert_probe_future_send::<Store<Live>>();
};
