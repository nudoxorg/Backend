//! Capacity stages for the canonical semantic IR and its direct adapters.

use core::{
    fmt,
    fmt::Write as _,
    mem::{size_of, size_of_val},
};

use compiler_ir::{
    Confidence, Diff, EntityVersion, FrontendTree, GenerationId, Ir, IrBuilder, ItemKind, LinkId,
    LinkKind, LinkTarget, PayloadHash, Snapshot, StableEntityId, TreeEntityId, TreeItemInput,
    TreeLinkInput, TreeLinkTarget, Visibility,
};
use server_index_graph_vector::{
    IrVectorColumn, Metric, ModelId, PartitionId, VectorAuthority, exact_ir_vector_query,
};
use server_index_trustfall::IrTrustfallGraph;

use crate::{BenchmarkError, measure::StageWork, model::MAX_CORPUS, runner::fixture::Corpus};

const VECTOR_DIMENSION: usize = 4;
const VECTOR_TOP_K: usize = 4;
const EMPTY_VERSION: EntityVersion = EntityVersion {
    stable: StableEntityId::from_raw([0; 16]),
    payload: PayloadHash::from_raw([0; 16]),
};

/// Existing compiler-owned input used by the monomorphized frontend adapter.
pub(crate) struct SemanticCorpus<'corpus> {
    corpus: &'corpus Corpus,
    versions: [EntityVersion; MAX_CORPUS],
}

impl<'corpus> SemanticCorpus<'corpus> {
    pub(crate) fn new(corpus: &'corpus Corpus) -> Self {
        let mut versions = [EMPTY_VERSION; MAX_CORPUS];
        for (index, version) in versions.iter_mut().take(corpus.len).enumerate() {
            let bytes = index.to_le_bytes();
            *version = EntityVersion {
                stable: StableEntityId::from_canonical_bytes(&bytes),
                payload: PayloadHash::from_canonical_bytes(&bytes),
            };
        }
        Self { corpus, versions }
    }
}

impl FrontendTree for SemanticCorpus<'_> {
    fn versions(&self) -> &[EntityVersion] {
        self.versions.get(..self.corpus.len).unwrap_or(&[])
    }

    fn items(&self) -> impl ExactSizeIterator<Item = TreeItemInput<'_>> {
        (0..self.corpus.len).map(|index| TreeItemInput {
            name: self.corpus.name(index).unwrap_or(b"?"),
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            typescript: None,
        })
    }

    fn links(&self) -> impl ExactSizeIterator<Item = TreeLinkInput> {
        (1..self.corpus.len).map(|index| TreeLinkInput {
            from: TreeEntityId::new(u32::try_from(index - 1).unwrap_or(0)),
            target: TreeLinkTarget::Local(TreeEntityId::new(
                u32::try_from(index).unwrap_or(u32::MAX),
            )),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            source: None,
        })
    }
}

pub(crate) fn build(source: &SemanticCorpus<'_>) -> Result<(Ir, StageWork), BenchmarkError> {
    let bytes_read = source.corpus.byte_len()?;
    let mut builder = IrBuilder::new();
    builder
        .add_frontend_tree(source)
        .map_err(BenchmarkError::SemanticIr)?;
    let ir = builder.finish().map_err(BenchmarkError::SemanticIr)?;
    let bytes_written =
        u64::try_from(ir.storage_columns().resident_bytes()).map_err(BenchmarkError::ByteCount)?;
    let work = StageWork {
        input_items: source.corpus.len,
        output_items: ir.entity_count(),
        bytes_read,
        bytes_written,
        durable_bytes: 0,
    };
    Ok((ir, work))
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl fmt::Write for CountingWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.bytes = self.bytes.checked_add(value.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}

pub(crate) fn render(ir: &Ir) -> Result<StageWork, BenchmarkError> {
    let mut output = CountingWriter::default();
    for item in ir.items() {
        if let Some(signature) = ir.signature(item.id()) {
            write!(output, "{signature}").map_err(|_| BenchmarkError::SemanticRender)?;
        }
        if let Some(embedding) =
            ir.embedding_text(item.id(), compiler_ir::EmbeddingProfile::CONTEXTUAL)
        {
            write!(output, "{embedding}").map_err(|_| BenchmarkError::SemanticRender)?;
        }
    }
    Ok(StageWork {
        input_items: ir.entity_count(),
        output_items: ir.entity_count().saturating_mul(2),
        bytes_read: u64::try_from(ir.storage_columns().resident_bytes())
            .map_err(BenchmarkError::ByteCount)?,
        bytes_written: u64::try_from(output.bytes).map_err(BenchmarkError::ByteCount)?,
        durable_bytes: 0,
    })
}

pub(crate) fn vcs(ir: &Ir) -> Result<StageWork, BenchmarkError> {
    let snapshot = Snapshot {
        generation: GenerationId::from_canonical_bytes(b"capacity-semantic-generation"),
        ir,
    };
    let entities = Diff::between(snapshot, snapshot).entities.count();
    let links = Diff::between(snapshot, snapshot).links.count();
    Ok(StageWork {
        input_items: ir.entity_count(),
        output_items: entities.saturating_add(links),
        bytes_read: u64::try_from(ir.storage_columns().resident_bytes())
            .map_err(BenchmarkError::ByteCount)?,
        bytes_written: 0,
        durable_bytes: 0,
    })
}

pub(crate) fn trustfall(ir: &Ir) -> Result<StageWork, BenchmarkError> {
    let graph = IrTrustfallGraph::new(ir);
    let mut output = [None; MAX_CORPUS];
    let mut written = 0_usize;
    for index in 0..ir.entity_count() {
        written = written.saturating_add(
            graph
                .neighbors(
                    compiler_ir::EntityId::new(
                        u32::try_from(index).map_err(BenchmarkError::ByteCount)?,
                    ),
                    &mut output,
                )
                .map_err(BenchmarkError::TrustfallIr)?,
        );
    }
    let columns = ir.graph_columns();
    let link_bytes = columns
        .from
        .len()
        .checked_mul(
            size_of::<LinkTarget>()
                + size_of::<LinkKind>()
                + size_of::<Confidence>()
                + size_of::<u32>()
                + size_of::<LinkId>(),
        )
        .and_then(|bytes| bytes.checked_add(size_of_val(columns.outgoing_offsets)))
        .ok_or(BenchmarkError::ByteCountOverflow)?;
    Ok(StageWork {
        input_items: ir.entity_count(),
        output_items: written,
        bytes_read: u64::try_from(link_bytes).map_err(BenchmarkError::ByteCount)?,
        bytes_written: u64::try_from(written)
            .map_err(BenchmarkError::ByteCount)?
            .checked_mul(
                u64::try_from(size_of::<server_index_trustfall::IrTrustfallHit>())
                    .map_err(BenchmarkError::ByteCount)?,
            )
            .ok_or(BenchmarkError::ByteCountOverflow)?,
        durable_bytes: 0,
    })
}

pub(crate) fn vector(ir: &Ir) -> Result<StageWork, BenchmarkError> {
    let count = ir.entity_count();
    let authority = VectorAuthority::new(
        server_index_core::IndexSnapshotId::from_canonical_bytes(b"semantic-vector-snapshot"),
        ModelId::new([0x81; 16]),
        u16::try_from(VECTOR_DIMENSION).map_err(BenchmarkError::ByteCount)?,
        Metric::SquaredEuclidean,
    );
    let mut ordinals = [u32::MAX; MAX_CORPUS];
    let mut coordinates = [0_i16; MAX_CORPUS * VECTOR_DIMENSION];
    let coordinate_capacity = coordinates.len();
    for (index, ordinal) in ordinals.iter_mut().take(count).enumerate() {
        *ordinal = u32::try_from(index).map_err(BenchmarkError::ByteCount)?;
        let value = i16::try_from(index).map_err(BenchmarkError::ByteCount)?;
        let start = index * VECTOR_DIMENSION;
        let end = start.saturating_add(VECTOR_DIMENSION);
        let destination =
            coordinates
                .get_mut(start..end)
                .ok_or(BenchmarkError::FixedSliceLength {
                    observed: end,
                    capacity: coordinate_capacity,
                })?;
        destination.copy_from_slice(&[value, value, 2, -2]);
    }
    let ordinals = ordinals
        .get(..count)
        .ok_or(BenchmarkError::FixedSliceLength {
            observed: count,
            capacity: ordinals.len(),
        })?;
    let coordinate_count = count.saturating_mul(VECTOR_DIMENSION);
    let coordinates =
        coordinates
            .get(..coordinate_count)
            .ok_or(BenchmarkError::FixedSliceLength {
                observed: coordinate_count,
                capacity: coordinate_capacity,
            })?;
    let column = IrVectorColumn::try_new(ir, authority, PartitionId::new(0), ordinals, coordinates)
        .map_err(BenchmarkError::IrVectorColumn)?;
    let mut output = [None; VECTOR_TOP_K];
    let result =
        exact_ir_vector_query(column, &[0, 0, 2, -2], VECTOR_TOP_K.min(count), &mut output)
            .map_err(|cause| BenchmarkError::VectorQuery {
                cause: Box::new(cause),
            })?;
    Ok(StageWork {
        input_items: count,
        output_items: result.written,
        bytes_read: u64::try_from(count * VECTOR_DIMENSION * size_of::<i16>())
            .map_err(BenchmarkError::ByteCount)?,
        bytes_written: u64::try_from(
            result.written * size_of::<server_index_graph_vector::VectorHit>(),
        )
        .map_err(BenchmarkError::ByteCount)?,
        durable_bytes: 0,
    })
}
