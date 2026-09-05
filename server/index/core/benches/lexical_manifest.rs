//! Fixed-fixture lexical manifest measurements for the caller-owned scratch data flow.
#![deny(unsafe_code)]

use std::{error::Error, fmt, hint::black_box, time::Instant};

use compiler_ir::EntityId;
use heart_identity::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};
use server_index_core::{
    EntityArtifactIdentity, EntityDocumentId, GenerationId, IndexSnapshot, LexicalManifest,
    LexicalOperation, LexicalRow, LexicalScore, LexicalSegment, LexicalSnapshotHit,
    LexicalTerminal, LexicalTopK,
};

const SEGMENTS: usize = 8;
const TOP_K: usize = 16;
const WARMUPS: usize = 5;
const SAMPLES: usize = 20;
const REPEATS: usize = 100;
const CASES: [usize; 3] = [32, 128, 256];

#[derive(Clone, Copy, Debug)]
enum BenchmarkError {
    Fixture,
    Execution,
    UnexpectedTerminal,
}

impl fmt::Display for BenchmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixture => formatter.write_str("fixed lexical benchmark fixture was rejected"),
            Self::Execution => {
                formatter.write_str("fixed lexical benchmark execution was rejected")
            }
            Self::UnexpectedTerminal => {
                formatter.write_str("fixed lexical benchmark returned a noncomplete terminal")
            }
        }
    }
}

impl Error for BenchmarkError {}

fn main() -> Result<(), Box<dyn Error>> {
    for rows_per_segment in CASES {
        run_case(rows_per_segment)?;
    }
    Ok(())
}

fn run_case(rows_per_segment: usize) -> Result<(), BenchmarkError> {
    let rows = fixture_rows(rows_per_segment)?;
    let mut segments = Vec::with_capacity(SEGMENTS);
    for segment_rows in &rows {
        segments.push(LexicalSegment::new(segment_rows).map_err(|_| BenchmarkError::Fixture)?);
    }
    let selected: Vec<_> = segments.iter().map(|segment| segment.id).collect();
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"lexical-manifest-benchmark"),
        &[],
        &selected,
    )
    .map_err(|_| BenchmarkError::Fixture)?;
    let manifest =
        LexicalManifest::new(snapshot, &segments, &[]).map_err(|_| BenchmarkError::Fixture)?;
    let top_k = LexicalTopK::new(TOP_K).map_err(|_| BenchmarkError::Fixture)?;
    let operation = LexicalOperation::new(b"needle");
    verify_equivalence(
        &manifest,
        operation,
        top_k,
        rows_per_segment,
        segments[0].id,
    )?;

    for _ in 0..WARMUPS {
        let _ = measure_baseline(
            &manifest,
            operation,
            top_k,
            rows_per_segment,
            segments[0].id,
        )?;
        let _ = measure_optimized(
            &manifest,
            operation,
            top_k,
            rows_per_segment,
            segments[0].id,
        )?;
    }
    let mut baseline = [0_u128; SAMPLES];
    let mut optimized = [0_u128; SAMPLES];
    for sample in 0..SAMPLES {
        baseline[sample] = measure_baseline(
            &manifest,
            operation,
            top_k,
            rows_per_segment,
            segments[0].id,
        )?;
        optimized[sample] = measure_optimized(
            &manifest,
            operation,
            top_k,
            rows_per_segment,
            segments[0].id,
        )?;
    }
    report(rows_per_segment, "baseline", baseline);
    report(rows_per_segment, "optimized", optimized);
    Ok(())
}

fn fixture_rows(rows_per_segment: usize) -> Result<Vec<Vec<LexicalRow<'static>>>, BenchmarkError> {
    let mut result = Vec::with_capacity(SEGMENTS);
    for segment in 0..SEGMENTS {
        let mut rows = Vec::with_capacity(rows_per_segment);
        for document_id in 0..rows_per_segment {
            rows.push(LexicalRow::new(
                b"needle",
                document(u32::try_from(document_id).map_err(|_| BenchmarkError::Fixture)?),
                LexicalScore::from(
                    u32::try_from(segment + 1).map_err(|_| BenchmarkError::Fixture)?,
                ),
            ));
        }
        result.push(rows);
    }
    Ok(result)
}

fn document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                b"lexical-manifest-benchmark-fragment",
            ),
        ),
        entity: EntityId::new(entity),
    }
}

fn placeholder(segment: server_index_core::LexicalSegmentId) -> LexicalSnapshotHit<'static> {
    LexicalSnapshotHit::new(
        segment,
        b"placeholder",
        document(u32::MAX),
        LexicalScore::from(0),
    )
}

fn verify_equivalence(
    manifest: &LexicalManifest<'_, '_>,
    operation: LexicalOperation<'_>,
    top_k: LexicalTopK,
    rows_per_segment: usize,
    segment: server_index_core::LexicalSegmentId,
) -> Result<(), BenchmarkError> {
    let matching_rows = rows_per_segment
        .checked_mul(SEGMENTS)
        .ok_or(BenchmarkError::Fixture)?;
    let mut baseline_scratch = vec![None; matching_rows];
    let mut baseline_output = [placeholder(segment); TOP_K];
    let baseline_written = baseline_execute(
        manifest,
        operation,
        top_k,
        &mut baseline_scratch,
        &mut baseline_output,
    )?;

    let mut optimized_scratch = vec![None; matching_rows];
    let mut optimized_output = [placeholder(segment); TOP_K];
    let terminal = manifest
        .execute(
            operation,
            top_k,
            &mut optimized_scratch,
            &mut optimized_output,
        )
        .map_err(|_| BenchmarkError::Execution)?;
    let hits = match terminal {
        LexicalTerminal::Complete { hits, .. } => hits,
        LexicalTerminal::Partial { .. } | LexicalTerminal::Degraded { .. } => {
            return Err(BenchmarkError::UnexpectedTerminal);
        }
    };
    if hits != &baseline_output[..baseline_written] {
        return Err(BenchmarkError::Execution);
    }
    Ok(())
}

fn measure_baseline(
    manifest: &LexicalManifest<'_, '_>,
    operation: LexicalOperation<'_>,
    top_k: LexicalTopK,
    rows_per_segment: usize,
    segment: server_index_core::LexicalSegmentId,
) -> Result<u128, BenchmarkError> {
    let matching_rows = rows_per_segment
        .checked_mul(SEGMENTS)
        .ok_or(BenchmarkError::Fixture)?;
    let mut scratch = vec![None; matching_rows];
    let mut output = [placeholder(segment); TOP_K];
    let start = Instant::now();
    for _ in 0..REPEATS {
        let written = baseline_execute(manifest, operation, top_k, &mut scratch, &mut output)?;
        black_box((&scratch, &output[..written]));
    }
    Ok(
        start.elapsed().as_nanos()
            / u128::try_from(REPEATS).map_err(|_| BenchmarkError::Fixture)?,
    )
}

fn measure_optimized(
    manifest: &LexicalManifest<'_, '_>,
    operation: LexicalOperation<'_>,
    top_k: LexicalTopK,
    rows_per_segment: usize,
    segment: server_index_core::LexicalSegmentId,
) -> Result<u128, BenchmarkError> {
    let matching_rows = rows_per_segment
        .checked_mul(SEGMENTS)
        .ok_or(BenchmarkError::Fixture)?;
    let mut scratch = vec![None; matching_rows];
    let mut output = [placeholder(segment); TOP_K];
    let start = Instant::now();
    for _ in 0..REPEATS {
        let terminal = manifest
            .execute(operation, top_k, &mut scratch, &mut output)
            .map_err(|_| BenchmarkError::Execution)?;
        black_box(terminal);
    }
    Ok(
        start.elapsed().as_nanos()
            / u128::try_from(REPEATS).map_err(|_| BenchmarkError::Fixture)?,
    )
}

fn baseline_execute<'manifest, 'bytes>(
    manifest: &LexicalManifest<'manifest, 'bytes>,
    operation: LexicalOperation<'_>,
    top_k: LexicalTopK,
    seen_documents: &mut [Option<EntityDocumentId>],
    output: &mut [LexicalSnapshotHit<'bytes>],
) -> Result<usize, BenchmarkError> {
    let matching_rows = manifest
        .segments
        .iter()
        .filter_map(|segment| segment.lookup(operation))
        .map(<[LexicalRow<'_>]>::len)
        .sum::<usize>();
    if seen_documents.len() < matching_rows {
        return Err(BenchmarkError::Execution);
    }
    let mut unique_documents = 0_usize;
    for (segment_position, segment) in manifest.segments.iter().enumerate() {
        if let Some(rows) = segment.lookup(operation) {
            for (row_position, row) in rows.iter().enumerate() {
                let was_seen = manifest
                    .segments
                    .iter()
                    .take(segment_position)
                    .filter_map(|previous| previous.lookup(operation))
                    .flat_map(|previous_rows| previous_rows.iter())
                    .any(|previous| previous.document == row.document)
                    || rows
                        .iter()
                        .take(row_position)
                        .any(|previous| previous.document == row.document);
                if !was_seen && !row.is_tombstone() {
                    unique_documents += 1;
                }
            }
        }
    }
    let required = core::cmp::min(unique_documents, usize::from(top_k));
    if output.len() < required {
        return Err(BenchmarkError::Execution);
    }
    let mut emitted = 0_usize;
    let mut hits = 0_usize;
    for segment in manifest.segments {
        if let Some(rows) = segment.lookup(operation) {
            for row in rows {
                if seen_documents[..emitted].contains(&Some(row.document)) {
                    continue;
                }
                seen_documents[emitted] = Some(row.document);
                emitted += 1;
                let Some(stored) = row.score() else {
                    continue;
                };
                let Some(score) = operation.relevance(stored, row.term.len()) else {
                    return Err(BenchmarkError::Execution);
                };
                let candidate = LexicalSnapshotHit::new(segment.id, row.term, row.document, score);
                let position = output[..hits]
                    .iter()
                    .position(|current| lexical_order(&candidate, current).is_lt())
                    .unwrap_or(hits);
                if position < required {
                    let new_hits = core::cmp::min(hits + 1, required);
                    for destination in (position + 1..new_hits).rev() {
                        output[destination] = output[destination - 1];
                    }
                    output[position] = candidate;
                    hits = new_hits;
                }
            }
        }
    }
    Ok(hits)
}

fn lexical_order(
    left: &LexicalSnapshotHit<'_>,
    right: &LexicalSnapshotHit<'_>,
) -> core::cmp::Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.document.cmp(&right.document))
        .then_with(|| left.segment.cmp(&right.segment))
}

fn report(rows_per_segment: usize, label: &str, mut samples: [u128; SAMPLES]) {
    samples.sort_unstable();
    let median = samples[SAMPLES / 2];
    let p95_index = (SAMPLES * 95).div_ceil(100).saturating_sub(1);
    let p95 = samples[p95_index];
    println!(
        "rows_per_segment={rows_per_segment} segments={SEGMENTS} {label}: min={}ns median={median}ns p95={p95}ns",
        samples[0],
    );
}
