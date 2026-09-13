//! Defines locality artifact errors behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the locality artifact errors invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::num::TryFromIntError;

use heart_identity::{ContentAuthorityError, ContentIdDecodeError, GenerationId};
use heart_object::ProviderSetError;
use heart_schema::UnknownSchemaId;
use thiserror::Error;

use crate::{EntryKey, MetadataBytes, RootEntryCount};

/// Exact region in the typed sorted-locality artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalityRegion {
    /// Fixed typed generation/count header.
    Header,
    /// Sorted compact root-row coordinates.
    ExceptionRows,
    /// Promise-class membership bits.
    PromiseBits,
    /// Omitted-zero promise rank directory.
    PromiseRanks,
    /// Non-empty provider values by promise rank.
    Providers,
    /// Present-remote-base membership bits.
    OverlayPresenceBits,
    /// Omitted-zero overlay rank directory.
    OverlayPresenceRanks,
    /// Canonical descriptor records by present-overlay rank.
    PresentOverlays,
    /// Shared remote generation for every overlay exception.
    OverlayBasis,
}

/// Validation or input-measure rejection for one locality artifact.
#[derive(Debug, Error, PartialEq)]
pub enum LocalityError {
    /// Exact lane geometry passed, but the bytes could not inhabit the typed
    /// wire representation and no public semantic violation explained it.
    #[error("locality {region:?} bytes violated their typed wire representation")]
    TypedLaneInvariant {
        /// Typed lane whose representation proof failed.
        region: LocalityRegion,
    },
    /// The fixed header carried a generation identity from another closed domain.
    #[error("locality generation identity failed checked decode")]
    Generation(#[from] ContentIdDecodeError),
    /// The artifact-global descriptor domain differs from the requested typed view.
    #[error("locality descriptor authority failed checked decode")]
    ContentDomain {
        /// Checked header-authority rejection with both complete operands.
        #[from]
        source: ContentAuthorityError,
    },
    /// One required artifact region ends beyond the supplied byte range.
    #[error("locality {region:?} needs {required:?} bytes but only {available:?} are available")]
    Truncated {
        /// Region whose exact end could not be borrowed.
        region: LocalityRegion,
        /// Complete prefix required through that region.
        required: MetadataBytes,
        /// Available artifact bytes.
        available: MetadataBytes,
    },
    /// The artifact contains bytes beyond its header-selected exact grammar.
    #[error("locality has {actual:?} bytes but exact layout requires {expected:?}")]
    TrailingBytes {
        /// Exact grammar extent.
        expected: MetadataBytes,
        /// Supplied artifact extent.
        actual: MetadataBytes,
    },
    /// A fixed-width lane cannot fit the host's addressable metadata space.
    #[error("locality layout overflows in {region:?} for count {count}")]
    LayoutOverflow {
        /// Lane whose count/width overflowed.
        region: LocalityRegion,
        /// Semantic lane element count.
        count: u32,
    },
    /// The complete fixed grammar does not fit this host's addressable bytes.
    #[error("locality layout needs {attempted} bytes, which this host cannot address")]
    LayoutAddressSpace {
        /// Exact wide-arithmetic byte extent.
        attempted: u64,
        /// Host-size conversion source.
        #[source]
        source: TryFromIntError,
    },
    /// Promise class cardinality exceeds sparse exception cardinality.
    #[error("locality declares {promises} promises among {exceptions} exceptions")]
    PromiseCountExceedsExceptions {
        /// Promise payload count.
        promises: u32,
        /// Sparse exception row count.
        exceptions: u32,
    },
    /// Descriptor payload cardinality exceeds overlay class cardinality.
    #[error("locality declares {present} present overlays among {overlays} overlays")]
    PresentCountExceedsOverlays {
        /// Descriptor payload count.
        present: u32,
        /// Overlay-class exception count.
        overlays: u32,
    },
    /// Sparse exception cardinality exceeds the bound semantic root.
    #[error("locality declares {exceptions} exception rows for a {root_count}-row root")]
    ExceptionCountExceedsRoot {
        /// Sparse exception row count.
        exceptions: u32,
        /// Header root row count.
        root_count: u32,
    },
    /// A sparse row coordinate is outside the bound root.
    #[error("locality exception row {row} at ordinal {ordinal} exceeds root count {root_count}")]
    ExceptionRowOutOfRange {
        /// Sparse exception ordinal.
        ordinal: u32,
        /// Observed compact row coordinate.
        row: u32,
        /// Bound root cardinality.
        root_count: u32,
    },
    /// Sparse coordinates are not a strict canonical set.
    #[error("locality exception rows are not strictly increasing at ordinal {ordinal}")]
    ExceptionRowsNotStrict {
        /// Sparse exception ordinal.
        ordinal: u32,
        /// Previous observed row coordinate.
        previous: u32,
        /// Current observed row coordinate.
        current: u32,
    },
    /// A canonical tail byte carries non-semantic membership bits.
    #[error("locality {region:?} has nonzero unused bits in byte {byte}: {observed:#04x}")]
    NonzeroUnusedBits {
        /// Membership lane.
        region: LocalityRegion,
        /// Tail byte ordinal within that lane.
        byte: usize,
        /// Complete observed tail-byte value.
        observed: u8,
    },
    /// Promise membership population differs from the header payload count.
    #[error("locality promise bits count {observed} differs from header {declared}")]
    PromiseCountMismatch {
        /// Header payload count.
        declared: u32,
        /// Single-pass membership population.
        observed: u32,
    },
    /// Present-overlay population differs from the header descriptor count.
    #[error("locality overlay presence count {observed} differs from header {declared}")]
    PresentOverlayCountMismatch {
        /// Header descriptor count.
        declared: u32,
        /// Single-pass membership population.
        observed: u32,
    },
    /// A rank-directory prefix differs from the population preceding it.
    #[error("locality {region:?} rank block {block} contains {observed}, expected {expected}")]
    RankPrefixMismatch {
        /// Rank lane.
        region: LocalityRegion,
        /// Retained rank-directory ordinal (block zero is omitted).
        block: u32,
        /// Observed encoded prefix.
        observed: u32,
        /// Expected monotone prefix.
        expected: u32,
    },
    /// A promised provider lane stores the invalid empty provider bitmap.
    #[error("locality provider ordinal {ordinal} is empty")]
    EmptyProvider {
        /// Promise-rank ordinal.
        ordinal: u32,
        /// Raw provider bitmap retained for diagnostics.
        observed: u64,
        /// Non-empty provider-set invariant rejection.
        #[source]
        source: ProviderSetError,
    },
    /// A present descriptor names an unknown closed schema.
    #[error("locality present overlay ordinal {ordinal} has an unknown schema")]
    PresentOverlaySchema {
        /// Present-overlay rank ordinal.
        ordinal: u32,
        /// Closed-vocabulary rejection.
        #[source]
        source: UnknownSchemaId,
    },
    /// Input rows were not supplied in strict canonical key order.
    #[error("locality exception keys {previous:?} then {actual:?} are not strictly increasing")]
    InputOrder {
        /// Previous input key.
        previous: EntryKey,
        /// Current input key.
        actual: EntryKey,
    },
    /// A root-issued row belongs to a different immutable generation.
    #[error(
        "locality row {key:?} belongs to generation {row_generation:?}, not root {root_generation:?}"
    )]
    InputGenerationMismatch {
        /// Semantic row key.
        key: EntryKey,
        /// Generation named by the row proof.
        row_generation: GenerationId,
        /// Generation accepted by preparation.
        root_generation: GenerationId,
    },
    /// A root-issued row was issued from a root with a different cardinality.
    #[error("locality row {key:?} belongs to a {row_count:?}-row root, not {root_count:?}")]
    InputRootCountMismatch {
        /// Semantic row key.
        key: EntryKey,
        /// Root cardinality named by the row proof.
        row_count: RootEntryCount,
        /// Root cardinality accepted by preparation.
        root_count: RootEntryCount,
    },
    /// A coordinate proof does not resolve to its stated semantic key.
    #[error("locality row proof for {expected:?} resolves to {actual:?}")]
    InputRowKeyMismatch {
        /// Key carried by the input proof.
        expected: EntryKey,
        /// Key in the bound root at that coordinate.
        actual: EntryKey,
    },
    /// A validated locality artifact is bound to a different semantic root.
    #[error("locality generation {locality_generation:?} did not match root {root_generation:?}")]
    GenerationMismatch {
        /// Artifact generation binding.
        locality_generation: GenerationId,
        /// Supplied root generation.
        root_generation: GenerationId,
    },
    /// A validated locality artifact names a different root cardinality.
    #[error("locality root count {locality_count:?} did not match root {root_count:?}")]
    RootCountMismatch {
        /// Artifact root count.
        locality_count: RootEntryCount,
        /// Supplied root count.
        root_count: RootEntryCount,
    },
    /// Inputs from one coherent overlay layer name different shared bases.
    #[error("locality overlay {key:?} has basis {actual:?}, expected {expected:?}")]
    MixedOverlayBasis {
        /// Offending semantic key.
        key: EntryKey,
        /// First observed shared basis.
        expected: GenerationId,
        /// Conflicting base.
        actual: GenerationId,
    },
}

/// Direct locality emission failure.
#[derive(Debug, Error, PartialEq)]
pub enum LocalityWriteError {
    /// Caller output is shorter than the already measured exact layout.
    #[error("locality output has {available:?} bytes but requires {required:?}")]
    OutputTooSmall {
        /// Exact measured locality bytes.
        required: MetadataBytes,
        /// Caller-provided capacity.
        available: MetadataBytes,
    },
    /// The private direct encoder failed its validation oracle.
    #[error("direct locality encoder violated its validated grammar")]
    Invariant(#[from] LocalityError),
}
