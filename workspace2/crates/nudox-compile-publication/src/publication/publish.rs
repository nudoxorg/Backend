use std::path::Path;

use nudox_compile_driver::CompiledFragment;
use nudox_durable_journal::{CancelError, DurablePublisher, PublicationError, SubmitError};
use nudox_ir_format::FragmentRangeManifest;

use super::types::{
    PublicationScratch, PublishCompiledError, PublishControl, PublishedCompilation,
    UncommittedPublication, UncommittedPublicationFacts,
};
use crate::{
    binding::{COMPILATION_BINDING_BYTES, CompilationBindingView},
    binding_store::GenerationBindingStore,
    generation::with_verified_generation,
    immutable::ImmutableArtifactStore,
    manifest::CanonicalCompilation,
    manifest_store::ImmutableManifestStore,
};

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
            manifest: *manifest,
            binding: binding_facts,
        })
    })
    .map_err(PublishCompiledError::Generation)?
}
