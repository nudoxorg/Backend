//! Measures `backend-store` benches capacity-planning runner publication work with production data paths.
//! Measurements separate setup from steady-state work and retain resource counters.
//! Results support capacity decisions without changing the measured implementation.
//! Durable compiler publication and deterministic compact-IR index derivation phases.

use core::mem::{MaybeUninit, size_of_val};
use std::{array, num::NonZeroUsize};

use backend_engine::driver::CompiledFragment;
use backend_engine::publication::{
    PublicationScratch, PublishControl, binding::COMPILATION_BINDING_BYTES, publish_compiled,
};
use server_index_build::{EntityFact, EntityProjection, build};
use backend_semantic::index_core::{ExactRow, LexicalRow};
use backend_store::journal::{DurablePublisher, PublicationLimits};

use crate::{
    BenchmarkError,
    measure::StageWork,
    model::MAX_CORPUS,
    runner::{failure::build_failure_fact, fixture::Fixture, support::directory_bytes},
};

const MANIFEST_BYTES: usize = 65_536;
const LOCALITY_BYTES: usize = 16_384;

#[allow(
    clippy::large_stack_arrays,
    reason = "the public publication API takes caller-owned bounded buffers; moving them to the heap would hide required working-set capacity"
)]
pub(crate) fn deterministic_build(fixture: &Fixture) -> Result<StageWork, BenchmarkError> {
    use backend_engine::publication::{OpenPublicationScratch, open_published};

    let publisher = DurablePublisher::reopen(
        &fixture.journal(),
        PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
            .map_err(|source| BenchmarkError::PublicationLimits(Box::new(source)))?,
    )
    .map_err(|source| BenchmarkError::PublicationOpen(Box::new(source)))?;
    let mut manifest = [0_u8; MANIFEST_BYTES];
    let mut manifest_facts = [None; MAX_CORPUS];
    let mut fragment_output = [0_u8; MANIFEST_BYTES];
    let mut locality = [0_u8; LOCALITY_BYTES];
    let opened = open_published(
        &publisher,
        &fixture.artifacts(),
        OpenPublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut manifest_facts,
            fragment_output: &mut fragment_output,
            locality_output: &mut locality,
        },
    )
    .map_err(|source| BenchmarkError::OpenPublished(Box::new(source)))?
    .ok_or(BenchmarkError::MissingPublishedCompilation)?;
    let operation = build_opened_fragments(&opened);
    let shutdown = publisher.shutdown();
    match (operation, shutdown) {
        (Ok(work), Ok(())) => Ok(work),
        (Err(operation), Ok(())) => Err(operation),
        (Ok(_), Err(shutdown)) => Err(BenchmarkError::PublicationShutdown(Box::new(shutdown))),
        (Err(operation), Err(_shutdown)) => Err(operation),
    }
}

fn build_opened_fragments(
    opened: &backend_engine::publication::OpenedCompilation<'_, '_>,
) -> Result<StageWork, BenchmarkError> {
    let mut input_bytes = 0_u64;
    let mut output_bytes = 0_u64;
    let mut items = 0_usize;
    for opened_fragment in opened.fragments() {
        let opened_fragment =
            opened_fragment.map_err(|source| BenchmarkError::OpenedFragment(Box::new(source)))?;
        let (exact_bytes, lexical_bytes) = build_opened_fragment(&opened_fragment)?;
        input_bytes = input_bytes
            .checked_add(
                u64::try_from(opened_fragment.view.as_ref().len())
                    .map_err(BenchmarkError::ByteCount)?,
            )
            .ok_or(BenchmarkError::ByteCountOverflow)?;
        let bytes = exact_bytes
            .checked_add(lexical_bytes)
            .ok_or(BenchmarkError::ByteCountOverflow)?;
        output_bytes = output_bytes
            .checked_add(u64::try_from(bytes).map_err(BenchmarkError::ByteCount)?)
            .ok_or(BenchmarkError::ByteCountOverflow)?;
        items = items
            .checked_add(1)
            .ok_or(BenchmarkError::ItemCountOverflow)?;
    }
    Ok(StageWork {
        input_items: items,
        output_items: items
            .checked_mul(2)
            .ok_or(BenchmarkError::ItemCountOverflow)?,
        bytes_read: input_bytes,
        bytes_written: output_bytes,
        durable_bytes: 0,
    })
}

fn build_opened_fragment(
    fragment: &backend_engine::publication::OpenedFragment<'_>,
) -> Result<(usize, usize), BenchmarkError> {
    let mut projections: [MaybeUninit<EntityProjection<'_>>; MAX_CORPUS] =
        array::from_fn(|_| MaybeUninit::uninit());
    let mut entities: [MaybeUninit<EntityFact<'_>>; MAX_CORPUS] =
        array::from_fn(|_| MaybeUninit::uninit());
    let mut exact_rows: [MaybeUninit<ExactRow<'_>>; MAX_CORPUS] =
        array::from_fn(|_| MaybeUninit::uninit());
    let mut lexical_rows: [MaybeUninit<LexicalRow<'_>>; MAX_CORPUS] =
        array::from_fn(|_| MaybeUninit::uninit());
    let mut atoms: [MaybeUninit<backend_semantic::ir::Atom<'_>>; MAX_CORPUS] =
        array::from_fn(|_| MaybeUninit::uninit());
    let mut type_nodes: [MaybeUninit<backend_semantic::ir::TypeNode>; MAX_CORPUS] =
        array::from_fn(|_| MaybeUninit::uninit());
    let built = build(
        fragment,
        server_index_build::IndexBuildScratch {
            projections: &mut projections,
            entities: &mut entities,
            exact_rows: &mut exact_rows,
            lexical_rows: &mut lexical_rows,
            atoms: &mut atoms,
            type_nodes: &mut type_nodes,
        },
    )
    .map_err(|cause| BenchmarkError::IndexBuild {
        cause: Box::new(build_failure_fact(cause)),
    })?;
    let exact_bytes = size_of_val(built.exact.rows);
    let lexical_bytes = built
        .lexical
        .rows
        .iter()
        .try_fold(0_usize, |total, fact| total.checked_add(fact.term.len()))
        .ok_or(BenchmarkError::ByteCountOverflow)?;
    Ok((exact_bytes, lexical_bytes))
}

#[allow(
    clippy::large_stack_arrays,
    reason = "the public publication API takes caller-owned bounded buffers; moving them to the heap would hide required working-set capacity"
)]
pub(crate) fn durable_publish(
    compiled: &[CompiledFragment<'_>],
    fixture: &Fixture,
) -> Result<StageWork, BenchmarkError> {
    let publisher = DurablePublisher::create(
        &fixture.journal(),
        PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
            .map_err(|source| BenchmarkError::PublicationLimits(Box::new(source)))?,
    )
    .map_err(|source| BenchmarkError::PublicationOpen(Box::new(source)))?;
    let mut manifest = [0_u8; MANIFEST_BYTES];
    let mut facts = [None; MAX_CORPUS];
    let mut ordinals = [0_usize; MAX_CORPUS];
    let mut locality = [0_u8; LOCALITY_BYTES];
    let mut binding = [0_u8; COMPILATION_BINDING_BYTES];
    let publication = publish_compiled(
        &publisher,
        &fixture.artifacts(),
        compiled,
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    );
    let shutdown = publisher.shutdown();
    match (publication, shutdown) {
        (Ok(publication), Ok(())) => {
            let fragment_bytes = compiled.iter().try_fold(0_u64, |total, fragment| {
                let width = u64::try_from(fragment.fragment.as_ref().len())
                    .map_err(BenchmarkError::ByteCount)?;
                total
                    .checked_add(width)
                    .ok_or(BenchmarkError::ByteCountOverflow)
            })?;
            let durable_bytes = durable_publication_bytes(fixture)?;
            Ok(StageWork {
                input_items: compiled.len(),
                output_items: usize::try_from(publication.manifest.fragment_count)
                    .map_err(BenchmarkError::ByteCount)?,
                bytes_read: fragment_bytes,
                bytes_written: durable_bytes,
                durable_bytes,
            })
        }
        (Err(publish), Ok(())) => Err(BenchmarkError::Publish(Box::new(publish))),
        (Ok(_), Err(shutdown)) => Err(BenchmarkError::PublicationShutdown(Box::new(shutdown))),
        (Err(publish), Err(shutdown)) => Err(BenchmarkError::PublishAndShutdown {
            publish: Box::new(publish),
            shutdown: Box::new(shutdown),
        }),
    }
}

fn durable_publication_bytes(fixture: &Fixture) -> Result<u64, BenchmarkError> {
    let artifact_bytes = directory_bytes(&fixture.artifacts())?;
    let journal_bytes = directory_bytes(&fixture.root.join("journal"))?;
    artifact_bytes
        .checked_add(journal_bytes)
        .ok_or(BenchmarkError::ByteCountOverflow)
}
