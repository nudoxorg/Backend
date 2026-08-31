//! Measures `heart-root` benches capacity-planning runner exact work with production data paths.
//! Measurements separate setup from steady-state work and retain resource counters.
//! Results support capacity decisions without changing the measured implementation.
//! Exact-core immutable-segment construction and exact lookup phase.

use server_index_core::{
    ExactManifest, ExactOperation, ExactRow, ExactSegment, GenerationId, IndexSnapshot,
};

use crate::{
    BenchmarkError,
    measure::StageWork,
    model::MAX_CORPUS,
    runner::{
        failure::exact_segment_fault,
        fixture::{Corpus, FragmentSlots},
    },
};

pub(crate) fn exact_query(
    corpus: &Corpus,
    fragments: &FragmentSlots,
) -> Result<StageWork, BenchmarkError> {
    let first = ExactRow::present(corpus.name(0)?, fragments.bytes(0)?);
    let mut rows = [first; MAX_CORPUS];
    for (index, row) in rows.iter_mut().take(corpus.len).enumerate() {
        *row = ExactRow::present(corpus.name(index)?, fragments.bytes(index)?);
    }
    let prefix = rows
        .get(..corpus.len)
        .ok_or(BenchmarkError::FixedSliceLength {
            observed: corpus.len,
            capacity: rows.len(),
        })?;
    let segment = ExactSegment::new(prefix).map_err(|cause| BenchmarkError::ExactSegment {
        cause: exact_segment_fault(cause),
    })?;
    let selected = [segment.id];
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"capacity-exact-generation"),
        &selected,
        &[],
    )
    .map_err(BenchmarkError::IndexSnapshot)?;
    let segments = [segment];
    let manifest =
        ExactManifest::new(snapshot, &segments, &[]).map_err(BenchmarkError::ExactManifest)?;
    let result = std::hint::black_box(manifest.execute(ExactOperation::new(corpus.name(0)?)));
    let output_items = usize::from(matches!(
        result,
        server_index_core::ExactTerminal::Complete {
            resolution: server_index_core::ExactResolution::Present { .. },
            ..
        }
    ));
    Ok(StageWork {
        input_items: 1,
        output_items,
        bytes_read: u64::try_from(corpus.name(0)?.len()).map_err(BenchmarkError::ByteCount)?,
        bytes_written: 0,
        durable_bytes: 0,
    })
}
