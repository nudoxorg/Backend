//! Fixed-fixture routing benchmark harness.

use std::time::{Duration, Instant};

use compiler_ir::EntityId;
use backend_version::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};
use server_index_core::{EntityArtifactIdentity, EntityDocumentId, IndexSnapshot};
use backend_execution::routing::{
    Coordinator, MAX_SEGMENTS, MissingAssignment, ObservedHit, OrderingRecipe, Query, RetryPolicy,
    RouteAttempt, RoutedHitSlot, SegmentOrdinal, SegmentRange, TopK, WorkerId, WorkerReply,
    merge_replies,
};
use server_index_vocabulary::LexicalSegmentId;

const WARMUPS: usize = 20;
const SAMPLES: usize = 100;
const ROWS_PER_SEGMENT: usize = 32;
const TERMS: [&[u8]; 4] = [b"term-a", b"term-b", b"term-c", b"term-d"];

fn main() {
    for segment_count in [1_usize, 4, MAX_SEGMENTS] {
        let Some((median, p95)) = fixture(segment_count) else {
            eprintln!("routing benchmark fixture {segment_count} rejected");
            return;
        };
        println!(
            "routing segments={segment_count} median_ns={} p95_ns={}",
            median.as_nanos(),
            p95.as_nanos()
        );
    }
}

#[allow(
    clippy::cognitive_complexity,
    reason = "the benchmark keeps one fixed fixture's setup, validation, and timing together"
)]
fn fixture(segment_count: usize) -> Option<(Duration, Duration)> {
    let mut segment_bytes = Vec::with_capacity(segment_count);
    for value in 0..segment_count {
        segment_bytes.push([u8::try_from(value + 1).ok()?]);
    }
    let segments: Vec<LexicalSegmentId> = segment_bytes
        .iter()
        .map(|bytes| LexicalSegmentId::from_canonical_bytes(bytes))
        .collect();
    let snapshot = IndexSnapshot::new(GenerationId::from_digest([7; 32]), &[], &segments).ok()?;
    let workers = [WorkerId::new(1), WorkerId::new(2), WorkerId::new(3)];
    let retry = RetryPolicy::new(2).ok()?;
    let coordinator = Coordinator::new(snapshot, &workers, retry).ok()?;
    let top_k = TopK::new(64).ok()?;
    let query = Query::new(b"fixed-query", OrderingRecipe::new(1)).with_top_k(top_k);
    let plan = coordinator.plan(query).ok()?;
    let mut rows = Vec::with_capacity(segment_count);
    for (ordinal, segment) in segments.iter().copied().enumerate() {
        let mut segment_rows = Vec::with_capacity(ROWS_PER_SEGMENT);
        for row in 0..ROWS_PER_SEGMENT {
            let document_value = if row % 4 == 0 {
                u8::try_from(row / 4).ok()?
            } else {
                u8::try_from(8 + ordinal * 24 + row - row / 4).ok()?
            };
            let score = if row % 4 == 0 {
                20_000_u32.checked_sub(u32::try_from(ordinal * 100 + row).ok()?)?
            } else {
                10_000_u32.checked_sub(u32::try_from(ordinal * 100 + row).ok()?)?
            };
            segment_rows.push(ObservedHit {
                segment,
                term: TERMS.get(row % TERMS.len()).copied()?,
                document: document(document_value),
                score,
            });
        }
        rows.push(segment_rows);
    }
    let mut replies = Vec::with_capacity(segment_count);
    for ordinal in 0..segment_count {
        let assignment = plan.assignment(ordinal)?;
        let worker = assignment.candidate(0)?;
        replies.push(WorkerReply::new(
            RouteAttempt {
                route: plan.route(),
                snapshot: plan.snapshot(),
                query: plan.query(),
                segment_ordinal: assignment.ordinal,
                segment: assignment.segment,
                range: SegmentRange::whole(),
                worker,
                ordinal: backend_execution::routing::AttemptOrdinal::new(0),
            },
            rows.get(ordinal)?.as_slice(),
        ));
    }
    let mut seen = vec![None; segment_count * ROWS_PER_SEGMENT];
    let ordinal = SegmentOrdinal::new(0).ok()?;
    let first_segment = segments.first().copied()?;
    let mut missing = vec![
        MissingAssignment {
            ordinal,
            segment: first_segment,
            range: SegmentRange::whole(),
        };
        segment_count
    ];
    let mut output = vec![RoutedHitSlot::vacant(); top_k.get()];
    let mut expected = Vec::with_capacity(segment_count * ROWS_PER_SEGMENT);
    for segment_rows in &rows {
        for &candidate in segment_rows {
            if let Some(current) = expected
                .iter_mut()
                .find(|current: &&mut ObservedHit<'static>| current.document == candidate.document)
            {
                if precedes(candidate, current) {
                    *current = candidate;
                }
            } else {
                expected.push(candidate);
            }
        }
    }
    let expected_unique = expected.len();
    if expected_unique != 8 + 24 * segment_count {
        return None;
    }
    expected.sort_unstable_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.document.cmp(&right.document))
            .then_with(|| left.segment.cmp(&right.segment))
            .then_with(|| left.term.cmp(right.term))
    });
    expected.truncate(top_k.get());
    let expected_checksum = checksum_observed(&expected)?;
    let newest_segment = segments.first().copied()?;
    for _ in 0..WARMUPS {
        let _ = merge_replies(&plan, &replies, &mut seen, &mut missing, &mut output).ok()?;
    }
    let mut timings = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let result = merge_replies(&plan, &replies, &mut seen, &mut missing, &mut output).ok()?;
        if result.hits.len() != expected.len() || !result.missing.is_empty() {
            return None;
        }
        for (actual_slot, expected_hit) in result.hits.iter().zip(expected.iter()) {
            let actual = actual_slot.get()?;
            if actual.document() != expected_hit.document
                || actual.score() != expected_hit.score
                || actual.term() != expected_hit.term
                || actual.segment() != expected_hit.segment
            {
                return None;
            }
        }
        if segment_count > 1
            && result
                .hits
                .iter()
                .take(ROWS_PER_SEGMENT / 4)
                .any(|slot| slot.get().is_none_or(|hit| hit.segment() != newest_segment))
        {
            return None;
        }
        if checksum_slots(result.hits)? != expected_checksum {
            return None;
        }
        timings.push(start.elapsed());
    }
    timings.sort_unstable();
    let median = *timings.get(SAMPLES / 2)?;
    let p95 = *timings.get(SAMPLES * 95 / 100)?;
    Some((median, p95))
}

fn precedes(left: ObservedHit<'_>, right: &ObservedHit<'_>) -> bool {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.document.cmp(&right.document))
        .then_with(|| left.segment.cmp(&right.segment))
        .then_with(|| left.term.cmp(right.term))
        .is_lt()
}

fn checksum_observed(rows: &[ObservedHit<'_>]) -> Option<u64> {
    let mut checksum = 0_u64;
    for (position, row) in rows.iter().enumerate() {
        let position = u64::try_from(position).ok()?;
        checksum = checksum
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(u64::from(row.score))
            .wrapping_add(position);
        for byte in row.term {
            checksum = checksum.rotate_left(5).wrapping_add(u64::from(*byte));
        }
    }
    Some(checksum)
}

fn checksum_slots(rows: &[RoutedHitSlot<'_>]) -> Option<u64> {
    let mut checksum = 0_u64;
    for (position, slot) in rows.iter().enumerate() {
        let position = u64::try_from(position).ok()?;
        let Some(row) = slot.get() else {
            continue;
        };
        checksum = checksum
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(u64::from(row.score()))
            .wrapping_add(position);
        for byte in row.term() {
            checksum = checksum.rotate_left(5).wrapping_add(u64::from(*byte));
        }
    }
    Some(checksum)
}

fn document(value: u8) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_digest([value; 32]),
        ),
        entity: EntityId::new(u32::from(value)),
    }
}
