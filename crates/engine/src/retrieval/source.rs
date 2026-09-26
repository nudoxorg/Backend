//! Source provenance for disposable Tantivy membership hits.

use backend_extension_tantivy::server::{TantivyProvenance, TantivySegmentHit};
use backend_extension_trustfall::server::{OccurrenceSourceEvidence, SemanticOccurrenceHit};
use backend_semantic::index_core::EntityDocumentId;
use backend_semantic::index_vocabulary::{IndexSnapshotId, VerifiedSemanticPublication};
use backend_semantic::ir::{
    LinkId, LinkOccurrenceId, SemanticImageIdentity, SemanticImageView, SourceSpan,
};

mod image;

/// A source location proved against one reopened canonical semantic image.
///
/// The fields remain private so image bytes, path bytes, and the source span
/// cannot be mixed from separate validation events.
pub struct CanonicalSource<'image, 'bytes> {
    image: &'image SemanticImageView<'bytes>,
    path: &'image [u8],
    publication: VerifiedSemanticPublication,
    provenance: TantivyProvenance,
    span: SourceSpan,
}

impl<'image, 'bytes> CanonicalSource<'image, 'bytes> {
    /// Returns the reopened image that proved this source location.
    #[must_use]
    pub const fn image(&self) -> &'image SemanticImageView<'bytes> {
        self.image
    }

    /// Returns the exact, possibly non-UTF-8, canonical path atom bytes.
    #[must_use]
    pub const fn path(&self) -> &'image [u8] {
        self.path
    }

    /// Returns the entity document identity carried by the membership hit.
    #[must_use]
    pub const fn document(&self) -> EntityDocumentId {
        self.provenance.document()
    }

    /// Returns the accepted durable Tantivy row provenance.
    ///
    /// This keeps the pinned snapshot, contributing segment, row, and document
    /// identity correlated with the canonical-image source proof.
    #[must_use]
    pub const fn provenance(&self) -> TantivyProvenance {
        self.provenance
    }

    /// Returns the verified publication authority that admitted this source.
    #[must_use]
    pub const fn publication(&self) -> VerifiedSemanticPublication {
        self.publication
    }

    /// Returns the proved half-open source span.
    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }

    /// Copies a canonical-image proof into an owned fact suitable for one bounded client reply.
    ///
    /// A raw Tantivy hit cannot call this transition: it must first become `CanonicalSource`
    /// through [`VerifiedSourceImage`] correlation.
    ///
    /// # Errors
    ///
    /// Returns [`SourceWireError::PathLength`] when the canonical path exceeds the bounded wire
    /// contract, or [`SourceWireError::Allocation`] when its exact owned copy cannot be reserved.
    pub fn into_owned_canonical_source(self) -> Result<OwnedCanonicalSource, SourceWireError> {
        let Self {
            path,
            publication,
            provenance,
            span,
            ..
        } = self;
        if path.len() > MAX_OWNED_CANONICAL_SOURCE_PATH_BYTES {
            return Err(SourceWireError::PathLength {
                actual: path.len(),
                maximum: MAX_OWNED_CANONICAL_SOURCE_PATH_BYTES,
            });
        }
        Ok(OwnedCanonicalSource {
            path: own_source_path(path)?,
            start: span.start(),
            end: span.end(),
            snapshot: publication.authority().snapshot,
            document: provenance.document(),
            publication,
        })
    }
}

/// Owned source proof emitted only after canonical-image correlation.
///
/// Private fields keep its snapshot, exact `EntityDocumentId`, publication, image, and source
/// coordinates inseparable. It owns non-UTF-8 path bytes so a client reply never borrows a
/// reopened image or a disposable search projection.
#[derive(Debug, Eq, PartialEq)]
pub struct OwnedCanonicalSource {
    path: Vec<u8>,
    start: u32,
    end: u32,
    snapshot: IndexSnapshotId,
    document: EntityDocumentId,
    publication: VerifiedSemanticPublication,
}

impl OwnedCanonicalSource {
    /// Returns the pinned snapshot proven by the canonical publication.
    #[must_use]
    pub const fn snapshot(&self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Returns the exact artifact-and-entity identity proven by the membership hit.
    #[must_use]
    pub const fn document(&self) -> EntityDocumentId {
        self.document
    }

    /// Returns the publication that admitted the canonical image.
    #[must_use]
    pub const fn publication(&self) -> VerifiedSemanticPublication {
        self.publication
    }

    /// Returns the proved canonical image identity.
    #[must_use]
    pub const fn image(&self) -> SemanticImageIdentity {
        self.publication.image().identity
    }

    /// Returns the owned, potentially non-UTF-8 source path bytes.
    #[must_use]
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    /// Returns the proved inclusive source offset.
    #[must_use]
    pub const fn start(&self) -> u32 {
        self.start
    }

    /// Returns the proved exclusive source offset.
    #[must_use]
    pub const fn end(&self) -> u32 {
        self.end
    }

    /// Separates owned proof fields only for an optional transport adapter that consumes this fact.
    ///
    /// The source proof itself remains unforgeable because this method is available only after a
    /// `CanonicalSource` transition; retrieval retains no dependency on any client transport.
    #[must_use]
    pub fn into_transport_parts(self) -> (Vec<u8>, u32, u32, IndexSnapshotId, EntityDocumentId) {
        (
            self.path,
            self.start,
            self.end,
            self.snapshot,
            self.document,
        )
    }
}

/// Occurrence-level source evidence bound to a verified semantic publication.
///
/// The Trustfall occurrence is a local candidate decoded from a reopened
/// image. This closed result proves that its image, occurrence/link rows, and
/// captured path/span all agree with the supplied publication authority.
#[derive(Clone, Copy)]
pub struct CanonicalOccurrenceSource<'image, 'bytes> {
    image: &'image SemanticImageView<'bytes>,
    publication: VerifiedSemanticPublication,
    occurrence: LinkOccurrenceId,
    link: LinkId,
    entity: backend_semantic::ir::EntityId,
    provenance: OccurrenceSourceEvidence<'image>,
}

impl<'image, 'bytes> CanonicalOccurrenceSource<'image, 'bytes> {
    /// Returns the reopened canonical image that proved this occurrence.
    #[must_use]
    pub const fn image(self) -> &'image SemanticImageView<'bytes> {
        self.image
    }

    /// Returns the verified publication that admitted this image.
    #[must_use]
    pub const fn publication(self) -> VerifiedSemanticPublication {
        self.publication
    }

    /// Returns the stable occurrence coordinate in the canonical image.
    #[must_use]
    pub const fn occurrence(self) -> LinkOccurrenceId {
        self.occurrence
    }

    /// Returns the canonical relation coordinate named by the occurrence.
    #[must_use]
    pub const fn link(self) -> LinkId {
        self.link
    }

    /// Returns the local target declaration associated with this occurrence.
    #[must_use]
    pub const fn entity(self) -> backend_semantic::ir::EntityId {
        self.entity
    }

    /// Returns the closed captured-or-unavailable source evidence.
    #[must_use]
    pub const fn provenance(self) -> OccurrenceSourceEvidence<'image> {
        self.provenance
    }

    /// Copies closed occurrence source evidence into an owned client-reply fact.
    ///
    /// # Errors
    ///
    /// Returns [`SourceWireError::PathLength`] when captured path evidence exceeds the bounded
    /// wire contract, or [`SourceWireError::Allocation`] when its exact owned copy cannot be
    /// reserved.
    pub fn into_owned_canonical_occurrence_source(
        self,
    ) -> Result<OwnedCanonicalOccurrenceSource, SourceWireError> {
        OwnedCanonicalOccurrenceSource::from_canonical(self)
    }
}

/// Owned captured-or-unavailable occurrence source proof.
///
/// This closed result can only be created from an already canonicalized occurrence. It retains
/// explicit `Unavailable` evidence rather than fabricating an empty source span.
#[derive(Debug, Eq, PartialEq)]
pub enum OwnedCanonicalOccurrenceSource {
    /// The canonical image captured an exact source coordinate for this occurrence.
    Captured(OwnedCanonicalOccurrenceSpan),
    /// The canonical image proved this occurrence but did not capture a source coordinate.
    Unavailable(OwnedUnavailableOccurrenceSource),
}

impl OwnedCanonicalOccurrenceSource {
    fn from_canonical(source: CanonicalOccurrenceSource<'_, '_>) -> Result<Self, SourceWireError> {
        let snapshot = source.publication().authority().snapshot;
        let publication = source.publication();
        match source.provenance() {
            OccurrenceSourceEvidence::Captured(captured) => {
                let path = captured.path();
                if path.len() > MAX_OWNED_CANONICAL_SOURCE_PATH_BYTES {
                    return Err(SourceWireError::PathLength {
                        actual: path.len(),
                        maximum: MAX_OWNED_CANONICAL_SOURCE_PATH_BYTES,
                    });
                }
                let span = captured.span();
                Ok(Self::Captured(OwnedCanonicalOccurrenceSpan {
                    path: own_source_path(path)?,
                    start: span.start(),
                    end: span.end(),
                    snapshot,
                    publication,
                    occurrence: source.occurrence(),
                    link: source.link(),
                    entity: source.entity(),
                }))
            }
            OccurrenceSourceEvidence::Unavailable => {
                Ok(Self::Unavailable(OwnedUnavailableOccurrenceSource {
                    snapshot,
                    publication,
                    occurrence: source.occurrence(),
                    link: source.link(),
                    entity: source.entity(),
                }))
            }
        }
    }
}

/// Owned captured occurrence coordinate with all canonical provenance retained privately.
#[derive(Debug, Eq, PartialEq)]
pub struct OwnedCanonicalOccurrenceSpan {
    path: Vec<u8>,
    start: u32,
    end: u32,
    snapshot: IndexSnapshotId,
    publication: VerifiedSemanticPublication,
    occurrence: LinkOccurrenceId,
    link: LinkId,
    entity: backend_semantic::ir::EntityId,
}

impl OwnedCanonicalOccurrenceSpan {
    /// Returns the remote snapshot proven by the canonical publication.
    #[must_use]
    pub const fn snapshot(&self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Returns the exact canonical publication that proved this capture.
    #[must_use]
    pub const fn publication(&self) -> VerifiedSemanticPublication {
        self.publication
    }

    /// Returns the proved canonical semantic image identity.
    #[must_use]
    pub const fn image(&self) -> SemanticImageIdentity {
        self.publication.image().identity
    }

    /// Returns the stable canonical occurrence coordinate.
    #[must_use]
    pub const fn occurrence(&self) -> LinkOccurrenceId {
        self.occurrence
    }

    /// Returns the exact canonical link coordinate.
    #[must_use]
    pub const fn link(&self) -> LinkId {
        self.link
    }

    /// Returns the relation target entity in the canonical image.
    #[must_use]
    pub const fn entity(&self) -> backend_semantic::ir::EntityId {
        self.entity
    }

    /// Returns the owned possibly non-UTF-8 path bytes.
    #[must_use]
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    /// Returns the captured inclusive source offset.
    #[must_use]
    pub const fn start(&self) -> u32 {
        self.start
    }

    /// Returns the captured exclusive source offset.
    #[must_use]
    pub const fn end(&self) -> u32 {
        self.end
    }
}

/// Owned proof that canonical occurrence source coordinates were unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnedUnavailableOccurrenceSource {
    snapshot: IndexSnapshotId,
    publication: VerifiedSemanticPublication,
    occurrence: LinkOccurrenceId,
    link: LinkId,
    entity: backend_semantic::ir::EntityId,
}

impl OwnedUnavailableOccurrenceSource {
    /// Returns the remote snapshot proven by the canonical publication.
    #[must_use]
    pub const fn snapshot(&self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Returns the exact canonical publication that proved this absence.
    #[must_use]
    pub const fn publication(&self) -> VerifiedSemanticPublication {
        self.publication
    }

    /// Returns the proved canonical semantic image identity.
    #[must_use]
    pub const fn image(&self) -> SemanticImageIdentity {
        self.publication.image().identity
    }

    /// Returns the stable canonical occurrence coordinate.
    #[must_use]
    pub const fn occurrence(&self) -> LinkOccurrenceId {
        self.occurrence
    }

    /// Returns the exact canonical link coordinate.
    #[must_use]
    pub const fn link(&self) -> LinkId {
        self.link
    }

    /// Returns the relation target entity in the canonical image.
    #[must_use]
    pub const fn entity(&self) -> backend_semantic::ir::EntityId {
        self.entity
    }
}

/// Failure while materializing a bounded owned source fact for transport.
#[derive(Debug, thiserror::Error)]
pub enum SourceWireError {
    /// Canonical path bytes exceeded the explicit client source-span limit.
    #[error("canonical source path has {actual} bytes, maximum {maximum}")]
    PathLength {
        /// Number of canonical path bytes observed.
        actual: usize,
        /// Largest client-transport path length.
        maximum: usize,
    },
    /// Allocating the owned canonical path could not reserve its exact bounded capacity.
    #[error("could not reserve {requested} bytes for an owned canonical source path")]
    Allocation {
        /// Exact bounded source-path capacity requested.
        requested: usize,
        /// Allocator failure retained at the transport ownership boundary.
        #[source]
        source: std::collections::TryReserveError,
    },
}

/// Largest path retained by an owned canonical source reply fact.
pub(crate) const MAX_OWNED_CANONICAL_SOURCE_PATH_BYTES: usize = 4096;

fn own_source_path(path: &[u8]) -> Result<Vec<u8>, SourceWireError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(path.len())
        .map_err(|source| SourceWireError::Allocation {
            requested: path.len(),
            source,
        })?;
    owned.extend_from_slice(path);
    Ok(owned)
}

/// Failure while correlating a disposable hit with canonical source facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CanonicalSourceError {
    /// The hit refers to a compact fragment, which is not canonical proof.
    #[error("Tantivy hit does not identify a complete semantic image")]
    CompactArtifact,
    /// The reopened image is not the image named by the verified publication.
    #[error("reopened semantic image does not match the verified publication")]
    WrongPublication,
    /// The output cannot hold every requested source result.
    #[error("source output has {available} slots, but {required} hits were supplied")]
    InsufficientOutput {
        /// Number of source results requested by the caller.
        required: usize,
        /// Number of caller-owned output slots supplied.
        available: usize,
    },
    /// A selected result slot did not contain a proven Tantivy hit.
    #[error("Tantivy result slot {slot} has no hit")]
    MissingHit {
        /// Index of the missing selected membership hit.
        slot: usize,
    },
    /// The caller's reusable scratch cannot hold every selected source.
    #[error("source scratch has {available} slots, but {required} hits were supplied")]
    ScratchTooSmall {
        /// Number of transactional scratch entries required.
        required: usize,
        /// Number of caller-owned scratch slots supplied.
        available: usize,
    },
    /// No canonical entity exists at the hit's entity coordinate.
    #[error("semantic image does not contain entity {entity}")]
    EntityAbsent {
        /// Missing canonical entity coordinate.
        entity: u32,
    },
    /// The canonical entity exists but retained no source span.
    #[error("canonical entity {entity} has no source span")]
    SourceUnavailable {
        /// Entity coordinate whose source fact is unavailable.
        entity: u32,
    },
    /// The source span names an atom that is absent from the image.
    #[error("source path atom for entity {entity} is unavailable")]
    PathUnavailable {
        /// Entity coordinate whose captured source path atom was absent.
        entity: u32,
    },
    /// The candidate occurrence did not agree with reopened canonical rows.
    #[error("semantic occurrence {occurrence} disagrees with reopened image evidence")]
    OccurrenceMismatch {
        /// Candidate occurrence coordinate.
        occurrence: u32,
    },
}

/// Resolves all supplied Tantivy hits against one caller-reopened image.
///
/// Tantivy membership is candidate evidence only; it never proves source
/// provenance without this canonical-image correlation.
///
/// Every hit is preflighted before `output` is changed. Empty source spans are
/// retained as captured facts; unavailable spans are reported separately.
/// # Errors
///
/// Returns a typed capacity, missing-hit, publication, artifact, entity, or
/// source correlation failure without changing `output`.
pub fn resolve_tantivy_sources<'image, 'bytes>(
    publication: VerifiedSemanticPublication,
    image: &'image SemanticImageView<'bytes>,
    hits: &[Option<TantivySegmentHit<'_>>],
    scratch: &mut [Option<CanonicalSource<'image, 'bytes>>],
    output: &mut [Option<CanonicalSource<'image, 'bytes>>],
) -> Result<usize, CanonicalSourceError> {
    if output.len() < hits.len() {
        return Err(CanonicalSourceError::InsufficientOutput {
            required: hits.len(),
            available: output.len(),
        });
    }
    if scratch.len() < hits.len() {
        return Err(CanonicalSourceError::ScratchTooSmall {
            required: hits.len(),
            available: scratch.len(),
        });
    }

    let authority = VerifiedSourceImage::verify(publication, image)?;
    for (slot, (scratch_slot, hit)) in scratch.iter_mut().zip(hits).enumerate() {
        let Some(hit) = hit else {
            return Err(CanonicalSourceError::MissingHit { slot });
        };
        *scratch_slot = Some(authority.resolve_tantivy(*hit)?);
    }

    let mut written = 0;
    for (output_slot, source) in output.iter_mut().zip(scratch.iter_mut()).take(hits.len()) {
        *output_slot = source.take();
        written += 1;
    }
    Ok(written)
}

/// Resolves one Tantivy hit without allocating or reopening the image.
///
/// # Errors
///
/// Returns a typed publication, artifact, entity, or source correlation failure.
pub fn resolve_tantivy_source<'image, 'bytes>(
    publication: VerifiedSemanticPublication,
    image: &'image SemanticImageView<'bytes>,
    hit: TantivySegmentHit<'_>,
) -> Result<CanonicalSource<'image, 'bytes>, CanonicalSourceError> {
    VerifiedSourceImage::verify(publication, image)?.resolve_tantivy(hit)
}

/// Binds one occurrence-level Trustfall candidate to a verified publication.
///
/// This does not elevate representative `Link.source` evidence. The candidate
/// is privately constructed by the graph's occurrence decoder, carries the
/// exact occurrence/link coordinates plus closed source evidence, and borrows
/// the reopened image. This transition proves that exact view against the
/// publication; no filesystem or source-file-content read occurs.
///
/// # Errors
///
/// Returns a typed publication or private-candidate/view correlation failure.
pub fn resolve_occurrence_source<'image, 'bytes>(
    publication: VerifiedSemanticPublication,
    hit: SemanticOccurrenceHit<'image, 'bytes>,
) -> Result<CanonicalOccurrenceSource<'image, 'bytes>, CanonicalSourceError> {
    VerifiedSourceImage::verify(publication, hit.image())?.resolve_occurrence(hit)
}

/// Resolves occurrence candidates transactionally through one proved image session.
///
/// The independently borrowed selection container need not outlive the result:
/// output retains only the image-borrowed source evidence and exact occurrence
/// identity. As with Tantivy source resolution, output is untouched on every
/// capacity or correlation failure.
///
/// # Errors
///
/// Returns a typed capacity or candidate/view correlation failure without
/// changing `output`.
pub fn resolve_occurrence_sources<'image, 'bytes, 'selection, 'candidate>(
    source_image: VerifiedSourceImage<'image, 'bytes>,
    hits: &'selection [SemanticOccurrenceHit<'candidate, 'bytes>],
    scratch: &mut [Option<CanonicalOccurrenceSource<'candidate, 'bytes>>],
    output: &mut [Option<CanonicalOccurrenceSource<'candidate, 'bytes>>],
) -> Result<usize, CanonicalSourceError>
where
    'image: 'candidate,
{
    if output.len() < hits.len() {
        return Err(CanonicalSourceError::InsufficientOutput {
            required: hits.len(),
            available: output.len(),
        });
    }
    if scratch.len() < hits.len() {
        return Err(CanonicalSourceError::ScratchTooSmall {
            required: hits.len(),
            available: scratch.len(),
        });
    }
    for (scratch_slot, hit) in scratch.iter_mut().zip(hits.iter().copied()) {
        *scratch_slot = Some(source_image.resolve_occurrence(hit)?);
    }
    let mut written = 0;
    for (output_slot, source) in output.iter_mut().zip(scratch.iter()).take(hits.len()) {
        *output_slot = *source;
        written += 1;
    }
    Ok(written)
}

/// Immutable source authority proved once for one reopened semantic image.
///
/// A batch validates its image identity and publication extent once here, then
/// performs only durable-hit and direct entity/path lookups per row. Its
/// private fields ensure callers can obtain this session only through the
/// publication/image proof transition.
#[derive(Clone, Copy)]
pub struct VerifiedSourceImage<'image, 'bytes> {
    publication: VerifiedSemanticPublication,
    image: &'image SemanticImageView<'bytes>,
}
