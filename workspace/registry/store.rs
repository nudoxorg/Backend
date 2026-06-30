//! The registry, at its core: a durable store of parsed packages, abstracted
//! over object storage (S3, local filesystem, ...) via the `object_store`
//! crate.
//!
//! The system is built around [`Package`]. You address a package's parsed
//! representation *by the package itself* — the object-store location is
//! derived deterministically from its coordinates

use std::sync::Arc;

use heart::Language;
use object_store::{ObjectStore, path::Path};

use crate::{Package, blob::Blob};

/// Failure mode of the registry store.
#[derive(Debug)]
pub enum StoreError {
	/// The underlying object store failed.
	Backend(object_store::Error),
	/// A blob failed to (de)serialize.
	Serialization,
}

/// The registry's durable, package-addressed store over object storage.
pub struct Store {
	backend: Arc<dyn ObjectStore>,
}

impl Store {
	/// Wrap an object-store backend (`AmazonS3`, `LocalFileSystem`, ...). The
	/// choice of backend is where self-hosting / per-source federation plugs in.
	pub fn new(backend: Arc<dyn ObjectStore>) -> Self { Self { backend } }

	/// Persist a package's parsed blob at its derived location.
	pub async fn put(&self, package: &Package, _blob: &Blob) -> Result<(), StoreError> {
		let _ = (&self.backend, Self::location(package));
		todo!("serialize the blob (bincode) and put it at location(package)")
	}

	/// Load a package's parsed blob.
	pub async fn get(&self, package: &Package) -> Result<Blob, StoreError> {
		let _ = (&self.backend, Self::location(package));
		todo!("get the bytes at location(package) and deserialize the blob")
	}

	/// The object-store location a package's blob lives at — the registry's
	/// address, derived from the package's coordinates (not an opaque ref).
	fn location(package: &Package) -> Path {
		let lang = match package.language {
			Language::Rust(_) => "rust",
			Language::Typescript(_) => "typescript",
		};
		Path::from(format!("{lang}/{}/{}", package.name, package.version))
	}
}
