//! Durable compiler publication from driver-issued compact fragments only.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::RecvError,
    },
};

use nudox_compile_driver::CompiledFragment;
use nudox_durable_journal::{
    CancelError, DurablePublisher, PublicationError, PublicationFacts, SharedPublicationFailure,
    SubmitError,
};
use nudox_hydration::VerifiedGenerationFacts;
use nudox_ir_format::{FragmentRangeManifest, FragmentRangeManifestError};
use thiserror::Error;

use crate::{
    binding::{
        COMPILATION_BINDING_BYTES, CompilationBindingFacts, CompilationBindingView,
        CompilationBindingWriteError,
    },
    binding_store::{BindingStoreError, GenerationBindingStore},
    generation::{GenerationBuildError, verify_reopened_generation, with_verified_generation},
    immutable::{ImmutableArtifactError, ImmutableArtifactStore},
    manifest::{
        CanonicalCompilation, CompilationManifestFacts, CompilationManifestView,
        CompilationPrepareError, CompilationWriteError, StoredFragmentFacts,
    },
    manifest_store::{ImmutableManifestError, ImmutableManifestStore},
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
    fn cancelled_before_storage(self) -> bool {
        match self {
            Self::CancelBeforeStorage => true,
            Self::Observe(cancelled) => cancelled.load(Ordering::Acquire),
            Self::Continue | Self::CancelBeforeAdmission | Self::CancelAfterAdmission => false,
        }
    }

    fn cancelled_before_admission(self) -> bool {
        match self {
            Self::CancelBeforeAdmission => true,
            Self::Observe(cancelled) => cancelled.load(Ordering::Acquire),
            Self::Continue | Self::CancelBeforeStorage | Self::CancelAfterAdmission => false,
        }
    }

    fn cancelled_after_admission(self) -> bool {
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
    /// A future durable-journal terminal could not be represented without leaking its borrowed witness.
    #[error("durable journal returned an unsupported publication terminal")]
    UnsupportedTerminal {
        /// Exact attempted facts retained without local store/proof borrows.
        attempted: UncommittedPublicationFacts,
    },
    /// A future durable-journal admission terminal could not be represented without leaking its borrowed witness.
    #[error("durable journal returned an unsupported admission terminal")]
    UnsupportedAdmission {
        /// Exact attempted facts retained without local store/proof borrows.
        attempted: UncommittedPublicationFacts,
    },
}

/// Rejection while publishing compiler-driver output through immutable storage and a durable head.
#[derive(Debug, Error)]
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
        facts: Arc<BindingGenerationMismatch>,
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
    FragmentFacts { facts: Arc<FragmentFactsMismatch> },
    #[error("durable selected compiler package could not rebuild its verified generation")]
    Generation(#[source] GenerationBuildError),
    #[error(
        "durable selected compiler generation differs from the rebuilt immutable package closure"
    )]
    GenerationMismatch { facts: Arc<GenerationFactsMismatch> },
}

/// Exact cold conflict between journal-selected generation facts and its deterministic binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindingGenerationMismatch {
    /// Facts selected by the durable journal.
    pub publication: PublicationFacts,
    /// Immutable binding facts addressed by that selected generation.
    pub binding: CompilationBindingFacts,
}

/// Exact cold disagreement between a package entry and its validated on-disk IR fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FragmentFactsMismatch {
    /// Canonical manifest ordinal of the conflicting fragment.
    pub ordinal: usize,
    /// Facts retained by the canonical package manifest.
    pub expected: StoredFragmentFacts,
    /// Facts re-derived from the exact validated fragment bytes.
    pub observed: StoredFragmentFacts,
}

/// Exact cold disagreement between durable/binding facts and a rebuilt complete object closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationFactsMismatch {
    /// Facts selected by the durable journal.
    pub publication: PublicationFacts,
    /// Binding facts addressed by that selected generation.
    pub binding: CompilationBindingFacts,
    /// Facts rebuilt from every immutable manifest and fragment byte.
    pub rebuilt: VerifiedGenerationFacts,
}

/// Publishes only validated outputs issued by `nudox-compile-driver::compile`.
///
/// Fragment and manifest bytes become immutable before a complete object closure is verified. The
/// generation-to-manifest binding is then synced before durable submission. This function blocks
/// only for `PendingPublication::wait`; it never returns a compiler-selected generation before
/// the journal returns a stable receipt.
#[allow(
    clippy::result_large_err,
    reason = "the cold terminal retains exact attempted generation facts and source-bearing durable causes"
)]
pub fn publish_compiled(
    publisher: &DurablePublisher,
    artifact_directory: &Path,
    compiled: &[CompiledFragment<'_>],
    control: PublishControl<'_>,
    scratch: PublicationScratch<'_, '_, '_, '_, '_>,
) -> Result<PublishedCompilation, PublishCompiledError> {
    if control.cancelled_before_storage() {
        return Err(PublishCompiledError::CancelledBeforeStorage);
    }
    if scratch.binding_output.len() < COMPILATION_BINDING_BYTES {
        return Err(PublishCompiledError::BindingOutputLength {
            observed: scratch.binding_output.len(),
        });
    }
    let canonical = CanonicalCompilation::prepare(compiled, scratch.ordinals)
        .map_err(PublishCompiledError::Canonical)?;
    let manifest = canonical
        .write_into(scratch.manifest_output, scratch.manifest_facts)
        .map_err(PublishCompiledError::ManifestWrite)?;

    let mut fragments = ImmutableArtifactStore::new(artifact_directory)
        .map_err(PublishCompiledError::FragmentStorageOwner)?;
    for (ordinal, fragment) in canonical.fragments().enumerate() {
        let ranges = FragmentRangeManifest::from_view(&fragment.fragment)
            .map_err(|source| PublishCompiledError::FragmentManifest { ordinal, source })?;
        fragments
            .ensure(&ranges, &fragment.fragment)
            .map_err(|source| PublishCompiledError::FragmentStorage { ordinal, source })?;
    }
    let mut manifests = ImmutableManifestStore::new(artifact_directory)
        .map_err(PublishCompiledError::ManifestStorage)?;
    manifests
        .ensure(&manifest)
        .map_err(PublishCompiledError::ManifestStorage)?;

    with_verified_generation(&canonical, &manifest, scratch.locality_output, |verified| {
        let expected_generation = *verified;
        let binding = CompilationBindingView::write_into(
            expected_generation,
            manifest.identity,
            scratch.binding_output,
        )
        .map_err(PublishCompiledError::BindingWrite)?;
        let binding_facts = *binding;
        let mut bindings = GenerationBindingStore::new(artifact_directory)
            .map_err(PublishCompiledError::BindingStorage)?;
        bindings
            .ensure(&binding)
            .map_err(PublishCompiledError::BindingStorage)?;
        let attempted = UncommittedPublicationFacts {
            generation: expected_generation,
            manifest: *manifest,
            binding: binding_facts,
        };
        if control.cancelled_before_admission() {
            return Err(PublishCompiledError::Uncommitted(
                UncommittedPublication::CancelledBeforeAdmission { attempted },
            ));
        }
        let published = match publisher.try_publish(verified) {
            Ok(pending) if control.cancelled_after_admission() => match pending.cancel() {
                Ok(_) => {
                    return Err(PublishCompiledError::Uncommitted(
                        UncommittedPublication::Cancelled { attempted },
                    ));
                }
                Err(CancelError::Completed { publication, .. }) => *publication,
                Err(CancelError::Failed { source, .. }) => {
                    return Err(PublishCompiledError::Uncommitted(
                        UncommittedPublication::Failed { attempted, source },
                    ));
                }
                Err(CancelError::OwnerLost { source, .. }) => {
                    return Err(PublishCompiledError::Uncommitted(
                        UncommittedPublication::OwnerLost { attempted, source },
                    ));
                }
                Err(_) => {
                    return Err(PublishCompiledError::Uncommitted(
                        UncommittedPublication::UnsupportedTerminal { attempted },
                    ));
                }
            },
            Ok(pending) => match pending.wait() {
                Ok(published) => published.publication,
                Err(PublicationError::Failed { source, .. }) => {
                    return Err(PublishCompiledError::Uncommitted(
                        UncommittedPublication::Failed { attempted, source },
                    ));
                }
                Err(PublicationError::Cancelled { .. }) => {
                    return Err(PublishCompiledError::Uncommitted(
                        UncommittedPublication::Cancelled { attempted },
                    ));
                }
                Err(PublicationError::OwnerLost { source, .. }) => {
                    return Err(PublishCompiledError::Uncommitted(
                        UncommittedPublication::OwnerLost { attempted, source },
                    ));
                }
                Err(_) => {
                    return Err(PublishCompiledError::Uncommitted(
                        UncommittedPublication::UnsupportedTerminal { attempted },
                    ));
                }
            },
            Err(SubmitError::Full { .. }) => {
                return Err(PublishCompiledError::Uncommitted(
                    UncommittedPublication::Full { attempted },
                ));
            }
            Err(SubmitError::Closed { .. }) => {
                return Err(PublishCompiledError::Uncommitted(
                    UncommittedPublication::Closed { attempted },
                ));
            }
            Err(_) => {
                return Err(PublishCompiledError::Uncommitted(
                    UncommittedPublication::UnsupportedAdmission { attempted },
                ));
            }
        };
        if published.generation != binding_facts.generation {
            return Err(PublishCompiledError::StableGenerationMismatch {
                binding: binding_facts,
                publication: published,
            });
        }
        Ok(PublishedCompilation {
            publication: published,
            manifest: *manifest,
            binding: binding_facts,
        })
    })
    .map_err(PublishCompiledError::Generation)?
}

/// Reopens only the journal-selected compiler package and validates its complete immutable closure.
///
/// `None` means that the durable journal has no selected generation. Once a generation is visible,
/// a missing binding, manifest, or fragment is a typed error rather than an empty result.
#[allow(
    clippy::result_large_err,
    reason = "the cold terminal retains exact durable facts and immutable corruption evidence"
)]
pub fn open_published<'manifest, 'facts>(
    publisher: &DurablePublisher,
    artifact_directory: &Path,
    scratch: OpenPublicationScratch<'manifest, 'facts, '_, '_>,
) -> Result<Option<OpenedCompilation<'manifest, 'facts>>, OpenPublishedError> {
    let Some(publication) = publisher.published().map_err(OpenPublishedError::Journal)? else {
        return Ok(None);
    };
    let bindings = GenerationBindingStore::existing(artifact_directory);
    let binding = bindings
        .load(publication.generation)
        .map_err(OpenPublishedError::Binding)?
        .ok_or(OpenPublishedError::MissingBinding {
            generation: publication.generation,
        })?;
    if binding.facts.generation != publication.generation {
        return Err(OpenPublishedError::BindingGeneration {
            facts: Arc::new(BindingGenerationMismatch {
                publication,
                binding: binding.facts,
            }),
        });
    }

    let OpenPublicationScratch {
        manifest_output,
        manifest_facts,
        fragment_output,
        locality_output,
    } = scratch;
    let manifests = ImmutableManifestStore::existing(artifact_directory);
    let manifest = manifests
        .open(binding.facts.manifest, manifest_output, manifest_facts)
        .map_err(OpenPublishedError::Manifest)?;
    let required_fragment_bytes = manifest.fragments().try_fold(0_usize, |total, facts| {
        let length = usize::try_from(facts.fragment_length).map_err(|source| {
            OpenPublishedError::FragmentOutputLengthAddressSpace {
                fragment: facts.fragment,
                source,
            }
        })?;
        total
            .checked_add(length)
            .ok_or(OpenPublishedError::FragmentOutputLengthOverflow)
    })?;
    if fragment_output.len() < required_fragment_bytes {
        return Err(OpenPublishedError::FragmentOutputTooSmall {
            required: required_fragment_bytes,
            available: fragment_output.len(),
        });
    }
    let fragment_output = &mut fragment_output[..required_fragment_bytes];
    let fragments = ImmutableArtifactStore::existing(artifact_directory);
    let mut offset = 0_usize;
    for (ordinal, expected) in manifest.fragments().enumerate() {
        let length = usize::try_from(expected.fragment_length).map_err(|source| {
            OpenPublishedError::FragmentOutputLengthAddressSpace {
                fragment: expected.fragment,
                source,
            }
        })?;
        let end = offset
            .checked_add(length)
            .ok_or(OpenPublishedError::FragmentOutputLengthOverflow)?;
        let fragment = fragments
            .open(expected, &mut fragment_output[offset..end])
            .map_err(|source| OpenPublishedError::Fragment { ordinal, source })?;
        let ranges = FragmentRangeManifest::from_view(&fragment)
            .map_err(|source| OpenPublishedError::FragmentRanges { ordinal, source })?;
        let observed = stored_fragment_facts(ranges);
        if observed != expected {
            return Err(OpenPublishedError::FragmentFacts {
                facts: Arc::new(FragmentFactsMismatch {
                    ordinal,
                    expected,
                    observed,
                }),
            });
        }
        offset = end;
    }
    let rebuilt = verify_reopened_generation(&manifest, fragment_output, locality_output)
        .map_err(OpenPublishedError::Generation)?;
    if rebuilt != publication.generation || rebuilt != binding.facts.generation {
        return Err(OpenPublishedError::GenerationMismatch {
            facts: Arc::new(GenerationFactsMismatch {
                publication,
                binding: binding.facts,
                rebuilt,
            }),
        });
    }
    Ok(Some(OpenedCompilation {
        publication,
        binding: binding.facts,
        manifest,
    }))
}

fn stored_fragment_facts(manifest: FragmentRangeManifest) -> StoredFragmentFacts {
    StoredFragmentFacts {
        fragment: manifest.fragment,
        fragment_length: manifest.fragment_length,
        source: manifest.source,
        recipe: manifest.recipe,
        ranges: manifest.ranges,
    }
}
