//! Focused projection invariants and process-sharing stress coverage.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use backend_library::{
    AuthorityScopeClaim, ProducerObservationClaims, ProducerObservationVerifier,
};
use backend_library::{
    Basis, Coverage, CoverageCapability, Frontier, Row, RowId, ViewDelta, ViewRoot,
    admit_complete_scope, admit_producer_observation, branch_key, log_key, object_version,
    package_key, view_key, view_state_root,
};
use backend_library::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencyTarget, PackageReference, ProductText,
    RegistryEcosystem,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;

static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

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
        b"projection-test".to_vec(),
    );
    let observation = admit_producer_observation(raw.clone(), &ExactObservation(raw))
        .unwrap_or_else(|error| panic!("observation: {error}"));
    let coverage = admit_complete_scope(declaration, observation)
        .unwrap_or_else(|error| panic!("coverage: {error}"));
    CoverageCapability::from_authorized_with_evidence(coverage, b"projection-test".to_vec())
        .unwrap_or_else(|error| panic!("capability: {error}"))
}

fn root(rows: Vec<Row>) -> ViewRoot {
    let source = view_state_root(&[]);
    let object = object_version(b"projection-test");
    let basis = Basis::with_context(source, object, branch_key("main"), log_key("library"), 1);
    ViewRoot::new_checked(
        view_key(b"projection-test"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        rows,
        vec![Coverage::Complete],
        capability(object),
    )
    .unwrap_or_else(|error| panic!("root: {error:?}"))
}

fn path() -> PathBuf {
    let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "backend-turso-projection-{}-{serial}.db",
        std::process::id()
    ))
}

#[test]
fn exact_root_reuses_database_and_hot_delta_changes_one_row() {
    futures_executor::block_on(async {
        let path = path();
        let empty = root(Vec::new());
        let package = package_key("workspace");
        let row = Row::new(RowId::Package(package), empty.basis(), "workspace");
        let prepared = empty
            .prepare(ViewDelta::Upsert { row }, capability(empty.basis().object))
            .unwrap_or_else(|error| panic!("prepare: {error:?}"));
        let (next, committed) = empty
            .clone()
            .commit(prepared)
            .unwrap_or_else(|error| panic!("commit: {error:?}"));

        let mut projection = TursoProjection::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        assert_eq!(
            projection
                .synchronize(&empty)
                .await
                .unwrap_or_else(|error| panic!("initial synchronize: {error}")),
            ProjectionUpdate::Rebuilt { rows: 0 }
        );
        assert_eq!(
            projection
                .synchronize(&empty)
                .await
                .unwrap_or_else(|error| panic!("reuse synchronize: {error}")),
            ProjectionUpdate::Reused { rows: 0 }
        );
        assert_eq!(
            projection
                .apply(&committed)
                .await
                .unwrap_or_else(|error| panic!("apply: {error}")),
            ProjectionUpdate::Advanced { changed_rows: 1 }
        );
        let found = projection
            .search("work", 10)
            .await
            .unwrap_or_else(|error| panic!("search: {error}"));
        assert_eq!(found.root.as_ref(), next.root().as_bytes());
        assert_eq!(found.ids.as_ref(), &[RowId::Package(package).stable_key()]);
        drop(projection);

        let mut reopened = TursoProjection::open(&path)
            .await
            .unwrap_or_else(|error| panic!("reopen: {error}"));
        assert_eq!(
            reopened
                .synchronize(&next)
                .await
                .unwrap_or_else(|error| panic!("reopen synchronize: {error}")),
            ProjectionUpdate::Reused { rows: 1 }
        );
        std::fs::remove_file(&path).unwrap_or_else(|error| panic!("remove projection: {error}"));
    });
}

#[test]
fn multiprocess_wal_stress_serializes_writers_and_preserves_read_snapshots() {
    futures_executor::block_on(async {
        const READERS: usize = 12;
        const WRITERS: usize = 4;

        let path = path();
        let base = root(Vec::new());
        let package = package_key("workspace");
        let first_row = Row::new(RowId::Package(package), base.basis(), "workspace");
        let first_prepared = base
            .prepare(
                ViewDelta::Upsert { row: first_row },
                capability(base.basis().object),
            )
            .unwrap_or_else(|error| panic!("prepare first delta: {error:?}"));
        let (first_view, first_delta) = base
            .clone()
            .commit(first_prepared)
            .unwrap_or_else(|error| panic!("commit first delta: {error:?}"));
        let second_row = Row::new(
            RowId::Package(package_key("workspace-v2")),
            first_view.basis(),
            "workspace-v2",
        );
        let second_prepared = first_view
            .prepare(
                ViewDelta::Upsert { row: second_row },
                capability(first_view.basis().object),
            )
            .unwrap_or_else(|error| panic!("prepare second delta: {error:?}"));
        let (second_view, second_delta) = first_view
            .clone()
            .commit(second_prepared)
            .unwrap_or_else(|error| panic!("commit second delta: {error:?}"));

        let mut initial = TursoProjection::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open initial projection: {error}"));
        assert_eq!(
            initial
                .synchronize(&base)
                .await
                .unwrap_or_else(|error| panic!("synchronize base: {error}")),
            ProjectionUpdate::Rebuilt { rows: 0 }
        );
        drop(initial);

        let barrier = Arc::new(Barrier::new(READERS + WRITERS));
        let (reader_sender, reader_receiver) = mpsc::channel();
        let (writer_sender, writer_receiver) = mpsc::channel();
        let mut handles = Vec::with_capacity(READERS + WRITERS);

        for _ in 0..READERS {
            let barrier = Arc::clone(&barrier);
            let sender = reader_sender.clone();
            let path = path.clone();
            let base = base.clone();
            handles.push(thread::spawn(move || {
                let prepared = futures_executor::block_on(async {
                    let mut projection = TursoProjection::open(&path).await?;
                    let update = projection.synchronize(&base).await?;
                    if !matches!(update, ProjectionUpdate::Reused { .. }) {
                        return Err(ProjectionError::StaleTransition);
                    }
                    Ok(projection)
                });
                barrier.wait();
                let result = match prepared {
                    Ok(projection) => futures_executor::block_on(projection.search("workspace", 8)),
                    Err(error) => Err(error),
                };
                sender.send(result).expect("reader result receiver");
            }));
        }

        for _ in 0..WRITERS {
            let barrier = Arc::clone(&barrier);
            let sender = writer_sender.clone();
            let path = path.clone();
            let base = base.clone();
            let first_delta = first_delta.clone();
            handles.push(thread::spawn(move || {
                let prepared = futures_executor::block_on(async {
                    let mut projection = TursoProjection::open(&path).await?;
                    let update = projection.synchronize(&base).await?;
                    if !matches!(update, ProjectionUpdate::Reused { .. }) {
                        return Err(ProjectionError::StaleTransition);
                    }
                    Ok(projection)
                });
                barrier.wait();
                let result = match prepared {
                    Ok(mut projection) => {
                        futures_executor::block_on(projection.apply(&first_delta))
                    }
                    Err(error) => Err(error),
                };
                sender.send(result).expect("writer result receiver");
            }));
        }
        drop(reader_sender);
        drop(writer_sender);

        for handle in handles {
            handle.join().expect("stress worker should not panic");
        }

        for _ in 0..READERS {
            let rows = reader_receiver
                .recv()
                .unwrap_or_else(|error| panic!("reader result: {error}"))
                .unwrap_or_else(|error| panic!("reader failed: {error}"));
            let is_base = rows.root.as_ref() == base.root().as_bytes();
            let is_first = rows.root.as_ref() == first_view.root().as_bytes();
            assert!(is_base || is_first, "reader crossed a committed root fence");
            assert_eq!(rows.ids.len(), usize::from(is_first));
        }

        let mut advanced = 0;
        let mut stale = 0;
        for _ in 0..WRITERS {
            match writer_receiver
                .recv()
                .unwrap_or_else(|error| panic!("writer result: {error}"))
            {
                Ok(ProjectionUpdate::Advanced { changed_rows }) => {
                    assert_eq!(changed_rows, 1);
                    advanced += 1;
                }
                Err(ProjectionError::StaleTransition) => stale += 1,
                other => panic!("unexpected overlapping writer outcome: {other:?}"),
            }
        }
        assert_eq!(advanced, 1, "exactly one writer may publish the base delta");
        assert_eq!(stale, WRITERS - 1, "losers must be typed stale transitions");

        let mut restarted = TursoProjection::open(&path)
            .await
            .unwrap_or_else(|error| panic!("reopen projection: {error}"));
        assert_eq!(
            restarted
                .synchronize(&first_view)
                .await
                .unwrap_or_else(|error| panic!("restart recovery: {error}")),
            ProjectionUpdate::Reused { rows: 1 }
        );
        assert_eq!(
            restarted
                .apply(&second_delta)
                .await
                .unwrap_or_else(|error| panic!("publish second delta: {error}")),
            ProjectionUpdate::Advanced { changed_rows: 1 }
        );
        assert_eq!(
            restarted
                .synchronize(&second_view)
                .await
                .unwrap_or_else(|error| panic!("second exact-root no-op: {error}")),
            ProjectionUpdate::Reused { rows: 2 }
        );
        assert_eq!(
            restarted
                .apply_all(&[])
                .await
                .unwrap_or_else(|error| panic!("empty delta no-op: {error}")),
            ProjectionUpdate::Reused { rows: 2 }
        );
        let rows = restarted
            .search("workspace", 8)
            .await
            .unwrap_or_else(|error| panic!("post-restart search: {error}"));
        assert_eq!(rows.root.as_ref(), second_view.root().as_bytes());
        assert_eq!(rows.ids.len(), 2);

        for suffix in ["", "-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
            let _ = std::fs::remove_file(sidecar);
        }
    });
}

#[test]
fn package_graph_reuses_root_and_answers_forward_and_reverse_edges() {
    futures_executor::block_on(async {
        let path = path();
        let view = root(Vec::new());
        let source = PackageReference::parse("pkg:cargo/app@1.0.0").expect("source");
        let target = PackageReference::parse("pkg:cargo/serde@1.0.0").expect("target");
        let edge = PackageDependencyRecord::new(
            source.clone(),
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "^1", None)
                .expect("target facts"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [1; 32],
                provenance: [2; 32],
            },
        );
        let facts = vec![(
            source.clone(),
            DependencyFacts::Known(vec![edge.clone()].into_boxed_slice()),
        )];
        let mut projection = TursoProjection::open(&path).await.expect("open");
        assert_eq!(
            projection
                .synchronize_package_graph(view.root(), &facts)
                .await
                .expect("project graph"),
            ProjectionUpdate::Rebuilt { rows: 1 }
        );
        assert_eq!(
            projection
                .synchronize_package_graph(view.root(), &facts)
                .await
                .expect("reuse graph"),
            ProjectionUpdate::Reused { rows: 1 }
        );
        let forward = projection
            .package_dependencies(&source)
            .await
            .expect("forward");
        assert_eq!(forward.edges.as_ref(), &[edge]);
        let reverse = projection
            .package_dependents(&target)
            .await
            .expect("reverse");
        assert_eq!(reverse.edges.len(), 1);
        assert_eq!(reverse.edges[0].source, source);
        let unknown = vec![(
            target.clone(),
            DependencyFacts::Unavailable(ProductText::new("metadata timeout").expect("reason")),
        )];
        let next = view_state_root(&[("graph".to_owned(), "next".to_owned())]);
        projection
            .synchronize_package_graph(next, &unknown)
            .await
            .expect("project unavailable");
        let state = projection
            .package_dependencies(&target)
            .await
            .expect("unavailable state")
            .state
            .expect("state row");
        assert_eq!(state.kind, 2);
        std::fs::remove_file(&path).expect("remove projection");
    });
}
