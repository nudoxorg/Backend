//! Exercises the `server-index-build` tests build contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Public compiler-publication to existing-core index journeys and canonicality attacks.

mod support;

use allocation_counter::{AllocationInfo, measure};
use backend_semantic::ir::{AtomId, TypeId};
use backend_semantic::ir::{AtomInput, EntityKind, EntityRecord, PrimitiveType, TypeNode};
use backend_version::{ContentId, SourceFactDomain};
use server_index_core::{ExactOperation, IndexSnapshot, LexicalRow};
use support::{
    BuildBuffers, BuildProofError, Fixture, OpenBuffers, TestError, compiled, next_fragment,
    publish, write_fragment, written,
};

#[test]
fn reopened_fragments_produce_existing_core_rows_without_warm_allocation() -> Result<(), TestError>
{
    let fixture = Fixture::new("reopened")?;
    let publisher = fixture.publisher()?;
    let mut alpha = [0_u8; 512];
    let mut beta = [0_u8; 512];
    let alpha_length = write_fragment(
        &mut alpha,
        b"source-alpha",
        &[entity(EntityKind::Function, 0, 0)],
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &[AtomInput { bytes: b"alpha" }],
    )?;
    let beta_length = write_fragment(
        &mut beta,
        b"source-beta",
        &[entity(EntityKind::Record, 0, 0)],
        &[TypeNode::Primitive(PrimitiveType::String)],
        &[AtomInput { bytes: b"beta" }],
    )?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[
            compiled(written(&beta, beta_length)?)?,
            compiled(written(&alpha, alpha_length)?)?,
        ],
    )?;
    let mut reopened = OpenBuffers::new();
    let opened = reopened.open(&publisher, &fixture.artifacts())?;
    let mut fragments = opened.fragments();
    let first = next_fragment(&mut fragments, 0)?;
    let second = next_fragment(&mut fragments, 1)?;
    let mut first_output = BuildBuffers::new();
    let first_index = first_output.build(&first)?;
    let mut second_output = BuildBuffers::new();
    let second_index = second_output.build(&second)?;
    verify_rows(&first_index)?;
    verify_rows(&second_index)?;
    if first_index.exact.id == second_index.exact.id
        || first_index.lexical.id == second_index.lexical.id
    {
        return Err(TestError::BuildProof(BuildProofError::ReusedIdentity));
    }
    assert_warm_build_does_not_allocate(&first)?;
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
fn raw_fragment_authority_preserves_order_and_type_coordinates() -> Result<(), TestError> {
    let atoms = [AtomInput { bytes: b"alpha" }, AtomInput { bytes: b"beta" }];
    let types = [TypeNode::Primitive(PrimitiveType::Bool)];
    let ordered = ids_for(
        "ordered",
        &[
            entity(EntityKind::Function, 0, 0),
            entity(EntityKind::Constant, 0, 1),
        ],
        &types,
        &atoms,
    )?;
    let reordered = ids_for(
        "reordered",
        &[
            entity(EntityKind::Constant, 0, 1),
            entity(EntityKind::Function, 0, 0),
        ],
        &types,
        &atoms,
    )?;
    if ordered == reordered {
        return Err(TestError::BuildProof(BuildProofError::SourceOrderChanged));
    }
    let first_reference = ids_for(
        "reference-first",
        &[entity(EntityKind::Record, 0, 0)],
        &[
            TypeNode::Reference(TypeId::new(1)),
            TypeNode::Primitive(PrimitiveType::Bool),
        ],
        &[AtomInput { bytes: b"typed" }],
    )?;
    let permuted_reference = ids_for(
        "reference-permuted",
        &[entity(EntityKind::Record, 1, 0)],
        &[
            TypeNode::Primitive(PrimitiveType::Bool),
            TypeNode::Reference(TypeId::new(0)),
        ],
        &[AtomInput { bytes: b"typed" }],
    )?;
    if first_reference == permuted_reference {
        return Err(TestError::BuildProof(
            BuildProofError::TypeCoordinatesCollapsed,
        ));
    }
    Ok(())
}

#[test]
fn manifest_bound_documents_keep_equal_fragments_addressable_without_change_spillover()
-> Result<(), TestError> {
    let baseline = paired_witnesses("paired-baseline", b"shared")?;
    let changed = paired_witnesses("paired-changed", b"changed")?;
    if baseline.alpha.key == baseline.beta.key || baseline.alpha.exact == baseline.beta.exact {
        return Err(TestError::BuildProof(
            BuildProofError::FragmentNamespaceCollapsed,
        ));
    }
    if baseline.alpha.document == baseline.beta.document
        || baseline.alpha.lexical == baseline.beta.lexical
    {
        return Err(TestError::BuildProof(
            BuildProofError::LexicalAuthorityCollapsed,
        ));
    }
    if baseline.alpha != changed.alpha || baseline.beta == changed.beta {
        return Err(TestError::BuildProof(
            BuildProofError::AtomChangeEscapedFragment,
        ));
    }
    Ok(())
}

const fn entity(kind: EntityKind, semantic_type: u32, name: u32) -> EntityRecord {
    EntityRecord {
        semantic_type: TypeId::new(semantic_type),
        name: AtomId::new(name),
        kind,
    }
}

fn verify_rows(index: &server_index_build::PreparedIndex<'_>) -> Result<(), TestError> {
    for ((fact, exact), lexical) in index
        .entities
        .iter()
        .zip(index.exact.rows)
        .zip(index.lexical.rows)
    {
        let document = server_index_core::EntityDocumentId {
            artifact: server_index_core::EntityArtifactIdentity::Compact(index.fragment.fragment),
            entity: fact.entity,
        };
        let document_bytes: [u8; server_index_core::ENTITY_DOCUMENT_ID_BYTES] = document.into();
        if exact.key != document_bytes
            || exact.key != fact.exact_key.as_ref()
            || exact.value_bytes().is_none()
            || lexical != &LexicalRow::new(fact.name, document, 1_u32.into())
        {
            return Err(TestError::BuildProof(BuildProofError::EntityRowMismatch));
        }
        let Some(found) = index
            .exact
            .lookup(ExactOperation::new(fact.exact_key.as_ref()))
        else {
            return Err(TestError::BuildProof(
                BuildProofError::ExactEntityLookupMismatch,
            ));
        };
        let Some(value_bytes) = found.value_bytes() else {
            return Err(TestError::BuildProof(
                BuildProofError::ExactEntityLookupMismatch,
            ));
        };
        let value = server_index_build::ExactEntityValue::try_from(value_bytes)?;
        if found.value_bytes() != exact.value_bytes() || value != fact.exact_value {
            return Err(TestError::BuildProof(
                BuildProofError::ExactEntityLookupMismatch,
            ));
        }
    }
    Ok(())
}

fn ids_for(
    label: &str,
    entities: &[EntityRecord],
    types: &[TypeNode],
    atoms: &[AtomInput<'_>],
) -> Result<
    (
        server_index_core::ExactSegmentId,
        server_index_core::LexicalSegmentId,
    ),
    TestError,
> {
    let fixture = Fixture::new(label)?;
    let publisher = fixture.publisher()?;
    let mut bytes = [0_u8; 512];
    let length = write_fragment(&mut bytes, b"same-source", entities, types, atoms)?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(written(&bytes, length)?)?],
    )?;
    let mut reopened = OpenBuffers::new();
    let opened = reopened.open(&publisher, &fixture.artifacts())?;
    let fragment = next_fragment(&mut opened.fragments(), 0)?;
    let mut output = BuildBuffers::new();
    let index = output.build(&fragment)?;
    let identities = (index.exact.id, index.lexical.id);
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(identities)
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct SegmentWitness {
    exact: server_index_core::ExactSegmentId,
    lexical: server_index_core::LexicalSegmentId,
    key: server_index_build::ExactEntityKey,
    document: server_index_core::EntityDocumentId,
}

#[derive(Clone, Copy)]
struct PairedWitnesses {
    alpha: SegmentWitness,
    beta: SegmentWitness,
}

fn paired_witnesses(label: &str, beta_atom: &[u8]) -> Result<PairedWitnesses, TestError> {
    const ALPHA_SOURCE: &[u8] = b"same-declaration-alpha";
    const BETA_SOURCE: &[u8] = b"same-declaration-beta";

    let fixture = Fixture::new(label)?;
    let publisher = fixture.publisher()?;
    let mut alpha_bytes = [0_u8; 512];
    let mut beta_bytes = [0_u8; 512];
    let alpha_length = write_fragment(
        &mut alpha_bytes,
        ALPHA_SOURCE,
        &[entity(EntityKind::Function, 0, 0)],
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &[AtomInput { bytes: b"shared" }],
    )?;
    let beta_length = write_fragment(
        &mut beta_bytes,
        BETA_SOURCE,
        &[entity(EntityKind::Function, 0, 0)],
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &[AtomInput { bytes: beta_atom }],
    )?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[
            compiled(written(&alpha_bytes, alpha_length)?)?,
            compiled(written(&beta_bytes, beta_length)?)?,
        ],
    )?;
    let mut reopened = OpenBuffers::new();
    let opened = reopened.open(&publisher, &fixture.artifacts())?;
    let mut fragments = opened.fragments();
    let first = next_fragment(&mut fragments, 0)?;
    let second = next_fragment(&mut fragments, 1)?;
    let mut first_output = BuildBuffers::new();
    let first = first_output.build(&first)?;
    let mut second_output = BuildBuffers::new();
    let second = second_output.build(&second)?;
    let alpha_source = ContentId::<SourceFactDomain>::from_canonical_bytes(ALPHA_SOURCE);
    let beta_source = ContentId::<SourceFactDomain>::from_canonical_bytes(BETA_SOURCE);
    let first_witness = witness(&first)?;
    let second_witness = witness(&second)?;
    let exact = [first.exact.id, second.exact.id];
    let lexical = [first.lexical.id, second.lexical.id];
    IndexSnapshot::new(opened.publication.generation.pinned_root, &exact, &lexical)
        .map_err(|cause| TestError::BuildProof(BuildProofError::SnapshotRejected { cause }))?;
    let paired = label_witnesses(
        &first,
        &second,
        first_witness,
        second_witness,
        alpha_source,
        beta_source,
    )?;
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(paired)
}

fn label_witnesses(
    first: &server_index_build::PreparedIndex<'_>,
    second: &server_index_build::PreparedIndex<'_>,
    first_witness: SegmentWitness,
    second_witness: SegmentWitness,
    alpha_source: ContentId<SourceFactDomain>,
    beta_source: ContentId<SourceFactDomain>,
) -> Result<PairedWitnesses, TestError> {
    if first.fragment.source.identity == alpha_source
        && second.fragment.source.identity == beta_source
    {
        return Ok(PairedWitnesses {
            alpha: first_witness,
            beta: second_witness,
        });
    }
    if first.fragment.source.identity == beta_source
        && second.fragment.source.identity == alpha_source
    {
        return Ok(PairedWitnesses {
            alpha: second_witness,
            beta: first_witness,
        });
    }
    Err(TestError::BuildProof(
        BuildProofError::FragmentNamespaceCollapsed,
    ))
}

fn assert_warm_build_does_not_allocate(
    fragment: &compiler_publication::OpenedFragment<'_>,
) -> Result<(), TestError> {
    let mut warmup = BuildBuffers::new();
    core::hint::black_box(warmup.build(fragment)?);
    let mut output = BuildBuffers::new();
    let mut built = None;
    let allocation = measure(|| built = Some(output.build(fragment).map(|index| index.exact.id)));
    match built {
        Some(Ok(_)) if allocation == AllocationInfo::default() => Ok(()),
        Some(Ok(_)) => Err(TestError::BuildProof(BuildProofError::WarmAllocation)),
        Some(Err(error)) => Err(error),
        None => Err(TestError::BuildProof(BuildProofError::MeasurementSkipped)),
    }
}

fn witness(index: &server_index_build::PreparedIndex<'_>) -> Result<SegmentWitness, TestError> {
    let Some(entity) = index.entities.first() else {
        return Err(TestError::BuildProof(BuildProofError::MissingIndexedEntity));
    };
    let Some(lexical) = index.lexical.rows.first() else {
        return Err(TestError::BuildProof(BuildProofError::MissingIndexedEntity));
    };
    Ok(SegmentWitness {
        exact: index.exact.id,
        lexical: index.lexical.id,
        key: entity.exact_key,
        document: lexical.document,
    })
}
