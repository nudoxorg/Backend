use std::path::Path;

use nudox_durable_journal::DurablePublisher;
use nudox_ir_format::FragmentRangeManifest;

use super::types::{OpenPublicationScratch, OpenPublishedError, OpenedCompilation};
use crate::{
    binding_store::GenerationBindingStore, generation::verify_reopened_generation,
    immutable::ImmutableArtifactStore, manifest::StoredFragmentFacts,
    manifest_store::ImmutableManifestStore,
};

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
            publication,
            binding: binding.facts,
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
                ordinal,
                expected,
                observed,
            });
        }
        offset = end;
    }
    let rebuilt = verify_reopened_generation(&manifest, fragment_output, locality_output)
        .map_err(OpenPublishedError::Generation)?;
    if rebuilt != publication.generation || rebuilt != binding.facts.generation {
        return Err(OpenPublishedError::GenerationMismatch {
            publication,
            binding: binding.facts,
            rebuilt,
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
