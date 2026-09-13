//! Defines publication open behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the publication open invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::path::Path;

use backend_semantic::ir::{FragmentRangeManifest, ImageProvenance, SemanticCoreReader};
use server_journal::DurablePublisher;

use super::types::{
    OpenPublicationScratch, OpenPublishedError, OpenSemanticPublicationScratch, OpenedCompilation,
    OpenedSemanticCompilation, OpenedSemanticGeneration, SemanticGenerationRequirements,
};
use crate::{
    binding_store::GenerationBindingStore,
    generation::{verify_reopened_generation, verify_reopened_semantic_generation},
    immutable::ImmutableArtifactStore,
    manifest::{CompilationManifestFacts, CompilationManifestFormat, StoredFragmentFacts},
    manifest_store::ImmutableManifestStore,
    semantic_immutable::ImmutableSemanticImageStore,
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
    scratch: OpenPublicationScratch<'manifest, 'facts, 'manifest, '_>,
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
    if manifest.format != CompilationManifestFormat::CompactV1 {
        return Err(OpenPublishedError::ManifestFormat {
            expected: CompilationManifestFormat::CompactV1,
            observed: manifest.format,
        });
    }
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
        fragments: fragment_output,
    }))
}

/// Reopens the journal-selected schema-2 semantic package and validates every
/// compact fragment, full semantic image, and generation-closure fact before
/// returning a paired borrowed reader.
#[allow(
    clippy::result_large_err,
    reason = "the cold terminal retains exact durable facts and immutable corruption evidence"
)]
pub fn open_published_semantic<'manifest, 'facts, 'fragments, 'semantic>(
    publisher: &DurablePublisher,
    artifact_directory: &Path,
    scratch: OpenSemanticPublicationScratch<'manifest, 'facts, 'fragments, 'semantic, '_>,
) -> Result<
    Option<OpenedSemanticCompilation<'manifest, 'facts, 'fragments, 'semantic>>,
    OpenPublishedError,
> {
    let Some(publication) = publisher.published().map_err(OpenPublishedError::Journal)? else {
        return Ok(None);
    };
    let binding = load_semantic_binding(artifact_directory, publication.generation)?;
    if binding.generation != publication.generation {
        return Err(OpenPublishedError::BindingGeneration {
            publication,
            binding,
        });
    }
    let generation =
        open_semantic_generation_from_binding(binding, None, artifact_directory, scratch)?;
    Ok(Some(OpenedSemanticCompilation {
        publication,
        generation,
    }))
}

/// Reopens one exact immutable semantic generation independently of the local journal head.
///
/// # Errors
///
/// Returns exact binding, manifest, artifact, provenance, or closure-integrity evidence.
/// The returned proof is created only when both caller-declared facts match the bytes
/// in this publication owner's immutable store.
#[allow(
    clippy::result_large_err,
    reason = "the cold terminal retains exact expected and observed immutable facts"
)]
pub fn open_semantic_generation<'manifest, 'facts, 'fragments, 'semantic>(
    expected_manifest: CompilationManifestFacts,
    expected_binding: crate::binding::CompilationBindingFacts,
    artifact_directory: &Path,
    scratch: OpenSemanticPublicationScratch<'manifest, 'facts, 'fragments, 'semantic, '_>,
) -> Result<OpenedSemanticGeneration<'manifest, 'facts, 'fragments, 'semantic>, OpenPublishedError>
{
    let observed = load_semantic_binding(artifact_directory, expected_binding.generation)?;
    if observed != expected_binding {
        return Err(OpenPublishedError::BindingFacts {
            expected: expected_binding,
            observed,
        });
    }
    open_semantic_generation_from_binding(
        observed,
        Some(expected_manifest),
        artifact_directory,
        scratch,
    )
}

fn load_semantic_binding(
    artifact_directory: &Path,
    generation: heart_hydration::VerifiedGenerationFacts,
) -> Result<crate::binding::CompilationBindingFacts, OpenPublishedError> {
    GenerationBindingStore::existing(artifact_directory)
        .load(generation)
        .map_err(OpenPublishedError::Binding)?
        .map(|stored| stored.facts)
        .ok_or(OpenPublishedError::MissingBinding { generation })
}

/// Reads and admits the exact manifest named by a semantic claim, returning
/// the output widths needed for a complete immutable reopen.
///
/// # Errors
///
/// Returns exact binding, manifest, schema, identity, or address-space facts.
#[allow(
    clippy::result_large_err,
    reason = "the cold terminal retains exact expected and observed immutable facts"
)]
pub fn semantic_generation_requirements(
    expected_manifest: CompilationManifestFacts,
    expected_binding: crate::binding::CompilationBindingFacts,
    artifact_directory: &Path,
    manifest_output: &mut [u8],
    manifest_facts: &mut [Option<StoredFragmentFacts>],
) -> Result<SemanticGenerationRequirements, OpenPublishedError> {
    let observed = load_semantic_binding(artifact_directory, expected_binding.generation)?;
    if observed != expected_binding {
        return Err(OpenPublishedError::BindingFacts {
            expected: expected_binding,
            observed,
        });
    }
    let manifest = ImmutableManifestStore::existing(artifact_directory)
        .open(observed.manifest, manifest_output, manifest_facts)
        .map_err(OpenPublishedError::Manifest)?;
    if *manifest != expected_manifest {
        return Err(OpenPublishedError::ManifestFacts {
            expected: expected_manifest,
            observed: *manifest,
        });
    }
    if manifest.format != CompilationManifestFormat::SemanticV2 {
        return Err(OpenPublishedError::ManifestFormat {
            expected: CompilationManifestFormat::SemanticV2,
            observed: manifest.format,
        });
    }
    semantic_requirements(&manifest)
}

fn open_semantic_generation_from_binding<'manifest, 'facts, 'fragments, 'semantic>(
    binding: crate::binding::CompilationBindingFacts,
    expected_manifest: Option<CompilationManifestFacts>,
    artifact_directory: &Path,
    scratch: OpenSemanticPublicationScratch<'manifest, 'facts, 'fragments, 'semantic, '_>,
) -> Result<OpenedSemanticGeneration<'manifest, 'facts, 'fragments, 'semantic>, OpenPublishedError>
{
    let OpenSemanticPublicationScratch {
        manifest_output,
        manifest_facts,
        fragment_output,
        semantic_image_output,
        locality_output,
    } = scratch;
    let manifests = ImmutableManifestStore::existing(artifact_directory);
    let manifest = manifests
        .open(binding.manifest, manifest_output, manifest_facts)
        .map_err(OpenPublishedError::Manifest)?;
    if let Some(expected) = expected_manifest
        && *manifest != expected
    {
        return Err(OpenPublishedError::ManifestFacts {
            expected,
            observed: *manifest,
        });
    }
    if manifest.format != CompilationManifestFormat::SemanticV2 {
        return Err(OpenPublishedError::ManifestFormat {
            expected: CompilationManifestFormat::SemanticV2,
            observed: manifest.format,
        });
    }
    let requirements = semantic_requirements(&manifest)?;
    let required_fragments = requirements.fragment_bytes;
    let required_semantic = requirements.semantic_image_bytes;
    if fragment_output.len() < required_fragments {
        return Err(OpenPublishedError::FragmentOutputTooSmall {
            required: required_fragments,
            available: fragment_output.len(),
        });
    }
    if semantic_image_output.len() < required_semantic {
        return Err(OpenPublishedError::SemanticOutputTooSmall {
            required: required_semantic,
            available: semantic_image_output.len(),
        });
    }
    let fragment_output = &mut fragment_output[..required_fragments];
    let semantic_image_output = &mut semantic_image_output[..required_semantic];
    let fragments = ImmutableArtifactStore::existing(artifact_directory);
    let semantic_images = ImmutableSemanticImageStore::existing(artifact_directory);
    let mut fragment_offset = 0_usize;
    let mut semantic_offset = 0_usize;
    for (ordinal, expected) in manifest.fragments().enumerate() {
        let fragment_length = usize::try_from(expected.fragment_length).map_err(|source| {
            OpenPublishedError::FragmentOutputLengthAddressSpace {
                fragment: expected.fragment,
                source,
            }
        })?;
        let fragment_end = fragment_offset
            .checked_add(fragment_length)
            .ok_or(OpenPublishedError::FragmentOutputLengthOverflow)?;
        let fragment = fragments
            .open(
                expected,
                &mut fragment_output[fragment_offset..fragment_end],
            )
            .map_err(|source| OpenPublishedError::Fragment { ordinal, source })?;
        let ranges = FragmentRangeManifest::from_view(&fragment)
            .map_err(|source| OpenPublishedError::FragmentRanges { ordinal, source })?;
        let observed = stored_fragment_facts_with_semantic(ranges, expected.semantic_image);
        if observed != expected {
            return Err(OpenPublishedError::FragmentFacts {
                ordinal,
                expected,
                observed,
            });
        }
        fragment_offset = fragment_end;

        let semantic = expected
            .semantic_image
            .ok_or(OpenPublishedError::MissingSemanticImage {
                fragment: expected.fragment,
            })?;
        let semantic_length = usize::try_from(semantic.byte_length).map_err(|source| {
            OpenPublishedError::SemanticOutputLengthAddressSpace {
                semantic_image: semantic.identity,
                source,
            }
        })?;
        let semantic_end = semantic_offset
            .checked_add(semantic_length)
            .ok_or(OpenPublishedError::SemanticOutputLengthOverflow)?;
        let semantic_image = semantic_images
            .open(
                semantic,
                &mut semantic_image_output[semantic_offset..semantic_end],
            )
            .map_err(|source| OpenPublishedError::SemanticImage { ordinal, source })?;
        match semantic_image.image_facts().provenance {
            ImageProvenance::Unavailable => {
                return Err(OpenPublishedError::SemanticProvenanceUnavailable { ordinal });
            }
            ImageProvenance::Captured { source, recipe, .. } => {
                if source != expected.source {
                    return Err(OpenPublishedError::SemanticSource {
                        ordinal,
                        expected: expected.source,
                        observed: source,
                    });
                }
                if recipe != expected.recipe {
                    return Err(OpenPublishedError::SemanticRecipe {
                        ordinal,
                        expected: expected.recipe,
                        observed: recipe,
                    });
                }
            }
        }
        semantic_offset = semantic_end;
    }
    let rebuilt = verify_reopened_semantic_generation(
        &manifest,
        fragment_output,
        semantic_image_output,
        locality_output,
    )
    .map_err(OpenPublishedError::Generation)?;
    if rebuilt != binding.generation {
        return Err(OpenPublishedError::SemanticGenerationMismatch {
            expected: binding.generation,
            rebuilt,
        });
    }
    Ok(OpenedSemanticGeneration {
        binding,
        manifest,
        fragments: fragment_output,
        semantic_images: semantic_image_output,
    })
}

fn semantic_requirements(
    manifest: &crate::manifest::CompilationManifestView<'_, '_>,
) -> Result<SemanticGenerationRequirements, OpenPublishedError> {
    let mut fragment_bytes = 0_usize;
    let mut semantic_image_bytes = 0_usize;
    for facts in manifest.fragments() {
        let fragment_length = usize::try_from(facts.fragment_length).map_err(|source| {
            OpenPublishedError::FragmentOutputLengthAddressSpace {
                fragment: facts.fragment,
                source,
            }
        })?;
        fragment_bytes = fragment_bytes
            .checked_add(fragment_length)
            .ok_or(OpenPublishedError::FragmentOutputLengthOverflow)?;
        let semantic = facts
            .semantic_image
            .ok_or(OpenPublishedError::MissingSemanticImage {
                fragment: facts.fragment,
            })?;
        let semantic_length = usize::try_from(semantic.byte_length).map_err(|source| {
            OpenPublishedError::SemanticOutputLengthAddressSpace {
                semantic_image: semantic.identity,
                source,
            }
        })?;
        semantic_image_bytes = semantic_image_bytes
            .checked_add(semantic_length)
            .ok_or(OpenPublishedError::SemanticOutputLengthOverflow)?;
    }
    Ok(SemanticGenerationRequirements {
        fragment_bytes,
        semantic_image_bytes,
    })
}

fn stored_fragment_facts(manifest: FragmentRangeManifest) -> StoredFragmentFacts {
    stored_fragment_facts_with_semantic(manifest, None)
}

fn stored_fragment_facts_with_semantic(
    manifest: FragmentRangeManifest,
    semantic_image: Option<crate::semantic_immutable::SemanticImageArtifactFacts>,
) -> StoredFragmentFacts {
    StoredFragmentFacts {
        fragment: manifest.fragment,
        fragment_length: manifest.fragment_length,
        semantic_image,
        source: manifest.source,
        recipe: manifest.recipe,
        ranges: manifest.ranges,
    }
}
