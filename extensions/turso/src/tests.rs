//! Focused projection invariants and process-sharing stress coverage.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use backend_library::{
    AuthorityScopeClaim, ProducerObservationClaims, ProducerObservationVerifier,
};
use backend_library::{
    Basis, Coverage, CoverageCapability, Fragment, Frontier, Row, RowId, ViewDelta, ViewRoot,
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
use std::time::Instant;

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
        // A retried transport receipt is safe to replay after the target
        // fence has committed. The projection must not re-run its row
        // mutation or force a rebuild just because the base is now old.
        assert_eq!(
            projection
                .apply(&committed)
                .await
                .unwrap_or_else(|error| panic!("replay apply: {error}")),
            ProjectionUpdate::Reused { rows: 1 }
        );
        let found = projection
            .search("workspace", 10)
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
fn an_older_schema_is_rebuilt_and_a_newer_schema_is_refused() {
    futures_executor::block_on(async {
        let path = path();
        let package = package_key("workspace");
        let view = root(vec![Row::new(
            RowId::Package(package),
            root(Vec::new()).basis(),
            "workspace",
        )]);
        let stamp = |version: i64| {
            let path = path.clone();
            async move {
                let projection = TursoProjection::open_or_rebuild(&path)
                    .await
                    .unwrap_or_else(|error| panic!("open: {error}"));
                projection
                    .connection
                    .execute(
                        "UPDATE backend_projection_meta SET schema_version = ?1 WHERE singleton=1",
                        [version],
                    )
                    .await
                    .unwrap_or_else(|error| panic!("stamp schema: {error}"));
            }
        };

        let mut projection = TursoProjection::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        projection
            .synchronize(&view)
            .await
            .unwrap_or_else(|error| panic!("synchronize: {error}"));
        drop(projection);

        stamp(schema::SCHEMA_VERSION - 1).await;
        assert!(matches!(
            TursoProjection::open(&path).await,
            Err(ProjectionError::Schema { .. })
        ));
        let mut rebuilt = TursoProjection::open_or_rebuild(&path)
            .await
            .unwrap_or_else(|error| panic!("rebuild older schema: {error}"));
        assert_eq!(
            rebuilt
                .synchronize(&view)
                .await
                .unwrap_or_else(|error| panic!("repopulate: {error}")),
            ProjectionUpdate::Rebuilt { rows: 1 }
        );
        drop(rebuilt);

        stamp(schema::SCHEMA_VERSION + 1).await;
        assert!(matches!(
            TursoProjection::open_or_rebuild(&path).await,
            Err(ProjectionError::Schema { .. })
        ));
        assert!(path.exists(), "a newer projection must never be discarded");
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
        let mut replayed = 0;
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
                // A writer that reads the fence after the winner committed
                // sees its own target already published: a replay, not a race.
                Ok(ProjectionUpdate::Reused { rows: 1 }) => replayed += 1,
                other => panic!("unexpected overlapping writer outcome: {other:?}"),
            }
        }
        assert_eq!(advanced, 1, "exactly one writer may publish the base delta");
        assert_eq!(
            stale + replayed,
            WRITERS - 1,
            "losers must be typed stale transitions or replay no-ops"
        );

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

#[test]
#[ignore = "bounded Turso FTS projection stress probe"]
fn stress_fts_projection_reports_build_query_and_delta_costs() {
    futures_executor::block_on(async {
        const ROWS: usize = 20_000;
        let path = path();
        let empty = root(Vec::new());
        let package = package_key("stress");
        let rows = (0..ROWS)
            .map(|index| {
                let marker = if index == ROWS - 1 {
                    " singular-needle"
                } else {
                    ""
                };
                Row::in_package(
                    RowId::Symbol(backend_library::symbol_key(&format!(
                        "stress::symbol_{index:05}"
                    ))),
                    empty.basis(),
                    package,
                    format!("symbol_{index:05}"),
                )
                .with_signature(format!("fn symbol_{index:05}()"))
                .with_document(vec![Fragment::Text(format!(
                    "indexed package documentation common-token{marker}"
                ))])
            })
            .collect::<Vec<_>>();
        let view = root(rows);
        let mut projection = TursoProjection::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let started = Instant::now();
        let update = projection
            .synchronize(&view)
            .await
            .unwrap_or_else(|error| panic!("synchronize: {error}"));
        let build_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(update, ProjectionUpdate::Rebuilt { rows: ROWS as u64 });

        let started = Instant::now();
        let rare = projection
            .search("singular needle", 10)
            .await
            .unwrap_or_else(|error| panic!("rare search: {error}"));
        let rare_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(rare.ids.len(), 1);

        let started = Instant::now();
        let common = projection
            .search("common token", 25)
            .await
            .unwrap_or_else(|error| panic!("common search: {error}"));
        let common_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(common.ids.len(), 25);

        let replacement = Row::in_package(
            RowId::Symbol(backend_library::symbol_key("stress::symbol_10000")),
            view.basis(),
            package,
            "symbol_10000",
        )
        .with_document(vec![Fragment::Text("changed edge".to_owned())]);
        let prepared = view
            .prepare(
                ViewDelta::Upsert { row: replacement },
                capability(view.basis().object),
            )
            .unwrap_or_else(|error| panic!("prepare: {error:?}"));
        let (_, committed) = view
            .commit(prepared)
            .unwrap_or_else(|error| panic!("commit: {error:?}"));
        let started = Instant::now();
        let update = projection
            .apply(&committed)
            .await
            .unwrap_or_else(|error| panic!("apply: {error}"));
        let delta_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(update, ProjectionUpdate::Advanced { changed_rows: 1 });

        let bytes = std::fs::metadata(&path)
            .unwrap_or_else(|error| panic!("metadata: {error}"))
            .len();
        eprintln!(
            "turso_fts_stress rows={ROWS} build_ms={build_ms:.2} rare_ms={rare_ms:.3} common_ms={common_ms:.3} one_row_delta_ms={delta_ms:.3} database_bytes={bytes}"
        );
        drop(projection);
        for suffix in ["", "-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
            let _ = std::fs::remove_file(sidecar);
        }
    });
}

#[test]
fn rebuild_keeps_an_unchanged_rowid_and_records_only_real_mutations() {
    futures_executor::block_on(async {
        let path = path();
        let basis = root(Vec::new()).basis();
        let keep = Row::new(RowId::Package(package_key("keep")), basis, "alpha");
        let gone = Row::new(RowId::Package(package_key("gone")), basis, "beta");
        let edit = Row::new(RowId::Package(package_key("edit")), basis, "gamma");
        let first = root(vec![keep.clone(), gone, edit]);
        let revised = root(vec![
            keep,
            Row::new(RowId::Package(package_key("edit")), basis, "gamma-two"),
        ]);
        let keep_key = RowId::Package(package_key("keep")).stable_key();

        let mut projection = TursoProjection::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        assert_eq!(
            projection
                .synchronize(&first)
                .await
                .unwrap_or_else(|error| panic!("first synchronize: {error}")),
            ProjectionUpdate::Rebuilt { rows: 3 }
        );
        let keep_rowid = stored_rowid(&projection, &keep_key).await;
        assert_eq!(recorded_changes(&projection, &first).await, 3);

        assert_eq!(
            projection
                .synchronize(&revised)
                .await
                .unwrap_or_else(|error| panic!("revised synchronize: {error}")),
            ProjectionUpdate::Rebuilt { rows: 2 }
        );
        assert_eq!(stored_rowid(&projection, &keep_key).await, keep_rowid);
        assert_eq!(recorded_changes(&projection, &revised).await, 2);

        let alpha = projection
            .search("alpha", 10)
            .await
            .unwrap_or_else(|error| panic!("search alpha: {error}"));
        assert_eq!(alpha.root.as_ref(), revised.root().as_bytes());
        assert_eq!(alpha.ids.as_ref(), &[keep_key]);
        let edited = projection
            .search("gamma-two", 10)
            .await
            .unwrap_or_else(|error| panic!("search edit: {error}"));
        assert_eq!(
            edited.ids.as_ref(),
            &[RowId::Package(package_key("edit")).stable_key()]
        );
        let removed = projection
            .search("beta", 10)
            .await
            .unwrap_or_else(|error| panic!("search removed: {error}"));
        assert!(removed.ids.is_empty());

        drop(projection);
        std::fs::remove_file(&path).unwrap_or_else(|error| panic!("remove projection: {error}"));
    });
}

async fn stored_rowid(projection: &TursoProjection, key: &str) -> i64 {
    let mut rows = projection
        .connection
        .query(
            "SELECT rowid FROM backend_projection_rows WHERE row_id = ?1",
            [key],
        )
        .await
        .unwrap_or_else(|error| panic!("rowid query: {error}"));
    let row = rows
        .next()
        .await
        .unwrap_or_else(|error| panic!("rowid next: {error}"))
        .unwrap_or_else(|| panic!("missing row {key}"));
    row.get(0)
        .unwrap_or_else(|error| panic!("rowid decode: {error}"))
}

async fn recorded_changes(projection: &TursoProjection, view: &ViewRoot) -> i64 {
    let mut rows = projection
        .connection
        .query(
            "SELECT changed_rows FROM backend_projection_commits WHERE root = ?1",
            turso::params![view.root().as_bytes().as_slice()],
        )
        .await
        .unwrap_or_else(|error| panic!("commit query: {error}"));
    let row = rows
        .next()
        .await
        .unwrap_or_else(|error| panic!("commit next: {error}"))
        .unwrap_or_else(|| panic!("missing commit"));
    row.get(0)
        .unwrap_or_else(|error| panic!("commit decode: {error}"))
}
