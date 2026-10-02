//! Turso authority for selected index generations and their query projections.
//!
//! [`TursoAuthority`] owns mutable package/source/branch/environment selection,
//! source observations, and projection watermarks. Immutable packs and complete
//! closures remain in content-addressed storage; catalog, graph, and lexical
//! tables are rebuildable projections bound to the selected logical root.
//! [`TursoProjection`] remains the bounded SQL interface for derived query rows.

#![deny(unsafe_code)]

mod authority;
mod connection;
mod error;
mod graph;
mod package_graph_read;
mod read;
mod schema;
mod writer;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use authority::{
    AttemptInvalidatedByObservationProof, AuthorityError, AuthorityHash, AuthorityNamespace,
    AuthorityPlane, COMPILER_PUBLICATION_ENVELOPE_SCHEMA, COMPILER_PUBLICATION_METADATA_SCHEMA,
    COMPILER_SEMANTIC_IMAGE_SCHEMA, CandidateAttempt, CandidateAttemptRecoveryClaim,
    CandidateAttemptRetirementReason, CandidateGeneration, ClosureClaim, ClosureReceipt,
    CompilerEnvelopeError, CompilerImageMember, CompilerPublicationEnvelope,
    CompilerPublicationMetadata, ExistingGenerationSelection, ProjectionKind, ProjectionWatermark,
    ReopenedCompilerImage, ReopenedCompilerMetadata, ReopenedCompilerPublication, SelectedFrontier,
    SelectedGeneration, SelectionOrigin, SourceObservation, SourceObservationReceipt,
    SourceObservationValue, SupersededAttemptProof, TursoAuthority,
    VERSIONED_PLANE_MANIFEST_SCHEMA, VERSIONED_PLANE_SEGMENT_SCHEMA,
    VersionedPlaneArtifactMetadata, VersionedPlaneError, VersionedPlaneManifestSchema,
    VersionedPlaneMember, VersionedPlaneMetadata, VersionedPlanePublication,
    VersionedPlaneSegmentSchema, reopen_selected_compiler_metadata,
    reopen_selected_compiler_publication,
};
pub use error::ProjectionError;
pub use graph::{PackageGraphSourceSelection, PackageGraphState, RootedPackageGraph};
pub use package_graph_read::PackageGraphReadError;
pub use read::RootedRows;

use std::fmt;

/// Durable path used by the product composition.
pub const FILE_NAME: &str = "projection.turso";

/// Durable selected-head authority path, kept separate from the rebuildable projection.
pub const AUTHORITY_FILE_NAME: &str = "index-authority.turso";

/// Result of aligning the projection to an immutable view root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionUpdate {
    /// The database already named this exact root; no SQL mutation ran.
    Reused {
        /// Number of rows retained without work.
        rows: u64,
    },
    /// A cold or stale database was rebuilt from the supplied root.
    Rebuilt {
        /// Rows materialized from the root.
        rows: u64,
    },
    /// One checked transition advanced the existing projection.
    Advanced {
        /// Rows inserted, replaced, or removed by the transition.
        changed_rows: u64,
    },
}

/// One process-owned Turso accelerator.
///
/// The database handle keeps the process alive while the connection is borrowed
/// by one bounded writer transaction or one read snapshot at a time.
pub struct TursoProjection {
    pub(crate) _database: turso::Database,
    pub(crate) connection: turso::Connection,
}

impl fmt::Debug for TursoProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TursoProjection")
            .finish_non_exhaustive()
    }
}
