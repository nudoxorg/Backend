//! Package-graph projection: exact reuse, root-only moves, and one-source edits
//! against a large persisted graph.
//!
//! Checked fact snapshots and roots are built outside the timer. The cold
//! publish is outside the timer too. The graph has thousands of sources, while
//! each edit changes one edge owned by one source; alternating two immutable
//! checked snapshots keeps snapshot construction outside the timed section.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "the fixed benchmark fixture is intentionally fail-fast and bounded"
)]

use std::hint::black_box;
use std::time::Instant;

use backend_extension_turso::{ProjectionUpdate, TursoProjection};
use backend_library::{
    CheckedPackageGraphFacts, DependencyAuthority, DependencyEvidence, DependencyFacts,
    DependencyScope, PackageDependencyRecord, PackageDependencyTarget, PackageGraphSourceKey,
    PackageReference, RegistryEcosystem, view_state_root,
};

const SOURCE_COUNT: usize = 4_096;
const EDGES_PER_SOURCE: usize = 8;
const EDGES: usize = SOURCE_COUNT * EDGES_PER_SOURCE;
const SAMPLES: usize = 32;
const WARMUPS: usize = 4;

fn edge(source: &PackageReference, index: usize, requirement: &str) -> PackageDependencyRecord {
    PackageDependencyRecord::new(
        source.clone(),
        PackageDependencyTarget::new(
            RegistryEcosystem::Cargo,
            format!("dep{index}"),
            requirement,
            None,
        )
        .expect("target"),
        DependencyScope::Runtime,
        false,
        DependencyEvidence {
            authority: DependencyAuthority::RegistryMetadata,
            frontier: [1; 32],
            provenance: [2; 32],
        },
    )
}

fn facts(
    requirement_for_first_source: &str,
) -> Vec<(
    PackageGraphSourceKey,
    DependencyFacts<Box<[PackageDependencyRecord]>>,
)> {
    (0..SOURCE_COUNT)
        .map(|source_index| {
            let source = PackageReference::parse(format!("pkg:cargo/app{source_index}@1.0.0"))
                .expect("source");
            let rows = (0..EDGES_PER_SOURCE)
                .map(|edge_index| {
                    let requirement = if source_index == 0 && edge_index == 0 {
                        requirement_for_first_source
                    } else {
                        "^1"
                    };
                    edge(
                        &source,
                        source_index * EDGES_PER_SOURCE + edge_index,
                        requirement,
                    )
                })
                .collect::<Vec<_>>()
                .into_boxed_slice();
            (
                PackageGraphSourceKey::unattributed(source),
                DependencyFacts::Known(rows),
            )
        })
        .collect()
}

fn percentile(samples: &mut [u128], rank: usize) -> u128 {
    samples.sort_unstable();
    samples[rank.min(samples.len().saturating_sub(1))]
}

fn main() {
    futures_executor::block_on(async {
        let path = std::env::temp_dir().join(format!(
            "nudox-package-graph-{}-{EDGES}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).expect("root");
        let database = path.join("projection.turso");
        let mut projection = TursoProjection::open(&database).await.expect("open");
        let stable = CheckedPackageGraphFacts::new(facts("^1")).expect("checked stable facts");
        let edited = CheckedPackageGraphFacts::new(facts("^2")).expect("checked one-source edit");
        let cold_root = view_state_root(&[("graph".to_owned(), "cold".to_owned())]);
        let cold = projection
            .synchronize_checked_package_graph(cold_root, &stable)
            .await
            .expect("cold");
        assert_eq!(cold, ProjectionUpdate::Rebuilt { rows: EDGES as u64 });
        let move_roots = (0..WARMUPS + SAMPLES)
            .map(|index| view_state_root(&[("graph".to_owned(), format!("move-{index}"))]))
            .collect::<Vec<_>>();
        assert_ne!(edited.witness(), stable.witness());
        // Keep one selected root so edits exercise the facts-witness mismatch
        // path, rather than rebuilding only because the view root changed.
        let edit_root = view_state_root(&[("graph".to_owned(), "same-edit-root".to_owned())]);
        let mut move_samples = [0_u128; SAMPLES];
        let mut reuse_samples = [0_u128; SAMPLES];
        for sample in 0..(WARMUPS + SAMPLES) {
            let started = Instant::now();
            let update = projection
                .synchronize_checked_package_graph(move_roots[sample], &stable)
                .await
                .expect("move");
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                black_box(update),
                ProjectionUpdate::Rebuilt { rows: EDGES as u64 }
            );
            if sample >= WARMUPS {
                move_samples[sample - WARMUPS] = elapsed;
            }
            let reused_at = Instant::now();
            let reused = projection
                .synchronize_checked_package_graph(move_roots[sample], &stable)
                .await
                .expect("reuse exact root and facts");
            assert_eq!(
                black_box(reused),
                ProjectionUpdate::Reused { rows: EDGES as u64 }
            );
            if sample >= WARMUPS {
                reuse_samples[sample - WARMUPS] = reused_at.elapsed().as_nanos();
            }
        }
        let mut edit_samples = [0_u128; SAMPLES];
        for sample in 0..(WARMUPS + SAMPLES) {
            let started = Instant::now();
            let update = projection
                .synchronize_checked_package_graph(
                    edit_root.clone(),
                    if sample % 2 == 0 { &edited } else { &stable },
                )
                .await
                .expect("edit");
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                black_box(update),
                ProjectionUpdate::Rebuilt { rows: EDGES as u64 }
            );
            if sample >= WARMUPS {
                edit_samples[sample - WARMUPS] = elapsed;
            }
        }
        println!(
            "package_graph sources={SOURCE_COUNT} edges={EDGES} changed_sources=1 changed_edges=1 reuse_median_ns={} reuse_p95_ns={} move_median_ns={} move_p95_ns={} delta_median_ns={} delta_p95_ns={}",
            percentile(&mut reuse_samples, SAMPLES / 2),
            percentile(&mut reuse_samples, SAMPLES * 95 / 100),
            percentile(&mut move_samples, SAMPLES / 2),
            percentile(&mut move_samples, SAMPLES * 95 / 100),
            percentile(&mut edit_samples, SAMPLES / 2),
            percentile(&mut edit_samples, SAMPLES * 95 / 100),
        );
        drop(projection);
        let _ = std::fs::remove_dir_all(path);
    });
}
