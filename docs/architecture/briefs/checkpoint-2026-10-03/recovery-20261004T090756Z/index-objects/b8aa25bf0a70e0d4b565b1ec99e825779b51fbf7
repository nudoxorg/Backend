//! Republishing a view whose rows did not change, and republishing one edited row.
//!
//! Views are built outside the timer. The cold publish is outside the timer.
//! Each sample uses a new frontier, so the exact-root reuse path is not measured.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "the fixed benchmark fixture is intentionally fail-fast"
)]

use std::hint::black_box;
use std::time::Instant;

use backend_extension_turso::{ProjectionUpdate, TursoProjection};
use backend_library::{
    AuthorityScopeClaim, Basis, Coverage, CoverageCapability, Fragment, Frontier,
    ProducerObservationClaims, ProducerObservationVerifier, Row, RowId, ViewRoot,
    admit_complete_scope, admit_producer_observation, branch_key, log_key, object_version,
    package_key, symbol_key, view_key, view_state_root,
};

const ROWS: usize = 1024;
const SAMPLES: usize = 16;
const WARMUPS: usize = 4;

struct ExactObservation(backend_library::UntrustedProducerObservation);

impl ProducerObservationVerifier for ExactObservation {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &backend_library::UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        if observation != &self.0 {
            return Err("mismatch");
        }
        Ok(ProducerObservationClaims::new(
            self.0.producer_identity(),
            self.0.scope_root(),
            self.0.context(),
            *blake3::hash(self.0.evidence()).as_bytes(),
        ))
    }
}

fn capability(object: backend_library::SemanticObject) -> CoverageCapability {
    let declaration = AuthorityScopeClaim::from_object_version(object);
    let raw = backend_library::UntrustedProducerObservation::new(
        [7; 32],
        declaration.scope_root(),
        [8; 32],
        b"projection-bench".to_vec(),
    );
    let observation =
        admit_producer_observation(raw.clone(), &ExactObservation(raw)).expect("observation");
    let coverage = admit_complete_scope(declaration, observation).expect("coverage");
    CoverageCapability::from_authorized_with_evidence(coverage, b"projection-bench".to_vec())
        .expect("capability")
}

fn view(rows: Vec<Row>, sequence: u64) -> ViewRoot {
    let source = view_state_root(&[]);
    let object = object_version(b"projection-bench");
    let basis = Basis::with_context(source, object, branch_key("main"), log_key("library"), 1);
    ViewRoot::new_checked(
        view_key(b"projection-bench"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, sequence),
        rows,
        vec![Coverage::Complete],
        capability(object),
    )
    .expect("view")
}

fn rows(label_for_last: &str) -> Vec<Row> {
    let basis_source = view_state_root(&[]);
    let object = object_version(b"projection-bench");
    let basis = Basis::with_context(
        basis_source,
        object,
        branch_key("main"),
        log_key("library"),
        1,
    );
    let package = package_key("workspace");
    (0..ROWS)
        .map(|index| {
            let label = if index + 1 == ROWS {
                label_for_last.to_owned()
            } else {
                format!("symbol_{index:05}")
            };
            Row::in_package(
                RowId::Symbol(symbol_key(&format!("bench::symbol_{index:05}"))),
                basis,
                package,
                label,
            )
            .with_signature(format!("fn symbol_{index:05}()"))
            .with_document(vec![Fragment::Text(format!(
                "indexed package documentation token {index}"
            ))])
        })
        .collect()
}

fn percentile(samples: &mut [u128], rank: usize) -> u128 {
    samples.sort_unstable();
    samples[rank.min(samples.len().saturating_sub(1))]
}

fn main() {
    futures_executor::block_on(async {
        let path =
            std::env::temp_dir().join(format!("nudox-row-move-{}-{ROWS}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).expect("root");
        let database = path.join("projection.turso");
        let stable = rows("symbol_last");
        let cold = view(stable.clone(), 0);
        let mut projection = TursoProjection::open(&database).await.expect("open");
        let seeded = projection.synchronize(&cold).await.expect("cold");
        assert_eq!(seeded, ProjectionUpdate::Rebuilt { rows: ROWS as u64 });
        let moves = (0..WARMUPS + SAMPLES)
            .map(|index| view(stable.clone(), u64::try_from(index + 1).expect("sequence")))
            .collect::<Vec<_>>();
        let edits = (0..WARMUPS + SAMPLES)
            .map(|index| {
                view(
                    rows(&format!("edited-{index}")),
                    u64::try_from(1_000 + index).expect("sequence"),
                )
            })
            .collect::<Vec<_>>();
        let mut move_samples = [0_u128; SAMPLES];
        for sample in 0..(WARMUPS + SAMPLES) {
            let started = Instant::now();
            let update = projection.synchronize(&moves[sample]).await.expect("move");
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                black_box(update),
                ProjectionUpdate::Rebuilt { rows: ROWS as u64 }
            );
            if sample >= WARMUPS {
                move_samples[sample - WARMUPS] = elapsed;
            }
        }
        let mut edit_samples = [0_u128; SAMPLES];
        for sample in 0..(WARMUPS + SAMPLES) {
            let started = Instant::now();
            let update = projection.synchronize(&edits[sample]).await.expect("edit");
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                black_box(update),
                ProjectionUpdate::Rebuilt { rows: ROWS as u64 }
            );
            if sample >= WARMUPS {
                edit_samples[sample - WARMUPS] = elapsed;
            }
        }
        println!(
            "row_move rows={ROWS} move_median_ns={} move_p95_ns={} edit_median_ns={} edit_p95_ns={}",
            percentile(&mut move_samples, SAMPLES / 2),
            percentile(&mut move_samples, SAMPLES * 95 / 100),
            percentile(&mut edit_samples, SAMPLES / 2),
            percentile(&mut edit_samples, SAMPLES * 95 / 100),
        );
        drop(projection);
        let _ = std::fs::remove_dir_all(path);
    });
}
