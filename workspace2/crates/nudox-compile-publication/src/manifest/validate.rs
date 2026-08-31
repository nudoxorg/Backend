use core::{cmp::Ordering, num::TryFromIntError, ops::Deref};

use nudox_compile_vocab::{CompileRecipeFact, Stage};
use nudox_id::{
    ArtifactId, ArtifactIdDecodeError, ContentIdDecodeError, IrFragmentDomain, IrFragmentEncoding,
};
use nudox_ir_format::{FragmentRange, RecipeFact, SectionKind, SourceIdentity};
use thiserror::Error;

use super::{
    CompilationManifestIdentity,
    build::{
        COMPILATION_MANIFEST_ENTRY_BYTES, COMPILATION_MANIFEST_HEADER_BYTES, MAGIC, RANGE_COUNT,
        VERSION,
    },
    wire::{decode_entry, fixed},
};

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
