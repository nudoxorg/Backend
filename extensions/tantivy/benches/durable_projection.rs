//! Fixed-fixture durable projection benchmark with cold build, warm reuse, and query timings.
#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::large_stack_arrays,
    reason = "the fixed benchmark fixture is intentionally fail-fast and bounded"
)]

use backend_semantic::ir::EntityId;
use backend_version::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};
use backend_semantic::index_core::{
    EntityArtifactIdentity, EntityDocumentId, IndexSnapshot, LexicalOperation, LexicalRow,
    LexicalScore, LexicalSegment,
};
use backend_extension_tantivy::server::TantivySegmentStore;
use std::{fs, time::Instant};

const SAMPLES: usize = 24;
const WARMUPS: usize = 3;

fn document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                b"durable-bench",
            ),
        ),
        entity: EntityId::new(entity),
    }
}
fn percentile(samples: &mut [u128], rank: usize) -> u128 {
    samples.sort_unstable();
    samples[rank.min(samples.len().saturating_sub(1))]
}
fn fixture(size: usize) -> (LexicalSegment<'static>, IndexSnapshot<'static>) {
    let rows = (0..size)
        .map(|entity| {
            LexicalRow::new(
                if entity < size / 2 {
                    &b"alpha"[..]
                } else {
                    &b"beta"[..]
                },
                document(entity as u32),
                LexicalScore::from((entity % 251 + 1) as u32),
            )
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let rows = Box::leak(rows);
    let segment = LexicalSegment::new(rows).expect("fixed benchmark segment");
    let ids = Box::leak(Box::new([segment.id]));
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"durable-bench-generation"),
        &[],
        ids,
    )
    .expect("fixed benchmark snapshot");
    (segment, snapshot)
}

fn run(size: usize) {
    let (segment, snapshot) = fixture(size);
    let mut cold = [0_u128; SAMPLES];
    let mut warm = [0_u128; SAMPLES];
    let mut search = [0_u128; SAMPLES];
    let root =
        std::env::temp_dir().join(format!("nudox-durable-bench-{}-{size}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = TantivySegmentStore::open(&root).expect("benchmark root");
    for sample in 0..(WARMUPS + SAMPLES) {
        let cold_root = root.join(format!("cold-{sample}"));
        let cold_store = TantivySegmentStore::open(&cold_root).expect("cold root");
        let started = Instant::now();
        let _opened = cold_store.project(segment).expect("cold project");
        if sample >= WARMUPS {
            cold[sample - WARMUPS] = started.elapsed().as_nanos();
        }
        let started = Instant::now();
        let opened = store.project(segment).expect("warm project");
        if sample >= WARMUPS {
            warm[sample - WARMUPS] = started.elapsed().as_nanos();
        }
        let opened_segments = [&opened];
        let pinned = store
            .compose(snapshot, &opened_segments)
            .expect("benchmark compose");
        let mut output = [None; 256];
        let mut candidates = [None; 256];
        let started = Instant::now();
        let written = pinned
            .search(
                LexicalOperation::new(b"alpha"),
                256,
                &mut output,
                &mut candidates,
            )
            .expect("benchmark search");
        if sample >= WARMUPS {
            search[sample - WARMUPS] = started.elapsed().as_nanos();
        }
        assert_eq!(written, size.div_ceil(2));
        let _ = fs::remove_dir_all(cold_root);
    }
    println!(
        "durable_projection size={size} samples={SAMPLES} warmups={WARMUPS} cold_build median={}ns p95={}ns warm_reuse median={}ns p95={}ns query median={}ns p95={}ns",
        percentile(&mut cold, SAMPLES / 2),
        percentile(&mut cold, SAMPLES * 95 / 100),
        percentile(&mut warm, SAMPLES / 2),
        percentile(&mut warm, SAMPLES * 95 / 100),
        percentile(&mut search, SAMPLES / 2),
        percentile(&mut search, SAMPLES * 95 / 100)
    );
    let _ = fs::remove_dir_all(root);
}

fn main() {
    for size in [64, 128, 256] {
        run(size);
    }
}
