//! Bounded GUI ownership for one verified semantic image and its rendered documents.
//!
//! The compiler owns publication and the retained byte snapshot. This module reopens those exact
//! bytes through the IR grammar, discovers them once, and materializes canonical semantic
//! documents for GPUI. It deliberately owns no parallel semantic DTO and cannot reconstruct source
//! syntax that the image did not retain.

use std::{collections::TryReserveError, string::FromUtf8Error, sync::Arc};

#[cfg(feature = "real-gpui")]
use std::num::NonZeroUsize;

use compiler_ir::{
    EntityId, FullSemanticImageError, LanguageProfile, SemanticDiscoveryError,
    SemanticDocumentError, SemanticImageCensus,
};
use interface_core::{CorrelationId, SemanticImageAccessError, SemanticImageAuthority};

#[cfg(feature = "real-gpui")]
use compiler_ir::{
    CanonicalTypeRenderLimits, SemanticImageDiscovery, SemanticImageView, SemanticReader,
    prepare_semantic_document,
};
#[cfg(feature = "real-gpui")]
use interface_core::{CompilerCapability, GeneratedArtifact};

/// Maximum canonical semantic-document text retained by one application window.
///
/// The bound applies to payload bytes, independently of the already-bounded semantic image and
/// entity count. Preparation measures every document before any document-text allocation begins.
pub const MAX_RENDERED_SEMANTIC_BYTES: usize = 4 * 1024 * 1024;

#[cfg(feature = "real-gpui")]
const MAX_SEMANTIC_TYPE_DEPTH: usize = 64;

/// One canonical entity document owned across GPUI render frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedSemanticDocument {
    /// Exact canonical entity coordinate in the reopened image.
    pub entity: EntityId,
    /// Canonical authority-backed semantic document.
    ///
    /// Immutable shared ownership is intentional here: GPUI elements must retain text beyond the
    /// state borrow that built a frame, while the window keeps the same document for later frames.
    pub text: Arc<str>,
}

/// Complete successful GUI projection of one exact generated semantic image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDocumentationProjection {
    /// Package request which produced this projection.
    pub correlation: CorrelationId,
    /// Exact durable semantic image whose bytes were reopened.
    pub semantic_image: SemanticImageAuthority,
    /// Language profile proven both by the generated recipe and image authority.
    pub profile: LanguageProfile,
    /// Allocation-free census over every reopened semantic plane.
    pub census: SemanticImageCensus,
    /// Total canonical UTF-8 bytes retained across all entity documents.
    pub encoded_bytes: usize,
    /// Documents in canonical entity order.
    pub documents: Box<[RenderedSemanticDocument]>,
}

/// Exact failure while projecting verified compiler output into owned GUI documents.
#[derive(Debug)]
pub enum PackageDocumentationError {
    /// The compiler owner could not lend an owned copy of the requested exact image.
    Snapshot {
        /// Original compiler snapshot failure.
        source: SemanticImageAccessError,
    },
    /// The copied bytes failed the complete semantic-image grammar on the GUI boundary.
    Reopen {
        /// Original full-image validation failure.
        source: FullSemanticImageError,
    },
    /// A complete reopened reader failed a discovery invariant.
    Discovery {
        /// Original missing pooled-coordinate fact.
        source: SemanticDiscoveryError,
    },
    /// The bounded preparation owner could not reserve one slot per validated entity.
    PreparationAllocation {
        /// Exact validated entity count.
        entities: usize,
        /// Standard allocation cause.
        source: TryReserveError,
    },
    /// The canonical output size overflowed the host address space.
    OutputLengthOverflow {
        /// Entity whose admitted document overflowed the running total.
        entity: EntityId,
        /// Bytes admitted before this entity.
        accumulated: usize,
        /// Exact bytes required by this entity.
        next: usize,
    },
    /// Canonical documents exceeded the explicit per-window text budget.
    OutputBudget {
        /// Entity which crossed the bound.
        entity: EntityId,
        /// Total bytes required through that entity.
        required: usize,
        /// Fixed maximum retained by one window.
        maximum: usize,
    },
    /// The final document owner could not reserve one slot per validated entity.
    DocumentAllocation {
        /// Exact validated entity count.
        entities: usize,
        /// Standard allocation cause.
        source: TryReserveError,
    },
    /// One exactly measured output buffer could not be reserved.
    OutputAllocation {
        /// Entity owning the allocation.
        entity: EntityId,
        /// Exact promised byte count.
        bytes: usize,
        /// Standard allocation cause.
        source: TryReserveError,
    },
    /// Semantic-document admission or writing failed with its complete original cause.
    Render {
        /// Original semantic renderer failure.
        source: SemanticDocumentError,
    },
    /// A prepared writer returned an extent other than its public promise.
    WrittenExtent {
        /// Entity whose extent disagreed.
        entity: EntityId,
        /// Prepared exact byte count.
        promised: usize,
        /// Observed returned byte count.
        observed: usize,
    },
    /// The renderer's checked UTF-8 borrow could not be reconstituted as the final string owner.
    OutputEncoding {
        /// Entity owning the invalid output.
        entity: EntityId,
        /// Exact standard conversion error, including the original bytes.
        source: FromUtf8Error,
    },
}

/// Failed package-document projection with the same request and image identities as its source.
#[derive(Debug)]
pub struct PackageDocumentationFailure {
    /// Package request which produced this terminal.
    pub correlation: CorrelationId,
    /// Exact durable semantic image requested from the compiler owner.
    pub semantic_image: SemanticImageAuthority,
    /// Exact failure; no diagnostic string replaces it in state.
    pub source: PackageDocumentationError,
}

/// Terminal state of one post-publication semantic-document projection.
#[derive(Debug)]
pub enum PackageDocumentationOutcome {
    /// Every canonical entity was discovered and rendered within the bound.
    Rendered(PackageDocumentationProjection),
    /// Publication succeeded, but GUI projection retained this exact later failure.
    Failed(PackageDocumentationFailure),
}

impl PackageDocumentationOutcome {
    /// Correlation of the package request which owns this outcome.
    #[must_use]
    pub const fn correlation(&self) -> CorrelationId {
        match self {
            Self::Rendered(projection) => projection.correlation,
            Self::Failed(failure) => failure.correlation,
        }
    }
}

#[cfg(feature = "real-gpui")]
pub(crate) fn project_generated_documents<Compiler: CompilerCapability>(
    compiler: &mut Compiler,
    correlation: CorrelationId,
    artifact: GeneratedArtifact,
) -> PackageDocumentationOutcome {
    match project(compiler, correlation, artifact) {
        Ok(projection) => PackageDocumentationOutcome::Rendered(projection),
        Err(source) => PackageDocumentationOutcome::Failed(PackageDocumentationFailure {
            correlation,
            semantic_image: artifact.semantic_image,
            source,
        }),
    }
}

#[cfg(feature = "real-gpui")]
fn project<Compiler: CompilerCapability>(
    compiler: &mut Compiler,
    correlation: CorrelationId,
    artifact: GeneratedArtifact,
) -> Result<PackageDocumentationProjection, PackageDocumentationError> {
    let snapshot = compiler
        .semantic_image_snapshot(artifact.semantic_image)
        .map_err(|source| PackageDocumentationError::Snapshot { source })?;
    let image = SemanticImageView::reopen(snapshot.as_ref())
        .map_err(|source| PackageDocumentationError::Reopen { source })?;
    let census = SemanticImageDiscovery::new(&image)
        .census()
        .map_err(|source| PackageDocumentationError::Discovery { source })?;
    let type_limits = CanonicalTypeRenderLimits::new(
        NonZeroUsize::new(MAX_SEMANTIC_TYPE_DEPTH)
            .expect("the static semantic type depth is nonzero"),
    );

    let mut prepared = Vec::new();
    prepared
        .try_reserve_exact(census.entities)
        .map_err(|source| PackageDocumentationError::PreparationAllocation {
            entities: census.entities,
            source,
        })?;
    let mut encoded_bytes = 0_usize;
    for entity in image.canonical_entities() {
        let document =
            prepare_semantic_document(artifact.recipe.profile, &image, entity.id, type_limits)
                .map_err(|source| PackageDocumentationError::Render { source })?;
        encoded_bytes = encoded_bytes.checked_add(document.encoded_len).ok_or(
            PackageDocumentationError::OutputLengthOverflow {
                entity: entity.id,
                accumulated: encoded_bytes,
                next: document.encoded_len,
            },
        )?;
        if encoded_bytes > MAX_RENDERED_SEMANTIC_BYTES {
            return Err(PackageDocumentationError::OutputBudget {
                entity: entity.id,
                required: encoded_bytes,
                maximum: MAX_RENDERED_SEMANTIC_BYTES,
            });
        }
        prepared.push(document);
    }

    let mut documents = Vec::new();
    documents
        .try_reserve_exact(prepared.len())
        .map_err(|source| PackageDocumentationError::DocumentAllocation {
            entities: prepared.len(),
            source,
        })?;
    for document in prepared {
        let mut output = Vec::new();
        output
            .try_reserve_exact(document.encoded_len)
            .map_err(|source| PackageDocumentationError::OutputAllocation {
                entity: document.entity,
                bytes: document.encoded_len,
                source,
            })?;
        output.resize(document.encoded_len, 0);
        let observed = document
            .write_into(&mut output)
            .map_err(|source| PackageDocumentationError::Render { source })?
            .len();
        if observed != document.encoded_len {
            return Err(PackageDocumentationError::WrittenExtent {
                entity: document.entity,
                promised: document.encoded_len,
                observed,
            });
        }
        let text = String::from_utf8(output).map_err(|source| {
            PackageDocumentationError::OutputEncoding {
                entity: document.entity,
                source,
            }
        })?;
        documents.push(RenderedSemanticDocument {
            entity: document.entity,
            text: Arc::from(text.into_boxed_str()),
        });
    }

    Ok(PackageDocumentationProjection {
        correlation,
        semantic_image: artifact.semantic_image,
        profile: artifact.recipe.profile,
        census,
        encoded_bytes,
        documents: documents.into_boxed_slice(),
    })
}
