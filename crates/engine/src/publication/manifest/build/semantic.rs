//! Canonical semantic compilation manifest preparation and write.

use crate::driver::CompiledSemantic;

use super::{
    COMPILATION_MANIFEST_HEADER_BYTES, COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES,
    CanonicalSemanticCompilation, CompilationManifestView, CompilationPrepareError,
    CompilationWriteError, MAGIC, PublicationFragment, SEMANTIC_VERSION, SemanticImageRegion,
    StoredFragmentFacts, fragment_identity, write_entry, write_semantic_image,
};

impl<'input, 'scratch, 'fragment, 'images>
    CanonicalSemanticCompilation<'input, 'scratch, 'fragment, 'images>
{
    #[allow(
        clippy::result_large_err,
        reason = "canonical semantic preparation retains exact fragment and image-region faults"
    )]
    pub(crate) fn prepare(
        inputs: &'input [CompiledSemantic<'fragment>],
        images: &'images [SemanticImageRegion],
        semantic_bytes: &'images [u8],
        scratch: &'scratch mut [usize],
    ) -> Result<Self, CompilationPrepareError> {
        if images.len() != inputs.len() {
            return Err(CompilationPrepareError::SemanticImageCount {
                fragments: inputs.len(),
                semantic_images: images.len(),
            });
        }
        if scratch.len() < inputs.len() {
            return Err(CompilationPrepareError::ScratchTooSmall {
                required: inputs.len(),
                available: scratch.len(),
            });
        }
        for (ordinal, image) in images.iter().copied().enumerate() {
            if image.bytes(semantic_bytes).is_none() {
                return Err(CompilationPrepareError::SemanticImageRegion {
                    ordinal,
                    offset: image.offset,
                    byte_length: image.byte_length,
                    available: semantic_bytes.len(),
                });
            }
        }
        let ordinals = &mut scratch[..inputs.len()];
        for (ordinal, slot) in ordinals.iter_mut().enumerate() {
            PublicationFragment::from_compiled(&inputs[ordinal].artifact)
                .map_err(|source| CompilationPrepareError::Fragment { ordinal, source })?;
            *slot = ordinal;
        }
        ordinals.sort_unstable_by_key(|ordinal| fragment_identity(&inputs[*ordinal].artifact));
        for pair in ordinals.windows(2) {
            let previous = fragment_identity(&inputs[pair[0]].artifact);
            let observed = fragment_identity(&inputs[pair[1]].artifact);
            if previous == observed {
                return Err(CompilationPrepareError::DuplicateFragment { identity: previous });
            }
        }
        Ok(Self {
            inputs,
            images,
            semantic_bytes,
            ordinals,
        })
    }

    #[allow(
        clippy::result_large_err,
        reason = "the cold measured-length path preserves its exact preparation terminal"
    )]
    fn required_bytes(&self) -> Result<usize, CompilationPrepareError> {
        let entries = self.ordinals.len();
        COMPILATION_MANIFEST_HEADER_BYTES
            .checked_add(
                entries
                    .checked_mul(COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES)
                    .ok_or(CompilationPrepareError::ManifestLengthOverflow { entries })?,
            )
            .ok_or(CompilationPrepareError::ManifestLengthOverflow { entries })
    }

    #[allow(
        clippy::result_large_err,
        reason = "fresh semantic manifest validation retains exact fragment and image facts"
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
        written[8..10].copy_from_slice(&SEMANTIC_VERSION.to_le_bytes());
        written[10..12].copy_from_slice(&0_u16.to_le_bytes());
        written[12..16].copy_from_slice(&count.to_le_bytes());
        for (ordinal, input_ordinal) in self.ordinals.iter().copied().enumerate() {
            let fragment = PublicationFragment::from_compiled(&self.inputs[input_ordinal].artifact)
                .map_err(|source| CompilationWriteError::Fragment { ordinal, source })?;
            let offset = COMPILATION_MANIFEST_HEADER_BYTES
                + ordinal * COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES;
            let entry = &mut written[offset..offset + COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES];
            write_entry(entry, &fragment.manifest);
            let image = self.images[input_ordinal];
            let facts = image.facts(self.semantic_bytes).ok_or(
                CompilationWriteError::SemanticImageRegion {
                    ordinal,
                    offset: image.offset,
                    byte_length: image.byte_length,
                    available: self.semantic_bytes.len(),
                },
            )?;
            write_semantic_image(entry, facts);
        }
        CompilationManifestView::validate(written, fact_scratch)
            .map_err(CompilationWriteError::FreshValidation)
    }

    pub(crate) fn artifacts(
        &self,
    ) -> impl Iterator<Item = (&CompiledSemantic<'fragment>, SemanticImageRegion)> + '_ {
        self.ordinals
            .iter()
            .copied()
            .map(|ordinal| (&self.inputs[ordinal], self.images[ordinal]))
    }
}
