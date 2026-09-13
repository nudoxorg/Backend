//! Defines publication publish behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the publication publish invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::path::Path;

use compiler_driver::{CompiledFragment, CompiledSemantic};
use backend_semantic::ir::{
    FragmentRangeManifest, ImageProvenance, SemanticCoreReader, SemanticImageView,
    encode_full_semantic_image, full_semantic_image_len,
};
use heart_hydration::VerifiedGeneration;
use backend_version::ObjectDomain;
use server_journal::{CancelError, DurablePublisher, PublicationError, SubmitError};

use super::types::{
    PublicationScratch, PublishCompiledError, PublishControl, PublishSemanticError,
    PublishedCompilation, SemanticPublicationScratch, UncommittedPublication,
    UncommittedPublicationFacts,
};
use crate::{
    binding::{COMPILATION_BINDING_BYTES, CompilationBindingView},
    binding_store::GenerationBindingStore,
    generation::{with_verified_generation, with_verified_semantic_generation},
    immutable::ImmutableArtifactStore,
    manifest::{CanonicalCompilation, CanonicalSemanticCompilation, SemanticImageRegion},
    manifest_store::ImmutableManifestStore,
    semantic_immutable::ImmutableSemanticImageStore,
};

/// Publishes only validated outputs issued by `compiler-driver::compile`.
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
        complete_publication(
            publisher,
            artifact_directory,
            control,
            &manifest,
            scratch.binding_output,
            verified,
        )
    })
    .map_err(PublishCompiledError::Generation)?
}

/// Publishes fused compact and full semantic output from one authority traversal.
///
/// Every semantic image is measured before caller output changes, encoded in
/// input order, fully reopened once, checked against the compact artifact's
/// exact source and recipe, then stored under its typed identity.  Journal
/// admission occurs only after the schema-2 manifest and both artifact classes
/// form a verified complete generation.
#[allow(
    clippy::result_large_err,
    reason = "the cold terminal retains exact semantic, storage, generation, and journal causes"
)]
pub fn publish_semantic(
    publisher: &DurablePublisher,
    artifact_directory: &Path,
    compiled: &[CompiledSemantic<'_>],
    control: PublishControl<'_>,
    scratch: SemanticPublicationScratch<'_, '_, '_, '_, '_, '_, '_>,
) -> Result<PublishedCompilation, PublishSemanticError> {
    if control.cancelled_before_storage() {
        return Err(PublishSemanticError::CancelledBeforeStorage);
    }
    if scratch.semantic_image_plan.len() < compiled.len() {
        return Err(PublishSemanticError::ImagePlanTooSmall {
            required: compiled.len(),
            available: scratch.semantic_image_plan.len(),
        });
    }
    if scratch.binding_output.len() < COMPILATION_BINDING_BYTES {
        return Err(PublishSemanticError::Publication(
            PublishCompiledError::BindingOutputLength {
                observed: scratch.binding_output.len(),
            },
        ));
    }
    let image_plan = &mut scratch.semantic_image_plan[..compiled.len()];
    let mut required = 0_usize;
    for (ordinal, output) in compiled.iter().enumerate() {
        let length = full_semantic_image_len(&output.ir)
            .map_err(|source| PublishSemanticError::ImageMeasure { ordinal, source })?;
        let byte_length = u32::try_from(length).map_err(|source| {
            PublishSemanticError::ImageLengthAddressSpace {
                ordinal,
                observed: length,
                source,
            }
        })?;
        let offset = required;
        required =
            required
                .checked_add(length)
                .ok_or(PublishSemanticError::ImageExtentOverflow {
                    ordinal,
                    offset,
                    length,
                })?;
        image_plan[ordinal] = SemanticImageRegion::from_measurement(offset, byte_length);
    }
    if scratch.semantic_image_output.len() < required {
        return Err(PublishSemanticError::ImageOutputTooSmall {
            required,
            available: scratch.semantic_image_output.len(),
        });
    }
    for (ordinal, (planned, output)) in image_plan.iter().copied().zip(compiled).enumerate() {
        let length = usize::try_from(u64::from(planned.byte_length)).map_err(|source| {
            PublishSemanticError::ImagePlanLengthAddressSpace {
                ordinal,
                observed: planned.byte_length,
                source,
            }
        })?;
        let end = planned.offset.checked_add(length).ok_or(
            PublishSemanticError::ImageExtentOverflow {
                ordinal,
                offset: planned.offset,
                length,
            },
        )?;
        let output_region = scratch
            .semantic_image_output
            .get_mut(planned.offset..end)
            .ok_or(PublishSemanticError::ImageExtentOverflow {
                ordinal,
                offset: planned.offset,
                length,
            })?;
        let written = encode_full_semantic_image(&output.ir, output_region)
            .map_err(|source| PublishSemanticError::ImageEncode { ordinal, source })?;
        if written != length {
            return Err(PublishSemanticError::ImageWriteLengthMismatch {
                ordinal,
                expected: length,
                observed: written,
            });
        }
    }
    if control.cancelled_before_storage() {
        return Err(PublishSemanticError::CancelledBeforeStorage);
    }

    let mut semantic_store = ImmutableSemanticImageStore::new(artifact_directory)
        .map_err(PublishSemanticError::SemanticStorageOwner)?;
    for (ordinal, (planned, output)) in image_plan.iter().copied().zip(compiled).enumerate() {
        let length = usize::try_from(u64::from(planned.byte_length)).map_err(|source| {
            PublishSemanticError::ImagePlanLengthAddressSpace {
                ordinal,
                observed: planned.byte_length,
                source,
            }
        })?;
        let end = planned.offset.checked_add(length).ok_or(
            PublishSemanticError::ImageExtentOverflow {
                ordinal,
                offset: planned.offset,
                length,
            },
        )?;
        let bytes = scratch
            .semantic_image_output
            .get(planned.offset..end)
            .ok_or(PublishSemanticError::ImageExtentOverflow {
                ordinal,
                offset: planned.offset,
                length,
            })?;
        let view = SemanticImageView::reopen(bytes)
            .map_err(|source| PublishSemanticError::ImageReopen { ordinal, source })?;
        match view.image_facts().provenance {
            ImageProvenance::Unavailable => {
                return Err(PublishSemanticError::ImageProvenanceUnavailable { ordinal });
            }
            ImageProvenance::Captured { source, recipe, .. } => {
                if source != output.artifact.source {
                    return Err(PublishSemanticError::ImageSource {
                        ordinal,
                        expected: output.artifact.source,
                        observed: source,
                    });
                }
                if recipe != output.artifact.recipe {
                    return Err(PublishSemanticError::ImageRecipe {
                        ordinal,
                        expected: output.artifact.recipe,
                        observed: recipe,
                    });
                }
            }
        }
        let stored = semantic_store
            .ensure(&view)
            .map_err(|source| PublishSemanticError::SemanticStorage { ordinal, source })?;
        let expected =
            planned
                .facts_for(bytes)
                .ok_or(PublishSemanticError::ImageExtentOverflow {
                    ordinal,
                    offset: planned.offset,
                    length,
                })?;
        if stored.facts != expected {
            return Err(PublishSemanticError::SemanticStorageFacts {
                ordinal,
                expected,
                observed: stored.facts,
            });
        }
    }

    let canonical = CanonicalSemanticCompilation::prepare(
        compiled,
        image_plan,
        &scratch.semantic_image_output[..required],
        scratch.ordinals,
    )
    .map_err(PublishSemanticError::Canonical)?;
    let manifest = canonical
        .write_into(scratch.manifest_output, scratch.manifest_facts)
        .map_err(PublishSemanticError::ManifestWrite)?;
    let mut fragments = ImmutableArtifactStore::new(artifact_directory)
        .map_err(PublishSemanticError::FragmentStorageOwner)?;
    for (ordinal, (output, _)) in canonical.artifacts().enumerate() {
        let ranges = FragmentRangeManifest::from_view(&output.artifact.fragment)
            .map_err(|source| PublishSemanticError::FragmentManifest { ordinal, source })?;
        fragments
            .ensure(&ranges, &output.artifact.fragment)
            .map_err(|source| PublishSemanticError::FragmentStorage { ordinal, source })?;
    }
    let mut manifests = ImmutableManifestStore::new(artifact_directory).map_err(|source| {
        PublishSemanticError::Publication(PublishCompiledError::ManifestStorage(source))
    })?;
    manifests.ensure(&manifest).map_err(|source| {
        PublishSemanticError::Publication(PublishCompiledError::ManifestStorage(source))
    })?;
    with_verified_semantic_generation(
        &canonical,
        &manifest,
        &scratch.semantic_image_output[..required],
        scratch.locality_output,
        |verified| {
            complete_publication(
                publisher,
                artifact_directory,
                control,
                &manifest,
                scratch.binding_output,
                verified,
            )
        },
    )
    .map_err(|source| PublishSemanticError::Publication(PublishCompiledError::Generation(source)))?
    .map_err(PublishSemanticError::Publication)
}

#[allow(
    clippy::result_large_err,
    reason = "the durable completion terminal retains exact binding, generation, and journal facts"
)]
fn complete_publication(
    publisher: &DurablePublisher,
    artifact_directory: &Path,
    control: PublishControl<'_>,
    manifest: &crate::manifest::CompilationManifestView<'_, '_>,
    binding_output: &mut [u8],
    verified: VerifiedGeneration<'_, ObjectDomain, &'_ [u8]>,
) -> Result<PublishedCompilation, PublishCompiledError> {
    let expected_generation = *verified;
    let binding =
        CompilationBindingView::write_into(expected_generation, manifest.identity, binding_output)
            .map_err(PublishCompiledError::BindingWrite)?;
    let binding_facts = *binding;
    let mut bindings = GenerationBindingStore::new(artifact_directory)
        .map_err(PublishCompiledError::BindingStorage)?;
    bindings
        .ensure(&binding)
        .map_err(PublishCompiledError::BindingStorage)?;
    let attempted = UncommittedPublicationFacts {
        generation: expected_generation,
        manifest: **manifest,
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
    };
    if published.generation != binding_facts.generation {
        return Err(PublishCompiledError::StableGenerationMismatch {
            binding: binding_facts,
            publication: published,
        });
    }
    Ok(PublishedCompilation {
        publication: published,
        manifest: **manifest,
        binding: binding_facts,
    })
}
