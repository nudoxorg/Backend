//! One in-memory nested projection: broad, full, and one-document membership.
#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "the fixed benchmark fixture is intentionally fail-fast and bounded"
)]

use std::hint::black_box;
use std::time::Instant;

use backend_extension_tantivy::server::{MAX_TANTIVY_DOCUMENTS, TantivyLexical};
use backend_semantic::index_core::{
    EntityArtifactIdentity, EntityDocumentId, IndexSnapshot, LexicalManifest, LexicalRow,
    LexicalScore, LexicalSegment,
};
use backend_semantic::ir::EntityId;
use backend_version::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};

const ROWS: usize = MAX_TANTIVY_DOCUMENTS;
const BROAD_HITS: usize = ROWS / 2;
const LIMIT: usize = 25;
const SAMPLES: usize = 32;
const WARMUPS: usize = 4;

fn document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                b"nested-collect",
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
            let term: &'static [u8] = if entity < BROAD_HITS {
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
    let selected = Box::leak(Box::new([segment.id]));
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"nested-collect-generation"),
        &[],
        selected,
    )
    .expect("snapshot");
    let snapshot_id = snapshot.id;
    let segments = Box::leak(Box::new([segment]));
    let manifest = LexicalManifest::new(snapshot, segments, &[]).expect("manifest");
    let projection = TantivyLexical::build(manifest).expect("projection");
    let mut broad_output = vec![None; LIMIT];
    let mut full_output = vec![None; LIMIT];
    let mut rare_output = vec![None; 1];
    let mut broad_samples = [0_u128; SAMPLES];
    let mut full_samples = [0_u128; SAMPLES];
    let mut rare_samples = [0_u128; SAMPLES];
    for sample in 0..(WARMUPS + SAMPLES) {
        broad_output.fill(None);
        let started = Instant::now();
        let terminal = projection
            .search(snapshot_id, "alpha", LIMIT, &mut broad_output)
            .expect("broad search");
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(black_box(terminal.written), LIMIT);
        assert_eq!(broad_output[0].expect("broad hit").document, document(0));
        if sample >= WARMUPS {
            broad_samples[sample - WARMUPS] = elapsed;
        }

        full_output.fill(None);
        let started = Instant::now();
        let terminal = projection
            .search(
                snapshot_id,
                "alpha OR beta OR zephyr",
                LIMIT,
                &mut full_output,
            )
            .expect("full search");
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(black_box(terminal.written), LIMIT);
        assert_eq!(full_output[0].expect("full hit").document, document(0));
        if sample >= WARMUPS {
            full_samples[sample - WARMUPS] = elapsed;
        }

        rare_output.fill(None);
        let started = Instant::now();
        let terminal = projection
            .search(snapshot_id, "zephyr", 1, &mut rare_output)
            .expect("rare search");
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(black_box(terminal.written), 1);
        assert_eq!(
            rare_output[0].expect("rare hit").document,
            document((ROWS - 1) as u32)
        );
        if sample >= WARMUPS {
            rare_samples[sample - WARMUPS] = elapsed;
        }
    }
    println!(
        "nested_collect documents={ROWS} broad_hits={BROAD_HITS} full_hits={ROWS} limit={LIMIT} broad_median_ns={} broad_p95_ns={} full_median_ns={} full_p95_ns={} rare_hits=1 rare_median_ns={} rare_p95_ns={}",
        percentile(&mut broad_samples, SAMPLES / 2),
        percentile(&mut broad_samples, SAMPLES * 95 / 100),
        percentile(&mut full_samples, SAMPLES / 2),
        percentile(&mut full_samples, SAMPLES * 95 / 100),
        percentile(&mut rare_samples, SAMPLES / 2),
        percentile(&mut rare_samples, SAMPLES * 95 / 100),
    );
}
