//! Defines publication publish behavior for the `backend-engine` publication, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the publication publish invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::path::Path;

use crate::driver::{CompiledFragment, CompiledSemantic};
use backend_semantic::ir::{
    FragmentRangeManifest, FragmentView, ImageProvenance, SemanticCoreReader, SemanticImageView,
    encode_full_semantic_image, full_semantic_image_len,
};
use backend_store::hydration::VerifiedGeneration;
use backend_store::journal::{CancelError, DurablePublisher, PublicationError, SubmitError};
use backend_version::ObjectDomain;

use super::types::{
    PublicationScratch, PublishCompiledError, PublishControl, PublishSemanticError,
    PublishedCompilation, SemanticPublicationScratch, StagedSemanticObjectClaim,
    UncommittedPublication, UncommittedPublicationFacts,
};
use crate::publication::{
    binding::{COMPILATION_BINDING_BYTES, CompilationBindingView},
    binding_store::GenerationBindingStore,
    generation::{with_verified_generation, with_verified_semantic_generation},
    immutable::ImmutableArtifactStore,
    manifest::{CanonicalCompilation, CanonicalSemanticCompilation, SemanticImageRegion},
    manifest_store::ImmutableManifestStore,
    semantic_immutable::ImmutableSemanticImageStore,
};

/// Publishes only validated outputs issued by `crate::driver::compile`.
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

    let mut fragments = Vec::new();
    fragments
        .try_reserve_exact(compiled.len())
        .map_err(PublishSemanticError::ArtifactDescriptorAllocation)?;
    for (ordinal, output) in compiled.iter().enumerate() {
        let fragment = FragmentView::validate(output.artifact.fragment.as_ref())
            .map_err(|source| PublishSemanticError::FragmentValidation { ordinal, source })?;
        fragments.push(CompiledFragment {
            source: output.artifact.source,
            recipe: output.artifact.recipe,
            fragment,
        });
    }

    publish_semantic_encoded(
        publisher,
        artifact_directory,
        &fragments,
        image_plan,
        &scratch.semantic_image_output[..required],
        control,
        scratch.manifest_output,
        scratch.manifest_facts,
        scratch.ordinals,
        scratch.locality_output,
        scratch.binding_output,
    )
}

/// Publishes already measured canonical image bytes from an isolated compile lane.
///
/// Image regions and compact fragments are reopened and checked against the exact source and
/// recipe authorities before entering the same immutable-storage, generation, and durable
/// selection path as [`publish_semantic`].
#[allow(
    clippy::result_large_err,
    reason = "the cold terminal retains exact semantic, storage, generation, and journal causes"
)]
pub(crate) fn publish_semantic_bytes(
    publisher: &DurablePublisher,
    artifact_directory: &Path,
    compiled: &[CompiledFragment<'_>],
    image_plan: &[SemanticImageRegion],
    semantic_bytes: &[u8],
    control: PublishControl<'_>,
    scratch: SemanticPublicationScratch<'_, '_, '_, '_, '_, '_, '_>,
) -> Result<PublishedCompilation, PublishSemanticError> {
    if image_plan.len() != compiled.len() {
        return Err(PublishSemanticError::ImageCountMismatch {
            fragments: compiled.len(),
            semantic_images: image_plan.len(),
        });
    }
    if scratch.semantic_image_plan.len() < image_plan.len() {
        return Err(PublishSemanticError::ImagePlanTooSmall {
            required: image_plan.len(),
            available: scratch.semantic_image_plan.len(),
        });
    }
    scratch.semantic_image_plan[..image_plan.len()].copy_from_slice(image_plan);
    publish_semantic_encoded(
        publisher,
        artifact_directory,
        compiled,
        &scratch.semantic_image_plan[..image_plan.len()],
        semantic_bytes,
        control,
        scratch.manifest_output,
        scratch.manifest_facts,
        scratch.ordinals,
        scratch.locality_output,
        scratch.binding_output,
    )
}

#[allow(
    clippy::result_large_err,
    reason = "the cold terminal retains exact semantic, storage, generation, and journal causes"
)]
fn publish_semantic_encoded(
    publisher: &DurablePublisher,
    artifact_directory: &Path,
    compiled: &[CompiledFragment<'_>],
    image_plan: &[SemanticImageRegion],
    semantic_bytes: &[u8],
    control: PublishControl<'_>,
    manifest_output: &mut [u8],
    manifest_facts: &mut [Option<crate::publication::manifest::StoredFragmentFacts>],
    ordinals: &mut [usize],
    locality_output: &mut [u8],
    binding_output: &mut [u8],
) -> Result<PublishedCompilation, PublishSemanticError> {
    with_prepared_semantic_encoded(
        compiled,
        image_plan,
        semantic_bytes,
        control,
        manifest_output,
        manifest_facts,
        ordinals,
        locality_output,
        binding_output,
        |_root, canonical, manifest, binding, verified| {
            let mut semantic_store = ImmutableSemanticImageStore::new(artifact_directory)
                .map_err(PublishSemanticError::SemanticStorageOwner)?;
            for (ordinal, (_, planned)) in canonical.artifacts().enumerate() {
                let bytes = planned.bytes(semantic_bytes).ok_or(
                    PublishSemanticError::ImageExtentOverflow {
                        ordinal,
                        offset: planned.offset,
                        length: planned.byte_length as usize,
                    },
                )?;
                let view = SemanticImageView::reopen(bytes)
                    .map_err(|source| PublishSemanticError::ImageReopen { ordinal, source })?;
                let expected =
                    planned
                        .facts_for(bytes)
                        .ok_or(PublishSemanticError::ImageExtentOverflow {
                            ordinal,
                            offset: planned.offset,
                            length: planned.byte_length as usize,
                        })?;
                let stored = semantic_store
                    .ensure(&view)
                    .map_err(|source| PublishSemanticError::SemanticStorage { ordinal, source })?;
                if stored.facts != expected {
                    return Err(PublishSemanticError::SemanticStorageFacts {
                        ordinal,
                        expected,
                        observed: stored.facts,
                    });
                }
            }

            let mut fragments = ImmutableArtifactStore::new(artifact_directory)
                .map_err(PublishSemanticError::FragmentStorageOwner)?;
            for (ordinal, (output, _)) in canonical.artifacts().enumerate() {
                let ranges = FragmentRangeManifest::from_view(&output.artifact.fragment)
                    .map_err(|source| PublishSemanticError::FragmentManifest { ordinal, source })?;
                fragments
                    .ensure(&ranges, &output.artifact.fragment)
                    .map_err(|source| PublishSemanticError::FragmentStorage { ordinal, source })?;
            }
            let mut manifests =
                ImmutableManifestStore::new(artifact_directory).map_err(|source| {
                    PublishSemanticError::Publication(PublishCompiledError::ManifestStorage(source))
                })?;
            manifests.ensure(manifest).map_err(|source| {
                PublishSemanticError::Publication(PublishCompiledError::ManifestStorage(source))
            })?;
            complete_publication_with_binding(
                publisher,
                artifact_directory,
                control,
                manifest,
                binding,
                verified,
            )
            .map_err(PublishSemanticError::Publication)
        },
    )
}

fn with_prepared_semantic_encoded<Output>(
    compiled: &[CompiledFragment<'_>],
    image_plan: &[SemanticImageRegion],
    semantic_bytes: &[u8],
    control: PublishControl<'_>,
    manifest_output: &mut [u8],
    manifest_facts: &mut [Option<crate::publication::manifest::StoredFragmentFacts>],
    ordinals: &mut [usize],
    locality_output: &mut [u8],
    binding_output: &mut [u8],
    visit: impl FnOnce(
        &backend_store::root::GenerationRoot<ObjectDomain>,
        &CanonicalSemanticCompilation<'_, '_, '_, '_>,
        &crate::publication::manifest::CompilationManifestView<'_, '_>,
        &CompilationBindingView<'_>,
        VerifiedGeneration<'_, ObjectDomain, &'_ [u8]>,
    ) -> Result<Output, PublishSemanticError>,
) -> Result<Output, PublishSemanticError> {
    if control.cancelled_before_storage() {
        return Err(PublishSemanticError::CancelledBeforeStorage);
    }
    if image_plan.len() != compiled.len() {
        return Err(PublishSemanticError::ImageCountMismatch {
            fragments: compiled.len(),
            semantic_images: image_plan.len(),
        });
    }
    if binding_output.len() < COMPILATION_BINDING_BYTES {
        return Err(PublishSemanticError::Publication(
            PublishCompiledError::BindingOutputLength {
                observed: binding_output.len(),
            },
        ));
    }
    let mut expected_offset = 0_usize;
    for (ordinal, region) in image_plan.iter().copied().enumerate() {
        if region.offset != expected_offset {
            return Err(PublishSemanticError::ImageRegionOffset {
                ordinal,
                expected: expected_offset,
                observed: region.offset,
            });
        }
        let length = usize::try_from(region.byte_length).map_err(|source| {
            PublishSemanticError::ImagePlanLengthAddressSpace {
                ordinal,
                observed: region.byte_length,
                source,
            }
        })?;
        expected_offset = expected_offset.checked_add(length).ok_or(
            PublishSemanticError::ImageExtentOverflow {
                ordinal,
                offset: region.offset,
                length,
            },
        )?;
    }
    if expected_offset != semantic_bytes.len() {
        return Err(PublishSemanticError::ImageBytesLength {
            expected: expected_offset,
            observed: semantic_bytes.len(),
        });
    }
    for (ordinal, (planned, output)) in image_plan.iter().copied().zip(compiled).enumerate() {
        let bytes =
            planned
                .bytes(semantic_bytes)
                .ok_or(PublishSemanticError::ImageExtentOverflow {
                    ordinal,
                    offset: planned.offset,
                    length: planned.byte_length as usize,
                })?;
        let view = SemanticImageView::reopen(bytes)
            .map_err(|source| PublishSemanticError::ImageReopen { ordinal, source })?;
        match view.image_facts().provenance {
            ImageProvenance::Unavailable => {
                return Err(PublishSemanticError::ImageProvenanceUnavailable { ordinal });
            }
            ImageProvenance::Captured { source, recipe, .. } => {
                if source != output.source {
                    return Err(PublishSemanticError::ImageSource {
                        ordinal,
                        expected: output.source,
                        observed: source,
                    });
                }
                if recipe != output.recipe {
                    return Err(PublishSemanticError::ImageRecipe {
                        ordinal,
                        expected: output.recipe,
                        observed: recipe,
                    });
                }
            }
        }
    }

    let canonical =
        CanonicalSemanticCompilation::prepare(compiled, image_plan, semantic_bytes, ordinals)
            .map_err(PublishSemanticError::Canonical)?;
    let manifest = canonical
        .write_into(manifest_output, manifest_facts)
        .map_err(PublishSemanticError::ManifestWrite)?;
    let verified = with_verified_semantic_generation(
        &canonical,
        &manifest,
        semantic_bytes,
        locality_output,
        |root, verified| {
            let binding =
                CompilationBindingView::write_into(*verified, manifest.identity, binding_output)
                    .map_err(|source| {
                        PublishSemanticError::Publication(PublishCompiledError::BindingWrite(
                            source,
                        ))
                    })?;
            visit(&root, &canonical, &manifest, &binding, verified)
        },
    )
    .map_err(|source| {
        PublishSemanticError::Publication(PublishCompiledError::Generation(source))
    })?;
    verified
}

/// Builds exact canonical manifest, binding, object-claim, and complete-generation facts without
/// writing immutable files or selecting a local journal head.
#[allow(
    clippy::result_large_err,
    reason = "the cold terminal retains exact semantic, storage, generation, and journal causes"
)]
pub(crate) fn prepare_semantic_bytes(
    compiled: &[CompiledFragment<'_>],
    image_plan: &[SemanticImageRegion],
    semantic_bytes: &[u8],
    control: PublishControl<'_>,
    scratch: SemanticPublicationScratch<'_, '_, '_, '_, '_, '_, '_>,
) -> Result<super::types::PreparedSemanticOutput, PublishSemanticError> {
    with_prepared_semantic_encoded(
        compiled,
        image_plan,
        semantic_bytes,
        control,
        scratch.manifest_output,
        scratch.manifest_facts,
        scratch.ordinals,
        scratch.locality_output,
        scratch.binding_output,
        |_root, canonical, manifest, binding, verified| {
            let mut object_claims = Vec::new();
            object_claims
                .try_reserve_exact(_root.len())
                .map_err(PublishSemanticError::OutputAllocation)?;
            for entry in _root.closure() {
                object_claims.push(StagedSemanticObjectClaim {
                    key: entry.key,
                    parent: entry.parent,
                    object: entry.object,
                });
            }
            let mut manifest_bytes = Vec::new();
            manifest_bytes
                .try_reserve_exact(manifest.as_ref().len())
                .map_err(PublishSemanticError::OutputAllocation)?;
            manifest_bytes.extend_from_slice(manifest.as_ref());
            let mut binding_bytes = Vec::new();
            binding_bytes
                .try_reserve_exact(binding.as_ref().len())
                .map_err(PublishSemanticError::OutputAllocation)?;
            binding_bytes.extend_from_slice(binding.as_ref());
            let mut canonical_ordinals = Vec::new();
            canonical_ordinals
                .try_reserve_exact(canonical.canonical_ordinals().len())
                .map_err(PublishSemanticError::OutputAllocation)?;
            canonical_ordinals.extend_from_slice(canonical.canonical_ordinals());
            Ok(super::types::PreparedSemanticOutput {
                generation: *verified,
                manifest: **manifest,
                binding: **binding,
                manifest_bytes: manifest_bytes.into_boxed_slice(),
                binding_bytes: binding_bytes.into_boxed_slice(),
                canonical_ordinals: canonical_ordinals.into_boxed_slice(),
                object_claims: object_claims.into_boxed_slice(),
            })
        },
    )
}

#[allow(
    clippy::result_large_err,
    reason = "the durable completion terminal retains exact binding, generation, and journal facts"
)]
fn complete_publication(
    publisher: &DurablePublisher,
    artifact_directory: &Path,
    control: PublishControl<'_>,
    manifest: &crate::publication::manifest::CompilationManifestView<'_, '_>,
    binding_output: &mut [u8],
    verified: VerifiedGeneration<'_, ObjectDomain, &'_ [u8]>,
) -> Result<PublishedCompilation, PublishCompiledError> {
    let expected_generation = *verified;
    let binding =
        CompilationBindingView::write_into(expected_generation, manifest.identity, binding_output)
            .map_err(PublishCompiledError::BindingWrite)?;
    complete_publication_with_binding(
        publisher,
        artifact_directory,
        control,
        manifest,
        &binding,
        verified,
    )
}

#[allow(
    clippy::result_large_err,
    reason = "the durable completion terminal retains exact binding, generation, and journal facts"
)]
fn complete_publication_with_binding(
    publisher: &DurablePublisher,
    artifact_directory: &Path,
    control: PublishControl<'_>,
    manifest: &crate::publication::manifest::CompilationManifestView<'_, '_>,
    binding: &CompilationBindingView<'_>,
    verified: VerifiedGeneration<'_, ObjectDomain, &'_ [u8]>,
) -> Result<PublishedCompilation, PublishCompiledError> {
    let expected_generation = *verified;
    let binding_facts = **binding;
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        num::NonZeroUsize,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use backend_semantic::ir::{
        AtomInput, EntityKind, EntityRecord, FragmentView, IrBuilder, PackageLineage,
        PreparedFragment, PrimitiveType, SemanticImageView, SourceIdentity, TypeId, TypeNode,
    };
    use backend_semantic::vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage,
    };
    use backend_store::journal::{DurablePublisher, PublicationLimits, PublicationPaths};
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

    use crate::driver::{CompiledFragment, CompiledSemantic};

    use super::*;

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    fn fixture(label: &str) -> PathBuf {
        let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "semantic-publication-{label}-{}-{ordinal}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test fixture directory is created");
        path
    }

    fn semantic_compiled(output: &mut [u8]) -> CompiledSemantic<'_> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(
                b"publication-test-source",
            ),
            byte_len: 23,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"publication-test-toolchain"),
        );
        let entities = [EntityRecord {
            semantic_type: TypeId::new(0),
            name: backend_semantic::ir::AtomId::new(0),
            kind: EntityKind::Constant,
        }];
        let nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
        let atoms = [AtomInput { bytes: b"fixture" }];
        let prepared = PreparedFragment::prepare(source, recipe, &entities, &nodes, &atoms)
            .expect("fixture fragment prepares");
        let written = prepared
            .write_into(output)
            .expect("fixture fragment writes");
        let length = written.len();
        let fragment = FragmentView::validate(&output[..length]).expect("fixture fragment reopens");
        let artifact = CompiledFragment {
            source,
            recipe,
            fragment,
        };
        let lineage = PackageLineage::new("cargo", "semantic-publication-fixture")
            .expect("fixture package lineage is valid");
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance(artifact.source, artifact.recipe, lineage, "src/lib.rs")
            .expect("fixture image provenance is valid");
        CompiledSemantic {
            artifact,
            ir: builder.finish().expect("fixture IR builds"),
        }
    }

    fn publisher(root: &Path) -> DurablePublisher {
        let limits = PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
            .expect("test publication limits are valid");
        DurablePublisher::create(
            &PublicationPaths::in_directory(&root.join("journal")),
            limits,
        )
        .expect("test publisher opens")
    }

    fn image_bytes(compiled: &CompiledSemantic<'_>) -> Vec<u8> {
        let plan = backend_semantic::ir::PreparedFullSemanticImage::new(&compiled.ir)
            .expect("fixture image measures");
        let mut bytes = vec![0_u8; plan.byte_len()];
        let written = plan.encode_into(&mut bytes).expect("fixture image writes");
        assert_eq!(written, bytes.len());
        bytes
    }

    #[test]
    fn staged_bytes_match_ir_publication_and_reject_tampered_images() {
        let mut fragment_bytes = [0_u8; 512];
        let compiled = semantic_compiled(&mut fragment_bytes);
        let image = image_bytes(&compiled);
        let image_length = u32::try_from(image.len()).expect("fixture image fits durable width");
        let image_plan = [SemanticImageRegion::from_measurement(0, image_length)];
        let old_root = fixture("ir-path");
        let bytes_root = fixture("bytes-path");
        let old_publisher = publisher(&old_root);
        let bytes_publisher = publisher(&bytes_root);
        let mut old_manifest = [0_u8; 512];
        let mut old_facts = [None; 1];
        let mut old_ordinals = [0_usize; 1];
        let mut old_plan = [SemanticImageRegion::EMPTY; 1];
        let mut old_image = vec![0_u8; image.len()];
        let mut old_locality = [0_u8; 512];
        let mut old_binding = [0_u8; COMPILATION_BINDING_BYTES];
        let old = publish_semantic(
            &old_publisher,
            &old_root.join("artifacts"),
            core::slice::from_ref(&compiled),
            PublishControl::Continue,
            SemanticPublicationScratch {
                manifest_output: &mut old_manifest,
                manifest_facts: &mut old_facts,
                ordinals: &mut old_ordinals,
                semantic_image_plan: &mut old_plan,
                semantic_image_output: &mut old_image,
                locality_output: &mut old_locality,
                binding_output: &mut old_binding,
            },
        )
        .expect("IR publication succeeds");

        let mut bytes_manifest = [0_u8; 512];
        let mut bytes_facts = [None; 1];
        let mut bytes_ordinals = [0_usize; 1];
        let mut bytes_plan = [SemanticImageRegion::EMPTY; 1];
        let mut unused_image_output = [];
        let mut bytes_locality = [0_u8; 512];
        let mut bytes_binding = [0_u8; COMPILATION_BINDING_BYTES];
        let staged = publish_semantic_bytes(
            &bytes_publisher,
            &bytes_root.join("artifacts"),
            core::slice::from_ref(&compiled.artifact),
            &image_plan,
            &image,
            PublishControl::Continue,
            SemanticPublicationScratch {
                manifest_output: &mut bytes_manifest,
                manifest_facts: &mut bytes_facts,
                ordinals: &mut bytes_ordinals,
                semantic_image_plan: &mut bytes_plan,
                semantic_image_output: &mut unused_image_output,
                locality_output: &mut bytes_locality,
                binding_output: &mut bytes_binding,
            },
        )
        .expect("staged image publication succeeds");
        assert_eq!(old, staged);

        let mut tampered_image = image.clone();
        tampered_image[0] ^= 0xff;
        let mut tampered_manifest = [0_u8; 512];
        let mut tampered_facts = [None; 1];
        let mut tampered_ordinals = [0_usize; 1];
        let mut tampered_plan = [SemanticImageRegion::EMPTY; 1];
        let mut tampered_image_output = [];
        let mut tampered_locality = [0_u8; 512];
        let mut tampered_binding = [0_u8; COMPILATION_BINDING_BYTES];
        let rejected = publish_semantic_bytes(
            &bytes_publisher,
            &bytes_root.join("tampered-artifacts"),
            core::slice::from_ref(&compiled.artifact),
            &image_plan,
            &tampered_image,
            PublishControl::Continue,
            SemanticPublicationScratch {
                manifest_output: &mut tampered_manifest,
                manifest_facts: &mut tampered_facts,
                ordinals: &mut tampered_ordinals,
                semantic_image_plan: &mut tampered_plan,
                semantic_image_output: &mut tampered_image_output,
                locality_output: &mut tampered_locality,
                binding_output: &mut tampered_binding,
            },
        );
        assert!(matches!(
            rejected,
            Err(PublishSemanticError::ImageReopen { .. })
        ));

        assert!(SemanticImageView::reopen(&image).is_ok());
        bytes_publisher
            .shutdown()
            .expect("byte publisher shuts down");
        old_publisher.shutdown().expect("IR publisher shuts down");
        fs::remove_dir_all(old_root).expect("IR fixture is removed");
        fs::remove_dir_all(bytes_root).expect("byte fixture is removed");
    }
}
