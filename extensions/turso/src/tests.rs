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
    CheckedPackageGraphFacts, DependencyAuthority, DependencyEvidence, DependencyFacts,
    DependencyScope, MAX_PACKAGE_GRAPH_ROWS, PackageDependencyRecord, PackageDependencySourceFacts,
    PackageDependencyTarget, PackageGraphSourceAuthority, PackageGraphSourceKey, PackageReference,
    ProductText, RegistryAuthorityId, RegistryEcosystem, package_dependency_facts_witness,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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

pub(crate) fn capability(object: backend_library::SemanticObject) -> CoverageCapability {
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

pub(crate) fn root(rows: Vec<Row>) -> ViewRoot {
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

pub(crate) fn replacement_view() -> ViewRoot {
    view_with_label("generation-target")
}

pub(crate) fn view_with_label(label: &str) -> ViewRoot {
    let base = fallback_seed_view();
    let row = Row::new(
        RowId::Package(package_key(label)),
        base.basis(),
        label,
    );
    let prepared = base
        .prepare(ViewDelta::Upsert { row }, capability(base.basis().object))
        .unwrap_or_else(|error| panic!("prepare replacement view: {error:?}"));
    base.commit(prepared)
        .unwrap_or_else(|error| panic!("commit replacement view: {error:?}"))
        .0
}

fn path() -> PathBuf {
    let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "backend-turso-projection-{}-{serial}.db",
        std::process::id()
    ))
}

pub(crate) fn fallback_seed_view() -> ViewRoot {
    let source = view_state_root(&[]);
    let object = object_version(b"projection-test-open-seed");
    let basis = Basis::with_context(
        source,
        object,
        branch_key("projection-seed"),
        log_key("projection-seed"),
        1,
    );
    ViewRoot::new_checked(
        view_key(b"projection-test-open-seed"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        Vec::new(),
        vec![Coverage::Complete],
        capability(object),
    )
    .unwrap_or_else(|error| panic!("fallback seed root: {error:?}"))
}

pub(crate) async fn open_test(path: &std::path::Path) -> Result<TursoProjection, ProjectionError> {
    match TursoProjection::open(path).await {
        Err(ProjectionError::NeedsSeed) => {
            let view = fallback_seed_view();
            TursoProjection::seed_if_empty(
                path,
                ProjectionSeed::new(&view, ProjectionGraphSeed::Unavailable),
            )
            .await
        }
        result => result,
    }
}

impl TursoProjection {
    async fn synchronize_from_current_for_test(
        &mut self,
        view: &ViewRoot,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        let revision = self.revision().await?;
        self.synchronize_from(revision, view).await
    }

    async fn synchronize_package_graph_current_for_test(
        &mut self,
        root: backend_library::ViewStateRoot,
        facts: &[PackageDependencySourceFacts],
    ) -> Result<ProjectionUpdate, ProjectionError> {
        let revision = self.package_graph_revision().await?;
        self.synchronize_package_graph_from(revision, root, facts)
            .await
    }

    async fn synchronize_checked_package_graph_current_for_test(
        &mut self,
        root: backend_library::ViewStateRoot,
        facts: &CheckedPackageGraphFacts,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        let revision = self.package_graph_revision().await?;
        self.synchronize_checked_package_graph_from(revision, root, facts)
            .await
    }
}

pub(crate) async fn synchronize_graph_from_current_for_test(
    projection: &mut TursoProjection,
    root: backend_library::ViewStateRoot,
    facts: &[PackageDependencySourceFacts],
) -> Result<ProjectionUpdate, ProjectionError> {
    projection
        .synchronize_package_graph_current_for_test(root, facts)
        .await
}

pub(crate) async fn synchronize_checked_graph_from_current_for_test(
    projection: &mut TursoProjection,
    root: backend_library::ViewStateRoot,
    facts: &CheckedPackageGraphFacts,
) -> Result<ProjectionUpdate, ProjectionError> {
    projection
        .synchronize_checked_package_graph_current_for_test(root, facts)
        .await
}

fn remove_database(path: &std::path::Path) {
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        let namespace = path.with_file_name(format!("{name}.namespace-v1"));
        let _ = std::fs::remove_dir_all(namespace);
    }
    for suffix in ["", "-wal", "-shm", "-tshm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
        let _ = std::fs::remove_file(sidecar);
    }
}

async fn assert_projection_integrity_without_fts(projection: &TursoProjection) {
    let mut indexes = projection
        .connection
        .query(
            "SELECT name FROM sqlite_master \
             WHERE type='index' AND tbl_name='backend_projection_rows'",
            (),
        )
        .await
        .unwrap_or_else(|error| panic!("projection index query: {error}"));
    let mut names = Vec::new();
    while let Some(row) = indexes
        .next()
        .await
        .unwrap_or_else(|error| panic!("projection index next: {error}"))
    {
        names.push(
            row.get::<String>(0)
                .unwrap_or_else(|error| panic!("projection index name: {error}")),
        );
    }
    assert!(
        names
            .iter()
            .any(|name| name == "backend_projection_rows_label")
    );
    assert!(!names.iter().any(|name| name.contains("fts")));

    let mut integrity = projection
        .connection
        .query("PRAGMA integrity_check", ())
        .await
        .unwrap_or_else(|error| panic!("projection integrity check: {error}"));
    let result = integrity
        .next()
        .await
        .unwrap_or_else(|error| panic!("projection integrity result: {error}"))
        .unwrap_or_else(|| panic!("projection integrity check returned no result"))
        .get::<String>(0)
        .unwrap_or_else(|error| panic!("projection integrity text: {error}"));
    assert_eq!(result, "ok");
}

#[test]
fn fresh_open_requires_a_complete_seed_and_cold_reopen_uses_the_selected_generation() {
    futures_executor::block_on(async {
        let path = path();
        let namespace_path = path.with_file_name(format!(
            "{}.namespace-v1",
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("path name")
        ));
        assert!(matches!(
            TursoProjection::open(&path).await,
            Err(ProjectionError::NeedsSeed)
        ));
        assert!(
            !namespace_path.exists(),
            "selected-only open must not create namespace state"
        );

        let view = replacement_view();
        let facts = CheckedPackageGraphFacts::new(Vec::new()).expect("checked empty graph");
        let seed = ProjectionSeed::new(&view, ProjectionGraphSeed::Checked(&facts));
        let seeded = TursoProjection::seed_if_empty(&path, seed)
            .await
            .expect("seed generation");
        let generation = seeded.generation;
        let selected = seeded
            .namespace
            .selected()
            .expect("selector")
            .expect("selected");
        assert_eq!(selected.generation, generation);
        assert_eq!(selected.seed_view_root, *view.root().as_bytes());
        assert_eq!(selected.seed_view_version, *view.version().as_bytes());
        assert_eq!(selected.graph_state, 1);
        assert_eq!(selected.graph_witness, facts.witness());
        assert_eq!(
            seeded
                .namespace
                .generation_count_for_test()
                .expect("bounded generation count"),
            1
        );
        let unrelated_seed = fallback_seed_view();
        assert!(matches!(
            TursoProjection::seed_if_empty(
                &path,
                ProjectionSeed::new(&unrelated_seed, ProjectionGraphSeed::Unavailable),
            )
            .await,
            Err(ProjectionError::AlreadySeeded)
        ));
        assert_eq!(
            seeded
                .namespace
                .selected()
                .expect("seed selection unchanged"),
            Some(selected),
            "an existing generation cannot be mistaken for an accepted replacement seed"
        );
        let rooted = seeded
            .lookup_label("generation-target", 4)
            .await
            .expect("seeded row");
        assert_eq!(rooted.root, *view.root().as_bytes());
        drop(seeded);

        let reopened = TursoProjection::open(&path)
            .await
            .expect("selected cold reopen");
        assert_eq!(reopened.generation, generation);
        assert_eq!(
            reopened
                .lookup_label("generation-target", 4)
                .await
                .expect("reopened row")
                .root,
            *view.root().as_bytes()
        );
        drop(reopened);
        remove_database(&path);
    });
}

#[test]
fn unsupported_selected_schema_is_refused_without_replacing_its_generation() {
    futures_executor::block_on(async {
        let path = path();
        let old_seed_view = fallback_seed_view();
        let old = TursoProjection::seed_if_empty(
            &path,
            ProjectionSeed::new(&old_seed_view, ProjectionGraphSeed::Unavailable),
        )
        .await
        .expect("initial generation");
        old.connection
            .execute(
                "UPDATE backend_projection_meta SET schema_version=?1 WHERE singleton=1",
                [schema::SCHEMA_VERSION - 1],
            )
            .await
            .expect("mark selected projection old");
        let namespace = super::projection_namespace::ProjectionNamespace::open_existing(&path)
            .expect("namespace");
        namespace
            .set_selected_schema_for_test(schema::SCHEMA_VERSION - 1)
            .expect("mark selector old");
        let selected_before = namespace.selected().expect("old selector");
        let generations_before = namespace
            .generation_count_for_test()
            .expect("generation inventory before refusal");

        let new_seed_view = replacement_view();
        assert!(matches!(
            TursoProjection::seed_if_empty(
                &path,
                ProjectionSeed::new(&new_seed_view, ProjectionGraphSeed::Unavailable),
            )
            .await,
            Err(ProjectionError::AlreadySeeded)
        ));
        assert!(matches!(
            TursoProjection::open(&path).await,
            Err(ProjectionError::Schema { found }) if found == schema::SCHEMA_VERSION - 1
        ));
        assert_eq!(
            namespace.selected().expect("selector remains old"),
            selected_before,
            "a seed cannot implicitly replace an unsupported selected generation"
        );
        assert_eq!(
            namespace
                .generation_count_for_test()
                .expect("generation inventory after refusal"),
            generations_before,
            "refusal does not leave an unselected replacement stage"
        );
        assert!(matches!(
            old.lookup_label("generation-target", 4).await,
            Err(ProjectionError::SupersededGeneration)
        ));
        drop(old);
        assert!(matches!(
            TursoProjection::open(&path).await,
            Err(ProjectionError::Schema { found }) if found == schema::SCHEMA_VERSION - 1
        ));
        remove_database(&path);
    });
}

#[test]
fn missing_selected_database_is_refused_without_creating_an_empty_replacement() {
    futures_executor::block_on(async {
        let path = path();
        let seed_view = fallback_seed_view();
        let seeded = TursoProjection::seed_if_empty(
            &path,
            ProjectionSeed::new(&seed_view, ProjectionGraphSeed::Unavailable),
        )
        .await
        .expect("seed initial selected generation");
        let selected_database = seeded
            .namespace
            .database_path(seeded.generation)
            .expect("selected database path");
        drop(seeded);

        std::fs::remove_file(&selected_database).expect("simulate missing selected database");
        assert!(matches!(
            TursoProjection::open(&path).await,
            Err(ProjectionError::CorruptNamespace {
                field: "missing_projection_database"
            })
        ));
        assert!(
            !selected_database.exists(),
            "selected-only open must not create a writable empty database"
        );
        remove_database(&path);
    });
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

        let mut projection = open_test(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        assert_eq!(
            projection
                .synchronize_from_current_for_test(&empty)
                .await
                .unwrap_or_else(|error| panic!("initial synchronize: {error}")),
            ProjectionUpdate::Rebuilt { rows: 0 }
        );
        assert_eq!(
            projection
                .synchronize_from_current_for_test(&empty)
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
            .lookup_label("workspace", 10)
            .await
            .unwrap_or_else(|error| panic!("lookup label: {error}"));
        assert_eq!(found.root.as_ref(), next.root().as_bytes());
        assert_eq!(found.ids.as_ref(), &[RowId::Package(package).stable_key()]);
        assert!(matches!(
            projection
                .lookup_label("workspace", MAX_LABEL_QUERY_ROWS + 1)
                .await,
            Err(ProjectionError::ReadLimitExceeded { maximum })
                if maximum == MAX_LABEL_QUERY_ROWS as usize
        ));
        assert_projection_integrity_without_fts(&projection).await;
        let prefix = projection
            .lookup_label("work", 10)
            .await
            .unwrap_or_else(|error| panic!("exact label lookup: {error}"));
        assert!(prefix.ids.is_empty(), "labels are matched exactly");
        drop(projection);

        let mut reopened = open_test(&path)
            .await
            .unwrap_or_else(|error| panic!("reopen: {error}"));
        assert_eq!(
            reopened
                .synchronize_from_current_for_test(&next)
                .await
                .unwrap_or_else(|error| panic!("reopen synchronize: {error}")),
            ProjectionUpdate::Reused { rows: 1 }
        );
        drop(reopened);
        remove_database(&path);
    });
}

#[test]
fn delayed_complete_view_cannot_roll_back_a_newer_projection_revision() {
    futures_executor::block_on(async {
        let path = path();
        let old_view = fallback_seed_view();
        let new_view = replacement_view();
        let seed = TursoProjection::seed_if_empty(
            &path,
            ProjectionSeed::new(&old_view, ProjectionGraphSeed::Unavailable),
        )
        .await
        .expect("seed initial selected root");
        drop(seed);

        let mut delayed = TursoProjection::open(&path)
            .await
            .expect("open delayed writer");
        let delayed_revision = delayed
            .revision()
            .await
            .expect("capture before source read");
        let mut current = TursoProjection::open(&path)
            .await
            .expect("open current writer");
        let current_revision = current.revision().await.expect("capture current base");
        current
            .synchronize_from(current_revision, &new_view)
            .await
            .expect("publish newer complete root");

        assert!(matches!(
            delayed.synchronize_from(delayed_revision, &old_view).await,
            Err(ProjectionError::StaleTransition)
        ));
        assert!(matches!(
            delayed.synchronize_from(delayed_revision, &new_view).await,
            Err(ProjectionError::StaleTransition)
        ));
        assert!(matches!(
            delayed.synchronize(&old_view).await,
            Err(ProjectionError::StaleTransition)
        ));
        drop(delayed);
        drop(current);

        let reopened = TursoProjection::open(&path).await.expect("cold reopen");
        let rows = reopened
            .lookup_label("generation-target", 4)
            .await
            .expect("newer root survives cold reopen");
        assert_eq!(rows.root, *new_view.root().as_bytes());
        assert_eq!(rows.ids.len(), 1);
        drop(reopened);
        remove_database(&path);
    });
}

#[test]
fn delayed_complete_view_rejects_exact_target_published_in_pretransaction_gap() {
    futures_executor::block_on(async {
        let path = path();
        let initial = fallback_seed_view();
        let target = replacement_view();
        let seed = TursoProjection::seed_if_empty(
            &path,
            ProjectionSeed::new(&initial, ProjectionGraphSeed::Unavailable),
        )
        .await
        .expect("seed initial selected root");
        drop(seed);

        let mut winner = TursoProjection::open(&path)
            .await
            .expect("open competing publisher");
        let winner_revision = winner.revision().await.expect("capture winner base");
        let worker_path = path.clone();
        let worker_target = target.clone();
        let (at_transaction_sender, at_transaction_receiver) = mpsc::channel();
        let (resume_sender, resume_receiver) = mpsc::channel();
        let delayed = thread::spawn(move || {
            let mut delayed = futures_executor::block_on(TursoProjection::open(&worker_path))
                .expect("open delayed writer");
            let revision = futures_executor::block_on(delayed.revision())
                .expect("capture before source read");
            crate::writer::set_before_projection_writer_tx_test_hook(move || {
                at_transaction_sender
                    .send(())
                    .expect("report pre-transaction barrier");
                resume_receiver
                    .recv()
                    .expect("release pre-transaction barrier");
            });
            futures_executor::block_on(delayed.synchronize_from(revision, &worker_target))
        });

        at_transaction_receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("delayed writer reached the transaction boundary");
        winner
            .synchronize_from(winner_revision, &target)
            .await
            .expect("commit exact target while delayed writer is paused");
        resume_sender.send(()).expect("resume delayed writer");
        assert!(matches!(
            delayed.join().expect("join delayed writer"),
            Err(ProjectionError::StaleTransition)
        ));
        drop(winner);

        let reopened = TursoProjection::open(&path).await.expect("cold reopen");
        let selected = reopened
            .lookup_label("generation-target", 4)
            .await
            .expect("selected target survives stale replay");
        assert_eq!(selected.root, *target.root().as_bytes());
        assert_eq!(selected.ids.len(), 1);
        drop(reopened);
        remove_database(&path);
    });
}

#[test]
fn reset_rebuild_obeys_its_committed_base_and_rejects_a_stale_reset() {
    futures_executor::block_on(async {
        let path = path();
        let base = fallback_seed_view();
        let make_target = |label: &'static str| {
            let row = Row::new(RowId::Package(package_key(label)), base.basis(), label);
            let prepared = base
                .prepare(ViewDelta::Upsert { row }, capability(base.basis().object))
                .expect("prepare target row");
            base.clone().commit(prepared).expect("commit target row").0
        };
        let make_reset = |target: ViewRoot| {
            let prepared = base
                .prepare(
                    ViewDelta::Reset {
                        root: Box::new(target),
                    },
                    capability(base.basis().object),
                )
                .expect("prepare checked reset");
            base.clone().commit(prepared).expect("commit checked reset")
        };
        let (committed_view, committed_reset) = make_reset(make_target("reset-committed"));
        let (_stale_view, stale_reset) = make_reset(make_target("reset-stale"));
        let seed = TursoProjection::seed_if_empty(
            &path,
            ProjectionSeed::new(&base, ProjectionGraphSeed::Unavailable),
        )
        .await
        .expect("seed reset base");
        drop(seed);

        let mut winner = TursoProjection::open(&path)
            .await
            .expect("open reset winner");
        let mut stale = TursoProjection::open(&path)
            .await
            .expect("open stale reset writer");
        assert_eq!(
            winner
                .apply(&committed_reset)
                .await
                .expect("valid reset applies"),
            ProjectionUpdate::Rebuilt { rows: 1 }
        );
        assert!(matches!(
            winner.apply(&committed_reset).await,
            Err(ProjectionError::StaleTransition)
        ));
        assert!(matches!(
            stale.apply(&stale_reset).await,
            Err(ProjectionError::StaleTransition)
        ));
        drop(winner);
        drop(stale);

        let reopened = TursoProjection::open(&path)
            .await
            .expect("cold reopen after reset");
        let rows = reopened
            .lookup_label("reset-committed", 4)
            .await
            .expect("committed reset remains selected");
        assert_eq!(rows.root, *committed_view.root().as_bytes());
        assert_eq!(rows.ids.len(), 1);
        let stale_rows = reopened
            .lookup_label("reset-stale", 4)
            .await
            .expect("stale reset row stays absent");
        assert!(stale_rows.ids.is_empty());
        drop(reopened);
        remove_database(&path);
    });
}

#[test]
fn delayed_graph_facts_cannot_replace_a_newer_graph_witness() {
    futures_executor::block_on(async {
        let path = path();
        let source = PackageReference::parse("pkg:cargo/revision-app@1.0.0").expect("source");
        let old_edge = dependency_edge(&source, "serde", "^1");
        let new_edge = dependency_edge(&source, "serde", "^2");
        let old_facts = graph_facts(&source, &[old_edge]);
        let new_facts = graph_facts(&source, &[new_edge.clone()]);
        let seed_view = fallback_seed_view();
        let seed = TursoProjection::seed_if_empty(
            &path,
            ProjectionSeed::new(&seed_view, ProjectionGraphSeed::Unavailable),
        )
        .await
        .expect("seed selected view root");
        drop(seed);

        let mut delayed = TursoProjection::open(&path)
            .await
            .expect("open delayed graph writer");
        let delayed_revision = delayed
            .package_graph_revision()
            .await
            .expect("capture graph revision before source read");
        let mut current = TursoProjection::open(&path)
            .await
            .expect("open current graph writer");
        let current_revision = current
            .package_graph_revision()
            .await
            .expect("capture current graph base");
        current
            .synchronize_package_graph_from(current_revision, seed_view.root(), &new_facts)
            .await
            .expect("publish newer graph facts");
        assert!(matches!(
            delayed
                .synchronize_package_graph_from(delayed_revision, seed_view.root(), &old_facts)
                .await,
            Err(ProjectionError::StaleTransition)
        ));
        drop(delayed);
        drop(current);

        let reopened = TursoProjection::open(&path)
            .await
            .expect("cold graph reopen");
        let graph = reopened
            .package_dependencies(&source)
            .await
            .expect("read selected graph");
        assert_eq!(graph.root.as_ref(), seed_view.root().as_bytes());
        assert_eq!(graph.edges.as_ref(), &[new_edge]);
        drop(reopened);
        remove_database(&path);
    });
}

#[test]
fn delayed_graph_revision_is_rejected_after_the_selected_view_root_advances() {
    futures_executor::block_on(async {
        let path = path();
        let source = PackageReference::parse("pkg:cargo/revision-app@1.0.0").expect("source");
        let old_edge = dependency_edge(&source, "serde", "^1");
        let old_facts = graph_facts(&source, &[old_edge.clone()]);
        let old_witness = CheckedPackageGraphFacts::new(old_facts.clone())
            .expect("checked old graph facts")
            .witness();
        let seed_view = fallback_seed_view();
        let new_view = replacement_view();
        let seed = TursoProjection::seed_if_empty(
            &path,
            ProjectionSeed::new(&seed_view, ProjectionGraphSeed::Unavailable),
        )
        .await
        .expect("seed selected view root");
        drop(seed);

        let mut initial = TursoProjection::open(&path)
            .await
            .expect("open initial graph writer");
        let initial_revision = initial
            .package_graph_revision()
            .await
            .expect("capture initial graph revision");
        initial
            .synchronize_package_graph_from(initial_revision, seed_view.root(), &old_facts)
            .await
            .expect("publish initial complete graph");
        drop(initial);

        let mut delayed = TursoProjection::open(&path)
            .await
            .expect("open delayed graph writer");
        let delayed_revision = delayed
            .package_graph_revision()
            .await
            .expect("capture graph revision before source read");
        let mut current = TursoProjection::open(&path)
            .await
            .expect("open current view writer");
        let current_revision = current.revision().await.expect("capture current view base");
        current
            .synchronize_from(current_revision, &new_view)
            .await
            .expect("advance selected view while preserving the complete graph witness");
        assert!(matches!(
            current.package_dependencies(&source).await,
            Err(ProjectionError::StaleTransition)
        ));
        let current_graph_revision = current
            .package_graph_revision()
            .await
            .expect("capture graph root after selected view advance");
        current
            .synchronize_package_graph_from(
                current_graph_revision,
                new_view.root(),
                &old_facts,
            )
            .await
            .expect("rebind preserved graph facts to the new selected view root");

        assert!(matches!(
            delayed
                .synchronize_package_graph_from(delayed_revision, new_view.root(), &old_facts)
                .await,
            Err(ProjectionError::StaleTransition)
        ));
        drop(delayed);
        drop(current);

        let reopened = TursoProjection::open(&path).await.expect("cold reopen");
        let graph = reopened
            .package_dependencies(&source)
            .await
            .expect("read the preserved complete graph");
        assert_eq!(graph.root.as_ref(), new_view.root().as_bytes());
        assert_eq!(graph.facts_witness, old_witness);
        assert_eq!(graph.edges.as_ref(), &[old_edge]);
        drop(reopened);
        remove_database(&path);
    });
}

#[test]
fn delayed_graph_revision_rejects_exact_target_published_in_pretransaction_gap() {
    futures_executor::block_on(async {
        let path = path();
        let view = fallback_seed_view();
        let source = PackageReference::parse("pkg:cargo/revision-app@1.0.0").expect("source");
        let old_edge = dependency_edge(&source, "serde", "^1");
        let new_edge = dependency_edge(&source, "serde", "^2");
        let old_facts = graph_facts(&source, &[old_edge]);
        let new_facts = graph_facts(&source, std::slice::from_ref(&new_edge));
        let old_checked = CheckedPackageGraphFacts::new(old_facts)
            .expect("checked old graph facts");
        let seed = TursoProjection::seed_if_empty(
            &path,
            ProjectionSeed::new(&view, ProjectionGraphSeed::Checked(&old_checked)),
        )
        .await
        .expect("seed complete graph");
        drop(seed);

        let mut winner = TursoProjection::open(&path)
            .await
            .expect("open competing graph writer");
        let winner_revision = winner
            .package_graph_revision()
            .await
            .expect("capture winner graph base");
        let worker_path = path.clone();
        let worker_view_root = view.root();
        let worker_facts = new_facts.clone();
        let (at_transaction_sender, at_transaction_receiver) = mpsc::channel();
        let (resume_sender, resume_receiver) = mpsc::channel();
        let delayed = thread::spawn(move || {
            let mut delayed = futures_executor::block_on(TursoProjection::open(&worker_path))
                .expect("open delayed graph writer");
            let revision = futures_executor::block_on(delayed.package_graph_revision())
                .expect("capture graph revision before source read");
            crate::writer::set_before_projection_writer_tx_test_hook(move || {
                at_transaction_sender
                    .send(())
                    .expect("report pre-transaction barrier");
                resume_receiver
                    .recv()
                    .expect("release pre-transaction barrier");
            });
            futures_executor::block_on(delayed.synchronize_package_graph_from(
                revision,
                worker_view_root,
                &worker_facts,
            ))
        });

        at_transaction_receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("delayed graph writer reached the transaction boundary");
        winner
            .synchronize_package_graph_from(winner_revision, view.root(), &new_facts)
            .await
            .expect("commit exact graph target while delayed writer is paused");
        resume_sender.send(()).expect("resume delayed graph writer");
        assert!(matches!(
            delayed.join().expect("join delayed graph writer"),
            Err(ProjectionError::StaleTransition)
        ));
        drop(winner);

        let reopened = TursoProjection::open(&path).await.expect("cold graph reopen");
        let graph = reopened
            .package_dependencies(&source)
            .await
            .expect("selected graph survives stale replay");
        assert_eq!(graph.root.as_ref(), view.root().as_bytes());
        assert_eq!(graph.edges.as_ref(), &[new_edge]);
        drop(reopened);
        remove_database(&path);
    });
}

#[test]
fn graph_publication_and_reads_are_bound_to_the_selected_view_root() {
    futures_executor::block_on(async {
        let path = path();
        let view = fallback_seed_view();
        let source = PackageReference::parse("pkg:cargo/view-bound-app@1.0.0").expect("source");
        let facts = graph_facts(&source, &[dependency_edge(&source, "serde", "^1")]);
        let mut projection = open_test(&path).await.expect("open selected projection");
        let revision = projection
            .package_graph_revision()
            .await
            .expect("capture graph base");
        let unrelated = view_state_root(&[("unrelated-graph-root".to_owned(), "x".to_owned())]);
        assert!(matches!(
            projection
                .synchronize_package_graph_from(revision, unrelated, &facts)
                .await,
            Err(ProjectionError::StaleTransition)
        ));
        let revision = projection
            .package_graph_revision()
            .await
            .expect("graph remains uninitialized");
        projection
            .synchronize_package_graph_from(revision, view.root(), &facts)
            .await
            .expect("publish graph at selected view root");

        projection
            .connection
            .execute(
                "UPDATE backend_projection_package_graph_meta SET root=?1 WHERE singleton=1",
                [vec![0xA7_u8; 32]],
            )
            .await
            .expect("simulate mismatched persisted graph root");
        assert!(matches!(
            projection.package_dependencies(&source).await,
            Err(ProjectionError::StaleTransition)
        ));
        assert!(matches!(
            projection
                .package_dependencies_for_source(&PackageGraphSourceKey::unattributed(
                    source.clone(),
                ))
                .await,
            Err(ProjectionError::StaleTransition)
        ));
        let target = PackageReference::parse("pkg:cargo/serde@1.0.0").expect("target");
        assert!(matches!(
            projection.package_dependents(&target).await,
            Err(ProjectionError::StaleTransition)
        ));
        let request = backend_library::PackageGraphPageRequest::new(
            source.clone(),
            backend_library::PackageGraphDirection::Dependencies,
            None,
            4,
        )
        .expect("valid first page");
        assert!(matches!(
            projection.read_package_graph_page(&request).await,
            Err(PackageGraphReadError::Projection(
                ProjectionError::StaleTransition
            ))
        ));
        let cursor = backend_library::PackageGraphCursor {
            schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: *view.root().as_bytes(),
            facts_witness: CheckedPackageGraphFacts::new(facts)
                .expect("checked facts")
                .witness(),
            recipe: [0; 32],
            catalog_snapshot: None,
            source: None,
            after_edge_id: [0; 32],
        };
        let continued = request.with_cursor(cursor);
        assert!(matches!(
            projection.read_package_graph_page(&continued).await,
            Err(PackageGraphReadError::StaleCursor)
        ));
        drop(projection);
        remove_database(&path);
    });
}

#[test]
fn graph_snapshot_oracle_rejects_a_row_root_advance_until_graph_rebind() {
    futures_executor::block_on(async {
        let path = path();
        let initial = fallback_seed_view();
        let source = PackageReference::parse("pkg:cargo/oracle-app@1.0.0").expect("source");
        let facts = graph_facts(&source, &[dependency_edge(&source, "serde", "^1")]);
        let mut projection = open_test(&path).await.expect("open selected projection");
        let revision = projection.package_graph_revision().await.expect("graph base");
        projection
            .synchronize_package_graph_from(revision, initial.root(), &facts)
            .await
            .expect("publish initial graph");
        assert_eq!(
            graph_snapshot(&projection).await.expect("coherent initial snapshot"),
            expected_graph_snapshot(initial.root().as_bytes(), &facts)
        );

        let revision = projection.revision().await.expect("row base before source read");
        let next = view_with_label("oracle-row-root-advance");
        projection.synchronize_from(revision, &next).await.expect("advance row view");
        assert!(matches!(
            graph_snapshot(&projection).await,
            Err(ProjectionError::StaleTransition)
        ));

        let revision = projection.package_graph_revision().await.expect("graph rebind base");
        projection
            .synchronize_package_graph_from(revision, next.root(), &facts)
            .await
            .expect("rebind unchanged facts");
        assert_eq!(
            graph_snapshot(&projection).await.expect("rebound snapshot"),
            expected_graph_snapshot(next.root().as_bytes(), &facts)
        );
        drop(projection);
        let reopened = TursoProjection::open(&path).await.expect("cold reopen");
        assert_eq!(
            graph_snapshot(&reopened).await.expect("persisted coherent snapshot"),
            expected_graph_snapshot(next.root().as_bytes(), &facts)
        );
        drop(reopened);
        remove_database(&path);
    });
}

#[test]
fn unsupported_schema_versions_are_refused_without_replacing_the_generation() {
    futures_executor::block_on(async {
        let path = path();
        let package = package_key("workspace");
        let view = root(vec![Row::new(
            RowId::Package(package),
            root(Vec::new()).basis(),
            "workspace",
        )]);
        let mut projection = open_test(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        projection
            .synchronize_from_current_for_test(&view)
            .await
            .unwrap_or_else(|error| panic!("synchronize: {error}"));
        projection
            .connection
            .execute(
                "UPDATE backend_projection_meta SET schema_version = ?1 WHERE singleton=1",
                [schema::SCHEMA_VERSION - 1],
            )
            .await
            .expect("stamp older schema");
        let namespace = super::projection_namespace::ProjectionNamespace::open_existing(&path)
            .expect("namespace");
        namespace
            .set_selected_schema_for_test(schema::SCHEMA_VERSION - 1)
            .expect("stamp older selector");
        let old_selector = namespace.selected().expect("old selector");
        let generation_count = namespace
            .generation_count_for_test()
            .expect("generation count");
        assert!(matches!(
            open_test(&path).await,
            Err(ProjectionError::Schema { found }) if found == schema::SCHEMA_VERSION - 1
        ));
        let unused_seed = fallback_seed_view();
        assert!(matches!(
            TursoProjection::seed_if_empty(
                &path,
                ProjectionSeed::new(&unused_seed, ProjectionGraphSeed::Unavailable),
            )
            .await,
            Err(ProjectionError::AlreadySeeded)
        ));
        assert_eq!(
            namespace.selected().expect("selector unchanged"),
            old_selector
        );
        assert_eq!(
            namespace
                .generation_count_for_test()
                .expect("generation count unchanged"),
            generation_count
        );

        projection
            .connection
            .execute(
                "UPDATE backend_projection_meta SET schema_version = ?1 WHERE singleton=1",
                [schema::SCHEMA_VERSION + 1],
            )
            .await
            .expect("stamp future schema");
        namespace
            .set_selected_schema_for_test(schema::SCHEMA_VERSION + 1)
            .expect("stamp future selector");
        let future_selector = namespace.selected().expect("future selector");
        assert!(matches!(
            open_test(&path).await,
            Err(ProjectionError::Schema { found }) if found == schema::SCHEMA_VERSION + 1
        ));
        assert_eq!(
            namespace.selected().expect("future selector unchanged"),
            future_selector
        );
        assert_eq!(
            namespace
                .generation_count_for_test()
                .expect("generation count remains bounded"),
            generation_count
        );
        drop(projection);
        remove_database(&path);
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

        let mut initial = open_test(&path)
            .await
            .unwrap_or_else(|error| panic!("open initial projection: {error}"));
        assert_eq!(
            initial
                .synchronize_from_current_for_test(&base)
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
                    let mut projection = open_test(&path).await?;
                    let update = projection.synchronize_from_current_for_test(&base).await?;
                    if !matches!(update, ProjectionUpdate::Reused { .. }) {
                        return Err(ProjectionError::StaleTransition);
                    }
                    Ok(projection)
                });
                barrier.wait();
                let result = match prepared {
                    Ok(projection) => {
                        futures_executor::block_on(projection.lookup_label("workspace", 8))
                    }
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
                    let mut projection = open_test(&path).await?;
                    let update = projection.synchronize_from_current_for_test(&base).await?;
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

        let mut restarted = open_test(&path)
            .await
            .unwrap_or_else(|error| panic!("reopen projection: {error}"));
        assert_eq!(
            restarted
                .synchronize_from_current_for_test(&first_view)
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
                .synchronize_from_current_for_test(&second_view)
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
            .lookup_label("workspace", 8)
            .await
            .unwrap_or_else(|error| panic!("post-restart label lookup: {error}"));
        assert_eq!(rows.root.as_ref(), second_view.root().as_bytes());
        assert_eq!(rows.ids.as_ref(), &[RowId::Package(package).stable_key()]);
        drop(restarted);
        remove_database(&path);
    });
}

#[test]
fn unpaged_reverse_graph_read_stops_at_its_bounded_result_limit() {
    futures_executor::block_on(async {
        let path = path();
        let mut projection = open_test(&path).await.expect("open projection");
        let source_facts = (0..=MAX_PACKAGE_GRAPH_ROWS)
            .map(|index| {
                let source = PackageReference::parse(format!("pkg:cargo/dependent-{index}@1.0.0"))
                    .expect("source coordinate");
                let edge = dependency_edge(&source, "shared-target", "^1");
                (
                    PackageGraphSourceKey::unattributed(source),
                    DependencyFacts::Known(vec![edge].into_boxed_slice()),
                )
            })
            .collect::<Vec<_>>();
        let checked =
            CheckedPackageGraphFacts::new(source_facts).expect("admit bounded source facts");
        let graph_root = fallback_seed_view().root();
        projection
            .synchronize_checked_package_graph_current_for_test(graph_root, &checked)
            .await
            .expect("publish valid graph with more reverse edges than one unpaged read admits");

        let target =
            PackageReference::parse("pkg:cargo/shared-target@1.0.0").expect("target coordinate");
        assert!(matches!(
            projection.package_dependents(&target).await,
            Err(ProjectionError::ReadLimitExceeded { maximum })
                if maximum == MAX_PACKAGE_GRAPH_ROWS
        ));
        drop(projection);
        remove_database(&path);
    });
}

#[test]
fn package_graph_keeps_same_coordinate_edges_separate_by_registry_authority() {
    futures_executor::block_on(async {
        let path = path();
        let source = PackageReference::parse("pkg:cargo/shared@1.0.0").expect("source");
        let target = PackageReference::parse("pkg:cargo/target@1.0.0").expect("target");
        let authority_a = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x31; 32]),
        );
        let authority_b = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x52; 32]),
        );
        let key_a = PackageGraphSourceKey::new(source.clone(), authority_a);
        let key_b = PackageGraphSourceKey::new(source.clone(), authority_b);
        let edge = |authority, frontier| {
            PackageDependencyRecord::new_with_source_authority(
                source.clone(),
                authority,
                PackageDependencyTarget::new(RegistryEcosystem::Cargo, "target", "*", None)
                    .expect("target facts"),
                DependencyScope::Runtime,
                false,
                DependencyEvidence {
                    authority: DependencyAuthority::RegistryMetadata,
                    frontier: [frontier; 32],
                    provenance: [frontier + 1; 32],
                },
            )
        };
        let edge_a = edge(authority_a, 1);
        let edge_b = edge(authority_b, 3);
        let facts = vec![
            (
                key_a.clone(),
                DependencyFacts::Known(vec![edge_a.clone()].into_boxed_slice()),
            ),
            (
                key_b.clone(),
                DependencyFacts::Known(vec![edge_b.clone()].into_boxed_slice()),
            ),
        ];
        let checked = CheckedPackageGraphFacts::new(facts).expect("checked source facts");
        let mut projection = open_test(&path).await.expect("open");
        projection
            .synchronize_checked_package_graph_current_for_test(
                fallback_seed_view().root(),
                &checked,
            )
            .await
            .expect("project both authorities");

        let ambiguous = projection
            .package_dependencies(&source)
            .await
            .expect("coordinate-only lookup");
        assert!(
            ambiguous.edges.is_empty(),
            "ambiguous lookup must not merge"
        );
        assert!(matches!(
            ambiguous.source_selection,
            PackageGraphSourceSelection::Ambiguous(keys)
                if keys.as_ref() == [key_a.clone(), key_b.clone()]
        ));
        let selected_a = projection
            .package_dependencies_for_source(&key_a)
            .await
            .expect("exact authority A");
        let selected_b = projection
            .package_dependencies_for_source(&key_b)
            .await
            .expect("exact authority B");
        assert_eq!(selected_a.edges.as_ref(), [edge_a]);
        assert_eq!(selected_b.edges.as_ref(), [edge_b]);
        assert_eq!(
            selected_a.source_selection,
            PackageGraphSourceSelection::Exact(key_a)
        );
        assert_eq!(
            selected_b.source_selection,
            PackageGraphSourceSelection::Exact(key_b)
        );

        let reverse = projection
            .package_dependents(&target)
            .await
            .expect("reverse by target");
        assert_eq!(reverse.edges.len(), 2);
        assert_ne!(
            reverse.edges[0].source_authority,
            reverse.edges[1].source_authority
        );

        let forged = PackageDependencyRecord::new_with_source_authority(
            source.clone(),
            authority_a,
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "target", "*", None)
                .expect("target facts"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::ForgeManifest,
                frontier: [7; 32],
                provenance: [8; 32],
            },
        );
        assert!(
            projection
                .synchronize_package_graph_current_for_test(
                    fallback_seed_view().root(),
                    &[(
                        PackageGraphSourceKey::new(source, authority_a),
                        DependencyFacts::Known(vec![forged].into_boxed_slice()),
                    )],
                )
                .await
                .is_err()
        );
        drop(projection);
        remove_database(&path);
    });
}

#[test]
fn package_graph_distinguishes_known_empty_unknown_and_absent_authorities() {
    futures_executor::block_on(async {
        let path = path();
        let coordinate = PackageReference::parse("pkg:cargo/shared@1.0.0").expect("source");
        let known_authority = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x61; 32]),
        );
        let unknown_authority = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x72; 32]),
        );
        let known = PackageGraphSourceKey::new(coordinate.clone(), known_authority);
        let unknown = PackageGraphSourceKey::new(coordinate.clone(), unknown_authority);
        let absent = PackageGraphSourceKey::new(
            coordinate.clone(),
            PackageGraphSourceAuthority::Registry(RegistryAuthorityId::from_configured_source(
                [0x83; 32],
            )),
        );
        let facts: [PackageDependencySourceFacts; 2] = [
            (
                known.clone(),
                DependencyFacts::Known(Vec::<PackageDependencyRecord>::new().into_boxed_slice()),
            ),
            (
                unknown.clone(),
                DependencyFacts::Unknown(ProductText::new("field omitted").expect("reason")),
            ),
        ];
        let mut projection = open_test(&path).await.expect("open");
        projection
            .synchronize_package_graph_current_for_test(
                fallback_seed_view().root(),
                &facts,
            )
            .await
            .expect("persist source states");
        let duplicate_source_facts: [PackageDependencySourceFacts; 2] = [
            (
                known.clone(),
                DependencyFacts::Known(Vec::<PackageDependencyRecord>::new().into_boxed_slice()),
            ),
            (
                known.clone(),
                DependencyFacts::Unknown(ProductText::new("duplicate").expect("reason")),
            ),
        ];
        assert!(
            projection
                .synchronize_package_graph_current_for_test(
                    fallback_seed_view().root(),
                    &duplicate_source_facts,
                )
                .await
                .is_err()
        );
        let known_result = projection
            .package_dependencies_for_source(&known)
            .await
            .expect("known empty");
        assert!(known_result.edges.is_empty());
        assert!(known_result.state.is_none());
        assert_eq!(
            known_result.source_selection,
            PackageGraphSourceSelection::Exact(known)
        );
        let unknown_result = projection
            .package_dependencies_for_source(&unknown)
            .await
            .expect("unknown state");
        assert_eq!(
            unknown_result.state.as_ref().map(|state| state.kind),
            Some(1)
        );
        assert_eq!(
            projection
                .package_dependencies_for_source(&absent)
                .await
                .expect("absent source")
                .source_selection,
            PackageGraphSourceSelection::Missing
        );
        assert!(matches!(
            projection.package_dependencies(&coordinate).await.expect("ambiguous").source_selection,
            PackageGraphSourceSelection::Ambiguous(keys) if keys.len() == 2
        ));
        drop(projection);
        remove_database(&path);
    });
}

#[test]
fn package_graph_reuses_root_and_answers_forward_and_reverse_edges() {
    futures_executor::block_on(async {
        let path = path();
        let view_a = fallback_seed_view();
        let view_b = view_with_label("graph-root-b");
        let view_c = view_with_label("graph-root-c");
        let view_d = view_with_label("graph-root-d");
        let root_a = view_a.root();
        let root_b = view_b.root();
        let root_c = view_c.root();
        let root_d = view_d.root();
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
        let facts = graph_facts(&source, std::slice::from_ref(&edge));
        let mut projection = open_test(&path).await.expect("open");
        assert_eq!(
            projection
                .synchronize_package_graph_current_for_test(root_a, &facts)
                .await
                .expect("project graph"),
            ProjectionUpdate::Rebuilt { rows: 1 }
        );
        assert_eq!(
            projection
                .synchronize_package_graph_current_for_test(root_a, &facts)
                .await
                .expect("reuse graph"),
            ProjectionUpdate::Reused { rows: 1 }
        );
        let first_edge_rowid = stored_edge(&projection, &edge.facts_version).await;
        projection
            .synchronize_from_current_for_test(&view_b)
            .await
            .expect("advance selected row root before moving graph root");
        let changes_before_root_move = graph_total_changes(&projection).await;
        assert_eq!(
            projection
                .synchronize_package_graph_current_for_test(root_b, &facts)
                .await
                .expect("project new root"),
            ProjectionUpdate::Rebuilt { rows: 1 }
        );
        assert_eq!(
            stored_edge(&projection, &edge.facts_version).await,
            first_edge_rowid,
            "a root-only move retains the selected edge row"
        );
        assert_eq!(
            graph_total_changes(&projection).await - changes_before_root_move,
            1,
            "a root-only move writes only selected graph metadata"
        );
        let mut meta_rows = projection
            .connection
            .query(
                "SELECT root FROM backend_projection_package_graph_meta WHERE singleton=1",
                (),
            )
            .await
            .expect("meta");
        let meta_root: Vec<u8> = meta_rows
            .next()
            .await
            .expect("meta row")
            .expect("meta")
            .get(0)
            .expect("root");
        assert_eq!(meta_root.as_slice(), root_b.as_bytes());
        let forward = projection
            .package_dependencies(&source)
            .await
            .expect("forward");
        assert_eq!(forward.root.as_ref(), root_b.as_bytes());
        assert_eq!(forward.edges.as_ref(), &[edge.clone()]);
        let reverse = projection
            .package_dependents(&target)
            .await
            .expect("reverse");
        assert_eq!(reverse.root.as_ref(), root_b.as_bytes());
        assert_eq!(reverse.edges.len(), 1);
        assert_eq!(reverse.edges[0].source, source);
        let facts_version = edge.facts_version;
        let rowid = stored_edge(&projection, &facts_version).await;
        let view_moved = view_with_label("graph-root-moved");
        projection
            .synchronize_from_current_for_test(&view_moved)
            .await
            .expect("advance selected row root before second graph move");
        let changes_before_second_root_move = graph_total_changes(&projection).await;
        let root_move = view_moved.root();
        assert_eq!(
            projection
                .synchronize_package_graph_current_for_test(root_move, &facts)
                .await
                .expect("move graph root"),
            ProjectionUpdate::Rebuilt { rows: 1 }
        );
        assert_eq!(stored_edge(&projection, &facts_version).await, rowid);
        assert_eq!(
            graph_total_changes(&projection).await - changes_before_second_root_move,
            1
        );
        let moved_forward = projection
            .package_dependencies(&source)
            .await
            .expect("forward after root move");
        assert_eq!(moved_forward.root.as_ref(), root_move.as_bytes());
        assert_eq!(moved_forward.edges.as_ref(), &[edge]);
        projection
            .synchronize_from_current_for_test(&view_c)
            .await
            .expect("advance selected row root before clearing graph");
        assert_eq!(
            projection
                .synchronize_package_graph_current_for_test(root_c, &[])
                .await
                .expect("clear graph"),
            ProjectionUpdate::Rebuilt { rows: 0 }
        );
        let forward_empty = projection
            .package_dependencies(&source)
            .await
            .expect("forward empty");
        assert!(forward_empty.edges.is_empty());
        let mut count_rows = projection
            .connection
            .query("SELECT COUNT(*) FROM backend_projection_package_edges", ())
            .await
            .expect("count");
        let count: i64 = count_rows
            .next()
            .await
            .expect("count row")
            .expect("count")
            .get(0)
            .expect("count");
        assert_eq!(count, 0);
        let edge_v2 = PackageDependencyRecord::new(
            source.clone(),
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "^2", None)
                .expect("target facts"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [1; 32],
                provenance: [2; 32],
            },
        );
        let facts_v2 = graph_facts(&source, std::slice::from_ref(&edge_v2));
        projection
            .synchronize_from_current_for_test(&view_d)
            .await
            .expect("advance selected row root before changed facts");
        assert_eq!(
            projection
                .synchronize_package_graph_current_for_test(root_d, &facts_v2)
                .await
                .expect("project changed requirement"),
            ProjectionUpdate::Rebuilt { rows: 1 }
        );
        let forward_v2 = projection
            .package_dependencies(&source)
            .await
            .expect("forward v2");
        assert_eq!(forward_v2.edges.len(), 1);
        assert_eq!(forward_v2.edges[0].target.requirement.as_str(), "^2");
        assert_ne!(forward_v2.edges[0].target.requirement.as_str(), "^1");
        let unavailable = vec![(
            PackageGraphSourceKey::unattributed(target.clone()),
            DependencyFacts::Unavailable(ProductText::new("metadata timeout").expect("reason")),
        )];
        let view_e = view_with_label("graph-root-e");
        projection
            .synchronize_from_current_for_test(&view_e)
            .await
            .expect("advance selected row root before unavailable state");
        let root_e = view_e.root();
        projection
            .synchronize_package_graph_current_for_test(root_e, &unavailable)
            .await
            .expect("project unavailable");
        let state = projection
            .package_dependencies(&target)
            .await
            .expect("unavailable state")
            .state
            .expect("state row");
        assert_eq!(state.kind, 2);
        assert_eq!(
            projection
                .package_dependencies(&target)
                .await
                .expect("state root")
                .root
                .as_ref(),
            root_e.as_bytes()
        );
        assert!(
            projection
                .package_dependencies(&source)
                .await
                .expect("source cleared")
                .edges
                .is_empty()
        );
        let state_rowid = stored_state_rowid(&projection, target.as_str()).await;
        let view_f = view_with_label("graph-root-f");
        projection
            .synchronize_from_current_for_test(&view_f)
            .await
            .expect("advance selected row root before state-only move");
        let root_f = view_f.root();
        let changes_before_state_root_move = graph_total_changes(&projection).await;
        assert_eq!(
            projection
                .synchronize_package_graph_current_for_test(root_f, &unavailable)
                .await
                .expect("keep matching state"),
            ProjectionUpdate::Rebuilt { rows: 0 }
        );
        assert_eq!(
            stored_state_rowid(&projection, target.as_str()).await,
            state_rowid
        );
        assert_eq!(
            graph_total_changes(&projection).await - changes_before_state_root_move,
            1,
            "a state root-only move writes only selected graph metadata"
        );
        let fenced = projection
            .package_dependencies(&target)
            .await
            .expect("fenced state");
        assert_eq!(fenced.root.as_ref(), root_f.as_bytes());
        let fenced_state = fenced.state.expect("kept state");
        assert_eq!(fenced_state.kind, 2);
        assert_eq!(fenced_state.reason, "metadata timeout");
        let changed = vec![(
            PackageGraphSourceKey::unattributed(target.clone()),
            DependencyFacts::Unavailable(ProductText::new("registry reset").expect("reason")),
        )];
        let view_g = view_with_label("graph-root-g");
        projection
            .synchronize_from_current_for_test(&view_g)
            .await
            .expect("advance selected row root before state reason change");
        let root_g = view_g.root();
        let state_rowid_before_reason_change =
            stored_state_rowid(&projection, target.as_str()).await;
        projection
            .synchronize_package_graph_current_for_test(root_g, &changed)
            .await
            .expect("replace state");
        assert_eq!(
            stored_state_rowid(&projection, target.as_str()).await,
            state_rowid_before_reason_change
        );
        let replaced = projection
            .package_dependencies(&target)
            .await
            .expect("replaced state");
        assert_eq!(replaced.root.as_ref(), root_g.as_bytes());
        assert_eq!(
            replaced.state.expect("replaced").reason.as_str(),
            "registry reset"
        );
        drop(projection);
        remove_database(&path);
    });
}

#[test]
fn package_graph_same_root_reconciles_changed_facts_after_restart() {
    futures_executor::block_on(async {
        let path = path();
        let selected_root = fallback_seed_view().root();
        let source = PackageReference::parse("pkg:cargo/app@1.0.0").expect("source");
        let old_runtime =
            dependency_edge_in_scope(&source, "serde", "^1", DependencyScope::Runtime);
        let development =
            dependency_edge_in_scope(&source, "serde", "^1", DependencyScope::Development);
        let old_facts = graph_facts(&source, &[old_runtime, development.clone()]);
        let expected_runtime =
            dependency_edge_in_scope(&source, "serde", "^2", DependencyScope::Runtime);
        let build = dependency_edge_in_scope(&source, "serde", "^1", DependencyScope::Build);
        let mut expected = vec![expected_runtime, development, build];
        expected.sort_unstable_by_key(|edge| edge.facts_version);
        let changed_facts = graph_facts(&source, &expected);
        let expected_witness = package_dependency_facts_witness(&changed_facts);

        let mut projection = open_test(&path).await.expect("open initial");
        assert_eq!(
            projection
                .synchronize_package_graph_current_for_test(selected_root, &old_facts)
                .await
                .expect("seed old graph"),
            ProjectionUpdate::Rebuilt { rows: 2 }
        );
        assert_ne!(
            package_dependency_facts_witness(&old_facts),
            expected_witness
        );
        drop(projection);

        let mut projection = open_test(&path).await.expect("reopen");
        assert_eq!(
            projection
                .synchronize_package_graph_current_for_test(selected_root, &changed_facts)
                .await
                .expect("replace graph at unchanged root"),
            ProjectionUpdate::Rebuilt { rows: 3 }
        );
        let forward = projection
            .package_dependencies(&source)
            .await
            .expect("forward after replacement");
        assert_eq!(forward.root.as_ref(), selected_root.as_bytes());
        assert_eq!(forward.facts_witness, expected_witness);
        assert_eq!(forward.edges.as_ref(), expected.as_slice());
        assert_eq!(graph_edge_count(&projection).await, 3);
        drop(projection);

        let reopened = open_test(&path).await.expect("reopen persisted graph");
        let persisted = reopened
            .package_dependencies(&source)
            .await
            .expect("persisted forward graph");
        assert_eq!(persisted.root.as_ref(), selected_root.as_bytes());
        assert_eq!(persisted.facts_witness, expected_witness);
        assert_eq!(persisted.edges.as_ref(), expected.as_slice());
        drop(reopened);
        for suffix in ["", "-wal", "-shm", "-tshm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
            let _ = std::fs::remove_file(sidecar);
        }
    });
}

#[test]
fn package_graph_source_witness_delta_edits_deletes_and_reopens_exactly() {
    futures_executor::block_on(async {
        let path = path();
        let coordinate = PackageReference::parse("pkg:cargo/shared@1.0.0").expect("source");
        let state_coordinate =
            PackageReference::parse("pkg:cargo/stateful@1.0.0").expect("state source");
        let authority_a = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x31; 32]),
        );
        let authority_b = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x52; 32]),
        );
        let authority_state = PackageGraphSourceAuthority::Forge([0x73; 32]);
        let key_a = PackageGraphSourceKey::new(coordinate.clone(), authority_a);
        let key_b = PackageGraphSourceKey::new(coordinate.clone(), authority_b);
        let state_key = PackageGraphSourceKey::new(state_coordinate.clone(), authority_state);
        let empty_key = PackageGraphSourceKey::unattributed(
            PackageReference::parse("pkg:cargo/empty@1.0.0").expect("empty source"),
        );
        let edge = |source: &PackageReference,
                    source_authority: PackageGraphSourceAuthority,
                    target: &str,
                    requirement: &str,
                    frontier: u8| {
            PackageDependencyRecord::new_with_source_authority(
                source.clone(),
                source_authority,
                PackageDependencyTarget::new(RegistryEcosystem::Cargo, target, requirement, None)
                    .expect("edge target"),
                DependencyScope::Runtime,
                false,
                DependencyEvidence {
                    authority: match source_authority {
                        PackageGraphSourceAuthority::Registry(_) => {
                            DependencyAuthority::RegistryMetadata
                        }
                        PackageGraphSourceAuthority::Forge(_) => DependencyAuthority::ForgeManifest,
                        _ => unreachable!("fixture authority"),
                    },
                    frontier: [frontier; 32],
                    provenance: [frontier.wrapping_add(1); 32],
                },
            )
        };
        let edge_a = edge(&coordinate, authority_a, "keep", "^1", 1);
        let edge_b_old = edge(&coordinate, authority_b, "replace", "^1", 3);
        let edge_b_new = edge(&coordinate, authority_b, "replace", "^2", 5);
        let initial = vec![
            (
                key_a.clone(),
                DependencyFacts::Known(vec![edge_a.clone()].into_boxed_slice()),
            ),
            (
                key_b.clone(),
                DependencyFacts::Known(vec![edge_b_old.clone()].into_boxed_slice()),
            ),
            (
                state_key.clone(),
                DependencyFacts::Unavailable(ProductText::new("forge timeout").expect("reason")),
            ),
        ];
        let selected_root = fallback_seed_view().root();
        let mut projection = open_test(&path).await.expect("open initial");
        projection
            .synchronize_package_graph_current_for_test(selected_root.clone(), &initial)
            .await
            .expect("seed graph");
        let edge_a_rowid = stored_edge(&projection, &edge_a.facts_version).await;
        drop(projection);

        // Reopening must retain source witnesses so this same-root update can
        // isolate the changed authority and state without rebuilding other
        // authorities at the shared coordinate.
        let mut projection = open_test(&path).await.expect("cold reopen");
        let changed = vec![
            (
                empty_key.clone(),
                DependencyFacts::Known(Vec::<PackageDependencyRecord>::new().into_boxed_slice()),
            ),
            (
                key_b.clone(),
                DependencyFacts::Known(vec![edge_b_new.clone()].into_boxed_slice()),
            ),
            (
                state_key.clone(),
                DependencyFacts::Unknown(
                    ProductText::new("manifest has no graph").expect("reason"),
                ),
            ),
            (
                key_a.clone(),
                DependencyFacts::Known(vec![edge_a.clone()].into_boxed_slice()),
            ),
        ];
        let checked = CheckedPackageGraphFacts::new(changed.clone()).expect("checked delta");
        assert_eq!(
            projection
                .synchronize_checked_package_graph_current_for_test(selected_root.clone(), &checked)
                .await
                .expect("same-root source delta"),
            ProjectionUpdate::Rebuilt { rows: 2 }
        );
        assert_eq!(
            stored_edge(&projection, &edge_a.facts_version).await,
            edge_a_rowid
        );
        assert_eq!(
            projection
                .package_dependencies_for_source(&key_a)
                .await
                .expect("authority A remains")
                .edges
                .as_ref(),
            [edge_a.clone()]
        );
        assert_eq!(
            projection
                .package_dependencies_for_source(&key_b)
                .await
                .expect("authority B is replaced")
                .edges
                .as_ref(),
            [edge_b_new.clone()]
        );
        assert_eq!(
            projection
                .package_dependencies_for_source(&state_key)
                .await
                .expect("unknown state")
                .state
                .expect("state row")
                .kind,
            1
        );
        assert!(
            projection
                .package_dependencies_for_source(&empty_key)
                .await
                .expect("known empty source")
                .state
                .is_none()
        );
        assert_eq!(
            crate::graph::package_graph_metadata_from(&projection.connection)
                .await
                .expect("graph metadata")
                .expect("metadata")
                .facts_witness,
            checked.witness()
        );

        let no_op_changes = graph_total_changes(&projection).await;
        assert_eq!(
            projection
                .synchronize_checked_package_graph_current_for_test(selected_root.clone(), &checked)
                .await
                .expect("exact no-op"),
            ProjectionUpdate::Reused { rows: 2 }
        );
        assert_eq!(graph_total_changes(&projection).await, no_op_changes);

        // Dropping the authority and the unknown source removes their edges,
        // state, and source-presence entries as one generation.
        let after_deletion = vec![
            (
                empty_key.clone(),
                DependencyFacts::Known(Vec::<PackageDependencyRecord>::new().into_boxed_slice()),
            ),
            (
                key_a.clone(),
                DependencyFacts::Known(vec![edge_a.clone()].into_boxed_slice()),
            ),
        ];
        projection
            .synchronize_package_graph_current_for_test(selected_root.clone(), &after_deletion)
            .await
            .expect("delete sources at same root");
        assert_eq!(
            projection
                .package_dependencies_for_source(&key_b)
                .await
                .expect("deleted authority")
                .source_selection,
            PackageGraphSourceSelection::Missing
        );
        assert_eq!(
            projection
                .package_dependencies_for_source(&state_key)
                .await
                .expect("deleted state source")
                .source_selection,
            PackageGraphSourceSelection::Missing
        );
        drop(projection);

        let reopened = open_test(&path).await.expect("reopen final graph");
        let final_graph = graph_snapshot(&reopened)
            .await
            .unwrap_or_else(|error| panic!("final source delta snapshot: {error}"));
        assert_eq!(final_graph.edge_count, 1);
        assert_eq!(final_graph.root.as_slice(), selected_root.as_bytes());
        drop(reopened);
        remove_database(&path);
    });
}

#[test]
fn package_graph_concurrent_writers_publish_whole_generations() {
    futures_executor::block_on(async {
        let path = path();
        let app = PackageReference::parse("pkg:cargo/race-app@1.0.0").expect("app");
        let state_source =
            PackageReference::parse("pkg:npm/race-state@1.0.0").expect("state source");

        let root_initial = fallback_seed_view().root();
        let facts_initial = race_generation_facts(&app, &state_source, "^1", "initial state");
        let expected_initial = expected_graph_snapshot(root_initial.as_bytes(), &facts_initial);
        let mut seed = open_test(&path).await.expect("open seed");
        seed.synchronize_package_graph_current_for_test(root_initial, &facts_initial)
            .await
            .expect("seed initial graph");
        drop(seed);

        let root_a = root_initial;
        let facts_a = race_generation_facts(&app, &state_source, "^2", "writer a state");
        let expected_a = expected_graph_snapshot(root_a.as_bytes(), &facts_a);
        let root_b = root_initial;
        let facts_b = race_generation_facts(&app, &state_source, "^3", "writer b state");
        let expected_b = expected_graph_snapshot(root_b.as_bytes(), &facts_b);
        assert_ne!(expected_initial, expected_a);
        assert_ne!(expected_initial, expected_b);
        assert_ne!(expected_a, expected_b);

        let reader = open_test(&path).await.expect("open reader");
        let start = Arc::new(Barrier::new(3));
        let reader_ready = Arc::new(Barrier::new(3));
        let writers_done = Arc::new(AtomicUsize::new(0));

        let reader_start = Arc::clone(&start);
        let reader_ready_for_thread = Arc::clone(&reader_ready);
        let reader_done = Arc::clone(&writers_done);
        let reader_thread = thread::spawn(move || {
            reader_start.wait();
            let first_snapshot = futures_executor::block_on(graph_snapshot(&reader));
            // Do one complete read snapshot before allowing either writer to
            // enter synchronization, then poll while their commits race.
            reader_ready_for_thread.wait();
            let mut observed = vec![first_snapshot?];
            while reader_done.load(Ordering::Acquire) < 2 {
                observed.push(futures_executor::block_on(graph_snapshot(&reader))?);
                thread::yield_now();
            }
            observed.push(futures_executor::block_on(graph_snapshot(&reader))?);
            Ok::<_, ProjectionError>(observed)
        });

        let writer_a = spawn_graph_writer(
            path.clone(),
            root_a,
            facts_a,
            Arc::clone(&start),
            Arc::clone(&reader_ready),
            Arc::clone(&writers_done),
        );
        let writer_b = spawn_graph_writer(
            path.clone(),
            root_b,
            facts_b,
            start,
            reader_ready,
            writers_done,
        );
        let result_a = writer_a.join().expect("writer a thread");
        let result_b = writer_b.join().expect("writer b thread");
        let committed_a = match result_a {
            Ok(ProjectionUpdate::Rebuilt { .. }) => true,
            Err(ProjectionError::StaleTransition) => false,
            result => panic!("writer a had an unexpected outcome: {result:?}"),
        };
        let committed_b = match result_b {
            Ok(ProjectionUpdate::Rebuilt { .. }) => true,
            Err(ProjectionError::StaleTransition) => false,
            result => panic!("writer b had an unexpected outcome: {result:?}"),
        };
        assert!(committed_a || committed_b, "at least one graph writer must commit");

        let observed = reader_thread
            .join()
            .expect("reader thread")
            .unwrap_or_else(|error| panic!("reader snapshot failed: {error}"));
        for snapshot in &observed {
            assert!(
                snapshot == &expected_initial
                    || (committed_a && snapshot == &expected_a)
                    || (committed_b && snapshot == &expected_b),
                "reader observed a mixed graph generation: {snapshot:?}"
            );
        }
        assert!(
            observed
                .iter()
                .any(|snapshot| snapshot == &expected_initial),
            "the seeded graph should be visible before either writer commits"
        );

        let reopened = open_test(&path).await.expect("reopen final graph");
        let final_snapshot = graph_snapshot(&reopened)
            .await
            .unwrap_or_else(|error| panic!("final graph snapshot: {error}"));
        assert!(
            (committed_a && final_snapshot == expected_a)
                || (committed_b && final_snapshot == expected_b),
            "final graph must be exactly one writer's complete generation: {final_snapshot:?}"
        );
        drop(reopened);
        for suffix in ["", "-wal", "-shm", "-tshm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
            let _ = std::fs::remove_file(sidecar);
        }
    });
}

#[test]
fn package_graph_root_move_keeps_an_unchanged_edge_and_replaces_one() {
    futures_executor::block_on(async {
        let path = path();
        let source = PackageReference::parse("pkg:cargo/app@1.0.0").expect("source");
        let kept = dependency_edge(&source, "serde", "^1");
        let old = dependency_edge(&source, "tokio", "^1");
        let facts = graph_facts(&source, &[kept.clone(), old.clone()]);
        let mut projection = open_test(&path).await.expect("open");
        let initial_view = fallback_seed_view();
        let root_a = initial_view.root();
        projection
            .synchronize_package_graph_current_for_test(root_a, &facts)
            .await
            .expect("seed");
        let kept_row = stored_edge(&projection, &kept.facts_version).await;
        let view_b = view_with_label("graph-keep-b");
        let root_b = view_b.root();
        projection
            .synchronize_from_current_for_test(&view_b)
            .await
            .expect("advance selected row root before graph root move");
        let before_root_move = graph_total_changes(&projection).await;
        projection
            .synchronize_package_graph_current_for_test(root_b, &facts)
            .await
            .expect("move");
        assert_eq!(
            stored_edge(&projection, &kept.facts_version).await,
            kept_row
        );
        assert_eq!(graph_total_changes(&projection).await - before_root_move, 1);
        let replacement = dependency_edge(&source, "tokio", "^2");
        let replaced = graph_facts(&source, &[kept.clone(), replacement.clone()]);
        let view_c = view_with_label("graph-keep-c");
        let root_c = view_c.root();
        projection
            .synchronize_from_current_for_test(&view_c)
            .await
            .expect("advance selected row root before replacing graph edge");
        let before_edge_delta = graph_total_changes(&projection).await;
        projection
            .synchronize_package_graph_current_for_test(root_c, &replaced)
            .await
            .expect("replace");
        let after_edge_delta = graph_total_changes(&projection).await;
        assert_eq!(
            after_edge_delta - before_edge_delta,
            4,
            "one-source reconciliation deletes/inserts the edge, advances its witness, and publishes metadata"
        );
        assert_eq!(
            stored_edge(&projection, &kept.facts_version).await,
            kept_row
        );
        let _replacement_row = stored_edge(&projection, &replacement.facts_version).await;
        let mut expected = vec![kept, replacement.clone()];
        expected.sort_unstable_by_key(|edge| edge.facts_version);
        let selected = projection
            .package_dependencies(&source)
            .await
            .expect("selected graph after edge delta");
        assert_eq!(selected.root.as_ref(), root_c.as_bytes());
        assert_eq!(selected.edges.as_ref(), expected.as_slice());
        let mut rows = projection
            .connection
            .query(
                "SELECT COUNT(*) FROM backend_projection_package_edges WHERE edge_id = ?1",
                turso::params![old.facts_version.as_slice()],
            )
            .await
            .expect("old edge count");
        let count: i64 = rows
            .next()
            .await
            .expect("old edge row")
            .expect("old edge")
            .get(0)
            .expect("count");
        assert_eq!(count, 0);
    });
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GraphSnapshot {
    root: Vec<u8>,
    facts_witness: [u8; 32],
    edge_count: i64,
    edge_ids: Vec<[u8; 32]>,
    states: Vec<(String, i64, Vec<u8>, i64, String)>,
}

fn expected_graph_snapshot(
    root: &[u8],
    facts: &[(
        PackageGraphSourceKey,
        DependencyFacts<Box<[PackageDependencyRecord]>>,
    )],
) -> GraphSnapshot {
    let mut edge_ids = Vec::new();
    let mut states = Vec::new();
    for (source, state) in facts {
        match state {
            DependencyFacts::Known(edges) => {
                edge_ids.extend(edges.iter().map(|edge| edge.facts_version));
            }
            DependencyFacts::Unknown(reason) => {
                states.push((
                    source.as_str().to_owned(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().to_vec(),
                    1,
                    reason.as_str().to_owned(),
                ));
            }
            DependencyFacts::Unavailable(reason) => {
                states.push((
                    source.as_str().to_owned(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().to_vec(),
                    2,
                    reason.as_str().to_owned(),
                ));
            }
        }
    }
    edge_ids.sort_unstable();
    states.sort_unstable();
    GraphSnapshot {
        root: root.to_vec(),
        facts_witness: package_dependency_facts_witness(facts),
        edge_count: i64::try_from(edge_ids.len()).expect("expected edge count"),
        edge_ids,
        states,
    }
}

fn race_generation_facts(
    app: &PackageReference,
    state_source: &PackageReference,
    requirement: &str,
    reason: &str,
) -> Vec<(
    PackageGraphSourceKey,
    DependencyFacts<Box<[PackageDependencyRecord]>>,
)> {
    vec![
        (
            PackageGraphSourceKey::unattributed(app.clone()),
            DependencyFacts::Known(
                vec![
                    dependency_edge_in_scope(app, "serde", requirement, DependencyScope::Runtime),
                    dependency_edge_in_scope(
                        app,
                        "serde",
                        requirement,
                        DependencyScope::Development,
                    ),
                ]
                .into_boxed_slice(),
            ),
        ),
        (
            PackageGraphSourceKey::unattributed(state_source.clone()),
            DependencyFacts::Unavailable(ProductText::new(reason).expect("state reason")),
        ),
    ]
}

fn spawn_graph_writer(
    path: PathBuf,
    root: backend_library::ViewStateRoot,
    facts: Vec<(
        PackageGraphSourceKey,
        DependencyFacts<Box<[PackageDependencyRecord]>>,
    )>,
    start: Arc<Barrier>,
    reader_ready: Arc<Barrier>,
    writers_done: Arc<AtomicUsize>,
) -> thread::JoinHandle<Result<ProjectionUpdate, ProjectionError>> {
    thread::spawn(move || {
        let opened = futures_executor::block_on(open_test(&path));
        start.wait();
        reader_ready.wait();
        let result = match opened {
            Ok(mut projection) => futures_executor::block_on(
                projection.synchronize_package_graph_current_for_test(root, &facts),
            ),
            Err(error) => Err(error),
        };
        writers_done.fetch_add(1, Ordering::Release);
        result
    })
}

async fn graph_snapshot(projection: &TursoProjection) -> Result<GraphSnapshot, ProjectionError> {
    let tx = projection.connection.unchecked_transaction().await?;
    let metadata = graph::package_graph_metadata_from(&tx)
        .await?
        .ok_or(ProjectionError::StaleTransition)?;
    let selected_view = crate::read::metadata_from(&tx)
        .await?
        .ok_or(ProjectionError::StaleTransition)?;
    if metadata.root != selected_view.root {
        tx.rollback().await?;
        return Err(ProjectionError::StaleTransition);
    }

    let mut edge_rows = tx
        .query(
            "SELECT edge_id FROM backend_projection_package_edges ORDER BY edge_id",
            (),
        )
        .await?;
    let mut edge_ids = Vec::new();
    while let Some(row) = edge_rows.next().await? {
        let id: Vec<u8> = row.get(0)?;
        edge_ids.push(
            id.try_into()
                .map_err(|_| ProjectionError::CorruptMetadata { field: "edge_id" })?,
        );
    }
    drop(edge_rows);

    let mut state_rows = tx
        .query(
            "SELECT source, source_authority_kind, source_authority_id, state, reason \
             FROM backend_projection_package_states \
             ORDER BY source, source_authority_kind, source_authority_id",
            (),
        )
        .await?;
    let mut states = Vec::new();
    while let Some(row) = state_rows.next().await? {
        states.push((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ));
    }
    drop(state_rows);
    tx.rollback().await?;

    Ok(GraphSnapshot {
        root: metadata.root,
        facts_witness: metadata.facts_witness,
        edge_count: metadata.edge_count,
        edge_ids,
        states,
    })
}

fn dependency_edge(
    source: &PackageReference,
    name: &str,
    requirement: &str,
) -> PackageDependencyRecord {
    dependency_edge_in_scope(source, name, requirement, DependencyScope::Runtime)
}

fn dependency_edge_in_scope(
    source: &PackageReference,
    name: &str,
    requirement: &str,
    scope: DependencyScope,
) -> PackageDependencyRecord {
    PackageDependencyRecord::new(
        source.clone(),
        PackageDependencyTarget::new(RegistryEcosystem::Cargo, name, requirement, None)
            .expect("target"),
        scope,
        false,
        DependencyEvidence {
            authority: DependencyAuthority::RegistryMetadata,
            frontier: [1; 32],
            provenance: [2; 32],
        },
    )
}

async fn graph_edge_count(projection: &TursoProjection) -> i64 {
    let mut rows = projection
        .connection
        .query("SELECT COUNT(*) FROM backend_projection_package_edges", ())
        .await
        .unwrap_or_else(|error| panic!("graph count: {error}"));
    let row = rows
        .next()
        .await
        .unwrap_or_else(|error| panic!("graph count next: {error}"))
        .unwrap_or_else(|| panic!("graph count row missing"));
    row.get(0)
        .unwrap_or_else(|error| panic!("graph edge count: {error}"))
}

async fn graph_total_changes(projection: &TursoProjection) -> i64 {
    let mut rows = projection
        .connection
        .query("SELECT total_changes()", ())
        .await
        .unwrap_or_else(|error| panic!("total changes query: {error}"));
    let row = rows
        .next()
        .await
        .unwrap_or_else(|error| panic!("total changes next: {error}"))
        .unwrap_or_else(|| panic!("total changes row missing"));
    row.get(0)
        .unwrap_or_else(|error| panic!("total changes decode: {error}"))
}

fn graph_facts(
    source: &PackageReference,
    edges: &[PackageDependencyRecord],
) -> Vec<(
    PackageGraphSourceKey,
    DependencyFacts<Box<[PackageDependencyRecord]>>,
)> {
    vec![(
        PackageGraphSourceKey::unattributed(source.clone()),
        DependencyFacts::Known(edges.to_vec().into_boxed_slice()),
    )]
}

async fn stored_edge(projection: &TursoProjection, edge_id: &[u8; 32]) -> i64 {
    let mut rows = projection
        .connection
        .query(
            "SELECT rowid FROM backend_projection_package_edges WHERE edge_id = ?1",
            turso::params![edge_id.as_slice()],
        )
        .await
        .unwrap_or_else(|error| panic!("edge query: {error}"));
    let row = rows
        .next()
        .await
        .unwrap_or_else(|error| panic!("edge next: {error}"))
        .unwrap_or_else(|| panic!("missing edge"));
    row.get(0)
        .unwrap_or_else(|error| panic!("edge rowid: {error}"))
}

#[test]
#[ignore = "bounded Turso exact-label projection stress probe"]
fn stress_exact_label_projection_reports_build_lookup_and_delta_costs() {
    futures_executor::block_on(async {
        const ROWS: usize = 20_000;
        let path = path();
        let empty = root(Vec::new());
        let package = package_key("stress");
        let rows = (0..ROWS)
            .map(|index| {
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
                    "package documentation for symbol_{index:05}"
                ))])
            })
            .collect::<Vec<_>>();
        let view = root(rows);
        let mut projection = open_test(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let started = Instant::now();
        let update = projection
            .synchronize_from_current_for_test(&view)
            .await
            .unwrap_or_else(|error| panic!("synchronize: {error}"));
        let build_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(update, ProjectionUpdate::Rebuilt { rows: ROWS as u64 });

        let started = Instant::now();
        let exact = projection
            .lookup_label("symbol_19999", 10)
            .await
            .unwrap_or_else(|error| panic!("exact label lookup: {error}"));
        let lookup_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(exact.root.as_ref(), view.root().as_bytes());
        assert_eq!(
            exact.ids.as_ref(),
            &[RowId::Symbol(backend_library::symbol_key("stress::symbol_19999")).stable_key()]
        );
        let absent = projection
            .lookup_label("singular needle", 10)
            .await
            .unwrap_or_else(|error| panic!("absent exact label lookup: {error}"));
        assert!(absent.ids.is_empty());

        let replacement = Row::in_package(
            RowId::Symbol(backend_library::symbol_key("stress::symbol_10000")),
            view.basis(),
            package,
            "symbol_10000-v2",
        )
        .with_document(vec![Fragment::Text("changed edge".to_owned())]);
        let prepared = view
            .prepare(
                ViewDelta::Upsert { row: replacement },
                capability(view.basis().object),
            )
            .unwrap_or_else(|error| panic!("prepare: {error:?}"));
        let (next, committed) = view
            .commit(prepared)
            .unwrap_or_else(|error| panic!("commit: {error:?}"));
        let started = Instant::now();
        let update = projection
            .apply(&committed)
            .await
            .unwrap_or_else(|error| panic!("apply: {error}"));
        let delta_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(update, ProjectionUpdate::Advanced { changed_rows: 1 });
        let updated = projection
            .lookup_label("symbol_10000-v2", 10)
            .await
            .unwrap_or_else(|error| panic!("updated exact label lookup: {error}"));
        assert_eq!(updated.root.as_ref(), next.root().as_bytes());
        assert_eq!(updated.ids.len(), 1);
        let stale_label = projection
            .lookup_label("symbol_10000", 10)
            .await
            .unwrap_or_else(|error| panic!("old exact label lookup: {error}"));
        assert!(stale_label.ids.is_empty());

        let bytes = ["", "-wal", "-shm", "-tshm"]
            .iter()
            .map(|suffix| {
                let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
                std::fs::metadata(sidecar).map_or(0, |metadata| metadata.len())
            })
            .sum::<u64>();
        eprintln!(
            "turso_exact_label_stress rows={ROWS} build_ms={build_ms:.2} exact_lookup_ms={lookup_ms:.3} one_row_delta_ms={delta_ms:.3} projection_bytes_with_sidecars={bytes}"
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

        let mut projection = open_test(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        assert_eq!(
            projection
                .synchronize_from_current_for_test(&first)
                .await
                .unwrap_or_else(|error| panic!("first synchronize: {error}")),
            ProjectionUpdate::Rebuilt { rows: 3 }
        );
        let keep_rowid = stored_rowid(&projection, &keep_key).await;
        assert_eq!(recorded_changes(&projection, &first).await, 3);

        assert_eq!(
            projection
                .synchronize_from_current_for_test(&revised)
                .await
                .unwrap_or_else(|error| panic!("revised synchronize: {error}")),
            ProjectionUpdate::Rebuilt { rows: 2 }
        );
        assert_eq!(stored_rowid(&projection, &keep_key).await, keep_rowid);
        assert_eq!(recorded_changes(&projection, &revised).await, 2);

        let alpha = projection
            .lookup_label("alpha", 10)
            .await
            .unwrap_or_else(|error| panic!("lookup alpha: {error}"));
        assert_eq!(alpha.root.as_ref(), revised.root().as_bytes());
        assert_eq!(alpha.ids.as_ref(), &[keep_key]);
        let edited = projection
            .lookup_label("gamma-two", 10)
            .await
            .unwrap_or_else(|error| panic!("lookup edit: {error}"));
        assert_eq!(
            edited.ids.as_ref(),
            &[RowId::Package(package_key("edit")).stable_key()]
        );
        let removed = projection
            .lookup_label("beta", 10)
            .await
            .unwrap_or_else(|error| panic!("lookup removed: {error}"));
        assert!(removed.ids.is_empty());

        drop(projection);
        remove_database(&path);
    });
}

#[test]
fn frontier_republish_keeps_rows_until_a_hot_delta_clears_the_digest() {
    futures_executor::block_on(async {
        let path = path();
        let basis = root(Vec::new()).basis();
        let kept = Row::new(RowId::Package(package_key("kept")), basis, "kept");
        let extra = Row::new(RowId::Package(package_key("extra")), basis, "extra");
        let object = basis.object;
        let first = root_at(vec![kept.clone()], 0);
        let moved = root_at(vec![kept.clone()], 1);
        let mut projection = open_test(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        projection
            .synchronize_from_current_for_test(&first)
            .await
            .unwrap_or_else(|error| panic!("seed: {error}"));
        let kept_key = kept.id.stable_key();
        let kept_rowid = stored_rowid(&projection, &kept_key).await;
        projection
            .synchronize_from_current_for_test(&moved)
            .await
            .unwrap_or_else(|error| panic!("move: {error}"));
        assert_eq!(stored_rowid(&projection, &kept_key).await, kept_rowid);
        let prepared = moved
            .prepare(ViewDelta::Upsert { row: extra.clone() }, capability(object))
            .unwrap_or_else(|error| panic!("prepare: {error:?}"));
        let (_next, committed) = moved
            .clone()
            .commit(prepared)
            .unwrap_or_else(|error| panic!("commit: {error:?}"));
        projection
            .apply(&committed)
            .await
            .unwrap_or_else(|error| panic!("apply: {error}"));
        let restored = root_at(vec![kept], 2);
        projection
            .synchronize_from_current_for_test(&restored)
            .await
            .unwrap_or_else(|error| panic!("restore: {error}"));
        assert_eq!(stored_rowid(&projection, &kept_key).await, kept_rowid);
        let extra_rows = projection
            .lookup_label("extra", 10)
            .await
            .unwrap_or_else(|error| panic!("lookup extra: {error}"));
        assert!(extra_rows.ids.is_empty());
    });
}

fn root_at(rows: Vec<Row>, sequence: u64) -> ViewRoot {
    let source = view_state_root(&[]);
    let object = object_version(b"projection-test");
    let basis = Basis::with_context(source, object, branch_key("main"), log_key("library"), 1);
    ViewRoot::new_checked(
        view_key(b"projection-test"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, sequence),
        rows,
        vec![Coverage::Complete],
        capability(object),
    )
    .unwrap_or_else(|error| panic!("root_at: {error:?}"))
}

#[test]
fn projection_row_content_and_hash_survive_synchronize_and_restart() {
    futures_executor::block_on(async {
        let path = path();
        let basis = root(Vec::new()).basis();
        let target = backend_library::symbol_key("hash::target");
        let mut row = Row::in_package(
            RowId::Symbol(backend_library::symbol_key("hash::item")),
            basis,
            package_key("workspace"),
            "hashed",
        )
        .with_signature("fn hashed()")
        .with_document(vec![
            Fragment::Text("alpha".to_owned()),
            Fragment::Code("beta".to_owned()),
            Fragment::Link {
                label: "docs".to_owned(),
                target,
            },
            Fragment::Break,
        ]);
        row.score = Some(7);
        let view = root(vec![row.clone()]);
        let mut projection = open_test(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        projection
            .synchronize_from_current_for_test(&view)
            .await
            .unwrap_or_else(|error| panic!("synchronize: {error}"));
        let key = row.id.stable_key();
        let mut rows = projection
            .connection
            .query(
                "SELECT content_hash, signature, document \
                 FROM backend_projection_rows WHERE row_id=?1",
                [key.as_str()],
            )
            .await
            .unwrap_or_else(|error| panic!("hash query: {error}"));
        let stored = rows
            .next()
            .await
            .unwrap_or_else(|error| panic!("hash next: {error}"))
            .unwrap_or_else(|| panic!("missing row"));
        let content_hash: Vec<u8> = stored
            .get(0)
            .unwrap_or_else(|error| panic!("hash bytes: {error}"));
        let signature: String = stored
            .get(1)
            .unwrap_or_else(|error| panic!("signature: {error}"));
        let document: String = stored
            .get(2)
            .unwrap_or_else(|error| panic!("document: {error}"));
        assert_eq!(content_hash, streaming_row_hash(&row).as_bytes().as_slice());
        assert_eq!(signature, "fn hashed()");
        assert_eq!(document, "alphabetadocs\n");
        drop(rows);

        let updated_row = row
            .clone()
            .with_signature("fn hashed_updated()")
            .with_document(vec![Fragment::Text("updated payload".to_owned())]);
        let prepared = view
            .prepare(
                ViewDelta::Upsert {
                    row: updated_row.clone(),
                },
                capability(view.basis().object),
            )
            .unwrap_or_else(|error| panic!("prepare content update: {error:?}"));
        let (next, committed) = view
            .clone()
            .commit(prepared)
            .unwrap_or_else(|error| panic!("commit content update: {error:?}"));
        projection
            .apply(&committed)
            .await
            .unwrap_or_else(|error| panic!("apply content update: {error}"));
        drop(projection);

        let restarted_projection = open_test(&path)
            .await
            .unwrap_or_else(|error| panic!("reopen projection: {error}"));
        let found = restarted_projection
            .lookup_label("hashed", 1)
            .await
            .unwrap_or_else(|error| panic!("restarted label lookup: {error}"));
        assert_eq!(found.root.as_ref(), next.root().as_bytes());
        assert_eq!(found.ids.as_ref(), &[key.clone()]);
        let mut rows = restarted_projection
            .connection
            .query(
                "SELECT content_hash, signature, document \
                 FROM backend_projection_rows WHERE row_id=?1",
                [key.as_str()],
            )
            .await
            .unwrap_or_else(|error| panic!("restarted row query: {error}"));
        let restarted_row = rows
            .next()
            .await
            .unwrap_or_else(|error| panic!("restarted row next: {error}"))
            .unwrap_or_else(|| panic!("restarted row missing"));
        assert_eq!(
            restarted_row
                .get::<Vec<u8>>(0)
                .unwrap_or_else(|error| panic!("restarted content hash: {error}"))
                .as_slice(),
            streaming_row_hash(&updated_row).as_bytes().as_slice()
        );
        assert_eq!(
            restarted_row
                .get::<String>(1)
                .unwrap_or_else(|error| panic!("restarted signature: {error}")),
            "fn hashed_updated()"
        );
        assert_eq!(
            restarted_row
                .get::<String>(2)
                .unwrap_or_else(|error| panic!("restarted document: {error}")),
            "updated payload"
        );
        drop(rows);
        drop(restarted_projection);
        remove_database(&path);
    });
}

fn streaming_row_hash(row: &Row) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.turso.row.v1\0");
    stream_field(&mut hasher, row.id.stable_key().as_bytes());
    stream_field(&mut hasher, row.basis.root.as_bytes());
    stream_field(&mut hasher, row.basis.object.as_bytes());
    stream_field(&mut hasher, row.basis.branch.as_bytes());
    stream_field(&mut hasher, row.basis.log.as_bytes());
    stream_field(&mut hasher, &row.basis.schema.to_be_bytes());
    stream_field(
        &mut hasher,
        &[match row.state {
            backend_library::RowState::Ready => 0,
            backend_library::RowState::Loading => 1,
            backend_library::RowState::Failed => 2,
        }],
    );
    stream_field(&mut hasher, row.label.as_bytes());
    let score = row.score.map(u32::to_be_bytes);
    stream_option(&mut hasher, score.as_ref().map(<[u8; 4]>::as_slice));
    stream_option(
        &mut hasher,
        row.package
            .as_ref()
            .map(|value| value.as_bytes().as_slice()),
    );
    stream_option(
        &mut hasher,
        row.parent.as_ref().map(|value| value.as_bytes().as_slice()),
    );
    stream_option(&mut hasher, row.signature.as_deref().map(str::as_bytes));
    for fragment in &row.document {
        match fragment {
            Fragment::Text(value) => {
                stream_field(&mut hasher, &[0]);
                stream_field(&mut hasher, value.as_bytes());
            }
            Fragment::Code(value) => {
                stream_field(&mut hasher, &[1]);
                stream_field(&mut hasher, value.as_bytes());
            }
            Fragment::Link { label, target } => {
                stream_field(&mut hasher, &[2]);
                stream_field(&mut hasher, label.as_bytes());
                stream_field(&mut hasher, target.as_bytes());
            }
            Fragment::Break => stream_field(&mut hasher, &[3]),
        }
    }
    hasher.finalize()
}

fn stream_option(hasher: &mut blake3::Hasher, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            stream_field(hasher, value);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn stream_field(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
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

async fn stored_state_rowid(projection: &TursoProjection, source: &str) -> i64 {
    let mut rows = projection
        .connection
        .query(
            "SELECT rowid FROM backend_projection_package_states WHERE source = ?1 \
             AND source_authority_kind=0 AND source_authority_id=?2",
            turso::params![source, [0_u8; 32].as_slice()],
        )
        .await
        .unwrap_or_else(|error| panic!("state rowid query: {error}"));
    let row = rows
        .next()
        .await
        .unwrap_or_else(|error| panic!("state rowid next: {error}"))
        .unwrap_or_else(|| panic!("missing state {source}"));
    row.get(0)
        .unwrap_or_else(|error| panic!("state rowid decode: {error}"))
}
