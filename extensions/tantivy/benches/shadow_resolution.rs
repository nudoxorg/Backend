//! Fixed-fixture measurements for lexical shadow resolution before Tantivy publication.
#![deny(unsafe_code)]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "the fixed benchmark fixture is intentionally fail-fast and bounded"
)]

use std::{error::Error, fmt, hint::black_box, time::Instant};

use backend_semantic::ir::EntityId;
use backend_version::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};
use server_index_core::{
    EntityArtifactIdentity, EntityDocumentId, LexicalOrderKey, LexicalRow, LexicalScore,
    LexicalSegment,
};

const SEGMENTS: usize = 8;
const WARMUPS: usize = 5;
const SAMPLES: usize = 20;
const REPEATS: usize = 100;
const REPEATS_NANOS: u128 = 100;
// Eight segments at these widths exercise 64, 128, and the production maximum of 256 rows.
const CASES: [usize; 3] = [8, 16, 32];

#[derive(Clone, Copy, Debug)]
enum BenchmarkError {
    Fixture,
    Mismatch,
}

impl fmt::Display for BenchmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixture => formatter.write_str("fixed Tantivy shadow fixture was rejected"),
            Self::Mismatch => formatter.write_str("shadow-resolution algorithms disagreed"),
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
    let pairwise = pairwise_survivors(&segments);
    let binary = binary_search_survivors(&segments);
    if pairwise != binary {
        return Err(BenchmarkError::Mismatch);
    }
    for _ in 0..WARMUPS {
        black_box(pairwise_survivors(&segments));
        black_box(binary_search_survivors(&segments));
    }
    let mut baseline = [0_u128; SAMPLES];
    let mut optimized = [0_u128; SAMPLES];
    for sample in 0..SAMPLES {
        baseline[sample] = measure(|| pairwise_survivors(&segments));
        optimized[sample] = measure(|| binary_search_survivors(&segments));
    }
    report(rows_per_segment, "pairwise", baseline);
    report(rows_per_segment, "binary", optimized);
    Ok(())
}

fn fixture_rows(rows_per_segment: usize) -> Result<Vec<Vec<LexicalRow<'static>>>, BenchmarkError> {
    let mut result = Vec::with_capacity(SEGMENTS);
    for segment in 0..SEGMENTS {
        let score =
            LexicalScore::from(u32::try_from(segment + 1).map_err(|_| BenchmarkError::Fixture)?);
        let mut rows = Vec::with_capacity(rows_per_segment);
        for entity in 0..rows_per_segment {
            rows.push(LexicalRow::new(
                b"needle",
                document(u32::try_from(entity).map_err(|_| BenchmarkError::Fixture)?),
                score,
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
                b"tantivy-shadow-benchmark-fragment",
            ),
        ),
        entity: EntityId::new(entity),
    }
}

fn pairwise_survivors(segments: &[LexicalSegment<'_>]) -> usize {
    let mut survivors = 0_usize;
    for (segment_position, segment) in segments.iter().enumerate() {
        for row in segment.rows {
            let shadowed = segments
                .iter()
                .take(segment_position)
                .flat_map(|newer| newer.rows)
                .any(|newer| newer.term == row.term && newer.document == row.document);
            if !shadowed && !row.is_tombstone() {
                survivors += 1;
            }
        }
    }
    survivors
}

fn binary_search_survivors(segments: &[LexicalSegment<'_>]) -> usize {
    let mut survivors = 0_usize;
    for (segment_position, segment) in segments.iter().enumerate() {
        for row in segment.rows {
            let key = LexicalOrderKey::from(*row);
            let shadowed = segments.iter().take(segment_position).any(|newer| {
                newer
                    .rows
                    .binary_search_by(|candidate| LexicalOrderKey::from(*candidate).cmp(&key))
                    .is_ok()
            });
            if !shadowed && !row.is_tombstone() {
                survivors += 1;
            }
        }
    }
    survivors
}

fn measure(operation: impl Fn() -> usize) -> u128 {
    let start = Instant::now();
    for _ in 0..REPEATS {
        black_box(operation());
    }
    start.elapsed().as_nanos() / REPEATS_NANOS
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
