//! Durable compiler publication from driver-issued compact fragments only.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::RecvError,
};

use nudox_durable_journal::{PublicationFacts, SharedPublicationFailure};
use nudox_hydration::VerifiedGenerationFacts;
use nudox_ir_format::FragmentRangeManifestError;
use thiserror::Error;

use crate::{
    binding::{COMPILATION_BINDING_BYTES, CompilationBindingFacts, CompilationBindingWriteError},
    binding_store::BindingStoreError,
    generation::GenerationBuildError,
    immutable::ImmutableArtifactError,
    manifest::{
        CompilationManifestFacts, CompilationManifestView, CompilationPrepareError,
        CompilationWriteError, StoredFragmentFacts,
    },
    manifest_store::ImmutableManifestError,
};

/// Caller-owned bounded buffers for one synchronous compiler publication.
pub struct PublicationScratch<'manifest, 'facts, 'order, 'locality, 'binding> {
    /// Output for the complete canonical package manifest.
    pub manifest_output: &'manifest mut [u8],
    /// Initialized-option fact slots for every validated manifest entry.
    pub manifest_facts: &'facts mut [Option<StoredFragmentFacts>],
    /// Reusable caller-owned canonical input ordinals.
    pub ordinals: &'order mut [usize],
    /// Output for all-resident generation-locality bytes.
    pub locality_output: &'locality mut [u8],
    /// Exact fixed output for the pre-journal immutable generation binding.
    pub binding_output: &'binding mut [u8],
}

/// Bounded caller-owned cancellation observation for synchronous publication checkpoints.
#[derive(Clone, Copy, Debug)]
pub enum PublishControl<'cancel> {
    /// Do not cancel this synchronous publication.
    Continue,
    /// Observe this caller-owned flag before storage, before admission, and immediately after admission.
    Observe(&'cancel AtomicBool),
    /// Return before any artifact directory or immutable byte is created.
    CancelBeforeStorage,
    /// Return after immutable artifacts and their binding are durable, but before journal admission.
    CancelBeforeAdmission,
    /// Request cancellation immediately after journal admission; completion wins if already stable.
    CancelAfterAdmission,
}

impl PublishControl<'_> {
    pub(super) fn cancelled_before_storage(self) -> bool {
        match self {
            Self::CancelBeforeStorage => true,
            Self::Observe(cancelled) => cancelled.load(Ordering::Acquire),
            Self::Continue | Self::CancelBeforeAdmission | Self::CancelAfterAdmission => false,
        }
    }

    pub(super) fn cancelled_before_admission(self) -> bool {
        match self {
            Self::CancelBeforeAdmission => true,
            Self::Observe(cancelled) => cancelled.load(Ordering::Acquire),
            Self::Continue | Self::CancelBeforeStorage | Self::CancelAfterAdmission => false,
        }
    }

    pub(super) fn cancelled_after_admission(self) -> bool {
        match self {
            Self::CancelAfterAdmission => true,
            Self::Observe(cancelled) => cancelled.load(Ordering::Acquire),
            Self::Continue | Self::CancelBeforeStorage | Self::CancelBeforeAdmission => false,
        }
    }
}

/// Caller-owned buffers used to reopen and verify the durable compiler-selected package.
pub struct OpenPublicationScratch<'manifest, 'facts, 'fragments, 'locality> {
    /// Output for the complete immutable package manifest.
    pub manifest_output: &'manifest mut [u8],
    /// Initialized-option fact slots for every decoded canonical manifest entry.
    pub manifest_facts: &'facts mut [Option<StoredFragmentFacts>],
    /// Exact concatenated output for every complete referenced fragment in canonical manifest order.
    pub fragment_output: &'fragments mut [u8],
    /// Output for all-resident generation-locality bytes during root reconstruction.
    pub locality_output: &'locality mut [u8],
}

/// Stable compiler publication facts exposed only after durable journal success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishedCompilation {
    /// Stable durable-journal receipt and exact selected generation facts.
    pub publication: PublicationFacts,
    /// Immutable package-manifest facts included in the selected generation closure.
    pub manifest: CompilationManifestFacts,
    /// Immutable binding facts that were persisted before submission and matched post-stable facts.
    pub binding: CompilationBindingFacts,
}

/// A durable selected compiler package with its borrowed, fully validated canonical manifest.
pub struct OpenedCompilation<'manifest, 'facts> {
    /// Stable journal facts from which this package was exclusively selected.
    pub publication: PublicationFacts,
    /// Immutable generation-to-manifest binding facts addressed by `publication.generation`.
    pub binding: CompilationBindingFacts,
    /// Borrowed canonical manifest validated along with every referenced fragment and generation root.
    pub manifest: CompilationManifestView<'manifest, 'facts>,
}

/// Exact attempted compiler publication facts retained when journal publication did not commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UncommittedPublicationFacts {
    /// Verified complete generation submitted or offered to the durable publisher.
    pub generation: VerifiedGenerationFacts,
    /// Complete immutable package manifest facts.
    pub manifest: CompilationManifestFacts,
    /// Exact pre-journal immutable binding facts.
    pub binding: CompilationBindingFacts,
}

/// Journal terminal after immutable compiler bytes existed but before a stable publication receipt.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UncommittedPublication {
    /// Bounded durable publisher admission was full; no compiler generation became selected.
    #[error("compiler publication admission is full")]
    Full {
        /// Exact attempted facts retained without local store/proof borrows.
        attempted: UncommittedPublicationFacts,
    },
    /// Durable publisher admission was closed; no compiler generation became selected.
    #[error("compiler publication owner is closed")]
    Closed {
        /// Exact attempted facts retained without local store/proof borrows.
        attempted: UncommittedPublicationFacts,
    },
    /// Durable publication failed with its shared exact journal or physical terminal source.
    #[error("compiler publication did not establish a stable durable receipt")]
    Failed {
        /// Exact attempted facts retained without local store/proof borrows.
        attempted: UncommittedPublicationFacts,
        /// Exact source-bearing durable terminal.
        #[source]
        source: SharedPublicationFailure,
    },
    /// Durable owner disappeared before a terminal could be observed.
    #[error("compiler publication owner disappeared before a stable receipt")]
    OwnerLost {
        /// Exact attempted facts retained without local store/proof borrows.
        attempted: UncommittedPublicationFacts,
        /// Exact receive terminal.
        #[source]
        source: RecvError,
    },
    /// Cancellation won after durable-journal admission but before any selected publication effect.
    #[error("admitted compiler publication was cancelled before a stable receipt")]
    Cancelled {
        /// Exact attempted facts retained without local store/proof borrows.
        attempted: UncommittedPublicationFacts,
    },
    /// Cancellation was observed after immutable artifacts and binding sync, before journal admission.
    #[error("compiler publication was cancelled before durable-journal admission")]
    CancelledBeforeAdmission {
        /// Exact immutable attempted facts; the artifacts may remain unreachable.
        attempted: UncommittedPublicationFacts,
    },
}

/// Rejection while publishing compiler-driver output through immutable storage and a durable head.
#[derive(Debug, Error)]
#[allow(
    clippy::large_enum_variant,
    reason = "cold exact fragment mismatch retains both complete 404-byte package facts without an allocation"
)]
#[non_exhaustive]
#[allow(
    missing_docs,
    reason = "each field repeats the exact fact documented by its enclosing terminal"
)]
pub enum PublishCompiledError {
    #[error("compiler publication was cancelled before immutable artifact storage")]
    CancelledBeforeStorage,
    #[error("compiler outputs could not form a canonical package")]
    Canonical(#[source] CompilationPrepareError),
    #[error("canonical package manifest could not be written")]
    ManifestWrite(#[source] CompilationWriteError),
    #[error("compiler fragment {ordinal} could not enter immutable storage")]
    FragmentStorage {
        ordinal: usize,
        #[source]
        source: ImmutableArtifactError,
    },
    #[error("compiler fragment storage owner could not open")]
    FragmentStorageOwner(#[source] ImmutableArtifactError),
    #[error("compiler fragment {ordinal} could not derive its full range manifest")]
    FragmentManifest {
        ordinal: usize,
        #[source]
        source: FragmentRangeManifestError,
    },
    #[error("canonical package manifest could not enter immutable storage")]
    ManifestStorage(#[source] ImmutableManifestError),
    #[error("compiler package could not construct a verified complete generation")]
    Generation(#[source] GenerationBuildError),
    #[error("pre-journal compiler generation binding could not be written")]
    BindingWrite(#[source] CompilationBindingWriteError),
    #[error("pre-journal compiler generation binding could not enter immutable storage")]
    BindingStorage(#[source] BindingStoreError),
    #[error("compiler publication did not commit")]
    Uncommitted(#[source] UncommittedPublication),
    #[error("durable receipt selected generation facts that differ from the pre-journal binding")]
    StableGenerationMismatch {
        binding: CompilationBindingFacts,
        publication: PublicationFacts,
    },
    #[error("compiler binding output has {observed} bytes, requires {COMPILATION_BINDING_BYTES}")]
    BindingOutputLength { observed: usize },
}

/// Failure while reopening the durable compiler-selected package.
#[derive(Debug, Error)]
#[allow(
    clippy::large_enum_variant,
    reason = "cold reopen conflicts retain both complete 404-byte validated facts without an allocation or source erasure"
)]
#[non_exhaustive]
#[allow(
    missing_docs,
    reason = "each field repeats the exact fact documented by its enclosing terminal"
)]
pub enum OpenPublishedError {
    #[error("durable journal published facts could not be read")]
    Journal(#[source] nudox_durable_journal::PublicationOpenError),
    #[error("durable selected compiler generation has no immutable binding")]
    MissingBinding { generation: VerifiedGenerationFacts },
    #[error("durable selected compiler binding could not be opened")]
    Binding(#[source] BindingStoreError),
    #[error("durable selected compiler binding names a different generation")]
    BindingGeneration {
        publication: PublicationFacts,
        binding: CompilationBindingFacts,
    },
    #[error("durable selected compiler manifest could not be opened")]
    Manifest(#[source] ImmutableManifestError),
    #[error("compiler manifest fragment output has {available} bytes, requires {required}")]
    FragmentOutputTooSmall { required: usize, available: usize },
    #[error("compiler manifest fragment {fragment:?} length cannot fit this address space")]
    FragmentOutputLengthAddressSpace {
        fragment: nudox_id::ArtifactId<nudox_id::IrFragmentEncoding, nudox_id::IrFragmentDomain>,
        #[source]
        source: core::num::TryFromIntError,
    },
    #[error("compiler manifest fragment byte sum overflowed native address space")]
    FragmentOutputLengthOverflow,
    #[error("durable selected compiler fragment {ordinal} could not be opened")]
    Fragment {
        ordinal: usize,
        #[source]
        source: ImmutableArtifactError,
    },
    #[error("durable selected compiler fragment {ordinal} could not commit all semantic ranges")]
    FragmentRanges {
        ordinal: usize,
        #[source]
        source: FragmentRangeManifestError,
    },
    #[error("durable selected compiler fragment facts differ from its package manifest")]
    FragmentFacts {
        ordinal: usize,
        expected: StoredFragmentFacts,
        observed: StoredFragmentFacts,
    },
    #[error("durable selected compiler package could not rebuild its verified generation")]
    Generation(#[source] GenerationBuildError),
    #[error(
        "durable selected compiler generation differs from the rebuilt immutable package closure"
    )]
    GenerationMismatch {
        publication: PublicationFacts,
        binding: CompilationBindingFacts,
        rebuilt: VerifiedGenerationFacts,
    },
}
