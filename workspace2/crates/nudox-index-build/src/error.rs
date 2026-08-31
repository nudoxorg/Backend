use core::num::TryFromIntError;

use nudox_index_core::{ExactSegmentError, LexicalSegmentError};
use nudox_ir_vocab::{AtomId, EntityId, TypeId};
use thiserror::Error;

/// A caller-owned region required by one bounded compiler-to-index build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildRegion {
    /// Sortable semantic declarations before canonical ordering.
    Projections,
    /// Typed canonical entity output facts.
    Entities,
    /// Existing exact-core rows.
    ExactRows,
    /// Existing lexical-core rows.
    LexicalRows,
    /// Direct atom lookup table.
    Atoms,
    /// Direct type-node lookup table.
    TypeNodes,
}

/// A compiler-to-index build failure retaining its exact owner and cause.
#[derive(Debug, Error)]
pub enum BuildError<'bytes> {
    /// Preflight rejected caller capacity before any caller slot was written.
    #[error(transparent)]
    Admission(#[from] BuildAdmissionError),
    /// A canonical builder ordinal could not be represented by the compiler entity identity.
    #[error("canonical entity ordinal {ordinal} does not fit compiler entity identity")]
    EntityOrdinalAddressSpace {
        /// Host ordinal generated after canonical entity ordering.
        ordinal: usize,
        /// Original checked conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// A host-sized length could not be represented by the canonical index identity stream.
    #[error("{field:?} length {observed} does not fit canonical index identity")]
    CanonicalLengthAddressSpace {
        /// The exact canonical record whose length could not be encoded.
        field: CanonicalLengthField,
        /// Complete host-sized length observed before hashing.
        observed: usize,
        /// Original checked conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// One internal caller-output initialization stage diverged from the admitted cardinality.
    #[error("{region:?} initialization required {required} entries; observed {available}")]
    ScratchInitialization {
        /// The stage whose exact source/output cardinality diverged.
        region: BuildRegion,
        /// Entries required by the admitted stage.
        required: usize,
        /// Entries actually available or initialized.
        available: usize,
    },
    /// A validated entity's atom coordinate could not be represented on this target.
    #[error("entity {entity:?} name coordinate {name:?} does not fit this target")]
    AtomAddressSpace {
        /// Entity that named the atom.
        entity: EntityId,
        /// Validated compact-fragment atom coordinate.
        name: AtomId,
        /// Original checked conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// A validated entity's type coordinate could not be represented on this target.
    #[error("entity {entity:?} type coordinate {semantic_type:?} does not fit this target")]
    TypeAddressSpace {
        /// Entity that named the type node.
        entity: EntityId,
        /// Validated compact-fragment type coordinate.
        semantic_type: TypeId,
        /// Original checked conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// The validated entity's atom was absent from the caller lookup region after collection.
    #[error("entity {entity:?} name atom {name:?} was not present in the collected fragment lane")]
    MissingAtom {
        /// Entity that named the missing atom.
        entity: EntityId,
        /// Atom coordinate that could not be recovered.
        name: AtomId,
    },
    /// The validated entity's type was absent from the caller lookup region after collection.
    #[error(
        "entity {entity:?} type node {semantic_type:?} was not present in the collected fragment lane"
    )]
    MissingTypeNode {
        /// Entity that named the missing type node.
        entity: EntityId,
        /// Type coordinate that could not be recovered.
        semantic_type: TypeId,
    },
    /// Existing exact-core validation rejected the derived canonical rows.
    #[error("exact-core rejected the derived canonical rows")]
    Exact {
        /// Exact existing-core rejection, including any borrowed offending row evidence.
        cause: ExactSegmentError<'bytes>,
    },
    /// Existing lexical-core validation rejected the derived canonical rows.
    #[error("lexical-core rejected the derived canonical rows")]
    Lexical {
        /// Exact existing-core rejection, including any borrowed offending row evidence.
        cause: LexicalSegmentError<'bytes>,
    },
}

/// A length-bearing component of the canonical fragment namespace stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalLengthField {
    /// Number of canonical entity projections.
    ProjectionCount,
    /// Number of bytes in one canonical entity name.
    EntityName,
    /// Number of bytes in one fixed namespace name chunk.
    EntityNameChunk,
}

/// A no-write rejection from caller scratch admission.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum BuildAdmissionError {
    /// A fragment's entity count exceeds the shared exact and lexical segment bound.
    #[error("fragment has {observed} entities; shared segment capacity is {maximum}")]
    EntityLimit {
        /// Shared maximum entities accepted by one index segment.
        maximum: usize,
        /// Complete entity count observed before writing caller output.
        observed: usize,
    },
    /// A caller-owned region is too small; preflight has not modified any region.
    #[error("{region:?} requires {required} entries; caller provided {available}")]
    OutputTooSmall {
        /// The precise insufficient region.
        region: BuildRegion,
        /// Complete entries required for this fragment.
        required: usize,
        /// Entries supplied by the caller.
        available: usize,
    },
}
