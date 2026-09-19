//! Immutable storage for complete, validated portable semantic images.

use core::num::TryFromIntError;
use std::path::{Path, PathBuf};

use backend_semantic::ir::{SemanticImageIdentity, SemanticImageReopenError, SemanticImageView};
use backend_version::{IrSemanticImageDomain, IrSemanticImageEncoding};
use thiserror::Error;

use crate::publication::storage::{ImmutableFileError, ImmutableFileStore, StorageNamespace, StoredFile};

/// Content-addressed facts required to reopen one complete semantic image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticImageArtifactFacts {
    /// Identity of the exact canonical semantic-image bytes.
    pub identity: SemanticImageIdentity,
    /// Exact complete image length.
    pub byte_length: u32,
}

/// Durable location and immutable facts of one stored semantic image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSemanticImage {
    /// Identity and length verified before the final identity path was exposed.
    pub facts: SemanticImageArtifactFacts,
    /// Safe fixed-name path of the immutable artifact.
    pub path: PathBuf,
}

/// Exact failure while measuring, storing, or reopening one semantic image.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ImmutableSemanticImageError {
    /// A complete validated image length exceeded the fixed durable width.
    #[error("semantic image has {observed} bytes, exceeding its durable length width")]
    Length {
        /// Observed validated byte length.
        observed: usize,
        /// Exact checked-conversion cause.
        #[source]
        source: TryFromIntError,
    },
    /// Immutable filesystem storage rejected the exact typed image artifact.
    #[error(transparent)]
    Storage(#[from] ImmutableFileError<IrSemanticImageEncoding, IrSemanticImageDomain>),
    /// Stored bytes did not reopen as a complete semantic image.
    #[error("stored immutable semantic image failed full grammar validation")]
    Grammar(#[source] SemanticImageReopenError),
}

/// Single-process owner of the semantic-image immutable namespace.
#[derive(Debug)]
pub struct ImmutableSemanticImageStore {
    store: ImmutableFileStore,
}

impl ImmutableSemanticImageStore {
    /// Opens the sibling semantic-image namespace without creating paths.
    pub(crate) fn existing(directory: &Path) -> Self {
        Self {
            store: ImmutableFileStore::existing(directory, StorageNamespace::SemanticImages),
        }
    }

    /// Creates or opens the sibling semantic-image namespace.
    pub fn new(directory: &Path) -> Result<Self, ImmutableSemanticImageError> {
        Ok(Self {
            store: ImmutableFileStore::new::<IrSemanticImageEncoding, IrSemanticImageDomain>(
                directory,
                StorageNamespace::SemanticImages,
            )?,
        })
    }

    /// Ensures the exact bytes behind one validated view are durably present.
    pub fn ensure(
        &mut self,
        image: &SemanticImageView<'_>,
    ) -> Result<StoredSemanticImage, ImmutableSemanticImageError> {
        let bytes = image.as_ref();
        let byte_length =
            u32::try_from(bytes.len()).map_err(|source| ImmutableSemanticImageError::Length {
                observed: bytes.len(),
                source,
            })?;
        let identity = SemanticImageIdentity::from_encoded_bytes(bytes);
        let stored = self.store.ensure(identity, byte_length, bytes)?;
        Ok(stored_semantic_image(stored))
    }

    /// Reads and fully revalidates one immutable semantic image into caller storage.
    pub fn open<'output>(
        &self,
        facts: SemanticImageArtifactFacts,
        output: &'output mut [u8],
    ) -> Result<SemanticImageView<'output>, ImmutableSemanticImageError> {
        let bytes = self
            .store
            .read_into(facts.identity, Some(facts.byte_length), output)?;
        SemanticImageView::reopen(bytes).map_err(ImmutableSemanticImageError::Grammar)
    }
}

fn stored_semantic_image(
    stored: StoredFile<IrSemanticImageEncoding, IrSemanticImageDomain>,
) -> StoredSemanticImage {
    StoredSemanticImage {
        facts: SemanticImageArtifactFacts {
            identity: stored.identity,
            byte_length: stored.length,
        },
        path: stored.path,
    }
}
