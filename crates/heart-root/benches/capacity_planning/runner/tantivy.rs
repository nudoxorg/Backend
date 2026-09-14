//! Measures `heart-root` benches capacity-planning runner tantivy work with production data paths.
//! Measurements separate setup from steady-state work and retain resource counters.
//! Results support capacity decisions without changing the measured implementation.
//! Lexical row derivation and Tantivy adapter build/query phases.

use backend_semantic::ir::EntityId;
use backend_version::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};
use backend_semantic::index_core::{
    EntityArtifactIdentity, EntityDocumentId, GenerationId, IndexSnapshot, LexicalManifest,
    LexicalRow, LexicalScore, LexicalSegment,
};
use backend_extension_tantivy::server::{TantivyLexical, TantivyTerminal};

use crate::{
    BenchmarkError,
    measure::StageWork,
    model::MAX_CORPUS,
    runner::{
        failure::lexical_segment_fault,
        fixture::{Corpus, FragmentSlots},
        support::fixed_prefix,
    },
};

const TANTIVY_TOP_K: usize = 4;

fn lexical_rows<'source>(
    corpus: &'source Corpus,
    fragments: &FragmentSlots,
) -> Result<[LexicalRow<'source>; MAX_CORPUS], BenchmarkError> {
    let placeholder = lexical_document(fragments, 0)?;
    let mut rows = [LexicalRow::new(b"", placeholder, LexicalScore::from(0)); MAX_CORPUS];
    for (index, row) in rows.iter_mut().take(corpus.len).enumerate() {
        let document = lexical_document(fragments, index)?;
        *row = LexicalRow::new(corpus.name(index)?, document, LexicalScore::from(1));
    }
    Ok(rows)
}

fn lexical_document(
    fragments: &FragmentSlots,
    index: usize,
) -> Result<EntityDocumentId, BenchmarkError> {
    let bytes = fragments.bytes(index)?;
    Ok(EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(bytes),
        ),
        entity: EntityId::new(0),
    })
}

pub(crate) fn tantivy_build(
    corpus: &Corpus,
    fragments: &FragmentSlots,
) -> Result<StageWork, BenchmarkError> {
    let rows = lexical_rows(corpus, fragments)?;
    let count = corpus.len;
    let rows = fixed_prefix(&rows, count)?;
    let segment = LexicalSegment::new(rows).map_err(|cause| BenchmarkError::LexicalSegment {
        cause: Box::new(lexical_segment_fault(cause)),
    })?;
    let selected = [segment.id];
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"capacity-lexical-generation"),
        &[],
        &selected,
    )
    .map_err(BenchmarkError::IndexSnapshot)?;
    let segments = [segment];
    let manifest =
        LexicalManifest::new(snapshot, &segments, &[]).map_err(BenchmarkError::LexicalManifest)?;
    let _adapter =
        std::hint::black_box(TantivyLexical::build(manifest).map_err(BenchmarkError::Tantivy)?);
    let bytes = u64::try_from(count)
        .map_err(BenchmarkError::ByteCount)?
        .checked_mul(u64::try_from(corpus.name(0)?.len()).map_err(BenchmarkError::ByteCount)?)
        .ok_or(BenchmarkError::ByteCountOverflow)?;
    let row_bytes = u64::try_from(size_of_val(rows)).map_err(BenchmarkError::ByteCount)?;
    let bytes_written = bytes
        .checked_add(row_bytes)
        .ok_or(BenchmarkError::ByteCountOverflow)?;
    Ok(StageWork {
        input_items: count,
        output_items: count,
        bytes_read: bytes,
        bytes_written,
        durable_bytes: 0,
    })
}

pub(crate) fn tantivy_query(
    corpus: &Corpus,
    fragments: &FragmentSlots,
) -> Result<StageWork, BenchmarkError> {
    let rows = lexical_rows(corpus, fragments)?;
    let count = corpus.len;
    let rows = fixed_prefix(&rows, count)?;
    let segment = LexicalSegment::new(rows).map_err(|cause| BenchmarkError::LexicalSegment {
        cause: Box::new(lexical_segment_fault(cause)),
    })?;
    let selected = [segment.id];
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"capacity-lexical-generation"),
        &[],
        &selected,
    )
    .map_err(BenchmarkError::IndexSnapshot)?;
    let segments = [segment];
    let manifest =
        LexicalManifest::new(snapshot, &segments, &[]).map_err(BenchmarkError::LexicalManifest)?;
    let adapter = TantivyLexical::build(manifest).map_err(BenchmarkError::Tantivy)?;
    let mut output = [None; TANTIVY_TOP_K];
    let terminal: TantivyTerminal = std::hint::black_box(
        adapter
            .search(
                snapshot.id,
                core::str::from_utf8(corpus.name(0)?).map_err(BenchmarkError::SourceNameUtf8)?,
                TANTIVY_TOP_K,
                &mut output,
            )
            .map_err(BenchmarkError::Tantivy)?,
    );
    Ok(StageWork {
        input_items: 1,
        output_items: terminal.written,
        bytes_read: u64::try_from(corpus.name(0)?.len()).map_err(BenchmarkError::ByteCount)?,
        bytes_written: u64::try_from(size_of_val(fixed_prefix(&output, terminal.written)?))
            .map_err(BenchmarkError::ByteCount)?,
        durable_bytes: 0,
    })
}
