//! Defines manifest-store behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the manifest-store invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Single-owner storage for validated immutable compiler package manifest bytes.

use std::path::{Path, PathBuf};

use backend_version::{IrManifestDomain, IrManifestEncoding};
use thiserror::Error;

use crate::{
    ImmutableFileError,
    manifest::{
        CompilationManifestError, CompilationManifestIdentity, CompilationManifestView,
        StoredFragmentFacts,
    },
    storage::{ImmutableFileStore, StorageNamespace, StoredFile},
};

/// Failure while durably storing one validated canonical package manifest.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ImmutableManifestError {
    /// The exact typed immutable filesystem terminal.
    #[error(transparent)]
    Storage(#[from] ImmutableFileError<IrManifestEncoding, IrManifestDomain>),
    /// Stored immutable manifest bytes did not pass the canonical package grammar.
    #[error("stored compiler package manifest did not pass canonical validation")]
    Manifest(#[source] CompilationManifestError),
}

/// Location and immutable facts of one stored compiler package manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredManifest {
    /// Typed identity of the complete canonical manifest bytes.
    pub identity: CompilationManifestIdentity,
    /// Exact byte length committed by the identity path.
    pub length: u32,
    /// Safe fixed-name path of the immutable manifest artifact.
    pub path: PathBuf,
}

/// Filesystem owner for one sibling immutable-manifest directory.
///
/// A mutable store reference serializes local transitions. Like fragment storage, this is a
/// single-process contract; callers needing cross-process ownership establish it externally.
#[derive(Debug)]
pub struct ImmutableManifestStore {
    store: ImmutableFileStore,
}

impl ImmutableManifestStore {
    pub(crate) fn existing(directory: &Path) -> Self {
        Self {
            store: ImmutableFileStore::existing(directory, StorageNamespace::Manifests),
        }
    }

    /// Opens or creates the sibling immutable-manifest directory.
    #[allow(
        clippy::result_large_err,
        reason = "manifest grammar errors retain exact entry facts"
    )]
    pub fn new(directory: &Path) -> Result<Self, ImmutableManifestError> {
        Ok(Self {
            store: ImmutableFileStore::new::<IrManifestEncoding, IrManifestDomain>(
                directory,
                StorageNamespace::Manifests,
            )?,
        })
    }

    /// Ensures validated complete manifest bytes are durable under their typed identity.
    ///
    /// The borrowed view is the sole admission capability. A missing identity stages, syncs,
    /// renames, and directory-syncs bytes; an existing identity is rehashed then reused without a
    /// rewrite or directory barrier.
    #[allow(
        clippy::result_large_err,
        reason = "manifest grammar errors retain exact entry facts"
    )]
    pub fn ensure(
        &mut self,
        manifest: &CompilationManifestView<'_, '_>,
    ) -> Result<StoredManifest, ImmutableManifestError> {
        let stored =
            self.store
                .ensure(manifest.identity, manifest.byte_length, manifest.as_ref())?;
        Ok(stored_manifest(stored))
    }

    /// Reads and validates one immutable manifest into caller-owned output and fact scratch.
    #[allow(
        clippy::result_large_err,
        reason = "manifest grammar errors retain exact entry facts"
    )]
    pub fn open<'manifest, 'facts>(
        &self,
        identity: CompilationManifestIdentity,
        output: &'manifest mut [u8],
        facts: &'facts mut [Option<StoredFragmentFacts>],
    ) -> Result<CompilationManifestView<'manifest, 'facts>, ImmutableManifestError> {
        let bytes = self.store.read_into(identity, None, output)?;
        let manifest = CompilationManifestView::validate(bytes, facts)
            .map_err(ImmutableManifestError::Manifest)?;
        if manifest.identity != identity {
            return Err(ImmutableManifestError::Storage(
                ImmutableFileError::ExistingIdentityMismatch {
                    expected: identity,
                    observed: manifest.identity,
                },
            ));
        }
        Ok(manifest)
    }
}

fn stored_manifest(stored: StoredFile<IrManifestEncoding, IrManifestDomain>) -> StoredManifest {
    StoredManifest {
        identity: stored.identity,
        length: stored.length,
        path: stored.path,
    }
}
