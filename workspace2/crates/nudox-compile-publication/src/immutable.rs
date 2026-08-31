//! Single-owner storage for validated immutable IR fragment bytes.

use std::path::{Path, PathBuf};

use nudox_id::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};
use nudox_ir_format::{
    FragmentError, FragmentRangeManifest, FragmentRangeVerifyError, FragmentView,
};
use thiserror::Error;

use crate::storage::{ImmutableFileError, ImmutableFileStore, StorageNamespace, StoredFile};

pub use crate::storage::ImmutableIoPhase;

/// Typed identity used for complete canonical IR fragment artifacts.
pub type FragmentIdentity = ArtifactId<IrFragmentEncoding, IrFragmentDomain>;

/// Failure while validating or durably storing one complete immutable fragment.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ImmutableArtifactError {
    /// The caller did not provide complete bytes satisfying the validated fragment manifest.
    #[error("fragment bytes do not satisfy their complete validated manifest")]
    Fragment(#[source] FragmentRangeVerifyError),
    /// Stored immutable fragment bytes did not pass the compact IR grammar.
    #[error("stored immutable fragment bytes did not pass compact IR validation")]
    FragmentGrammar(#[source] FragmentError),
    /// The exact typed immutable filesystem terminal.
    #[error(transparent)]
    Storage(#[from] ImmutableFileError<IrFragmentEncoding, IrFragmentDomain>),
}

/// Location and immutable facts of one stored IR fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredArtifact {
    /// Typed identity of the complete canonical bytes.
    pub identity: FragmentIdentity,
    /// Exact byte length committed by the identity path.
    pub length: u32,
    /// Safe fixed-name path of the immutable artifact.
    pub path: PathBuf,
}

/// Filesystem owner for one sibling immutable-fragment directory.
///
/// A mutable store reference serializes local temp-to-final transitions. This is a single-process
/// ownership contract: the requested `rename` may replace a same-path write by another process,
/// so callers needing cross-process ownership must establish it outside this type.
#[derive(Debug)]
pub struct ImmutableArtifactStore {
    store: ImmutableFileStore,
}

impl ImmutableArtifactStore {
    /// Opens the sibling immutable-fragment directory without creating any paths.
    pub(crate) fn existing(directory: &Path) -> Self {
        Self {
            store: ImmutableFileStore::existing(directory, StorageNamespace::Fragments),
        }
    }

    /// Opens or creates the sibling immutable-fragment directory.
    pub fn new(directory: &Path) -> Result<Self, ImmutableArtifactError> {
        Ok(Self {
            store: ImmutableFileStore::new::<IrFragmentEncoding, IrFragmentDomain>(
                directory,
                StorageNamespace::Fragments,
            )?,
        })
    }

    /// Ensures one manifest-validated complete fragment is durably available by typed identity.
    ///
    /// Validation precedes every filesystem action. A missing path is staged in the same
    /// directory, file-synced, renamed under its immutable identity, and directory-synced. An
    /// existing path is rehashed and returned without a directory barrier or rewrite.
    pub fn ensure(
        &mut self,
        manifest: &FragmentRangeManifest,
        fragment: &FragmentView<'_>,
    ) -> Result<StoredArtifact, ImmutableArtifactError> {
        manifest
            .verify_fragment(fragment.as_ref())
            .map_err(ImmutableArtifactError::Fragment)?;
        let stored = self.store.ensure(
            manifest.fragment,
            manifest.fragment_length,
            fragment.as_ref(),
        )?;
        Ok(stored_artifact(stored))
    }

    /// Reads one complete immutable fragment into caller storage and validates its compact grammar.
    pub fn open<'fragment>(
        &self,
        facts: crate::manifest::StoredFragmentFacts,
        output: &'fragment mut [u8],
    ) -> Result<FragmentView<'fragment>, ImmutableArtifactError> {
        let bytes = self
            .store
            .read_into(facts.fragment, Some(facts.fragment_length), output)?;
        FragmentView::validate(bytes).map_err(ImmutableArtifactError::FragmentGrammar)
    }
}

fn stored_artifact(stored: StoredFile<IrFragmentEncoding, IrFragmentDomain>) -> StoredArtifact {
    StoredArtifact {
        identity: stored.identity,
        length: stored.length,
        path: stored.path,
    }
}
