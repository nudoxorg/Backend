//! Republishing a view whose rows did not change, and republishing one edited row.
//!
//! Fixture construction stays outside operation timers. Cold seed/reopen,
//! selected-root moves, and one-row content edits are reported separately.
//! These deterministic fixtures exercise the projection-side CAS only; they
//! are not evidence that a source observation is fresh.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "the fixed benchmark fixture is intentionally fail-fast"
)]

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use backend_extension_turso::{
    ProjectionGraphSeed, ProjectionSeed, ProjectionUpdate, TursoProjection,
};
use backend_library::{
    AuthorityScopeClaim, Basis, Coverage, CoverageCapability, Fragment, Frontier,
    ProducerObservationClaims, ProducerObservationVerifier, Row, RowId, ViewRoot,
    admit_complete_scope, admit_producer_observation, branch_key, log_key, object_version,
    package_key, symbol_key, view_key, view_state_root,
};

const ROWS: usize = 1024;
const SAMPLES: usize = 32;
const WARMUPS: usize = 4;
static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(0);

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

fn private_temp_root(label: &str) -> PathBuf {
    for _ in 0..128 {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nudox-turso-{label}-{}-{timestamp:x}-{sequence:x}",
            std::process::id()
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => return path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create private benchmark root: {error}"),
        }
    }
    panic!("could not allocate a unique private row-move benchmark root");
}

#[derive(Default)]
struct PhysicalFootprint {
    namespace_file_bytes: u64,
    selected_generation_bytes: u64,
    retained_generation_bytes: u64,
    allocated_namespace_bytes: u64,
    generation_count: usize,
    selector_bytes: u64,
    counter_bytes: u64,
    gate_bytes: u64,
    wal_file_bytes: u64,
    shm_file_bytes: u64,
}

fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.blocks().saturating_mul(512)
    }
    #[cfg(not(unix))]
    {
        metadata.len()
    }
}

fn count_tree(path: &Path, footprint: &mut PhysicalFootprint) {
    let metadata = fs::symlink_metadata(path).expect("Turso namespace entry metadata");
    assert!(
        !metadata.file_type().is_symlink(),
        "Turso namespace symlink"
    );
    footprint.allocated_namespace_bytes = footprint
        .allocated_namespace_bytes
        .saturating_add(allocated_bytes(&metadata));
    if metadata.file_type().is_file() {
        let bytes = metadata.len();
        footprint.namespace_file_bytes = footprint.namespace_file_bytes.saturating_add(bytes);
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-wal") || name.ends_with(".wal"))
        {
            footprint.wal_file_bytes = footprint.wal_file_bytes.saturating_add(bytes);
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-shm") || name.ends_with(".shm"))
        {
            footprint.shm_file_bytes = footprint.shm_file_bytes.saturating_add(bytes);
        }
    } else {
        assert!(
            metadata.file_type().is_dir(),
            "unexpected Turso namespace object"
        );
        for entry in fs::read_dir(path).expect("read Turso namespace") {
            count_tree(&entry.expect("Turso namespace entry").path(), footprint);
        }
    }
}

fn selected_footprint(database: &Path, generation: u64) -> PhysicalFootprint {
    let parent = database.parent().expect("database parent");
    let name = database.file_name().expect("database filename");
    let namespace = parent.join(format!("{}.namespace-v1", name.to_string_lossy()));
    let namespace_metadata = fs::symlink_metadata(&namespace).expect("namespace metadata");
    assert!(
        namespace_metadata.file_type().is_dir(),
        "Turso namespace directory"
    );
    let selector = fs::read(namespace.join("selected")).expect("selected selector");
    assert_eq!(selector.len(), 155, "selector format length");
    assert_eq!(&selector[..8], b"BPTSEL01", "selector magic");
    assert!(
        selector[8] == 0 && selector[9] == 1,
        "selector format version"
    );
    assert_eq!(
        u64::from_be_bytes(selector[10..18].try_into().expect("selector generation")),
        generation
    );
    let selected_name = format!("g{generation:016x}");
    let generations = namespace.join("generations");
    let generations_metadata = fs::symlink_metadata(&generations).expect("generations metadata");
    assert!(
        generations_metadata.file_type().is_dir(),
        "generations directory"
    );
    let selected_dir = generations.join(&selected_name);
    let marker = fs::read(selected_dir.join(".generation")).expect("selected generation marker");
    assert_eq!(marker.len(), 155, "generation marker format length");
    assert_eq!(&marker[..8], b"BPTGEN01", "generation marker magic");
    assert!(
        marker[8] == 0 && marker[9] == 1,
        "generation marker format version"
    );
    assert_eq!(
        u64::from_be_bytes(marker[10..18].try_into().expect("marker generation")),
        generation
    );
    assert!(
        selected_dir.join("projection.turso").is_file(),
        "selected database file"
    );

    let mut footprint = PhysicalFootprint {
        allocated_namespace_bytes: allocated_bytes(&namespace_metadata)
            .saturating_add(allocated_bytes(&generations_metadata)),
        selector_bytes: selector.len() as u64,
        ..PhysicalFootprint::default()
    };
    for entry in fs::read_dir(&generations).expect("read generations") {
        let path = entry.expect("generation entry").path();
        assert!(path.is_dir(), "generation entry directory");
        footprint.generation_count = footprint.generation_count.saturating_add(1);
        let before = footprint.namespace_file_bytes;
        count_tree(&path, &mut footprint);
        let generation_bytes = footprint.namespace_file_bytes.saturating_sub(before);
        if path.file_name().and_then(|name| name.to_str()) == Some(selected_name.as_str()) {
            footprint.selected_generation_bytes = generation_bytes;
        } else {
            footprint.retained_generation_bytes = footprint
                .retained_generation_bytes
                .saturating_add(generation_bytes);
        }
    }
    for entry in fs::read_dir(&namespace).expect("read namespace controls") {
        let path = entry.expect("namespace control").path();
        if path.file_name().and_then(|name| name.to_str()) == Some("generations") {
            continue;
        }
        match path.file_name().and_then(|name| name.to_str()) {
            Some("selected") => {}
            Some("last-generation") => {
                footprint.counter_bytes = fs::metadata(&path).expect("counter metadata").len();
            }
            Some("selection.lock") => {
                footprint.gate_bytes = fs::metadata(&path).expect("gate metadata").len();
            }
            _ => {}
        }
        count_tree(&path, &mut footprint);
    }
    assert_eq!(
        fs::read(namespace.join("selected")).expect("stable selector reread"),
        selector,
        "selector changed during storage census"
    );
    footprint
}

fn verify_selected(
    projection: &TursoProjection,
    expected_root: &ViewRoot,
    expected_label: &str,
    expected_row_id: RowId,
) {
    let revision = futures_executor::block_on(projection.revision()).expect("selected revision");
    assert_eq!(
        revision.root(),
        *expected_root.root().as_bytes(),
        "selected root"
    );
    let rows = futures_executor::block_on(projection.lookup_label(expected_label, 1))
        .expect("selected label query");
    assert_eq!(rows.root, revision.root(), "query root fence");
    assert_eq!(
        rows.ids.as_ref(),
        [expected_row_id.stable_key()],
        "selected row identity"
    );
}

fn main() {
    futures_executor::block_on(async {
        let path = private_temp_root(&format!("row-move-{ROWS}"));
        let database = path.join("projection.turso");
        let stable = rows("symbol_last");
        let cold = view(stable.clone(), 0);
        let cold_seed_started = Instant::now();
        let mut projection = TursoProjection::seed_if_empty(
            &database,
            ProjectionSeed::new(&cold, ProjectionGraphSeed::Unavailable),
        )
        .await
        .expect("complete cold seed");
        let cold_seed_ns = cold_seed_started.elapsed().as_nanos();
        let seeded = ProjectionUpdate::Rebuilt {
            rows: cold.row_count(),
        };
        assert_eq!(seeded, ProjectionUpdate::Rebuilt { rows: ROWS as u64 });
        let seed_revision = projection.revision().await.expect("seed revision");
        assert_eq!(seed_revision.root(), *cold.root().as_bytes());
        let selected_generation = seed_revision.generation().get();

        drop(projection);
        let cold_reopen_started = Instant::now();
        let mut projection = TursoProjection::open(&database)
            .await
            .expect("selected-only cold reopen");
        let reopen_revision = projection.revision().await.expect("reopen revision");
        let reopened_view = view(rows("symbol_last"), 0);
        assert_eq!(
            projection
                .synchronize_from(reopen_revision, &reopened_view)
                .await
                .expect("fenced cold-reopen reuse"),
            ProjectionUpdate::Reused { rows: ROWS as u64 }
        );
        let cold_reopen_ns = cold_reopen_started.elapsed().as_nanos();
        verify_selected(
            &projection,
            &reopened_view,
            "symbol_last",
            RowId::Symbol(symbol_key("bench::symbol_01023")),
        );

        let mut move_samples = [0_u128; SAMPLES];
        for sample in 0..(WARMUPS + SAMPLES) {
            let expected = projection.revision().await.expect("view-move revision");
            let target = view(
                stable.clone(),
                u64::try_from(sample + 1).expect("root-move sequence"),
            );
            assert_ne!(
                expected.root(),
                *target.root().as_bytes(),
                "root-move fixture"
            );
            let started = Instant::now();
            let update = projection
                .synchronize_from(expected, &target)
                .await
                .expect("fenced root move");
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                black_box(update),
                ProjectionUpdate::Rebuilt { rows: ROWS as u64 }
            );
            assert_eq!(
                projection
                    .revision()
                    .await
                    .expect("moved revision")
                    .generation()
                    .get(),
                selected_generation,
                "a root move must remain inside the selected generation"
            );
            verify_selected(
                &projection,
                &target,
                "symbol_last",
                RowId::Symbol(symbol_key("bench::symbol_01023")),
            );
            if sample >= WARMUPS {
                move_samples[sample - WARMUPS] = elapsed;
            }
        }

        let edit_path = private_temp_root("row-move-one-row-edit");
        let edit_database = edit_path.join("projection.turso");
        let edit_stable = rows("symbol_last");
        let edit_base = view(edit_stable.clone(), 0);
        let edit_seed_started = Instant::now();
        let mut edit_projection = TursoProjection::seed_if_empty(
            &edit_database,
            ProjectionSeed::new(&edit_base, ProjectionGraphSeed::Unavailable),
        )
        .await
        .expect("independent complete one-row-edit baseline seed");
        let edit_cold_seed_ns = edit_seed_started.elapsed().as_nanos();
        let edit_generation = edit_projection
            .revision()
            .await
            .expect("edit baseline revision")
            .generation()
            .get();
        verify_selected(
            &edit_projection,
            &edit_base,
            "symbol_last",
            RowId::Symbol(symbol_key("bench::symbol_01023")),
        );

        let mut edit_samples = [0_u128; SAMPLES];
        for sample in 0..(WARMUPS + SAMPLES) {
            let expected = edit_projection
                .revision()
                .await
                .expect("edit revision");
            let target = view(
                rows(&format!("edited-{sample}")),
                u64::try_from(1_000 + sample).expect("edit sequence"),
            );
            assert_eq!(target.rows().len(), ROWS, "edit fixture row count");
            let edited_id = RowId::Symbol(symbol_key("bench::symbol_01023"));
            for row in edit_stable.iter() {
                if row.id == edited_id {
                    assert_eq!(row.label, "symbol_last");
                    assert_eq!(
                        target.row_ref(edited_id).expect("edited row").label,
                        format!("edited-{sample}")
                    );
                } else {
                    assert_eq!(target.row_ref(row.id), Some(row));
                }
            }
            let started = Instant::now();
            let update = edit_projection
                .synchronize_from(expected, &target)
                .await
                .expect("fenced one-row edit");
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                black_box(update),
                ProjectionUpdate::Rebuilt { rows: ROWS as u64 }
            );
            assert_eq!(
                edit_projection
                    .revision()
                    .await
                    .expect("edited revision")
                    .generation()
                    .get(),
                edit_generation,
                "one-row content edits must remain inside the selected generation"
            );
            verify_selected(
                &edit_projection,
                &target,
                &format!("edited-{sample}"),
                RowId::Symbol(symbol_key("bench::symbol_01023")),
            );
            if sample >= WARMUPS {
                edit_samples[sample - WARMUPS] = elapsed;
            }
        }
        let footprint = selected_footprint(&database, selected_generation);
        let edit_footprint = selected_footprint(&edit_database, edit_generation);
        println!(
            "row_move rows={ROWS} samples={SAMPLES} warmups={WARMUPS} cold_seed_ns={cold_seed_ns} cold_selected_reopen_reuse_ns={cold_reopen_ns} edit_cold_seed_ns={edit_cold_seed_ns} move_median_ns={} move_p95_ns={} edit_median_ns={} edit_p95_ns={} selected_generation={} generation_count={} namespace_file_bytes={} allocated_namespace_bytes={} selected_generation_bytes={} retained_generation_bytes={} selector_bytes={} counter_bytes={} gate_bytes={} wal_bytes={} shm_bytes={} edit_selected_generation={} edit_generation_count={} edit_namespace_file_bytes={} edit_allocated_namespace_bytes={} edit_selected_generation_bytes={} edit_retained_generation_bytes={} edit_selector_bytes={} edit_counter_bytes={} edit_gate_bytes={} edit_wal_bytes={} edit_shm_bytes={}",
            percentile(&mut move_samples, SAMPLES / 2),
            percentile(&mut move_samples, SAMPLES * 95 / 100),
            percentile(&mut edit_samples, SAMPLES / 2),
            percentile(&mut edit_samples, SAMPLES * 95 / 100),
            selected_generation,
            footprint.generation_count,
            footprint.namespace_file_bytes,
            footprint.allocated_namespace_bytes,
            footprint.selected_generation_bytes,
            footprint.retained_generation_bytes,
            footprint.selector_bytes,
            footprint.counter_bytes,
            footprint.gate_bytes,
            footprint.wal_file_bytes,
            footprint.shm_file_bytes,
            edit_generation,
            edit_footprint.generation_count,
            edit_footprint.namespace_file_bytes,
            edit_footprint.allocated_namespace_bytes,
            edit_footprint.selected_generation_bytes,
            edit_footprint.retained_generation_bytes,
            edit_footprint.selector_bytes,
            edit_footprint.counter_bytes,
            edit_footprint.gate_bytes,
            edit_footprint.wal_file_bytes,
            edit_footprint.shm_file_bytes,
        );
        drop(edit_projection);
        drop(projection);
        fs::remove_dir_all(edit_path).expect("remove owned private row-edit benchmark root");
        fs::remove_dir_all(path).expect("remove owned private benchmark root");
    });
}
