//! Reopen facts, borrowed artifacts, cursors, and exact integrity terminals.
//!
//! Every cursor yields only a manifest-bound compact view or a compact/semantic
//! pair.  Publication-side admission and cancellation facts live in the sibling
//! module, preventing a reopened immutable byte failure from being flattened into
//! a submit terminal.

use core::ops::Deref;

use backend_semantic::ir::{
    FragmentError, FragmentRange, FragmentRangeManifest, FragmentRangeManifestError, FragmentView,
    RecipeFact, SectionKind, SemanticImageIdentity, SemanticImageReopenError, SemanticImageView,
    SourceIdentity,
};
use backend_store::hydration::VerifiedGenerationFacts;
use backend_store::journal::PublicationFacts;
use thiserror::Error;

use crate::publication::{
    binding::CompilationBindingFacts,
    binding_store::BindingStoreError,
    generation::GenerationBuildError,
    immutable::ImmutableArtifactError,
    manifest::{CompilationManifestView, StoredFragmentFacts},
    manifest_store::ImmutableManifestError,
    semantic_immutable::ImmutableSemanticImageError,
};

mod artifact;

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

/// Caller-owned buffers used to reopen a schema-2 semantic publication.
pub struct OpenSemanticPublicationScratch<'manifest, 'facts, 'fragments, 'semantic, 'locality> {
    /// Output for the complete immutable package manifest.
    pub manifest_output: &'manifest mut [u8],
    /// Decoded manifest facts for every paired artifact row.
    pub manifest_facts: &'facts mut [Option<StoredFragmentFacts>],
    /// Concatenated compact fragment bytes in canonical manifest order.
    pub fragment_output: &'fragments mut [u8],
    /// Concatenated full semantic-image bytes in canonical manifest order.
    pub semantic_image_output: &'semantic mut [u8],
    /// Output for complete-generation locality reconstruction.
    pub locality_output: &'locality mut [u8],
}

/// Exact caller-owned output widths required to reopen one semantic generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticGenerationRequirements {
    /// Concatenated compact-fragment bytes in manifest order.
    pub fragment_bytes: usize,
    /// Concatenated full semantic-image bytes in manifest order.
    pub semantic_image_bytes: usize,
}

/// A durable selected compiler package with its borrowed, fully validated canonical manifest.
pub struct OpenedCompilation<'manifest, 'facts> {
    /// Stable journal facts from which this package was exclusively selected.
    pub publication: PublicationFacts,
    /// Immutable generation-to-manifest binding facts addressed by `publication.generation`.
    pub binding: CompilationBindingFacts,
    /// Borrowed canonical manifest validated along with every referenced fragment and generation root.
    pub manifest: CompilationManifestView<'manifest, 'facts>,
    pub(crate) fragments: &'manifest [u8],
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

/// Durable compiler package whose compact and full semantic artifacts were
/// jointly verified against one selected generation.
pub struct OpenedSemanticCompilation<'manifest, 'facts, 'fragments, 'semantic> {
    /// Stable journal facts selecting this exact complete generation.
    pub publication: PublicationFacts,
    pub(crate) generation: OpenedSemanticGeneration<'manifest, 'facts, 'fragments, 'semantic>,
}

impl<'manifest, 'facts, 'fragments, 'semantic> Deref
    for OpenedSemanticCompilation<'manifest, 'facts, 'fragments, 'semantic>
{
    type Target = OpenedSemanticGeneration<'manifest, 'facts, 'fragments, 'semantic>;

    fn deref(&self) -> &Self::Target {
        &self.generation
    }
}

impl<'manifest, 'facts, 'fragments, 'semantic>
    OpenedSemanticCompilation<'manifest, 'facts, 'fragments, 'semantic>
{
    /// Drops locality-specific journal receipt facts while retaining the
    /// verified immutable semantic generation.
    #[must_use]
    pub fn into_generation(
        self,
    ) -> OpenedSemanticGeneration<'manifest, 'facts, 'fragments, 'semantic> {
        self.generation
    }
}

/// One exact semantic generation reopened from immutable closure facts.
///
/// Unlike [`OpenedSemanticCompilation`], this proof does not carry a local
/// journal receipt. It can therefore activate a replicated semantic claim
/// whose immutable generation is present in this owner's artifact store even
/// when another generation is the owner's current journal head.
pub struct OpenedSemanticGeneration<'manifest, 'facts, 'fragments, 'semantic> {
    /// Immutable generation-to-manifest binding.
    pub binding: CompilationBindingFacts,
    /// Validated schema-2 manifest pairing every artifact.
    pub manifest: CompilationManifestView<'manifest, 'facts>,
    pub(crate) fragments: &'fragments [u8],
    pub(crate) semantic_images: &'semantic [u8],
}

impl<'manifest, 'facts, 'fragments, 'semantic>
    OpenedSemanticGeneration<'manifest, 'facts, 'fragments, 'semantic>
{
    /// Iterates paired compact and full semantic views in manifest order.
    pub fn artifacts(&self) -> OpenedSemanticArtifactCursor<'_, 'fragments, 'semantic> {
        OpenedSemanticArtifactCursor {
            facts: self.manifest.fragments(),
            fragment_bytes: self.fragments,
            semantic_bytes: self.semantic_images,
            fragment_offset: 0,
            semantic_offset: 0,
            ordinal: 0,
            failed: false,
        }
    }
}

/// One manifest-bound compact compatibility view and authoritative semantic image.
pub struct OpenedSemanticArtifact<'fragment, 'semantic> {
    /// Compact artifact retained for compatibility and range retrieval.
    pub fragment: OpenedFragment<'fragment>,
    /// Complete typed semantic reader over the paired immutable image.
    pub semantic_image: SemanticImageView<'semantic>,
}

/// Exact-size-on-success cursor over paired semantic publication artifacts.
pub struct OpenedSemanticArtifactCursor<'manifest, 'fragment, 'semantic> {
    facts: crate::publication::manifest::CompilationManifestEntries<'manifest>,
    fragment_bytes: &'fragment [u8],
    semantic_bytes: &'semantic [u8],
    fragment_offset: usize,
    semantic_offset: usize,
    ordinal: usize,
    failed: bool,
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
    facts: crate::publication::manifest::CompilationManifestEntries<'opened>,
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
        observed: crate::publication::immutable::FragmentIdentity,
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
pub(super) fn opened_fragment<'fragment>(
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

/// Rejection while yielding one paired artifact from a reopened semantic package.
#[derive(Debug, Error)]
#[non_exhaustive]
#[allow(
    missing_docs,
    reason = "each variant retains the exact manifest row, coordinate, and source"
)]
pub enum OpenedSemanticArtifactError {
    #[error("opened semantic artifact ordinal overflowed this address space")]
    OrdinalOverflow { facts: StoredFragmentFacts },
    #[error("opened semantic artifact {ordinal} has no schema-2 semantic image fact")]
    MissingSemanticImage {
        ordinal: usize,
        facts: StoredFragmentFacts,
    },
    #[error("opened semantic image {ordinal} length cannot fit this address space")]
    LengthAddressSpace {
        ordinal: usize,
        facts: crate::publication::semantic_immutable::SemanticImageArtifactFacts,
        #[source]
        source: core::num::TryFromIntError,
    },
    #[error("opened semantic image {ordinal} extent overflowed")]
    RangeOverflow {
        ordinal: usize,
        facts: crate::publication::semantic_immutable::SemanticImageArtifactFacts,
        offset: usize,
        length: usize,
    },
    #[error("opened semantic image {ordinal} region is truncated")]
    RegionTruncated {
        ordinal: usize,
        facts: crate::publication::semantic_immutable::SemanticImageArtifactFacts,
        available: usize,
        required: usize,
    },
    #[error("opened semantic image {ordinal} failed complete semantic grammar validation")]
    Grammar {
        ordinal: usize,
        facts: crate::publication::semantic_immutable::SemanticImageArtifactFacts,
        #[source]
        source: SemanticImageReopenError,
    },
    #[error("opened semantic image {ordinal} identity differs from its manifest fact")]
    Identity {
        ordinal: usize,
        expected: SemanticImageIdentity,
        observed: SemanticImageIdentity,
    },
    #[error("paired compact artifact {ordinal} failed durable reconstruction")]
    Fragment {
        ordinal: usize,
        #[source]
        source: OpenedFragmentError,
    },
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
    Journal(#[source] backend_store::journal::PublicationOpenError),
    #[error("durable selected compiler generation has no immutable binding")]
    MissingBinding { generation: VerifiedGenerationFacts },
    #[error("durable selected compiler binding could not be opened")]
    Binding(#[source] BindingStoreError),
    #[error("durable selected compiler binding names a different generation")]
    BindingGeneration {
        publication: PublicationFacts,
        binding: CompilationBindingFacts,
    },
    #[error("immutable semantic binding differs from the requested generation binding")]
    BindingFacts {
        expected: CompilationBindingFacts,
        observed: CompilationBindingFacts,
    },
    #[error("durable selected compiler manifest could not be opened")]
    Manifest(#[source] ImmutableManifestError),
    #[error("immutable semantic manifest differs from the requested manifest facts")]
    ManifestFacts {
        expected: crate::publication::manifest::CompilationManifestFacts,
        observed: crate::publication::manifest::CompilationManifestFacts,
    },
    #[error("compiler publication uses {observed:?}, but this reopen path requires {expected:?}")]
    ManifestFormat {
        expected: crate::publication::manifest::CompilationManifestFormat,
        observed: crate::publication::manifest::CompilationManifestFormat,
    },
    #[error("compiler manifest fragment output has {available} bytes, requires {required}")]
    FragmentOutputTooSmall { required: usize, available: usize },
    #[error("compiler manifest fragment {fragment:?} length cannot fit this address space")]
    FragmentOutputLengthAddressSpace {
        fragment: backend_version::ArtifactId<
            backend_version::IrFragmentEncoding,
            backend_version::IrFragmentDomain,
        >,
        #[source]
        source: core::num::TryFromIntError,
    },
    #[error("compiler manifest fragment byte sum overflowed native address space")]
    FragmentOutputLengthOverflow,
    #[error("semantic manifest fragment {fragment:?} omitted its complete semantic image")]
    MissingSemanticImage {
        fragment: crate::publication::immutable::FragmentIdentity,
    },
    #[error("semantic image {semantic_image:?} length cannot fit this address space")]
    SemanticOutputLengthAddressSpace {
        semantic_image: SemanticImageIdentity,
        #[source]
        source: core::num::TryFromIntError,
    },
    #[error("semantic-image byte sum overflowed native address space")]
    SemanticOutputLengthOverflow,
    #[error("semantic-image output has {available} bytes, requires {required}")]
    SemanticOutputTooSmall { required: usize, available: usize },
    #[error("durable selected compiler fragment {ordinal} could not be opened")]
    Fragment {
        ordinal: usize,
        #[source]
        source: ImmutableArtifactError,
    },
    #[error("durable selected semantic image {ordinal} could not be opened")]
    SemanticImage {
        ordinal: usize,
        #[source]
        source: ImmutableSemanticImageError,
    },
    #[error("durable semantic image {ordinal} has no captured compiler provenance")]
    SemanticProvenanceUnavailable { ordinal: usize },
    #[error("durable semantic image {ordinal} source differs from its manifest fragment")]
    SemanticSource {
        ordinal: usize,
        expected: SourceIdentity,
        observed: SourceIdentity,
    },
    #[error("durable semantic image {ordinal} recipe differs from its manifest fragment")]
    SemanticRecipe {
        ordinal: usize,
        expected: RecipeFact,
        observed: RecipeFact,
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
    #[error("semantic generation differs from its rebuilt immutable package closure")]
    SemanticGenerationMismatch {
        expected: VerifiedGenerationFacts,
        rebuilt: VerifiedGenerationFacts,
    },
}
