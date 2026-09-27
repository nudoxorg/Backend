//! One durable segment: a broad membership query and a one-row query.
#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "the fixed benchmark fixture is intentionally fail-fast and bounded"
)]

use backend_extension_tantivy::server::TantivySegmentStore;
use backend_semantic::index_core::{
    EntityArtifactIdentity, EntityDocumentId, IndexSnapshot, LexicalOperation, LexicalRow,
    LexicalScore, LexicalSegment,
};
use backend_semantic::ir::EntityId;
use backend_version::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};
use std::hint::black_box;
use std::time::Instant;

const ROWS: usize = 4096;
const BROAD_LIMIT: usize = 25;
const SAMPLES: usize = 32;
const WARMUPS: usize = 4;

fn document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                b"segment-collect",
            ),
        ),
        entity: EntityId::new(entity),
    }
}

fn percentile(samples: &mut [u128], rank: usize) -> u128 {
    samples.sort_unstable();
    samples[rank.min(samples.len().saturating_sub(1))]
}

fn main() {
    let rows = (0..ROWS)
        .map(|entity| {
            let term: &'static [u8] = if entity < ROWS / 2 {
                b"alpha"
            } else if entity + 1 == ROWS {
                b"zephyr"
            } else {
                b"beta"
            };
            LexicalRow::new(
                term,
                document(entity as u32),
                LexicalScore::from((entity % 251 + 1) as u32),
            )
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let rows = Box::leak(rows);
    let segment = LexicalSegment::new(rows).expect("segment");
    let ids = Box::leak(Box::new([segment.id]));
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"segment-collect-generation"),
        &[],
        ids,
    )
    .expect("snapshot");
    let root = std::env::temp_dir().join(format!(
        "nudox-segment-collect-{}-{ROWS}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let store = TantivySegmentStore::open(&root).expect("store");
    let opened = store.project(segment).expect("project");
    let opened_segments = [&opened];
    let pinned = store.compose(snapshot, &opened_segments).expect("compose");
    let broad = LexicalOperation::new(b"alpha");
    let rare = LexicalOperation::new(b"zephyr");
    let mut broad_samples = [0_u128; SAMPLES];
    let mut rare_samples = [0_u128; SAMPLES];
    let mut broad_output = vec![None; BROAD_LIMIT];
    let mut broad_candidates = vec![None; ROWS];
    let mut rare_output = vec![None; 1];
    let mut rare_candidates = vec![None; ROWS];
    for sample in 0..(WARMUPS + SAMPLES) {
        let started = Instant::now();
        let written = pinned
            .search(broad, BROAD_LIMIT, &mut broad_output, &mut broad_candidates)
            .expect("broad search");
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(black_box(written), BROAD_LIMIT);
        if sample >= WARMUPS {
            broad_samples[sample - WARMUPS] = elapsed;
        }
        let started = Instant::now();
        let written = pinned
            .search(rare, 1, &mut rare_output, &mut rare_candidates)
            .expect("rare search");
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(black_box(written), 1);
        if sample >= WARMUPS {
            rare_samples[sample - WARMUPS] = elapsed;
        }
    }
    println!(
        "segment_collect rows={ROWS} broad_hits=2048 broad_limit={BROAD_LIMIT} broad_median_ns={} broad_p95_ns={} rare_hits=1 rare_median_ns={} rare_p95_ns={}",
        percentile(&mut broad_samples, SAMPLES / 2),
        percentile(&mut broad_samples, SAMPLES * 95 / 100),
        percentile(&mut rare_samples, SAMPLES / 2),
        percentile(&mut rare_samples, SAMPLES * 95 / 100),
    );
    let _ = std::fs::remove_dir_all(root);
}
