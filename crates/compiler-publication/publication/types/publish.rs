//! Publish-admission facts, caller-owned preparation buffers, and exact terminals.
//!
//! This module owns the state before and through durable admission.  Reopen rows,
//! borrowed artifact cursors, and their integrity terminals live in the sibling
//! module so submit-time cancellation cannot be confused with reopen corruption.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::RecvError,
};

use backend_semantic::ir::{
    FragmentRangeManifestError, RecipeFact, SemanticImageEncodeError, SemanticImageReopenError,
    SourceIdentity,
};
use backend_store::hydration::VerifiedGenerationFacts;
use backend_store::journal::{PublicationFacts, SharedPublicationFailure};
use thiserror::Error;

use crate::{
    binding::{COMPILATION_BINDING_BYTES, CompilationBindingFacts, CompilationBindingWriteError},
    binding_store::BindingStoreError,
    generation::GenerationBuildError,
    immutable::ImmutableArtifactError,
    manifest::{
        CompilationManifestFacts, CompilationPrepareError, CompilationWriteError,
        StoredFragmentFacts,
    },
    manifest_store::ImmutableManifestError,
    semantic_immutable::ImmutableSemanticImageError,
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

/// Caller-owned bounded outputs for one fused semantic publication.
pub struct SemanticPublicationScratch<
    'manifest,
    'facts,
    'order,
    'plan,
    'semantic,
    'locality,
    'binding,
> {
    /// Output for the complete schema-2 package manifest.
    pub manifest_output: &'manifest mut [u8],
    /// Decoded manifest facts proving every paired artifact row.
    pub manifest_facts: &'facts mut [Option<StoredFragmentFacts>],
    /// Reusable caller-owned canonical input ordinals.
    pub ordinals: &'order mut [usize],
    /// Reusable exact per-image extent plan, one slot per compiler output.
    pub semantic_image_plan: &'plan mut [crate::manifest::SemanticImageRegion],
    /// Concatenated complete semantic-image output in caller input order.
    pub semantic_image_output: &'semantic mut [u8],
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
    pub(crate) fn cancelled_before_storage(self) -> bool {
        match self {
            Self::CancelBeforeStorage => true,
            Self::Observe(cancelled) => cancelled.load(Ordering::Acquire),
            Self::Continue | Self::CancelBeforeAdmission | Self::CancelAfterAdmission => false,
        }
    }

    pub(crate) fn cancelled_before_admission(self) -> bool {
        match self {
            Self::CancelBeforeAdmission => true,
            Self::Observe(cancelled) => cancelled.load(Ordering::Acquire),
            Self::Continue | Self::CancelBeforeStorage | Self::CancelAfterAdmission => false,
        }
    }

    pub(crate) fn cancelled_after_admission(self) -> bool {
        match self {
            Self::CancelAfterAdmission => true,
            Self::Observe(cancelled) => cancelled.load(Ordering::Acquire),
            Self::Continue | Self::CancelBeforeStorage | Self::CancelBeforeAdmission => false,
        }
    }
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

/// Rejection while publishing fused compact and full semantic compiler output.
#[derive(Debug, Error)]
#[non_exhaustive]
#[allow(
    missing_docs,
    reason = "each variant retains the exact typed semantic-publication boundary and source"
)]
pub enum PublishSemanticError {
    #[error("semantic publication was cancelled before image preparation or storage")]
    CancelledBeforeStorage,
    #[error("semantic image plan has {available} slots, requires {required}")]
    ImagePlanTooSmall { required: usize, available: usize },
    #[error("semantic image {ordinal} could not be measured")]
    ImageMeasure {
        ordinal: usize,
        #[source]
        source: SemanticImageEncodeError,
    },
    #[error("semantic image {ordinal} has {observed} bytes, exceeding its durable length width")]
    ImageLengthAddressSpace {
        ordinal: usize,
        observed: usize,
        #[source]
        source: core::num::TryFromIntError,
    },
    #[error("semantic image {ordinal} durable length {observed} does not fit this address space")]
    ImagePlanLengthAddressSpace {
        ordinal: usize,
        observed: u32,
        #[source]
        source: core::num::TryFromIntError,
    },
    #[error("semantic image {ordinal} extent overflowed after byte {offset}")]
    ImageExtentOverflow {
        ordinal: usize,
        offset: usize,
        length: usize,
    },
    #[error("semantic image output has {available} bytes, requires {required}")]
    ImageOutputTooSmall { required: usize, available: usize },
    #[error("semantic image {ordinal} could not be encoded")]
    ImageEncode {
        ordinal: usize,
        #[source]
        source: SemanticImageEncodeError,
    },
    #[error("semantic image {ordinal} wrote {observed} bytes after promising exactly {expected}")]
    ImageWriteLengthMismatch {
        ordinal: usize,
        expected: usize,
        observed: usize,
    },
    #[error("semantic image {ordinal} failed its independent full reopen")]
    ImageReopen {
        ordinal: usize,
        #[source]
        source: SemanticImageReopenError,
    },
    #[error("semantic image {ordinal} has no captured compiler provenance")]
    ImageProvenanceUnavailable { ordinal: usize },
    #[error("semantic image {ordinal} source differs from its fused compact artifact")]
    ImageSource {
        ordinal: usize,
        expected: SourceIdentity,
        observed: SourceIdentity,
    },
    #[error("semantic image {ordinal} recipe differs from its fused compact artifact")]
    ImageRecipe {
        ordinal: usize,
        expected: RecipeFact,
        observed: RecipeFact,
    },
    #[error("fused compiler outputs could not form a canonical semantic package")]
    Canonical(#[source] CompilationPrepareError),
    #[error("canonical semantic package manifest could not be written")]
    ManifestWrite(#[source] CompilationWriteError),
    #[error("semantic compiler fragment storage owner could not open")]
    FragmentStorageOwner(#[source] ImmutableArtifactError),
    #[error("semantic compiler fragment {ordinal} could not enter immutable storage")]
    FragmentStorage {
        ordinal: usize,
        #[source]
        source: ImmutableArtifactError,
    },
    #[error("semantic compiler fragment {ordinal} could not derive its full range manifest")]
    FragmentManifest {
        ordinal: usize,
        #[source]
        source: FragmentRangeManifestError,
    },
    #[error("semantic image storage owner could not open")]
    SemanticStorageOwner(#[source] ImmutableSemanticImageError),
    #[error("semantic image {ordinal} could not enter immutable storage")]
    SemanticStorage {
        ordinal: usize,
        #[source]
        source: ImmutableSemanticImageError,
    },
    #[error("semantic image {ordinal} storage facts differ from its measured encoded region")]
    SemanticStorageFacts {
        ordinal: usize,
        expected: crate::semantic_immutable::SemanticImageArtifactFacts,
        observed: crate::semantic_immutable::SemanticImageArtifactFacts,
    },
    #[error("semantic publication failed after all paired artifacts were prepared")]
    Publication(#[source] PublishCompiledError),
}
