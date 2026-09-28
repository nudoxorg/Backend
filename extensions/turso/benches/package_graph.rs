//! Package-graph projection: exact reuse, a root-only move, and a one-edge edit.
//!
//! Checked fact snapshots and roots are built outside the timer. The cold
//! publish is outside the timer too. Each sample times a root-only move, exact
//! reuse, and then a same-root one-edge mutation using a new checked snapshot.
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
    DependencyScope, PackageDependencyRecord, PackageDependencyTarget, PackageReference,
    PackageGraphSourceKey, RegistryEcosystem, view_state_root,
};

const EDGES: usize = 512;
const SAMPLES: usize = 32;
const WARMUPS: usize = 4;

fn edge(index: usize, requirement: &str) -> PackageDependencyRecord {
    let source = PackageReference::parse("pkg:cargo/app@1.0.0").expect("source");
    PackageDependencyRecord::new(
        source,
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
    requirement_for_first: &str,
) -> Vec<(
    PackageGraphSourceKey,
    DependencyFacts<Box<[PackageDependencyRecord]>>,
)> {
    let source = PackageReference::parse("pkg:cargo/app@1.0.0").expect("source");
    let rows = (0..EDGES)
        .map(|index| {
            let requirement = if index == 0 {
                requirement_for_first
            } else {
                "^1"
            };
            edge(index, requirement)
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    vec![(
        PackageGraphSourceKey::unattributed(source),
        DependencyFacts::Known(rows),
    )]
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
        let cold_root = view_state_root(&[("graph".to_owned(), "cold".to_owned())]);
        let cold = projection
            .synchronize_checked_package_graph(cold_root, &stable)
            .await
            .expect("cold");
        assert_eq!(cold, ProjectionUpdate::Rebuilt { rows: EDGES as u64 });
        let move_roots = (0..WARMUPS + SAMPLES)
            .map(|index| view_state_root(&[("graph".to_owned(), format!("move-{index}"))]))
            .collect::<Vec<_>>();
        let edit_facts = (0..WARMUPS + SAMPLES)
            .map(|index| {
                CheckedPackageGraphFacts::new(facts(&format!("^edit{index}")))
                    .expect("checked changed facts")
            })
            .collect::<Vec<_>>();
        assert!(
            edit_facts
                .iter()
                .all(|changed| changed.witness() != stable.witness()),
            "a changed fact set must use a new checked snapshot and witness"
        );
        assert!(
            edit_facts
                .windows(2)
                .all(|pair| pair[0].witness() != pair[1].witness())
        );
        // Keep one selected root so edits exercise the facts-witness mismatch
        // path, rather than rebuilding only because the view root changed.
        let edit_root = view_state_root(&[("graph".to_owned(), "same-edit-root".to_owned())]);
        let edit_roots = vec![edit_root.clone(); WARMUPS + SAMPLES];
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
                .synchronize_checked_package_graph(edit_roots[sample], &edit_facts[sample])
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
            "package_graph edges={EDGES} reuse_median_ns={} reuse_p95_ns={} move_median_ns={} move_p95_ns={} edit_median_ns={} edit_p95_ns={}",
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
