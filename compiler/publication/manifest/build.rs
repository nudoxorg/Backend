//! Defines manifest build behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the manifest build invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Canonical package manifests over complete recipe-bearing IR fragments.

use core::{num::TryFromIntError, ops::Deref};

use compiler_driver::CompiledFragment;
use compiler_ir::{
    FragmentRangeManifest, FragmentRangeManifestError, FragmentView, RecipeFact, SectionKind,
    SourceIdentity,
};
use compiler_vocabulary::{CompileRecipeFact, Stage};
use heart_identity::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};
use thiserror::Error;

use super::{
    CompilationManifestError, CompilationManifestView, StoredFragmentFacts,
    wire::{fragment_identity, write_entry},
};

/// Typed identity of one complete canonical compiler package manifest.
/// Fixed header width of the compiler package manifest wire format.
pub const COMPILATION_MANIFEST_HEADER_BYTES: usize = 16;
/// Fixed width of one complete recipe-bearing fragment entry.
pub const COMPILATION_MANIFEST_ENTRY_BYTES: usize = 404;

pub(super) const MAGIC: [u8; 8] = *b"NUDXCPM\0";
pub(super) const VERSION: u16 = 1;
pub(super) const RANGE_COUNT: usize = 6;
pub(super) const RANGE_BYTES: usize = 44;
pub(super) const SOURCE_IDENTITY_OFFSET: usize = 0;
pub(super) const SOURCE_LENGTH_OFFSET: usize = 32;
pub(super) const RECIPE_IDENTITY_OFFSET: usize = 36;
pub(super) const RECIPE_PROFILE_OFFSET: usize = 68;
pub(super) const RECIPE_STAGE_OFFSET: usize = 70;
pub(super) const RECIPE_TOOL_OFFSET: usize = 71;
pub(super) const RECIPE_TOOLCHAIN_OFFSET: usize = 72;
pub(super) const FRAGMENT_IDENTITY_OFFSET: usize = 104;
pub(super) const FRAGMENT_LENGTH_OFFSET: usize = 136;
pub(super) const RANGE_OFFSET: usize = 140;
pub(super) const SECTION_ORDER: [compiler_ir::SectionKind; RANGE_COUNT] = [
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
            manifest.recipe.profile,
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
