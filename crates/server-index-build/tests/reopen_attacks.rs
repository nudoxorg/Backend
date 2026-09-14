//! Exercises the `server-index-build` tests reopen-attacks contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Immutable reopened-byte corruption attacks before the index builder boundary.

mod support;

use std::fs;

use backend_semantic::ir::{AtomId, TypeId};
use backend_semantic::ir::{
    AtomInput, EntityKind, EntityRecord, FragmentRangeManifest, FragmentView, PrimitiveType,
    TypeNode,
};
use backend_engine::publication::{
    OpenPublicationScratch, OpenPublishedError, immutable::ImmutableArtifactStore, open_published,
};
use support::{
    BuildProofError, Fixture, OpenBuffers, TestError, compiled, publish, write_fragment, written,
};

#[test]
fn truncated_or_substituted_immutable_bytes_fail_before_the_builder_boundary()
-> Result<(), TestError> {
    let fixture = Fixture::new("reopen-attacks")?;
    let publisher = fixture.publisher()?;
    let mut selected_bytes = [0_u8; 512];
    let mut replacement_bytes = [0_u8; 512];
    let selected_length = write_fragment(
        &mut selected_bytes,
        b"selected-source",
        &[entity()],
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &[AtomInput { bytes: b"selected" }],
    )?;
    let replacement_length = write_fragment(
        &mut replacement_bytes,
        b"replacement-source",
        &[entity()],
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &[AtomInput {
            bytes: b"replacement",
        }],
    )?;
    let selected = compiled(written(&selected_bytes, selected_length)?)?;
    let range = FragmentRangeManifest::from_view(&selected.fragment)?;
    publish(&publisher, &fixture.artifacts(), &[selected])?;
    let replacement = FragmentView::validate(written(&selected_bytes, selected_length)?)?;
    let mut artifacts = ImmutableArtifactStore::new(&fixture.artifacts())?;
    let stored = artifacts.ensure(&range, &replacement)?;
    fs::write(&stored.path, b"truncated")?;
    reject_reopen(
        &publisher,
        &fixture.artifacts(),
        BuildProofError::TruncatedArtifactAccepted,
    )?;
    fs::write(
        &stored.path,
        written(&replacement_bytes, replacement_length)?,
    )?;
    reject_reopen(
        &publisher,
        &fixture.artifacts(),
        BuildProofError::SubstitutedArtifactAccepted,
    )?;
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

const fn entity() -> EntityRecord {
    EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    }
}

fn reject_reopen(
    publisher: &backend_store::journal::DurablePublisher,
    artifacts: &std::path::Path,
    accepted: BuildProofError,
) -> Result<(), TestError> {
    let mut buffers = OpenBuffers::new();
    match open_published(
        publisher,
        artifacts,
        OpenPublicationScratch {
            manifest_output: &mut buffers.manifest,
            manifest_facts: &mut buffers.facts,
            fragment_output: &mut buffers.fragments,
            locality_output: &mut buffers.locality,
        },
    ) {
        Err(OpenPublishedError::Fragment { ordinal: 0, .. }) => Ok(()),
        Ok(_) => Err(TestError::BuildProof(accepted)),
        Err(cause) => Err(TestError::BuildProof(
            BuildProofError::UnexpectedReopenTerminal {
                cause: Box::new(cause),
            },
        )),
    }
}
