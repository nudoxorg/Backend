//! Verified source image session for reusable publication lookups.

use backend_extension_tantivy::server::TantivySegmentHit;
use backend_extension_trustfall::server::SemanticOccurrenceHit;
use backend_semantic::index_core::EntityArtifactIdentity;
use backend_semantic::index_vocabulary::VerifiedSemanticPublication;
use backend_semantic::ir::{
    SemanticCoreReader, SemanticImageIdentity, SemanticImageView, SemanticReader,
};

use super::{
    CanonicalOccurrenceSource, CanonicalSource, CanonicalSourceError, VerifiedSourceImage,
};

impl<'image, 'bytes> VerifiedSourceImage<'image, 'bytes> {
    /// Proves a publication's image identity, byte extent, and entity count against one view.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalSourceError::WrongPublication`] if any proven image
    /// fact disagrees with the reopened view.
    pub fn verify(
        publication: VerifiedSemanticPublication,
        image: &'image SemanticImageView<'bytes>,
    ) -> Result<Self, CanonicalSourceError> {
        let image_bytes = image.as_ref();
        let declared = publication.image();
        if declared.identity != SemanticImageIdentity::from_encoded_bytes(image_bytes)
            || usize::try_from(declared.extent.byte_length()).ok() != Some(image_bytes.len())
            || publication.entity_count() != image.canonical_entities().len()
        {
            return Err(CanonicalSourceError::WrongPublication);
        }
        Ok(Self { publication, image })
    }

    /// Returns the verified publication carried by this reusable source session.
    #[must_use]
    pub const fn publication(self) -> VerifiedSemanticPublication {
        self.publication
    }

    /// Returns the reopened image proved by this source session.
    #[must_use]
    pub const fn image(self) -> &'image SemanticImageView<'bytes> {
        self.image
    }

    /// Resolves one durable Tantivy hit without revalidating the reopened image.
    ///
    /// # Errors
    ///
    /// Returns a typed snapshot, artifact, entity, or source correlation failure.
    pub fn resolve_tantivy(
        &self,
        hit: TantivySegmentHit<'_>,
    ) -> Result<CanonicalSource<'image, 'bytes>, CanonicalSourceError> {
        let provenance = hit.provenance();
        if provenance.snapshot() != self.publication.authority().snapshot {
            return Err(CanonicalSourceError::WrongPublication);
        }
        let document = provenance.document();
        let EntityArtifactIdentity::Semantic(identity) = document.artifact else {
            return Err(CanonicalSourceError::CompactArtifact);
        };
        if identity != self.publication.image().identity {
            return Err(CanonicalSourceError::WrongPublication);
        }
        let Some(entity) = self.image.entity(document.entity) else {
            return Err(CanonicalSourceError::EntityAbsent {
                entity: document.entity.raw,
            });
        };
        let Some(span) = entity.source else {
            return Err(CanonicalSourceError::SourceUnavailable {
                entity: document.entity.raw,
            });
        };
        let Some(path) = self.image.atom(span.file()) else {
            return Err(CanonicalSourceError::PathUnavailable {
                entity: document.entity.raw,
            });
        };
        Ok(CanonicalSource {
            image: self.image,
            path,
            publication: self.publication,
            provenance,
            span,
        })
    }

    /// Binds one privately constructed occurrence candidate to this exact image session.
    ///
    /// `SemanticOccurrenceHit` can be formed only by the graph's validated
    /// occurrence decoder and already borrows the image, including its closed
    /// captured-or-unavailable path/span state. Pointer equality prevents a
    /// candidate decoded from another reopened view from entering this session,
    /// so this operation is constant-time and does not collapse occurrence rows
    /// into a representative link source.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalSourceError::OccurrenceMismatch`] when the candidate
    /// borrows a different reopened view than this session.
    pub fn resolve_occurrence<'candidate>(
        self,
        hit: SemanticOccurrenceHit<'candidate, 'bytes>,
    ) -> Result<CanonicalOccurrenceSource<'candidate, 'bytes>, CanonicalSourceError>
    where
        'image: 'candidate,
    {
        let image: &'candidate SemanticImageView<'bytes> = self.image;
        if !core::ptr::eq(image, hit.image()) {
            return Err(CanonicalSourceError::OccurrenceMismatch {
                occurrence: hit.occurrence().raw,
            });
        }
        Ok(CanonicalOccurrenceSource {
            image,
            publication: self.publication,
            occurrence: hit.occurrence(),
            link: hit.link(),
            entity: hit.entity(),
            provenance: hit.provenance(),
        })
    }
}
