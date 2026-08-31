//! Canonical package manifests over complete recipe-bearing IR fragments.

use core::{cmp::Ordering, num::TryFromIntError, ops::Deref};

use nudox_compile_driver::CompiledFragment;
use nudox_compile_vocab::{CompileRecipeFact, Language, NativeTool, Stage};
use nudox_id::{
    ArtifactId, ArtifactIdDecodeError, CompileRecipeDomain, ContentId, ContentIdDecodeError,
    IrFragmentDomain, IrFragmentEncoding, IrFragmentRangeEncoding, IrManifestDomain,
    IrManifestEncoding, SourceFactDomain, ToolchainDomain,
};
use nudox_ir_format::{
    FragmentRange, FragmentRangeManifest, FragmentRangeManifestError, FragmentView, RecipeFact,
    SectionKind, SourceIdentity,
};
use thiserror::Error;

/// Typed identity of one complete canonical compiler package manifest.
pub type CompilationManifestIdentity = ArtifactId<IrManifestEncoding, IrManifestDomain>;

/// Fixed header width of the compiler package manifest wire format.
pub const COMPILATION_MANIFEST_HEADER_BYTES: usize = 16;
/// Fixed width of one complete recipe-bearing fragment entry.
pub const COMPILATION_MANIFEST_ENTRY_BYTES: usize = 404;

const MAGIC: [u8; 8] = *b"NUDXCPM\0";
const VERSION: u16 = 1;
const RANGE_COUNT: usize = 6;
const RANGE_BYTES: usize = 44;
const SOURCE_IDENTITY_OFFSET: usize = 0;
const SOURCE_LENGTH_OFFSET: usize = 32;
const RECIPE_IDENTITY_OFFSET: usize = 36;
const RECIPE_LANGUAGE_OFFSET: usize = 68;
const RECIPE_STAGE_OFFSET: usize = 69;
const RECIPE_TOOL_OFFSET: usize = 70;
const RECIPE_RESERVED_OFFSET: usize = 71;
const RECIPE_TOOLCHAIN_OFFSET: usize = 72;
const FRAGMENT_IDENTITY_OFFSET: usize = 104;
const FRAGMENT_LENGTH_OFFSET: usize = 136;
const RANGE_OFFSET: usize = 140;
const SECTION_ORDER: [SectionKind; RANGE_COUNT] = [
    SectionKind::EntityTypes,
    SectionKind::TypeNodes,
    SectionKind::AtomRecords,
    SectionKind::AtomBytes,
    SectionKind::SourceIdentity,
    SectionKind::RecipeFact,
];

/// A complete borrowed IR fragment admitted only through its validated range manifest.
#[derive(Clone, Copy)]
pub(crate) struct PublicationFragment<'view, 'fragment> {
    view: &'view FragmentView<'fragment>,
    manifest: FragmentRangeManifest,
}

impl<'view, 'fragment> PublicationFragment<'view, 'fragment> {
    /// Admits a borrowed fragment only after committing all required ranges and recipe facts.
    #[allow(
        clippy::result_large_err,
        reason = "the terminal preserves both exact typed recipe facts without allocation or erasure"
    )]
    pub(crate) fn from_view(
        view: &'view FragmentView<'fragment>,
    ) -> Result<Self, PublicationFragmentError> {
        let manifest = FragmentRangeManifest::from_view(view)
            .map_err(PublicationFragmentError::RangeManifest)?;
        let expected = CompileRecipeFact::derive(
            manifest.recipe.language,
            manifest.recipe.stage,
            manifest.recipe.tool,
            manifest.source.identity,
            manifest.recipe.toolchain,
        );
        if manifest.recipe != expected {
            return Err(PublicationFragmentError::RecipeIdentity {
                expected,
                observed: manifest.recipe,
            });
        }
        if manifest.recipe.stage != Stage::LowerIr {
            return Err(PublicationFragmentError::SemanticStage {
                observed: manifest.recipe.stage,
            });
        }
        Ok(Self { view, manifest })
    }

    /// Admits only a compiler driver's typed lowering result after independently checking that
    /// the driver's terminal facts are the facts retained by its validated compact IR bytes.
    #[allow(
        clippy::result_large_err,
        reason = "the terminal preserves exact typed compiler and fragment facts without allocation or erasure"
    )]
    pub(crate) fn from_compiled(
        compiled: &'view CompiledFragment<'fragment>,
    ) -> Result<Self, PublicationFragmentError> {
        let fragment = Self::from_view(&compiled.fragment)?;
        if fragment.source != compiled.source {
            return Err(PublicationFragmentError::CompilerSource {
                compiler: compiled.source,
                fragment: fragment.source,
            });
        }
        if fragment.recipe != compiled.recipe {
            return Err(PublicationFragmentError::CompilerRecipe {
                compiler: compiled.recipe,
                fragment: fragment.recipe,
            });
        }
        Ok(fragment)
    }
}

impl Deref for PublicationFragment<'_, '_> {
    type Target = FragmentRangeManifest;

    fn deref(&self) -> &Self::Target {
        &self.manifest
    }
}

impl AsRef<[u8]> for PublicationFragment<'_, '_> {
    fn as_ref(&self) -> &[u8] {
        self.view.as_ref()
    }
}

/// Rejection before a complete borrowed fragment can enter a compiler package.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PublicationFragmentError {
    /// The validated IR view could not produce every complete range commitment.
    #[error("could not commit complete IR fragment ranges")]
    RangeManifest(#[source] FragmentRangeManifestError),
    /// Persisted recipe fields did not derive the persisted recipe identity.
    #[error("fragment recipe identity disagrees with its source and toolchain facts")]
    RecipeIdentity {
        /// Canonical recipe fact derived from the persisted fields.
        expected: CompileRecipeFact,
        /// Recipe fact retained by the fragment.
        observed: CompileRecipeFact,
    },
    /// A compact IR fragment named a non-lowering semantic stage.
    #[error("fragment recipe stage {observed:?} cannot produce compact IR")]
    SemanticStage {
        /// Rejected closed recipe stage.
        observed: Stage,
    },
    /// The driver's terminal source fact differed from the source fact retained by its IR.
    #[error("compiler terminal source fact differs from its validated compact IR source fact")]
    CompilerSource {
        /// Source fact returned by the driver terminal.
        compiler: SourceIdentity,
        /// Source fact decoded from the compact IR bytes.
        fragment: SourceIdentity,
    },
    /// The driver's terminal recipe fact differed from the recipe fact retained by its IR.
    #[error("compiler terminal recipe fact differs from its validated compact IR recipe fact")]
    CompilerRecipe {
        /// Recipe fact returned by the driver terminal.
        compiler: RecipeFact,
        /// Recipe fact decoded from the compact IR bytes.
        fragment: RecipeFact,
    },
}

/// Canonically sorted package inputs borrowing the exact fragment output buffers.
pub(crate) struct CanonicalCompilation<'input, 'scratch, 'fragment> {
    inputs: &'input [CompiledFragment<'fragment>],
    ordinals: &'scratch [usize],
}

impl<'input, 'scratch, 'fragment> CanonicalCompilation<'input, 'scratch, 'fragment> {
    /// Writes caller input ordinals into caller scratch, sorts them by typed fragment identity,
    /// and rejects duplicate immutable fragments before any manifest byte is written.
    #[allow(
        clippy::result_large_err,
        reason = "admission retains exact typed fragment facts"
    )]
    pub(crate) fn prepare(
        inputs: &'input [CompiledFragment<'fragment>],
        scratch: &'scratch mut [usize],
    ) -> Result<Self, CompilationPrepareError> {
        if scratch.len() < inputs.len() {
            return Err(CompilationPrepareError::ScratchTooSmall {
                required: inputs.len(),
                available: scratch.len(),
            });
        }
        let ordinals = &mut scratch[..inputs.len()];
        for (ordinal, slot) in ordinals.iter_mut().enumerate() {
            PublicationFragment::from_compiled(&inputs[ordinal])
                .map_err(|source| CompilationPrepareError::Fragment { ordinal, source })?;
            *slot = ordinal;
        }
        ordinals.sort_unstable_by_key(|ordinal| fragment_identity(&inputs[*ordinal]));
        for pair in ordinals.windows(2) {
            if fragment_identity(&inputs[pair[0]]) == fragment_identity(&inputs[pair[1]]) {
                return Err(CompilationPrepareError::DuplicateFragment {
                    identity: fragment_identity(&inputs[pair[0]]),
                });
            }
        }
        Ok(Self { inputs, ordinals })
    }

    /// Exact canonical package byte count required for [`Self::write_into`].
    #[allow(
        clippy::result_large_err,
        reason = "exact admission errors are preserved"
    )]
    pub(crate) fn required_bytes(&self) -> Result<usize, CompilationPrepareError> {
        let entries = self.ordinals.len();
        COMPILATION_MANIFEST_HEADER_BYTES
            .checked_add(
                entries
                    .checked_mul(COMPILATION_MANIFEST_ENTRY_BYTES)
                    .ok_or(CompilationPrepareError::ManifestLengthOverflow { entries })?,
            )
            .ok_or(CompilationPrepareError::ManifestLengthOverflow { entries })
    }

    /// Writes and independently validates one canonical package manifest in caller output.
    #[allow(
        clippy::result_large_err,
        reason = "the terminal retains exact independent manifest-validation facts without allocation or erasure"
    )]
    pub(crate) fn write_into<'output, 'facts>(
        &self,
        output: &'output mut [u8],
        fact_scratch: &'facts mut [Option<StoredFragmentFacts>],
    ) -> Result<CompilationManifestView<'output, 'facts>, CompilationWriteError> {
        let required = self
            .required_bytes()
            .map_err(CompilationWriteError::Preparation)?;
        if output.len() < required {
            return Err(CompilationWriteError::OutputTooSmall {
                required,
                available: output.len(),
            });
        }
        let count = u32::try_from(self.ordinals.len()).map_err(|source| {
            CompilationWriteError::FragmentCountAddressSpace {
                observed: self.ordinals.len(),
                source,
            }
        })?;
        let written = &mut output[..required];
        written[..8].copy_from_slice(&MAGIC);
        written[8..10].copy_from_slice(&VERSION.to_le_bytes());
        written[10..12].copy_from_slice(&0_u16.to_le_bytes());
        written[12..16].copy_from_slice(&count.to_le_bytes());
        for (ordinal, input_ordinal) in self.ordinals.iter().copied().enumerate() {
            let fragment = PublicationFragment::from_compiled(&self.inputs[input_ordinal])
                .map_err(|source| CompilationWriteError::Fragment { ordinal, source })?;
            let offset =
                COMPILATION_MANIFEST_HEADER_BYTES + ordinal * COMPILATION_MANIFEST_ENTRY_BYTES;
            write_entry(
                &mut written[offset..offset + COMPILATION_MANIFEST_ENTRY_BYTES],
                &fragment.manifest,
            );
        }
        CompilationManifestView::validate(written, fact_scratch)
            .map_err(CompilationWriteError::FreshValidation)
    }

    /// Iterates the sorted borrowed fragments whose bytes must be persisted before publication.
    pub(crate) fn fragments(&self) -> impl Iterator<Item = &CompiledFragment<'fragment>> {
        self.ordinals
            .iter()
            .copied()
            .map(|ordinal| &self.inputs[ordinal])
    }
}

/// Failure while canonicalizing caller-provided compiler fragments.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CompilationPrepareError {
    /// Caller canonicalization scratch cannot hold every input fragment.
    #[error("publication scratch holds {available} fragments, requires {required}")]
    ScratchTooSmall {
        /// Exact input fragment count.
        required: usize,
        /// Caller-provided scratch capacity.
        available: usize,
    },
    /// Two inputs named the same immutable complete fragment identity.
    #[error("publication includes fragment identity {identity:?} more than once")]
    DuplicateFragment {
        /// Duplicate complete fragment identity.
        identity: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    },
    /// One driver output did not agree with its own validated compact fragment facts.
    #[error("compiler output {ordinal} cannot enter a canonical publication")]
    Fragment {
        /// Input ordinal retaining the exact mismatch location.
        ordinal: usize,
        /// Exact typed source, recipe, range, or stage rejection.
        #[source]
        source: PublicationFragmentError,
    },
    /// Canonical output extent overflowed native address-space arithmetic.
    #[error("publication manifest length overflowed for {entries} fragments")]
    ManifestLengthOverflow {
        /// Input fragment count that overflowed the byte calculation.
        entries: usize,
    },
}

/// Failure while writing one canonical compiler package manifest.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CompilationWriteError {
    /// Caller input could not form a bounded canonical manifest.
    #[error("could not prepare canonical compiler publication")]
    Preparation(#[source] CompilationPrepareError),
    /// Caller output is shorter than the preflighted complete manifest extent.
    #[error("manifest output has {available} bytes, requires {required}")]
    OutputTooSmall {
        /// Exact required canonical extent.
        required: usize,
        /// Caller-provided output extent.
        available: usize,
    },
    /// Fragment count cannot be represented by the fixed manifest coordinate.
    #[error("fragment count {observed} cannot be represented by the manifest wire")]
    FragmentCountAddressSpace {
        /// Native fragment count.
        observed: usize,
        /// Checked conversion source.
        #[source]
        source: TryFromIntError,
    },
    /// A previously admitted driver fragment ceased to satisfy admission while being written.
    #[error("compiler output {ordinal} ceased to satisfy canonical fragment admission")]
    Fragment {
        /// Canonical output ordinal.
        ordinal: usize,
        /// Exact typed source, recipe, range, or stage rejection.
        #[source]
        source: PublicationFragmentError,
    },
    /// Fresh bytes did not pass the same decoder used for persisted manifests.
    #[error("fresh canonical compiler manifest failed independent validation")]
    FreshValidation(#[source] CompilationManifestError),
}

/// Immutable facts exposed by a validated canonical compiler package manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilationManifestFacts {
    /// Typed identity of the complete canonical manifest bytes.
    pub identity: CompilationManifestIdentity,
    /// Exact number of complete fragment entries.
    pub fragment_count: u32,
    /// Exact complete canonical manifest length.
    pub byte_length: u32,
}

/// A borrowed validated compiler package manifest.
pub struct CompilationManifestView<'manifest, 'facts> {
    bytes: &'manifest [u8],
    facts: CompilationManifestFacts,
    entries: &'facts [Option<StoredFragmentFacts>],
}

impl<'manifest, 'facts> CompilationManifestView<'manifest, 'facts> {
    /// Validates all fixed header, recipe, fragment, range, and canonical-order facts.
    #[allow(
        clippy::result_large_err,
        reason = "the decoder retains exact typed malformed-record facts without allocation or erasure"
    )]
    pub fn validate(
        bytes: &'manifest [u8],
        fact_scratch: &'facts mut [Option<StoredFragmentFacts>],
    ) -> Result<Self, CompilationManifestError> {
        if bytes.len() < COMPILATION_MANIFEST_HEADER_BYTES {
            return Err(CompilationManifestError::HeaderLength {
                observed: bytes.len(),
            });
        }
        let magic = fixed::<8>(bytes, 0);
        if magic != MAGIC {
            return Err(CompilationManifestError::Magic { observed: magic });
        }
        let version = u16::from_le_bytes(fixed::<2>(bytes, 8));
        if version != VERSION {
            return Err(CompilationManifestError::Version { observed: version });
        }
        let reserved = u16::from_le_bytes(fixed::<2>(bytes, 10));
        if reserved != 0 {
            return Err(CompilationManifestError::HeaderReserved { observed: reserved });
        }
        let fragment_count = u32::from_le_bytes(fixed::<4>(bytes, 12));
        let count = usize::try_from(fragment_count).map_err(|source| {
            CompilationManifestError::FragmentCountAddressSpace {
                fragment_count,
                source,
            }
        })?;
        if fact_scratch.len() < count {
            return Err(CompilationManifestError::FactScratchTooSmall {
                required: count,
                available: fact_scratch.len(),
            });
        }
        let expected = COMPILATION_MANIFEST_HEADER_BYTES
            .checked_add(
                count
                    .checked_mul(COMPILATION_MANIFEST_ENTRY_BYTES)
                    .ok_or(CompilationManifestError::LengthOverflow { fragment_count })?,
            )
            .ok_or(CompilationManifestError::LengthOverflow { fragment_count })?;
        if bytes.len() != expected {
            return Err(CompilationManifestError::Length {
                expected,
                observed: bytes.len(),
            });
        }
        let entries = &mut fact_scratch[..count];
        entries.fill(None);
        let mut previous: Option<ArtifactId<IrFragmentEncoding, IrFragmentDomain>> = None;
        for (ordinal, slot) in entries.iter_mut().enumerate() {
            let offset =
                COMPILATION_MANIFEST_HEADER_BYTES + ordinal * COMPILATION_MANIFEST_ENTRY_BYTES;
            let entry = decode_entry(
                &bytes[offset..offset + COMPILATION_MANIFEST_ENTRY_BYTES],
                ordinal,
            )?;
            if let Some(previous_identity) = previous
                && previous_identity.cmp(&entry.fragment) != Ordering::Less
            {
                return Err(CompilationManifestError::Order {
                    ordinal,
                    previous: previous_identity,
                    observed: entry.fragment,
                });
            }
            previous = Some(entry.fragment);
            *slot = Some(entry);
        }
        let byte_length = u32::try_from(bytes.len()).map_err(|source| {
            CompilationManifestError::ByteLengthAddressSpace {
                observed: bytes.len(),
                source,
            }
        })?;
        Ok(Self {
            bytes,
            facts: CompilationManifestFacts {
                identity: CompilationManifestIdentity::from_encoded_bytes(bytes),
                fragment_count,
                byte_length,
            },
            entries,
        })
    }

    /// Iterates complete fragment facts in canonical package order.
    ///
    /// This view has already validated every entry before it exists, so iteration is infallible.
    pub fn fragments(&self) -> CompilationManifestEntries<'_> {
        CompilationManifestEntries {
            entries: self.entries.iter(),
        }
    }
}

impl Deref for CompilationManifestView<'_, '_> {
    type Target = CompilationManifestFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl AsRef<[u8]> for CompilationManifestView<'_, '_> {
    fn as_ref(&self) -> &[u8] {
        self.bytes
    }
}

/// One decoded complete fragment record from a package manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredFragmentFacts {
    /// Exact content-addressed complete IR fragment identity.
    pub fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    /// Exact complete fragment byte length.
    pub fragment_length: u32,
    /// Source fact retained by the complete fragment.
    pub source: SourceIdentity,
    /// Recipe fact retained by the complete fragment.
    pub recipe: RecipeFact,
    /// Six exact required semantic-lane commitments.
    pub ranges: [FragmentRange; RANGE_COUNT],
}

/// Cursor over validated package fragment records.
pub struct CompilationManifestEntries<'manifest> {
    entries: core::slice::Iter<'manifest, Option<StoredFragmentFacts>>,
}

impl Iterator for CompilationManifestEntries<'_> {
    type Item = StoredFragmentFacts;

    fn next(&mut self) -> Option<Self::Item> {
        self.entries.find_map(|entry| *entry)
    }
}

/// Rejection while decoding an untrusted compiler package manifest.
#[derive(Debug, Error)]
#[non_exhaustive]
#[allow(
    missing_docs,
    reason = "each field repeats the exact fact documented by its enclosing terminal"
)]
pub enum CompilationManifestError {
    /// Bytes did not include the fixed complete header.
    #[error(
        "compiler manifest has {observed} bytes, requires at least {COMPILATION_MANIFEST_HEADER_BYTES}"
    )]
    HeaderLength { observed: usize },
    /// Fixed manifest magic differed from this format.
    #[error("compiler manifest magic is invalid")]
    Magic { observed: [u8; 8] },
    /// Fixed manifest version is unsupported.
    #[error("compiler manifest version {observed} is unsupported")]
    Version { observed: u16 },
    /// Fixed header reserved bits were nonzero.
    #[error("compiler manifest header reserved bits are nonzero: {observed}")]
    HeaderReserved { observed: u16 },
    /// Declared fragment count could not fit the host address space.
    #[error("compiler manifest fragment count {fragment_count} cannot fit this address space")]
    FragmentCountAddressSpace {
        fragment_count: u32,
        #[source]
        source: TryFromIntError,
    },
    /// Declared fragment count overflowed fixed-manifest byte arithmetic.
    #[error("compiler manifest byte length overflowed for {fragment_count} fragments")]
    LengthOverflow { fragment_count: u32 },
    /// Exact complete file length did not match its declared entry count.
    #[error("compiler manifest has {observed} bytes, expected {expected}")]
    Length { expected: usize, observed: usize },
    /// Caller fact scratch could not retain every fully decoded package entry.
    #[error("compiler manifest fact scratch holds {available} entries, requires {required}")]
    FactScratchTooSmall { required: usize, available: usize },
    /// Complete manifest bytes cannot fit the compact public byte length.
    #[error("compiler manifest length {observed} cannot fit the compact fact")]
    ByteLengthAddressSpace {
        observed: usize,
        #[source]
        source: TryFromIntError,
    },
    /// One source identity lacked its typed authority.
    #[error("manifest fragment {ordinal} has an invalid source identity")]
    SourceIdentity {
        ordinal: usize,
        #[source]
        source: ContentIdDecodeError,
    },
    /// One recipe identity lacked its typed authority.
    #[error("manifest fragment {ordinal} has an invalid recipe identity")]
    RecipeIdentityAuthority {
        ordinal: usize,
        #[source]
        source: ContentIdDecodeError,
    },
    /// One toolchain identity lacked its typed authority.
    #[error("manifest fragment {ordinal} has an invalid toolchain identity")]
    ToolchainAuthority {
        ordinal: usize,
        #[source]
        source: ContentIdDecodeError,
    },
    /// One recipe used an unknown closed language tag.
    #[error("manifest fragment {ordinal} has unknown language tag {observed}")]
    Language { ordinal: usize, observed: u8 },
    /// One recipe used an unknown closed stage tag.
    #[error("manifest fragment {ordinal} has unknown stage tag {observed}")]
    Stage { ordinal: usize, observed: u8 },
    /// One recipe used an unknown closed tool tag.
    #[error("manifest fragment {ordinal} has unknown tool tag {observed}")]
    Tool { ordinal: usize, observed: u8 },
    /// One recipe record carried nonzero reserved bits.
    #[error("manifest fragment {ordinal} recipe reserved byte is nonzero: {observed}")]
    RecipeReserved { ordinal: usize, observed: u8 },
    /// Recipe fields did not derive their recorded identity.
    #[error("manifest fragment {ordinal} recipe identity disagrees with source/toolchain facts")]
    RecipeIdentity {
        ordinal: usize,
        expected: CompileRecipeFact,
        observed: CompileRecipeFact,
    },
    /// Compact IR entry named a non-lowering semantic stage.
    #[error("manifest fragment {ordinal} recipe stage {observed:?} cannot produce compact IR")]
    SemanticStage { ordinal: usize, observed: Stage },
    /// One complete fragment identity had the wrong typed authority.
    #[error("manifest fragment {ordinal} has an invalid complete fragment identity")]
    FragmentIdentity {
        ordinal: usize,
        #[source]
        source: ArtifactIdDecodeError,
    },
    /// One range used an unknown closed section code.
    #[error("manifest fragment {ordinal} range {range} has unknown section {observed}")]
    RangeSection {
        ordinal: usize,
        range: usize,
        observed: u16,
    },
    /// One range did not remain in the required fixed semantic-lane order.
    #[error("manifest fragment {ordinal} range {range} names {observed:?}, expected {expected:?}")]
    RangeOrder {
        ordinal: usize,
        range: usize,
        expected: SectionKind,
        observed: SectionKind,
    },
    /// One range retained nonzero reserved bits.
    #[error("manifest fragment {ordinal} range {range} reserved bits are nonzero: {observed}")]
    RangeReserved {
        ordinal: usize,
        range: usize,
        observed: u16,
    },
    /// One range extent overflowed its fixed fragment coordinate.
    #[error("manifest fragment {ordinal} range {range} extent overflows its fragment coordinate")]
    RangeExtentOverflow {
        ordinal: usize,
        range: usize,
        offset: u32,
        length: u32,
    },
    /// One range extended beyond the complete fragment length it claimed to describe.
    #[error(
        "manifest fragment {ordinal} range {range} ends at {end}, beyond fragment length {fragment_length}"
    )]
    RangeOutsideFragment {
        ordinal: usize,
        range: usize,
        end: u32,
        fragment_length: u32,
    },
    /// One range identity had the wrong typed authority.
    #[error("manifest fragment {ordinal} range {range} has an invalid identity")]
    RangeIdentity {
        ordinal: usize,
        range: usize,
        #[source]
        source: ArtifactIdDecodeError,
    },
    /// Fragment entries were not strictly sorted by complete typed identity.
    #[error("manifest fragment {ordinal} identity order is not strictly increasing")]
    Order {
        ordinal: usize,
        previous: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
        observed: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    },
}

fn write_entry(output: &mut [u8], manifest: &FragmentRangeManifest) {
    output[SOURCE_IDENTITY_OFFSET..SOURCE_IDENTITY_OFFSET + 32]
        .copy_from_slice(manifest.source.identity.as_ref());
    output[SOURCE_LENGTH_OFFSET..SOURCE_LENGTH_OFFSET + 4]
        .copy_from_slice(&manifest.source.byte_len.to_le_bytes());
    output[RECIPE_IDENTITY_OFFSET..RECIPE_IDENTITY_OFFSET + 32]
        .copy_from_slice(manifest.recipe.identity.as_ref());
    output[RECIPE_LANGUAGE_OFFSET] = u8::from(manifest.recipe.language);
    output[RECIPE_STAGE_OFFSET] = u8::from(manifest.recipe.stage);
    output[RECIPE_TOOL_OFFSET] = u8::from(manifest.recipe.tool);
    output[RECIPE_RESERVED_OFFSET] = 0;
    output[RECIPE_TOOLCHAIN_OFFSET..RECIPE_TOOLCHAIN_OFFSET + 32]
        .copy_from_slice(manifest.recipe.toolchain.as_ref());
    output[FRAGMENT_IDENTITY_OFFSET..FRAGMENT_IDENTITY_OFFSET + 32]
        .copy_from_slice(manifest.fragment.as_ref());
    output[FRAGMENT_LENGTH_OFFSET..FRAGMENT_LENGTH_OFFSET + 4]
        .copy_from_slice(&manifest.fragment_length.to_le_bytes());
    for (ordinal, range) in manifest.ranges.iter().enumerate() {
        let offset = RANGE_OFFSET + ordinal * RANGE_BYTES;
        output[offset..offset + 2].copy_from_slice(&u16::from(range.section).to_le_bytes());
        output[offset + 2..offset + 4].copy_from_slice(&0_u16.to_le_bytes());
        output[offset + 4..offset + 8].copy_from_slice(&range.offset.to_le_bytes());
        output[offset + 8..offset + 12].copy_from_slice(&range.length.to_le_bytes());
        output[offset + 12..offset + 44].copy_from_slice(range.identity.as_ref());
    }
}

fn fragment_identity(
    compiled: &CompiledFragment<'_>,
) -> ArtifactId<IrFragmentEncoding, IrFragmentDomain> {
    ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
        compiled.fragment.as_ref(),
    )
}

#[allow(
    clippy::result_large_err,
    reason = "the decoder retains exact typed malformed-record facts without allocation or erasure"
)]
fn decode_entry(
    bytes: &[u8],
    ordinal: usize,
) -> Result<StoredFragmentFacts, CompilationManifestError> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::try_from(
            &bytes[SOURCE_IDENTITY_OFFSET..SOURCE_IDENTITY_OFFSET + 32],
        )
        .map_err(|source| CompilationManifestError::SourceIdentity { ordinal, source })?,
        byte_len: u32::from_le_bytes(fixed::<4>(bytes, SOURCE_LENGTH_OFFSET)),
    };
    let language = Language::try_from(bytes[RECIPE_LANGUAGE_OFFSET])
        .map_err(|observed| CompilationManifestError::Language { ordinal, observed })?;
    let stage = Stage::try_from(bytes[RECIPE_STAGE_OFFSET])
        .map_err(|observed| CompilationManifestError::Stage { ordinal, observed })?;
    let tool = NativeTool::try_from(bytes[RECIPE_TOOL_OFFSET])
        .map_err(|observed| CompilationManifestError::Tool { ordinal, observed })?;
    if bytes[RECIPE_RESERVED_OFFSET] != 0 {
        return Err(CompilationManifestError::RecipeReserved {
            ordinal,
            observed: bytes[RECIPE_RESERVED_OFFSET],
        });
    }
    let recipe = RecipeFact {
        identity: ContentId::<CompileRecipeDomain>::try_from(
            &bytes[RECIPE_IDENTITY_OFFSET..RECIPE_IDENTITY_OFFSET + 32],
        )
        .map_err(|source| CompilationManifestError::RecipeIdentityAuthority { ordinal, source })?,
        language,
        stage,
        tool,
        toolchain: ContentId::<ToolchainDomain>::try_from(
            &bytes[RECIPE_TOOLCHAIN_OFFSET..RECIPE_TOOLCHAIN_OFFSET + 32],
        )
        .map_err(|source| CompilationManifestError::ToolchainAuthority { ordinal, source })?,
    };
    let expected_recipe =
        CompileRecipeFact::derive(language, stage, tool, source.identity, recipe.toolchain);
    if recipe != expected_recipe {
        return Err(CompilationManifestError::RecipeIdentity {
            ordinal,
            expected: expected_recipe,
            observed: recipe,
        });
    }
    if stage != Stage::LowerIr {
        return Err(CompilationManifestError::SemanticStage {
            ordinal,
            observed: stage,
        });
    }
    let fragment = ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::try_from(
        &bytes[FRAGMENT_IDENTITY_OFFSET..FRAGMENT_IDENTITY_OFFSET + 32],
    )
    .map_err(|source| CompilationManifestError::FragmentIdentity { ordinal, source })?;
    let fragment_length = u32::from_le_bytes(fixed::<4>(bytes, FRAGMENT_LENGTH_OFFSET));
    let mut ranges = [FragmentRange {
        section: SectionKind::EntityTypes,
        offset: 0,
        length: 0,
        identity: ArtifactId::<IrFragmentRangeEncoding, IrFragmentDomain>::from_encoded_bytes(&[]),
    }; RANGE_COUNT];
    for (range_ordinal, expected_section) in SECTION_ORDER.iter().copied().enumerate() {
        let offset = RANGE_OFFSET + range_ordinal * RANGE_BYTES;
        let raw_section = u16::from_le_bytes(fixed::<2>(bytes, offset));
        let section = SectionKind::try_from(raw_section).map_err(|observed| {
            CompilationManifestError::RangeSection {
                ordinal,
                range: range_ordinal,
                observed,
            }
        })?;
        if section != expected_section {
            return Err(CompilationManifestError::RangeOrder {
                ordinal,
                range: range_ordinal,
                expected: expected_section,
                observed: section,
            });
        }
        let reserved = u16::from_le_bytes(fixed::<2>(bytes, offset + 2));
        if reserved != 0 {
            return Err(CompilationManifestError::RangeReserved {
                ordinal,
                range: range_ordinal,
                observed: reserved,
            });
        }
        let range_offset = u32::from_le_bytes(fixed::<4>(bytes, offset + 4));
        let range_length = u32::from_le_bytes(fixed::<4>(bytes, offset + 8));
        let end = range_offset.checked_add(range_length).ok_or(
            CompilationManifestError::RangeExtentOverflow {
                ordinal,
                range: range_ordinal,
                offset: range_offset,
                length: range_length,
            },
        )?;
        if end > fragment_length {
            return Err(CompilationManifestError::RangeOutsideFragment {
                ordinal,
                range: range_ordinal,
                end,
                fragment_length,
            });
        }
        ranges[range_ordinal] = FragmentRange {
            section,
            offset: range_offset,
            length: range_length,
            identity: ArtifactId::<IrFragmentRangeEncoding, IrFragmentDomain>::try_from(
                &bytes[offset + 12..offset + 44],
            )
            .map_err(|source| CompilationManifestError::RangeIdentity {
                ordinal,
                range: range_ordinal,
                source,
            })?,
        };
    }
    Ok(StoredFragmentFacts {
        fragment,
        fragment_length,
        source,
        recipe,
        ranges,
    })
}

fn fixed<const WIDTH: usize>(input: &[u8], offset: usize) -> [u8; WIDTH] {
    let mut output = [0; WIDTH];
    output.copy_from_slice(&input[offset..offset + WIDTH]);
    output
}

#[cfg(test)]
#[allow(
    clippy::result_large_err,
    reason = "focused tests preserve exact terminal diagnostics through a single typed fixture error"
)]
mod tests {
    use nudox_compile_driver::CompiledFragment;
    use nudox_id::{
        ArtifactId, ContentId, IrManifestDomain, IrManifestEncoding, SourceFactDomain,
        ToolchainDomain,
    };
    use nudox_ir_format::{
        AtomInput, EntityKind, EntityRecord, FragmentView, PreparedFragment, PrimitiveType,
        SourceIdentity, TypeNode,
    };
    use nudox_ir_vocab::{AtomId, TypeId};
    use thiserror::Error;

    use super::{
        CanonicalCompilation, CompilationManifestError, CompilationManifestView,
        CompilationPrepareError, CompilationWriteError,
    };
    use nudox_compile_vocab::{CompileRecipeFact, Language, NativeTool, Stage};

    #[derive(Debug, Error)]
    enum TestError {
        #[error(transparent)]
        Prepare(#[from] nudox_ir_format::PrepareError),
        #[error(transparent)]
        Write(#[from] nudox_ir_format::WriteError),
        #[error(transparent)]
        Fragment(#[from] nudox_ir_format::FragmentError),
        #[error(transparent)]
        Canonical(#[from] CompilationPrepareError),
        #[error(transparent)]
        ManifestWrite(#[from] CompilationWriteError),
        #[error(transparent)]
        Manifest(#[from] CompilationManifestError),
        #[error("manifest was missing expected fragment {ordinal}")]
        MissingFragment { ordinal: u8 },
    }

    #[test]
    fn canonical_manifest_is_order_stable_and_uses_the_ir_manifest_identity()
    -> Result<(), TestError> {
        let (alpha_bytes, alpha_length) = fragment(b"alpha-source", b"alpha")?;
        let (bravo_bytes, bravo_length) = fragment(b"bravo-source", b"bravo")?;
        let mut first_scratch = [0; 2];
        let first_inputs = [
            compiled(FragmentView::validate(&bravo_bytes[..bravo_length])?),
            compiled(FragmentView::validate(&alpha_bytes[..alpha_length])?),
        ];
        let first = CanonicalCompilation::prepare(&first_inputs, &mut first_scratch)?;
        let mut first_output = [0_u8; 1024];
        let mut first_facts = [None; 2];
        let first_manifest = first.write_into(&mut first_output, &mut first_facts)?;
        requires_ir_manifest_identity(first_manifest.identity);

        let mut second_scratch = [0; 2];
        let second_inputs = [
            compiled(FragmentView::validate(&alpha_bytes[..alpha_length])?),
            compiled(FragmentView::validate(&bravo_bytes[..bravo_length])?),
        ];
        let second = CanonicalCompilation::prepare(&second_inputs, &mut second_scratch)?;
        let mut second_output = [0_u8; 1024];
        let mut second_facts = [None; 2];
        let second_manifest = second.write_into(&mut second_output, &mut second_facts)?;
        assert_eq!(first_manifest.as_ref(), second_manifest.as_ref());
        assert_eq!(first_manifest.identity, second_manifest.identity);

        let mut reopened_facts = [None; 2];
        let reopened =
            CompilationManifestView::validate(first_manifest.as_ref(), &mut reopened_facts)?;
        let first_entry = reopened
            .fragments()
            .next()
            .ok_or(TestError::MissingFragment { ordinal: 0 })?;
        let second_entry = reopened
            .fragments()
            .nth(1)
            .ok_or(TestError::MissingFragment { ordinal: 1 })?;
        assert!(first_entry.fragment < second_entry.fragment);
        assert_eq!(reopened.fragment_count, 2);
        Ok(())
    }

    #[test]
    fn manifest_rejects_a_recipe_identity_mutant_without_losing_the_fragment_ordinal()
    -> Result<(), TestError> {
        let (bytes, length) = fragment(b"alpha-source", b"alpha")?;
        let fragment = compiled(FragmentView::validate(&bytes[..length])?);
        let mut scratch = [0];
        let inputs = [fragment];
        let canonical = CanonicalCompilation::prepare(&inputs, &mut scratch)?;
        let mut output = [0_u8; 512];
        let mut facts = [None; 1];
        let manifest = canonical.write_into(&mut output, &mut facts)?;
        let mut mutant = [0_u8; 512];
        mutant[..manifest.as_ref().len()].copy_from_slice(manifest.as_ref());
        let identity_payload =
            super::COMPILATION_MANIFEST_HEADER_BYTES + super::RECIPE_IDENTITY_OFFSET + 2;
        mutant[identity_payload] ^= 1;
        let mut mutant_facts = [None; 1];
        assert!(matches!(
            CompilationManifestView::validate(
                &mutant[..manifest.as_ref().len()],
                &mut mutant_facts
            ),
            Err(CompilationManifestError::RecipeIdentity { ordinal: 0, .. })
        ));
        Ok(())
    }

    #[test]
    fn manifest_rejects_a_range_extent_overflow_without_losing_its_location()
    -> Result<(), TestError> {
        let (bytes, length) = fragment(b"alpha-source", b"alpha")?;
        let fragment = compiled(FragmentView::validate(&bytes[..length])?);
        let inputs = [fragment];
        let mut scratch = [0];
        let canonical = CanonicalCompilation::prepare(&inputs, &mut scratch)?;
        let mut output = [0_u8; 512];
        let mut facts = [None; 1];
        let manifest = canonical.write_into(&mut output, &mut facts)?;
        let mut mutant = [0_u8; 512];
        mutant[..manifest.as_ref().len()].copy_from_slice(manifest.as_ref());
        let offset = super::COMPILATION_MANIFEST_HEADER_BYTES + super::RANGE_OFFSET + 4;
        mutant[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut mutant_facts = [None; 1];
        assert!(matches!(
            CompilationManifestView::validate(
                &mutant[..manifest.as_ref().len()],
                &mut mutant_facts
            ),
            Err(CompilationManifestError::RangeExtentOverflow {
                ordinal: 0,
                range: 0,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn empty_packages_use_empty_ordinal_scratch_and_have_a_canonical_header()
    -> Result<(), TestError> {
        let inputs: &[CompiledFragment<'_>] = &[];
        let mut scratch = [];
        let package = CanonicalCompilation::prepare(inputs, &mut scratch)?;
        assert_eq!(
            package.required_bytes()?,
            super::COMPILATION_MANIFEST_HEADER_BYTES
        );
        let mut output = [0_u8; super::COMPILATION_MANIFEST_HEADER_BYTES];
        let mut facts = [];
        let manifest = package.write_into(&mut output, &mut facts)?;
        assert_eq!(manifest.fragment_count, 0);
        assert!(manifest.fragments().next().is_none());
        Ok(())
    }

    #[test]
    fn canonicalization_reports_undersized_ordinal_scratch() -> Result<(), TestError> {
        let (bytes, length) = fragment(b"alpha-source", b"alpha")?;
        let fragment = compiled(FragmentView::validate(&bytes[..length])?);
        let mut scratch = [];
        assert!(matches!(
            CanonicalCompilation::prepare(&[fragment], &mut scratch),
            Err(CompilationPrepareError::ScratchTooSmall {
                required: 1,
                available: 0,
            })
        ));
        Ok(())
    }

    fn requires_ir_manifest_identity(_: ArtifactId<IrManifestEncoding, IrManifestDomain>) {}

    fn compiled(fragment: FragmentView<'_>) -> CompiledFragment<'_> {
        CompiledFragment {
            source: fragment.source,
            recipe: fragment.recipe,
            fragment,
        }
    }

    fn fragment(source_bytes: &[u8], name: &[u8]) -> Result<([u8; 256], usize), TestError> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            Language::Rust,
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"manifest-toolchain"),
        );
        let entities = [EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        }];
        let nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
        let atoms = [AtomInput { bytes: name }];
        let prepared = PreparedFragment::prepare(source, recipe, &entities, &nodes, &atoms)?;
        let mut output = [0; 256];
        let length = prepared.required_capacity();
        let _ = prepared.write_into(&mut output)?;
        Ok((output, length))
    }
}
