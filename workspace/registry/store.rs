//! The content-addressed object store — the registry's durable, immutable blob
//! layer over `object_store` (S3 / GCS / Azure / local filesystem).
//!
//! ## Layout
//! Two key spaces, both derived — never opaque refs:
//! - **`cas/{blake3-hex}`** — immutable, content-addressed file and section
//!   bytes. Keyed purely by hash, so identical content across packages and
//!   versions is stored exactly once (dedupe) and every read is integrity-
//!   checkable.
//! - **`ptr/{ecosystem}/{origin}/{name}/{version}`** — a small mutable pointer
//!   from a package's coordinates to its current manifest's [`ContentHash`].
//!   Names are percent-encoded *and* the leaf is hashed so a scoped name like
//!   `@types/node` or a version with slashes can never break the key layout or
//!   escape its prefix.
//!
//! ## Idempotency
//! A `cas/` put of content whose hash already exists is a no-op — puts are
//! idempotent by construction, which is what makes blob emission safely
//! retryable.

use std::sync::Arc;

use heart::{
	Cold, Connect, ConnectError, Live,
	access::AccessContext,
	content::ContentHash,
	identity::{PackageCoordinates, PackageId},
};
use object_store::{ObjectStore, path::Path};

use crate::{
	blob::{BlobManifest, creation::PendingSection},
	error::StoreError,
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

impl Store<Cold> {
	/// Wrap an object-store backend without verifying reachability. The choice of
	/// backend (`AmazonS3`, `GoogleCloudStorage`, `LocalFileSystem`, ...) is where
	/// self-hosting / per-source federation plugs in.
	pub fn new(backend: Arc<dyn ObjectStore>) -> Self {
		Self { backend, _state: std::marker::PhantomData }
	}
}

impl Connect for Store<Cold> {
	type Live = Store<Live>;

	/// A "connection" is a bucket-reachability check: HEAD/list a sentinel key
	/// and confirm credentials + network before going [`Live`].
	async fn connect(self) -> Result<Self::Live, ConnectError> {
		let _ = &self.backend;
		todo!("probe the bucket (list a sentinel prefix); map failures to ConnectError::ObjectStore")
	}
}

impl Store<Live> {
	/// The `cas/{hash}` object path for a content hash.
	pub fn cas_path(hash: ContentHash) -> Path {
		let _ = hash;
		todo!("build a Path of the form cas/<blake3-hex> from hash.to_hex()")
	}

	/// The `ptr/...` pointer path for a package's coordinates. Percent-encodes +
	/// hashes the name/version leaf so scoped names can't break the layout.
	pub fn pointer_path(coordinates: &PackageCoordinates) -> Path {
		let _ = coordinates;
		todo!("build ptr/<ecosystem>/<origin-token>/<pct-name>/<pct-version>, leaf hashed for safety")
	}

	/// Idempotently store one content-addressed section. If an object already
	/// exists at `cas/{section.hash}`, this is a no-op (returns `false`);
	/// otherwise it writes and returns `true`. `// object-store put may run on
	/// spawn_blocking depending on backend`.
	pub async fn put_section(&self, section: &PendingSection) -> Result<bool, StoreError> {
		let _ = (&self.backend, section);
		todo!("HEAD the cas section; if absent, put bytes; verify no hash collision")
	}

	/// Fetch a content-addressed section's bytes, verifying they hash back to the
	/// key. `// blake3 verification runs on spawn_blocking`.
	pub async fn get_section(&self, hash: ContentHash) -> Result<bytes::Bytes, StoreError> {
		let _ = (&self.backend, hash);
		todo!("get the cas section, verify_integrity, return bytes")
	}

	/// Fetch a ranged slice of a content-addressed section (for large files a
	/// caller only partially needs).
	pub async fn get_section_range(
		&self,
		hash: ContentHash,
		range: std::ops::Range<u64>,
	) -> Result<bytes::Bytes, StoreError> {
		let _ = (&self.backend, hash, range);
		todo!("object_store get_range on the cas section; note: ranged reads can't full-verify")
	}

	/// Record a manifest: write it as its own `cas/` object and repoint the
	/// package's `ptr/` to it. Threaded through the access layer.
	pub async fn put_manifest(
		&self,
		ctx: &AccessContext,
		manifest: &BlobManifest,
	) -> Result<ContentHash, StoreError> {
		let _ = (&self.backend, ctx, manifest);
		todo!("serialize+hash manifest, idempotent-put to cas/, atomically repoint ptr/")
	}

	/// Load the current manifest for a package by resolving its `ptr/` to a
	/// manifest hash and fetching it.
	pub async fn get_manifest(
		&self,
		ctx: &AccessContext,
		package: &PackageCoordinates,
	) -> Result<BlobManifest, StoreError> {
		let _ = (&self.backend, ctx, package);
		todo!("read ptr/ -> manifest hash, get_section, deserialize into BlobManifest")
	}

	/// Whether a package currently has a stored manifest.
	pub async fn exists(&self, package: &PackageCoordinates) -> Result<bool, StoreError> {
		let _ = (&self.backend, package);
		todo!("HEAD the ptr/ key")
	}

	/// Resolve a package's coordinates to its deterministic id (pure delegation
	/// to heart; kept here so callers don't re-derive the layout key twice).
	pub fn id_of(package: &PackageCoordinates) -> PackageId { package.id() }
}
