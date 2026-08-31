//! Durable compiler publication from driver-issued compact fragments only.

use core::ops::Deref;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::RecvError,
};

use nudox_durable_journal::{PublicationFacts, SharedPublicationFailure};
use nudox_hydration::VerifiedGenerationFacts;
use nudox_ir_format::{
    FragmentError, FragmentRange, FragmentRangeManifest, FragmentRangeManifestError, FragmentView,
    RecipeFact, SectionKind, SourceIdentity,
};
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
    pub(super) fragments: &'manifest [u8],
}

impl<'manifest, 'facts> OpenedCompilation<'manifest, 'facts> {
    /// Reconstructs manifest-named IR views from the exact immutable bytes verified during reopen.
    ///
    /// The cursor derives each region from the canonical manifest's typed fragment length.  It
    /// independently validates the fragment grammar and all six committed semantic ranges before
    /// yielding it, so a consumer cannot pair a manifest fact with arbitrary caller bytes.
    pub fn fragments(&self) -> OpenedFragmentCursor<'_, 'manifest> {
        OpenedFragmentCursor {
            facts: self.manifest.fragments(),
            bytes: self.fragments,
            offset: 0,
            ordinal: 0,
            failed: false,
        }
    }
}

/// One manifest-named compact IR fragment borrowed from an opened durable compilation.
pub struct OpenedFragment<'fragment> {
    view: OpenedFragmentView<'fragment>,
}

impl<'fragment> Deref for OpenedFragment<'fragment> {
    type Target = OpenedFragmentView<'fragment>;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

/// Immutable readable facts of one exact manifest-named reopened IR fragment.
///
/// This projection deliberately has no public constructor.  Only [`OpenedCompilation::fragments`]
/// can bind its paired manifest fact and compact IR view after validating their exact bytes.
pub struct OpenedFragmentView<'fragment> {
    /// Complete canonical facts selected by the immutable package manifest.
    pub facts: StoredFragmentFacts,
    /// The exact validated compact IR bytes committed by `facts.fragment`.
    pub view: FragmentView<'fragment>,
}

/// Cursor over immutable fragments selected by one opened durable compiler publication.
pub struct OpenedFragmentCursor<'opened, 'fragments> {
    facts: crate::manifest::CompilationManifestEntries<'opened>,
    bytes: &'fragments [u8],
    offset: usize,
    ordinal: usize,
    failed: bool,
}

impl<'opened, 'fragments> Iterator for OpenedFragmentCursor<'opened, 'fragments> {
    type Item = Result<OpenedFragment<'fragments>, OpenedFragmentError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let facts = self.facts.next()?;
        let ordinal = self.ordinal;
        let Some(next_ordinal) = ordinal.checked_add(1) else {
            self.failed = true;
            return Some(Err(OpenedFragmentError::OrdinalOverflow { facts }));
        };
        self.ordinal = next_ordinal;
        match opened_fragment(self.bytes, self.offset, ordinal, facts) {
            Ok((fragment, next_offset)) => {
                self.offset = next_offset;
                Some(Ok(fragment))
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

impl core::iter::FusedIterator for OpenedFragmentCursor<'_, '_> {}

/// Rejection while reconstructing one manifest-named borrowed fragment view.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OpenedFragmentError {
    /// Advancing the host-side manifest ordinal overflowed before a fragment could be exposed.
    #[error("opened compiler fragment ordinal overflowed this address space")]
    OrdinalOverflow {
        /// Exact manifest facts that could not receive a host ordinal.
        facts: StoredFragmentFacts,
    },
    /// The immutable manifest's complete fragment length does not fit this address space.
    #[error("opened compiler fragment {ordinal} length cannot fit this address space")]
    LengthAddressSpace {
        /// Canonical manifest position.
        ordinal: usize,
        /// Exact manifest facts for the rejected fragment.
        facts: StoredFragmentFacts,
        /// Exact address-space conversion source.
        #[source]
        source: core::num::TryFromIntError,
    },
    /// Deriving the manifest-named byte interval overflowed native address space.
    #[error("opened compiler fragment {ordinal} range overflows this address space")]
    RangeOverflow {
        /// Canonical manifest position.
        ordinal: usize,
        /// Exact manifest facts for the rejected fragment.
        facts: StoredFragmentFacts,
        /// Validated output offset before this fragment.
        offset: usize,
        /// Required complete fragment length.
        length: usize,
    },
    /// The exact reopened region was shorter than the immutable manifest requires.
    #[error(
        "opened compiler fragment {ordinal} has {available} bytes remaining, requires {required}"
    )]
    RegionTruncated {
        /// Canonical manifest position.
        ordinal: usize,
        /// Exact manifest facts for the rejected fragment.
        facts: StoredFragmentFacts,
        /// Bytes remaining in the immutable reopened region.
        available: usize,
        /// Complete bytes required by the immutable manifest.
        required: usize,
    },
    /// The immutable reopened region no longer satisfies the compact IR grammar.
    #[error("opened compiler fragment {ordinal} failed compact IR validation")]
    Grammar {
        /// Canonical manifest position.
        ordinal: usize,
        /// Exact manifest facts for the rejected fragment.
        facts: StoredFragmentFacts,
        /// Exact compact-IR grammar rejection.
        #[source]
        source: FragmentError,
    },
    /// The fragment's derived semantic range commitments differ from its package manifest facts.
    #[error(
        "opened compiler fragment {ordinal} semantic range commitments differ from its manifest"
    )]
    Ranges {
        /// Canonical manifest position.
        ordinal: usize,
        /// Exact manifest facts for the rejected fragment.
        facts: StoredFragmentFacts,
        /// Exact compact-IR range derivation rejection.
        #[source]
        source: FragmentRangeManifestError,
    },
    /// The complete reopened fragment facts differ from the canonical package manifest.
    #[error("opened compiler fragment {ordinal} facts differ from its immutable package manifest")]
    Facts {
        /// Canonical manifest position.
        ordinal: usize,
        /// Complete facts selected by the immutable package manifest.
        expected: StoredFragmentFacts,
        /// Exact derived fact that differs while the complete expected package fact is retained.
        mismatch: OpenedFragmentFactMismatch,
    },
}

/// One exact fact that disagreed while reopening a manifest-named immutable fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OpenedFragmentFactMismatch {
    /// The complete content-addressed immutable fragment identity changed.
    FragmentIdentity {
        /// Identity derived from the reopened bytes.
        observed: crate::immutable::FragmentIdentity,
    },
    /// The complete immutable fragment byte length changed.
    Length {
        /// Length derived from the reopened bytes.
        observed: u32,
    },
    /// The source provenance fact changed.
    Source {
        /// Source fact derived from the reopened bytes.
        observed: SourceIdentity,
    },
    /// The compiler recipe fact changed.
    Recipe {
        /// Recipe fact derived from the reopened bytes.
        observed: RecipeFact,
    },
    /// One canonical semantic section commitment changed.
    Range {
        /// Canonical expected section kind.
        section: SectionKind,
        /// Commitment derived from the reopened bytes.
        observed: FragmentRange,
    },
}

#[allow(
    clippy::result_large_err,
    reason = "cold integrity failures retain complete typed manifest facts and compact-IR sources without allocation"
)]
fn opened_fragment<'fragment>(
    bytes: &'fragment [u8],
    offset: usize,
    ordinal: usize,
    facts: StoredFragmentFacts,
) -> Result<(OpenedFragment<'fragment>, usize), OpenedFragmentError> {
    let length = usize::try_from(facts.fragment_length).map_err(|source| {
        OpenedFragmentError::LengthAddressSpace {
            ordinal,
            facts,
            source,
        }
    })?;
    let end = offset
        .checked_add(length)
        .ok_or(OpenedFragmentError::RangeOverflow {
            ordinal,
            facts,
            offset,
            length,
        })?;
    let Some(region) = bytes.get(offset..end) else {
        return Err(OpenedFragmentError::RegionTruncated {
            ordinal,
            facts,
            available: bytes.len().saturating_sub(offset),
            required: length,
        });
    };
    let view = FragmentView::validate(region).map_err(|source| OpenedFragmentError::Grammar {
        ordinal,
        facts,
        source,
    })?;
    let ranges =
        FragmentRangeManifest::from_view(&view).map_err(|source| OpenedFragmentError::Ranges {
            ordinal,
            facts,
            source,
        })?;
    let Some(mismatch) = fragment_fact_mismatch(facts, ranges) else {
        return Ok((
            OpenedFragment {
                view: OpenedFragmentView { facts, view },
            },
            end,
        ));
    };
    Err(OpenedFragmentError::Facts {
        ordinal,
        expected: facts,
        mismatch,
    })
}

fn fragment_fact_mismatch(
    expected: StoredFragmentFacts,
    observed: FragmentRangeManifest,
) -> Option<OpenedFragmentFactMismatch> {
    if observed.fragment != expected.fragment {
        return Some(OpenedFragmentFactMismatch::FragmentIdentity {
            observed: observed.fragment,
        });
    }
    if observed.fragment_length != expected.fragment_length {
        return Some(OpenedFragmentFactMismatch::Length {
            observed: observed.fragment_length,
        });
    }
    if observed.source != expected.source {
        return Some(OpenedFragmentFactMismatch::Source {
            observed: observed.source,
        });
    }
    if observed.recipe != expected.recipe {
        return Some(OpenedFragmentFactMismatch::Recipe {
            observed: observed.recipe,
        });
    }
    for (expected_range, observed_range) in expected.ranges.into_iter().zip(observed.ranges) {
        if expected_range != observed_range {
            return Some(OpenedFragmentFactMismatch::Range {
                section: expected_range.section,
                observed: observed_range,
            });
        }
    }
    None
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
